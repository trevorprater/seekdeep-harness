//! The pinned `@anthropic-ai/claude-agent-sdk` package and the Claude Code CLI it distributes.
//!
//! The source resolves the SDK with `import.meta.resolve` and takes the platform package as the
//! SDK's store sibling, so the pinned CLI wins over any host-installed `claude`.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// The `@anthropic-ai/claude-agent-sdk` version the package manifest pins.
pub(crate) const PINNED_SDK_VERSION: &str = "0.3.220";
/// The Claude Code CLI that SDK ships.
pub(crate) const PINNED_CLAUDE_CODE_VERSION: &str = "2.1.220";

/// The SDK package as `import.meta.resolve` finds it: the package's own link, followed to the
/// store copy so the platform package is its sibling.
pub(crate) fn sdk_root() -> anyhow::Result<PathBuf> {
    let linked = Path::new(env!("CARGO_MANIFEST_DIR")).join(
        "../../packages/subagent/subagent-claude-code/node_modules/@anthropic-ai/claude-agent-sdk",
    );
    Ok(std::fs::canonicalize(linked)?)
}

/// The SDK manifest fields the source pins.
pub(crate) struct SdkManifest {
    pub(crate) version: String,
    pub(crate) claude_code_version: String,
    pub(crate) optional_dependencies: BTreeMap<String, String>,
}

/// Reads the pinned SDK's manifest.
///
/// # Errors
///
/// Returns the read or parse failure, or a manifest missing one of the pinned fields.
pub(crate) fn sdk_manifest(sdk_root: &Path) -> anyhow::Result<SdkManifest> {
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(sdk_root.join("package.json"))?)?;
    let field = |name: &str| -> anyhow::Result<String> {
        manifest
            .get(name)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("the SDK manifest has no string {name}"))
    };
    Ok(SdkManifest {
        version: field("version")?,
        claude_code_version: field("claudeCodeVersion")?,
        optional_dependencies: serde_json::from_value(
            manifest
                .get("optionalDependencies")
                .cloned()
                .unwrap_or_default(),
        )?,
    })
}

/// Node's `process.platform` spelling for this host.
pub(crate) fn node_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}

/// Node's `process.arch` spelling for this host.
pub(crate) fn node_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    }
}

/// The SDK's optional platform package for one host.
pub(crate) fn platform_package(platform: &str, arch: &str) -> String {
    format!("@anthropic-ai/claude-agent-sdk-{platform}-{arch}")
}

/// The CLI inside the platform package, which sits beside the SDK in the store.
pub(crate) fn claude_bin(sdk_root: &Path, platform_package: &str, platform: &str) -> PathBuf {
    let (_, directory) = platform_package
        .split_once('/')
        .expect("platform packages are scoped");
    sdk_root
        .join("..")
        .join(directory)
        .join(if platform == "win32" {
            "claude.exe"
        } else {
            "claude"
        })
}

/// The pinned CLI resolved for this host.
pub(crate) struct PinnedCli {
    pub(crate) sdk_root: PathBuf,
    pub(crate) platform_package: String,
    pub(crate) claude_bin: PathBuf,
}

impl PinnedCli {
    /// Resolves the SDK and its platform CLI for this host.
    ///
    /// # Errors
    ///
    /// Returns the failure to follow the package's SDK link.
    pub(crate) fn resolve() -> anyhow::Result<Self> {
        let sdk_root = sdk_root()?;
        let platform_package = platform_package(node_platform(), node_arch());
        let claude_bin = claude_bin(&sdk_root, &platform_package, node_platform());
        Ok(Self {
            sdk_root,
            platform_package,
            claude_bin,
        })
    }

    /// The SDK manifest, its platform package, and `executable --version` all agree on the pins.
    ///
    /// # Errors
    ///
    /// Returns manifest or launch failures; a version mismatch panics like the source's assertion.
    pub(crate) fn assert_versions(
        &self,
        executable: &Path,
        env: &BTreeMap<String, String>,
    ) -> anyhow::Result<()> {
        let manifest = sdk_manifest(&self.sdk_root)?;
        assert_eq!(manifest.version, PINNED_SDK_VERSION);
        assert_eq!(manifest.claude_code_version, PINNED_CLAUDE_CODE_VERSION);
        assert_eq!(
            manifest.optional_dependencies.get(&self.platform_package),
            Some(&PINNED_SDK_VERSION.to_owned())
        );
        let version = std::process::Command::new(executable)
            .arg("--version")
            .envs(env)
            .output()?;
        anyhow::ensure!(
            version.status.success(),
            "claude --version failed: {}",
            String::from_utf8_lossy(&version.stderr)
        );
        assert_eq!(
            String::from_utf8(version.stdout)?.trim(),
            format!("{PINNED_CLAUDE_CODE_VERSION} (Claude Code)")
        );
        Ok(())
    }
}
