//! Browser fixture transport: the Rust `FixtureApi` behind the Connection service face.
//!
//! Source `apply` picks `FixtureApiClient` when the page URL carries `?fixture`; the
//! fixture is the fake server, so every unary call, Remote call, response, and
//! downstream frame is answered in-page by the same Rust fixture the native suites
//! pin, driven by the same generation controller through the browser event loop.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    sync::Arc,
};

use js_sys::{Function, JSON, Promise, Reflect};
use seekdeep_abort::AbortSignal;
use seekdeep_client_connection::{
    ConnectionConfig, ConnectionSinks, ConnectionState, EventFrame, FixtureApi, FixtureOptions,
    RpcId,
};
use serde_json::Value;
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};
use wasm_bindgen_futures::future_to_promise;

use crate::wasm::{call_method, is_loopback_hostname, object, random_uuid};

thread_local! {
    /// JavaScript callbacks addressed by id from `Send` sinks: the page is single-threaded,
    /// so the controller's sinks carry only the id and resolve it here when they fire.
    static CALLBACKS: RefCell<HashMap<u64, JsValue>> = RefCell::new(HashMap::new());
    static NEXT_CALLBACK: Cell<u64> = const { Cell::new(1) };
}

fn retain(callback: JsValue) -> u64 {
    let id = NEXT_CALLBACK.with(|next| {
        let id = next.get();
        next.set(id.wrapping_add(1));
        id
    });
    CALLBACKS.with(|callbacks| callbacks.borrow_mut().insert(id, callback));
    id
}

fn release(id: u64) {
    CALLBACKS.with(|callbacks| callbacks.borrow_mut().remove(&id));
}

fn invoke(id: u64, arguments: &[JsValue]) {
    let Some(callback) = CALLBACKS.with(|callbacks| callbacks.borrow().get(&id).cloned()) else {
        return;
    };
    let Some(function) = callback.dyn_ref::<Function>() else {
        return;
    };
    let array = js_sys::Array::new();
    for argument in arguments {
        array.push(argument);
    }
    if let Err(error) = function.apply(&JsValue::UNDEFINED, &array) {
        web_sys::console::error_2(
            &JsValue::from_str("[web-runtime] fixture connection sink threw:"),
            &error,
        );
    }
}

/// The page query when the URL selects the fixture transport (`?fixture`, `?fixture=empty`, ...).
pub(crate) fn fixture_page_query() -> Option<String> {
    let location = Reflect::get(&js_sys::global(), &JsValue::from_str("location")).ok()?;
    if !location.is_object() {
        return None;
    }
    let search = Reflect::get(&location, &JsValue::from_str("search"))
        .ok()?
        .as_string()?;
    search
        .trim_start_matches('?')
        .split('&')
        .any(|pair| pair.split('=').next() == Some("fixture"))
        .then_some(search)
}

fn to_js(value: &Value) -> Result<JsValue, JsValue> {
    let text = serde_json::to_string(value)
        .map_err(|error| JsValue::from(js_sys::Error::new(&error.to_string())))?;
    JSON::parse(&text)
}

fn from_js(value: &JsValue) -> Result<Value, JsValue> {
    if value.is_undefined() {
        return Ok(Value::Null);
    }
    let text = JSON::stringify(value)?;
    let Some(text) = text.as_string() else {
        return Ok(Value::Null);
    };
    serde_json::from_str(&text)
        .map_err(|error| JsValue::from(js_sys::Error::new(&error.to_string())))
}

/// Mirrors a JavaScript `AbortSignal` onto a Rust signal, reason included.
fn abort_signal(signal: &JsValue) -> AbortSignal {
    let rust = AbortSignal::default();
    if !signal.is_object() {
        return rust;
    }
    let reason_of = |signal: &JsValue| {
        Reflect::get(signal, &JsValue::from_str("reason"))
            .ok()
            .and_then(|reason| from_js(&reason).ok())
            .unwrap_or(Value::Null)
    };
    if Reflect::get(signal, &JsValue::from_str("aborted"))
        .ok()
        .and_then(|value| value.as_bool())
        == Some(true)
    {
        rust.abort_with_reason(reason_of(signal));
        return rust;
    }
    let target = signal.clone();
    let mirrored = rust.clone();
    let listener = Closure::once_into_js(move || {
        mirrored.abort_with_reason(reason_of(&target));
    });
    let _ = call_method(
        signal,
        "addEventListener",
        &[JsValue::from_str("abort"), listener],
    );
    rust
}

fn connection_config(config: &JsValue) -> ConnectionConfig {
    let mut resolved = ConnectionConfig::default();
    if !config.is_object() {
        return resolved;
    }
    let number = |name: &str| {
        Reflect::get(config, &JsValue::from_str(name))
            .ok()
            .and_then(|value| value.as_f64())
    };
    if let Some(value) = number("backoffBaseMs") {
        resolved.backoff_base_ms = value;
    }
    if let Some(value) = number("backoffFactor") {
        resolved.backoff_factor = value;
    }
    if let Some(value) = number("backoffMaxMs") {
        resolved.backoff_max_ms = value;
    }
    if let Some(value) = number("streamOpenTimeoutMs") {
        resolved.stream_open_timeout_ms = value;
    }
    resolved
}

fn frame_sink(sinks: &JsValue, name: &str) -> Option<Arc<dyn Fn(EventFrame) + Send + Sync>> {
    let callback = Reflect::get(sinks, &JsValue::from_str(name)).ok()?;
    if !callback.is_function() {
        return None;
    }
    let id = retain(callback);
    Some(Arc::new(move |frame: EventFrame| {
        if let Ok(text) = serde_json::to_string(&frame)
            && let Ok(value) = JSON::parse(&text)
        {
            invoke(id, &[value]);
        }
    }))
}

/// The complete fixture-backed `ctx.connection` value for one page.
///
/// # Errors
///
/// Returns JavaScript face-construction failures.
#[allow(clippy::too_many_lines)] // The service face is assembled atomically at this boundary.
pub(crate) fn connection_object(query: &str, hostname: &str) -> Result<JsValue, JsValue> {
    let fixture = FixtureApi::new(FixtureOptions::from_query(query));
    let is_loopback = is_loopback_hostname(hostname);
    let handle = fixture.connection_handle(is_loopback);

    let call = {
        let fixture = fixture.clone();
        Closure::wrap(Box::new(
            move |method: String, payload: JsValue, signal: JsValue| -> Promise {
                let fixture = fixture.clone();
                future_to_promise(async move {
                    let rpc_id = RpcId::new(random_uuid()?);
                    let payload = from_js(&payload)?;
                    let response = fixture
                        .unary(&method, rpc_id, payload, abort_signal(&signal))
                        .await
                        .map_err(|error| JsValue::from(js_sys::Error::new(&error.to_string())))?;
                    to_js(
                        &serde_json::to_value(&response).map_err(|error| {
                            JsValue::from(js_sys::Error::new(&error.to_string()))
                        })?,
                    )
                })
            },
        )
            as Box<dyn FnMut(String, JsValue, JsValue) -> Promise>)
    };
    let respond = {
        let fixture = fixture.clone();
        Closure::wrap(
            Box::new(move |message: JsValue, _signal: JsValue| -> Promise {
                match from_js(&message).and_then(|message| to_js(&fixture.respond(&message))) {
                    Ok(receipt) => Promise::resolve(&receipt),
                    Err(error) => Promise::reject(&error),
                }
            }) as Box<dyn FnMut(JsValue, JsValue) -> Promise>,
        )
    };
    let call_rpc = {
        let handle = handle.clone();
        Closure::wrap(Box::new(
            move |channel: String,
                  endpoint: String,
                  payload: JsValue,
                  signal: JsValue|
                  -> Promise {
                let handle = handle.clone();
                future_to_promise(async move {
                    let payload = from_js(&payload)?;
                    let result = handle
                        .call(&channel, &endpoint, payload, abort_signal(&signal))
                        .await
                        .map_err(|error| JsValue::from(js_sys::Error::new(&error.to_string())))?;
                    to_js(
                        &serde_json::to_value(&result).map_err(|error| {
                            JsValue::from(js_sys::Error::new(&error.to_string()))
                        })?,
                    )
                })
            },
        )
            as Box<dyn FnMut(String, String, JsValue, JsValue) -> Promise>)
    };
    let subscribe_envelopes = {
        let fixture = fixture.clone();
        Closure::wrap(
            Box::new(move |listener: JsValue| -> Result<JsValue, JsValue> {
                if !listener.is_function() {
                    return Err(js_sys::Error::new(
                        "connection envelope listener must be a function",
                    )
                    .into());
                }
                let id = retain(listener);
                let subscription = fixture.subscribe_envelopes(Arc::new(move |envelopes| {
                    for envelope in &envelopes {
                        if let Ok(value) = JSON::parse(envelope.as_raw()) {
                            invoke(id, &[value]);
                        }
                    }
                }));
                let dispose = Closure::wrap(Box::new(move || {
                    subscription.dispose();
                    release(id);
                }) as Box<dyn FnMut()>);
                Ok(dispose.into_js_value())
            }) as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
        )
    };
    install_timing_hooks(&fixture)?;
    let client = object(&[
        ("call", call.into_js_value()),
        ("respond", respond.into_js_value()),
        ("callRpc", call_rpc.into_js_value()),
        ("subscribeEnvelopes", subscribe_envelopes.into_js_value()),
    ])?;
    let api = crate::wasm::api_proxy(&client.clone().into())?;

    let get_description = {
        let handle = handle.clone();
        Closure::wrap(Box::new(move || -> JsValue {
            handle
                .host_description()
                .and_then(|value| to_js(&value).ok())
                .unwrap_or(JsValue::UNDEFINED)
        }) as Box<dyn FnMut() -> JsValue>)
    };
    let subscribe_description = {
        let handle = handle.clone();
        Closure::wrap(Box::new(move |listener: JsValue| -> JsValue {
            let id = retain(listener);
            let subscription = handle.subscribe_host_description(Arc::new(move || invoke(id, &[])));
            Closure::wrap(Box::new(move || {
                subscription.dispose();
                release(id);
            }) as Box<dyn FnMut()>)
            .into_js_value()
        }) as Box<dyn FnMut(JsValue) -> JsValue>)
    };
    let host_description = object(&[
        ("getSnapshot", get_description.into_js_value()),
        ("subscribe", subscribe_description.into_js_value()),
    ])?;

    let start = {
        let handle = handle.clone();
        Closure::wrap(Box::new(
            move |sinks: JsValue, config: JsValue| -> Result<JsValue, JsValue> {
                let connected = Reflect::get(&sinks, &JsValue::from_str("onConnected"))
                    .ok()
                    .filter(JsValue::is_function)
                    .map(retain);
                let state = Reflect::get(&sinks, &JsValue::from_str("onStateChange"))
                    .ok()
                    .filter(JsValue::is_function)
                    .map(retain);
                let rust_sinks = ConnectionSinks {
                    on_mux_envelope: frame_sink(&sinks, "onMuxEnvelope"),
                    on_host_envelope: frame_sink(&sinks, "onHostEnvelope"),
                    on_connected: connected.map(|id| {
                        Arc::new(move |description: Value| {
                            if let Ok(value) = to_js(&description) {
                                invoke(id, &[value]);
                            }
                        }) as Arc<dyn Fn(Value) + Send + Sync>
                    }),
                    on_state_change: state.map(|id| {
                        Arc::new(move |state: ConnectionState| {
                            let name = match state {
                                ConnectionState::Connected => "connected",
                                ConnectionState::Reconnecting => "reconnecting",
                            };
                            invoke(id, &[JsValue::from_str(name)]);
                        }) as Arc<dyn Fn(ConnectionState) + Send + Sync>
                    }),
                };
                let stop_handle = handle
                    .start(rust_sinks, connection_config(&config))
                    .map_err(|error| JsValue::from(js_sys::Error::new(&error.to_string())))?;
                let stop = Closure::wrap(Box::new(move || stop_handle.stop()) as Box<dyn FnMut()>);
                Ok(object(&[("stop", stop.into_js_value())])?.into())
            },
        )
            as Box<dyn FnMut(JsValue, JsValue) -> Result<JsValue, JsValue>>)
    };
    let rpc = object(&[("call", {
        let client = client.clone();
        Closure::wrap(Box::new(
            move |channel: String,
                  endpoint: String,
                  payload: JsValue,
                  signal: JsValue|
                  -> Promise {
                match call_method(
                    &client,
                    "callRpc",
                    &[
                        JsValue::from_str(&channel),
                        JsValue::from_str(&endpoint),
                        payload,
                        signal,
                    ],
                ) {
                    Ok(value) => Promise::resolve(&value),
                    Err(error) => Promise::reject(&error),
                }
            },
        )
            as Box<dyn FnMut(String, String, JsValue, JsValue) -> Promise>)
        .into_js_value()
    })])?;
    Ok(object(&[
        ("api", api),
        ("rpc", rpc.into()),
        ("isLoopback", JsValue::from_bool(is_loopback)),
        ("hostDescription", host_description.into()),
        ("start", start.into_js_value()),
    ])?
    .into())
}

type HookBody = Box<dyn FnMut(JsValue, JsValue, JsValue, JsValue) -> Result<JsValue, JsValue>>;

fn hook(name: &'static str, body: HookBody) -> (&'static str, JsValue) {
    (name, Closure::wrap(body).into_js_value())
}

fn string_of(value: &JsValue) -> String {
    value.as_string().unwrap_or_default()
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // JavaScript numbers cross the hook boundary.
fn count_of(value: &JsValue, fallback: f64) -> u64 {
    value.as_f64().unwrap_or(fallback).max(0.0) as u64
}

fn js_error(error: &dyn std::fmt::Display) -> JsValue {
    js_sys::Error::new(&error.to_string()).into()
}

/// Source: `globalThis.__fxTiming` exposes the fixture's timing hooks to browser lanes.
#[allow(clippy::too_many_lines)] // One hook table mirrors the source object member for member.
fn install_timing_hooks(fixture: &Arc<FixtureApi>) -> Result<(), JsValue> {
    let f = fixture.clone();
    let set_history_delay = hook(
        "setHistoryDelay",
        Box::new(move |ms, _, _, _| {
            f.set_history_delay(count_of(&ms, 0.0));
            Ok(JsValue::UNDEFINED)
        }),
    );
    let f = fixture.clone();
    let fail_next_history = hook(
        "failNextHistory",
        Box::new(move |_, _, _, _| {
            f.fail_next_history();
            Ok(JsValue::UNDEFINED)
        }),
    );
    let f = fixture.clone();
    let append_user = hook(
        "appendUser",
        Box::new(move |id, message, _, _| {
            f.append_user(&string_of(&id), &string_of(&message));
            Ok(JsValue::UNDEFINED)
        }),
    );
    let f = fixture.clone();
    let append_title = hook(
        "appendTitle",
        Box::new(move |id, title, _, _| {
            f.append_title(&string_of(&id), &string_of(&title));
            Ok(JsValue::UNDEFINED)
        }),
    );
    let f = fixture.clone();
    let append_silent = hook(
        "appendSilent",
        Box::new(move |id, message, _, _| {
            f.append_silent(&string_of(&id), &string_of(&message));
            Ok(JsValue::UNDEFINED)
        }),
    );
    let f = fixture.clone();
    let break_streams = hook(
        "breakStreams",
        Box::new(move |_, _, _, _| {
            f.break_streams();
            Ok(JsValue::UNDEFINED)
        }),
    );
    let f = fixture.clone();
    let start_storm = hook(
        "startReasoningChunkStorm",
        Box::new(move |id, count, per_interval, interval| {
            f.start_reasoning_chunk_storm(
                &string_of(&id),
                count_of(&count, 0.0),
                count_of(&per_interval, 0.0),
                count_of(&interval, 0.0),
            )
            .map(|marker| JsValue::from_str(&marker))
            .map_err(|error| js_error(&error))
        }),
    );
    let f = fixture.clone();
    let storm_state = hook(
        "reasoningChunkStormState",
        Box::new(move |_, _, _, _| match f.reasoning_chunk_storm_state() {
            None => Ok(JsValue::NULL),
            Some(state) => to_js(&serde_json::json!({
                "sessionId": state.session_id,
                "chunkCount": state.chunk_count,
                "chunksPerInterval": state.chunks_per_interval,
                "intervalMs": state.interval_ms,
                "emitted": state.emitted,
                "marker": state.marker,
                "emitting": state.emitting,
            })),
        }),
    );
    let f = fixture.clone();
    let begin_retry = hook(
        "beginModelRetry",
        Box::new(move |id, _, _, _| {
            f.begin_model_retry(&string_of(&id));
            Ok(JsValue::UNDEFINED)
        }),
    );
    let f = fixture.clone();
    let schedule_retry = hook(
        "scheduleModelRetry",
        Box::new(move |id, retry, delay, _| {
            f.schedule_model_retry(
                &string_of(&id),
                count_of(&retry, 1.0),
                count_of(&delay, 450.0),
            );
            Ok(JsValue::UNDEFINED)
        }),
    );
    let f = fixture.clone();
    let cancel_retry = hook(
        "cancelModelRetryDuringBackoff",
        Box::new(move |id, delay, _, _| {
            f.cancel_model_retry_during_backoff(&string_of(&id), count_of(&delay, 450.0));
            Ok(JsValue::UNDEFINED)
        }),
    );
    let f = fixture.clone();
    let complete_retry = hook(
        "completeModelRetry",
        Box::new(move |id, _, _, _| {
            f.complete_model_retry(&string_of(&id));
            Ok(JsValue::UNDEFINED)
        }),
    );
    let hooks = object(&[
        set_history_delay,
        fail_next_history,
        append_user,
        append_title,
        start_storm,
        storm_state,
        begin_retry,
        schedule_retry,
        cancel_retry,
        complete_retry,
        append_silent,
        break_streams,
    ])?;
    Reflect::set(&js_sys::global(), &JsValue::from_str("__fxTiming"), &hooks)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn frame_sink_preserves_raw_payload_strings_keys_and_numbers() {
        let received = js_sys::Array::new();
        let captured = received.clone();
        let callback = Closure::<dyn FnMut(JsValue)>::new(move |frame| {
            captured.push(&frame);
        })
        .into_js_value();
        let sinks = object(&[("frame", callback)]).unwrap();
        let sink = frame_sink(&sinks, "frame").unwrap();
        let frame: EventFrame = serde_json::from_str(
            r#"{"rpcId":"raw-frame","payload":{"text":"\ud800","literal":"\\ud800","\udfff":"kept","huge":1e400,"negativeZero":-0}}"#,
        )
        .unwrap();
        sink(frame);
        assert_eq!(received.length(), 1);
        let correct = Function::new_with_args(
            "frame",
            "return frame.rpcId === 'raw-frame' && frame.payload.text.charCodeAt(0) === 0xd800 && frame.payload.literal === '\\\\ud800' && frame.payload['\\udfff'] === 'kept' && frame.payload.huge === Infinity && Object.is(frame.payload.negativeZero, -0)",
        );
        assert_eq!(
            correct
                .call1(&JsValue::UNDEFINED, &received.get(0))
                .unwrap()
                .as_bool(),
            Some(true)
        );
    }
}
