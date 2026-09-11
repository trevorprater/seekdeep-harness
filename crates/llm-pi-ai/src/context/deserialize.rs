use seekdeep_lossless_json::JsonValue;
use serde::{Deserialize, Deserializer, de::Error as _};

use crate::json::required;

use super::{PiMessage, PiUserContent, PiUserContentBlock};

impl<'de> Deserialize<'de> for PiUserContentBlock {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = <JsonValue as Deserialize>::deserialize(deserializer)?;
        let value = value.as_ref();
        let kind: String = required(value, "type").map_err(D::Error::custom)?;
        match kind.as_str() {
            "text" => Ok(Self::Text {
                text: required(value, "text").map_err(D::Error::custom)?,
            }),
            "image" => Ok(Self::Image {
                data: required(value, "data").map_err(D::Error::custom)?,
                mime_type: required(value, "mimeType").map_err(D::Error::custom)?,
            }),
            _ => Err(D::Error::unknown_variant(&kind, &["text", "image"])),
        }
    }
}

impl<'de> Deserialize<'de> for PiUserContent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = <JsonValue as Deserialize>::deserialize(deserializer)?;
        if value.as_ref().is_string() {
            value
                .deserialize()
                .map(Self::Text)
                .map_err(D::Error::custom)
        } else {
            value
                .deserialize()
                .map(Self::Blocks)
                .map_err(D::Error::custom)
        }
    }
}

impl<'de> Deserialize<'de> for PiMessage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = <JsonValue as Deserialize>::deserialize(deserializer)?;
        let role: String = required(value.as_ref(), "role").map_err(D::Error::custom)?;
        match role.as_str() {
            "user" => value
                .deserialize()
                .map(Self::User)
                .map_err(D::Error::custom),
            "assistant" => value
                .deserialize()
                .map(Self::Assistant)
                .map_err(D::Error::custom),
            "toolResult" => value
                .deserialize()
                .map(Self::ToolResult)
                .map_err(D::Error::custom),
            _ => Err(D::Error::unknown_variant(
                &role,
                &["user", "assistant", "toolResult"],
            )),
        }
    }
}
