//! `/plugins/events` SSE frame contract.

use serde_json::Value;

/// System SSE endpoint shared by both halves.
pub const EVENTS_ENDPOINT: &str = "/plugins/events";

/// Merge-extensible Client plugin event frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginsEventFrame {
    /// Full graph sent on connection.
    Graph {
        /// JSON graph payload retained for forward compatibility.
        graph: Value,
    },
    /// One rebuilt Client bundle.
    Rebuilt {
        /// Stable package identity.
        id: String,
        /// New bundle revision.
        rev: String,
    },
    /// A newer Host frame ignored by this Client version.
    Unknown {
        /// Unknown type discriminator when string-valued.
        frame_type: Option<String>,
        /// Complete parsed payload.
        payload: Value,
    },
}

/// Parses one JSON-compatible frame.
///
/// # Errors
///
/// Requires an object and exact fields for known frame types; unknown types
/// remain lossless and ignored.
pub fn parse_plugins_event_frame(value: &Value) -> anyhow::Result<PluginsEventFrame> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("client-hmr: event frame is not an object"))?;
    let frame_type = object.get("type").and_then(Value::as_str);
    match frame_type {
        Some("graph") => Ok(PluginsEventFrame::Graph {
            graph: object
                .get("graph")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("client-hmr: graph frame has no graph"))?,
        }),
        Some("rebuilt") => Ok(PluginsEventFrame::Rebuilt {
            id: object
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("client-hmr: rebuilt frame id must be a string"))?
                .to_owned(),
            rev: object
                .get("rev")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("client-hmr: rebuilt frame rev must be a string"))?
                .to_owned(),
        }),
        _ => Ok(PluginsEventFrame::Unknown {
            frame_type: frame_type.map(str::to_owned),
            payload: Value::Object(object.clone()),
        }),
    }
}

/// Serializes one known frame as an SSE data record.
#[must_use]
///
/// # Panics
///
/// Panics only if `serde_json::Value` stops being serializable.
pub fn sse_data(value: &Value) -> String {
    format!(
        "data: {}\n\n",
        serde_json::to_string(value).expect("JSON frame serializes")
    )
}
