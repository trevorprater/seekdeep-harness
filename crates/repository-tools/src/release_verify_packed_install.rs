//! Isolated install-and-run verification for packed release tarballs.

use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fmt::Write as _,
    path::{Path, PathBuf},
};

use indexmap::IndexMap;

use crate::{
    release_families::ReleaseFamily,
    release_process::{ReleaseRunOptions, capture},
    release_tarball::packed_identity,
};

/// One packed dependency URL and declared version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackedDependency {
    /// Absolute `file:` URL.
    pub url: String,
    /// Packed manifest version.
    pub version: String,
}

/// Reads every `.tgz` in each directory into package-name keyed dependencies.
///
/// # Errors
///
/// Returns directory, empty-directory, tarball, identity, or file-URL failures.
pub fn packed_dependencies(
    directories: &[PathBuf],
) -> anyhow::Result<IndexMap<String, PackedDependency>> {
    let mut dependencies = IndexMap::new();
    for directory in directories {
        let mut tarballs = std::fs::read_dir(directory)?
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.file_type().is_ok_and(|kind| kind.is_file())
                    && entry.file_name().to_string_lossy().ends_with(".tgz")
            })
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        tarballs.sort_by(|left, right| {
            left.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .encode_utf16()
                .cmp(
                    right
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .encode_utf16(),
                )
        });
        if tarballs.is_empty() {
            anyhow::bail!("{} holds no packed tarball", directory.display());
        }
        for tarball in tarballs {
            let identity = packed_identity(&tarball)?;
            let url = url::Url::from_file_path(&tarball)
                .map_err(|()| anyhow::anyhow!("cannot form file URL for {}", tarball.display()))?;
            dependencies.insert(
                identity.name,
                PackedDependency {
                    url: url.into(),
                    version: identity.version,
                },
            );
        }
    }
    Ok(dependencies)
}

/// Builds the sanitized installed-artifact environment.
#[must_use]
pub fn consumer_environment(
    consumer_root: &Path,
    inherited: impl IntoIterator<Item = (OsString, OsString)>,
) -> BTreeMap<OsString, OsString> {
    let mut environment = inherited.into_iter().collect::<BTreeMap<_, _>>();
    for key in [
        "npm_config_user_agent",
        "NPM_CONFIG_USER_AGENT",
        "NODE_OPTIONS",
        "NODE_PATH",
    ] {
        environment.remove(OsStr::new(key));
    }
    environment.insert(
        "SEEKDEEP_HOME".into(),
        consumer_root.join(".seekdeep").into_os_string(),
    );
    environment.insert(
        "SEEKDEEP_AGENTS_HOME".into(),
        consumer_root.join(".agents").into_os_string(),
    );
    environment.insert("SEEKDEEP_TELEMETRY_DISABLED".into(), "1".into());
    environment
}

/// Installs and drives packed artifacts with the real npm/Node commands.
///
/// # Errors
///
/// Returns tarball, temp, install, executable, or version mismatch failures.
pub fn verify_packed_install(
    family: ReleaseFamily,
    directories: &[PathBuf],
) -> anyhow::Result<String> {
    verify_packed_install_with(family, directories, |command, args, cwd, env| {
        capture(
            command,
            args,
            &ReleaseRunOptions {
                cwd: Some(cwd.to_owned()),
                env: Some(env.clone()),
            },
        )
    })
}

/// Installs and drives packed artifacts through an injected process runner.
///
/// # Errors
///
/// Returns tarball, temp, runner, executable, or version mismatch failures.
pub fn verify_packed_install_with(
    family: ReleaseFamily,
    directories: &[PathBuf],
    mut runner: impl FnMut(
        &str,
        &[String],
        &Path,
        &BTreeMap<OsString, OsString>,
    ) -> anyhow::Result<String>,
) -> anyhow::Result<String> {
    let Some(entry) = family.installed_entry() else {
        return Ok(format!(
            "release verify-packed-install: family {} publishes no executable, nothing to drive\n",
            family.identifier()
        ));
    };
    let packed = packed_dependencies(directories)?;
    let expected = packed.get(&entry.package_name).ok_or_else(|| {
        anyhow::anyhow!("{} is not among the packed tarballs", entry.package_name)
    })?;
    let temporary = PackedConsumerDirectory::new(family)?;
    let dependencies = packed
        .iter()
        .map(|(name, packed)| (name.clone(), serde_json::Value::String(packed.url.clone())))
        .collect::<serde_json::Map<_, _>>();
    std::fs::write(
        temporary.path.join("package.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&serde_json::json!({
                "name": format!("seekdeep-packed-install-{}", family.identifier()),
                "version": "0.0.0",
                "private": true,
                "dependencies": dependencies,
            }))?
        ),
    )?;
    let environment = consumer_environment(&temporary.path, std::env::vars_os());
    let mut output = format!(
        "release verify-packed-install: installing {} tarball(s) into {}\n",
        packed.len(),
        temporary.path.display()
    );
    runner(
        "npm",
        &[
            "install".to_owned(),
            "--no-audit".to_owned(),
            "--no-fund".to_owned(),
            "--package-lock=false".to_owned(),
            "--omit=optional".to_owned(),
        ],
        &temporary.path,
        &environment,
    )?;
    let executable = entry
        .package_name
        .split('/')
        .fold(temporary.path.join("node_modules"), |path, segment| {
            path.join(segment)
        });
    let executable = executable.join(entry.bin_path);
    let version = runner(
        &node_executable().to_string_lossy(),
        &[
            executable.to_string_lossy().into_owned(),
            "--version".to_owned(),
        ],
        &temporary.path,
        &environment,
    )?;
    if version != expected.version {
        anyhow::bail!(
            "installed {} --version reported {}, expected {}",
            entry.package_name,
            serde_json::to_string(&version)?,
            expected.version
        );
    }
    let _ = writeln!(
        output,
        "release verify-packed-install: installed {} reports {version}",
        entry.package_name
    );
    Ok(output)
}

struct PackedConsumerDirectory {
    path: PathBuf,
}

impl PackedConsumerDirectory {
    fn new(family: ReleaseFamily) -> anyhow::Result<Self> {
        for attempt in 0..1_000_u16 {
            let path = std::env::temp_dir().join(format!(
                "seekdeep-packed-{}-{}-{attempt}",
                family.identifier(),
                std::process::id()
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        anyhow::bail!("could not allocate packed-install consumer directory")
    }
}

impl Drop for PackedConsumerDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn node_executable() -> OsString {
    std::env::var_os("npm_node_execpath").unwrap_or_else(|| "node".into())
}
