//! Browser Tool-block narrowing and primitive-prop conversion.

use js_sys::{Array, JSON, Object, Reflect};
use serde_json::Value;
use wasm_bindgen::{JsCast as _, JsValue};

use crate::{
    DiffCardModel, ReadCardModel, SearchCard, SearchCardModel, TerminalCardModel, ToolCallBlock,
    ToolCallHead, ToolErrorInfo, ToolRowState, ToolRowVariant, WebCardModel,
    browser::{object, required_bool, required_property, required_string},
};

#[derive(Clone)]
pub(crate) struct BrowserToolBlock {
    pub(crate) model: ToolCallBlock,
    pub(crate) tool_name: String,
    pub(crate) sub_calls: Array,
}

impl BrowserToolBlock {
    pub(crate) fn parse(raw: &JsValue) -> Result<Self, JsValue> {
        let call_id = required_string(raw, "callId", "Tool call")?;
        let sub_calls = required_property(raw, "subCalls", "Tool call")?
            .dyn_into::<Array>()
            .map_err(|_| js_sys::TypeError::new("Tool call subCalls must be an array"))?;
        if Reflect::has(raw, &JsValue::from_str("kind"))? {
            let call_value = required_property(raw, "call", "settled Tool call")?;
            let (call, tool_name) = if call_value.is_null() {
                (None, String::new())
            } else {
                let args_raw = required_string(&call_value, "argsRaw", "Tool call head")?;
                let name = required_string(&call_value, "name", "Tool call head")?;
                (Some(ToolCallHead { args_raw }), name)
            };
            let content_value = required_property(raw, "content", "settled Tool call")?;
            let content = json_array(&content_value, "settled Tool call content")?;
            let error_value = Reflect::get(raw, &JsValue::from_str("error"))?;
            let error = if error_value.is_null() || error_value.is_undefined() {
                None
            } else {
                Some(ToolErrorInfo {
                    name: required_string(&error_value, "name", "Tool error")?,
                    code: required_string(&error_value, "code", "Tool error")?,
                })
            };
            Ok(Self {
                model: ToolCallBlock::Settled {
                    call_id,
                    call,
                    call_view: optional_json(raw, "callView")?,
                    result_view: optional_json(raw, "resultView")?,
                    content,
                    is_error: required_bool(raw, "isError", "settled Tool call")?,
                    error,
                },
                tool_name,
                sub_calls,
            })
        } else {
            let tool_name = required_string(raw, "name", "running Tool call")?;
            Ok(Self {
                model: ToolCallBlock::Running {
                    call_id,
                    args_raw: required_string(raw, "argsRaw", "running Tool call")?,
                    call_view: optional_json(raw, "callView")?,
                },
                tool_name,
                sub_calls,
            })
        }
    }

    pub(crate) fn raw_arguments(&self) -> &str {
        match &self.model {
            ToolCallBlock::Running { args_raw, .. } => args_raw,
            ToolCallBlock::Settled { call, .. } => {
                call.as_ref().map_or("", |call| call.args_raw.as_str())
            }
        }
    }

    pub(crate) fn error_code(&self) -> Option<&str> {
        match &self.model {
            ToolCallBlock::Settled { error, .. } => error.as_ref().map(|error| error.code.as_str()),
            ToolCallBlock::Running { .. } => None,
        }
    }

    pub(crate) fn concatenated_text(&self) -> String {
        let ToolCallBlock::Settled { content, .. } = &self.model else {
            return String::new();
        };
        content
            .iter()
            .filter_map(|block| {
                (block.get("type").and_then(Value::as_str) == Some("text"))
                    .then(|| block.get("text").and_then(Value::as_str))
                    .flatten()
            })
            .collect::<String>()
    }
}

pub(crate) const fn variant_name(variant: ToolRowVariant) -> &'static str {
    match variant {
        ToolRowVariant::Search => "search",
        ToolRowVariant::Read => "read",
        ToolRowVariant::Bash => "bash",
        ToolRowVariant::Write => "write",
        ToolRowVariant::Edit => "edit",
        ToolRowVariant::Code => "code",
        ToolRowVariant::Others => "others",
    }
}

pub(crate) const fn state_name(state: ToolRowState) -> &'static str {
    match state {
        ToolRowState::Running => "running",
        ToolRowState::Ok => "ok",
        ToolRowState::Error => "error",
        ToolRowState::Stopped => "stopped",
    }
}

pub(crate) fn terminal_card_props(model: &TerminalCardModel) -> Result<Object, JsValue> {
    object(&[
        ("command", JsValue::from_str(&model.card.command)),
        (
            "cwd",
            model
                .card
                .cwd
                .as_deref()
                .map_or(JsValue::UNDEFINED, JsValue::from_str),
        ),
        (
            "output",
            model
                .card
                .output
                .as_deref()
                .map_or(JsValue::UNDEFINED, JsValue::from_str),
        ),
        (
            "exitCode",
            model
                .card
                .exit_code
                .as_ref()
                .and_then(serde_json::Number::as_f64)
                .map_or(JsValue::UNDEFINED, JsValue::from_f64),
        ),
        (
            "signal",
            model
                .card
                .signal
                .as_deref()
                .map_or(JsValue::UNDEFINED, JsValue::from_str),
        ),
        ("running", JsValue::from_bool(model.card.running)),
    ])
}

pub(crate) fn diff_card_props(model: &DiffCardModel) -> Result<Object, JsValue> {
    let diffs = Array::new();
    for diff in &model.diffs {
        diffs.push(
            object(&[
                ("path", JsValue::from_str(&diff.path)),
                (
                    "oldText",
                    diff.old_text
                        .as_deref()
                        .map_or(JsValue::NULL, JsValue::from_str),
                ),
                ("newText", JsValue::from_str(&diff.new_text)),
            ])?
            .as_ref(),
        );
    }
    object(&[("diffs", diffs.into())])
}

pub(crate) fn read_card_props(model: &ReadCardModel) -> Result<Object, JsValue> {
    let lines = Array::new();
    for line in &model.lines {
        lines.push(
            object(&[
                ("number", wire_u64_number(line.number)),
                ("text", JsValue::from_str(&line.text)),
            ])?
            .as_ref(),
        );
    }
    object(&[
        ("label", JsValue::from_str(&model.label)),
        ("lines", lines.into()),
        ("totalLines", wire_u64_number(model.total_lines)),
        (
            "lang",
            model
                .lang
                .as_deref()
                .map_or(JsValue::UNDEFINED, JsValue::from_str),
        ),
    ])
}

pub(crate) fn search_card_props(model: &SearchCardModel) -> Result<Object, JsValue> {
    match &model.card {
        SearchCard::Matches {
            files,
            truncated,
            total,
        } => {
            let values = Array::new();
            for file in files {
                let matches = Array::new();
                for matched in &file.matches {
                    matches.push(
                        object(&[
                            (
                                "lineNumber",
                                matched
                                    .line_number
                                    .as_f64()
                                    .map_or(JsValue::UNDEFINED, JsValue::from_f64),
                            ),
                            ("line", JsValue::from_str(&matched.line)),
                        ])?
                        .as_ref(),
                    );
                }
                values.push(
                    object(&[
                        ("path", JsValue::from_str(&file.path)),
                        ("matches", matches.into()),
                    ])?
                    .as_ref(),
                );
            }
            object(&[
                ("kind", JsValue::from_str("matches")),
                ("files", values.into()),
                ("truncated", JsValue::from_bool(*truncated)),
                (
                    "total",
                    total.as_f64().map_or(JsValue::UNDEFINED, JsValue::from_f64),
                ),
            ])
        }
        SearchCard::Paths {
            paths,
            truncated,
            total,
        } => {
            let values = Array::new();
            for path in paths {
                values.push(&JsValue::from_str(path));
            }
            object(&[
                ("kind", JsValue::from_str("paths")),
                ("paths", values.into()),
                ("truncated", JsValue::from_bool(*truncated)),
                (
                    "total",
                    total.as_f64().map_or(JsValue::UNDEFINED, JsValue::from_f64),
                ),
            ])
        }
    }
}

pub(crate) fn web_card_props(model: &WebCardModel) -> Result<Object, JsValue> {
    match model {
        WebCardModel::Search {
            answer,
            sources,
            truncated,
        } => {
            let rows = Array::new();
            for source in sources {
                rows.push(
                    object(&[
                        ("url", JsValue::from_str(&source.url)),
                        (
                            "title",
                            source
                                .title
                                .as_deref()
                                .map_or(JsValue::UNDEFINED, JsValue::from_str),
                        ),
                        (
                            "snippet",
                            source
                                .snippet
                                .as_deref()
                                .map_or(JsValue::UNDEFINED, JsValue::from_str),
                        ),
                        (
                            "publishedAt",
                            source
                                .published_at
                                .as_deref()
                                .map_or(JsValue::UNDEFINED, JsValue::from_str),
                        ),
                    ])?
                    .as_ref(),
                );
            }
            object(&[
                ("kind", JsValue::from_str("search")),
                (
                    "answer",
                    answer
                        .as_deref()
                        .map_or(JsValue::UNDEFINED, JsValue::from_str),
                ),
                ("sources", rows.into()),
                ("truncated", JsValue::from_bool(*truncated)),
            ])
        }
        WebCardModel::Fetch {
            url,
            status_code,
            truncated,
        } => object(&[
            ("kind", JsValue::from_str("fetch")),
            ("url", JsValue::from_str(url)),
            ("statusCode", JsValue::from_f64(f64::from(*status_code))),
            ("truncated", JsValue::from_bool(*truncated)),
        ]),
    }
}

fn optional_json(value: &JsValue, key: &str) -> Result<Option<Value>, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_null() || property.is_undefined() {
        Ok(None)
    } else {
        json_value(&property).map(Some)
    }
}

fn json_array(value: &JsValue, owner: &str) -> Result<Vec<Value>, JsValue> {
    let values = value
        .clone()
        .dyn_into::<Array>()
        .map_err(|_| js_sys::TypeError::new(&format!("{owner} must be an array")))?;
    (0..values.length())
        .map(|index| json_value(&values.get(index)))
        .collect()
}

fn json_value(value: &JsValue) -> Result<Value, JsValue> {
    let encoded = JSON::stringify(value)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("Tool wire value is not JSON-compatible"))?;
    serde_json::from_str(&encoded)
        .map_err(|error| js_sys::TypeError::new(&format!("invalid Tool wire JSON: {error}")).into())
}

#[allow(clippy::cast_precision_loss)] // The source wire value is already a JavaScript number.
fn wire_u64_number(value: u64) -> JsValue {
    JsValue::from_f64(value as f64)
}
