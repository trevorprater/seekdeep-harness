//! Source-pinned contract models, Rust emission, and declarative browser construction plans.

mod browser_driver;
mod codec_driver;
mod plan;
mod registry_driver;

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use seekdeep_typert_generator::{emitter::FaceModelEmitter, model::FaceModel};
use serde_json::{Value, json};

const ORDER: &[&str] = &[
    "@deepseek-ai/dsh-commands",
    "@deepseek-ai/dsh-goal",
    "@deepseek-ai/dsh-cordis-host-runner",
    "@deepseek-ai/dsh-host-plugin-inventory",
    "@deepseek-ai/dsh-message-feedback",
];

const CAPTURE: &str = r"
const { createRequire } = require('node:module');
const { resolve } = require('node:path');
const root = resolve(process.argv[1]);
const req = createRequire(resolve(root, 'package.json'));
req('tsx/cjs');
const { WorkspaceAnalyzer } = req('./packages/typert/generator/src/analyzer.ts');
const { FaceModelEmitter } = req('./packages/typert/generator/src/emitter.ts');
const order = JSON.parse(process.argv[2]);
const workspace = new WorkspaceAnalyzer({ root, packages: order, faces: ['host'], mode: 'check' }).analyze();
const face = workspace.faces[0];
const emitter = new FaceModelEmitter(face);
process.stdout.write(JSON.stringify({ face, artifacts: order.map(name => emitter.emit(name).remote.js) },
  (key, value) => typeof value === 'bigint' ? { $bigint: String(value) } : value));
";

pub(super) fn run(source: Option<&Path>, check: bool) -> anyhow::Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let model_path = root.join("crates/api-remotes-client/contracts/host-model.json");
    let plan_path = root.join("crates/api-remotes-client/contracts/remote-plans.json");
    let pin = include_str!("../../SOURCE_SNAPSHOT")
        .lines()
        .find_map(|line| line.strip_prefix("commit="))
        .ok_or_else(|| anyhow::anyhow!("source pin absent"))?;
    let captured = if let Some(source) = source {
        super::verify_source(source)?;
        let output = Command::new("node")
            .args(["-e", CAPTURE])
            .arg(source)
            .arg(serde_json::to_string(ORDER)?)
            .output()?;
        anyhow::ensure!(
            output.status.success(),
            "source contract analysis failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Some(serde_json::from_slice::<Value>(&output.stdout)?)
    } else {
        None
    };
    let face_value = if let Some(value) = &captured {
        value["face"].clone()
    } else {
        let model = serde_json::from_slice::<Value>(&std::fs::read(&model_path)?)?;
        anyhow::ensure!(
            model["sourceCommit"] == pin,
            "contract model differs from SOURCE_SNAPSHOT"
        );
        model["face"].clone()
    };
    let face: FaceModel = serde_json::from_value(face_value.clone())?;
    let emitter = FaceModelEmitter::new(&face);
    let mut plans = Vec::new();
    for (index, package) in ORDER.iter().enumerate() {
        let artifact = emitter
            .emit(package)?
            .remote
            .ok_or_else(|| anyhow::anyhow!("missing Remote artifact for {package}"))?;
        if let Some(captured) = &captured {
            let expected = captured["artifacts"][index]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("source artifact missing"))?;
            anyhow::ensure!(
                normalize(&artifact.js) == normalize(expected),
                "Rust Remote emitter differs from source for {package}"
            );
        }
        plans.push(plan::compile(&normalize(&artifact.js))?);
    }
    let model = json!({"sourceCommit":pin,"face":face_value});
    let plans = json!({"sourceCommit":pin,"zodVersion":"4.4.3","modules":plans});
    if captured.is_some() {
        publish(&model_path, &model, check)?;
    }
    publish(&plan_path, &plans, check)?;
    println!(
        "generated {} source-faithful Remote modules through the Rust emitter",
        ORDER.len()
    );
    Ok(())
}

fn normalize(value: &str) -> String {
    value
        .replace("@deepseek-ai/dsh-", "@seekdeep-ai/seekdeep-")
        .replace("@deepseek-ai/", "@seekdeep-ai/")
        .replace("_deepseek_ai_dsh_", "_seekdeep_ai_seekdeep_")
        .replace("DeepSeek Harness", "SeekDeep Harness")
}

pub(super) fn bundle_zod(root: &Path, bundle: &str) -> anyhow::Result<String> {
    use std::{io::Write as _, process::Stdio};
    let dependencies = root.join("support/browser-dependencies/node_modules");
    let esbuild = dependencies.join("esbuild/bin/esbuild");
    let zod = dependencies.join("zod/index.js");
    anyhow::ensure!(
        esbuild.is_file() && zod.is_file(),
        "install the pinned browser build dependencies: pnpm --dir support/browser-dependencies install --ignore-workspace --frozen-lockfile"
    );
    let entry = format!(
        "import {{ z as __seekdeepRemoteZod }} from {};\n{bundle}",
        serde_json::to_string(&zod.to_string_lossy())?
    );
    let mut child = Command::new("node")
        .arg(esbuild)
        .args([
            "--bundle",
            "--format=iife",
            "--platform=browser",
            "--target=es2022",
            "--legal-comments=inline",
            "--sourcefile=api-remotes.js",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("esbuild stdin missing"))?
        .write_all(entry.as_bytes())?;
    let output = child.wait_with_output()?;
    anyhow::ensure!(
        output.status.success(),
        "Remote schema dependency bundle failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?)
}

fn publish(path: &Path, value: &Value, check: bool) -> anyhow::Result<()> {
    let text = format!("{}\n", serde_json::to_string(value)?);
    if check {
        anyhow::ensure!(
            std::fs::read_to_string(path)? == text,
            "stale contract artifact: {}",
            path.display()
        );
    } else {
        std::fs::create_dir_all(
            path.parent()
                .ok_or_else(|| anyhow::anyhow!("artifact parent absent"))?,
        )?;
        std::fs::write(path, text)?;
    }
    Ok(())
}

pub(super) fn registry_oracle(source: &Path) -> anyhow::Result<()> {
    corpus(source, false, false)
}

pub(super) fn gateway_oracle(source: &Path, source_regressions: bool) -> anyhow::Result<()> {
    corpus(source, true, source_regressions)
}

fn corpus(source: &Path, gateway: bool, source_regressions: bool) -> anyhow::Result<()> {
    super::verify_source(source)?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| anyhow::anyhow!("workspace root absent"))?
        .to_owned();
    let directory = super::cargo_metadata()?
        .target_directory
        .join("xtask/remote-registry-oracle");
    std::fs::create_dir_all(&directory)?;
    let (adapter, gateway_adapter) = write_corpus_adapters(&root, &directory)?;
    let fixture = directory.join("registry.test.ts");
    let source_package = source.join(if gateway {
        "packages/api/gateway"
    } else {
        "packages/typert/registry"
    });
    let zod = root.join("support/browser-dependencies/node_modules/zod/index.js");
    let tests = normalize(&std::fs::read_to_string(source_package.join(if gateway {
        "tests/gateway.client.spec.ts"
    } else {
        "tests/typert.spec.ts"
    }))?)
    .replace(
        "'@seekdeep-ai/cordis'",
        &serde_json::to_string(&adapter.to_string_lossy())?,
    )
    .replace(
        "'@seekdeep-ai/seekdeep-typert-registry'",
        &serde_json::to_string(&adapter.to_string_lossy())?,
    )
    .replace(
        "'../src/client/index.ts'",
        &serde_json::to_string(
            &if gateway { &gateway_adapter } else { &adapter }.to_string_lossy(),
        )?,
    )
    .replace("'zod'", &serde_json::to_string(&zod.to_string_lossy())?);
    let tests = if gateway {
        format!("{tests}\n{}", registry_driver::GATEWAY_ADDITIONAL)
    } else {
        tests
    };
    let tests = if source_regressions {
        source_gateway_regressions(source, &source_package)?
    } else {
        tests
    };
    std::fs::write(&fixture, tests)?;
    let config = directory.join("vitest.config.mjs");
    let vitest = source.join("node_modules/vitest/dist/index.js");
    std::fs::write(
        &config,
        format!(
            "export default {{ resolve: {{ alias: {{ vitest: {} }} }}, test: {{ include: [{}], maxWorkers: 1 }} }};\n",
            serde_json::to_string(&vitest.to_string_lossy())?,
            serde_json::to_string(&fixture.to_string_lossy())?
        ),
    )?;
    let mut command = Command::new("node");
    command
        .arg(source.join("node_modules/vitest/vitest.mjs"))
        .args(["run", "--config"])
        .arg(config)
        .current_dir(&root);
    if source_regressions {
        command.args([
            "-t",
            "preserves immediate-remount|owns pending|clears retained",
        ]);
    }
    let status = command.status()?;
    anyhow::ensure!(
        status.success(),
        "source registry corpus failed against browser WASM"
    );
    Ok(())
}

fn write_corpus_adapters(root: &Path, directory: &Path) -> anyhow::Result<(PathBuf, PathBuf)> {
    let adapter = directory.join("adapter.mjs");
    let production_wrapper = std::fs::read_to_string(root.join("vendor/cordis/lib/index.js"))?;
    let wrapper = production_wrapper
        .replace(
            "from './client.js'",
            &format!(
                "from {}",
                serde_json::to_string(&root.join("vendor/cordis/lib/client.js").to_string_lossy())?
            ),
        )
        .replace(
            "new URL('./client_bg.wasm', import.meta.url)",
            &format!(
                "await readFile({})",
                serde_json::to_string(
                    &root
                        .join("vendor/cordis/lib/client_bg.wasm")
                        .to_string_lossy()
                )?
            ),
        );
    std::fs::write(
        directory.join("cordis.mjs"),
        format!("import {{ readFile }} from 'node:fs/promises';\n{wrapper}"),
    )?;
    std::fs::write(
        &adapter,
        registry_driver::ADAPTER
            .replace("__ROOT__", &serde_json::to_string(&root.to_string_lossy())?),
    )?;
    let gateway_adapter = directory.join("gateway.mjs");
    std::fs::write(
        &gateway_adapter,
        format!(
            "import {{ readFileSync }} from 'node:fs';\nimport {{ runInThisContext }} from 'node:vm';\nimport * as cordis from './cordis.mjs';\nimport './adapter.mjs';\nlet row; window.__ModuleLoader__ = {{ load(value) {{ row = value; }} }};\nrunInThisContext(readFileSync({}, 'utf8'));\nconst plugin = row.factory(() => cordis);\nexport const apply = plugin.apply;\nexport const inject = plugin.inject;\n",
            serde_json::to_string(
                &root
                    .join("packages/api/gateway/lib/client.js")
                    .to_string_lossy()
            )?
        ),
    )?;
    Ok((adapter, gateway_adapter))
}

fn source_gateway_regressions(source: &Path, source_package: &Path) -> anyhow::Result<String> {
    let source_test = std::fs::read_to_string(source_package.join("tests/gateway.client.spec.ts"))?
        .replace(
            "'@deepseek-ai/cordis'",
            &serde_json::to_string(&source.join("vendor/cordis/lib/index.js").to_string_lossy())?,
        )
        .replace(
            "'@deepseek-ai/dsh-typert-registry'",
            &serde_json::to_string(
                &source
                    .join("packages/typert/registry/src/index.ts")
                    .to_string_lossy(),
            )?,
        )
        .replace(
            "'../src/client/index.ts'",
            &serde_json::to_string(&source_package.join("src/client/index.ts").to_string_lossy())?,
        )
        .replace(
            "'zod'",
            &serde_json::to_string(
                &source
                    .join("packages/typert/registry/node_modules/zod/index.js")
                    .to_string_lossy(),
            )?,
        );
    Ok(format!(
        "{source_test}\n{}",
        registry_driver::GATEWAY_ADDITIONAL
    ))
}

pub(super) fn browser_path(source: &Path) -> anyhow::Result<()> {
    super::verify_source(source)?;
    let metadata = super::cargo_metadata()?;
    let directory = metadata.target_directory.join("xtask/remote-browser-path");
    std::fs::create_dir_all(&directory)?;
    let driver = directory.join("path.mjs");
    std::fs::write(&driver, browser_driver::DRIVER)?;
    let host = metadata
        .target_directory
        .join("debug")
        .join(if cfg!(windows) {
            "seekdeep.exe"
        } else {
            "seekdeep"
        });
    anyhow::ensure!(
        host.is_file(),
        "build the Rust Host first: cargo build -p seekdeep"
    );
    let status = Command::new("node")
        .arg(&driver)
        .arg(source)
        .arg(&host)
        .arg(&directory)
        .current_dir(&metadata.workspace_root)
        .status()?;
    anyhow::ensure!(status.success(), "integrated browser Remote path failed");
    Ok(())
}

pub(super) fn codec_oracle(source: &Path) -> anyhow::Result<()> {
    super::verify_source(source)?;
    let metadata = super::cargo_metadata()?;
    let directory = metadata.target_directory.join("xtask/remote-codec-oracle");
    std::fs::create_dir_all(&directory)?;
    let driver = directory.join("codec.mjs");
    std::fs::write(&driver, codec_driver::DRIVER)?;
    let status = Command::new("node")
        .arg(driver)
        .arg(source)
        .current_dir(metadata.workspace_root)
        .status()?;
    anyhow::ensure!(
        status.success(),
        "built Remote metadata/codec differential failed"
    );
    Ok(())
}

pub(super) fn milestone(source: &Path) -> anyhow::Result<()> {
    let started = std::time::Instant::now();
    run(Some(source), true)?;
    let metadata = super::cargo_metadata()?;
    let build_started = std::time::Instant::now();
    let status = Command::new("cargo")
        .args(["build", "--locked", "-p", "seekdeep"])
        .env("CARGO_INCREMENTAL", "0")
        .env("CARGO_BUILD_JOBS", "2")
        .current_dir(&metadata.workspace_root)
        .status()?;
    anyhow::ensure!(status.success(), "Rust Host build failed");
    println!(
        "Remote milestone: native Host build {:.2}s",
        build_started.elapsed().as_secs_f64()
    );
    for (package, artifact, id, directory) in [
        (
            "seekdeep-cordis",
            "seekdeep_cordis",
            "@seekdeep-ai/cordis",
            "vendor/cordis/lib",
        ),
        (
            "seekdeep-client-foundation-wasm",
            "seekdeep_client_foundation_wasm",
            "@seekdeep-ai/seekdeep-typert-registry",
            "packages/typert/registry/lib",
        ),
        (
            "seekdeep-client-foundation-wasm",
            "seekdeep_client_foundation_wasm",
            "@seekdeep-ai/seekdeep-client-connection",
            "packages/client/connection/lib",
        ),
        (
            "seekdeep-client-foundation-wasm",
            "seekdeep_client_foundation_wasm",
            "@seekdeep-ai/seekdeep-api-gateway",
            "packages/api/gateway/lib",
        ),
        (
            "seekdeep-api-remotes-client",
            "seekdeep_api_remotes_client",
            "@seekdeep-ai/seekdeep-api-remotes",
            "packages/api/remotes/lib",
        ),
    ] {
        super::wasm_package_once(
            package,
            artifact,
            id,
            &metadata.workspace_root.join(directory),
        )?;
    }
    codec_oracle(source)?;
    registry_oracle(source)?;
    gateway_oracle(source, true)?;
    gateway_oracle(source, false)?;
    browser_path(source)?;
    println!(
        "Remote milestone passed in {:.2}s: generated contracts → browser Typert → Client gateway → Rust Host",
        started.elapsed().as_secs_f64()
    );
    Ok(())
}
