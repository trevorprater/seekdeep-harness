//! Fixed-density heuristic shared by the service and projection folds.

use seekdeep_core::request_header::EpochHeader;
use seekdeep_llm::{ContentBlock, Message};

const CHARS_PER_TOKEN: usize = 4;
const BLOCK_OVERHEAD: u64 = 4;

/// Role-field framing overhead added to every priced message.
pub const ROLE_OVERHEAD: u64 = 4;

fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

fn dense_tokens(value: &str) -> u64 {
    u64::try_from(utf16_len(value).div_ceil(CHARS_PER_TOKEN)).unwrap_or(u64::MAX)
}

fn serialized_tokens<T: serde::Serialize>(value: &T) -> u64 {
    dense_tokens(&serde_json::to_string(value).expect("typed token-meter JSON is serializable"))
}

/// Prices content blocks recursively under the fixed source heuristic.
#[must_use]
pub fn estimate_content(blocks: &[ContentBlock]) -> u64 {
    blocks.iter().fold(0_u64, |total, block| {
        let tokens = match block {
            ContentBlock::Text { text } | ContentBlock::Reasoning { text } => {
                dense_tokens(text).saturating_add(BLOCK_OVERHEAD)
            }
            ContentBlock::ToolCall {
                name, arguments, ..
            } => dense_tokens(name)
                .saturating_add(dense_tokens(arguments))
                .saturating_add(BLOCK_OVERHEAD),
            ContentBlock::ToolResult { content, .. } => {
                estimate_content(content).saturating_add(BLOCK_OVERHEAD)
            }
            ContentBlock::Image { .. } | ContentBlock::Unknown { .. } => {
                BLOCK_OVERHEAD.saturating_add(serialized_tokens(block))
            }
        };
        total.saturating_add(tokens)
    })
}

/// Prices one model-visible message including role framing.
#[must_use]
pub fn estimate_message(message: &Message) -> u64 {
    estimate_content(message.content()).saturating_add(ROLE_OVERHEAD)
}

/// Prices the system-prompt part of a canonical envelope.
#[must_use]
pub fn estimate_system_tokens(header: Option<&EpochHeader>) -> u64 {
    header
        .and_then(|header| header.system.as_deref())
        .map_or(0, |system| {
            dense_tokens(system).saturating_add(ROLE_OVERHEAD)
        })
}

/// Prices the tool-schema part of a canonical envelope.
#[must_use]
pub fn estimate_tools_tokens(header: Option<&EpochHeader>) -> u64 {
    header
        .and_then(|header| header.tools.as_deref())
        .filter(|tools| !tools.is_empty())
        .map_or(0, |tools| {
            serialized_tokens(&tools).saturating_add(BLOCK_OVERHEAD)
        })
}

/// Prices the complete non-surface request envelope.
#[must_use]
pub fn estimate_header(header: Option<&EpochHeader>) -> u64 {
    estimate_system_tokens(header).saturating_add(estimate_tools_tokens(header))
}

#[cfg(test)]
mod tests {
    use seekdeep_llm::{CallId, MessageRole, MessageSource};
    use serde_json::{Map, json};

    use super::*;

    #[test]
    fn uses_javascript_utf16_density_and_every_content_shape() {
        let blocks = vec![
            ContentBlock::Text {
                text: "hello".to_owned(),
            },
            ContentBlock::Reasoning {
                text: "1234".to_owned(),
            },
            ContentBlock::ToolCall {
                id: CallId::new("ignored"),
                name: "tool".to_owned(),
                arguments: "{\"x\":1}".to_owned(),
            },
            ContentBlock::ToolResult {
                tool_call_id: CallId::new("ignored"),
                content: vec![ContentBlock::Text {
                    text: "nested".to_owned(),
                }],
                is_error: Some(false),
            },
            ContentBlock::Unknown {
                block_type: "custom".to_owned(),
                fields: Map::from_iter([("value".to_owned(), json!("x"))]),
            },
        ];
        let message = Message::new(MessageRole::User, blocks.clone(), MessageSource::user());
        assert_eq!(estimate_message(&message), estimate_content(&blocks) + 4);
        assert_eq!(dense_tokens("😀😀"), 1);
        assert_eq!(dense_tokens("😀😀😀"), 2);
    }
}
