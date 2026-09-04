//! Sequential native compilation and symlink-free SDK carrier assembly.

use std::{
    fs::{self, File},
    io::Read as _,
    path::{Component, Path, PathBuf},
    process::Command,
};

use serde_json::{Value, json};

use super::{Arch, BuildOptions, Host, Platform, Target};

/// Source-compatible development entry path below the Node carrier root.
pub const ENTRY_BIN: &str =
    "node_modules/@seekdeep-ai/seekdeep-sdk-jsonrpc-demo/lib/packaged-bin.js";
/// Repository-owned native runtime data directory.
pub const RUNTIME_DIRECTORY: &str = "python/sdk-runtime/src/deepseek_harness_runtime/runtime";
const RUNTIME_BIN: &str = "seekdeep-jsonrpc-agent-packaged";
const HELPER_BIN: &str = "seekdeep-pty-spawn-helper";

/// Products selected by one successful or dry-run build request.
#[derive(Clone, Debug)]
pub struct BuildReport {
    /// Executables and required helper sidecars, in target order.
    pub products: Vec<PathBuf>,
    /// Architecture-qualified native libraries for the selected Python runtime wheels.
    pub binding_libraries: Vec<PathBuf>,
    /// The generated dev-only carrier directory.
    pub node_carrier: PathBuf,
}

struct Artifacts {
    runtime: PathBuf,
    helper: Option<PathBuf>,
    binding: PathBuf,
}

/// Builds the host development carrier and each requested native executable serially.
///
/// Dry runs do not launch commands or modify files. Existing generated Node-carrier
/// contents are replaced only inside the validated repository-owned runtime directory.
///
/// # Errors
/// Rejects unsupported hosts, cross-architecture Linux builds, malformed manifests,
/// stale/missing or wrong-architecture binaries, symlinked output paths, and failed commands.
pub fn build_executables(
    root: &Path,
    options: &BuildOptions,
    host: &Host,
) -> anyhow::Result<BuildReport> {
    options.validate()?;
    let root = root.canonicalize()?;
    let manifest: Value =
        serde_json::from_slice(&fs::read(root.join("python/sdk-runtime/package.json"))?)?;
    anyhow::ensure!(
        manifest["name"] == "seekdeep-jsonrpc-agent-pkg"
            && manifest["dependencies"]
                .as_object()
                .is_some_and(|value| !value.is_empty()),
        "build-exe-for-python-sdk: runtime manifest must declare the named non-empty runtime closure"
    );
    let runtime_directory = root.join(RUNTIME_DIRECTORY);
    let node_carrier = runtime_directory.join("node");
    let output = root.join("dist-exe");
    let products = product_paths(&output, &options.targets);
    let binding_libraries = options
        .targets
        .iter()
        .map(|target| output.join(target.binding_basename()))
        .collect::<Vec<_>>();
    println!(
        "build-exe-for-python-sdk: targets: {}",
        options
            .targets
            .iter()
            .map(Target::spec)
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!(
        "build-exe-for-python-sdk: staging: {}",
        node_carrier.display()
    );
    if options.dry_run {
        print_dry_run(options, host, &node_carrier, &runtime_directory, &products);
        return Ok(BuildReport {
            products,
            binding_libraries,
            node_carrier,
        });
    }
    let host_target = host.target()?;
    validate_host_targets(&host_target, &options.targets)?;
    let target_directory = cargo_target_directory(&root)?;
    let host_artifacts = compile(&root, &target_directory, &host_target, options.skip_build)?;
    validate_manifest(
        &host_artifacts.runtime,
        &manifest,
        &crate::repository_version(&root)?,
    )?;
    ensure_owned_directory(&root, &runtime_directory)?;
    stage_python_bindings(&root, &runtime_directory, &host_target, &host_artifacts)?;
    stage_node_carrier(&root, &node_carrier, &host_target, &host_artifacts)?;
    ensure_owned_directory(&root, &output)?;
    for target in &options.targets {
        let artifacts = if same_target(target, &host_target) {
            &host_artifacts
        } else {
            &compile(&root, &target_directory, target, options.skip_build)?
        };
        let product = output.join(target.basename());
        copy_executable(&artifacts.runtime, &product)?;
        if let Some(helper) = &artifacts.helper {
            copy_executable(helper, &helper_path(&product))?;
        }
        copy_executable(&artifacts.binding, &output.join(target.binding_basename()))?;
    }
    println!("build-exe-for-python-sdk: products:");
    for product in &products {
        let tenths = (u128::from(fs::metadata(product)?.len()) * 10 + 524_288) / 1_048_576;
        println!(
            "  {}  ({}.{:01} MB)",
            product.display(),
            tenths / 10,
            tenths % 10
        );
    }
    for product in products.iter().chain(&binding_libraries) {
        let destination = runtime_directory.join(
            product
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("product basename missing"))?,
        );
        copy_executable(product, &destination)?;
        println!("build-exe-for-python-sdk: synced {}", destination.display());
    }
    Ok(BuildReport {
        products,
        binding_libraries,
        node_carrier,
    })
}

fn same_target(left: &Target, right: &Target) -> bool {
    left.platform() == right.platform() && left.arch() == right.arch()
}

fn product_paths(output: &Path, targets: &[Target]) -> Vec<PathBuf> {
    targets
        .iter()
        .flat_map(|target| {
            let product = output.join(target.basename());
            if target.platform() == Platform::Macos {
                vec![product.clone(), helper_path(&product)]
            } else {
                vec![product]
            }
        })
        .collect()
}

fn helper_path(path: &Path) -> PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push("-spawn-helper");
    value.into()
}

fn validate_host_targets(host: &Target, targets: &[Target]) -> anyhow::Result<()> {
    for target in targets {
        anyhow::ensure!(
            target.platform() != Platform::Linux || same_target(host, target),
            "build-exe-for-python-sdk: build the Linux runtime on its target architecture; target {} does not match host {}.",
            target.platform_arch(),
            host.platform_arch()
        );
    }
    Ok(())
}

fn print_dry_run(
    options: &BuildOptions,
    host: &Host,
    node: &Path,
    runtime: &Path,
    products: &[PathBuf],
) {
    println!(
        "build-exe-for-python-sdk: [dry-run] cargo metadata --locked --format-version 1 --no-deps"
    );
    if options.skip_build {
        println!("build-exe-for-python-sdk: skipping Cargo compilation (--skip-build)");
    } else {
        let mut targets = Vec::new();
        if let Ok(host) = host.target() {
            targets.push(host);
        }
        for target in &options.targets {
            if !targets.iter().any(|known| same_target(known, target)) {
                targets.push(target.clone());
            }
        }
        for target in targets {
            println!(
                "build-exe-for-python-sdk: [dry-run] {}",
                format_command("cargo", &cargo_build_args(&target))
            );
        }
    }
    println!("build-exe-for-python-sdk: [dry-run] verify compiled runtime manifest");
    println!(
        "build-exe-for-python-sdk: [dry-run] generate Python runtime declarations and native hook binding"
    );
    for target in &options.targets {
        println!(
            "build-exe-for-python-sdk: [dry-run] stage {}",
            target.binding_basename()
        );
    }
    println!(
        "build-exe-for-python-sdk: [dry-run] replace generated Node carrier {}",
        node.display()
    );
    println!(
        "build-exe-for-python-sdk: [dry-run] write {}",
        node.join(ENTRY_BIN).display()
    );
    println!("build-exe-for-python-sdk: [dry-run] would produce:");
    for product in products {
        println!("  {}", product.display());
        println!(
            "build-exe-for-python-sdk: [dry-run] cp {} {}",
            product.display(),
            runtime
                .join(product.file_name().unwrap_or_default())
                .display()
        );
    }
}

fn cargo_target_directory(root: &Path) -> anyhow::Result<PathBuf> {
    let output = Command::new("cargo")
        .args(["metadata", "--locked", "--format-version", "1", "--no-deps"])
        .env("CI", "true")
        .env("CARGO_INCREMENTAL", "0")
        .current_dir(root)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "build-exe-for-python-sdk: cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: Value = serde_json::from_slice(&output.stdout)?;
    metadata["target_directory"]
        .as_str()
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("Cargo metadata has no target directory"))
}

fn cargo_build_args(target: &Target) -> Vec<String> {
    let mut arguments = vec![
        "build",
        "--locked",
        "--release",
        "--target",
        target.rust_target(),
        "-p",
        "seekdeep-sdk-jsonrpc-demo",
        "--bin",
        RUNTIME_BIN,
        "-p",
        "seekdeep-python-sdk-ffi",
        "--lib",
    ];
    if target.platform() == Platform::Macos {
        arguments.extend(["-p", "seekdeep-pty-spawn-helper", "--bin", HELPER_BIN]);
    }
    arguments.into_iter().map(str::to_owned).collect()
}

fn compile(
    root: &Path,
    target_directory: &Path,
    target: &Target,
    skip: bool,
) -> anyhow::Result<Artifacts> {
    if skip {
        println!(
            "build-exe-for-python-sdk: skipping Cargo compilation for {} (--skip-build)",
            target.spec()
        );
    } else {
        let arguments = cargo_build_args(target);
        println!(
            "build-exe-for-python-sdk: build {}: {}",
            target.spec(),
            format_command("cargo", &arguments)
        );
        let mut command = Command::new("cargo");
        command
            .args(&arguments)
            .current_dir(root)
            .env("CI", "true")
            .env("CARGO_INCREMENTAL", "0");
        if target.platform() == Platform::Macos
            && std::env::var_os("MACOSX_DEPLOYMENT_TARGET").is_none()
        {
            command.env("MACOSX_DEPLOYMENT_TARGET", "13.5");
        }
        let status = command.status()?;
        anyhow::ensure!(
            status.success(),
            "build-exe-for-python-sdk: build {} failed ({status}): {}",
            target.spec(),
            format_command("cargo", &arguments)
        );
    }
    let directory = target_directory.join(target.rust_target()).join("release");
    let artifacts = Artifacts {
        runtime: directory.join(RUNTIME_BIN),
        helper: (target.platform() == Platform::Macos).then(|| directory.join(HELPER_BIN)),
        binding: directory.join(target.cargo_binding_basename()),
    };
    validate_native_artifact(&artifacts.runtime, target)?;
    if let Some(helper) = &artifacts.helper {
        validate_native_artifact(helper, target)?;
    }
    validate_native_library(&artifacts.binding, target)?;
    Ok(artifacts)
}

/// Confirms the executable file's native format and architecture before copying it.
///
/// # Errors
/// Rejects absent or non-executable artifacts, truncated headers, and foreign formats or architectures.
pub fn validate_native_artifact(path: &Path, target: &Target) -> anyhow::Result<()> {
    validate_native_file(path, target, false)
}

/// Confirms a C ABI library's native format and architecture without requiring executable mode bits.
///
/// # Errors
/// Requires shared-object or dynamic-library headers with the expected architecture.
pub fn validate_native_library(path: &Path, target: &Target) -> anyhow::Result<()> {
    validate_native_file(path, target, true)
}

fn validate_native_file(path: &Path, target: &Target, library: bool) -> anyhow::Result<()> {
    anyhow::ensure!(
        path.is_file(),
        "build-exe-for-python-sdk: compiled product {} is missing; run without --skip-build",
        path.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        anyhow::ensure!(
            library || fs::metadata(path)?.permissions().mode() & 0o100 != 0,
            "build-exe-for-python-sdk: product {} is not executable",
            path.display()
        );
    }
    let mut header = [0_u8; 32];
    File::open(path)?.read_exact(&mut header)?;
    let matches = match target.platform() {
        Platform::Linux => {
            &header[..4] == b"\x7fELF"
                && header[4] == 2
                && header[5] == 1
                && if library {
                    u16::from_le_bytes([header[16], header[17]]) == 3
                } else {
                    matches!(u16::from_le_bytes([header[16], header[17]]), 2 | 3)
                }
                && u16::from_le_bytes([header[18], header[19]])
                    == match target.arch() {
                        Arch::X64 => 62,
                        Arch::Arm64 => 183,
                    }
        }
        Platform::Macos => {
            header[..4] == [0xcf, 0xfa, 0xed, 0xfe]
                && u32::from_le_bytes([header[4], header[5], header[6], header[7]])
                    == match target.arch() {
                        Arch::X64 => 0x0100_0007,
                        Arch::Arm64 => 0x0100_000c,
                    }
                && u32::from_le_bytes([header[12], header[13], header[14], header[15]])
                    == if library { 6 } else { 2 }
        }
    };
    anyhow::ensure!(
        matches,
        "build-exe-for-python-sdk: product {} does not match native target {}",
        path.display(),
        target.platform_arch()
    );
    Ok(())
}

fn stage_python_bindings(
    root: &Path,
    runtime: &Path,
    target: &Target,
    artifacts: &Artifacts,
) -> anyhow::Result<()> {
    let package = runtime
        .parent()
        .ok_or_else(|| anyhow::anyhow!("runtime package parent is absent"))?;
    ensure_owned_directory(root, package)?;
    let name = target.binding_basename();
    copy_executable(&artifacts.binding, &runtime.join(&name))?;
    for (filename, text) in seekdeep_python_sdk::bindings::runtime_bindings(&name)? {
        write_generated_binding(&package.join(filename), &text)?;
    }
    write_generated_binding(
        &root.join("python/sdk-runtime/hatch_build.py"),
        crate::staging::NATIVE_HATCH_BINDING,
    )?;
    println!("build-exe-for-python-sdk: generated Rust-backed Python runtime bindings");
    Ok(())
}

fn write_generated_binding(path: &Path, text: &str) -> anyhow::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            anyhow::ensure!(
                metadata.is_file() && !metadata.file_type().is_symlink(),
                "generated Python binding is not an owned file: {}",
                path.display()
            );
            let current = fs::read_to_string(path)?;
            anyhow::ensure!(
                current.starts_with("# Generated by seekdeep-python-sdk;")
                    || current.starts_with(
                        "\"\"\"Generated binding to the compiled Rust runtime-wheel policy."
                    ),
                "refusing to overwrite an authored Python binding: {}",
                path.display()
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    fs::write(path, text)?;
    Ok(())
}

fn validate_manifest(binary: &Path, manifest: &Value, version: &str) -> anyhow::Result<()> {
    let output = Command::new(binary)
        .env_clear()
        .env("SEEKDEEP_INTERNAL_RUNTIME_MANIFEST", "1")
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "build-exe-for-python-sdk: compiled runtime manifest failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let compiled: Value = serde_json::from_slice(&output.stdout)?;
    anyhow::ensure!(
        compiled["formatVersion"] == 1
            && compiled["binary"] == "seekdeep-sdk-jsonrpc-demo"
            && compiled["version"] == version
            && compiled["runtimeManifest"] == *manifest,
        "build-exe-for-python-sdk: compiled runtime manifest is stale or incompatible; rebuild without --skip-build"
    );
    let plugins = compiled["plugins"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("compiled runtime has no plugin inventory"))?;
    for plugin in plugins {
        let name = plugin
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("compiled plugin name is not a string"))?;
        anyhow::ensure!(
            manifest["dependencies"].get(name).is_some(),
            "compiled runtime exposes undeclared plugin {name}"
        );
    }
    Ok(())
}

fn stage_node_carrier(
    root: &Path,
    node: &Path,
    host: &Target,
    artifacts: &Artifacts,
) -> anyhow::Result<()> {
    validate_output_ancestors(root, node)?;
    if node.exists() {
        fs::remove_dir_all(node)?;
    }
    fs::create_dir_all(node.join("native"))?;
    let runtime = node.join("native").join(host.basename());
    copy_executable(&artifacts.runtime, &runtime)?;
    if let Some(helper) = &artifacts.helper {
        copy_executable(helper, &helper_path(&runtime))?;
    }
    let entry = node.join(ENTRY_BIN);
    fs::create_dir_all(
        entry
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Node entry has no parent"))?,
    )?;
    let relative = format!("../../../../native/{}", host.basename());
    let script = format!(
        "// Generated launch binding; runtime behavior is compiled Rust.\nimport {{ fileURLToPath }} from 'node:url';\nimport process from 'node:process';\nconst executable = fileURLToPath(new URL({}, import.meta.url));\nprocess.execve(executable, [executable, ...process.argv.slice(2)], process.env);\n",
        serde_json::to_string(&relative)?
    );
    fs::write(entry, script)?;
    let version = crate::repository_version(root)?;
    let package = json!({"name":"@seekdeep-ai/seekdeep-sdk-jsonrpc-demo","version":version,"type":"module","exports":{".":"./lib/packaged-bin.js"}});
    fs::write(
        node.join("node_modules/@seekdeep-ai/seekdeep-sdk-jsonrpc-demo/package.json"),
        format!("{}\n", serde_json::to_string_pretty(&package)?),
    )?;
    let carrier = json!({"name":"seekdeep-jsonrpc-agent-pkg","version":version,"private":true,"type":"module","bin":ENTRY_BIN});
    fs::write(
        node.join("package.json"),
        format!("{}\n", serde_json::to_string_pretty(&carrier)?),
    )?;
    Ok(())
}

fn validate_output_ancestors(root: &Path, output: &Path) -> anyhow::Result<()> {
    let relative = output
        .strip_prefix(root)
        .map_err(|_| anyhow::anyhow!("generated output is outside the repository"))?;
    anyhow::ensure!(
        relative.components().next().is_some(),
        "refusing to replace the repository root"
    );
    let mut current = root.to_owned();
    for component in relative.components() {
        anyhow::ensure!(
            matches!(component, Component::Normal(_)),
            "generated output has a non-normal path component"
        );
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => anyhow::ensure!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "build-exe-for-python-sdk: generated output path {} is not an owned directory",
                current.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn ensure_owned_directory(root: &Path, output: &Path) -> anyhow::Result<()> {
    validate_output_ancestors(root, output)?;
    fs::create_dir_all(output)?;
    Ok(())
}

fn copy_executable(source: &Path, destination: &Path) -> anyhow::Result<()> {
    if fs::symlink_metadata(destination).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        anyhow::bail!(
            "build-exe-for-python-sdk: refusing to overwrite executable symlink {}",
            destination.display()
        );
    }
    fs::copy(source, destination)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(
            destination,
            fs::Permissions::from_mode(fs::metadata(source)?.permissions().mode() & 0o777),
        )?;
    }
    Ok(())
}

fn format_command(command: &str, arguments: &[String]) -> String {
    std::iter::once(command)
        .chain(arguments.iter().map(String::as_str))
        .map(|part| {
            if part.contains(' ') {
                serde_json::to_string(part).expect("command argument JSON")
            } else {
                part.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_outputs_reject_root_escape_and_symlinked_parents() {
        let root = tempfile::tempdir().unwrap();
        assert!(validate_output_ancestors(root.path(), root.path()).is_err());
        assert!(validate_output_ancestors(root.path(), &root.path().join("../outside")).is_err());
        #[cfg(unix)]
        {
            let outside = tempfile::tempdir().unwrap();
            fs::write(outside.path().join("preserve"), b"owned elsewhere").unwrap();
            std::os::unix::fs::symlink(outside.path(), root.path().join("linked")).unwrap();
            assert!(ensure_owned_directory(root.path(), &root.path().join("linked/node")).is_err());
            assert_eq!(
                fs::read(outside.path().join("preserve")).unwrap(),
                b"owned elsewhere"
            );
        }
    }

    #[test]
    fn linux_builds_require_their_native_host_but_macos_cross_targets_remain_selectable() {
        let host = Target::parse("node24-macos-arm64").unwrap();
        assert!(
            validate_host_targets(&host, &[Target::parse("node24-linux-x64").unwrap()]).is_err()
        );
        assert!(
            validate_host_targets(&host, &[Target::parse("node24-macos-x64").unwrap()]).is_ok()
        );
        let linux = Target::parse("node24-linux-arm64").unwrap();
        assert!(validate_host_targets(&linux, std::slice::from_ref(&linux)).is_ok());
    }

    #[test]
    fn cargo_commands_request_the_helper_only_for_macos_and_never_clean() {
        for (target, helper) in [("node24-linux-x64", false), ("node24-macos-arm64", true)] {
            let arguments = cargo_build_args(&Target::parse(target).unwrap());
            assert!(arguments.iter().any(|argument| argument == "--locked"));
            assert!(arguments.iter().any(|argument| argument == "--release"));
            assert_eq!(
                arguments.iter().any(|argument| argument == HELPER_BIN),
                helper
            );
            assert!(!arguments.iter().any(|argument| argument == "clean"));
        }
    }
}
