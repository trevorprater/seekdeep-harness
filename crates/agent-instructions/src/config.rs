//! Configuration normalization for workspace instruction discovery and rendering.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Component, Path};

use seekdeep_schemastery::Schema;
use seekdeep_util::home_paths::resolve_process_seekdeep_home;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Directory entries that identify the project root while walking upward from the session cwd.
pub const DEFAULT_PROJECT_ROOT_MARKERS: &[&str] = &[".git"];
/// Ordered same-directory project candidates.
pub const DEFAULT_INSTRUCTION_FILE_CANDIDATES: &[&str] = &["AGENTS.md", "CLAUDE.md"];
/// Ordered same-directory local-overlay candidates.
pub const DEFAULT_LOCAL_INSTRUCTION_FILE_CANDIDATES: &[&str] =
    &["AGENTS.local.md", "CLAUDE.local.md"];
/// Default UTF-8 byte cap for one source file.
pub const DEFAULT_MAX_SOURCE_BYTES: u64 = 1_048_576;

/// User-facing workspace instruction loader configuration.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// Harness home containing the fixed user-global AGENTS.md.
    pub dsh_home: Option<String>,
    /// Directory entries that identify the project root while walking upward.
    pub project_root_markers: Option<Vec<String>>,
    /// UTF-8 byte cap for one rendered baseline or dynamic batch.
    pub max_bytes: u64,
    /// Maximum UTF-8 bytes read from one instruction file.
    pub max_source_bytes: Option<u64>,
    /// Ordered same-directory project candidates.
    pub instruction_file_candidates: Option<Vec<String>>,
    /// Ordered same-directory local-overlay candidates.
    pub local_instruction_file_candidates: Option<Vec<String>>,
}

/// The source-compatible admission schema for Config.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn config_schema() -> Schema {
    Schema::object([
        ("dshHome", Schema::string()),
        (
            "projectRootMarkers",
            Schema::array(Schema::string()).with_default(json!([".git"])),
        ),
        ("maxBytes", Schema::number().required()),
        (
            "maxSourceBytes",
            Schema::number()
                .step(1.0)
                .min(1.0)
                .with_default(DEFAULT_MAX_SOURCE_BYTES),
        ),
        (
            "instructionFileCandidates",
            Schema::array(Schema::string()).with_default(json!(["AGENTS.md", "CLAUDE.md"])),
        ),
        (
            "localInstructionFileCandidates",
            Schema::array(Schema::string())
                .with_default(json!(["AGENTS.local.md", "CLAUDE.local.md"])),
        ),
    ])
}

/// Normalized instruction discovery configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedDiscoveryConfig {
    /// Resolved harness home path.
    pub dsh_home: String,
    /// Project root markers.
    pub project_root_markers: Vec<String>,
    /// Instruction candidates.
    pub instruction_file_candidates: Vec<String>,
    /// Local overlay candidates.
    pub local_instruction_file_candidates: Vec<String>,
}

/// Normalized configuration used by discovery and reconciliation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedConfig {
    /// Resolved harness home path.
    pub dsh_home: String,
    /// Project root markers.
    pub project_root_markers: Vec<String>,
    /// Instruction candidates.
    pub instruction_file_candidates: Vec<String>,
    /// Local overlay candidates.
    pub local_instruction_file_candidates: Vec<String>,
    /// Rendered byte budget.
    pub max_bytes: u64,
    /// Per-file source byte cap.
    pub max_source_bytes: u64,
}

/// Identifies the discovery, precedence, and budget semantics of one baseline.
///
/// # Panics
///
/// Panics if the identity cannot be JSON-serialized, which cannot happen.
#[must_use]
pub fn workspace_baseline_identity(
    config: &ResolvedConfig,
    cwd: &str,
    project_root: &str,
) -> String {
    serde_json::to_string(&json!({
        "projectRoot": relative_path(cwd, project_root),
        "projectRootMarkers": config.project_root_markers,
        "maxBytes": config.max_bytes,
        "maxSourceBytes": config.max_source_bytes,
        "instructionFileCandidates": config.instruction_file_candidates,
        "localInstructionFileCandidates": config.local_instruction_file_candidates,
    }))
    .expect("baseline identity serializes")
}

/// Resolves defaults, the harness home, and valid same-directory candidates.
///
/// # Errors
///
/// Returns when the operating-system or harness home cannot be resolved.
pub fn resolve_config(config: &Config) -> anyhow::Result<ResolvedConfig> {
    let discovery = resolve_discovery_config(config)?;
    Ok(ResolvedConfig {
        dsh_home: discovery.dsh_home,
        project_root_markers: discovery.project_root_markers,
        instruction_file_candidates: discovery.instruction_file_candidates,
        local_instruction_file_candidates: discovery.local_instruction_file_candidates,
        max_bytes: config.max_bytes,
        max_source_bytes: config.max_source_bytes.unwrap_or(DEFAULT_MAX_SOURCE_BYTES),
    })
}

/// Resolves the subset of configuration used before instruction content is rendered.
///
/// # Errors
///
/// Returns when the operating-system or harness home cannot be resolved.
pub fn resolve_discovery_config(config: &Config) -> anyhow::Result<ResolvedDiscoveryConfig> {
    let home = resolve_process_seekdeep_home(config.dsh_home.as_deref().map(OsStr::new))?;
    Ok(ResolvedDiscoveryConfig {
        dsh_home: home.to_string_lossy().into_owned(),
        project_root_markers: config.project_root_markers.clone().unwrap_or_else(|| {
            DEFAULT_PROJECT_ROOT_MARKERS
                .iter()
                .map(|s| (*s).to_owned())
                .collect()
        }),
        instruction_file_candidates: resolve_instruction_file_candidates(
            config.instruction_file_candidates.as_deref(),
            DEFAULT_INSTRUCTION_FILE_CANDIDATES,
        ),
        local_instruction_file_candidates: resolve_instruction_file_candidates(
            config.local_instruction_file_candidates.as_deref(),
            DEFAULT_LOCAL_INSTRUCTION_FILE_CANDIDATES,
        ),
    })
}

fn resolve_instruction_file_candidates(
    candidates: Option<&[String]>,
    fallback: &[&str],
) -> Vec<String> {
    let reserved: HashSet<&str> = ["", ".", ".."].into_iter().collect();
    match candidates {
        Some(list) => list
            .iter()
            .filter(|candidate| {
                !reserved.contains(candidate.as_str())
                    && !candidate.chars().any(|c| c == '/' || c == '\\')
            })
            .cloned()
            .collect(),
        None => fallback
            .iter()
            .map(|candidate| (*candidate).to_owned())
            .collect(),
    }
}

fn relative_path(from: &str, to: &str) -> String {
    let from = Path::new(from).components().collect::<Vec<_>>();
    let to = Path::new(to).components().collect::<Vec<_>>();
    let mut common = 0;
    while common < from.len() && common < to.len() && from[common] == to[common] {
        common += 1;
    }
    let mut parts: Vec<String> = Vec::new();
    for _ in common..from.len() {
        parts.push("..".to_owned());
    }
    for component in &to[common..] {
        if let Component::Normal(segment) = component {
            parts.push(segment.to_string_lossy().into_owned());
        }
    }
    parts.join(std::path::MAIN_SEPARATOR_STR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_defaults_and_relative_baseline_identity() {
        let config = Config {
            max_bytes: 4096,
            ..Config::default()
        };
        let resolved = resolve_config(&config).expect("resolve");
        assert_eq!(resolved.max_bytes, 4096);
        assert_eq!(resolved.max_source_bytes, DEFAULT_MAX_SOURCE_BYTES);
        assert_eq!(resolved.project_root_markers, vec![".git"]);
        assert_eq!(
            resolved.instruction_file_candidates,
            vec!["AGENTS.md", "CLAUDE.md"]
        );
        let identity = workspace_baseline_identity(&resolved, "/repo/sub", "/repo");
        assert!(identity.contains("projectRoot"));
        assert_eq!(relative_path("/repo/sub", "/repo"), "..");
        assert_eq!(relative_path("/repo", "/repo"), "");
    }

    #[test]
    fn filters_reserved_and_separator_candidates() {
        assert_eq!(
            resolve_instruction_file_candidates(
                Some(&["AGENTS.md", ".", "..", "sub/file.md", "back\\slash.md"].map(str::to_owned)),
                DEFAULT_INSTRUCTION_FILE_CANDIDATES,
            ),
            vec!["AGENTS.md"]
        );
    }
}
