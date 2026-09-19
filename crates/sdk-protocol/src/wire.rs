//! JSON-RPC envelopes preserve raw application values through serialization.

use std::fmt;

use anyhow::Context as _;
use seekdeep_lossless_json::{JsonRef, JsonString, JsonValue};
use serde_json::{Map, Value};

use super::JsonRpcResponseError;

/// JSON-RPC failure with exact JavaScript message code units and raw error data.
/// Its diagnostic display uses a quoted JSON string when UTF-8 cannot represent the message.
#[derive(Clone, Debug)]
pub struct JsonRpcRawResponseError {
    /// Numeric JSON-RPC code, when the peer supplied an integer.
    pub code: Option<i64>,
    /// Peer message or the stable fallback, without surrogate replacement.
    pub message: JsonString,
    /// Optional structured error payload, retaining every JSON string and key.
    pub data: Option<JsonValue>,
}

impl fmt::Display for JsonRpcRawResponseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            self.message
                .as_str()
                .unwrap_or_else(|| self.message.as_raw()),
        )
    }
}

impl std::error::Error for JsonRpcRawResponseError {}

impl JsonRpcRawResponseError {
    /// Converts a peer failure to the ordinary Unicode-scalar API.
    ///
    /// # Errors
    /// Rejects unrepresentable message or data values, retaining this raw error as the cause.
    pub fn try_into_legacy(self) -> anyhow::Result<JsonRpcResponseError> {
        let Some(message) = self.message.as_str().map(str::to_owned) else {
            return Err(anyhow::Error::new(self).context(
                "JSON-RPC error message cannot be represented by the ordinary UTF-8 API",
            ));
        };
        let data = match self
            .data
            .as_ref()
            .map(|data| data.clone().try_into_serde_json())
            .transpose()
        {
            Ok(data) => data,
            Err(error) => {
                return Err(anyhow::Error::new(self).context(format!(
                    "JSON-RPC error data cannot be represented as serde_json::Value: {error}"
                )));
            }
        };
        Ok(JsonRpcResponseError {
            code: self.code,
            message,
            data,
        })
    }
}

impl TryFrom<JsonRpcRawResponseError> for JsonRpcResponseError {
    type Error = anyhow::Error;

    fn try_from(error: JsonRpcRawResponseError) -> Result<Self, Self::Error> {
        error.try_into_legacy()
    }
}

pub(super) fn ordinary_response(response: anyhow::Result<JsonValue>) -> anyhow::Result<Value> {
    let value = response.map_err(|error| match error.downcast::<JsonRpcRawResponseError>() {
        Ok(raw) => raw
            .try_into_legacy()
            .map_or_else(std::convert::identity, anyhow::Error::new),
        Err(error) => error,
    })?;
    value
        .try_into_serde_json()
        .context("JSON-RPC result cannot be represented as serde_json::Value")
}

pub(super) fn ordinary_params(params: JsonValue) -> anyhow::Result<Map<String, Value>> {
    let params = params
        .try_into_serde_json()
        .context("JSON-RPC parameters cannot be represented as serde_json::Value")?;
    Ok(match params {
        Value::Object(params) => params,
        _ => Map::new(),
    })
}

pub(super) fn object_params(params: Option<JsonRef<'_>>) -> JsonValue {
    params.filter(|params| params.is_object()).map_or_else(
        || JsonValue::from(Value::Object(Map::new())),
        JsonRef::to_owned,
    )
}

pub(super) fn valid_id(id: JsonRef<'_>) -> bool {
    id.is_string() || id.as_f64().is_some()
}

fn string(value: &str) -> JsonValue {
    JsonValue::from(Value::String(value.to_owned()))
}

pub(super) fn request(id: &str, method: &str, params: JsonValue) -> JsonValue {
    JsonValue::object([
        ("jsonrpc", string("2.0")),
        ("id", string(id)),
        ("method", string(method)),
        ("params", params),
    ])
}

pub(super) fn notification(method: &str, params: Option<JsonValue>) -> JsonValue {
    let mut fields = vec![("jsonrpc", string("2.0")), ("method", string(method))];
    if let Some(params) = params {
        fields.push(("params", params));
    }
    JsonValue::object(fields)
}

pub(super) fn response(id: JsonValue, response: anyhow::Result<JsonValue>) -> JsonValue {
    let mut fields = vec![("jsonrpc", string("2.0")), ("id", id)];
    match response {
        Ok(value) => fields.push(("result", value)),
        Err(error) => fields.push(("error", error_value(&error))),
    }
    JsonValue::object(fields)
}

fn error_value(error: &anyhow::Error) -> JsonValue {
    if let Some(error) = error.downcast_ref::<JsonRpcRawResponseError>() {
        return structured_error(error.code, error.message.clone(), error.data.clone());
    }
    if let Some(error) = error.downcast_ref::<JsonRpcResponseError>() {
        return structured_error(
            error.code,
            error.message.clone().into(),
            error.data.clone().map(JsonValue::from),
        );
    }
    let message = error.to_string();
    let code = if message.starts_with("method not found: ") {
        -32601
    } else {
        -32603
    };
    structured_error(Some(code), message.into(), None)
}

fn structured_error(code: Option<i64>, message: JsonString, data: Option<JsonValue>) -> JsonValue {
    let mut fields = vec![
        ("code", JsonValue::from(Value::from(code.unwrap_or(-32603)))),
        ("message", JsonValue::from(message)),
    ];
    if let Some(data) = data {
        fields.push(("data", data));
    }
    JsonValue::object(fields)
}

pub(super) fn decode_response(frame: &JsonValue) -> anyhow::Result<JsonValue> {
    let Some(error) = frame.get("error").filter(|error| error.is_object()) else {
        return Ok(frame
            .get("result")
            .map_or_else(|| JsonValue::from(Value::Null), JsonRef::to_owned));
    };
    let message = error
        .get("message")
        .filter(|message| message.is_string())
        .map(JsonRef::deserialize::<JsonString>)
        .transpose()?
        .unwrap_or_else(|| "JSON-RPC error".into());
    Err(JsonRpcRawResponseError {
        code: error.get("code").and_then(JsonRef::as_i64),
        message,
        data: error.get("data").map(JsonRef::to_owned),
    }
    .into())
}
