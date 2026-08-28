//! Target-portable trajectory contribution contract.

use serde_json::{Value, json};

use crate::TrajectoryRequestHeaderState;

/// One independently assembled contribution to the trajectory ledger.
#[derive(Clone, Debug, PartialEq)]
pub enum TrajectoryContribution {
    /// Finalized Conversation node.
    Node {
        /// Complete node value.
        node: Value,
    },
    /// Assistant final/partial/request bundle.
    Assistant {
        /// Finalized or interruption-frozen node.
        node: Option<Value>,
        /// In-flight partial; `None` serializes as null.
        partial: Option<Value>,
        /// Provider request lifecycle.
        request: Option<Value>,
    },
    /// Root Tool call/result tree.
    Tool {
        /// Recursive root block.
        root: Value,
    },
    /// Model-visible request header.
    RequestHeader {
        /// Header state and placement.
        header: TrajectoryRequestHeaderState,
    },
    /// Compaction provider request.
    Compaction {
        /// Complete request view.
        request: Value,
    },
    /// Session termination boundary.
    SessionEnd {
        /// Boundary sequence.
        seq: u64,
        /// Boundary Unix milliseconds.
        time: i64,
    },
    /// Turn termination boundary.
    TurnEnd {
        /// Turn number.
        turn: i64,
        /// Boundary Unix milliseconds.
        time: i64,
        /// Display-safe failure.
        error: Option<String>,
    },
}

impl TrajectoryContribution {
    /// Parses one JSON-safe contribution.
    ///
    /// # Errors
    ///
    /// Returns a missing, unknown, or malformed variant diagnostic.
    pub fn from_value(value: &Value) -> Result<Self, String> {
        match value.get("kind").and_then(Value::as_str) {
            Some("node") => Ok(Self::Node {
                node: required(value, "node")?.clone(),
            }),
            Some("assistant") => Ok(Self::Assistant {
                node: value.get("node").cloned(),
                partial: value
                    .get("partial")
                    .filter(|value| !value.is_null())
                    .cloned(),
                request: value.get("request").cloned(),
            }),
            Some("tool") => Ok(Self::Tool {
                root: required(value, "root")?.clone(),
            }),
            Some("request-header") => Ok(Self::RequestHeader {
                header: serde_json::from_value(required(value, "header")?.clone())
                    .map_err(|error| error.to_string())?,
            }),
            Some("compaction") => Ok(Self::Compaction {
                request: required(value, "request")?.clone(),
            }),
            Some("session-end") => Ok(Self::SessionEnd {
                seq: required(value, "seq")?
                    .as_u64()
                    .ok_or_else(|| "trajectory session-end seq must be a u64".to_owned())?,
                time: required(value, "time")?
                    .as_i64()
                    .ok_or_else(|| "trajectory session-end time must be an i64".to_owned())?,
            }),
            Some("turn-end") => Ok(Self::TurnEnd {
                turn: required(value, "turn")?
                    .as_i64()
                    .ok_or_else(|| "trajectory turn-end turn must be an i64".to_owned())?,
                time: required(value, "time")?
                    .as_i64()
                    .ok_or_else(|| "trajectory turn-end time must be an i64".to_owned())?,
                error: value
                    .get("error")
                    .map(|error| {
                        error
                            .as_str()
                            .map(ToOwned::to_owned)
                            .ok_or_else(|| "trajectory turn-end error must be a string".to_owned())
                    })
                    .transpose()?,
            }),
            Some(kind) => Err(format!("unknown trajectory contribution kind {kind:?}")),
            None => Err("trajectory contribution omitted kind".to_owned()),
        }
    }

    /// Serializes the exact source object shape.
    #[must_use]
    pub fn to_value(&self) -> Value {
        match self {
            Self::Node { node } => json!({"kind": "node", "node": node}),
            Self::Assistant {
                node,
                partial,
                request,
            } => {
                let mut value = serde_json::Map::from_iter([
                    ("kind".to_owned(), json!("assistant")),
                    ("partial".to_owned(), partial.clone().unwrap_or(Value::Null)),
                ]);
                if let Some(node) = node {
                    value.insert("node".to_owned(), node.clone());
                }
                if let Some(request) = request {
                    value.insert("request".to_owned(), request.clone());
                }
                Value::Object(value)
            }
            Self::Tool { root } => json!({"kind": "tool", "root": root}),
            Self::RequestHeader { header } => {
                json!({"kind": "request-header", "header": header})
            }
            Self::Compaction { request } => {
                json!({"kind": "compaction", "request": request})
            }
            Self::SessionEnd { seq, time } => {
                json!({"kind": "session-end", "seq": seq, "time": time})
            }
            Self::TurnEnd { turn, time, error } => {
                let mut value = serde_json::Map::from_iter([
                    ("kind".to_owned(), json!("turn-end")),
                    ("turn".to_owned(), json!(turn)),
                    ("time".to_owned(), json!(time)),
                ]);
                if let Some(error) = error {
                    value.insert("error".to_owned(), json!(error));
                }
                Value::Object(value)
            }
        }
    }
}

fn required<'a>(value: &'a Value, key: &str) -> Result<&'a Value, String> {
    value
        .get(key)
        .ok_or_else(|| format!("trajectory contribution omitted {key}"))
}
