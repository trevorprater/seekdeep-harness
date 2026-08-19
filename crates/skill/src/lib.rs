//! Agent skill registry foundation: the shared skill types and the
//! model-visible rendering plus name/invocation validation.

use std::sync::Arc;

use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Standard precedence rank for packaged skill providers and local bundled roots.
pub const BUNDLED_SKILL_RANK: f64 = 600.0;

/// Returns whether a string is a valid kebab-case skill name.
#[must_use]
pub fn is_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name.split('-').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Origin bucket for a skill contribution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SkillSource(pub String);

impl std::fmt::Display for SkillSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Optional provider-specific base used by loaded skill bodies.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SkillResourceBase {
    /// Local directory base.
    Directory {
        /// Absolute or workspace-relative path.
        path: String,
    },
    /// Remote URL base.
    Url {
        /// Base URL.
        url: String,
    },
    /// Opaque resource description.
    Opaque {
        /// Human-readable description.
        description: String,
    },
}

/// Invocation controls shared by skill discovery consumers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillInvocationPolicy {
    /// Whether model-facing catalogs and loaders include this skill.
    pub model_invocable: bool,
    /// Whether human-facing command catalogs and loaders include this skill.
    pub user_invocable: bool,
}

/// Invocation-neutral skill metadata.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillSummary {
    /// Kebab-case identifier.
    pub name: String,
    /// Short routing description.
    pub description: String,
    /// Optional fuller when-to-use guidance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when_to_use: Option<String>,
    /// Invocation controls.
    pub invocation: SkillInvocationPolicy,
    /// Origin bucket.
    pub source: SkillSource,
    /// Provider label.
    pub provider: String,
    /// Optional resource base.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_base: Option<SkillResourceBase>,
}

/// Complete parsed skill definition including the loaded body.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillDefinition {
    /// Shared summary fields.
    #[serde(flatten)]
    pub summary: SkillSummary,
    /// Markdown instruction body.
    pub content: String,
    /// Absolute file path when the skill came from disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Parsed optional frontmatter metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

/// Whether a skill may be advertised to and loaded by a model.
#[must_use]
pub fn is_model_invocable(skill: &SkillSummary) -> bool {
    skill.invocation.model_invocable
}

/// Whether a skill may be advertised to and loaded by a human-facing command.
#[must_use]
pub fn is_user_invocable(skill: &SkillSummary) -> bool {
    skill.invocation.user_invocable
}

/// Renders one loaded skill for the model as a canonical `skill_content` block.
#[must_use]
pub fn render_skill_content(skill: &SkillDefinition) -> String {
    let summary = &skill.summary;
    let resource_hint = render_resource_hint(summary);
    let mut lines = vec![format!(
        "<skill_content name=\"{}\">",
        escape_attr(&summary.name)
    )];
    lines.push("<skill_resources>".to_owned());
    lines.extend(resource_hint);
    lines.push("</skill_resources>".to_owned());
    lines.push(String::new());
    lines.push("<skill_instructions>".to_owned());
    lines.push(skill.content.clone());
    lines.push("</skill_instructions>".to_owned());
    lines.push("</skill_content>".to_owned());
    lines.join(
        "
",
    )
}

fn render_resource_hint(summary: &SkillSummary) -> Vec<String> {
    match &summary.resource_base {
        None => vec![
            format!(
                "Resources for this skill are managed by provider \"{}\".",
                escape_text(&summary.provider),
            ),
            "Load referenced resources only as needed.".to_owned(),
        ],
        Some(SkillResourceBase::Directory { path }) => vec![
            format!("Base directory for this skill: {}", escape_text(path)),
            "Resolve relative paths mentioned by this skill against the base directory before using them. Load referenced resources only as needed.".to_owned(),
        ],
        Some(SkillResourceBase::Url { url }) => vec![
            format!("Base URL for this skill: {}", escape_text(url)),
            "Resolve relative URLs mentioned by this skill against the base URL before using them. Load referenced resources only as needed.".to_owned(),
        ],
        Some(SkillResourceBase::Opaque { description }) => vec![
            format!("Resources for this skill: {}", escape_text(description)),
            "Load referenced resources only as needed.".to_owned(),
        ],
    }
}

/// Escapes model-facing attribute text so it cannot open framing tags.
#[must_use]
pub fn escape_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
}

/// Escapes model-facing prose embedded inside skill markup.
#[must_use]
pub fn escape_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Registers the package's explained empty invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register("seekdeep-skill", InvariantInstaller::noop())
}

#[cfg(test)]
mod tests {
    use seekdeep_cordis::Context;
    use seekdeep_invariants::InvariantConfig;

    use super::*;

    fn skill() -> SkillDefinition {
        SkillDefinition {
            summary: SkillSummary {
                name: "dsh-badge".to_owned(),
                description: "Add a badge".to_owned(),
                when_to_use: None,
                invocation: SkillInvocationPolicy {
                    model_invocable: true,
                    user_invocable: true,
                },
                source: SkillSource("bundled".to_owned()),
                provider: "dsh-badge".to_owned(),
                resource_base: Some(SkillResourceBase::Directory {
                    path: "/skills/badge".to_owned(),
                }),
            },
            content: "render a badge".to_owned(),
            path: None,
            metadata: None,
        }
    }

    #[test]
    fn skill_name_grammar_accepts_kebab_case_only() {
        assert!(is_skill_name("dsh-badge"));
        assert!(is_skill_name("skill-filesystem"));
        assert!(is_skill_name("a1-b2"));
        assert!(!is_skill_name(""));
        assert!(!is_skill_name("Badge"));
        assert!(!is_skill_name("-badge"));
        assert!(!is_skill_name("badge-"));
        assert!(!is_skill_name("badge--x"));
    }

    #[test]
    fn invocation_policy_is_resolved_independently() {
        let skill = skill();
        assert!(is_model_invocable(&skill.summary));
        assert!(is_user_invocable(&skill.summary));
        let mut summary = skill.summary.clone();
        summary.invocation.user_invocable = false;
        assert!(!is_user_invocable(&summary));
    }

    #[test]
    fn render_skill_content_embeds_escaped_attributes_and_verbatim_body() {
        let rendered = render_skill_content(&skill());
        assert!(rendered.contains(r#"<skill_content name="dsh-badge">"#));
        assert!(rendered.contains("<skill_instructions>"));
        assert!(rendered.contains("render a badge"));
        assert!(rendered.contains("</skill_content>"));

        let mut evil = skill();
        evil.summary.name = "a\"<&".to_owned();
        let rendered = render_skill_content(&evil);
        assert!(rendered.contains("a&quot;&lt;&amp;"));
    }

    #[test]
    fn escaping_is_total_for_prose_and_attributes() {
        assert_eq!(escape_text("a&b<c>d"), "a&amp;b&lt;c&gt;d");
        assert_eq!(escape_attr("a\"b<c&d"), "a&quot;b&lt;c&amp;d");
    }

    #[tokio::test]
    async fn explained_empty_invariant_reserves_and_releases_package_identity() {
        let context = Context::new();
        let registry =
            InvariantRegistry::install(&context, &InvariantConfig::default()).expect("registry");
        let registration = register_invariant(&registry).expect("register");
        assert!(register_invariant(&registry).is_err());
        registration.dispose().await.expect("dispose");
        register_invariant(&registry).expect("replacement");
    }
}
