//! Legacy target spelling and build-option validation, independent of process execution.

use std::{collections::HashSet, sync::LazyLock};

use serde::{Deserialize, Serialize};

/// Native executable platform; Windows is outside the source distribution contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    /// GNU/Linux.
    Linux,
    /// macOS.
    Macos,
}

impl Platform {
    /// Artifact platform spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::Macos => "macos",
        }
    }
}

/// Native executable architecture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Arch {
    /// AMD64.
    X64,
    /// `AArch64`.
    Arm64,
}

impl Arch {
    /// Source artifact architecture spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::X64 => "x64",
            Self::Arm64 => "arm64",
        }
    }
}

/// Validated legacy `node<major>-<platform>-<arch>` build target.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Target {
    node_range: String,
    platform: Platform,
    arch: Arch,
}

impl Target {
    /// Parses the source target vocabulary and exact validation diagnostics.
    ///
    /// # Errors
    /// Rejects malformed triples, non-Node ranges, and unsupported platforms or architectures.
    pub fn parse(spec: &str) -> anyhow::Result<Self> {
        static NODE_RANGE: LazyLock<regress::Regex> =
            LazyLock::new(|| regress::Regex::new(r"^node\d+$").expect("Node target range"));
        let parts = spec.split('-').collect::<Vec<_>>();
        let [node_range, platform, arch] = parts.as_slice() else {
            anyhow::bail!(
                "build-exe-for-python-sdk: target {} must be <nodeRange>-<platform>-<arch>, e.g. node24-linux-x64.",
                quoted(spec)
            );
        };
        anyhow::ensure!(
            NODE_RANGE.find(node_range).is_some(),
            "build-exe-for-python-sdk: target {}: node range must look like node24, got {}.",
            quoted(spec),
            quoted(node_range)
        );
        let platform = match *platform {
            "linux" => Platform::Linux,
            "macos" => Platform::Macos,
            value => anyhow::bail!(
                "build-exe-for-python-sdk: target {}: platform must be one of linux, macos, got {}.",
                quoted(spec),
                quoted(value)
            ),
        };
        let arch = match *arch {
            "x64" => Arch::X64,
            "arm64" => Arch::Arm64,
            value => anyhow::bail!(
                "build-exe-for-python-sdk: target {}: arch must be one of x64, arm64, got {}.",
                quoted(spec),
                quoted(value)
            ),
        };
        Ok(Self {
            node_range: (*node_range).to_owned(),
            platform,
            arch,
        })
    }

    /// The retained source target spelling.
    pub fn spec(&self) -> String {
        format!(
            "{}-{}-{}",
            self.node_range,
            self.platform.as_str(),
            self.arch.as_str()
        )
    }
    /// Artifact platform.
    pub const fn platform(&self) -> Platform {
        self.platform
    }
    /// Artifact architecture.
    pub const fn arch(&self) -> Arch {
        self.arch
    }
    /// Node-independent native artifact suffix.
    pub fn platform_arch(&self) -> String {
        format!("{}-{}", self.platform.as_str(), self.arch.as_str())
    }
    /// Required Rust target triple.
    pub const fn rust_target(&self) -> &'static str {
        match (self.platform, self.arch) {
            (Platform::Linux, Arch::X64) => "x86_64-unknown-linux-gnu",
            (Platform::Linux, Arch::Arm64) => "aarch64-unknown-linux-gnu",
            (Platform::Macos, Arch::X64) => "x86_64-apple-darwin",
            (Platform::Macos, Arch::Arm64) => "aarch64-apple-darwin",
        }
    }
    /// Product-facing executable basename.
    pub fn basename(&self) -> String {
        format!("seekdeep-jsonrpc-agent-pkg-{}", self.platform_arch())
    }
}

/// Injected source host vocabulary for deterministic target and CLI tests.
#[derive(Clone, Debug)]
pub struct Host {
    /// Node's platform spelling.
    pub platform: String,
    /// Node's architecture spelling.
    pub arch: String,
}

impl Host {
    /// Current native host, expressed using the source host spellings.
    pub fn current() -> Self {
        Self {
            platform: match std::env::consts::OS {
                "macos" => "darwin",
                "windows" => "win32",
                other => other,
            }
            .to_owned(),
            arch: match std::env::consts::ARCH {
                "aarch64" => "arm64",
                "x86_64" => "x64",
                "x86" => "ia32",
                "powerpc64" => "ppc64",
                other => other,
            }
            .to_owned(),
        }
    }

    /// Resolves the source's default host target.
    ///
    /// # Errors
    /// Rejects unsupported host platform or architecture.
    pub fn target(&self) -> anyhow::Result<Target> {
        let platform = match self.platform.as_str() {
            "darwin" => "macos",
            "linux" => "linux",
            other => anyhow::bail!(
                "build-exe-for-python-sdk: unsupported host platform {other}; pass --targets explicitly."
            ),
        };
        anyhow::ensure!(
            matches!(self.arch.as_str(), "x64" | "arm64"),
            "build-exe-for-python-sdk: unsupported host arch {}; pass --targets explicitly.",
            self.arch
        );
        Target::parse(&format!("node24-{platform}-{}", self.arch))
    }
}

/// Validated native build request using the source option names and defaults.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildOptions {
    /// Requested platform products in caller order.
    pub targets: Vec<Target>,
    /// Use existing compiled release artifacts.
    pub skip_build: bool,
    /// Print planned commands and writes without executing them.
    pub dry_run: bool,
}

impl BuildOptions {
    /// Rechecks target-list invariants for programmatic callers.
    ///
    /// # Errors
    /// Rejects empty target lists and output-name collisions.
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.targets.is_empty(),
            "build-exe-for-python-sdk: --targets is empty."
        );
        let mut seen = HashSet::new();
        for target in &self.targets {
            anyhow::ensure!(
                seen.insert((target.platform, target.arch)),
                "build-exe-for-python-sdk: duplicate platform-arch {} in --targets; canonical product names would collide.",
                target.platform_arch()
            );
        }
        Ok(())
    }
}

/// Help or one validated build request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CliOutcome {
    /// Print help and exit successfully.
    Help,
    /// Execute the selected build.
    Build(BuildOptions),
}

/// Source-compatible parse refusal, distinguishing raw option errors from target validation.
#[derive(Debug)]
pub struct CliError {
    /// Complete diagnostic.
    pub message: String,
    /// Raw option errors also print usage.
    pub show_usage: bool,
}

impl std::fmt::Display for CliError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(formatter)
    }
}
impl std::error::Error for CliError {}

/// Parses flags with the source's last-value, boolean, help, and collision behavior.
///
/// # Errors
/// Rejects unknown options, positional arguments, missing values, and invalid or colliding targets.
pub fn parse_cli(arguments: &[String], host: &Host) -> Result<CliOutcome, CliError> {
    let (targets, skip_build, dry_run, help) =
        parse_flags(arguments).map_err(|message| CliError {
            message: format!("build-exe-for-python-sdk: {message}"),
            show_usage: true,
        })?;
    if help {
        return Ok(CliOutcome::Help);
    }
    let targets = match targets {
        None => host.target().map(|target| vec![target]),
        Some(targets) => targets
            .split(',')
            .map(js_trim)
            .filter(|target| !target.is_empty())
            .map(Target::parse)
            .collect(),
    }
    .map_err(|error| CliError {
        message: error.to_string(),
        show_usage: false,
    })?;
    if targets.is_empty() {
        return Err(CliError {
            message: "build-exe-for-python-sdk: --targets is empty.".to_owned(),
            show_usage: false,
        });
    }
    let mut seen = HashSet::new();
    for target in &targets {
        if !seen.insert((target.platform, target.arch)) {
            return Err(CliError {
                message: format!(
                    "build-exe-for-python-sdk: duplicate platform-arch {} in --targets; canonical product names would collide.",
                    target.platform_arch()
                ),
                show_usage: false,
            });
        }
    }
    Ok(CliOutcome::Build(BuildOptions {
        targets,
        skip_build,
        dry_run,
    }))
}

fn parse_flags(arguments: &[String]) -> Result<(Option<String>, bool, bool, bool), String> {
    let (mut targets, mut skip, mut dry, mut help) = (None, false, false, false);
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        if argument == "--" {
            if let Some(value) = arguments.next() {
                return Err(positional(value));
            }
            break;
        }
        let (name, inline) = argument
            .split_once('=')
            .map_or((argument.as_str(), None), |(name, value)| {
                (name, Some(value))
            });
        match name {
            "--targets" => {
                let value = if let Some(value) = inline {
                    value
                } else {
                    let value = arguments
                        .next()
                        .ok_or_else(|| "Option '--targets <value>' argument missing".to_owned())?;
                    if value.starts_with('-') && value != "-" {
                        return Err("Option '--targets' argument is ambiguous.\nDid you forget to specify the option argument for '--targets'?\nTo specify an option argument starting with a dash use '--targets=-XYZ'.".to_owned());
                    }
                    value
                };
                targets = Some(value.to_owned());
            }
            "--skip-build" | "--dry-run" | "--help" => {
                if inline.is_some() {
                    return Err(format!("Option '{name}' does not take an argument"));
                }
                match name {
                    "--skip-build" => skip = true,
                    "--dry-run" => dry = true,
                    _ => help = true,
                }
            }
            name if name.starts_with("--") => return Err(format!("Unknown option '{name}'")),
            name if name.starts_with('-') && name.len() > 1 => {
                let first = name[1..]
                    .encode_utf16()
                    .next()
                    .expect("non-empty short option");
                return Err(format!(
                    "Unknown option '-{}'",
                    String::from_utf16_lossy(&[first])
                ));
            }
            _ => return Err(positional(argument)),
        }
    }
    Ok((targets, skip, dry, help))
}

fn positional(value: &str) -> String {
    format!("Unexpected argument '{value}'. This command does not take positional arguments")
}
fn quoted(value: &str) -> String {
    serde_json::to_string(value).expect("string JSON")
}
fn js_trim(value: &str) -> &str {
    value.trim_matches(|character| matches!(character, '\u{9}'..='\u{d}' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'))
}
