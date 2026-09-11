//! Iterative JSON transfer across the guarded JavaScript realm.

use std::collections::HashSet;

use boa_engine::{
    Context, JsObject, JsString, JsValue, JsVariant, js_string, object::builtins::JsArray,
    property::PropertyKey,
};
use seekdeep_code_runtime_worker_thread::{CodeJsonString, CodeJsonToken, CodeJsonValue};

enum InputFrame {
    Array(Vec<JsValue>),
    Object {
        value: JsObject,
        key: Option<JsString>,
    },
}

pub(super) fn to_javascript(
    value: &CodeJsonValue,
    context: &mut Context,
) -> Result<JsValue, String> {
    let mut frames = Vec::<InputFrame>::new();
    let mut root = None;
    for token in value.tokens() {
        let child = match token {
            CodeJsonToken::ArrayStart => {
                frames.push(InputFrame::Array(Vec::new()));
                continue;
            }
            CodeJsonToken::ObjectStart => {
                frames.push(InputFrame::Object {
                    value: JsObject::with_object_proto(context.intrinsics()),
                    key: None,
                });
                continue;
            }
            CodeJsonToken::Key(key) => {
                let Some(InputFrame::Object { key: slot, .. }) = frames.last_mut() else {
                    return Err("JSON object key has no containing object".to_owned());
                };
                *slot = Some(JsString::from(
                    key.to_utf16().expect("JSON key is a string").as_slice(),
                ));
                continue;
            }
            CodeJsonToken::Scalar(value) => {
                if value.is_null() {
                    JsValue::null()
                } else if let Some(value) = value.as_bool() {
                    JsValue::from(value)
                } else if let Some(units) = value.to_utf16() {
                    JsValue::from(JsString::from(units.as_slice()))
                } else {
                    JsValue::from(
                        value
                            .as_f64()
                            .ok_or_else(|| "invalid JSON number".to_owned())?,
                    )
                }
            }
            CodeJsonToken::ArrayEnd => {
                let Some(InputFrame::Array(items)) = frames.pop() else {
                    return Err("JSON array closure has no containing array".to_owned());
                };
                JsArray::from_iter(items, context).into()
            }
            CodeJsonToken::ObjectEnd => {
                let Some(InputFrame::Object { value, .. }) = frames.pop() else {
                    return Err("JSON object closure has no containing object".to_owned());
                };
                value.into()
            }
        };
        match frames.last_mut() {
            Some(InputFrame::Array(items)) => items.push(child),
            Some(InputFrame::Object { value, key }) => {
                let key = key
                    .take()
                    .ok_or_else(|| "JSON object value has no key".to_owned())?;
                value
                    .create_data_property_or_throw(key, child, context)
                    .map_err(|error| error.to_string())?;
            }
            None => root = Some(child),
        }
    }
    root.ok_or_else(|| "JSON transfer produced no value".to_owned())
}

enum OutputTask {
    Value(JsValue),
    Key(CodeJsonString),
    Punctuation(char),
    Leave(JsObject),
}

pub(super) fn from_javascript(
    root: JsValue,
    context: &mut Context,
) -> Result<CodeJsonValue, String> {
    let mut tasks = vec![OutputTask::Value(root)];
    let mut ancestors = HashSet::new();
    let mut raw = String::new();
    while let Some(task) = tasks.pop() {
        match task {
            OutputTask::Key(key) => raw.push_str(key.as_raw()),
            OutputTask::Punctuation(value) => raw.push(value),
            OutputTask::Leave(object) => {
                ancestors.remove(&object);
            }
            OutputTask::Value(value) => match value.variant() {
                JsVariant::Null => raw.push_str("null"),
                JsVariant::Boolean(value) => raw.push_str(if value { "true" } else { "false" }),
                JsVariant::String(value) => {
                    raw.push_str(CodeJsonString::from_utf16(&value.to_vec()).as_raw());
                }
                JsVariant::Integer32(value) => raw.push_str(&value.to_string()),
                JsVariant::Float64(value) => {
                    if value == 0.0 && value.is_sign_negative() {
                        return Err("JavaScript negative zero is not lossless JSON".to_owned());
                    }
                    if !value.is_finite() {
                        return Err("JavaScript number is not finite JSON".to_owned());
                    }
                    let text = JsValue::from(value)
                        .to_string(context)
                        .map_err(|error| error.to_string())?;
                    raw.push_str(&text.to_std_string_escaped());
                }
                JsVariant::Undefined | JsVariant::BigInt(_) | JsVariant::Symbol(_) => {
                    return Err("JavaScript value is not lossless JSON".to_owned());
                }
                JsVariant::Object(object) => {
                    if !ancestors.insert(object.clone()) {
                        return Err("JavaScript value is circular".to_owned());
                    }
                    tasks.push(OutputTask::Leave(object.clone()));
                    if object.is_array() {
                        let length = object
                            .get(js_string!("length"), context)
                            .map_err(|error| error.to_string())?
                            .to_length(context)
                            .map_err(|error| error.to_string())?;
                        raw.push('[');
                        tasks.push(OutputTask::Punctuation(']'));
                        for index in (0..length).rev() {
                            let value = object
                                .get(PropertyKey::from(index), context)
                                .map_err(|error| error.to_string())?;
                            tasks.push(OutputTask::Value(value));
                            if index != 0 {
                                tasks.push(OutputTask::Punctuation(','));
                            }
                        }
                    } else {
                        let mut entries = Vec::new();
                        for key in object
                            .own_property_keys(context)
                            .map_err(|error| error.to_string())?
                        {
                            let name = match &key {
                                PropertyKey::String(value) => {
                                    CodeJsonString::from_utf16(&value.to_vec())
                                }
                                PropertyKey::Index(value) => value.get().to_string().into(),
                                PropertyKey::Symbol(_) => {
                                    return Err(
                                        "JavaScript symbol key is not lossless JSON".to_owned()
                                    );
                                }
                            };
                            let value = object
                                .get(key, context)
                                .map_err(|error| error.to_string())?;
                            entries.push((name, value));
                        }
                        raw.push('{');
                        tasks.push(OutputTask::Punctuation('}'));
                        for (index, (key, value)) in entries.into_iter().enumerate().rev() {
                            tasks.push(OutputTask::Value(value));
                            tasks.push(OutputTask::Punctuation(':'));
                            tasks.push(OutputTask::Key(key));
                            if index != 0 {
                                tasks.push(OutputTask::Punctuation(','));
                            }
                        }
                    }
                }
            },
        }
    }
    CodeJsonValue::parse(raw).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use boa_engine::Source;

    use super::*;

    #[test]
    fn exact_code_units_and_own_proto_keys_survive_both_directions() {
        let mut context = Context::default();
        let input = CodeJsonValue::parse(
            r#"{"__proto__":{"\ud800":"\udfff"},"payload":["😀","\\ud800","\ud800"]}"#.to_owned(),
        )
        .unwrap();
        let value = to_javascript(&input, &mut context).unwrap();
        let round_trip = from_javascript(value, &mut context).unwrap();
        assert_eq!(round_trip, input);
        let returned = context
            .eval(Source::from_bytes(
                r"({['\udfff']: '\ud800', literal: '\\ud800'})",
            ))
            .unwrap();
        assert_eq!(
            from_javascript(returned, &mut context).unwrap(),
            CodeJsonValue::parse(r#"{"\udfff":"\ud800","literal":"\\ud800"}"#.to_owned()).unwrap()
        );
    }

    #[test]
    fn cycles_and_non_json_values_are_rejected() {
        let mut context = Context::default();
        for source in [
            "undefined",
            "1n",
            "Symbol('x')",
            "NaN",
            "Infinity",
            "-0",
            "(() => { const a={}; a.self=a; return a; })()",
        ] {
            let value = context.eval(Source::from_bytes(source)).unwrap();
            assert!(from_javascript(value, &mut context).is_err(), "{source}");
        }
    }
}
