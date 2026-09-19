use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail, ensure};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::{
    process::Runner,
    repo::{Repository, read_json},
};

use super::{
    capture_checked, command, manifest_string, path_string, read_packed_manifest, run_checked,
    tarball_name,
};

const ENTRY_PACKAGE_NAME: &str = "@seekdeep-ai/node-addon-landlock-run";

/// Selects packed payload coverage and the required kernel-enforcement proof.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackedInstallOptions {
    /// Directory containing all archives needed by the selected coverage.
    pub tarball_dir: PathBuf,
    /// Whether payload presence is required only for the host's platform and entries.
    pub current_platform_only: bool,
    /// Node-style platform and architecture name used to select the installed payload.
    pub host_platform: String,
    /// Whether an unenforcing Linux kernel must fail the rehearsal.
    pub require_landlock: bool,
}

/// Evidence produced by a consumer installation assembled exclusively from local tarballs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallVerification {
    /// Number of package archives whose manifests passed the payload gates.
    pub checked_packages: usize,
    /// Number of installed binaries matched byte-for-byte to workspace builds.
    pub byte_pinned_binaries: usize,
    /// Whether both denied and granted writes were checked under the installed launcher.
    pub confinement_proved: bool,
    /// Enforcement result returned by the installed entry's functional probe.
    pub enforcement: String,
}

fn tarball_path(directory: &Path, manifest: &Value) -> Result<PathBuf> {
    let tarball = directory.join(tarball_name(manifest)?);
    if !tarball.exists() {
        bail!("missing packed tarball: {}", tarball.display());
    }
    Ok(tarball)
}

fn js_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

fn verify_packed_manifest(packed: &Value) -> Result<()> {
    let name = manifest_string(packed, "name")?;
    for script in ["preinstall", "install", "postinstall", "prepare"] {
        if packed["scripts"].get(script).is_some_and(js_truthy) {
            bail!(
                "{name}: packed manifest carries a \"{script}\" lifecycle script — this family has no install fallback"
            );
        }
    }
    for field in ["dependencies", "optionalDependencies", "peerDependencies"] {
        if let Some(dependencies) = packed.get(field).and_then(Value::as_object) {
            for (dependency, version) in dependencies {
                let version = version.as_str().with_context(|| {
                    format!("{name}: packed {field} has a non-string version for {dependency}")
                })?;
                if version.contains("workspace:") {
                    bail!(
                        "{name}: packed {field} still uses the workspace protocol: {dependency}@{version}"
                    );
                }
            }
        }
    }
    Ok(())
}

fn package_install_dir(consumer: &Path, name: &str) -> PathBuf {
    name.split('/')
        .fold(consumer.join("node_modules"), |path, component| {
            path.join(component)
        })
}

fn unpack_tarball(
    repository: &Repository,
    tarball_dir: &Path,
    consumer: &Path,
    manifest: &Value,
    runner: &mut dyn Runner,
) -> Result<()> {
    let extraction = tempfile::Builder::new()
        .prefix("extract-")
        .tempdir_in(consumer)?;
    run_checked(
        runner,
        &command(
            "tar",
            vec![
                "-xzf".to_owned(),
                path_string(&tarball_path(tarball_dir, manifest)?),
                "-C".to_owned(),
                path_string(extraction.path()),
            ],
            &repository.root,
            false,
        ),
    )?;
    let name = manifest_string(manifest, "name")?;
    let destination = package_install_dir(consumer, name);
    super::remove_if_present(&destination)?;
    fs::create_dir_all(
        destination
            .parent()
            .context("installed package has no parent")?,
    )?;
    fs::rename(extraction.path().join("package"), &destination)?;
    runner.log(&format!(
        "Unpacked {name} -> {}",
        super::relative_path(consumer, &destination)
    ));
    Ok(())
}

fn node_args(value: &Value) -> Result<Vec<String>> {
    value
        .as_array()
        .context("installed grantArgs did not return an array")?
        .iter()
        .map(|argument| {
            argument
                .as_str()
                .map(str::to_owned)
                .context("installed grantArgs returned a non-string argument")
        })
        .collect()
}

fn executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        nix::unistd::access(path, nix::unistd::AccessFlags::X_OK).is_ok()
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

struct VerifiedPayload {
    entry: Value,
    current: Option<(PathBuf, Value)>,
    checked_packages: usize,
}

fn verify_payloads(
    repository: &Repository,
    options: &PackedInstallOptions,
    runner: &mut dyn Runner,
) -> Result<VerifiedPayload> {
    let directories = repository.package_dirs()?;
    let manifests = directories
        .into_iter()
        .map(|directory| {
            let manifest = read_json(&repository.root.join(&directory).join("package.json"))?;
            Ok((directory, manifest))
        })
        .collect::<Result<Vec<_>>>()?;
    let entry = manifests
        .iter()
        .find(|(_, manifest)| manifest["name"].as_str() == Some(ENTRY_PACKAGE_NAME))
        .map(|(_, manifest)| manifest)
        .with_context(|| format!("missing source manifest for {ENTRY_PACKAGE_NAME}"))?;
    let platforms = repository.platform_dirs()?;
    let entries = repository.entry_dirs()?;
    let expected_platform_name = format!("{ENTRY_PACKAGE_NAME}-{}", options.host_platform);
    let current = manifests.iter().find(|(directory, manifest)| {
        platforms.contains(directory) && manifest["name"].as_str() == Some(&expected_platform_name)
    });
    let expected = manifests
        .iter()
        .filter(|(directory, _)| {
            !options.current_platform_only
                || entries.contains(directory)
                || current.is_some_and(|(current_directory, _)| current_directory == directory)
        })
        .collect::<Vec<_>>();
    for (_, manifest) in &expected {
        tarball_path(&options.tarball_dir, manifest)?;
    }
    let packed_entry = read_packed_manifest(
        &tarball_path(&options.tarball_dir, entry)?,
        &repository.root,
        runner,
    )?;
    let mut platform_names = manifests
        .iter()
        .filter(|(directory, _)| platforms.contains(directory))
        .map(|(_, manifest)| Ok(manifest_string(manifest, "name")?.to_owned()))
        .collect::<Result<Vec<_>>>()?;
    platform_names.sort();
    let mut optional_names = packed_entry["optionalDependencies"]
        .as_object()
        .map(|optional| optional.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    optional_names.sort();
    if optional_names != platform_names {
        bail!(
            "packed entry optionalDependencies mismatch\nactual:\n{}\nexpected:\n{}",
            optional_names.join("\n"),
            platform_names.join("\n")
        );
    }
    for (_, manifest) in &expected {
        let packed = read_packed_manifest(
            &tarball_path(&options.tarball_dir, manifest)?,
            &repository.root,
            runner,
        )?;
        verify_packed_manifest(&packed)?;
    }
    Ok(VerifiedPayload {
        entry: entry.clone(),
        current: current.cloned(),
        checked_packages: expected.len(),
    })
}

fn install_consumer(
    repository: &Repository,
    options: &PackedInstallOptions,
    payload: &VerifiedPayload,
    runner: &mut dyn Runner,
) -> Result<(tempfile::TempDir, usize)> {
    let consumer = tempfile::Builder::new()
        .prefix("nalr-packed-install-")
        .tempdir()?;
    fs::write(
        consumer.path().join("package.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&json!({
                "name": "nalr-packed-install-check",
                "version": "0.0.0",
                "private": true,
                "type": "module"
            }))?
        ),
    )?;
    runner.log(&format!(
        "Verifying packed install in {}",
        consumer.path().display()
    ));
    unpack_tarball(
        repository,
        &options.tarball_dir,
        consumer.path(),
        &payload.entry,
        runner,
    )?;
    let mut byte_pinned = 0;
    if let Some((directory, manifest)) = &payload.current {
        unpack_tarball(
            repository,
            &options.tarball_dir,
            consumer.path(),
            manifest,
            runner,
        )?;
        let prebuilds = read_json(&repository.root.join(directory).join("prebuilds.json"))?;
        let binaries = prebuilds["binaries"]
            .as_array()
            .context("prebuilds lacks binaries")?;
        for binary in binaries {
            let path = manifest_string(binary, "path")?;
            let workspace = repository.root.join(directory).join(path);
            let installed =
                package_install_dir(consumer.path(), manifest_string(manifest, "name")?).join(path);
            if Sha256::digest(fs::read(workspace)?) != Sha256::digest(fs::read(installed)?) {
                bail!("installed {path} differs from the workspace build it was packed from");
            }
            runner.log(&format!("Byte-pinned {path} against the workspace build"));
            byte_pinned += 1;
        }
    } else if options.host_platform.starts_with("linux-") {
        bail!(
            "linux host without a platform package in the matrix: {}",
            options.host_platform
        );
    }
    Ok((consumer, byte_pinned))
}

fn entry_call(
    consumer: &Path,
    method: &str,
    arguments: &Value,
    runner: &mut dyn Runner,
) -> Result<Value> {
    let mut spec = command(
        "node",
        vec![
            path_string(&consumer.join("driver.mjs")),
            method.to_owned(),
            serde_json::to_string(arguments)?,
        ],
        consumer,
        true,
    );
    spec.max_buffer = 64 * 1024 * 1024;
    let output = capture_checked(runner, &spec)?;
    Ok(serde_json::from_str(&output.stdout)?)
}

fn prove_confinement(consumer: &Path, resolved: &Path, runner: &mut dyn Runner) -> Result<()> {
    let work = tempfile::Builder::new().prefix("nalr-confine-").tempdir()?;
    let denied = work.path().join("denied.txt");
    let mut denied_args = node_args(&entry_call(
        consumer,
        "grantArgs",
        &json!([{"readOnly":["/"]}]),
        runner,
    )?)?;
    denied_args.extend([
        "--".to_owned(),
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        format!("echo x > {}", denied.display()),
    ]);
    let denied_output = runner.run(&command(
        &path_string(resolved),
        denied_args,
        consumer,
        true,
    ))?;
    ensure!(
        denied_output.status != Some(0),
        "write outside the grants must fail"
    );
    ensure!(!denied.exists(), "denied write must not land on disk");
    let granted = work.path().join("granted.txt");
    let mut granted_args = node_args(&entry_call(
        consumer,
        "grantArgs",
        &json!([{"readOnly":["/"],"readWrite":[work.path()]}]),
        runner,
    )?)?;
    granted_args.extend([
        "--".to_owned(),
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        format!("echo ok > {}", granted.display()),
    ]);
    let granted_output = runner.run(&command(
        &path_string(resolved),
        granted_args,
        consumer,
        true,
    ))?;
    ensure!(
        granted_output.status == Some(0),
        "granted write must succeed: {}",
        granted_output.stderr
    );
    ensure!(
        fs::read_to_string(granted)?.trim() == "ok",
        "granted write content mismatch"
    );
    runner.log("confinement world-proof passed through the installed launcher");
    Ok(())
}

fn verify_installed_api(
    options: &PackedInstallOptions,
    consumer: &Path,
    runner: &mut dyn Runner,
) -> Result<(String, bool)> {
    fs::write(
        consumer.join("driver.mjs"),
        format!(
            "import {{ grantArgs, launcherPath, probe }} from '{ENTRY_PACKAGE_NAME}';\nconst entry = {{ grantArgs, launcherPath, probe }};\nprocess.stdout.write(JSON.stringify(entry[process.argv[2]](...JSON.parse(process.argv[3]))));\n"
        ),
    )?;
    let resolved = entry_call(consumer, "launcherPath", &json!([]), runner)?;
    let resolved = PathBuf::from(
        resolved
            .as_str()
            .context("launcherPath must return a string")?,
    );
    ensure!(resolved.is_absolute(), "launcherPath must be absolute");
    let expected_segment = format!("{ENTRY_PACKAGE_NAME}-{}", options.host_platform)
        .split('/')
        .collect::<PathBuf>();
    ensure!(
        path_string(&resolved).contains(&path_string(&expected_segment)),
        "launcherPath must point into the platform package: {}",
        resolved.display()
    );
    if options.host_platform.starts_with("linux-") {
        ensure!(
            resolved.exists(),
            "installed launcher missing at {}",
            resolved.display()
        );
        ensure!(
            executable(&resolved),
            "installed launcher is not executable — the pack path stripped the mode bit: {}",
            resolved.display()
        );
    } else {
        ensure!(
            !resolved.exists(),
            "no platform package exists for this host — the fallback path must not exist"
        );
    }
    let enforcement = entry_call(consumer, "probe", &json!([resolved]), runner)?;
    let enforcement = enforcement
        .as_str()
        .context("probe must return an enforcement string")?
        .to_owned();
    let mut confinement_proved = false;
    if options.host_platform.starts_with("linux-") {
        runner.log(&format!(
            "probe through the installed launcher: {enforcement}"
        ));
        if enforcement == "unusable" {
            ensure!(
                !options.require_landlock,
                "NALR_REQUIRE_LANDLOCK=1 but the probe reports unusable"
            );
            runner.log("kernel does not enforce Landlock — skipping the confinement world-proof");
        } else {
            prove_confinement(consumer, &resolved, runner)?;
            confinement_proved = true;
        }
    } else {
        ensure!(
            enforcement == "unusable",
            "non-linux host probe must report unusable"
        );
        runner.log("non-linux host: fallback resolution and unusable probe verified");
    }
    Ok((enforcement, confinement_proved))
}

/// Rehearses packed installation, installed entry resolution, and kernel confinement.
///
/// # Errors
///
/// Rejects incomplete payloads, install-time fallbacks, unconverted workspace dependencies,
/// altered binary bytes or modes, invalid entry responses, and failed confinement proofs.
pub fn verify_packed_install(
    repository: &Repository,
    options: &PackedInstallOptions,
    runner: &mut dyn Runner,
) -> Result<InstallVerification> {
    let payload = verify_payloads(repository, options, runner)?;
    let (consumer, byte_pinned) = install_consumer(repository, options, &payload, runner)?;
    let (enforcement, confinement_proved) = verify_installed_api(options, consumer.path(), runner)?;
    runner.log("Packed install verification passed.");
    Ok(InstallVerification {
        checked_packages: payload.checked_packages,
        byte_pinned_binaries: byte_pinned,
        confinement_proved,
        enforcement,
    })
}
