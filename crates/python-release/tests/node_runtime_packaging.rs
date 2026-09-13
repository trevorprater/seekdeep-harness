//! Verified official archive acquisition and complete relocated release closures.

use std::{
    cell::RefCell,
    collections::BTreeMap,
    fmt::Write as _,
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    process::Command,
};

use seekdeep_code_runtime_worker_thread::node_assets;
use seekdeep_python_release::{
    Package, RuntimePlatform,
    executable::{Host, Target},
    node_runtime::{
        self, DistributionFetcher, NodeDistribution, acquire_distributions, archive_checksum,
        select_distribution,
    },
    wheel,
};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use zip::write::SimpleFileOptions;

#[path = "common/node_fixture.rs"]
mod node_fixture;

#[derive(Default)]
struct FixtureFetcher {
    responses: BTreeMap<String, PathBuf>,
    requests: RefCell<Vec<String>>,
}

impl DistributionFetcher for FixtureFetcher {
    fn download(&self, url: &str, destination: &Path) -> anyhow::Result<()> {
        self.requests.borrow_mut().push(url.to_owned());
        let response = self
            .responses
            .get(url)
            .ok_or_else(|| anyhow::anyhow!("unexpected fixture request: {url}"))?;
        fs::copy(response, destination)?;
        Ok(())
    }
}

fn response(root: &Path, name: &str, content: impl AsRef<[u8]>) -> PathBuf {
    let path = root.join(name);
    fs::write(&path, content).unwrap();
    path
}

fn archive(root: &Path, target: &Target, variant: &str) -> (NodeDistribution, PathBuf, String) {
    let distribution = NodeDistribution::for_version("v24.3.0", target).unwrap();
    let basename = distribution.archive.strip_suffix(".tar.gz").unwrap();
    let source = root.join(basename);
    fs::create_dir_all(source.join("bin")).unwrap();
    let node = source.join("bin/node");
    fs::write(
        &node,
        if variant == "wrong-architecture" {
            node_fixture::native_header(&Target::parse("node24-linux-arm64").unwrap(), false)
        } else {
            node_fixture::native_header(target, false)
        },
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(
            &node,
            fs::Permissions::from_mode(if variant == "not-executable" {
                0o644
            } else {
                0o755
            }),
        )
        .unwrap();
        if variant == "symlink" {
            fs::remove_file(&node).unwrap();
            std::os::unix::fs::symlink("/usr/bin/false", &node).unwrap();
        }
    }
    if variant != "missing-license" {
        fs::write(source.join("LICENSE"), "fixture official license\n").unwrap();
    }
    let path = root.join(&distribution.archive);
    let status = Command::new("tar")
        .arg("-czf")
        .arg(&path)
        .arg("-C")
        .arg(root)
        .arg(basename)
        .status()
        .unwrap();
    assert!(status.success());
    let digest = format!("{:x}", Sha256::digest(fs::read(&path).unwrap()));
    (distribution, path, digest)
}

#[test]
fn source_major_resolution_preserves_index_order_platform_filtering_and_explicit_pins() {
    let target = Target::parse("node24-macos-arm64").unwrap();
    let index = json!([
        {"version":"v240.1.0","files":["osx-arm64-tar"]},
        {"version":"v26.9.0","files":["osx-arm64-tar"]},
        {"version":"v24.99.0","files":["linux-x64"]},
        {"version":"v24.3.0","files":["osx-arm64-tar"]},
        {"version":"v24.4.0","files":["osx-arm64-tar"]}
    ]);
    let selected = select_distribution(&index, &target, None).unwrap();
    assert_eq!(selected.version, "v24.3.0");
    assert_eq!(
        selected.url(),
        "https://nodejs.org/dist/v24.3.0/node-v24.3.0-darwin-arm64.tar.gz"
    );
    assert_eq!(
        select_distribution(&Value::Null, &target, Some("24.21.0"))
            .unwrap()
            .version,
        "v24.21.0"
    );
    assert!(select_distribution(&index, &target, Some("v22.1.0")).is_err());
    assert!(select_distribution(&json!([]), &target, None).is_err());
    for version in ["24", "v24.1", "v24.1.0/../../escape", "v24.x.0"] {
        assert!(NodeDistribution::for_version(version, &target).is_err());
    }
}

#[test]
fn checksum_selection_is_exact_and_rejects_ambiguity() {
    let archive = "node-v24.3.0-linux-x64.tar.gz";
    let digest = "a".repeat(64);
    assert_eq!(
        archive_checksum(
            &format!("{}  {archive}.sig\n{digest}  {archive}\n", "b".repeat(64)),
            archive
        )
        .unwrap(),
        digest
    );
    assert_eq!(
        archive_checksum(
            &format!("{} *{archive}\r\n", digest.to_uppercase()),
            archive
        )
        .unwrap(),
        digest
    );
    assert!(
        archive_checksum(
            &format!("{digest}  {archive}\n{digest}  {archive}\n"),
            archive
        )
        .is_err()
    );
    assert!(archive_checksum(&format!("bad  {archive}\n"), archive).is_err());
    assert!(archive_checksum("", archive).is_err());
}

#[test]
fn acquisition_uses_one_index_snapshot_and_verifies_archives_before_each_fresh_extraction() {
    let root = tempfile::tempdir().unwrap();
    let targets = [
        Target::parse("node24-linux-x64").unwrap(),
        Target::parse("node24-macos-arm64").unwrap(),
    ];
    let mut fetcher = FixtureFetcher::default();
    let mut sums = String::new();
    for target in &targets {
        let (distribution, path, digest) = archive(root.path(), target, "valid");
        writeln!(sums, "{digest}  {}", distribution.archive).unwrap();
        fetcher.responses.insert(distribution.url(), path);
    }
    fetcher.responses.insert(
        "https://nodejs.org/dist/index.json".to_owned(),
        response(
            root.path(),
            "index.json",
            json!([{"version":"v24.3.0","files":["linux-x64","osx-arm64-tar"]}]).to_string(),
        ),
    );
    fetcher.responses.insert(
        "https://nodejs.org/dist/v24.3.0/SHASUMS256.txt".to_owned(),
        response(root.path(), "sums", sums),
    );
    let cache = root.path().join("cache");
    let acquired = acquire_distributions(&targets, &cache, None, &fetcher).unwrap();
    assert_eq!(fetcher.requests.borrow().len(), 4);
    for (node, target) in acquired.iter().zip(&targets) {
        assert_eq!(
            fs::read(node.directory().join("bin/node")).unwrap(),
            node_fixture::native_header(target, false)
        );
        assert_eq!(
            fs::read(node.directory().join("LICENSE")).unwrap(),
            b"fixture official license\n"
        );
    }
    let again = acquire_distributions(&targets, &cache, Some("v24.3.0"), &fetcher).unwrap();
    assert_eq!(fetcher.requests.borrow().len(), 4);
    assert_ne!(acquired[0].directory(), again[0].directory());
    fs::write(
        cache
            .join("v24.3.0")
            .join(&acquired[0].distribution.archive),
        b"modified",
    )
    .unwrap();
    assert!(
        acquire_distributions(&targets, &cache, Some("v24.3.0"), &fetcher)
            .unwrap_err()
            .to_string()
            .contains("digest mismatch")
    );
}

#[test]
fn acquisition_rejects_bad_hashes_missing_licenses_symlinks_modes_and_architectures() {
    let target = Target::parse("node24-linux-x64").unwrap();
    for variant in [
        "bad-hash",
        "missing-license",
        "symlink",
        "not-executable",
        "wrong-architecture",
    ] {
        let root = tempfile::tempdir().unwrap();
        let (distribution, path, digest) = archive(root.path(), &target, variant);
        let mut fetcher = FixtureFetcher::default();
        fetcher.responses.insert(distribution.url(), path);
        let digest = if variant == "bad-hash" {
            "0".repeat(64)
        } else {
            digest
        };
        fetcher.responses.insert(
            "https://nodejs.org/dist/v24.3.0/SHASUMS256.txt".to_owned(),
            response(
                root.path(),
                "sums",
                format!("{digest}  {}\n", distribution.archive),
            ),
        );
        assert!(
            acquire_distributions(
                std::slice::from_ref(&target),
                &root.path().join("cache"),
                Some("v24.3.0"),
                &fetcher
            )
            .is_err(),
            "{variant}"
        );
    }
}

#[test]
fn release_copy_keeps_dependencies_and_rejects_tampering_wrong_targets_and_links() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let target = Target::parse("node24-linux-x64").unwrap();
    node_fixture::make_assets(&source, &target);
    let copied = root.path().join("copied");
    node_runtime::copy_directory(&source, &copied, &target).unwrap();
    assert_eq!(
        fs::read(source.join(node_assets::MANIFEST)).unwrap(),
        fs::read(copied.join(node_assets::MANIFEST)).unwrap()
    );
    assert!(copied.join("node_modules/readdirp/index.js").is_file());
    assert!(
        node_runtime::verify_directory(&source, &Target::parse("node24-macos-arm64").unwrap())
            .is_err()
    );
    fs::remove_file(copied.join("node_modules/readdirp/index.js")).unwrap();
    assert!(node_runtime::verify_directory(&copied, &target).is_err());
    fs::write(source.join("extra"), "not in manifest").unwrap();
    assert!(node_runtime::verify_directory(&source, &target).is_err());
    fs::remove_file(source.join("extra")).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("node_modules/chokidar/package.json", source.join("linked"))
            .unwrap();
        assert!(node_runtime::copy_directory(&source, &root.path().join("bad"), &target).is_err());
    }
}

fn runtime_wheel(
    path: &Path,
    assets: &Path,
    platform: &RuntimePlatform,
    target: &Target,
    executable: Option<&Path>,
) {
    let mut archive = zip::ZipWriter::new(fs::File::create(path).unwrap());
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);
    archive
        .start_file("runtime.dist-info/WHEEL", options)
        .unwrap();
    write!(
        archive,
        "Wheel-Version: 1.0\nTag: py3-none-{}\n",
        platform.tag
    )
    .unwrap();
    archive
        .start_file("runtime.dist-info/METADATA", options)
        .unwrap();
    archive.write_all(b"Name: seekdeep-harness-runtime-bin\nVersion: 1.2.3\nLicense-Expression: MIT\nLicense-File: LICENSE\nLicense-File: THIRD_PARTY_NOTICES.md\n").unwrap();
    for suffix in seekdeep_python_release::runtime_suffixes(&platform.executable) {
        archive
            .start_file(
                format!(
                    "deepseek_harness_runtime/runtime/{}{suffix}",
                    platform.executable
                ),
                options.unix_permissions(0o755),
            )
            .unwrap();
        if suffix.is_empty()
            && let Some(executable) = executable
        {
            archive.write_all(&fs::read(executable).unwrap()).unwrap();
        } else {
            archive.write_all(b"runtime fixture").unwrap();
        }
    }
    archive
        .start_file(
            format!(
                "deepseek_harness_runtime/runtime/{}",
                target.binding_basename()
            ),
            options,
        )
        .unwrap();
    archive
        .write_all(&node_fixture::native_header(target, true))
        .unwrap();
    node_fixture::zip_assets(&mut archive, assets);
    let ripgrep = tempfile::tempdir().unwrap();
    node_fixture::make_ripgrep_assets(ripgrep.path(), target);
    node_fixture::zip_ripgrep_assets(&mut archive, ripgrep.path());
    archive.finish().unwrap();
}

#[test]
fn wheel_gate_rejects_a_missing_dependency_or_changed_node_even_when_native_payload_exists() {
    let root = tempfile::tempdir().unwrap();
    let target = Target::parse("node24-linux-x64").unwrap();
    let platform = RuntimePlatform {
        tag: "manylinux_2_28_x86_64".to_owned(),
        executable: target.basename(),
    };
    let assets = root.path().join("assets");
    node_fixture::make_assets(&assets, &target);
    let path = root.path().join("runtime.whl");
    runtime_wheel(&path, &assets, &platform, &target, None);
    wheel::verify_wheel(&path, Package::Runtime, "1.2.3", Some(&platform)).unwrap();
    fs::remove_file(assets.join("node_modules/readdirp/index.js")).unwrap();
    runtime_wheel(&path, &assets, &platform, &target, None);
    assert!(
        wheel::verify_wheel(&path, Package::Runtime, "1.2.3", Some(&platform))
            .unwrap_err()
            .to_string()
            .contains("missing or modified")
    );
    node_fixture::make_assets(&assets, &target);
    fs::write(assets.join("bin/node"), "replaced").unwrap();
    runtime_wheel(&path, &assets, &platform, &target, None);
    assert!(
        wheel::verify_wheel(&path, Package::Runtime, "1.2.3", Some(&platform))
            .unwrap_err()
            .to_string()
            .contains("missing or modified")
    );
}

#[test]
#[ignore = "requires the verified official host Node archive and fresh compiled worker package"]
fn official_node_and_compiled_runtime_execute_from_relocated_native_and_wheel_layouts() {
    let target = Host::current().target().unwrap();
    let version = std::env::var("SEEKDEEP_RELEASE_NODE_TEST_VERSION").unwrap();
    let official = PathBuf::from(std::env::var_os("SEEKDEEP_RELEASE_NODE_TEST_ARCHIVE").unwrap());
    let sums = PathBuf::from(std::env::var_os("SEEKDEEP_RELEASE_NODE_TEST_SHASUMS").unwrap());
    let compiled = PathBuf::from(std::env::var_os("SEEKDEEP_RELEASE_NODE_TEST_BASE").unwrap());
    let descriptor = NodeDistribution::for_version(&version, &target).unwrap();
    let mut fetcher = FixtureFetcher::default();
    fetcher.responses.insert(descriptor.url(), official);
    fetcher.responses.insert(
        format!(
            "https://nodejs.org/dist/{}/SHASUMS256.txt",
            descriptor.version
        ),
        sums,
    );
    let root = tempfile::tempdir().unwrap();
    let mut acquired = acquire_distributions(
        std::slice::from_ref(&target),
        &root.path().join("cache"),
        Some(&version),
        &fetcher,
    )
    .unwrap();
    let node = acquired.pop().unwrap();
    let native = root.path().join("native");
    let native_assets = native
        .join("code-runtime-node")
        .join(target.platform_arch());
    fs::create_dir_all(native_assets.parent().unwrap()).unwrap();
    node_runtime::stage_distribution(
        &compiled,
        &node.distribution,
        node.directory(),
        &node.archive_sha256,
        &native_assets,
    )
    .unwrap();
    let current = std::env::current_exe().unwrap();
    let probe = native.join(target.basename());
    fs::copy(&current, &probe).unwrap();
    relocated_child(&probe, &native_assets, &version, false);

    let platform = seekdeep_python_release::load_platforms(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../python/sdk-runtime/platforms.json"),
    )
    .unwrap()
    .get(&target.platform_arch())
    .unwrap()
    .clone();
    let wheel_path = root.path().join("runtime.whl");
    runtime_wheel(
        &wheel_path,
        &native_assets,
        &platform,
        &target,
        Some(&probe),
    );
    wheel::verify_wheel(&wheel_path, Package::Runtime, "1.2.3", Some(&platform)).unwrap();
    let installed = root.path().join("installed");
    fs::create_dir(&installed).unwrap();
    zip::ZipArchive::new(fs::File::open(&wheel_path).unwrap())
        .unwrap()
        .extract(&installed)
        .unwrap();
    let runtime = installed.join("deepseek_harness_runtime/runtime");
    let installed_probe = runtime.join(target.basename());
    let installed_assets = runtime.join("code-runtime-node");
    relocated_child(&installed_probe, &installed_assets, &version, false);
    python_relocated_child(&installed_probe, &installed_assets, &version);
    fs::rename(
        installed_assets.join("bin/node"),
        installed_assets.join("bin/node-removed"),
    )
    .unwrap();
    relocated_child(&installed_probe, &installed_assets, &version, true);
}

fn python_relocated_child(executable: &Path, assets: &Path, version: &str) {
    let python = Command::new("python3")
        .args(["-c", "import sys; print(sys.executable)"])
        .output()
        .unwrap();
    assert!(python.status.success());
    let python = String::from_utf8(python.stdout).unwrap();
    let python_result = Command::new(python.trim())
        .args([
            "-c",
            "import subprocess,sys; raise SystemExit(subprocess.call(sys.argv[1:]))",
        ])
        .arg(executable)
        .args([
            "--ignored",
            "--exact",
            "relocated_runtime_child",
            "--nocapture",
        ])
        .env_clear()
        .env("PATH", "/nonexistent")
        .env("SEEKDEEP_RELEASE_NODE_CHILD", "1")
        .env("SEEKDEEP_RELEASE_NODE_ASSETS", assets)
        .env("SEEKDEEP_RELEASE_NODE_VERSION", version)
        .env("SEEKDEEP_RELEASE_NODE_MISSING", "0")
        .current_dir(executable.parent().unwrap())
        .output()
        .unwrap();
    assert!(
        python_result.status.success(),
        "Python-launched wheel runtime failed:\n{}\n{}",
        String::from_utf8_lossy(&python_result.stdout),
        String::from_utf8_lossy(&python_result.stderr)
    );
}

fn relocated_child(executable: &Path, assets: &Path, version: &str, missing: bool) {
    let output = Command::new(executable)
        .args([
            "--ignored",
            "--exact",
            "relocated_runtime_child",
            "--nocapture",
        ])
        .env_clear()
        .env("PATH", "/nonexistent")
        .env("SEEKDEEP_RELEASE_NODE_CHILD", "1")
        .env("SEEKDEEP_RELEASE_NODE_ASSETS", assets)
        .env("SEEKDEEP_RELEASE_NODE_VERSION", version)
        .env(
            "SEEKDEEP_RELEASE_NODE_MISSING",
            if missing { "1" } else { "0" },
        )
        .current_dir(executable.parent().unwrap())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "relocated runtime failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
}

#[tokio::test]
#[ignore = "launched by the parent inside each relocated artifact"]
async fn relocated_runtime_child() {
    use seekdeep_code_runtime::{CODE_RUNTIME, CodeRunRequest};
    use seekdeep_code_runtime_worker_thread::{
        WorkerThreadCodeRuntime, WorkerThreadCodeRuntimeConfig, node_runtime_assets, plugin,
    };
    use seekdeep_cordis::Context;
    assert_eq!(std::env::var("SEEKDEEP_RELEASE_NODE_CHILD").unwrap(), "1");
    assert_eq!(std::env::var("PATH").unwrap(), "/nonexistent");
    assert!(std::env::var_os("SEEKDEEP_NODE_BINARY").is_none());
    assert!(std::env::var_os("SEEKDEEP_CODE_RUNTIME_NODE_DIR").is_none());
    if std::env::var("SEEKDEEP_RELEASE_NODE_MISSING").unwrap() == "1" {
        assert!(WorkerThreadCodeRuntime::new(&WorkerThreadCodeRuntimeConfig::default()).is_err());
        return;
    }
    let expected = PathBuf::from(std::env::var_os("SEEKDEEP_RELEASE_NODE_ASSETS").unwrap())
        .canonicalize()
        .unwrap();
    assert_eq!(node_runtime_assets().unwrap(), expected);
    let ctx = Context::new();
    let fiber = ctx.plugin(plugin(), json!({})).unwrap();
    fiber.await_settled().await.unwrap();
    let runtime = ctx.get(CODE_RUNTIME).unwrap();
    let result = runtime.run(CodeRunRequest { program: "const p = await import('node:process'); const {createHash} = await import('node:crypto'); const {isMainThread} = await import('node:worker_threads'); return {version:p.version,executable:p.execPath,worker:!isMainThread,digest:createHash('sha256').update('relocated').digest('hex')};".into(), bindings: Vec::new(), signal: None }).await.unwrap();
    assert_eq!(result.error, None, "{result:?}");
    let value: Value = serde_json::from_str(result.value.as_ref().unwrap().as_raw()).unwrap();
    assert_eq!(
        value["version"],
        std::env::var("SEEKDEEP_RELEASE_NODE_VERSION").unwrap()
    );
    assert_eq!(
        value["executable"],
        expected.join("bin/node").to_str().unwrap()
    );
    assert_eq!(value["worker"], true);
    assert_eq!(
        value["digest"],
        format!("{:x}", Sha256::digest(b"relocated"))
    );
    fiber.dispose().await.unwrap();
    let watch_dir = tempfile::tempdir().unwrap();
    let script = r"
const fs=require('node:fs'), path=require('node:path');
const assets=process.argv[1], directory=process.argv[2];
const runtime=require(path.join(assets,'wasm-runtime.cjs'));
if(typeof runtime.start_loader!=='function') throw new Error('missing loader runtime');
const watcher=require(path.join(assets,'node_modules/chokidar')).watch(directory,{ignoreInitial:true});
const timer=setTimeout(()=>{throw new Error('packaged Chokidar did not observe a real file')},5000);
watcher.once('ready',()=>fs.writeFileSync(path.join(directory,'probe'),'value'));
watcher.once('add',async()=>{clearTimeout(timer);await watcher.close();process.stdout.write('PACKAGED_WATCH_OK\n')});
";
    let output = Command::new(expected.join("bin/node"))
        .args(["--expose-internals", "-e", script])
        .arg(&expected)
        .arg(watch_dir.path())
        .env_clear()
        .env("PATH", "/nonexistent")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "PACKAGED_WATCH_OK\n"
    );
    println!("RELOCATED_NODE_OK {}", expected.display());
}
