//! Build the Rust/WASM Node compatibility entry distributed beside native packages.

use std::{collections::BTreeMap, fs, path::PathBuf};

use anyhow::{Result, bail};

use crate::{
    process::{CommandSpec, Runner, run_checked},
    repo::{Repository, verify_entry_lib},
};

/// Compile Rust behavior, generate the Node binding, and stage the entry package.
///
/// # Errors
///
/// Returns Cargo, wasm-bindgen, filesystem, and entry prepack failures.
pub fn build_entry(repo: &Repository, runner: &mut dyn Runner) -> Result<PathBuf> {
    let workspace = repo.root.join("../..");
    let package = repo.root.join("packages/entry");
    let target = repo.root.join(".release/entry-target");
    run_checked(
        runner,
        &CommandSpec {
            program: "cargo".to_owned(),
            args: vec![
                "build".to_owned(),
                "--release".to_owned(),
                "--package".to_owned(),
                "seekdeep-landlock-entry-wasm".to_owned(),
                "--target".to_owned(),
                "wasm32-unknown-unknown".to_owned(),
                "--target-dir".to_owned(),
                target.to_string_lossy().into_owned(),
            ],
            cwd: workspace.clone(),
            env: BTreeMap::from([("CARGO_INCREMENTAL".to_owned(), "0".to_owned())]),
            capture: false,
            max_buffer: 1_048_576,
            inherit_stdin: false,
        },
    )?;
    let staging = tempfile::Builder::new()
        .prefix("landlock-entry-")
        .tempdir_in(&package)?;
    run_checked(
        runner,
        &CommandSpec {
            program: "wasm-bindgen".to_owned(),
            args: vec![
                target
                    .join("wasm32-unknown-unknown/release/seekdeep_landlock_entry_wasm.wasm")
                    .to_string_lossy()
                    .into_owned(),
                "--target".to_owned(),
                "nodejs".to_owned(),
                "--out-dir".to_owned(),
                staging.path().to_string_lossy().into_owned(),
                "--out-name".to_owned(),
                "seekdeep_landlock_entry".to_owned(),
            ],
            cwd: workspace,
            env: BTreeMap::new(),
            capture: false,
            max_buffer: 1_048_576,
            inherit_stdin: false,
        },
    )?;
    fs::rename(
        staging.path().join("seekdeep_landlock_entry.js"),
        staging.path().join("seekdeep_landlock_entry.cjs"),
    )?;
    fs::copy(package.join("index.js"), staging.path().join("index.js"))?;
    fs::copy(
        package.join("index.d.ts"),
        staging.path().join("index.d.ts"),
    )?;
    for (source, name) in [
        ("crates/landlock-run/src/main.rs", "landlock-run.main.rs"),
        ("crates/landlock-run/src/lib.rs", "landlock-run.lib.rs"),
        ("crates/landlock-run/Cargo.toml", "landlock-run.Cargo.toml"),
        ("Cargo.toml", "seekdeep-workspace.Cargo.toml"),
        ("Cargo.lock", "seekdeep-workspace.Cargo.lock"),
        (
            "rust-toolchain.toml",
            "seekdeep-workspace.rust-toolchain.toml",
        ),
        ("LICENSE", "seekdeep-harness.LICENSE"),
    ] {
        fs::copy(
            repo.root.join("../..").join(source),
            staging.path().join(name),
        )?;
    }
    let output = package.join("lib");
    fs::create_dir_all(&output)?;
    for entry in fs::read_dir(staging.path())? {
        let entry = entry?;
        fs::copy(entry.path(), output.join(entry.file_name()))?;
    }
    runner.log(&verify_entry_wasm(&package)?);
    Ok(output)
}

/// Apply the entry prepack gate to the complete Rust/WASM payload.
///
/// # Errors
///
/// Returns when an entry export, its Node loader, compiled WASM, launcher source, or license is absent.
pub fn verify_entry_wasm(package: &std::path::Path) -> Result<String> {
    let message = verify_entry_lib(package)?;
    for file in [
        "lib/seekdeep_landlock_entry.cjs",
        "lib/seekdeep_landlock_entry_bg.wasm",
    ] {
        if !package.join(file).is_file() {
            bail!("verify-entry-lib: missing {file} — run `pnpm build:ts` before packing.");
        }
    }
    let wasm = fs::read(package.join("lib/seekdeep_landlock_entry_bg.wasm"))?;
    if !wasm.starts_with(b"\0asm\x01\0\0\0") {
        bail!(
            "verify-entry-lib: lib/seekdeep_landlock_entry_bg.wasm is not a compiled WebAssembly module — run `pnpm build:ts` before packing."
        );
    }
    for file in [
        "lib/landlock-run.main.rs",
        "lib/landlock-run.lib.rs",
        "lib/landlock-run.Cargo.toml",
        "lib/seekdeep-workspace.Cargo.toml",
        "lib/seekdeep-workspace.Cargo.lock",
        "lib/seekdeep-workspace.rust-toolchain.toml",
        "lib/seekdeep-harness.LICENSE",
    ] {
        if !package.join(file).is_file() {
            bail!("verify-entry-lib: missing {file} — run `pnpm build:ts` before packing.");
        }
    }
    Ok(message)
}
