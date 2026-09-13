//! Stages the compiled Rust Node compatibility boundary for a release artifact.

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

use seekdeep_code_runtime_worker_thread::node_assets::{
    DEPENDENCIES, verify_manifest, write_manifest,
};

static NEXT_STAGE: AtomicU64 = AtomicU64::new(1);

fn main() -> anyhow::Result<()> {
    let mut arguments = std::env::args_os().skip(1);
    let wasm = arguments.next().map(PathBuf::from).ok_or_else(|| {
        anyhow::anyhow!("usage: package-code-runtime-node <compiled.wasm> <output-directory>")
    })?;
    let output = arguments.next().map(PathBuf::from).ok_or_else(|| {
        anyhow::anyhow!("usage: package-code-runtime-node <compiled.wasm> <output-directory>")
    })?;
    anyhow::ensure!(arguments.next().is_none(), "unexpected packaging argument");
    anyhow::ensure!(
        wasm.is_file(),
        "compiled Node runtime is missing: {}",
        wasm.display()
    );
    let output = std::path::absolute(output)?;
    let staging = Staging::new(
        output
            .parent()
            .ok_or_else(|| anyhow::anyhow!("runtime output needs a parent directory"))?,
    )?;
    package(&wasm, &staging.0)?;
    publish(&staging.0, &output)?;
    println!("{}", output.canonicalize()?.display());
    Ok(())
}

fn package(wasm: &Path, output: &Path) -> anyhow::Result<()> {
    let status = Command::new("wasm-bindgen")
        .arg(wasm)
        .args([
            "--target",
            "nodejs",
            "--out-name",
            "seekdeep_code_runtime_node",
            "--out-dir",
        ])
        .arg(output)
        .status()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                anyhow::anyhow!(
                    "wasm-bindgen is not installed; the Node code-runtime boundary needs the CLI \
                     matching the workspace crate: cargo install --locked wasm-bindgen-cli \
                     --version 0.2.127"
                )
            } else {
                anyhow::Error::from(error).context("cannot start wasm-bindgen")
            }
        })?;
    anyhow::ensure!(
        status.success(),
        "wasm-bindgen failed for the Node code-runtime boundary"
    );
    std::fs::write(
        output.join("loader.mjs"),
        include_bytes!("../../node/loader.mjs"),
    )?;
    std::fs::write(
        output.join("plugin-loader.cjs"),
        include_bytes!("../../node/plugin-loader.cjs"),
    )?;
    std::fs::write(
        output.join("wasm-runtime.cjs"),
        include_bytes!("../../node/wasm-runtime.cjs"),
    )?;
    std::fs::write(
        output.join("package.json"),
        b"{\"private\":true,\"type\":\"commonjs\"}\n",
    )?;
    std::fs::write(
        output.join("snippets/package.json"),
        b"{\"private\":true,\"type\":\"module\"}\n",
    )?;
    copy_dependencies(output)?;
    verify(output)?;
    write_manifest(output)?;
    verify_manifest(output)?;
    Ok(())
}

fn copy_dependencies(output: &Path) -> anyhow::Result<()> {
    let root = std::env::var_os("SEEKDEEP_NODE_RUNTIME_DEPENDENCIES_DIR").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../support/node-runtime-dependencies"),
        PathBuf::from,
    );
    for (name, version) in DEPENDENCIES {
        let source = root.join("node_modules").join(name).canonicalize()?;
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(source.join("package.json"))?)?;
        anyhow::ensure!(
            manifest["name"] == *name && manifest["version"] == *version,
            "Node runtime requires {name}@{version}"
        );
        anyhow::ensure!(
            source.join("LICENSE").is_file(),
            "Node dependency {name} is missing its license"
        );
        copy_tree(&source, &output.join("node_modules").join(name))?;
    }
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(destination)?;
    let mut entries = std::fs::read_dir(source)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let kind = entry.file_type()?;
        anyhow::ensure!(
            !kind.is_symlink(),
            "Node dependency contains a symlink: {}",
            entry.path().display()
        );
        let destination = destination.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &destination)?;
        } else {
            anyhow::ensure!(
                kind.is_file(),
                "Node dependency entry is not a regular file"
            );
            std::fs::copy(entry.path(), destination)?;
        }
    }
    Ok(())
}

fn verify(output: &Path) -> anyhow::Result<()> {
    let script = r"
const path = require('node:path');
const fs = require('node:fs');
const { createRequire } = require('node:module');
const directory = fs.realpathSync(path.resolve(process.argv[1]));
const local = createRequire(path.join(directory, 'package.json'));
const runtime = local('./wasm-runtime.cjs');
if (typeof runtime.start !== 'function') throw new Error('missing compiled Node runtime start export');
if (typeof runtime.start_loader !== 'function') throw new Error('missing compiled Node plugin loader start export');
for (const [name, version] of [['chokidar', '4.0.3'], ['readdirp', '4.1.2']]) {
  const packageDirectory = path.join(directory, 'node_modules', name);
  const manifest = JSON.parse(fs.readFileSync(path.join(packageDirectory, 'package.json'), 'utf8'));
  if (manifest.name !== name || manifest.version !== version) throw new Error('incorrect packaged dependency: ' + name);
  const resolver = name === 'readdirp' ? createRequire(path.join(directory, 'node_modules/chokidar/package.json')) : local;
  if (!resolver.resolve(name).startsWith(packageDirectory + path.sep)) throw new Error('dependency escaped the runtime package: ' + name);
}
if (typeof local('chokidar').watch !== 'function') throw new Error('missing native watcher export');
";
    let status =
        Command::new(std::env::var_os("SEEKDEEP_NODE_BINARY").unwrap_or_else(|| "node".into()))
            .args(["-e", script])
            .arg(output)
            .status()?;
    anyhow::ensure!(
        status.success(),
        "compiled Node code-runtime package failed to load"
    );
    Ok(())
}

struct Staging(PathBuf);

impl Staging {
    fn new(parent: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(parent)?;
        loop {
            let sequence = NEXT_STAGE.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!(
                ".seekdeep-node-package-{}-{sequence}",
                std::process::id()
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn publish(staging: &Path, output: &Path) -> anyhow::Result<()> {
    if !output.exists() {
        std::fs::rename(staging, output)?;
        return Ok(());
    }
    let metadata = std::fs::symlink_metadata(output)?;
    anyhow::ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "runtime output must be a directory, not a link"
    );
    let empty = std::fs::read_dir(output)?.next().is_none();
    anyhow::ensure!(
        empty
            || output.join("loader.mjs").is_file()
                && output.join("seekdeep_code_runtime_node_bg.wasm").is_file(),
        "refusing to replace a directory that is not a packaged Node runtime"
    );
    let backup = Staging::new(
        output
            .parent()
            .ok_or_else(|| anyhow::anyhow!("runtime output has no parent"))?,
    )?;
    std::fs::remove_dir(&backup.0)?;
    std::fs::rename(output, &backup.0)?;
    if let Err(error) = std::fs::rename(staging, output) {
        if let Err(restore) = std::fs::rename(&backup.0, output) {
            let retained = backup.0.clone();
            std::mem::forget(backup);
            anyhow::bail!(
                "cannot publish runtime: {error}; cannot restore output: {restore}; previous package retained at {}",
                retained.display()
            );
        }
        return Err(error.into());
    }
    Ok(())
}
