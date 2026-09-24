//! Host-only download query contracts.

use seekdeep_core::session::SessionId;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    rpc::ContractError,
    sessions::{require_nonempty_string, require_object, require_string},
};

/// Parsed query for one Session-log ZIP download.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLogQuery {
    /// Root Session.
    pub session_id: SessionId,
    /// Present only when descendants were explicitly requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_descendants: Option<bool>,
}

impl SessionLogQuery {
    /// Parses raw string-valued query parameters and applies the source transform.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty Session id or any flag other than `true`, `false`, or absence.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        let session_id =
            SessionId::new(require_nonempty_string(object, "sessionId", "$.sessionId")?);
        let include_descendants = match object.get("includeDescendants") {
            None => None,
            Some(_) => {
                match require_string(object, "includeDescendants", "$.includeDescendants", false)? {
                    "true" => Some(true),
                    "false" => None,
                    _ => {
                        return Err(ContractError::new(
                            "$.includeDescendants",
                            "expected true or false query literal",
                        ));
                    }
                }
            }
        };
        Ok(Self {
            session_id,
            include_descendants,
        })
    }
}
