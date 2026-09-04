//! Repository-versioned Python wheel staging, native payload rules, and verification.

use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
    sync::LazyLock,
};

use indexmap::IndexMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod executable;
pub mod hook;
pub mod staging;
pub mod wheel;

/// Resolves the native binding target paired with a runtime executable.
///
/// # Errors
/// Rejects executable names outside the supported native naming scheme.
pub fn runtime_binding_target(executable: &str) -> anyhow::Result<executable::Target> {
    let suffix = executable
        .strip_prefix("seekdeep-jsonrpc-agent-pkg-")
        .ok_or_else(|| anyhow::anyhow!("unsupported runtime executable name {executable}"))?;
    executable::Target::parse(&format!("node24-{suffix}"))
}

/// Returns the architecture-qualified binding library filename.
///
/// # Errors
/// Rejects an unsupported executable name.
pub fn runtime_binding_name(executable: &str) -> anyhow::Result<String> {
    Ok(runtime_binding_target(executable)?.binding_basename())
}

/// Public Python distribution; the import namespace remains `deepseek_harness`.
pub const SDK_DISTRIBUTION: &str = "seekdeep-harness-sdk";
/// Platform-specific runtime distribution.
pub const RUNTIME_DISTRIBUTION: &str = "seekdeep-harness-runtime-bin";

/// One platform's wheel tag and runtime executable basename.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimePlatform {
    /// Platform tag without the Python/ABI prefix.
    pub tag: String,
    /// Exact native executable basename.
    pub executable: String,
}

/// Ordered platform manifest, preserving source diagnostics and matrix order.
pub type RuntimePlatforms = IndexMap<String, RuntimePlatform>;

/// Reads the closed platform-entry shape from the checked-in manifest.
///
/// # Errors
/// Rejects unreadable/invalid JSON, an empty/non-object root, and non-string or extra fields.
pub fn load_platforms(path: &Path) -> anyhow::Result<RuntimePlatforms> {
    let payload = std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "could not read runtime platform manifest from {}",
                path.display()
            )
        })?;
    let entries = payload
        .as_object()
        .filter(|entries| !entries.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{} must contain a non-empty platform object",
                path.display()
            )
        })?;
    let mut platforms = RuntimePlatforms::new();
    for (name, value) in entries {
        let platform: RuntimePlatform = serde_json::from_value(value.clone()).map_err(|_| {
            anyhow::anyhow!(
                "{} platform entries must contain string tag and executable fields",
                path.display()
            )
        })?;
        platforms.insert(name.clone(), platform);
    }
    Ok(platforms)
}

/// Reads the authoritative repository version; development wheel sentinels are not consulted.
///
/// # Errors
/// Rejects unreadable or malformed package metadata and unsupported version spelling.
pub fn repository_version(root: &Path) -> anyhow::Result<String> {
    static VERSION: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^\d+\.\d+\.\d+(?:-[0-9A-Za-z.]+)?$").expect("repository version pattern")
    });
    let path = root.join("package.json");
    let payload = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .ok_or_else(|| {
            anyhow::anyhow!("could not read repository version from {}", path.display())
        })?;
    let value = payload.get("version").unwrap_or(&Value::Null);
    let version = value
        .as_str()
        .filter(|version| VERSION.is_match(version))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{} version must be X.Y.Z with an optional prerelease segment, got {}",
                path.display(),
                python_repr(value)
            )
        })?;
    Ok(version.to_owned())
}

/// Converts the source's supported prerelease spellings to PEP 440.
///
/// # Errors
/// Rejects a prerelease without a supported alpha, beta, or release-candidate spelling.
pub fn pep440_version(version: &str) -> anyhow::Result<String> {
    static PRERELEASE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(a|b|c|rc|alpha|beta|pre|preview)\.?(\d+)$").expect("prerelease pattern")
    });
    let Some((stable, prerelease)) = version.split_once('-') else {
        return Ok(version.to_owned());
    };
    let captures = PRERELEASE.captures(prerelease).ok_or_else(|| {
        anyhow::anyhow!(
            "prerelease segment {} has no PEP 440 spelling; use rc.N, alpha.N, or beta.N",
            python_repr(&Value::String(prerelease.to_owned()))
        )
    })?;
    let identifier = match &captures[1] {
        "alpha" => "a",
        "beta" => "b",
        "c" | "pre" | "preview" => "rc",
        value => value,
    };
    Ok(format!("{stable}{identifier}{}", &captures[2]))
}

/// Validates an optional release tag against the repository spelling.
///
/// # Errors
/// Rejects a tag other than `python-v<repository-version>`.
pub fn validate_release_tag(tag: Option<&str>, version: &str) -> anyhow::Result<()> {
    if let Some(tag) = tag {
        let expected = format!("python-v{version}");
        anyhow::ensure!(
            tag == expected,
            "release tag must match repository version: expected {}, got {}",
            python_repr(&Value::String(expected)),
            python_repr(&Value::String(tag.to_owned()))
        );
    }
    Ok(())
}

/// Executable suffixes required by the pinned platform manifest.
pub fn runtime_suffixes(executable_name: &str) -> &'static [&'static str] {
    if executable_name.contains("-macos-") {
        &["", "-spawn-helper"]
    } else {
        &[""]
    }
}

/// Closed release package selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Package {
    /// Platform-independent Python API wheel.
    Sdk,
    /// One platform's native runtime carrier.
    Runtime,
}

impl Package {
    /// Public distribution name.
    pub const fn distribution(self) -> &'static str {
        match self {
            Self::Sdk => SDK_DISTRIBUTION,
            Self::Runtime => RUNTIME_DISTRIBUTION,
        }
    }
    /// Repository package directory name.
    pub const fn directory(self) -> &'static str {
        match self {
            Self::Sdk => "sdk",
            Self::Runtime => "sdk-runtime",
        }
    }
}

/// Computes the expected wheel path from distribution, PEP 440 version, and platform.
pub fn expected_wheel(
    output: &Path,
    package: Package,
    version: &str,
    platform: Option<&RuntimePlatform>,
) -> PathBuf {
    let distribution = package.distribution().replace('-', "_");
    let tag = platform.map_or("any", |platform| platform.tag.as_str());
    output.join(format!("{distribution}-{version}-py3-none-{tag}.whl"))
}

pub(crate) fn python_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".to_owned(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::String(value) => python_string_repr(value),
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(python_repr)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Object(values) => format!(
            "{{{}}}",
            values
                .iter()
                .map(|(key, value)| format!(
                    "{}: {}",
                    python_repr(&Value::String(key.clone())),
                    python_repr(value)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Number(value) => value.to_string(),
    }
}

fn python_string_repr(value: &str) -> String {
    static NONPRINTABLE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"[\p{C}\p{Z}]").expect("nonprintable Unicode categories"));
    let quote = if value.contains('\'') && !value.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut result = String::from(quote);
    let mut encoded = [0; 4];
    for character in value.chars() {
        match character {
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            '\\' => result.push_str("\\\\"),
            character if character == quote => {
                result.push('\\');
                result.push(character);
            }
            character
                if character != ' '
                    && NONPRINTABLE.is_match(character.encode_utf8(&mut encoded)) =>
            {
                let code = u32::from(character);
                match code {
                    0..=0xff => write!(result, "\\x{code:02x}"),
                    0x100..=0xffff => write!(result, "\\u{code:04x}"),
                    _ => write!(result, "\\U{code:08x}"),
                }
                .expect("writing to a string is infallible");
            }
            character => result.push(character),
        }
    }
    result.push(quote);
    result
}
