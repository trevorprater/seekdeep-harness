//! Reproducible Node-only installer binding containing compiled Rust behavior.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    process::Command,
};

use base64::Engine as _;
use serde::Deserialize;

const PACKAGE: &str = "seekdeep-subprocess-postinstall";
const ARTIFACT: &str = "seekdeep_subprocess_postinstall";
const ENTRY: &str = "packages/subprocess/subprocess-local/scripts/ensure-spawn-helper.mjs";

pub(super) fn run(metadata: &super::CargoMetadata, check: bool) -> anyhow::Result<()> {
    let target = metadata
        .target_directory
        .join("xtask/subprocess-postinstall");
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(|home| PathBuf::from(home).join(".cargo"))
        });
    let flags = source_remappings(&metadata.workspace_root, cargo_home.as_deref())?;
    let mut command = Command::new("cargo");
    command
        .current_dir(&metadata.workspace_root)
        .env("CARGO_TARGET_DIR", &target)
        .env("CARGO_INCREMENTAL", "0")
        .env("CARGO_BUILD_JOBS", "2")
        .env("CARGO_PROFILE_RELEASE_DEBUG", "0")
        .env("CARGO_PROFILE_RELEASE_STRIP", "symbols")
        .env("CARGO_ENCODED_RUSTFLAGS", flags.join("\u{1f}"))
        .args([
            "build",
            "--locked",
            "--release",
            "--target",
            "wasm32-unknown-unknown",
            "--package",
            PACKAGE,
            "--lib",
        ]);
    anyhow::ensure!(command.status()?.success(), "postinstall Rust build failed");
    let staging = tempfile::tempdir()?;
    let status = Command::new("wasm-bindgen")
        .arg(
            target
                .join("wasm32-unknown-unknown/release")
                .join(format!("{ARTIFACT}.wasm")),
        )
        .args([
            "--target",
            "nodejs",
            "--out-name",
            "postinstall",
            "--no-typescript",
            "--remove-name-section",
            "--remove-producers-section",
            "--out-dir",
        ])
        .arg(staging.path())
        .status()
        .map_err(super::wasm_bindgen_launch_error)?;
    anyhow::ensure!(status.success(), "postinstall binding generation failed");
    let bindings = std::fs::read_to_string(staging.path().join("postinstall.js"))?;
    let bytes = std::fs::read(staging.path().join("postinstall_bg.wasm"))?;
    let script = embed(&bindings, &bytes)?;
    let output = metadata.workspace_root.join(ENTRY);
    if check {
        if std::fs::read_to_string(&output)? != script {
            let diagnostic = target.join("mismatch");
            std::fs::create_dir_all(&diagnostic)?;
            std::fs::write(diagnostic.join("ensure-spawn-helper.mjs"), &script)?;
            std::fs::write(diagnostic.join("postinstall.js"), &bindings)?;
            std::fs::write(diagnostic.join("postinstall_bg.wasm"), &bytes)?;
            anyhow::bail!(
                "stale subprocess postinstall binding; run cargo xtask subprocess-postinstall; generated files retained at {}",
                diagnostic.display()
            );
        }
    } else {
        std::fs::create_dir_all(output.parent().unwrap_or(Path::new(".")))?;
        std::fs::write(&output, script)?;
    }
    println!(
        "subprocess postinstall Rust binding {}: {ENTRY}",
        if check { "verified" } else { "written" }
    );
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(transparent)]
struct PackageId(String);

#[derive(Deserialize)]
struct DependencyMetadata {
    packages: Vec<DependencyPackage>,
    resolve: DependencyResolution,
}

#[derive(Deserialize)]
struct DependencyPackage {
    id: PackageId,
    name: String,
    manifest_path: PathBuf,
    targets: Vec<DependencyTarget>,
}

#[derive(Deserialize)]
struct DependencyTarget {
    kind: Vec<String>,
}

#[derive(Deserialize)]
struct DependencyResolution {
    nodes: Vec<DependencyNode>,
}

#[derive(Deserialize)]
struct DependencyNode {
    id: PackageId,
    deps: Vec<DependencyEdge>,
}

#[derive(Deserialize)]
struct DependencyEdge {
    pkg: PackageId,
    dep_kinds: Vec<DependencyKind>,
}

#[derive(Deserialize)]
struct DependencyKind {
    kind: Option<String>,
}

fn runtime_packages(metadata: &DependencyMetadata) -> anyhow::Result<Vec<&DependencyPackage>> {
    let root = metadata
        .packages
        .iter()
        .find(|package| package.name == PACKAGE)
        .ok_or_else(|| anyhow::anyhow!("postinstall package is absent from Cargo metadata"))?;
    let packages: BTreeMap<_, _> = metadata
        .packages
        .iter()
        .map(|package| (&package.id, package))
        .collect();
    let nodes: BTreeMap<_, _> = metadata
        .resolve
        .nodes
        .iter()
        .map(|node| (&node.id, node))
        .collect();
    let mut pending = vec![root.id.clone()];
    let mut visited = BTreeSet::new();
    let mut runtime = Vec::new();
    while let Some(id) = pending.pop() {
        if !visited.insert(id.clone()) {
            continue;
        }
        let package = packages
            .get(&id)
            .ok_or_else(|| anyhow::anyhow!("Cargo metadata omitted a dependency package"))?;
        if package
            .targets
            .iter()
            .any(|target| target.kind.iter().any(|kind| kind == "proc-macro"))
        {
            continue;
        }
        runtime.push(*package);
        let node = nodes
            .get(&id)
            .ok_or_else(|| anyhow::anyhow!("Cargo metadata omitted a dependency resolution"))?;
        for edge in &node.deps {
            let mut normal = false;
            for kind in &edge.dep_kinds {
                match kind.kind.as_deref() {
                    None => normal = true,
                    Some("build" | "dev") => {}
                    Some(kind) => anyhow::bail!("unsupported Cargo dependency kind: {kind}"),
                }
            }
            if normal {
                pending.push(edge.pkg.clone());
            }
        }
    }
    Ok(runtime)
}

fn file_remapping(file: &Path, root: &Path, virtual_root: &str) -> anyhow::Result<String> {
    let relative = file.strip_prefix(root)?;
    let virtual_file = format!(
        "{virtual_root}/{}",
        relative.to_string_lossy().replace('\\', "/")
    );
    Ok(format!(
        "--remap-path-prefix={}={virtual_file}",
        file.display()
    ))
}

fn source_remappings(workspace: &Path, cargo_home: Option<&Path>) -> anyhow::Result<Vec<String>> {
    let mut roots = vec![(workspace, "/seekdeep")];
    if let Some(cargo_home) = cargo_home {
        roots.push((cargo_home, "/cargo"));
    }
    let mut flags: Vec<_> = roots
        .iter()
        .map(|(root, target)| format!("--remap-path-prefix={}={target}", root.display()))
        .collect();
    let output = Command::new("cargo")
        .current_dir(workspace)
        .args([
            "metadata",
            "--locked",
            "--format-version",
            "1",
            "--filter-platform",
            "wasm32-unknown-unknown",
        ])
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "postinstall dependency metadata failed"
    );
    let metadata: DependencyMetadata = serde_json::from_slice(&output.stdout)?;
    let mut files = BTreeSet::new();
    for package in runtime_packages(&metadata)? {
        let directory = package
            .manifest_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Cargo manifest has no parent"))?;
        for entry in walkdir::WalkDir::new(directory).sort_by_file_name() {
            let entry = entry?;
            if entry.file_type().is_file()
                && entry.path().extension().is_some_and(|ext| ext == "rs")
            {
                files.insert(entry.into_path());
            }
        }
    }
    // rustc remaps prefixes textually; a directory remap retains Windows suffix separators.
    for file in files {
        let (root, target) = roots
            .iter()
            .rev()
            .find(|(root, _)| file.starts_with(root))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "postinstall dependency is outside remapped source roots: {}",
                    file.display()
                )
            })?;
        flags.push(file_remapping(&file, root, target)?);
    }
    Ok(flags)
}

fn embed(bindings: &str, wasm: &[u8]) -> anyhow::Result<String> {
    const READ: &str = "const wasmBytes = require('fs').readFileSync(wasmPath);";
    anyhow::ensure!(
        bindings.matches(READ).count() == 1
            && bindings
                .lines()
                .filter(|line| line.starts_with("const wasmPath = "))
                .count()
                == 1,
        "unsupported wasm-bindgen Node initialization"
    );
    let encoded = base64::engine::general_purpose::STANDARD.encode(wasm);
    let bindings = bindings
        .lines()
        .filter(|line| !line.starts_with("const wasmPath = "))
        .collect::<Vec<_>>()
        .join("\n")
        .replace(
            READ,
            &format!("const wasmBytes = Buffer.from('{encoded}', 'base64');"),
        );
    Ok(format!(
        "/** Generated by cargo xtask subprocess-postinstall from compiled Rust. */\n\
         import {{ createRequire }} from 'node:module';\n\
         const require = createRequire(import.meta.url);\n\
         const exports = Object.create(null);\n\
         {bindings}\n\
         exports.ensureNodePtySpawnHelpers(import.meta.resolve('node-pty'), process.platform, process.arch);\n"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_dependency_walk_excludes_development_build_and_proc_macro_graphs() {
        let metadata: DependencyMetadata = serde_json::from_value(serde_json::json!({
            "packages": [
                {"id":"root","name":PACKAGE,"manifest_path":"root/Cargo.toml","targets":[{"kind":["cdylib","rlib"]}]},
                {"id":"library","name":"library","manifest_path":"library/Cargo.toml","targets":[{"kind":["lib"]}]},
                {"id":"macro","name":"macro","manifest_path":"macro/Cargo.toml","targets":[{"kind":["proc-macro"]}]}
            ],
            "resolve":{"nodes":[
                {"id":"root","deps":[
                    {"pkg":"library","dep_kinds":[{"kind":null}]},
                    {"pkg":"macro","dep_kinds":[{"kind":null}]},
                    {"pkg":"unused-dev","dep_kinds":[{"kind":"dev"}]},
                    {"pkg":"unused-build","dep_kinds":[{"kind":"build"}]}
                ]},
                {"id":"library","deps":[]}
            ]}
        })).unwrap();
        let names = runtime_packages(&metadata)
            .unwrap()
            .into_iter()
            .map(|package| package.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, [PACKAGE, "library"]);
    }

    #[test]
    fn full_filename_remapping_normalizes_compiled_windows_style_suffixes() {
        let temporary = tempfile::tempdir().unwrap();
        let cargo = temporary.path().join("cargo");
        let source = cargo.join(r"registry\src\fixture\src").join("externref.rs");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, "fn main() { print!(\"{}\", file!()); }\n").unwrap();
        let binary = temporary
            .path()
            .join(format!("remap-probe{}", std::env::consts::EXE_SUFFIX));
        let prefix = format!("--remap-path-prefix={}=/cargo", cargo.display());
        let full = file_remapping(&source, &cargo, "/cargo").unwrap();
        for exact in [false, true] {
            let mut command = Command::new("rustc");
            command
                .args(["--edition=2024", "--crate-name", "remap_probe"])
                .arg(&source)
                .arg("-o")
                .arg(&binary)
                .arg(&prefix);
            if exact {
                command.arg(&full);
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let output = Command::new(&binary).output().unwrap();
            assert!(output.status.success());
            let filename = String::from_utf8(output.stdout).unwrap();
            if exact {
                assert_eq!(filename, "/cargo/registry/src/fixture/src/externref.rs");
            } else {
                assert!(
                    filename.contains('\\'),
                    "directory remaps retain suffix separators"
                );
            }
        }
    }
}
