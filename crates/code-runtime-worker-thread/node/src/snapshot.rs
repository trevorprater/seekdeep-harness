//! Iterative source-compatible snapshots at the Node message boundary.

use js_sys::Array;
use serde::Serialize;
use serde_json::{Value, json};
use wasm_bindgen::prelude::*;

use crate::{CodeJsonValue, bridge, worker_json::decode_code_json};

#[derive(Clone)]
pub(crate) struct Intrinsics {
    object_prototype: JsValue,
    array_prototype: JsValue,
}

impl Intrinsics {
    pub(crate) fn capture() -> Result<Self, JsValue> {
        let object = bridge::parse("{}")?;
        let array = bridge::array(&[])?;
        Ok(Self {
            object_prototype: bridge::prototype(&object)?,
            array_prototype: bridge::prototype(&array)?,
        })
    }

    fn intrinsic_constructor(prototype: &JsValue, name: &str) -> bool {
        let Ok(descriptor) = bridge::descriptor(prototype, &JsValue::from_str("constructor"))
        else {
            return false;
        };
        let Ok(constructor) = bridge::get(&descriptor, "value") else {
            return false;
        };
        constructor.is_function()
            && bridge::get(&constructor, "name")
                .ok()
                .and_then(|value| value.as_string())
                .as_deref()
                == Some(name)
            && bridge::get(&constructor, "prototype").is_ok_and(|value| value == *prototype)
            && bridge::function_source(&constructor)
                .is_ok_and(|value| value == format!("function {name}() {{ [native code] }}"))
    }

    fn foreign_object_prototype(prototype: &JsValue) -> bool {
        bridge::prototype(prototype).is_ok_and(|value| value.is_null())
            && Self::intrinsic_constructor(prototype, "Object")
    }

    fn plain_array(&self, value: &JsValue) -> bool {
        let Ok(prototype) = bridge::prototype(value) else {
            return false;
        };
        if prototype == self.array_prototype {
            return true;
        }
        if !bridge::is_array(&prototype) || !Self::intrinsic_constructor(&prototype, "Array") {
            return false;
        }
        bridge::prototype(&prototype).is_ok_and(|value| {
            value.is_object() && !value.is_null() && Self::foreign_object_prototype(&value)
        })
    }

    fn plain_object(&self, value: &JsValue) -> bool {
        let Ok(prototype) = bridge::prototype(value) else {
            return false;
        };
        prototype.is_null()
            || prototype == self.object_prototype
            || prototype.is_object() && Self::foreign_object_prototype(&prototype)
    }
}

enum Task {
    Visit(JsValue),
    ArrayItem(JsValue, u32),
    ObjectProperty(JsValue, JsValue),
    Leave(JsValue),
}

pub(crate) struct Snapshot {
    pub(crate) wire: CodeJsonValue,
    pub(crate) bytes: usize,
}

#[derive(Serialize)]
struct ObjectMarker<'a> {
    kind: &'static str,
    keys: &'a [CodeJsonValue],
}

struct ObjectSnapshot {
    marker: CodeJsonValue,
    properties: Vec<JsValue>,
    bytes: usize,
}

fn object_snapshot(value: &JsValue, keys: &Array) -> Option<ObjectSnapshot> {
    let mut labels = Vec::with_capacity(keys.length() as usize);
    let mut properties = Vec::with_capacity(keys.length() as usize);
    let mut bytes = 0usize;
    for index in 0..keys.length() {
        let key = keys.get(index);
        if !key.is_string() {
            return None;
        }
        let encoded = bridge::stringify(&key).ok()?;
        let descriptor = bridge::descriptor(value, &key).ok()?;
        if bridge::get(&descriptor, "enumerable").ok()?.as_bool() != Some(true) {
            return None;
        }
        bytes = bytes.checked_add(encoded.len().checked_add(1)?)?;
        labels.push(CodeJsonValue::parse(encoded).ok()?);
        properties.push(key);
    }
    bytes = bytes.checked_add(2usize.checked_add(labels.len().saturating_sub(1))?)?;
    let marker = CodeJsonValue::parse(
        serde_json::to_string(&ObjectMarker {
            kind: "object",
            keys: &labels,
        })
        .ok()?,
    )
    .ok()?;
    Some(ObjectSnapshot {
        marker,
        properties,
        bytes,
    })
}

/// Reads each accepted application property once, retaining no application handles.
pub(crate) fn snapshot(value: &JsValue, intrinsics: &Intrinsics) -> Option<Snapshot> {
    let active = bridge::new_set();
    let mut tasks = vec![Task::Visit(value.clone())];
    let mut wire = Vec::new();
    let mut bytes = 0usize;
    while let Some(task) = tasks.pop() {
        match task {
            Task::Leave(value) => bridge::set_delete(&active, &value),
            Task::ArrayItem(value, index) => {
                let key = JsValue::from_str(&index.to_string());
                if !bridge::has_own(&value, &key).ok()? {
                    return None;
                }
                tasks.push(Task::Visit(bridge::get_key(&value, &key).ok()?));
            }
            Task::ObjectProperty(value, key) => {
                tasks.push(Task::Visit(bridge::get_key(&value, &key).ok()?));
            }
            Task::Visit(value) => {
                if value.is_null() {
                    wire.push(CodeJsonValue::from(Value::Null));
                    bytes = bytes.checked_add(4)?;
                } else if let Some(value) = value.as_bool() {
                    wire.push(CodeJsonValue::from(Value::Bool(value)));
                    bytes = bytes.checked_add(if value { 4 } else { 5 })?;
                } else if value.is_string() {
                    let encoded = bridge::stringify(&value).ok()?;
                    bytes = bytes.checked_add(encoded.len())?;
                    wire.push(CodeJsonValue::parse(encoded).ok()?);
                } else if let Some(value) = value.as_f64() {
                    if !value.is_finite() || value == 0.0 && value.is_sign_negative() {
                        return None;
                    }
                    let mut rendered = ryu_js::Buffer::new();
                    let rendered = rendered.format(value);
                    wire.push(CodeJsonValue::parse(rendered.to_owned()).ok()?);
                    bytes = bytes.checked_add(rendered.len())?;
                } else if value.is_object() && !value.is_function() {
                    if bridge::set_has(&active, &value) {
                        return None;
                    }
                    let keys = bridge::own_keys(&value).ok()?;
                    bridge::set_add(&active, &value);
                    tasks.push(Task::Leave(value.clone()));
                    if bridge::is_array(&value) {
                        if !intrinsics.plain_array(&value) {
                            return None;
                        }
                        let length = bridge::get(&value, "length").ok()?.as_f64()?;
                        #[expect(
                            clippy::cast_possible_truncation,
                            clippy::cast_sign_loss,
                            reason = "native Array.length is an unsigned 32-bit integer"
                        )]
                        let length = length as u32;
                        if keys.length() != length.checked_add(1)? {
                            return None;
                        }
                        wire.push(CodeJsonValue::from(
                            json!({"kind":"array", "length":length}),
                        ));
                        bytes = bytes
                            .checked_add(2usize.checked_add(length.saturating_sub(1) as usize)?)?;
                        for index in (0..length).rev() {
                            tasks.push(Task::ArrayItem(value.clone(), index));
                        }
                    } else {
                        if !intrinsics.plain_object(&value) {
                            return None;
                        }
                        let object = object_snapshot(&value, &keys)?;
                        bytes = bytes.checked_add(object.bytes)?;
                        wire.push(object.marker);
                        for key in object.properties.into_iter().rev() {
                            tasks.push(Task::ObjectProperty(value.clone(), key));
                        }
                    }
                } else {
                    return None;
                }
            }
        }
    }
    Some(Snapshot {
        wire: CodeJsonValue::array(&wire),
        bytes,
    })
}

/// Validates a transported wire before making a Rust JSON copy of its shallow tokens.
pub(crate) fn read_wire(value: &JsValue, intrinsics: &Intrinsics) -> Option<CodeJsonValue> {
    let snapshot = snapshot(value, intrinsics)?;
    let wire = decode_code_json(&snapshot.wire)?;
    decode_code_json(&wire)?;
    Some(wire)
}

/// Reconstructs flat validated tokens with data properties immune to prototype setters.
pub(crate) fn materialize(wire: &CodeJsonValue) -> Result<JsValue, JsValue> {
    let value = decode_code_json(wire)
        .ok_or_else(|| bridge::error("binding resolution must be lossless JSON"))?;
    bridge::parse(value.as_raw())
}
