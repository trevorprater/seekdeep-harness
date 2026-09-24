//! Rust ownership of native Node workers, timers, and pipe-draining teardown.

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use js_sys::Promise;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wasm_bindgen::{JsCast as _, prelude::*};
use wasm_bindgen_futures::{JsFuture, spawn_local};

use crate::{
    CodeJsonString, CodeJsonValue, bridge,
    snapshot::{Intrinsics, read_wire},
};

#[derive(Deserialize)]
struct HostRequest {
    #[serde(rename = "type")]
    kind: String,
    run: Option<u64>,
    #[serde(
        default,
        deserialize_with = "seekdeep_lossless_json::deserialize_optional"
    )]
    boot: Option<CodeJsonValue>,
    limits: Option<Value>,
    #[serde(rename = "callId")]
    call_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "seekdeep_lossless_json::deserialize_optional"
    )]
    message: Option<CodeJsonValue>,
    completion: Option<Value>,
}

#[derive(Serialize)]
struct Terminal<T> {
    run: u64,
    #[serde(rename = "type")]
    kind: &'static str,
    completion: T,
}

#[derive(Serialize)]
struct CallEvent<'a> {
    run: u64,
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(rename = "callId")]
    call_id: &'a str,
    global: &'a CodeJsonString,
    name: &'a CodeJsonString,
    args: Option<CodeJsonValue>,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum ProgramCompletion {
    Success {
        #[serde(skip_serializing_if = "Option::is_none")]
        value: Option<CodeJsonValue>,
    },
    InvalidOutput,
    Failure {
        #[serde(rename = "failureKind")]
        failure_kind: String,
        message: CodeJsonString,
    },
    WorkerError {
        message: CodeJsonString,
    },
}

#[derive(Serialize)]
struct OutputEvent<'a> {
    run: u64,
    #[serde(rename = "type")]
    kind: &'static str,
    text: &'a CodeJsonString,
}

struct Host {
    connection: JsValue,
    input: String,
    workers: HashMap<u64, Rc<RefCell<Run>>>,
    callbacks: Vec<JsValue>,
    intrinsics: Intrinsics,
}

struct Run {
    id: u64,
    worker: JsValue,
    settled: bool,
    callbacks: Vec<(JsValue, String, JsValue)>,
    compute_timer: JsValue,
    wall_timer: JsValue,
}

thread_local! {
    static HOST: RefCell<Option<Rc<RefCell<Host>>>> = const { RefCell::new(None) };
}

fn emit(host: &Rc<RefCell<Host>>, message: &impl Serialize) -> Result<(), JsValue> {
    let connection = host.borrow().connection.clone();
    let text =
        serde_json::to_string(message).map_err(|error| bridge::error(&error.to_string()))? + "\n";
    bridge::method(&connection, "write", &[JsValue::from_str(&text)])?;
    Ok(())
}

fn listen(
    run: &Rc<RefCell<Run>>,
    emitter: &JsValue,
    event: &str,
    callback: JsValue,
) -> Result<(), JsValue> {
    bridge::method(emitter, "on", &[JsValue::from_str(event), callback.clone()])?;
    run.borrow_mut()
        .callbacks
        .push((emitter.clone(), event.to_owned(), callback));
    Ok(())
}

fn timer(name: &str, callback: &JsValue, delay: f64) -> Result<JsValue, JsValue> {
    bridge::method(
        &bridge::api("global")?,
        name,
        &[callback.clone(), JsValue::from_f64(delay)],
    )
}

fn drain(stream: JsValue) -> Result<Promise, JsValue> {
    if bridge::get(&stream, "readableEnded")?.as_bool() == Some(true)
        || bridge::get(&stream, "destroyed")?.as_bool() == Some(true)
    {
        return Ok(bridge::resolve(&JsValue::UNDEFINED).unchecked_into());
    }
    let executor = Closure::once_into_js(move |resolve: JsValue, _: JsValue| {
        let registered = Rc::new(RefCell::new(None::<JsValue>));
        let registered_copy = registered.clone();
        let draining = stream.clone();
        let callback = Closure::<dyn FnMut()>::new(move || {
            if let Some(callback) = registered_copy.borrow_mut().take() {
                for event in ["end", "close", "error"] {
                    let _ = bridge::method(
                        &draining,
                        "off",
                        &[JsValue::from_str(event), callback.clone()],
                    );
                }
            }
            let _ = bridge::apply(&resolve, &JsValue::UNDEFINED, &[]);
        })
        .into_js_value();
        *registered.borrow_mut() = Some(callback.clone());
        for event in ["end", "close", "error"] {
            let _ = bridge::method(
                &stream,
                "once",
                &[JsValue::from_str(event), callback.clone()],
            );
        }
        if bridge::get(&stream, "readableEnded").is_ok_and(|value| value.as_bool() == Some(true))
            || bridge::get(&stream, "destroyed").is_ok_and(|value| value.as_bool() == Some(true))
        {
            let _ = bridge::apply(&callback, &JsValue::UNDEFINED, &[]);
        }
    });
    Ok(bridge::promise(&executor).unchecked_into())
}

fn finish(
    host: &Rc<RefCell<Host>>,
    run: &Rc<RefCell<Run>>,
    completion: impl Serialize,
) -> Result<(), JsValue> {
    let (id, worker, compute_timer, wall_timer) = {
        let mut run = run.borrow_mut();
        if run.settled {
            return Ok(());
        }
        run.settled = true;
        (
            run.id,
            run.worker.clone(),
            run.compute_timer.clone(),
            run.wall_timer.clone(),
        )
    };
    let global = bridge::api("global")?;
    bridge::method(&global, "clearInterval", &[compute_timer])?;
    bridge::method(&global, "clearTimeout", &[wall_timer])?;
    emit(
        host,
        &Terminal {
            run: id,
            kind: "terminal",
            completion,
        },
    )?;
    let host = host.clone();
    let run = run.clone();
    spawn_local(async move {
        let result = async {
            let immediate = Closure::once_into_js(move |resolve: JsValue, _: JsValue| {
                let _ = bridge::method(&global, "setImmediate", &[resolve]);
            });
            JsFuture::from(bridge::promise(&immediate).unchecked_into::<Promise>()).await?;
            let stdout = drain(bridge::get(&worker, "stdout")?)?;
            let stderr = drain(bridge::get(&worker, "stderr")?)?;
            let terminated = bridge::method(&worker, "terminate", &[])?;
            JsFuture::from(terminated.unchecked_into::<Promise>()).await?;
            JsFuture::from(stdout).await?;
            JsFuture::from(stderr).await?;
            Ok::<(), JsValue>(())
        }
        .await;
        let callbacks = std::mem::take(&mut run.borrow_mut().callbacks);
        for (emitter, event, callback) in callbacks {
            if !emitter.is_undefined() {
                let _ = bridge::method(&emitter, "off", &[JsValue::from_str(&event), callback]);
            }
        }
        host.borrow_mut().workers.remove(&id);
        let outcome = match result {
            Ok(()) => json!({"run":id, "type":"stopped"}),
            Err(error) => json!({"run":id, "type":"stopped", "error":bridge::message(&error)}),
        };
        let _ = emit(&host, &outcome);
    });
    Ok(())
}

fn worker_message(
    host: &Rc<RefCell<Host>>,
    run: &Rc<RefCell<Run>>,
    message: &JsValue,
) -> Result<(), JsValue> {
    if run.borrow().settled || !message.is_object() || message.is_null() {
        return Ok(());
    }
    let id = run.borrow().id;
    let Ok(kind) = bridge::get(message, "type") else {
        return Ok(());
    };
    match kind.as_string().as_deref() {
        Some("log") => {
            let text = bridge::get(message, "text")?;
            if text.is_string() {
                let text = bridge::text(&text)?;
                emit(
                    host,
                    &OutputEvent {
                        run: id,
                        kind: "log",
                        text: &text,
                    },
                )?;
            }
        }
        Some("call") => {
            let Some(call_id) = bridge::get(message, "id")?.as_f64() else {
                return Ok(());
            };
            let global = bridge::get(message, "global")?;
            let name = bridge::get(message, "name")?;
            if !global.is_string() || !name.is_string() {
                return Ok(());
            }
            let global = bridge::text(&global)?;
            let name = bridge::text(&name)?;
            let argument = bridge::get(message, "args")?;
            let intrinsics = host.borrow().intrinsics.clone();
            let wire = read_wire(&argument, &intrinsics);
            let call_id = bridge::string(&JsValue::from_f64(call_id))?;
            emit(
                host,
                &CallEvent {
                    run: id,
                    kind: "call",
                    call_id: &call_id,
                    global: &global,
                    name: &name,
                    args: wire,
                },
            )?;
        }
        Some("done") => {
            let error = bridge::get(message, "error")?;
            let done = if error.is_undefined() {
                let value = bridge::get(message, "value")?;
                if value.is_undefined() {
                    ProgramCompletion::Success { value: None }
                } else {
                    let intrinsics = host.borrow().intrinsics.clone();
                    if let Some(wire) = read_wire(&value, &intrinsics) {
                        ProgramCompletion::Success { value: Some(wire) }
                    } else {
                        ProgramCompletion::InvalidOutput
                    }
                }
            } else if error.is_object() && !error.is_null() {
                let Some(kind) = bridge::get(&error, "kind")?.as_string() else {
                    return Ok(());
                };
                if !["exception", "invalid-output", "output-limit"].contains(&kind.as_str()) {
                    return Ok(());
                }
                let message = bridge::get(&error, "message")?;
                if !message.is_string() {
                    return Ok(());
                }
                let message = bridge::text(&message)?;
                ProgramCompletion::Failure {
                    failure_kind: kind,
                    message,
                }
            } else {
                return Ok(());
            };
            finish(host, run, done)?;
        }
        Some("output-limit") => finish(host, run, json!({"kind":"output-limit"}))?,
        _ => {}
    }
    Ok(())
}

fn start_run(host: &Rc<RefCell<Host>>, request: &HostRequest) -> Result<(), JsValue> {
    let id = request.run.ok_or_else(|| bridge::error("invalid run ID"))?;
    if host.borrow().workers.contains_key(&id) {
        return Err(bridge::error("duplicate run ID"));
    }
    let limits = request
        .limits
        .as_ref()
        .ok_or_else(|| bridge::error("missing worker limits"))?;
    let boot = bridge::parse(
        request
            .boot
            .as_ref()
            .ok_or_else(|| bridge::error("missing worker bootstrap"))?
            .as_raw(),
    )?;
    let options = bridge::object(&[
        ("workerData", boot),
        ("env", bridge::object(&[])?),
        ("execArgv", bridge::array(&[])?),
        (
            "resourceLimits",
            bridge::from_json(&json!({"maxOldGenerationSizeMb":limits["maxOldGenerationSizeMb"]}))?,
        ),
        ("stdout", JsValue::TRUE),
        ("stderr", JsValue::TRUE),
    ])?;
    let arguments = bridge::array(&[bridge::api("filename")?, options])?;
    let worker = bridge::construct(&bridge::api("Worker")?, &arguments)?;
    let run = Rc::new(RefCell::new(Run {
        id,
        worker: worker.clone(),
        settled: false,
        callbacks: Vec::new(),
        compute_timer: JsValue::UNDEFINED,
        wall_timer: JsValue::UNDEFINED,
    }));
    host.borrow_mut().workers.insert(id, run.clone());
    let host_copy = host.clone();
    let run_copy = run.clone();
    let message = Closure::<dyn FnMut(JsValue) -> Result<(), JsValue>>::new(move |message| {
        worker_message(&host_copy, &run_copy, &message)
    })
    .into_js_value();
    listen(&run, &worker, "message", message)?;
    for stream in ["stdout", "stderr"] {
        let host_copy = host.clone();
        let callback =
            Closure::<dyn FnMut(JsValue) -> Result<(), JsValue>>::new(move |chunk: JsValue| {
                let text = bridge::text(&bridge::method(
                    &chunk,
                    "toString",
                    &[JsValue::from_str("utf8")],
                )?)?;
                emit(
                    &host_copy,
                    &OutputEvent {
                        run: id,
                        kind: "pipe",
                        text: &text,
                    },
                )
            })
            .into_js_value();
        listen(&run, &bridge::get(&worker, stream)?, "data", callback)?;
    }
    let host_copy = host.clone();
    let run_copy = run.clone();
    let error = Closure::<dyn FnMut(JsValue) -> Result<(), JsValue>>::new(move |error: JsValue| {
        finish(
            &host_copy,
            &run_copy,
            ProgramCompletion::WorkerError {
                message: bridge::message_text(&error),
            },
        )
    })
    .into_js_value();
    listen(&run, &worker, "error", error)?;
    let host_copy = host.clone();
    let run_copy = run.clone();
    let exit = Closure::<dyn FnMut(JsValue) -> Result<(), JsValue>>::new(move |code: JsValue| {
        finish(
            &host_copy,
            &run_copy,
            json!({"kind":"worker-exit", "code":code.as_f64().unwrap_or(0.0)}),
        )
    })
    .into_js_value();
    listen(&run, &worker, "exit", exit)?;
    install_limits(host, &run, &worker, limits)
}

fn install_limits(
    host: &Rc<RefCell<Host>>,
    run: &Rc<RefCell<Run>>,
    worker: &JsValue,
    limits: &Value,
) -> Result<(), JsValue> {
    let compute = limits["computeMs"]
        .as_f64()
        .ok_or_else(|| bridge::error("invalid compute limit"))?;
    let host_copy = host.clone();
    let run_copy = run.clone();
    let performance = bridge::get(worker, "performance")?;
    let callback = Closure::<dyn FnMut() -> Result<(), JsValue>>::new(move || {
        let utilization = bridge::method(&performance, "eventLoopUtilization", &[])?;
        if bridge::get(&utilization, "active")?.as_f64().unwrap_or(0.0) > compute {
            finish(&host_copy, &run_copy, json!({"kind":"compute-timeout"}))?;
        }
        Ok(())
    })
    .into_js_value();
    let compute_timer = timer("setInterval", &callback, 25.0)?;
    run.borrow_mut()
        .callbacks
        .push((JsValue::UNDEFINED, String::new(), callback));
    run.borrow_mut().compute_timer = compute_timer;
    let wall = limits["maxWallMs"]
        .as_f64()
        .ok_or_else(|| bridge::error("invalid wall limit"))?;
    let host_copy = host.clone();
    let run_copy = run.clone();
    let callback = Closure::<dyn FnMut() -> Result<(), JsValue>>::new(move || {
        finish(&host_copy, &run_copy, json!({"kind":"wall-timeout"}))
    })
    .into_js_value();
    let wall_timer = timer("setTimeout", &callback, wall)?;
    run.borrow_mut()
        .callbacks
        .push((JsValue::UNDEFINED, String::new(), callback));
    run.borrow_mut().wall_timer = wall_timer;
    Ok(())
}

fn receive(host: &Rc<RefCell<Host>>, request: &HostRequest) -> Result<(), JsValue> {
    match request.kind.as_str() {
        "start" => {
            if let Err(error) = start_run(host, request) {
                emit(
                    host,
                    &Terminal {
                        run: request.run.unwrap_or_default(),
                        kind: "terminal",
                        completion: ProgramCompletion::WorkerError {
                            message: bridge::message_text(&error),
                        },
                    },
                )?;
                emit(host, &json!({"run":request.run, "type":"stopped"}))?;
            }
        }
        "reply" => {
            let run = request
                .run
                .and_then(|id| host.borrow().workers.get(&id).cloned());
            if let Some(run) = run {
                if run.borrow().settled {
                    return Ok(());
                }
                let message = bridge::parse(
                    request
                        .message
                        .as_ref()
                        .ok_or_else(|| bridge::error("missing reply payload"))?
                        .as_raw(),
                )?;
                let call_id = request
                    .call_id
                    .as_deref()
                    .ok_or_else(|| bridge::error("missing reply ID"))?;
                let number = match call_id {
                    "NaN" => f64::NAN,
                    "Infinity" => f64::INFINITY,
                    "-Infinity" => f64::NEG_INFINITY,
                    value => value
                        .parse::<f64>()
                        .map_err(|_| bridge::error("invalid reply ID"))?,
                };
                bridge::define(
                    &message,
                    &JsValue::from_str("id"),
                    &JsValue::from_f64(number),
                    true,
                    true,
                )?;
                bridge::method(&run.borrow().worker, "postMessage", &[message])?;
            }
        }
        "stop" => {
            let run = request
                .run
                .and_then(|id| host.borrow().workers.get(&id).cloned());
            if let Some(run) = run {
                finish(
                    host,
                    &run,
                    request
                        .completion
                        .as_ref()
                        .ok_or_else(|| bridge::error("missing stop completion"))?,
                )?;
            }
        }
        _ => return Err(bridge::error("invalid native host command")),
    }
    Ok(())
}

pub(crate) fn start(apis: &JsValue) -> Result<(), JsValue> {
    let process = bridge::get(apis, "process")?;
    let argv = bridge::get(&process, "argv")?;
    let endpoint = bridge::get_key(&argv, &JsValue::from_str("2"))?;
    let connection = bridge::apply(
        &bridge::get(apis, "createConnection")?,
        &JsValue::UNDEFINED,
        &[endpoint],
    )?;
    bridge::method(&connection, "setEncoding", &[JsValue::from_str("utf8")])?;
    let host = Rc::new(RefCell::new(Host {
        connection: connection.clone(),
        input: String::new(),
        workers: HashMap::new(),
        callbacks: Vec::new(),
        intrinsics: Intrinsics::capture()?,
    }));
    HOST.with(|slot| *slot.borrow_mut() = Some(host.clone()));
    let host_copy = host.clone();
    let callback =
        Closure::<dyn FnMut(JsValue) -> Result<(), JsValue>>::new(move |chunk: JsValue| {
            host_copy.borrow_mut().input.push_str(
                &chunk
                    .as_string()
                    .ok_or_else(|| bridge::error("invalid native host input"))?,
            );
            loop {
                let line = {
                    let mut host = host_copy.borrow_mut();
                    let Some(index) = host.input.find('\n') else {
                        break;
                    };
                    let line = host.input[..index].to_owned();
                    host.input.drain(..=index);
                    line
                };
                let request = serde_json::from_str(&line)
                    .map_err(|error| bridge::error(&error.to_string()))?;
                receive(&host_copy, &request)?;
            }
            Ok(())
        })
        .into_js_value();
    bridge::method(
        &connection,
        "on",
        &[JsValue::from_str("data"), callback.clone()],
    )?;
    host.borrow_mut().callbacks.push(callback);
    let host_copy = host.clone();
    let callback = Closure::<dyn FnMut() -> Result<(), JsValue>>::new(move || {
        let runs = host_copy
            .borrow()
            .workers
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for run in runs {
            finish(
                &host_copy,
                &run,
                json!({"kind":"abort", "reason":"native runtime disconnected"}),
            )?;
        }
        Ok(())
    })
    .into_js_value();
    bridge::method(
        &connection,
        "on",
        &[JsValue::from_str("end"), callback.clone()],
    )?;
    bridge::method(
        &connection,
        "on",
        &[JsValue::from_str("error"), callback.clone()],
    )?;
    host.borrow_mut().callbacks.push(callback);
    emit(
        &host,
        &json!({"type":"ready", "protocol":1, "node":bridge::get(&process, "version")?.as_string().unwrap_or_default()}),
    )?;
    Ok(())
}
