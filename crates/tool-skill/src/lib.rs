//! Durable session skill catalog rendering: the model-facing available-skills
//! catalog prose and the slash-name invocation gesture, shared by the skill loader
//! tool and the step listeners that publish and replace the catalog.

use std::sync::{Arc, LazyLock};

use regex::Regex;
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use seekdeep_llm::{ContentBlock, MessageSource, UserMessage};
use seekdeep_skill::{SkillSummary, escape_text};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// Default maximum normalized description length rendered in the session catalog.
pub const DEFAULT_CATALOG_DESCRIPTION_MAX_LENGTH: usize = 500;

/// One durable catalog entry: the skill name and its normalized description.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntry {
    /// Kebab-case skill name.
    pub name: String,
    /// Normalized, length-bounded description.
    pub description: String,
}

/// Durable source for a published session skill catalog.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillCatalogSource {
    /// Catalog source kind.
    pub kind: SkillCatalogSourceKind,
    /// Catalog form.
    pub form: SkillCatalogForm,
    /// Marks a replacement catalog rather than the first publication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<bool>,
    /// Exactly the entries this message published, in catalog order.
    pub entries: Vec<CatalogEntry>,
}

/// Closed source kind for catalog messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SkillCatalogSourceKind {
    /// A session skill catalog.
    SkillCatalog,
}

/// Closed catalog form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SkillCatalogForm {
    /// A catalog-form context message.
    Catalog,
}

impl SkillCatalogSource {
    /// Builds the lossless message source carried by catalog messages.
    #[must_use]
    pub fn message_source(&self) -> MessageSource {
        let mut fields = Map::new();
        fields.insert("form".to_owned(), Value::String("catalog".to_owned()));
        if self.update == Some(true) {
            fields.insert("update".to_owned(), Value::Bool(true));
        }
        fields.insert(
            "entries".to_owned(),
            serde_json::to_value(&self.entries).unwrap_or(Value::Null),
        );
        MessageSource {
            kind: "skill-catalog".to_owned(),
            fields,
        }
    }
}

/// Model-facing skill catalog configuration.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    /// Maximum normalized description length rendered in the session catalog; minimum 3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_description_max_length: Option<usize>,
}

/// Durable entry list mirroring the rendered catalog lines, for non-model consumers.
#[must_use]
pub fn catalog_source_entries(
    skills: &[SkillSummary],
    description_max_length: usize,
) -> Vec<CatalogEntry> {
    skills
        .iter()
        .map(|skill| CatalogEntry {
            name: skill.name.clone(),
            description: catalog_description(&skill.description, description_max_length),
        })
        .collect()
}

/// Normalized, length-bounded description exactly as the catalog publishes it (unescaped).
#[must_use]
pub fn catalog_description(value: &str, max_length: usize) -> String {
    let normalized = WHITESPACE.replace_all(value, " ").trim().to_owned();
    if normalized.chars().count() <= max_length {
        normalized
    } else {
        let truncated: String = normalized
            .chars()
            .take(max_length.saturating_sub(3))
            .collect();
        format!("{truncated}...")
    }
}

/// Catalog identity over the durable entry list rather than the rendered prose.
#[must_use]
pub fn digest_catalog_entries(entries: &[CatalogEntry]) -> String {
    let canonical = entries
        .iter()
        .map(|entry| serde_json::to_string(&[&entry.name, &entry.description]).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
    hex::encode(Sha256::digest(canonical.as_bytes()))
}

/// Model-facing catalog lines, projected from the same entries the source records.
#[must_use]
pub fn render_catalog_entries(entries: &[CatalogEntry]) -> Vec<String> {
    entries
        .iter()
        .map(|entry| format!("- `{}`: {}", entry.name, escape_text(&entry.description)))
        .collect()
}

/// Renders the first-session catalog message.
#[must_use]
pub fn render_catalog_message(entries: &[CatalogEntry]) -> UserMessage {
    let text = [
        "<system-reminder>",
        "A skill is a reusable set of task-specific instructions. The following skills are available in this session:",
        "",
        "<available_skills>",
        render_catalog_entries(entries).join("\n").as_str(),
        "</available_skills>",
        "",
        "If the user names a skill, or the task clearly matches a skill's description, call the `skill` tool with the exact skill name before taking task actions. Load all applicable skills, then follow their full instructions. This catalog contains summaries only; do not infer or follow a skill's instructions until it has been loaded.",
        "A user may also invoke a skill directly; its <skill_content> block then appears in this conversation. Follow it, and do not call the `skill` tool again for that skill.",
        "</system-reminder>",
    ]
    .join("\n");
    UserMessage::new(
        vec![ContentBlock::Text { text }],
        SkillCatalogSource {
            kind: SkillCatalogSourceKind::SkillCatalog,
            form: SkillCatalogForm::Catalog,
            update: None,
            entries: entries.to_vec(),
        }
        .message_source(),
    )
}

/// Renders a replacement catalog message.
#[must_use]
pub fn render_catalog_update(entries: &[CatalogEntry]) -> UserMessage {
    let availability = if entries.is_empty() {
        [
            "No skills are currently available through the `skill` tool. Do not use names from earlier skill catalogs.",
            "A user may still invoke a skill directly; its <skill_content> block then appears in this conversation. Follow it, and do not call the `skill` tool for it.",
        ]
        .join("\n")
    } else {
        [
            "Use only names in this replacement catalog. If the user names a listed skill, or the task clearly matches its description, call the `skill` tool with the exact name before acting.",
            "A user may also invoke a skill directly; its <skill_content> block then appears in this conversation. Follow it, and do not call the `skill` tool again for that skill.",
        ]
        .join("\n")
    };
    let text = [
        "<system-reminder>",
        "The available skill catalog changed. This complete catalog replaces every earlier available-skills list in this session:",
        "",
        "<available_skills>",
        render_catalog_entries(entries).join("\n").as_str(),
        "</available_skills>",
        "",
        availability.as_str(),
        "</system-reminder>",
    ]
    .join("\n");
    UserMessage::new(
        vec![ContentBlock::Text { text }],
        SkillCatalogSource {
            kind: SkillCatalogSourceKind::SkillCatalog,
            form: SkillCatalogForm::Catalog,
            update: Some(true),
            entries: entries.to_vec(),
        }
        .message_source(),
    )
}

/// Reads entries of one durable catalog source, or none when unreadable.
#[must_use]
pub fn read_catalog_entries(source: &MessageSource) -> Option<Vec<CatalogEntry>> {
    let entries = source.fields.get("entries")?.as_array()?;
    let mut readable = Vec::with_capacity(entries.len());
    for entry in entries {
        let object = entry.as_object()?;
        let name = object.get("name")?.as_str()?;
        if name.is_empty() {
            return None;
        }
        let description = object.get("description")?.as_str()?;
        readable.push(CatalogEntry {
            name: name.to_owned(),
            description: description.to_owned(),
        });
    }
    Some(readable)
}

/// Finds the first usable catalog message in a proposed batch.
#[must_use]
pub fn catalog_message(messages: &[UserMessage]) -> Option<(UserMessage, Vec<CatalogEntry>)> {
    for message in messages {
        if message.source().kind != "skill-catalog" {
            continue;
        }
        if let Some(entries) = read_catalog_entries(message.source()) {
            return Some((message.clone(), entries));
        }
    }
    None
}

/// Slash-name gesture tokens from claimed user messages, deduplicated in first-seen order.
#[must_use]
pub fn invoked_skill_names(messages: &[UserMessage]) -> Vec<String> {
    let mut names = Vec::new();
    for message in messages {
        if message.source().kind != "user" {
            continue;
        }
        for block in message.content() {
            let ContentBlock::Text { text } = block else {
                continue;
            };
            for captures in SKILL_GESTURE.captures_iter(text) {
                let Some(full) = captures.get(0) else {
                    continue;
                };
                let Some(name) = captures.get(1) else {
                    continue;
                };
                let trailing = &text[full.end()..];
                if !trailing.is_empty() && !trailing.chars().next().is_some_and(char::is_whitespace)
                {
                    continue;
                }
                if !names.iter().any(|existing| existing == name.as_str()) {
                    names.push(name.as_str().to_owned());
                }
            }
        }
    }
    names
}

/// Registers the package's explained empty invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register("seekdeep-tool-skill", InvariantInstaller::noop())
}

static WHITESPACE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s+").expect("static whitespace regex"));
static SKILL_GESTURE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|\s)/([a-z0-9]+(?:-[a-z0-9]+)*)").expect("static skill gesture regex")
});

#[cfg(test)]
mod tests {
    use seekdeep_skill::{SkillInvocationPolicy, SkillSource};

    use super::*;

    fn summary(name: &str, description: &str) -> SkillSummary {
        SkillSummary {
            name: name.to_owned(),
            description: description.to_owned(),
            when_to_use: None,
            invocation: SkillInvocationPolicy {
                model_invocable: true,
                user_invocable: true,
            },
            source: SkillSource("bundled".to_owned()),
            provider: "stub".to_owned(),
            resource_base: None,
        }
    }

    #[test]
    fn catalog_description_normalizes_and_truncates() {
        assert_eq!(catalog_description("a  b\n\tc", 10), "a b c");
        assert_eq!(catalog_description("exactly ten", 11), "exactly ten");
        assert_eq!(catalog_description("1234567890", 8), "12345...");
    }

    #[test]
    fn digest_is_stable_over_entries_and_detects_change() {
        let entries = catalog_source_entries(&[summary("a-b", "first")], 500);
        let same = catalog_source_entries(&[summary("a-b", "first")], 500);
        assert_eq!(
            digest_catalog_entries(&entries),
            digest_catalog_entries(&same)
        );
        let changed = catalog_source_entries(&[summary("a-b", "second")], 500);
        assert_ne!(
            digest_catalog_entries(&entries),
            digest_catalog_entries(&changed)
        );
    }

    #[test]
    fn catalog_entries_round_trip_through_message_source() {
        let entries =
            catalog_source_entries(&[summary("a-b", "first"), summary("c-d", "second")], 500);
        let source = SkillCatalogSource {
            kind: SkillCatalogSourceKind::SkillCatalog,
            form: SkillCatalogForm::Catalog,
            update: Some(true),
            entries: entries.clone(),
        }
        .message_source();
        assert_eq!(read_catalog_entries(&source), Some(entries));
    }

    #[test]
    fn catalog_message_renders_escaped_descriptions() {
        let message =
            render_catalog_message(&catalog_source_entries(&[summary("a-b", "a <b> & c")], 500));
        let Some(ContentBlock::Text { text }) = message.content().first() else {
            panic!("expected text block");
        };
        assert!(text.contains("<available_skills>"));
        assert!(text.contains("- `a-b`: a &lt;b&gt; &amp; c"));
        assert!(text.contains("</system-reminder>"));
    }

    #[test]
    fn gesture_scans_only_user_text_and_respects_boundaries() {
        let user = UserMessage::new(
            vec![ContentBlock::Text {
                text: "use /dsh-badge and /skill-filesystem /usr/bin 5/8 /x/y".to_owned(),
            }],
            MessageSource::user(),
        );
        let plugin = UserMessage::new(
            vec![ContentBlock::Text {
                text: "/forged".to_owned(),
            }],
            MessageSource::plugin("stub"),
        );
        assert_eq!(
            invoked_skill_names(&[user, plugin]),
            vec!["dsh-badge".to_owned(), "skill-filesystem".to_owned()]
        );
    }
}
