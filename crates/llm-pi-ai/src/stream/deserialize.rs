use seekdeep_lossless_json::JsonValue;
use serde::{Deserialize, Deserializer, de::Error as _};

use crate::json::required;

use super::PiAssistantEvent;

impl<'de> Deserialize<'de> for PiAssistantEvent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = <JsonValue as Deserialize>::deserialize(deserializer)?;
        let value = value.as_ref();
        let kind: String = required(value, "type").map_err(D::Error::custom)?;
        macro_rules! field {
            ($key:literal) => {
                required(value, $key).map_err(D::Error::custom)?
            };
        }
        Ok(match kind.as_str() {
            "start" => Self::Start {
                partial: field!("partial"),
            },
            "text_start" => Self::TextStart {
                content_index: field!("contentIndex"),
                partial: field!("partial"),
            },
            "text_delta" => Self::TextDelta {
                content_index: field!("contentIndex"),
                delta: field!("delta"),
                partial: field!("partial"),
            },
            "text_end" => Self::TextEnd {
                content_index: field!("contentIndex"),
                content: field!("content"),
                partial: field!("partial"),
            },
            "thinking_start" => Self::ThinkingStart {
                content_index: field!("contentIndex"),
                partial: field!("partial"),
            },
            "thinking_delta" => Self::ThinkingDelta {
                content_index: field!("contentIndex"),
                delta: field!("delta"),
                partial: field!("partial"),
            },
            "thinking_end" => Self::ThinkingEnd {
                content_index: field!("contentIndex"),
                content: field!("content"),
                partial: field!("partial"),
            },
            "toolcall_start" => Self::ToolCallStart {
                content_index: field!("contentIndex"),
                partial: field!("partial"),
            },
            "toolcall_delta" => Self::ToolCallDelta {
                content_index: field!("contentIndex"),
                delta: field!("delta"),
                partial: field!("partial"),
            },
            "toolcall_end" => Self::ToolCallEnd {
                content_index: field!("contentIndex"),
                tool_call: field!("toolCall"),
                partial: field!("partial"),
            },
            "done" => Self::Done {
                reason: field!("reason"),
                message: field!("message"),
            },
            "error" => Self::Error {
                reason: field!("reason"),
                error: field!("error"),
            },
            _ => {
                return Err(D::Error::unknown_variant(
                    &kind,
                    &[
                        "start",
                        "text_start",
                        "text_delta",
                        "text_end",
                        "thinking_start",
                        "thinking_delta",
                        "thinking_end",
                        "toolcall_start",
                        "toolcall_delta",
                        "toolcall_end",
                        "done",
                        "error",
                    ],
                ));
            }
        })
    }
}
