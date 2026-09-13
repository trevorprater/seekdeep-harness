use seekdeep_lossless_json::JsonValue;
use serde::{Deserialize, Deserializer, de::Error as _};

use crate::json::{optional, required};

use super::PiAssistantBlock;

impl<'de> Deserialize<'de> for PiAssistantBlock {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = <JsonValue as Deserialize>::deserialize(deserializer)?;
        let value = value.as_ref();
        let kind: String = required(value, "type").map_err(D::Error::custom)?;
        match kind.as_str() {
            "text" => Ok(Self::Text {
                text: required(value, "text").map_err(D::Error::custom)?,
                text_signature: optional(value, "textSignature").map_err(D::Error::custom)?,
            }),
            "thinking" => Ok(Self::Thinking {
                thinking: required(value, "thinking").map_err(D::Error::custom)?,
                thinking_signature: optional(value, "thinkingSignature")
                    .map_err(D::Error::custom)?,
                redacted: optional(value, "redacted").map_err(D::Error::custom)?,
            }),
            "toolCall" => Ok(Self::ToolCall {
                id: required(value, "id").map_err(D::Error::custom)?,
                name: required(value, "name").map_err(D::Error::custom)?,
                arguments: required(value, "arguments").map_err(D::Error::custom)?,
                thought_signature: optional(value, "thoughtSignature").map_err(D::Error::custom)?,
            }),
            _ => Err(D::Error::unknown_variant(
                &kind,
                &["text", "thinking", "toolCall"],
            )),
        }
    }
}
