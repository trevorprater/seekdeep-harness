//! Rust-owned V8 isolates for native Code Mode execution.

use std::{
    cell::RefCell,
    collections::HashSet,
    sync::{Arc, Once},
    time::{Duration, Instant},
};

use futures::{FutureExt as _, StreamExt as _, future::LocalBoxFuture, stream::FuturesUnordered};
use seekdeep_code_runtime::{CodeBindingFunction, CodeBindingNamespace, CodeRunFailureKind};
use seekdeep_llm::AbortSignal;
use serde_json::{Value, json};

use crate::{
    output_ledger::LogBuffer,
    snapshot::{SnapshotIntrinsics, snapshot_json},
    typescript::strip_typescript,
    watchdog::{Control, Watchdog},
    worker_globals::{WORKER_GLOBALS, binding_setup},
    worker_json::{decode_worker_json, encode_worker_json},
};

static INITIALIZE: Once = Once::new();

thread_local! {
    static RUN_STATE: RefCell<Option<RunState>> = const { RefCell::new(None) };
    // Separate storage permits heap interruption inside a callback using RunState.
    static TERMINATION: RefCell<Option<(v8::IsolateHandle, Arc<Control>)>> = const { RefCell::new(None) };
}

pub(crate) fn initialize_v8() {
    INITIALIZE.call_once(|| {
        let platform = v8::new_default_platform(0, false).make_shared();
        v8::V8::initialize_platform(platform);
        v8::V8::initialize();
    });
}

struct RunState {
    buffer: LogBuffer,
    logs: Vec<String>,
    bindings: Vec<CodeBindingNamespace>,
    answered_port_calls: HashSet<u64>,
    pending: Vec<LocalBoxFuture<'static, PendingReply>>,
    intrinsics: SnapshotIntrinsics,
}

enum ReplyTarget {
    Promise(v8::Global<v8::PromiseResolver>),
    Port(f64),
}

struct PendingReply {
    target: ReplyTarget,
    result: anyhow::Result<Option<Value>>,
}

/// Terminal engine state before the host-side output ledger is applied.
#[derive(Debug)]
pub(crate) enum EngineCompletion {
    Success(Option<Value>),
    Exception(String),
    InvalidOutput,
    OutputLimit,
    WorkerExit(i32),
    HeapLimit,
    ComputeTimeout,
    WallTimeout,
    Abort(Value),
    ForgedFailure(CodeRunFailureKind, String),
}

/// Complete worker-owned outcome.
#[derive(Debug)]
pub(crate) struct EngineOutcome {
    pub(crate) logs: Vec<String>,
    pub(crate) completion: EngineCompletion,
}

/// Execution bounds and cancellation observed within one worker.
pub(crate) struct EngineLimits {
    pub(crate) max_output_bytes: usize,
    pub(crate) max_old_generation_size_mb: f64,
    pub(crate) compute_ms: f64,
    pub(crate) max_wall_ms: f64,
    pub(crate) signal: AbortSignal,
}

struct RunStateGuard;

impl RunStateGuard {
    fn install(
        scope: &mut v8::PinScope,
        limits: &EngineLimits,
        bindings: Vec<CodeBindingNamespace>,
        control: Arc<Control>,
    ) -> anyhow::Result<Self> {
        let state = RunState {
            buffer: LogBuffer::new(limits.max_output_bytes),
            logs: Vec::new(),
            bindings,
            answered_port_calls: HashSet::new(),
            pending: Vec::new(),
            intrinsics: SnapshotIntrinsics::capture(scope)
                .ok_or_else(|| anyhow::anyhow!("cannot capture worker JSON intrinsics"))?,
        };
        RUN_STATE.with(|slot| {
            assert!(slot.borrow().is_none(), "one run per worker thread");
            *slot.borrow_mut() = Some(state);
        });
        TERMINATION.with(|slot| {
            *slot.borrow_mut() = Some((scope.thread_safe_handle(), control));
        });
        Ok(Self)
    }

    fn take(self, completion: EngineCompletion) -> EngineOutcome {
        let state = RUN_STATE.with(|slot| slot.borrow_mut().take().expect("active worker"));
        drop(self);
        EngineOutcome {
            logs: state.logs,
            completion,
        }
    }
}

impl Drop for RunStateGuard {
    fn drop(&mut self) {
        RUN_STATE.with(|slot| slot.borrow_mut().take());
        TERMINATION.with(|slot| slot.borrow_mut().take());
    }
}

extern "C" fn near_heap_limit(_: *mut std::ffi::c_void, current: usize, _: usize) -> usize {
    terminate(EngineCompletion::HeapLimit);
    // V8 needs finite emergency headroom to unwind termination without process OOM.
    current.saturating_add(16 * 1024 * 1024)
}

fn terminate(completion: EngineCompletion) {
    TERMINATION.with(|slot| {
        if let Some((isolate, control)) = slot.borrow().as_ref() {
            control.stop(completion);
            isolate.terminate_execution();
        }
    });
}

fn heap_bytes(mebibytes: f64) -> usize {
    // Node's worker resource limit has a two-MiB minimum.
    format!("{:.0}", (mebibytes.max(2.0) * 1_048_576.0).trunc())
        .parse()
        .unwrap_or(usize::MAX)
}

/// Drives one isolate; pending host calls are owned outside this worker.
pub(crate) async fn evaluate_program(
    program: &str,
    limits: EngineLimits,
    bindings: Vec<CodeBindingNamespace>,
) -> anyhow::Result<EngineOutcome> {
    let started = Instant::now();
    let stripped = strip_typescript(program)?;
    let javascript = stripped
        .replace(
            "import(\"node:worker_threads\")",
            "globalThis.__seekdeep_worker_threads_module__",
        )
        .replace(
            "import('node:worker_threads')",
            "globalThis.__seekdeep_worker_threads_module__",
        );
    let source = format!("'use strict';\n{javascript}\n__seekdeep_program__()");
    let setup = binding_setup(&bindings)?;
    initialize_v8();
    let params = v8::CreateParams::default()
        .set_max_old_generation_size_in_bytes(heap_bytes(limits.max_old_generation_size_mb));
    let mut isolate = v8::Isolate::new(params);
    isolate.set_microtasks_policy(v8::MicrotasksPolicy::Explicit);
    v8::scope!(let handle_scope, &mut isolate);
    let context = v8::Context::new(handle_scope, v8::ContextOptions::default());
    let scope = &mut v8::ContextScope::new(handle_scope, context);
    let control = Arc::new(Control::default());
    let guard = RunStateGuard::install(scope, &limits, bindings, control.clone())?;
    scope.add_near_heap_limit_callback(near_heap_limit, std::ptr::null_mut());
    let watchdog = Watchdog::new(control, scope.thread_safe_handle(), &limits, started)?;
    let completion = match install_worker_globals(scope, &setup) {
        Ok(()) => {
            watchdog.control.enter();
            let completion = run_program(scope, &source, &watchdog.control).await;
            watchdog.control.leave();
            completion
        }
        Err(message) => EngineCompletion::Exception(message),
    };
    let completion = watchdog.control.take_completion().unwrap_or(completion);
    Ok(guard.take(completion))
}

async fn run_program(
    scope: &mut v8::PinScope<'_, '_>,
    source: &str,
    control: &Control,
) -> EngineCompletion {
    let returned = match evaluate_script(scope, source) {
        Ok(value) => value,
        Err(message) => return EngineCompletion::Exception(message),
    };
    let Ok(promise) = v8::Local::<v8::Promise>::try_from(returned) else {
        return EngineCompletion::Exception("program wrapper did not return a promise".to_owned());
    };
    let mut pending = FuturesUnordered::new();
    loop {
        scope.perform_microtask_checkpoint();
        if let Some(completion) = control.take_completion() {
            return completion;
        }
        match promise.state() {
            v8::PromiseState::Fulfilled => {
                let value = promise.result(scope);
                return if value.is_undefined() {
                    EngineCompletion::Success(None)
                } else {
                    snapshot(scope, value).map_or(EngineCompletion::InvalidOutput, |value| {
                        EngineCompletion::Success(Some(value))
                    })
                };
            }
            v8::PromiseState::Rejected => {
                return EngineCompletion::Exception(render_rejection(scope, promise.result(scope)));
            }
            v8::PromiseState::Pending => {}
        }
        pending.extend(RUN_STATE.with(|slot| {
            std::mem::take(&mut slot.borrow_mut().as_mut().expect("active worker").pending)
        }));
        control.leave();
        let reply = tokio::select! {
            reply = pending.next(), if !pending.is_empty() => reply,
            () = control.stopped.notified() => None,
        };
        control.enter();
        if let Some(completion) = control.take_completion() {
            return completion;
        }
        if let Some(reply) = reply
            && let Err(message) = deliver_reply(scope, reply)
        {
            return EngineCompletion::Exception(message);
        }
    }
}

fn evaluate_script<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    source: &str,
) -> Result<v8::Local<'s, v8::Value>, String> {
    v8::tc_scope!(let caught, scope);
    let source = v8::String::new(caught, source).ok_or("worker source allocation failed")?;
    v8::Script::compile(caught, source, None)
        .and_then(|script| script.run(caught))
        .ok_or_else(|| {
            caught.exception().map_or_else(
                || "worker execution terminated".to_owned(),
                |error| render_rejection(caught, error),
            )
        })
}

fn install_worker_globals(scope: &mut v8::PinScope, setup: &str) -> Result<(), String> {
    let global = scope.get_current_context().global(scope);
    for (id, name) in [
        "__seekdeep_log__",
        "__seekdeep_exit__",
        "__seekdeep_sleep__",
        "__seekdeep_call__",
        "__seekdeep_port_control__",
        "__seekdeep_port_call__",
    ]
    .into_iter()
    .enumerate()
    {
        let data = v8::Integer::new(scope, i32::try_from(id).expect("six callbacks"));
        let callback = v8::Function::builder(native_callback)
            .data(data.into())
            .build(scope)
            .ok_or("cannot create worker callback")?;
        let key = v8::String::new(scope, name).ok_or("cannot create worker callback name")?;
        global
            .set(scope, key.into(), callback.into())
            .ok_or("cannot install worker callback")?;
    }
    evaluate_script(scope, WORKER_GLOBALS)?;
    evaluate_script(scope, setup)?;
    Ok(())
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "V8 owns the callback signature"
)]
fn native_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut returned: v8::ReturnValue<'s>,
) {
    let result = match args.data().int32_value(scope) {
        Some(0) => Ok(capture_log(scope, &args)),
        Some(1) => {
            if let Some(code) = args.get(0).int32_value(scope) {
                terminate(EngineCompletion::WorkerExit(code));
            }
            Ok(v8::undefined(scope).into())
        }
        Some(2) => sleep(scope, &args),
        Some(3) => binding_call(scope, &args),
        Some(4) => Ok(port_control(scope, args.get(0))),
        Some(5) => Ok(port_call(scope, args.get(0))),
        _ => Err("unknown worker callback".to_owned()),
    };
    match result {
        Ok(value) => returned.set(value),
        Err(message) => {
            if let Some(message) = v8::String::new(scope, &message) {
                let error = v8::Exception::error(scope, message);
                scope.throw_exception(error);
            }
        }
    }
}

fn binding_function(global: &str, name: &str) -> Option<CodeBindingFunction> {
    RUN_STATE.with(|slot| {
        slot.borrow()
            .as_ref()
            .expect("active worker")
            .bindings
            .iter()
            .find(|namespace| namespace.global == global)
            .and_then(|namespace| namespace.functions.get(name))
            .cloned()
    })
}

fn unknown_binding(global: &str, name: &str) -> String {
    let qualified =
        serde_json::to_string(&format!("{global}.{name}")).expect("a string is JSON serializable");
    format!("unknown binding {qualified}")
}

fn snapshot<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    value: v8::Local<'s, v8::Value>,
) -> Option<Value> {
    let intrinsics = RUN_STATE.with(|slot| {
        slot.borrow()
            .as_ref()
            .expect("active worker")
            .intrinsics
            .clone()
    });
    snapshot_json(scope, value, &intrinsics)
}

fn binding_call<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: &v8::FunctionCallbackArguments<'s>,
) -> Result<v8::Local<'s, v8::Value>, String> {
    let global = string_value(scope, args.get(0)).ok_or("binding global must be a string")?;
    let name = string_value(scope, args.get(1)).ok_or("binding name must be a string")?;
    let argument = snapshot(scope, args.get(2)).ok_or("binding arguments must be lossless JSON")?;
    let function =
        binding_function(&global, &name).ok_or_else(|| unknown_binding(&global, &name))?;
    let operation = function(argument);
    queue_promise(
        scope,
        async move { operation.await.map(Some) }.boxed_local(),
    )
}

fn queue_promise<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    operation: LocalBoxFuture<'static, anyhow::Result<Option<Value>>>,
) -> Result<v8::Local<'s, v8::Value>, String> {
    let resolver = v8::PromiseResolver::new(scope).ok_or("cannot create worker promise")?;
    let promise = resolver.get_promise(scope);
    let target = ReplyTarget::Promise(v8::Global::new(scope, resolver));
    queue_reply(target, operation);
    Ok(promise.into())
}

fn queue_reply(
    target: ReplyTarget,
    operation: LocalBoxFuture<'static, anyhow::Result<Option<Value>>>,
) {
    RUN_STATE.with(|slot| {
        slot.borrow_mut()
            .as_mut()
            .expect("active worker")
            .pending
            .push(
                async move {
                    PendingReply {
                        target,
                        result: operation.await,
                    }
                }
                .boxed_local(),
            );
    });
}

fn sleep<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: &v8::FunctionCallbackArguments,
) -> Result<v8::Local<'s, v8::Value>, String> {
    let Some(delay) = args.get(0).number_value(scope) else {
        return Ok(v8::undefined(scope).into());
    };
    let delay = if delay.is_finite() && (1.0..=2_147_483_647.0).contains(&delay) {
        delay.trunc()
    } else {
        1.0
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs_f64(delay / 1_000.0);
    queue_promise(
        scope,
        async move {
            tokio::time::sleep_until(deadline).await;
            Ok(None)
        }
        .boxed_local(),
    )
}

fn port_control<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    raw: v8::Local<v8::Value>,
) -> v8::Local<'s, v8::Value> {
    let mut handled = true;
    if let Ok(message) = v8::Local::<v8::Object>::try_from(raw) {
        match property_string(scope, message, "type").as_deref() {
            Some("log") => {
                if let Some(text) = property_string(scope, message, "text") {
                    push_log(&text);
                }
            }
            Some("output-limit") => terminate(EngineCompletion::OutputLimit),
            Some("done") => {
                if let Some(completion) = forged_done(scope, message) {
                    terminate(completion);
                }
            }
            Some("call") => {
                if let (Some(id), Some(_), Some(_)) = (
                    property(scope, message, "id")
                        .filter(|value| value.is_number())
                        .and_then(|value| value.number_value(scope)),
                    property_string(scope, message, "global"),
                    property_string(scope, message, "name"),
                ) {
                    handled = RUN_STATE.with(|slot| {
                        !slot
                            .borrow_mut()
                            .as_mut()
                            .expect("active worker")
                            .answered_port_calls
                            .insert(number_key(id))
                    });
                }
            }
            _ => {}
        }
    }
    v8::Boolean::new(scope, handled).into()
}

fn forged_done(
    scope: &mut v8::PinScope,
    message: v8::Local<v8::Object>,
) -> Option<EngineCompletion> {
    let error = property(scope, message, "error")?;
    if !error.is_undefined() {
        let error = v8::Local::<v8::Object>::try_from(error).ok()?;
        let kind = match property_string(scope, error, "kind")?.as_str() {
            "exception" => CodeRunFailureKind::Exception,
            "invalid-output" => CodeRunFailureKind::InvalidOutput,
            "output-limit" => CodeRunFailureKind::OutputLimit,
            _ => return None,
        };
        return Some(EngineCompletion::ForgedFailure(
            kind,
            property_string(scope, error, "message")?,
        ));
    }
    let value = property(scope, message, "value")?;
    if value.is_undefined() {
        return Some(EngineCompletion::Success(None));
    }
    Some(
        snapshot(scope, value)
            .and_then(|wire| decode_worker_json(&wire))
            .map_or(EngineCompletion::InvalidOutput, |value| {
                EngineCompletion::Success(Some(value))
            }),
    )
}

fn port_call<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    raw: v8::Local<v8::Value>,
) -> v8::Local<'s, v8::Value> {
    if let Ok(message) = v8::Local::<v8::Object>::try_from(raw)
        && let (Some(id), Some(global), Some(name)) = (
            property(scope, message, "id")
                .filter(|value| value.is_number())
                .and_then(|value| value.number_value(scope)),
            property_string(scope, message, "global"),
            property_string(scope, message, "name"),
        )
    {
        let argument = property(scope, message, "args")
            .and_then(|value| snapshot(scope, value))
            .and_then(|wire| decode_worker_json(&wire));
        let function = binding_function(&global, &name);
        let operation = match (function, argument) {
            (None, _) => {
                futures::future::ready(Err(anyhow::Error::msg(unknown_binding(&global, &name))))
                    .boxed_local()
            }
            (Some(_), None) => futures::future::ready(Err(anyhow::anyhow!(
                "binding arguments must be lossless JSON"
            )))
            .boxed_local(),
            (Some(function), Some(argument)) => function(argument)
                .map(|result| result.map(Some))
                .boxed_local(),
        };
        queue_reply(ReplyTarget::Port(id), operation);
    }
    v8::undefined(scope).into()
}

fn deliver_reply(scope: &mut v8::PinScope, reply: PendingReply) -> Result<(), String> {
    match reply.target {
        ReplyTarget::Promise(resolver) => {
            let resolver = v8::Local::new(scope, resolver);
            match reply.result {
                Ok(value) => {
                    let value = match value {
                        Some(value) => from_json(scope, &encode_worker_json(&value))?,
                        None => v8::undefined(scope).into(),
                    };
                    resolver
                        .resolve(scope, value)
                        .ok_or("cannot resolve worker promise")?;
                }
                Err(error) => {
                    let message = v8::String::new(scope, &error.to_string())
                        .ok_or("cannot allocate binding error")?;
                    let error = v8::Exception::error(scope, message);
                    resolver
                        .reject(scope, error)
                        .ok_or("cannot reject worker promise")?;
                }
            }
        }
        ReplyTarget::Port(id) => {
            let reply = match reply.result {
                Ok(Some(value)) => {
                    json!({ "type": "reply", "id": null, "ok": true, "value": encode_worker_json(&value) })
                }
                Ok(None) => return Err("missing port binding resolution".to_owned()),
                Err(error) => {
                    json!({ "type": "reply", "id": null, "ok": false, "message": error.to_string() })
                }
            };
            let reply = from_json(scope, &reply)?;
            let object =
                v8::Local::<v8::Object>::try_from(reply).map_err(|_| "invalid reply object")?;
            let key = v8::String::new(scope, "id").ok_or("cannot allocate reply id")?;
            let id = v8::Number::new(scope, id);
            object
                .set(scope, key.into(), id.into())
                .ok_or("cannot set reply id")?;
            let global = scope.get_current_context().global(scope);
            let dispatch = property(scope, global, "__seekdeep_deliver_reply__")
                .and_then(|value| v8::Local::<v8::Function>::try_from(value).ok())
                .ok_or("missing worker reply dispatcher")?;
            let receiver = v8::undefined(scope);
            dispatch
                .call(scope, receiver.into(), &[reply])
                .ok_or("worker reply dispatcher threw")?;
        }
    }
    Ok(())
}

fn from_json<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    value: &Value,
) -> Result<v8::Local<'s, v8::Value>, String> {
    let json = serde_json::to_string(value).map_err(|error| error.to_string())?;
    let json = v8::String::new(scope, &json).ok_or("cannot allocate binding JSON")?;
    v8::json::parse(scope, json).ok_or_else(|| "cannot decode binding JSON".to_owned())
}

fn property<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    object: v8::Local<v8::Object>,
    name: &str,
) -> Option<v8::Local<'s, v8::Value>> {
    let key = v8::String::new(scope, name)?;
    object.get(scope, key.into())
}

fn property_string(
    scope: &mut v8::PinScope,
    object: v8::Local<v8::Object>,
    name: &str,
) -> Option<String> {
    let value = property(scope, object, name)?;
    string_value(scope, value)
}

fn string_value(scope: &mut v8::PinScope, value: v8::Local<v8::Value>) -> Option<String> {
    v8::Local::<v8::String>::try_from(value)
        .ok()
        .map(|value| value.to_rust_string_lossy(scope))
}

fn number_key(value: f64) -> u64 {
    if value.is_nan() {
        f64::NAN.to_bits()
    } else if value == 0.0 {
        0
    } else {
        value.to_bits()
    }
}

fn push_log(text: &str) {
    let limited = RUN_STATE.with(|slot| {
        let mut state = slot.borrow_mut();
        let state = state.as_mut().expect("active worker");
        let pushed = state.buffer.push(text);
        if let Some(emitted) = pushed.emitted {
            state.logs.push(emitted);
        }
        pushed.limit_reached
    });
    if limited {
        terminate(EngineCompletion::OutputLimit);
    }
}

fn capture_log<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: &v8::FunctionCallbackArguments<'s>,
) -> v8::Local<'s, v8::Value> {
    let mut rendered = Vec::new();
    for index in 0..args.length() {
        let value = args.get(index);
        rendered.push(
            string_value(scope, value)
                .unwrap_or_else(|| inspect_value(scope, value, 0, &mut Vec::new())),
        );
    }
    push_log(&rendered.join(" "));
    v8::undefined(scope).into()
}

fn render_rejection(scope: &mut v8::PinScope, value: v8::Local<v8::Value>) -> String {
    v8::tc_scope!(let caught, scope);
    let detail = if value.is_native_error() {
        v8::Local::<v8::Object>::try_from(value)
            .ok()
            .and_then(|object| property(caught, object, "stack"))
            .filter(|value| !value.is_null_or_undefined())
            .unwrap_or(value)
    } else {
        value
    };
    detail.to_string(caught).map_or_else(
        || "program threw an unrenderable value".to_owned(),
        |message| message.to_rust_string_lossy(caught),
    )
}

fn inspect_value<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    value: v8::Local<'s, v8::Value>,
    depth: usize,
    active: &mut Vec<v8::Local<'s, v8::Object>>,
) -> String {
    if let Some(text) = string_value(scope, value) {
        return quote_inspect_string(&text);
    }
    if value.is_function() {
        return "[Function]".to_owned();
    }
    if let Ok(symbol) = v8::Local::<v8::Symbol>::try_from(value) {
        let description = string_value(scope, symbol.description(scope)).unwrap_or_default();
        return format!("Symbol({description})");
    }
    if value.is_number()
        && value
            .number_value(scope)
            .is_some_and(|number| number == 0.0 && number.is_sign_negative())
    {
        return "-0".to_owned();
    }
    let Ok(object) = v8::Local::<v8::Object>::try_from(value) else {
        let text = value.to_string(scope).map_or_else(
            || "undefined".to_owned(),
            |value| value.to_rust_string_lossy(scope),
        );
        return if value.is_big_int() {
            format!("{text}n")
        } else {
            text
        };
    };
    if depth >= 4 {
        return if object.is_array() {
            "[Array]"
        } else {
            "[Object]"
        }
        .to_owned();
    }
    if active
        .iter()
        .any(|entry| entry.strict_equals(object.into()))
    {
        return "[Circular]".to_owned();
    }
    active.push(object);
    let mut entries = Vec::new();
    let rendered = if let Ok(array) = v8::Local::<v8::Array>::try_from(object) {
        for index in 0..array.length().min(100) {
            if let Some(value) = array.get_index(scope, index) {
                entries.push(inspect_value(scope, value, depth + 1, active));
            }
        }
        if array.length() > 100 {
            entries.push(format!("... {} more items", array.length() - 100));
        }
        format!("[ {} ]", entries.join(", "))
    } else {
        if let Some(keys) = object.get_own_property_names(
            scope,
            v8::GetPropertyNamesArgs {
                key_conversion: v8::KeyConversionMode::ConvertToString,
                ..v8::GetPropertyNamesArgs::default()
            },
        ) {
            for index in 0..keys.length().min(100) {
                if let Some(key) = keys.get_index(scope, index)
                    && let Some(label) = string_value(scope, key)
                    && let Some(value) = object.get(scope, key)
                {
                    entries.push(format!(
                        "{label}: {}",
                        inspect_value(scope, value, depth + 1, active)
                    ));
                }
            }
        }
        format!("{{ {} }}", entries.join(", "))
    };
    active.pop();
    rendered
}

fn quote_inspect_string(value: &str) -> String {
    let mut output = String::with_capacity(value.len().min(10_000) + 2);
    output.push('\'');
    for character in value.chars().take(10_000) {
        match character {
            '\'' => output.push_str("\\'"),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character => output.push(character),
        }
    }
    output.push('\'');
    output
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod engine_tests;
