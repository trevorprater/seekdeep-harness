//! Current-surface projection and byte-bounded rendering.

use seekdeep_compaction::is_compact_checkpoint_source;
use seekdeep_core::session::{SessionId, derive_event_message};
use seekdeep_llm::ContentBlock;
use seekdeep_session_query::types::SessionSurfaceSnapshot;
use seekdeep_util::output_retention::{Omitted, TextRetainer, TextRetentionStrategy};
use serde::{Deserialize, Serialize};

use crate::serialization::stringify_tag_safe_json;
use crate::types::{ReferencedConversationItem, ReferencedConversationRole};

/// Snapshot data serialized inside the untrusted prompt.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferencedSessionData {
    /// Durable source session id.
    pub session_id: SessionId,
    /// Display label.
    pub label: String,
    /// Source working directory, when recorded.
    pub cwd: Option<String>,
    /// Highest captured log seq, or none for an empty log.
    pub captured_through_seq: Option<u64>,
    /// Text-only projected conversation.
    pub conversation: Vec<ReferencedConversationItem>,
}

/// Retention facts stored beside the durable context.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReferenceRetentionStats {
    /// Whether the snapshot carried a compaction checkpoint.
    pub compacted: bool,
    /// Messages in the projected source conversation.
    pub original_messages: usize,
    /// Messages retained after budget fitting.
    pub retained_messages: usize,
    /// Messages dropped during budget fitting.
    pub omitted_messages: usize,
    /// UTF-8 bytes omitted during budget fitting.
    pub omitted_bytes: usize,
    /// Whether any content was omitted or truncated.
    pub truncated: bool,
}

/// Retained data plus retention facts for one source snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct RetainedReferencedSession {
    /// Data serialized into the prompt.
    pub data: ReferencedSessionData,
    /// Retention facts stored beside the durable context.
    pub stats: ReferenceRetentionStats,
}

#[derive(Clone, Debug, PartialEq)]
struct ProjectedItem {
    role: ReferencedConversationRole,
    text: String,
    checkpoint: bool,
    original_text: String,
    omitted_bytes: usize,
}

/// Projects current user/assistant conversation while excluding tools, reasoning, and injected context.
fn project_session_conversation(snapshot: &SessionSurfaceSnapshot) -> Vec<ProjectedItem> {
    let mut conversation = Vec::new();
    for event in &snapshot.events {
        match event.event_type.as_str() {
            "user/message" => {
                let Some(message) = derive_event_message(event) else {
                    continue;
                };
                let checkpoint = is_compact_checkpoint_source(message.source());
                if !checkpoint && message.source().kind != "user" {
                    continue;
                }
                let text = text_content(message.content());
                if !text.is_empty() {
                    conversation.push(ProjectedItem {
                        role: ReferencedConversationRole::User,
                        text: text.clone(),
                        checkpoint,
                        original_text: text,
                        omitted_bytes: 0,
                    });
                }
            }
            "assistant/message" => {
                let Some(message) = derive_event_message(event) else {
                    continue;
                };
                let text = text_content(message.content());
                if !text.is_empty() {
                    conversation.push(ProjectedItem {
                        role: ReferencedConversationRole::Assistant,
                        text: text.clone(),
                        checkpoint: false,
                        original_text: text,
                        omitted_bytes: 0,
                    });
                }
            }
            _ => {}
        }
    }
    conversation
}

/// Fits one projected snapshot into an exact rendered JSON-object byte cap.
#[must_use]
pub fn retain_referenced_session(
    snapshot: &SessionSurfaceSnapshot,
    label: &str,
    max_bytes: usize,
) -> Option<RetainedReferencedSession> {
    let original = project_session_conversation(snapshot);
    let mut retained = original.clone();
    let mut omitted_messages = 0;
    let mut dropped_omitted_bytes = 0;

    let data = |items: &[ProjectedItem]| ReferencedSessionData {
        session_id: snapshot.session.id.clone(),
        label: label.to_owned(),
        cwd: snapshot.session.cwd.clone(),
        captured_through_seq: snapshot.captured_through_seq,
        conversation: items
            .iter()
            .map(|item| ReferencedConversationItem {
                role: item.role,
                text: item.text.clone(),
            })
            .collect(),
    };
    let size = |items: &[ProjectedItem]| stringify_tag_safe_json(&data(items)).len();

    while size(&retained) > max_bytes {
        let newest_index = retained.len().wrapping_sub(1);
        let drop_index = retained
            .iter()
            .enumerate()
            .find_map(|(index, item)| (!item.checkpoint && index != newest_index).then_some(index));
        let Some(drop_index) = drop_index else {
            break;
        };
        let removed = retained.remove(drop_index);
        omitted_messages += 1;
        dropped_omitted_bytes += removed.original_text.len();
    }

    while size(&retained) > max_bytes {
        let mut longest_index = None;
        let mut longest_bytes = 0;
        for (index, item) in retained.iter().enumerate() {
            let bytes = item.text.len();
            if bytes > longest_bytes {
                longest_bytes = bytes;
                longest_index = Some(index);
            }
        }
        let longest_index = longest_index?;
        if longest_bytes == 0 {
            return None;
        }
        let overflow = size(&retained) - max_bytes;
        let target = longest_bytes.saturating_sub(overflow);
        let shortened = truncate_with_notice(&retained[longest_index].original_text, target);
        if shortened.text == retained[longest_index].text {
            return None;
        }
        retained[longest_index].text = shortened.text;
        retained[longest_index].omitted_bytes = shortened.omitted_bytes;
    }

    let compacted = original.iter().any(|item| item.checkpoint);
    let retained_omitted_bytes: usize = retained.iter().map(|item| item.omitted_bytes).sum();
    let omitted_bytes = retained_omitted_bytes + dropped_omitted_bytes;
    Some(RetainedReferencedSession {
        data: data(&retained),
        stats: ReferenceRetentionStats {
            compacted,
            original_messages: original.len(),
            retained_messages: retained.len(),
            omitted_messages,
            omitted_bytes,
            truncated: omitted_messages > 0 || omitted_bytes > 0,
        },
    })
}

fn text_content(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(
            "
",
        )
}

struct Truncation {
    text: String,
    omitted_bytes: usize,
}

fn truncate_with_notice(text: &str, max_output_bytes: usize) -> Truncation {
    if text.len() <= max_output_bytes {
        return Truncation {
            text: text.to_owned(),
            omitted_bytes: 0,
        };
    }
    let mut low = 0;
    let mut high = max_output_bytes;
    let mut best = Truncation {
        text: String::new(),
        omitted_bytes: text.len(),
    };
    while low <= high {
        let retained_bytes = usize::midpoint(low, high);
        let head_bytes = retained_bytes.div_ceil(2);
        let tail_bytes = retained_bytes / 2;
        let mut retainer = TextRetainer::new(TextRetentionStrategy::HeadTail {
            head_bytes,
            tail_bytes,
        });
        retainer.push_str(text);
        let result = retainer.finish();
        let Omitted::Exact(omitted) = result.omitted_bytes else {
            panic!("session-reference retention did not report exact omitted bytes");
        };
        let candidate = format!(
            "{}
[… omitted {} UTF-8 bytes …]",
            result.text, omitted
        );
        if candidate.len() <= max_output_bytes {
            best = Truncation {
                text: candidate,
                omitted_bytes: omitted,
            };
            low = retained_bytes + 1;
        } else {
            high = retained_bytes.saturating_sub(1);
        }
    }
    best
}
