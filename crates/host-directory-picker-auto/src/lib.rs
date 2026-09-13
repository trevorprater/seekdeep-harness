//! Boot-time adaptive native-or-browse directory picker composition.

use std::{ffi::OsStr, path::Path, sync::Arc};

use seekdeep_cordis::{Plugin, fiber::EffectHandle};
use seekdeep_host_webserver::{ListenHost, WEB_SERVER};
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use seekdeep_loader::{Entry, EntryId, EntryParent, LOADER, PluginSpecifier};
use seekdeep_util::launch_environment::launch_environment_of;

/// Stable chooser plugin name.
pub const NAME: &str = "directory-picker-auto";
/// Stable invariant companion name.
pub const INVARIANT_NAME: &str = "host-directory-picker-auto-invariant";
const PACKAGE_NAME: &str = "@seekdeep-ai/seekdeep-host-directory-picker-auto";

/// Native Host backend package.
pub const NATIVE_BACKEND_PACKAGE: &str = "@seekdeep-ai/seekdeep-host-directory-picker-native";
/// Browse Host backend package.
pub const BROWSE_BACKEND_PACKAGE: &str = "@seekdeep-ai/seekdeep-host-directory-picker-browse";
/// Native browser surface package.
pub const NATIVE_SURFACE_PACKAGE: &str = "@seekdeep-ai/seekdeep-client-ui-directory-picker-native";
/// Browse browser surface package.
pub const BROWSE_SURFACE_PACKAGE: &str = "@seekdeep-ai/seekdeep-client-ui-directory-picker-browse";

/// Concrete interaction selected for one boot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectoryPickerBackendKind {
    /// Host-local OS chooser.
    Native,
    /// Remote-safe in-app filesystem browser.
    Browse,
}

impl DirectoryPickerBackendKind {
    fn packages(self) -> (&'static str, &'static str) {
        match self {
            Self::Native => (NATIVE_BACKEND_PACKAGE, NATIVE_SURFACE_PACKAGE),
            Self::Browse => (BROWSE_BACKEND_PACKAGE, BROWSE_SURFACE_PACKAGE),
        }
    }
}

/// Platform fact sampled once at boot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolvePlatform {
    /// macOS.
    Darwin,
    /// Windows.
    Win32,
    /// Linux.
    Linux,
    /// Any unsupported host literal.
    Other(String),
}

impl ResolvePlatform {
    /// Samples the current target platform.
    #[must_use]
    pub fn current() -> Self {
        match std::env::consts::OS {
            "macos" => Self::Darwin,
            "windows" => Self::Win32,
            "linux" => Self::Linux,
            platform => Self::Other(platform.to_owned()),
        }
    }
}

/// Environment subset used by resolution.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DirectoryPickerEnv {
    /// SSH connection tuple.
    pub ssh_connection: Option<String>,
    /// SSH pseudo-terminal path.
    pub ssh_tty: Option<String>,
    /// X11 display.
    pub display: Option<String>,
    /// Wayland display.
    pub wayland_display: Option<String>,
}

/// Pure boot-time resolver input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryPickerHostFacts {
    /// Effective webserver bind host.
    pub bind_host: ListenHost,
    /// Host platform.
    pub platform: ResolvePlatform,
    /// Frozen launch environment subset.
    pub env: DirectoryPickerEnv,
    /// Whether Linux has Zenity or `KDialog` on `PATH`.
    pub linux_chooser: bool,
}

/// Resolves the stable interaction backend for one boot.
#[must_use]
pub fn resolve_directory_picker_backend(
    facts: &DirectoryPickerHostFacts,
) -> DirectoryPickerBackendKind {
    if facts.bind_host != ListenHost::Loopback {
        return DirectoryPickerBackendKind::Browse;
    }
    if present(facts.env.ssh_connection.as_deref()) || present(facts.env.ssh_tty.as_deref()) {
        return DirectoryPickerBackendKind::Browse;
    }
    match facts.platform {
        ResolvePlatform::Darwin | ResolvePlatform::Win32 => DirectoryPickerBackendKind::Native,
        ResolvePlatform::Linux if facts.linux_chooser => {
            if present(facts.env.display.as_deref())
                || present(facts.env.wayland_display.as_deref())
            {
                DirectoryPickerBackendKind::Native
            } else {
                DirectoryPickerBackendKind::Browse
            }
        }
        ResolvePlatform::Linux | ResolvePlatform::Other(_) => DirectoryPickerBackendKind::Browse,
    }
}

/// Whether one path names an executable file.
#[must_use]
pub fn can_execute(candidate: &Path) -> bool {
    let Ok(metadata) = candidate.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Scans PATH directories for executable `zenity` or `kdialog` binaries.
#[must_use]
pub fn has_linux_chooser_binary(
    path_value: Option<&OsStr>,
    is_executable: impl Fn(&Path) -> bool,
) -> bool {
    let Some(path_value) = path_value.filter(|value| !value.is_empty()) else {
        return false;
    };
    for directory in std::env::split_paths(path_value) {
        if directory.as_os_str().is_empty() {
            continue;
        }
        for name in ["zenity", "kdialog"] {
            if is_executable(&directory.join(name)) {
                return true;
            }
        }
    }
    false
}

/// Builds the adaptive chooser plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, ["webServer", "loader"], |context, _| {
        Box::pin(async move {
            let server = context
                .get(WEB_SERVER)
                .ok_or_else(|| anyhow::anyhow!("directory-picker-auto requires webServer"))?;
            let environment = launch_environment_of(&context);
            let value = |name| environment.get(name).map(|entry| entry.value);
            let platform = ResolvePlatform::current();
            let linux_chooser = matches!(platform, ResolvePlatform::Linux)
                && has_linux_chooser_binary(value("PATH").as_deref().map(OsStr::new), can_execute);
            let backend = resolve_directory_picker_backend(&DirectoryPickerHostFacts {
                bind_host: server.host(),
                platform,
                env: DirectoryPickerEnv {
                    ssh_connection: value("SSH_CONNECTION"),
                    ssh_tty: value("SSH_TTY"),
                    display: value("DISPLAY"),
                    wayland_display: value("WAYLAND_DISPLAY"),
                },
                linux_chooser,
            });
            mount_interaction(&context, backend).await
        })
    })
}

async fn mount_interaction(
    context: &seekdeep_cordis::Context,
    backend: DirectoryPickerBackendKind,
) -> anyhow::Result<()> {
    let loader = context
        .get(LOADER)
        .ok_or_else(|| anyhow::anyhow!("directory-picker-auto requires loader"))?;
    let owner = context.fiber().id();
    let packages = backend.packages();
    let mut ids = Vec::new();
    for (suffix, package) in [("backend", packages.0), ("surface", packages.1)] {
        let id = EntryId::new(format!("directory-picker-auto-{owner}-{suffix}"))?;
        let entry = Entry::new(id.clone(), PluginSpecifier::new(package)?);
        if let Err(error) = loader.create_entry(entry, EntryParent::Root, None).await {
            unmount(&loader, &mut ids).await?;
            return Err(error.into());
        }
        ids.push(id);
    }

    let cleanup_loader = loader.clone();
    let effect = EffectHandle::new("directory-picker-auto: interaction entries", move || {
        let mut ids = ids;
        Box::pin(async move { unmount(&cleanup_loader, &mut ids).await })
    });
    match context.own(effect.clone()) {
        Ok(_) => Ok(()),
        Err(error) => {
            effect.dispose().await?;
            Err(error.into())
        }
    }
}

async fn unmount(
    loader: &Arc<seekdeep_loader::LoaderSettlement>,
    ids: &mut Vec<EntryId>,
) -> anyhow::Result<()> {
    while let Some(id) = ids.pop() {
        loader.remove_programmatic_entry_if_present(&id).await?;
    }
    Ok(())
}

/// Registers the chooser's explained-empty invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant registry failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(PACKAGE_NAME, InvariantInstaller::noop())
}

fn present(value: Option<&str>) -> bool {
    value.is_some_and(|value| !value.is_empty())
}
