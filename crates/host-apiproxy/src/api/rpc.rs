//! Four-quadrant RPC message model and its first-level wire schemas.

use std::fmt;

use seekdeep_client_connection::{ClientRequest, RpcError, RpcId, RpcResult, ServerResponse};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};
use thiserror::Error;

/// Narrow domain request: the physical carrier owns the full-form type and method.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RpcRequest<T = Value> {
    /// Request correlation identifier.
    #[serde(rename = "rpcId")]
    pub rpc_id: RpcId,
    /// Method-specific business payload.
    pub payload: T,
}

impl<T> RpcRequest<T> {
    /// Creates one narrow request.
    #[must_use]
    pub fn new(rpc_id: RpcId, payload: T) -> Self {
        Self { rpc_id, payload }
    }
}

/// Narrow domain response: the carrier restores the `server-response` type.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RpcResponse<T = Value> {
    /// Echoed request correlation identifier.
    #[serde(rename = "rpcId")]
    pub rpc_id: RpcId,
    /// Business success or failure.
    pub result: RpcResult<T>,
}

impl<T> RpcResponse<T> {
    /// Creates one narrow response.
    #[must_use]
    pub fn new(rpc_id: RpcId, result: RpcResult<T>) -> Self {
        Self { rpc_id, result }
    }
}

/// A problem produced while parsing an API Proxy contract value.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{path}: {message}")]
pub struct ContractError {
    path: String,
    message: String,
}

impl ContractError {
    /// Creates a validation problem at one JSON-style path.
    #[must_use]
    pub fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }

    /// JSON-style location of the invalid member.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Human-readable validation failure.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// Message initiated by the Host and carried on one downstream stream.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServerRequest {
    /// Must be `server-request` on the wire.
    #[serde(rename = "type")]
    pub kind: String,
    /// Host-minted correlation identifier.
    #[serde(rename = "rpcId")]
    pub rpc_id: RpcId,
    /// Method tag whose second-level schema validates `payload`.
    pub method: String,
    /// Method-specific payload.
    pub payload: Value,
}

impl ServerRequest {
    /// Creates an exact full-form Host request.
    #[must_use]
    pub fn new(rpc_id: RpcId, method: impl Into<String>, payload: Value) -> Self {
        Self {
            kind: "server-request".to_owned(),
            rpc_id,
            method: method.into(),
            payload,
        }
    }
}

/// Response sent by the Client to a Host-initiated request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClientResponse<T = Value> {
    /// Must be `client-response` on the wire.
    #[serde(rename = "type")]
    pub kind: String,
    /// Echoed Host request identifier.
    #[serde(rename = "rpcId")]
    pub rpc_id: RpcId,
    /// Business result.
    pub result: RpcResult<T>,
}

impl<T> ClientResponse<T> {
    /// Creates an exact correlated Client response.
    #[must_use]
    pub fn new(rpc_id: RpcId, result: RpcResult<T>) -> Self {
        Self {
            kind: "client-response".to_owned(),
            rpc_id,
            result,
        }
    }
}

/// Authoritative four-member logical message union.
#[derive(Clone, Debug, PartialEq)]
pub enum RpcMessage {
    /// Client-initiated request.
    ClientRequest(ClientRequest),
    /// Host response to a Client request.
    ServerResponse(ServerResponse),
    /// Host-initiated request or push.
    ServerRequest(ServerRequest),
    /// Client response to a Host request.
    ClientResponse(ClientResponse),
}

impl Serialize for RpcMessage {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::ClientRequest(value) => value.serialize(serializer),
            Self::ServerResponse(value) => value.serialize(serializer),
            Self::ServerRequest(value) => value.serialize(serializer),
            Self::ClientResponse(value) => value.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for RpcMessage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        parse_rpc_message(&value).map_err(serde::de::Error::custom)
    }
}

/// Closed rejection reasons for a response carrier receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RpcReceiptReason {
    /// No request with that correlation id remains pending.
    NotPending,
    /// The response failed its method-specific schema.
    BadResponse,
}

impl RpcReceiptReason {
    /// Exact wire literal.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotPending => "not-pending",
            Self::BadResponse => "bad-response",
        }
    }
}

impl fmt::Display for RpcReceiptReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Carrier-level receipt for a Client response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RpcReceipt {
    /// The response was delivered to its pending interaction.
    Accepted,
    /// The carrier rejected the response without making it a logical message.
    Rejected {
        /// Stable closed rejection reason.
        reason: RpcReceiptReason,
    },
}

impl Serialize for RpcReceipt {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Accepted => serde_json::json!({ "accepted": true }).serialize(serializer),
            Self::Rejected { reason } => {
                serde_json::json!({ "accepted": false, "reason": reason.as_str() })
                    .serialize(serializer)
            }
        }
    }
}

impl<'de> Deserialize<'de> for RpcReceipt {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        parse_rpc_receipt(&value).map_err(serde::de::Error::custom)
    }
}

/// Parses the `RpcId` schema. Empty strings are deliberately valid echo tokens.
///
/// # Errors
///
/// Returns an error unless `value` is a JSON string.
pub fn parse_rpc_id(value: &Value) -> Result<RpcId, ContractError> {
    value
        .as_str()
        .map(RpcId::new)
        .ok_or_else(|| ContractError::new("$", "expected string"))
}

/// Parses and normalizes the closed RPC business-error union.
///
/// # Errors
///
/// Returns an error when the discriminant, message, or code-specific details
/// do not satisfy the pinned contract.
pub fn parse_rpc_error(value: &Value) -> Result<RpcError, ContractError> {
    let object = object(value, "$")?;
    let code = string_field(object, "code", "$.code")?;
    let message = string_field(object, "message", "$.message")?;
    let details = object_field(object, "details", "$.details")?;
    let normalized = normalize_error_details(code, details)?;
    Ok(RpcError {
        code: code.to_owned(),
        message: message.to_owned(),
        details: normalized,
    })
}

/// Parses a business result using the supplied second-level success-value parser.
///
/// Unlike a full-form wire response, this generic schema requires the `value`
/// member on success, exactly like source `rpcResultSchema(valueSchema)`.
///
/// # Errors
///
/// Returns an error when the result envelope is malformed, its error branch is
/// invalid, or `parse_value` rejects the success value.
pub fn parse_rpc_result<T>(
    value: &Value,
    parse_value: impl FnOnce(&Value) -> Result<T, ContractError>,
) -> Result<RpcResult<T>, ContractError> {
    let object = object(value, "$")?;
    if bool_field(object, "ok", "$.ok")? {
        let value = object
            .get("value")
            .ok_or_else(|| ContractError::new("$.value", "required property is missing"))?;
        Ok(RpcResult::Success {
            value: Some(parse_value(value)?),
        })
    } else {
        let error = object
            .get("error")
            .ok_or_else(|| ContractError::new("$.error", "required property is missing"))?;
        Ok(RpcResult::Failure {
            error: parse_rpc_error(error)?,
        })
    }
}

/// Parses the first-level Client request schema; business payload stays wide.
///
/// # Errors
///
/// Returns an error when the full-form envelope is malformed.
pub fn parse_client_request(value: &Value) -> Result<ClientRequest, ContractError> {
    let object = quadrant(value, "client-request")?;
    Ok(ClientRequest::new(
        parse_rpc_id(field(object, "rpcId", "$.rpcId")?)?,
        string_field(object, "method", "$.method")?,
        field(object, "payload", "$.payload")?.clone(),
    ))
}

/// Parses the first-level Host response schema; a void success may omit value.
///
/// # Errors
///
/// Returns an error when the full-form envelope or business-error branch is malformed.
pub fn parse_server_response(value: &Value) -> Result<ServerResponse, ContractError> {
    let object = quadrant(value, "server-response")?;
    Ok(ServerResponse::new(
        parse_rpc_id(field(object, "rpcId", "$.rpcId")?)?,
        parse_wire_result(field(object, "result", "$.result")?)?,
    ))
}

/// Parses the first-level Host request schema; business payload stays wide.
///
/// # Errors
///
/// Returns an error when the full-form envelope is malformed.
pub fn parse_server_request(value: &Value) -> Result<ServerRequest, ContractError> {
    let object = quadrant(value, "server-request")?;
    Ok(ServerRequest::new(
        parse_rpc_id(field(object, "rpcId", "$.rpcId")?)?,
        string_field(object, "method", "$.method")?,
        field(object, "payload", "$.payload")?.clone(),
    ))
}

/// Parses the first-level Client response schema; a void success may omit value.
///
/// # Errors
///
/// Returns an error when the full-form envelope or business-error branch is malformed.
pub fn parse_client_response(value: &Value) -> Result<ClientResponse, ContractError> {
    let object = quadrant(value, "client-response")?;
    Ok(ClientResponse::new(
        parse_rpc_id(field(object, "rpcId", "$.rpcId")?)?,
        parse_wire_result(field(object, "result", "$.result")?)?,
    ))
}

/// Parses the authoritative wire union by its exact `type` discriminant.
///
/// # Errors
///
/// Returns an error for an unknown quadrant or a malformed quadrant envelope.
pub fn parse_rpc_message(value: &Value) -> Result<RpcMessage, ContractError> {
    let object = object(value, "$")?;
    match string_field(object, "type", "$.type")? {
        "client-request" => parse_client_request(value).map(RpcMessage::ClientRequest),
        "server-response" => parse_server_response(value).map(RpcMessage::ServerResponse),
        "server-request" => parse_server_request(value).map(RpcMessage::ServerRequest),
        "client-response" => parse_client_response(value).map(RpcMessage::ClientResponse),
        _ => Err(ContractError::new("$.type", "unknown RPC message type")),
    }
}

/// Parses the carrier receipt with its exact reason set.
///
/// # Errors
///
/// Returns an error when the receipt is malformed or has an unknown rejection reason.
pub fn parse_rpc_receipt(value: &Value) -> Result<RpcReceipt, ContractError> {
    let object = object(value, "$")?;
    if bool_field(object, "accepted", "$.accepted")? {
        return Ok(RpcReceipt::Accepted);
    }
    let reason = match string_field(object, "reason", "$.reason")? {
        "not-pending" => RpcReceiptReason::NotPending,
        "bad-response" => RpcReceiptReason::BadResponse,
        _ => return Err(ContractError::new("$.reason", "unknown receipt reason")),
    };
    Ok(RpcReceipt::Rejected { reason })
}

fn parse_wire_result(value: &Value) -> Result<RpcResult<Value>, ContractError> {
    let object = object(value, "$.result")?;
    if bool_field(object, "ok", "$.result.ok")? {
        Ok(RpcResult::Success {
            value: object.get("value").cloned(),
        })
    } else {
        let error = object
            .get("error")
            .ok_or_else(|| ContractError::new("$.result.error", "required property is missing"))?;
        Ok(RpcResult::Failure {
            error: parse_rpc_error(error)?,
        })
    }
}

fn quadrant<'a>(value: &'a Value, expected: &str) -> Result<&'a Map<String, Value>, ContractError> {
    let object = object(value, "$")?;
    let actual = string_field(object, "type", "$.type")?;
    if actual == expected {
        Ok(object)
    } else {
        Err(ContractError::new("$.type", format!("expected {expected}")))
    }
}

fn normalize_error_details(
    code: &str,
    details: &Map<String, Value>,
) -> Result<Map<String, Value>, ContractError> {
    let strings: &[&str] = match code {
        "cancelled" | "command-error" | "unknown-command" | "internal" => &[],
        "session-not-found" | "title-invalid" | "fork-unavailable" => &["sessionId"],
        "model-unavailable" => &["provider", "model"],
        "session-conflict" => &["sessionId", "requestedCwd"],
        "invalid-time-zone" => &["value"],
        "workspace-attach-failed" => &["sessionId", "workspaceId"],
        "workspace-not-found" | "workspace-move-invalid" => &["workspaceId"],
        "workspace-invalid-path"
        | "directory-unreadable"
        | "directory-exists"
        | "directory-create-failed" => &["path"],
        "workspace-name-conflict" => &["name"],
        "directory-picker-unavailable" => &["capability"],
        "agent-preset-read-only" | "agent-preset-invalid" => &["agentPreset", "reason"],
        "agent-preset-locked" => &["sessionId", "agentPreset"],
        "agent-preset-conflict" => &["sessionId", "requestedPreset"],
        "agent-preset-not-found" => &["agentPreset"],
        "agent-busy" | "attachment-error" => &["reason"],
        "queue-item-not-found" | "steer-unavailable" => &["itemId"],
        "settings-rejected" | "settings-not-exposed" | "settings-conflict" => &["ns"],
        "credential-rejected" => &["ref"],
        "model-discovery-failed" => &["settingsNs"],
        "subagent-parent-unavailable" => &["parentSessionId"],
        "subagent-not-found" | "subagent-catalog-diagnostic" => {
            &["parentSessionId", "childSessionId"]
        }
        "subagent-not-resumable" | "subagent-unauthorized" | "subagent-delivery-unavailable" => {
            &["childSessionId"]
        }
        "bad-request" => {
            let issues = details.get("issues").ok_or_else(|| {
                ContractError::new("$.details.issues", "required property is missing")
            })?;
            if !issues.is_array() {
                return Err(ContractError::new("$.details.issues", "expected array"));
            }
            return Ok(Map::from_iter([("issues".to_owned(), issues.clone())]));
        }
        _ => return Err(ContractError::new("$.code", "unknown RPC error code")),
    };

    let mut normalized = Map::new();
    for name in strings {
        let value = details.get(*name).ok_or_else(|| {
            ContractError::new(format!("$.details.{name}"), "required property is missing")
        })?;
        if !value.is_string() {
            return Err(ContractError::new(
                format!("$.details.{name}"),
                "expected string",
            ));
        }
        normalized.insert((*name).to_owned(), value.clone());
    }

    match code {
        "session-conflict" => copy_optional_string(details, &mut normalized, "existingCwd")?,
        "workspace-move-invalid" => {
            copy_required_string(details, &mut normalized, "sessionId")?;
            copy_optional_string(details, &mut normalized, "beforeSessionId")?;
        }
        "agent-preset-conflict" => {
            copy_optional_string(details, &mut normalized, "existingPreset")?;
        }
        "agent-preset-not-found" => {
            let available = details.get("available").ok_or_else(|| {
                ContractError::new("$.details.available", "required property is missing")
            })?;
            let array = available
                .as_array()
                .ok_or_else(|| ContractError::new("$.details.available", "expected array"))?;
            if !array.iter().all(Value::is_string) {
                return Err(ContractError::new(
                    "$.details.available",
                    "expected string array",
                ));
            }
            normalized.insert("available".to_owned(), available.clone());
        }
        "settings-conflict" => {
            copy_required_number(details, &mut normalized, "expected")?;
            copy_required_number(details, &mut normalized, "actual")?;
        }
        "model-discovery-failed" => {
            copy_optional_string(details, &mut normalized, "baseURL")?;
        }
        "subagent-catalog-diagnostic" => {
            let reason = string_field(details, "reason", "$.details.reason")?;
            if !matches!(reason, "corrupt" | "unsupported" | "unavailable") {
                return Err(ContractError::new(
                    "$.details.reason",
                    "unknown catalog diagnostic reason",
                ));
            }
            normalized.insert("reason".to_owned(), Value::String(reason.to_owned()));
        }
        _ => {}
    }
    Ok(normalized)
}

fn copy_required_string(
    source: &Map<String, Value>,
    target: &mut Map<String, Value>,
    name: &str,
) -> Result<(), ContractError> {
    let value = source.get(name).ok_or_else(|| {
        ContractError::new(format!("$.details.{name}"), "required property is missing")
    })?;
    if !value.is_string() {
        return Err(ContractError::new(
            format!("$.details.{name}"),
            "expected string",
        ));
    }
    target.insert(name.to_owned(), value.clone());
    Ok(())
}

fn copy_optional_string(
    source: &Map<String, Value>,
    target: &mut Map<String, Value>,
    name: &str,
) -> Result<(), ContractError> {
    if let Some(value) = source.get(name) {
        if !value.is_string() {
            return Err(ContractError::new(
                format!("$.details.{name}"),
                "expected string",
            ));
        }
        target.insert(name.to_owned(), value.clone());
    }
    Ok(())
}

fn copy_required_number(
    source: &Map<String, Value>,
    target: &mut Map<String, Value>,
    name: &str,
) -> Result<(), ContractError> {
    let value = source.get(name).ok_or_else(|| {
        ContractError::new(format!("$.details.{name}"), "required property is missing")
    })?;
    if !value.is_number() {
        return Err(ContractError::new(
            format!("$.details.{name}"),
            "expected number",
        ));
    }
    target.insert(name.to_owned(), value.clone());
    Ok(())
}

fn object<'a>(value: &'a Value, path: &str) -> Result<&'a Map<String, Value>, ContractError> {
    value
        .as_object()
        .ok_or_else(|| ContractError::new(path, "expected object"))
}

fn field<'a>(
    object: &'a Map<String, Value>,
    name: &str,
    path: &str,
) -> Result<&'a Value, ContractError> {
    object
        .get(name)
        .ok_or_else(|| ContractError::new(path, "required property is missing"))
}

fn string_field<'a>(
    object: &'a Map<String, Value>,
    name: &str,
    path: &str,
) -> Result<&'a str, ContractError> {
    field(object, name, path)?
        .as_str()
        .ok_or_else(|| ContractError::new(path, "expected string"))
}

fn bool_field(object: &Map<String, Value>, name: &str, path: &str) -> Result<bool, ContractError> {
    field(object, name, path)?
        .as_bool()
        .ok_or_else(|| ContractError::new(path, "expected boolean"))
}

fn object_field<'a>(
    fields: &'a Map<String, Value>,
    name: &str,
    path: &str,
) -> Result<&'a Map<String, Value>, ContractError> {
    object(field(fields, name, path)?, path)
}
