//! One native Node worker's Rust-owned program and binding boundary.

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use js_sys::{Array, Promise};
use serde::Serialize;
use serde_json::json;
use wasm_bindgen::{JsCast as _, prelude::*};
use wasm_bindgen_futures::{JsFuture, spawn_local};

use crate::{
    CodeJsonString, CodeJsonValue, bridge,
    output_json::{code_string_bytes_up_to, truncate_code_string_bytes},
    snapshot::{Intrinsics, materialize, read_wire, snapshot},
};

#[derive(Serialize)]
struct WorkerCall<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    id: u64,
    global: &'a str,
    name: &'a CodeJsonString,
    args: &'a CodeJsonValue,
}

#[derive(Serialize)]
struct WorkerFailure {
    kind: &'static str,
    message: CodeJsonString,
}

#[derive(Serialize)]
struct WorkerLog<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    text: &'a CodeJsonString,
}

#[derive(Serialize)]
struct WorkerDone {
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<CodeJsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<WorkerFailure>,
}

impl WorkerDone {
    fn success(value: Option<CodeJsonValue>) -> Self {
        Self {
            kind: "done",
            value,
            error: None,
        }
    }

    fn failure(kind: &'static str, message: impl Into<CodeJsonString>) -> Self {
        Self {
            kind: "done",
            value: None,
            error: Some(WorkerFailure {
                kind,
                message: message.into(),
            }),
        }
    }
}

struct Pending {
    resolve: JsValue,
    reject: JsValue,
    member: CodeJsonString,
    error_class: Option<JsValue>,
}

struct WorkerState {
    port: JsValue,
    intrinsics: Intrinsics,
    inspect: JsValue,
    inspect_options: JsValue,
    max_bytes: usize,
    log_bytes: usize,
    log_entries: usize,
    truncated: bool,
    next_call: u64,
    pending: HashMap<u64, Pending>,
    callbacks: Vec<JsValue>,
}

thread_local! {
    static WORKER: RefCell<Option<Rc<RefCell<WorkerState>>>> = const { RefCell::new(None) };
}

fn remember(state: &Rc<RefCell<WorkerState>>, callback: JsValue) -> JsValue {
    state.borrow_mut().callbacks.push(callback.clone());
    callback
}

fn post(state: &Rc<RefCell<WorkerState>>, message: &impl Serialize) -> Result<(), JsValue> {
    let port = state.borrow().port.clone();
    let encoded =
        serde_json::to_string(message).map_err(|error| bridge::error(&error.to_string()))?;
    bridge::method(&port, "postMessage", &[bridge::parse(&encoded)?])?;
    Ok(())
}

fn log(state: &Rc<RefCell<WorkerState>>, text: &CodeJsonString) -> Result<(), JsValue> {
    let (emitted, limited) = {
        let mut state = state.borrow_mut();
        if state.truncated {
            return Ok(());
        }
        let separator = usize::from(state.log_entries != 0);
        let available = state
            .max_bytes
            .saturating_sub(state.log_bytes.saturating_add(separator));
        if let Some(bytes) = code_string_bytes_up_to(text, available) {
            state.log_bytes += bytes + separator;
            state.log_entries += 1;
            (Some(text.clone()), false)
        } else {
            state.truncated = true;
            let prefix = truncate_code_string_bytes(text, available);
            if prefix.is_empty() {
                (None, true)
            } else {
                state.log_bytes += code_string_bytes_up_to(&prefix, available)
                    .expect("fitting prefix")
                    + separator;
                state.log_entries += 1;
                (Some(prefix), true)
            }
        }
    };
    if let Some(text) = emitted {
        post(
            state,
            &WorkerLog {
                kind: "log",
                text: &text,
            },
        )?;
    }
    if limited {
        post(state, &json!({"type":"output-limit"}))?;
    }
    Ok(())
}

fn make_console(state: &Rc<RefCell<WorkerState>>) -> Result<JsValue, JsValue> {
    let console = bridge::object(&[])?;
    for level in ["log", "info", "warn", "error", "debug"] {
        let state_copy = state.clone();
        let callback = Closure::<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>::new(
            move |arguments: JsValue| {
                let arguments: Array = arguments.unchecked_into();
                let (inspect, options) = {
                    let state = state_copy.borrow();
                    (state.inspect.clone(), state.inspect_options.clone())
                };
                let mut values = Vec::with_capacity(arguments.length() as usize);
                for index in 0..arguments.length() {
                    let value = arguments.get(index);
                    values.push(if value.is_string() {
                        bridge::text(&value)?
                    } else {
                        bridge::text(&bridge::apply(
                            &inspect,
                            &JsValue::UNDEFINED,
                            &[value, options.clone()],
                        )?)?
                    });
                }
                log(&state_copy, &CodeJsonString::join(&values, " "))?;
                Ok(JsValue::UNDEFINED)
            },
        )
        .into_js_value();
        let function = bridge::variadic(&remember(state, callback));
        bridge::define(&console, &JsValue::from_str(level), &function, true, true)?;
    }
    Ok(console)
}

fn patch_stream(state: &Rc<RefCell<WorkerState>>, stream: &JsValue) -> Result<(), JsValue> {
    let state_copy = state.clone();
    let callback = Closure::<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>::new(
        move |arguments: JsValue| {
            let arguments: Array = arguments.unchecked_into();
            let chunk = arguments.get(0);
            let text = bridge::text(&chunk)?;
            log(&state_copy, &text)?;
            let callback = [arguments.get(1), arguments.get(2)]
                .into_iter()
                .find(JsValue::is_function);
            if let Some(callback) = callback {
                let callback = Closure::once_into_js(move || {
                    if let Err(error) =
                        bridge::apply(&callback, &JsValue::UNDEFINED, &[JsValue::NULL])
                    {
                        wasm_bindgen::throw_val(error);
                    }
                });
                bridge::method(&bridge::api("global")?, "queueMicrotask", &[callback])?;
            }
            Ok(JsValue::TRUE)
        },
    )
    .into_js_value();
    let function = bridge::variadic(&remember(state, callback));
    bridge::set_key(stream, &JsValue::from_str("write"), &function)?;
    Ok(())
}

fn make_error_class(
    state: &Rc<RefCell<WorkerState>>,
    descriptor: &JsValue,
) -> Result<JsValue, JsValue> {
    let descriptor = descriptor.clone();
    let fields = Closure::<dyn Fn(JsValue, JsValue) -> Result<(), JsValue>>::new(
        move |error: JsValue, member_name: JsValue| {
            bridge::define(
                &error,
                &JsValue::from_str("name"),
                &bridge::get(&descriptor, "name")?,
                true,
                false,
            )?;
            bridge::define(
                &error,
                &bridge::get(&descriptor, "memberNameProperty")?,
                &member_name,
                true,
                false,
            )
        },
    )
    .into_js_value();
    let fields = remember(state, fields);
    let constructor = bridge::function(&bridge::array(&[
        JsValue::from_str("CapturedError"),
        JsValue::from_str("fields"),
        JsValue::from_str(
            "return class BindingCallError extends CapturedError { constructor(memberName, message) { super(message); fields(this, memberName); } }",
        ),
    ])?)?;
    bridge::apply(
        &constructor,
        &JsValue::UNDEFINED,
        &[bridge::get(&bridge::api("global")?, "Error")?, fields],
    )
}

fn binding_error(
    class: Option<&JsValue>,
    member: &CodeJsonString,
    message: impl Into<CodeJsonString>,
) -> JsValue {
    let message = message.into();
    let message = match bridge::parse(message.as_raw()) {
        Ok(message) => message,
        Err(error) => return error,
    };
    class.map_or_else(
        || bridge::error_value(&message),
        |class| {
            bridge::parse(member.as_raw())
                .and_then(|member| bridge::array(&[member, message.clone()]))
                .and_then(|arguments| bridge::construct(class, &arguments))
                .unwrap_or_else(|error| error)
        },
    )
}

fn binding_call(
    state: &Rc<RefCell<WorkerState>>,
    global: &str,
    member: &CodeJsonString,
    class: Option<JsValue>,
    argument: &JsValue,
) -> JsValue {
    let intrinsics = state.borrow().intrinsics.clone();
    let Some(argument) = snapshot(argument, &intrinsics) else {
        return bridge::reject(&binding_error(
            class.as_ref(),
            member,
            "binding arguments must be lossless JSON",
        ));
    };
    let state_copy = state.clone();
    let global = global.to_owned();
    let member = member.clone();
    let executor = Closure::once_into_js(move |resolve: JsValue, reject: JsValue| {
        let id = {
            let mut state = state_copy.borrow_mut();
            let id = state.next_call;
            state.next_call += 1;
            state.pending.insert(
                id,
                Pending {
                    resolve,
                    reject,
                    member: member.clone(),
                    error_class: class,
                },
            );
            id
        };
        if let Err(error) = post(
            &state_copy,
            &WorkerCall {
                kind: "call",
                id,
                global: &global,
                name: &member,
                args: &argument.wire,
            },
        ) {
            let pending = state_copy.borrow_mut().pending.remove(&id);
            if let Some(pending) = pending {
                let mut message: CodeJsonString =
                    "binding arguments must be structured-cloneable: ".into();
                message.push_utf16(bridge::message_text(&error).utf16_units());
                let error = binding_error(pending.error_class.as_ref(), &pending.member, message);
                let _ = bridge::apply(&pending.reject, &JsValue::UNDEFINED, &[error]);
            }
        }
    });
    bridge::promise(&executor)
}

fn wire_replies(state: &Rc<RefCell<WorkerState>>) -> Result<(), JsValue> {
    let state_copy = state.clone();
    let callback =
        Closure::<dyn FnMut(JsValue) -> Result<(), JsValue>>::new(move |reply: JsValue| {
            if bridge::get(&reply, "type")?.as_string().as_deref() != Some("reply") {
                return Ok(());
            }
            let Some(id) = bridge::get(&reply, "id")?.as_f64() else {
                return Ok(());
            };
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "only generated positive integer IDs can match the pending map"
            )]
            let id_key = id as u64;
            if !id.is_finite() || id < 0.0 || id.fract() != 0.0 {
                return Ok(());
            }
            let Some(pending) = state_copy.borrow_mut().pending.remove(&id_key) else {
                return Ok(());
            };
            if bridge::get(&reply, "ok")?.as_bool() == Some(true) {
                let intrinsics = state_copy.borrow().intrinsics.clone();
                let wire = bridge::get(&reply, "value")
                    .ok()
                    .and_then(|value| read_wire(&value, &intrinsics));
                if let Some(wire) = wire {
                    bridge::apply(
                        &pending.resolve,
                        &JsValue::UNDEFINED,
                        &[materialize(&wire)?],
                    )?;
                } else {
                    let error = binding_error(
                        pending.error_class.as_ref(),
                        &pending.member,
                        "binding resolution must be lossless JSON",
                    );
                    bridge::apply(&pending.reject, &JsValue::UNDEFINED, &[error])?;
                }
            } else {
                let message = bridge::text(&bridge::get(&reply, "message")?)?;
                let error = binding_error(pending.error_class.as_ref(), &pending.member, message);
                bridge::apply(&pending.reject, &JsValue::UNDEFINED, &[error])?;
            }
            Ok(())
        })
        .into_js_value();
    let callback = remember(state, callback);
    let port = state.borrow().port.clone();
    bridge::method(&port, "on", &[JsValue::from_str("message"), callback])?;
    Ok(())
}

fn completion(state: &Rc<RefCell<WorkerState>>, outcome: Result<JsValue, JsValue>) -> WorkerDone {
    let (intrinsics, max_bytes, remaining) = {
        let state = state.borrow();
        (
            state.intrinsics.clone(),
            state.max_bytes,
            state.max_bytes.saturating_sub(state.log_bytes),
        )
    };
    let failure = |kind: &'static str, message: CodeJsonString| {
        if code_string_bytes_up_to(&message, remaining).is_some() {
            WorkerDone::failure(kind, message)
        } else {
            WorkerDone::failure(
                "output-limit",
                format!("outer output exceeded {max_bytes} bytes"),
            )
        }
    };
    match outcome {
        Ok(value) if value.is_undefined() => WorkerDone::success(None),
        Ok(value) => {
            let Some(snapshot) = snapshot(&value, &intrinsics) else {
                return failure(
                    "invalid-output",
                    "program completion must be lossless JSON".into(),
                );
            };
            if snapshot.bytes > remaining {
                return WorkerDone::failure(
                    "output-limit",
                    format!("outer output exceeded {max_bytes} bytes"),
                );
            }
            WorkerDone::success(Some(snapshot.wire))
        }
        Err(error) => {
            let rendered = if bridge::is_error(&error) {
                bridge::get(&error, "stack").and_then(|stack| {
                    if stack.is_null() || stack.is_undefined() {
                        bridge::get(&error, "message").and_then(|message| bridge::text(&message))
                    } else {
                        bridge::text(&stack)
                    }
                })
            } else {
                bridge::text(&error)
            };
            failure(
                "exception",
                rendered.unwrap_or_else(|_| "program threw an unrenderable value".into()),
            )
        }
    }
}

pub(crate) fn start(apis: &JsValue) -> Result<(), JsValue> {
    let port = bridge::get(apis, "parentPort")?;
    if port.is_null() || port.is_undefined() {
        return Err(bridge::error(
            "seekdeep-code-runtime-worker-thread: worker entry loaded outside a worker thread",
        ));
    }
    let data = bridge::get(apis, "workerData")?;
    let max_bytes = bridge::get(&data, "maxOutputBytes")?
        .as_f64()
        .ok_or_else(|| bridge::error("invalid output limit"))?;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "native host validates the positive safe-integer output limit"
    )]
    let max_bytes = max_bytes as usize;
    let state = Rc::new(RefCell::new(WorkerState {
        port,
        intrinsics: Intrinsics::capture()?,
        inspect: bridge::get(apis, "inspect")?,
        inspect_options: bridge::from_json(
            &json!({"depth":4, "maxArrayLength":100, "maxStringLength":10_000}),
        )?,
        max_bytes,
        log_bytes: 2,
        log_entries: 0,
        truncated: false,
        next_call: 1,
        pending: HashMap::new(),
        callbacks: Vec::new(),
    }));
    WORKER.with(|slot| *slot.borrow_mut() = Some(state.clone()));
    let process = bridge::get(apis, "process")?;
    patch_stream(&state, &bridge::get(&process, "stdout")?)?;
    patch_stream(&state, &bridge::get(&process, "stderr")?)?;
    wire_replies(&state)?;
    let result = run_program(&state, &data);
    spawn_local(async move {
        let outcome = match result {
            Ok(promise) => JsFuture::from(promise.unchecked_into::<Promise>()).await,
            Err(error) => Err(error),
        };
        let done = completion(&state, outcome);
        if let Err(error) = post(&state, &done) {
            wasm_bindgen::throw_val(error);
        }
    });
    Ok(())
}

fn run_program(state: &Rc<RefCell<WorkerState>>, data: &JsValue) -> Result<JsValue, JsValue> {
    let declarations = bridge::get(data, "namespaces")?;
    let declarations: Array = declarations
        .dyn_into()
        .map_err(|_| bridge::error("invalid worker namespaces"))?;
    let mut parameters = Vec::new();
    let mut values = Vec::new();
    let mut error_parameters = Vec::new();
    let mut error_values = Vec::new();
    for index in 0..declarations.length() {
        let declaration = declarations.get(index);
        let global = bridge::get(&declaration, "global")?
            .as_string()
            .ok_or_else(|| bridge::error("invalid namespace global"))?;
        let descriptor = bridge::get(&declaration, "errorClass")?;
        let error_class = if descriptor.is_undefined() {
            None
        } else {
            let class = make_error_class(state, &descriptor)?;
            error_parameters.push(bridge::get(&descriptor, "name")?);
            error_values.push(class.clone());
            Some(class)
        };
        let namespace = bridge::object(&[])?;
        let names: Array = bridge::get(&declaration, "names")?
            .dyn_into()
            .map_err(|_| bridge::error("invalid namespace members"))?;
        for index in 0..names.length() {
            let name_value = names.get(index);
            if !name_value.is_string() {
                return Err(bridge::error("invalid namespace member"));
            }
            let name = bridge::text(&name_value)?;
            let global_copy = global.clone();
            let name_copy = name.clone();
            let class = error_class.clone();
            let state_copy = state.clone();
            let callback = Closure::<dyn Fn(JsValue) -> JsValue>::new(move |argument: JsValue| {
                binding_call(
                    &state_copy,
                    &global_copy,
                    &name_copy,
                    class.clone(),
                    &argument,
                )
            })
            .into_js_value();
            let callback = remember(state, callback);
            let callback = bridge::unary(&callback);
            bridge::define(&namespace, &name_value, &callback, true, false)?;
        }
        parameters.push(JsValue::from_str(&global));
        values.push(namespace);
    }
    parameters.extend(error_parameters);
    values.extend(error_values);
    parameters.push(JsValue::from_str("console"));
    values.push(make_console(state)?);
    let code = bridge::get(data, "code")?
        .as_string()
        .ok_or_else(|| bridge::error("invalid worker code"))?;
    parameters.push(JsValue::from_str(&format!("'use strict';\n{code}")));
    bridge::array(&parameters)
        .and_then(|parameters| bridge::async_function(&parameters))
        .and_then(|function| bridge::apply(&function, &JsValue::UNDEFINED, &values))
}
