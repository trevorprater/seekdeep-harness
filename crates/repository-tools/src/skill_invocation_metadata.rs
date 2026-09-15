//! Alignment of Claude Code and Codex invocation metadata for repository Skills.

use std::path::Path;

use serde_yml::{Mapping, Value};

/// Cross-product Skill policy inspection result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillInvocationMetadataReport {
    /// Skill directories carrying Codex product metadata.
    pub pair_count: usize,
    /// Ordered malformed-metadata or policy-alignment diagnostics.
    pub violations: Vec<String>,
}

/// Reports cross-product invocation-policy mismatches for repository Skills.
///
/// # Errors
///
/// Returns Skill-root directory traversal failures.
pub fn inspect_skill_invocation_metadata(
    repository_root: &Path,
) -> anyhow::Result<SkillInvocationMetadataReport> {
    let skills = skill_directories(repository_root)?;
    let mut violations = Vec::new();
    for skill in &skills {
        let relative_root = format!(".agents/skills/{skill}");
        let directory = repository_root.join(&relative_root);
        let skill_file = directory.join("SKILL.md");
        let openai_file = directory.join("agents/openai.yaml");
        if !skill_file.exists() {
            violations.push(format!(
                "{relative_root}: agents/openai.yaml has no sibling SKILL.md"
            ));
            continue;
        }
        let frontmatter = match std::fs::read_to_string(&skill_file)
            .map_err(anyhow::Error::from)
            .and_then(|source| parse_skill_frontmatter(&source))
        {
            Ok(frontmatter) => frontmatter,
            Err(error) => {
                violations.push(format!("{relative_root}/SKILL.md: {error}"));
                continue;
            }
        };
        let openai = match std::fs::read_to_string(&openai_file)
            .map_err(anyhow::Error::from)
            .and_then(|source| {
                parse_yaml_object(&source, "agents/openai.yaml must be a YAML object")
            }) {
            Ok(openai) => openai,
            Err(error) => {
                violations.push(format!("{relative_root}/agents/openai.yaml: {error}"));
                continue;
            }
        };

        let disable_model_invocation = mapping_value(&frontmatter, "disable-model-invocation");
        if disable_model_invocation.is_some_and(|value| !value.is_bool()) {
            violations.push(format!(
                "{relative_root}/SKILL.md: disable-model-invocation must be a boolean"
            ));
            continue;
        }
        let user_invocable = mapping_value(&frontmatter, "user-invocable");
        if user_invocable.is_some_and(|value| !value.is_bool()) {
            violations.push(format!(
                "{relative_root}/SKILL.md: user-invocable must be a boolean"
            ));
            continue;
        }
        let policy = mapping_value(&openai, "policy").and_then(Value::as_mapping);
        let allow_implicit =
            policy.and_then(|policy| mapping_value(policy, "allow_implicit_invocation"));
        if allow_implicit.is_some_and(|value| !value.is_bool()) {
            violations.push(format!(
                "{relative_root}/agents/openai.yaml: policy.allow_implicit_invocation must be a boolean"
            ));
            continue;
        }

        let claude_manual_only = disable_model_invocation.and_then(Value::as_bool) == Some(true);
        let codex_manual_only = allow_implicit.and_then(Value::as_bool) == Some(false);
        if claude_manual_only != codex_manual_only {
            violations.push(format!(
                "{relative_root}: Claude Code manual-only={claude_manual_only} but Codex manual-only={codex_manual_only}"
            ));
        }
        if claude_manual_only && user_invocable.and_then(Value::as_bool) == Some(false) {
            violations.push(format!(
                "{relative_root}/SKILL.md: a manual-only skill must remain user-invocable"
            ));
        }
    }
    Ok(SkillInvocationMetadataReport {
        pair_count: skills.len(),
        violations,
    })
}

fn skill_directories(root: &Path) -> anyhow::Result<Vec<String>> {
    let skills_root = root.join(".agents/skills");
    if !skills_root.exists() {
        return Ok(Vec::new());
    }
    let mut skills = Vec::new();
    for entry in std::fs::read_dir(skills_root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() && entry.path().join("agents/openai.yaml").exists() {
            skills.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    skills.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
    Ok(skills)
}

fn parse_skill_frontmatter(source: &str) -> anyhow::Result<Mapping> {
    let lines = source.split('\n').collect::<Vec<_>>();
    anyhow::ensure!(
        lines.first().copied() == Some("---"),
        "SKILL.md must start with YAML frontmatter"
    );
    let end = lines
        .iter()
        .enumerate()
        .skip(1)
        .find_map(|(index, line)| (*line == "---").then_some(index))
        .ok_or_else(|| anyhow::anyhow!("SKILL.md frontmatter is not closed"))?;
    parse_yaml_object(
        &lines[1..end].join("\n"),
        "SKILL.md frontmatter must be a YAML object",
    )
}

fn parse_yaml_object(source: &str, shape_error: &str) -> anyhow::Result<Mapping> {
    let value: Value = serde_yml::from_str(source)?;
    value
        .as_mapping()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!(shape_error.to_owned()))
}

fn mapping_value<'a>(mapping: &'a Mapping, key: &str) -> Option<&'a Value> {
    mapping.get(Value::String(key.to_owned()))
}
