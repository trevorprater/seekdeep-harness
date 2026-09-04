//! Typed native operations; callbacks expose only interpreter primitives and package API calls.

use std::path::{Path, PathBuf};

use seekdeep_python_sdk::{Error, ErrorKind, Result, runtime};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{Callback, Reply};

#[derive(Deserialize)]
#[serde(tag = "op")]
enum Operation {
    #[serde(rename = "api.normalize")]
    Normalize {
        value: seekdeep_python_sdk::ObjectHandle,
    },
    #[serde(rename = "api.final_response")]
    FinalResponse {
        events: Vec<serde_json::Map<String, Value>>,
    },
    #[serde(rename = "api.final_response_object")]
    FinalResponseObject {
        events: seekdeep_python_sdk::ObjectHandle,
    },
    #[serde(rename = "api.finish_reason")]
    FinishReason {
        events: Vec<serde_json::Map<String, Value>>,
    },
    #[serde(rename = "api.finish_reason_object")]
    FinishReasonObject {
        events: seekdeep_python_sdk::ObjectHandle,
    },
    #[serde(rename = "api.inbox_receipt")]
    InboxReceipt {
        notification: seekdeep_python_sdk::NotificationData,
        session: seekdeep_python_sdk::SessionId,
        message: seekdeep_python_sdk::MessageId,
    },
    #[serde(rename = "api.int_or_none")]
    IntOrNone {
        value: seekdeep_python_sdk::ObjectHandle,
    },
    #[serde(rename = "notification.create")]
    CreateNotification {
        value: seekdeep_python_sdk::NotificationData,
    },
    #[serde(rename = "about")]
    About,
    #[serde(rename = "runtime.platform")]
    Platform { platform: String, machine: String },
    #[serde(rename = "runtime.package")]
    Package { module: String },
    #[serde(rename = "runtime.default_config")]
    DefaultConfig,
    #[serde(rename = "runtime.executable")]
    Executable,
    #[serde(rename = "runtime.node")]
    Node,
    #[serde(rename = "runtime.resolve")]
    Resolve { mode: ObjectId },
}

#[derive(Clone, Copy, Deserialize)]
#[serde(transparent)]
struct ObjectId(u64);

// Keep the complete runtime and value-operation ABI table beside its closed request enum.
#[allow(clippy::too_many_lines)]
pub(crate) fn run(bytes: &[u8], callback: Callback) -> Result<Reply> {
    let input: Value = serde_json::from_slice(bytes)
        .map_err(|error| Error::new(ErrorKind::Value, error.to_string()))?;
    if input
        .get("op")
        .and_then(Value::as_str)
        .is_some_and(crate::client::handles)
    {
        return crate::client::run(bytes, callback);
    }
    let operation: Operation = serde_json::from_slice(bytes)
        .map_err(|error| Error::new(ErrorKind::Value, error.to_string()))?;
    match operation {
        Operation::Normalize { value } => {
            let callback = callback.with_owner(value.owner);
            let value = crate::objects::Object::new(callback, json!(value))?;
            let result = if value.invoke("object.is_string", Value::Null)?.as_bool() == Some(true) {
                crate::objects::Object::new(
                    callback,
                    value.invoke("object.text_block", Value::Null)?,
                )?
            } else {
                value
            };
            return Ok(Reply::object(result.handle(), result));
        }
        Operation::FinalResponse { events } => {
            Ok(json!(seekdeep_python_sdk::final_response(&events)))
        }
        Operation::FinalResponseObject { events } => {
            return crate::projection::final_response(callback, events);
        }
        Operation::FinishReason { events } => {
            Ok(json!(seekdeep_python_sdk::finish_reason(&events)?))
        }
        Operation::FinishReasonObject { events } => {
            return crate::projection::finish_reason(callback, events);
        }
        Operation::InboxReceipt {
            notification,
            session,
            message,
        } => Ok(json!(seekdeep_python_sdk::is_inbox_receipt(
            &seekdeep_python_sdk::Notification::new(notification),
            &session,
            &message,
        )?)),
        Operation::IntOrNone { value } => {
            let value =
                crate::objects::Object::new(callback.with_owner(value.owner), json!(value))?;
            if value.invoke("object.is_integer", Value::Null)?.as_bool() == Some(true) {
                return Ok(Reply::object(value.handle(), value));
            }
            Ok(Value::Null)
        }
        Operation::CreateNotification { value } => {
            let notification = crate::objects::notification(callback, value)?;
            let handle = notification.object_handle().ok_or_else(|| {
                Error::new(ErrorKind::Type, "foreign notification has no object handle")
            })?;
            return Ok(Reply::object(handle, notification));
        }
        Operation::About => {
            Ok(json!({"abiVersion":crate::ABI_VERSION,"version":env!("CARGO_PKG_VERSION")}))
        }
        Operation::Platform { platform, machine } => {
            Ok(json!(runtime::platform_tag(&platform, &machine)?))
        }
        Operation::Package { module } => {
            let module = Path::new(&module);
            let cwd = if module.is_absolute() {
                PathBuf::new()
            } else {
                PathBuf::from(callback_string(callback, "getcwd", json!([]))?)
            };
            Ok(json!(runtime::bundled_package_dir(module, &cwd)?))
        }
        Operation::DefaultConfig => {
            let root = callback_string(callback, "runtime.package", json!([]))?;
            Ok(json!(runtime::bundled_default_config_path(Path::new(
                &root
            ))?))
        }
        Operation::Executable => {
            let tag = callback_string(callback, "runtime.platform_tag", json!([]))?;
            let root = callback_string(callback, "runtime.package", json!([]))?;
            Ok(json!(runtime::bundled_runtime_path(
                Path::new(&root),
                &tag
            )?))
        }
        Operation::Node => {
            let root = callback_string(callback, "runtime.package", json!([]))?;
            let argv = runtime::node_launch_args(Path::new(&root), || {
                let value = callback.invoke("runtime.find_node", json!([]))?;
                if value.is_null() {
                    Ok(None)
                } else {
                    string(value).map(Some)
                }
            })?;
            Ok(json!(argv))
        }
        Operation::Resolve { mode } => {
            let selected = runtime::selected_mode_with(
                mode,
                || {
                    serde_json::from_value(
                        callback
                            .invoke("environment_object", json!([runtime::RUNTIME_MODE_ENV_VAR]))?,
                    )
                    .map_err(|error| Error::new(ErrorKind::Type, error.to_string()))
                },
                |value| boolean(&callback.invoke("is_none", json!([value.0]))?),
                |value, expected| boolean(&callback.invoke("equals", json!([value.0, expected]))?),
                |value| callback_string(callback, "representation", json!([value.0])),
            )?;
            match selected {
                runtime::RuntimeMode::Exe => Ok(json!([callback_string(
                    callback,
                    "runtime.executable",
                    json!([])
                )?])),
                runtime::RuntimeMode::Node => callback.invoke("runtime.node", json!([])),
            }
        }
    }
    .map(Reply::json)
}

fn callback_string(callback: Callback, operation: &str, arguments: Value) -> Result<String> {
    string(callback.invoke(operation, arguments)?)
}

fn string(value: Value) -> Result<String> {
    match value {
        Value::String(value) => Ok(value),
        _ => Err(Error::new(
            ErrorKind::Type,
            "interpreter callback must return a string",
        )),
    }
}

fn boolean(value: &Value) -> Result<bool> {
    match value {
        Value::Bool(value) => Ok(*value),
        _ => Err(Error::new(
            ErrorKind::Type,
            "interpreter callback must return a boolean",
        )),
    }
}
