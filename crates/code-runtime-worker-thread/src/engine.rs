//! Boa execution-engine integration and captured worker globals.

use std::{cell::RefCell, collections::HashSet, fmt::Write as _, rc::Rc, time::Duration};

use boa_engine::{
    Context, JsNativeError, JsResult, JsValue, NativeFunction, Source,
    builtins::promise::PromiseState, context::ContextBuilder, job::JobExecutor, js_string,
    object::builtins::JsPromise, script::Script, value::JsVariant,
};
use cpu_time::ThreadTime;
use seekdeep_code_runtime::{CodeBindingNamespace, CodeRunFailureKind};
use seekdeep_llm::AbortSignal;
use serde_json::Value;

use crate::{
    job_executor::WorkerJobExecutor,
    output_ledger::LogBuffer,
    snapshot::snapshot_json,
    typescript::strip_typescript,
    worker_json::{decode_worker_json, encode_worker_json},
};

const OUTPUT_LIMIT_SENTINEL: &str = "__SEEKDEEP_OUTPUT_LIMIT__";
const PROCESS_EXIT_SENTINEL: &str = "__SEEKDEEP_PROCESS_EXIT__";
const PORT_TERMINAL_SENTINEL: &str = "__SEEKDEEP_PORT_TERMINAL__";

thread_local! {
    static RUN_STATE: RefCell<Option<RunState>> = const { RefCell::new(None) };
}

struct RunState {
    buffer: LogBuffer,
    logs: Vec<String>,
    output_limit: bool,
    process_exit: Option<i32>,
    bindings: Vec<CodeBindingNamespace>,
    answered_port_calls: HashSet<u64>,
    terminal_override: Option<EngineCompletion>,
}

impl RunState {
    fn new(max_output_bytes: usize, bindings: Vec<CodeBindingNamespace>) -> Self {
        Self {
            buffer: LogBuffer::new(max_output_bytes),
            logs: Vec::new(),
            output_limit: false,
            process_exit: None,
            bindings,
            answered_port_calls: HashSet::new(),
            terminal_override: None,
        }
    }
}

/// Terminal engine state before the host-side output ledger is applied.
#[derive(Debug)]
pub(crate) enum EngineCompletion {
    /// Program completed with an absent or lossless JSON value.
    Success(Option<Value>),
    /// Program threw or rejected.
    Exception(String),
    /// Program returned a value outside the strict JSON boundary.
    InvalidOutput,
    /// Captured worker output crossed its cap.
    OutputLimit,
    /// The worker requested an early process exit.
    WorkerExit(i32),
    /// Measured worker busy time crossed the configured budget.
    ComputeTimeout,
    /// Absolute wall time crossed the configured ceiling.
    WallTimeout,
    /// Caller or runtime lifecycle cancellation won.
    Abort(Value),
    /// Valid hostile port traffic supplied a terminal error.
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
    pub(crate) compute_ms: f64,
    pub(crate) max_wall_ms: f64,
    pub(crate) signal: AbortSignal,
}

struct RunStateGuard;

impl RunStateGuard {
    fn install(max_output_bytes: usize, bindings: Vec<CodeBindingNamespace>) -> Self {
        RUN_STATE.with(|state| {
            assert!(state.borrow().is_none(), "one run per worker thread");
            *state.borrow_mut() = Some(RunState::new(max_output_bytes, bindings));
        });
        Self
    }

    fn take(self, completion: EngineCompletion) -> EngineOutcome {
        let state = RUN_STATE.with(|state| state.borrow_mut().take());
        std::mem::forget(self);
        let state = state.expect("run state remains installed");
        let completion = if state.output_limit {
            EngineCompletion::OutputLimit
        } else if let Some(code) = state.process_exit {
            EngineCompletion::WorkerExit(code)
        } else if let Some(terminal) = state.terminal_override {
            terminal
        } else {
            completion
        };
        EngineOutcome {
            logs: state.logs,
            completion,
        }
    }
}

impl Drop for RunStateGuard {
    fn drop(&mut self) {
        RUN_STATE.with(|state| {
            state.borrow_mut().take();
        });
    }
}

pub(crate) fn ensure_worker_not_terminal() -> JsResult<()> {
    let sentinel = RUN_STATE.with(|state| {
        let state = state.borrow();
        let state = state.as_ref()?;
        if state.output_limit {
            Some(OUTPUT_LIMIT_SENTINEL)
        } else if state.process_exit.is_some() {
            Some(PROCESS_EXIT_SENTINEL)
        } else if state.terminal_override.is_some() {
            Some(PORT_TERMINAL_SENTINEL)
        } else {
            None
        }
    });
    sentinel.map_or(Ok(()), |sentinel| {
        Err(JsNativeError::error().with_message(sentinel).into())
    })
}

/// Runs one isolated program and all of its Boa jobs.
pub(crate) async fn evaluate_program(
    program: &str,
    limits: EngineLimits,
    bindings: Vec<CodeBindingNamespace>,
) -> anyhow::Result<EngineOutcome> {
    let wall_deadline =
        tokio::time::Instant::now() + Duration::from_secs_f64(limits.max_wall_ms / 1_000.0);
    let stripped = strip_typescript(program)?;
    let javascript = rewrite_worker_imports(&stripped);
    let source = format!("{javascript}\n__seekdeep_program__()");
    let executor = Rc::new(WorkerJobExecutor::new());
    let mut context = ContextBuilder::new()
        .job_executor(executor.clone())
        .build()
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    install_worker_globals(&mut context, &bindings)?;
    context
        .runtime_limits_mut()
        .set_loop_iteration_limit(loop_iteration_limit(limits.compute_ms));
    let guard = RunStateGuard::install(limits.max_output_bytes, bindings);
    let cpu_start = ThreadTime::now();
    let script = Script::parse(Source::from_bytes(&source), None, &mut context)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let returned = {
        let evaluation = script.evaluate_async_with_budget(&mut context, 256);
        tokio::pin!(evaluation);
        match tokio::select! {
            result = &mut evaluation => Ok(result),
            completion = stop_signal(&limits.signal, cpu_start, limits.compute_ms, wall_deadline) => Err(completion),
        } {
            Err(completion) => return Ok(guard.take(completion)),
            Ok(value) => value,
        }
    };
    let returned = match returned {
        Ok(value) => value,
        Err(error) => return Ok(guard.take(completion_for_error(&error))),
    };
    let Some(promise) = returned
        .as_object()
        .and_then(|object| JsPromise::from_object(object).ok())
    else {
        return Ok(guard.take(EngineCompletion::Exception(
            "program wrapper did not return a promise".to_owned(),
        )));
    };
    let jobs = {
        let context = RefCell::new(&mut context);
        let jobs = executor.run_jobs_async(&context);
        tokio::pin!(jobs);
        tokio::select! {
            result = &mut jobs => Ok(result),
            completion = stop_signal(&limits.signal, cpu_start, limits.compute_ms, wall_deadline) => Err(completion),
        }
    };
    let jobs = match jobs {
        Ok(jobs) => jobs,
        Err(completion) => return Ok(guard.take(completion)),
    };
    if let Err(error) = jobs {
        return Ok(guard.take(completion_for_error(&error)));
    }
    let completion = match promise.state() {
        PromiseState::Fulfilled(value) if value.is_undefined() => EngineCompletion::Success(None),
        PromiseState::Fulfilled(value) => snapshot_json(&value, &mut context)
            .map_or(EngineCompletion::InvalidOutput, |value| {
                EngineCompletion::Success(Some(value))
            }),
        PromiseState::Rejected(error) => {
            EngineCompletion::Exception(render_rejection(&error, &mut context))
        }
        PromiseState::Pending => {
            return Ok(guard.take(
                stop_signal(&limits.signal, cpu_start, limits.compute_ms, wall_deadline).await,
            ));
        }
    };
    Ok(guard.take(completion))
}

fn rewrite_worker_imports(javascript: &str) -> String {
    javascript
        .replace(
            "import(\"node:worker_threads\")",
            "globalThis.__seekdeep_worker_threads_module__",
        )
        .replace(
            "import('node:worker_threads')",
            "globalThis.__seekdeep_worker_threads_module__",
        )
}

fn loop_iteration_limit(compute_ms: f64) -> u64 {
    format!("{:.0}", (compute_ms * 100_000.0).max(1.0))
        .parse()
        .unwrap_or(u64::MAX)
}

fn completion_for_error(error: &boa_engine::JsError) -> EngineCompletion {
    if error.as_native().is_some_and(|native| {
        native
            .message()
            .starts_with("Maximum loop iteration limit ")
    }) {
        EngineCompletion::ComputeTimeout
    } else {
        EngineCompletion::Exception(error.to_string())
    }
}

async fn stop_signal(
    signal: &AbortSignal,
    cpu_start: ThreadTime,
    compute_ms: f64,
    wall_deadline: tokio::time::Instant,
) -> EngineCompletion {
    tokio::select! {
        () = signal.cancelled() => EngineCompletion::Abort(signal.reason().unwrap_or(Value::Null)),
        () = compute_exhausted(cpu_start, compute_ms) => EngineCompletion::ComputeTimeout,
        () = tokio::time::sleep_until(wall_deadline) => EngineCompletion::WallTimeout,
    }
}

async fn compute_exhausted(start: ThreadTime, compute_ms: f64) {
    loop {
        tokio::time::sleep(Duration::from_millis(5)).await;
        if start.elapsed().as_secs_f64() * 1_000.0 > compute_ms {
            return;
        }
    }
}

fn install_worker_globals(
    context: &mut Context,
    bindings: &[CodeBindingNamespace],
) -> anyhow::Result<()> {
    context
        .register_global_builtin_callable(
            js_string!("__seekdeep_log__"),
            0,
            NativeFunction::from_fn_ptr(capture_log),
        )
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    context
        .register_global_builtin_callable(
            js_string!("__seekdeep_port_control__"),
            1,
            NativeFunction::from_fn_ptr(port_control),
        )
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    context
        .register_global_builtin_callable(
            js_string!("__seekdeep_port_call__"),
            1,
            NativeFunction::from_async_fn(port_call),
        )
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    context
        .register_global_builtin_callable(
            js_string!("__seekdeep_call__"),
            3,
            NativeFunction::from_async_fn(binding_call),
        )
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    context
        .register_global_builtin_callable(
            js_string!("__seekdeep_exit__"),
            1,
            NativeFunction::from_fn_ptr(process_exit),
        )
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    context
        .register_global_builtin_callable(
            js_string!("__seekdeep_sleep__"),
            1,
            NativeFunction::from_async_fn(sleep),
        )
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    context
        .eval(Source::from_bytes(WORKER_GLOBALS))
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    context
        .eval(Source::from_bytes(&binding_setup(bindings)?))
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    Ok(())
}

const WORKER_GLOBALS: &str = r"
(() => {
  const capturedObject = Object;
  const capturedPromise = Promise;
  const capturedString = String;
  const log = globalThis.__seekdeep_log__;
  const exit = globalThis.__seekdeep_exit__;
  const sleep = globalThis.__seekdeep_sleep__;
  const portControl = globalThis.__seekdeep_port_control__;
  const portCall = globalThis.__seekdeep_port_call__;
  const defineData = (target, key, value, enumerable = true, configurable = true) => {
    const descriptor = capturedObject.create(null);
    descriptor.configurable = configurable;
    descriptor.enumerable = enumerable;
    descriptor.writable = false;
    descriptor.value = value;
    capturedObject.defineProperty(target, key, descriptor);
  };
  const listeners = [];
  const deliverReply = message => {
    for (let index = 0; index < listeners.length; index++) {
      const listener = listeners[index];
      if (typeof listener === 'function') listener(message);
    }
  };
  defineData(globalThis, '__seekdeep_deliver_reply__', deliverReply, false, false);
  const parentPort = capturedObject.create(null);
  defineData(parentPort, 'postMessage', message => {
    if (!portControl(message)) void portCall(message);
  });
  defineData(parentPort, 'on', (event, listener) => {
    if (event === 'message' && typeof listener === 'function') defineData(listeners, listeners.length, listener);
    return parentPort;
  });
  defineData(parentPort, 'off', (event, listener) => {
    if (event === 'message') {
      for (let index = 0; index < listeners.length; index++) {
        if (listeners[index] === listener) defineData(listeners, index, undefined);
      }
    }
    return parentPort;
  });
  const workerThreadsModule = capturedPromise.resolve({ parentPort });
  defineData(globalThis, '__seekdeep_worker_threads_module__', workerThreadsModule, false, false);
  const consoleShim = capturedObject.create(null);
  for (const level of ['log', 'info', 'warn', 'error', 'debug']) {
    capturedObject.defineProperty(consoleShim, level, {
      enumerable: true,
      value: (...args) => log(...args),
    });
  }
  const streamPrototype = capturedObject.create(null);
  const write = function(chunk, ...rest) {
    log(typeof chunk === 'string' ? chunk : String(chunk));
    const callback = rest.find(value => typeof value === 'function');
    if (callback) capturedPromise.resolve().then(() => callback(null));
    return true;
  };
  capturedObject.defineProperty(streamPrototype, 'write', { value: write });
  const makeStream = () => {
    const stream = capturedObject.create(streamPrototype);
    capturedObject.defineProperty(stream, 'write', { value: write, writable: true });
    return stream;
  };
  const processShim = capturedObject.create(null);
  capturedObject.defineProperties(processShim, {
    env: { enumerable: true, value: capturedObject.create(null) },
    stdout: { enumerable: true, value: makeStream() },
    stderr: { enumerable: true, value: makeStream() },
    exit: { enumerable: true, value: code => exit(code) },
  });
  const bufferShim = capturedObject.create(null);
  capturedObject.defineProperties(bufferShim, {
    byteLength: { writable: true, value: value => capturedString(value).length },
    alloc: { writable: true, value: size => new Uint8Array(size) },
    from: { writable: true, value: value => value },
  });
  let nextTimer = 1;
  const cancelledTimers = new Set();
  const setTimeoutShim = (callback, delay = 0, ...args) => {
    const id = nextTimer++;
    sleep(delay).then(() => {
      if (!cancelledTimers.delete(id)) callback(...args);
    });
    return id;
  };
  const clearTimeoutShim = id => { cancelledTimers.add(id); };
  capturedObject.defineProperties(globalThis, {
    console: { configurable: true, writable: true, value: consoleShim },
    process: { configurable: true, writable: true, value: processShim },
    Buffer: { configurable: true, writable: true, value: bufferShim },
    setTimeout: { configurable: true, writable: true, value: setTimeoutShim },
    clearTimeout: { configurable: true, writable: true, value: clearTimeoutShim },
    queueMicrotask: { configurable: true, writable: true, value: callback => capturedPromise.resolve().then(callback) },
  });
  delete globalThis.__seekdeep_log__;
  delete globalThis.__seekdeep_exit__;
  delete globalThis.__seekdeep_sleep__;
  delete globalThis.__seekdeep_port_control__;
  delete globalThis.__seekdeep_port_call__;
})();
";

const BINDING_SETUP_PREFIX: &str = r"
(() => {
  const call = globalThis.__seekdeep_call__;
  const capturedArray = Array;
  const capturedError = Error;
  const capturedObject = Object;
  const capturedString = String;
  const capturedCreate = capturedObject.create;
  const capturedDefine = capturedObject.defineProperty;
  const capturedObjectPrototype = capturedObject.prototype;
  const defineData = (target, key, value, enumerable = true) => {
    const descriptor = capturedCreate(null);
    descriptor.configurable = true;
    descriptor.enumerable = enumerable;
    descriptor.writable = true;
    descriptor.value = value;
    capturedDefine(target, key, descriptor);
  };
  const setLength = (target, value) => {
    const descriptor = capturedCreate(null);
    descriptor.value = value;
    capturedDefine(target, 'length', descriptor);
  };
  const append = (target, value) => { defineData(target, target.length, value); };
  const decode = wire => {
    const frames = [];
    let root;
    let rootSet = false;
    const attach = value => {
      const parent = frames.length === 0 ? undefined : frames[frames.length - 1];
      if (parent === undefined) {
        if (rootSet) throw new capturedError('invalid binding wire');
        root = value;
        rootSet = true;
        return;
      }
      if (parent.kind === 'array') defineData(parent.target, parent.index, value);
      else defineData(parent.target, parent.keys[parent.index], value);
      parent.index += 1;
    };
    for (let tokenIndex = 0; tokenIndex < wire.length; tokenIndex++) {
      const token = wire[tokenIndex];
      let value;
      let frame;
      if (token === null || typeof token === 'boolean' || typeof token === 'number' || typeof token === 'string') {
        value = token;
      } else if (token.kind === 'array') {
        value = [];
        if (token.length > 0) frame = { kind: 'array', target: value, length: token.length, index: 0 };
      } else {
        value = capturedCreate(capturedObjectPrototype);
        if (token.keys.length > 0) frame = { kind: 'object', target: value, keys: token.keys, index: 0 };
      }
      attach(value);
      if (frame !== undefined) append(frames, frame);
      while (frames.length > 0) {
        const current = frames[frames.length - 1];
        const length = current.kind === 'array' ? current.length : current.keys.length;
        if (current.index < length) break;
        setLength(frames, frames.length - 1);
      }
    }
    if (!rootSet || frames.length !== 0) throw new capturedError('invalid binding wire');
    return root;
  };
";

fn binding_setup(bindings: &[CodeBindingNamespace]) -> anyhow::Result<String> {
    let mut source = String::from(BINDING_SETUP_PREFIX);

    for (namespace_index, namespace) in bindings.iter().enumerate() {
        let global = serde_json::to_string(&namespace.global)?;
        let error_variable = if let Some(descriptor) = &namespace.error_class {
            let variable = format!("BindingError{namespace_index}");
            let class_name = &descriptor.name;
            let class_name_json = serde_json::to_string(class_name)?;
            let member_json = serde_json::to_string(&descriptor.member_name_property)?;
            write!(
                &mut source,
                "  const {variable} = class {class_name} extends capturedError {{\n    constructor(memberName, message) {{\n      super(message);\n      defineData(this, 'name', {class_name_json});\n      defineData(this, {member_json}, memberName);\n    }}\n  }};\n  defineData(globalThis, {class_name_json}, {variable}, false);\n"
            )?;
            Some(variable)
        } else {
            None
        };
        let namespace_variable = format!("namespace{namespace_index}");
        writeln!(
            &mut source,
            "  const {namespace_variable} = capturedCreate(null);"
        )?;
        for name in namespace.functions.keys() {
            let name_json = serde_json::to_string(name)?;
            let call_expression = format!("decode(await call({global}, {name_json}, args))");
            let body = error_variable.as_ref().map_or_else(
                || format!("return {call_expression};"),
                |error_variable| {
                    format!(
                        "try {{ return {call_expression}; }} catch (error) {{ const message = error !== null && typeof error === 'object' && typeof error.message === 'string' ? error.message : capturedString(error); throw new {error_variable}({name_json}, message); }}"
                    )
                },
            );
            writeln!(
                &mut source,
                "  defineData({namespace_variable}, {name_json}, async args => {{ {body} }});\n"
            )?;
        }
        writeln!(
            &mut source,
            "  defineData(globalThis, {global}, {namespace_variable}, false);"
        )?;
    }
    source.push_str("  delete globalThis.__seekdeep_call__;\n})();\n");
    Ok(source)
}

async fn binding_call(
    _this: &JsValue,
    args: &[JsValue],
    context: &RefCell<&mut Context>,
) -> JsResult<JsValue> {
    let global = args
        .first()
        .and_then(JsValue::as_string)
        .map(|value| value.to_std_string_escaped())
        .ok_or_else(|| JsNativeError::typ().with_message("binding global must be a string"))?;
    let name = args
        .get(1)
        .and_then(JsValue::as_string)
        .map(|value| value.to_std_string_escaped())
        .ok_or_else(|| JsNativeError::typ().with_message("binding name must be a string"))?;
    let argument = {
        let mut context = context.borrow_mut();
        args.get(2)
            .and_then(|value| snapshot_json(value, &mut context))
    }
    .ok_or_else(|| {
        JsNativeError::error().with_message("binding arguments must be lossless JSON")
    })?;
    let function = RUN_STATE.with(|state| {
        let state = state.borrow();
        let state = state.as_ref().expect("binding called inside a run");
        state
            .bindings
            .iter()
            .find(|namespace| namespace.global == global)
            .and_then(|namespace| namespace.functions.get(&name))
            .cloned()
    });
    let Some(function) = function else {
        let qualified = serde_json::to_string(&format!("{global}.{name}"))
            .unwrap_or_else(|_| "\"unknown\"".to_owned());
        return Err(JsNativeError::error()
            .with_message(format!("unknown binding {qualified}"))
            .into());
    };
    let resolved = function(argument)
        .await
        .map_err(|error| JsNativeError::error().with_message(error.to_string()))?;
    let wire = encode_worker_json(&resolved);
    JsValue::from_json(&wire, &mut context.borrow_mut())
}

fn port_control(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(message) = args.first().and_then(JsValue::as_object) else {
        return Ok(JsValue::new(true));
    };
    let Some(message_type) = message
        .get(js_string!("type"), context)
        .ok()
        .and_then(|value| value.as_string())
        .map(|value| value.to_std_string_escaped())
    else {
        return Ok(JsValue::new(true));
    };
    match message_type.as_str() {
        "log" => {
            let Some(text) = message
                .get(js_string!("text"), context)
                .ok()
                .and_then(|value| value.as_string())
                .map(|value| value.to_std_string_escaped())
            else {
                return Ok(JsValue::new(true));
            };
            if push_captured_log(&text) {
                Err(JsNativeError::error()
                    .with_message(OUTPUT_LIMIT_SENTINEL)
                    .into())
            } else {
                Ok(JsValue::new(true))
            }
        }
        "output-limit" => {
            RUN_STATE.with(|state| {
                state
                    .borrow_mut()
                    .as_mut()
                    .expect("port called inside a run")
                    .output_limit = true;
            });
            Err(JsNativeError::error()
                .with_message(OUTPUT_LIMIT_SENTINEL)
                .into())
        }
        "done" => {
            let completion = parse_forged_done(&message, context);
            let Some(completion) = completion else {
                return Ok(JsValue::new(true));
            };
            RUN_STATE.with(|state| {
                state
                    .borrow_mut()
                    .as_mut()
                    .expect("port called inside a run")
                    .terminal_override = Some(completion);
            });
            Err(JsNativeError::error()
                .with_message(PORT_TERMINAL_SENTINEL)
                .into())
        }
        "call" => {
            let id = message.get(js_string!("id"), context).ok();
            let global = message.get(js_string!("global"), context).ok();
            let name = message.get(js_string!("name"), context).ok();
            let (Some(id), Some(global), Some(name)) = (id, global, name) else {
                return Ok(JsValue::new(true));
            };
            let Some(id) = id.as_number() else {
                return Ok(JsValue::new(true));
            };
            if global.as_string().is_none() || name.as_string().is_none() {
                return Ok(JsValue::new(true));
            }
            let duplicate = RUN_STATE.with(|state| {
                let mut state = state.borrow_mut();
                !state
                    .as_mut()
                    .expect("port called inside a run")
                    .answered_port_calls
                    .insert(number_key(id))
            });
            Ok(JsValue::new(duplicate))
        }
        _ => Ok(JsValue::new(true)),
    }
}

fn parse_forged_done(
    message: &boa_engine::JsObject,
    context: &mut Context,
) -> Option<EngineCompletion> {
    let error = message.get(js_string!("error"), context).ok()?;
    if !error.is_undefined() {
        let error = error.as_object()?;
        let kind = error
            .get(js_string!("kind"), context)
            .ok()?
            .as_string()?
            .to_std_string_escaped();
        let message = error
            .get(js_string!("message"), context)
            .ok()?
            .as_string()?
            .to_std_string_escaped();
        let kind = match kind.as_str() {
            "exception" => CodeRunFailureKind::Exception,
            "invalid-output" => CodeRunFailureKind::InvalidOutput,
            "output-limit" => CodeRunFailureKind::OutputLimit,
            _ => return None,
        };
        return Some(EngineCompletion::ForgedFailure(kind, message));
    }
    let value = message.get(js_string!("value"), context).ok()?;
    if value.is_undefined() {
        return Some(EngineCompletion::Success(None));
    }
    let Some(wire) = snapshot_json(&value, context) else {
        return Some(EngineCompletion::InvalidOutput);
    };
    Some(
        decode_worker_json(&wire).map_or(EngineCompletion::InvalidOutput, |value| {
            EngineCompletion::Success(Some(value))
        }),
    )
}

async fn port_call(
    _this: &JsValue,
    args: &[JsValue],
    context: &RefCell<&mut Context>,
) -> JsResult<JsValue> {
    let Some(message) = args.first().and_then(JsValue::as_object) else {
        return Ok(JsValue::undefined());
    };
    let (id, global, name, argument) = {
        let mut context = context.borrow_mut();
        let Some(id) = message
            .get(js_string!("id"), &mut context)
            .ok()
            .and_then(|value| value.as_number())
        else {
            return Ok(JsValue::undefined());
        };
        let Some(global) = message
            .get(js_string!("global"), &mut context)
            .ok()
            .and_then(|value| value.as_string())
            .map(|value| value.to_std_string_escaped())
        else {
            return Ok(JsValue::undefined());
        };
        let Some(name) = message
            .get(js_string!("name"), &mut context)
            .ok()
            .and_then(|value| value.as_string())
            .map(|value| value.to_std_string_escaped())
        else {
            return Ok(JsValue::undefined());
        };
        let argument = message
            .get(js_string!("args"), &mut context)
            .ok()
            .and_then(|value| snapshot_json(&value, &mut context))
            .and_then(|wire| decode_worker_json(&wire));
        (id, global, name, argument)
    };
    let function = RUN_STATE.with(|state| {
        let state = state.borrow();
        let state = state.as_ref().expect("port called inside a run");
        state
            .bindings
            .iter()
            .find(|namespace| namespace.global == global)
            .and_then(|namespace| namespace.functions.get(&name))
            .cloned()
    });
    let reply = if let Some(argument) = argument {
        if let Some(function) = function {
            match function(argument).await {
                Ok(value) => serde_json::json!({
                    "type": "reply",
                    "id": port_id_value(id),
                    "ok": true,
                    "value": encode_worker_json(&value),
                }),
                Err(error) => port_failure_reply(id, &error.to_string()),
            }
        } else {
            let qualified = serde_json::to_string(&format!("{global}.{name}"))
                .unwrap_or_else(|_| "\"unknown\"".to_owned());
            port_failure_reply(id, &format!("unknown binding {qualified}"))
        }
    } else {
        port_failure_reply(id, "binding arguments must be lossless JSON")
    };
    deliver_reply(&reply, context)?;
    Ok(JsValue::undefined())
}

fn port_failure_reply(id: f64, message: &str) -> Value {
    serde_json::json!({
        "type": "reply",
        "id": port_id_value(id),
        "ok": false,
        "message": message,
    })
}

fn port_id_value(id: f64) -> Value {
    if id.is_finite() && id.fract() == 0.0 && !(id == 0.0 && id.is_sign_negative()) {
        if (0.0..=9_007_199_254_740_991.0).contains(&id)
            && let Ok(integer) = ryu_js::Buffer::new().format(id).parse::<u64>()
        {
            return Value::Number(serde_json::Number::from(integer));
        }
        if (-9_007_199_254_740_991.0..0.0).contains(&id)
            && let Ok(integer) = ryu_js::Buffer::new().format(id).parse::<i64>()
        {
            return Value::Number(serde_json::Number::from(integer));
        }
    }
    serde_json::Number::from_f64(id).map_or(Value::Null, Value::Number)
}

fn deliver_reply(reply: &Value, context: &RefCell<&mut Context>) -> JsResult<()> {
    let mut context = context.borrow_mut();
    let reply = JsValue::from_json(reply, &mut context)?;
    let global = context.global_object().clone();
    let dispatch = global.get(js_string!("__seekdeep_deliver_reply__"), &mut context)?;
    let Some(dispatch) = dispatch
        .as_object()
        .filter(boa_engine::JsObject::is_callable)
    else {
        return Err(JsNativeError::error()
            .with_message("missing worker reply dispatcher")
            .into());
    };
    dispatch.call(&JsValue::undefined(), &[reply], &mut context)?;
    Ok(())
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

fn capture_log(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let text = args
        .iter()
        .map(|value| {
            if let Some(string) = value.as_string() {
                string.to_std_string_escaped()
            } else {
                inspect_value(value, context, 0, &mut HashSet::new())
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    let limit = push_captured_log(&text);
    if limit {
        Err(JsNativeError::error()
            .with_message(OUTPUT_LIMIT_SENTINEL)
            .into())
    } else {
        Ok(JsValue::undefined())
    }
}

fn push_captured_log(text: &str) -> bool {
    RUN_STATE.with(|state| {
        let mut state = state.borrow_mut();
        let state = state.as_mut().expect("log called inside a run");
        let pushed = state.buffer.push(text);
        if let Some(emitted) = pushed.emitted {
            state.logs.push(emitted);
        }
        if pushed.limit_reached {
            state.output_limit = true;
        }
        pushed.limit_reached
    })
}

fn process_exit(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let code = args.first().map_or(Ok(0), |value| value.to_i32(context))?;
    RUN_STATE.with(|state| {
        state
            .borrow_mut()
            .as_mut()
            .expect("exit called inside a run")
            .process_exit = Some(code);
    });
    Err(JsNativeError::error()
        .with_message(PROCESS_EXIT_SENTINEL)
        .into())
}

async fn sleep(
    _this: &JsValue,
    args: &[JsValue],
    context: &RefCell<&mut Context>,
) -> JsResult<JsValue> {
    let milliseconds = args
        .first()
        .map_or(Ok(0.0), |value| value.to_number(&mut context.borrow_mut()))?;
    let milliseconds = if milliseconds.is_finite() && milliseconds > 0.0 {
        milliseconds.min(f64::from(u32::MAX))
    } else {
        0.0
    };
    tokio::time::sleep(Duration::from_secs_f64(milliseconds / 1_000.0)).await;
    Ok(JsValue::undefined())
}

fn render_rejection(value: &JsValue, context: &mut Context) -> String {
    value.to_string(context).map_or_else(
        |_| "program threw an unrenderable value".to_owned(),
        |message| message.to_std_string_escaped(),
    )
}

fn inspect_value(
    value: &JsValue,
    context: &mut Context,
    depth: usize,
    active: &mut HashSet<boa_engine::JsObject>,
) -> String {
    match value.variant() {
        JsVariant::Null => "null".to_owned(),
        JsVariant::Undefined => "undefined".to_owned(),
        JsVariant::Boolean(value) => value.to_string(),
        JsVariant::Integer32(value) => value.to_string(),
        JsVariant::Float64(value) => ryu_js::Buffer::new().format(value).to_owned(),
        JsVariant::String(value) => quote_inspect_string(&value.to_std_string_escaped()),
        JsVariant::BigInt(value) => format!("{value}n"),
        JsVariant::Symbol(value) => value.to_string(),
        JsVariant::Object(object) if object.is_callable() => "[Function]".to_owned(),
        JsVariant::Object(object) => {
            if depth >= 4 {
                return if object.is_array() {
                    "[Array]"
                } else {
                    "[Object]"
                }
                .to_owned();
            }
            if !active.insert(object.clone()) {
                return "[Circular]".to_owned();
            }
            let rendered = if object.is_array() {
                let keys = object.own_property_keys(context).unwrap_or_default();
                let length = keys.len().saturating_sub(1).min(100);
                let mut entries = Vec::with_capacity(length);
                for index in 0..length {
                    let value = object
                        .get(u32::try_from(index).unwrap_or(u32::MAX), context)
                        .unwrap_or_else(|_| JsValue::undefined());
                    entries.push(inspect_value(&value, context, depth + 1, active));
                }
                if keys.len().saturating_sub(1) > length {
                    entries.push(format!("... {} more items", keys.len() - 1 - length));
                }
                format!("[ {} ]", entries.join(", "))
            } else {
                let keys = object.own_property_keys(context).unwrap_or_default();
                let mut entries = Vec::new();
                for key in keys.into_iter().take(100) {
                    let label = match &key {
                        boa_engine::property::PropertyKey::String(value) => {
                            value.to_std_string_escaped()
                        }
                        boa_engine::property::PropertyKey::Index(value) => value.get().to_string(),
                        boa_engine::property::PropertyKey::Symbol(_) => continue,
                    };
                    let property = object
                        .get(key, context)
                        .unwrap_or_else(|_| JsValue::undefined());
                    entries.push(format!(
                        "{label}: {}",
                        inspect_value(&property, context, depth + 1, active)
                    ));
                }
                format!("{{ {} }}", entries.join(", "))
            };
            active.remove(&object);
            rendered
        }
    }
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
mod tests {
    use std::sync::Arc;

    use indexmap::IndexMap;
    use seekdeep_code_runtime::{CodeBindingErrorClass, CodeBindingFunction, CodeBindingNamespace};
    use serde_json::json;

    use super::*;

    fn binding(
        function: impl Fn(Value) -> anyhow::Result<Value> + Send + Sync + 'static,
    ) -> CodeBindingFunction {
        Arc::new(move |argument| {
            let result = function(argument);
            Box::pin(async move { result })
        })
    }

    fn tools(functions: IndexMap<String, CodeBindingFunction>) -> Vec<CodeBindingNamespace> {
        vec![CodeBindingNamespace {
            global: "tools".to_owned(),
            functions,
            error_class: Some(CodeBindingErrorClass {
                name: "ToolCallError".to_owned(),
                member_name_property: "toolName".to_owned(),
            }),
        }]
    }

    fn limits(max_output_bytes: usize) -> EngineLimits {
        EngineLimits {
            max_output_bytes,
            compute_ms: 60_000.0,
            max_wall_ms: 2_000.0,
            signal: AbortSignal::default(),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn evaluates_async_typescript_captures_node_like_globals_and_output() {
        let outcome = evaluate_program(
            "interface Point { x: number; y: number }; const p: Point = { x: 1, y: 2 } as Point; console.log('point', p); process.stdout.write('raw-out\\n'); console.warn('careful'); return await Promise.resolve(p.x + p.y);",
            limits(1_000),
            Vec::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            outcome.logs,
            vec!["point { x: 1, y: 2 }", "raw-out\n", "careful"]
        );
        assert!(
            matches!(outcome.completion, EngineCompletion::Success(Some(value)) if value == json!(3))
        );

        let environment = evaluate_program(
            "return JSON.stringify(process.env)",
            limits(1_000),
            Vec::new(),
        )
        .await
        .unwrap();
        assert!(
            matches!(environment.completion, EngineCompletion::Success(Some(value)) if value == json!("{}"))
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn classifies_exception_invalid_absent_pending_exit_and_limit() {
        let thrown = evaluate_program(
            "console.log('before'); throw new Error('boom')",
            limits(1_000),
            Vec::new(),
        )
        .await
        .unwrap();
        assert_eq!(thrown.logs, ["before"]);
        assert!(
            matches!(thrown.completion, EngineCompletion::Exception(message) if message.contains("boom"))
        );

        assert!(matches!(
            evaluate_program("return { f: () => 1 }", limits(1_000), Vec::new())
                .await
                .unwrap()
                .completion,
            EngineCompletion::InvalidOutput
        ));
        assert!(matches!(
            evaluate_program("const x = 1", limits(1_000), Vec::new())
                .await
                .unwrap()
                .completion,
            EngineCompletion::Success(None)
        ));
        assert!(matches!(
            evaluate_program(
                "return await new Promise(() => {})",
                EngineLimits {
                    max_wall_ms: 20.0,
                    ..limits(1_000)
                },
                Vec::new(),
            )
            .await
            .unwrap()
            .completion,
            EngineCompletion::WallTimeout
        ));
        assert!(matches!(
            evaluate_program("process.exit(7)", limits(1_000), Vec::new())
                .await
                .unwrap()
                .completion,
            EngineCompletion::WorkerExit(7)
        ));
        let limited = evaluate_program(
            "console.log('x'.repeat(1000)); return 1",
            limits(64),
            Vec::new(),
        )
        .await
        .unwrap();
        assert!(matches!(limited.completion, EngineCompletion::OutputLimit));
        assert_eq!(limited.logs.len(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stdout_callback_and_timer_are_asynchronous_and_driven() {
        let outcome = evaluate_program(
            "await new Promise(resolve => process.stdout.write('flushed', resolve)); await new Promise(resolve => setTimeout(resolve, 5)); return 'done'",
            limits(1_000),
            Vec::new(),
        )
        .await
        .unwrap();
        assert_eq!(outcome.logs, ["flushed"]);
        assert!(
            matches!(outcome.completion, EngineCompletion::Success(Some(value)) if value == json!("done"))
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn bridges_calls_resolutions_and_typed_rejections() {
        let bindings = tools(IndexMap::from([
            (
                "echo".to_owned(),
                binding(|argument| Ok(json!({ "echoed": argument }))),
            ),
            ("fail".to_owned(), binding(|_| anyhow::bail!("nope"))),
        ]));
        let outcome = evaluate_program(
            "const first = await tools.echo({ n: 1 }); let caught; try { await tools.fail({}) } catch (error) { caught = { typed: error instanceof ToolCallError, name: error.name, toolName: error.toolName, message: error.message }; } return { first, caught };",
            limits(10_000),
            bindings,
        )
        .await
        .unwrap();
        assert!(
            matches!(outcome.completion, EngineCompletion::Success(Some(value)) if value == json!({
                "first": { "echoed": { "n": 1 } },
                "caught": { "typed": true, "name": "ToolCallError", "toolName": "fail", "message": "nope" }
            }))
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn bridges_deep_json_and_prototype_colliding_member_names() {
        let mut functions = IndexMap::new();
        functions.insert("echo".to_owned(), binding(Ok));
        functions.insert("__proto__".to_owned(), binding(|_| Ok(json!("proto-ok"))));
        functions.insert("constructor".to_owned(), binding(|_| Ok(json!("ctor-ok"))));
        let outcome = evaluate_program(
            "let value = 'leaf'; for (let depth = 0; depth < 3000; depth++) value = [value]; const echoed = await tools.echo(value); return { echoed, collisions: [await tools['__proto__']({}), await tools['constructor']({}), typeof tools['hasOwnProperty']] };",
            limits(10_000_000),
            tools(functions),
        )
        .await
        .unwrap();
        let EngineCompletion::Success(Some(mut value)) = outcome.completion else {
            panic!("deep binding did not complete")
        };
        assert_eq!(
            value["collisions"],
            json!(["proto-ok", "ctor-ok", "undefined"])
        );
        let echoed = value.as_object_mut().unwrap().remove("echoed").unwrap();
        let mut cursor = &echoed;
        for _ in 0..3_000 {
            cursor = cursor.as_array().unwrap().first().unwrap();
        }
        assert_eq!(cursor, "leaf");
        std::mem::forget(echoed);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_lossy_arguments_before_calling_host() {
        let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = called.clone();
        let bindings = tools(IndexMap::from([(
            "never".to_owned(),
            binding(move |_| {
                observed.store(true, std::sync::atomic::Ordering::Release);
                Ok(Value::Null)
            }),
        )]));
        let outcome = evaluate_program(
            "const decorated = [1]; Object.defineProperty(decorated, 'extra', { value: true }); try { await tools.never(decorated) } catch (error) { return { typed: error instanceof ToolCallError, name: error.name, toolName: error.toolName, message: error.message }; }",
            limits(10_000),
            bindings,
        )
        .await
        .unwrap();
        assert!(!called.load(std::sync::atomic::Ordering::Acquire));
        assert!(
            matches!(outcome.completion, EngineCompletion::Success(Some(value)) if value == json!({
                "typed": true,
                "name": "ToolCallError",
                "toolName": "never",
                "message": "binding arguments must be lossless JSON"
            }))
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn compute_budget_interrupts_hot_loop_but_excludes_binding_wait() {
        let hot = evaluate_program(
            "for (;;) {}",
            EngineLimits {
                compute_ms: 25.0,
                max_wall_ms: 2_000.0,
                ..limits(1_000)
            },
            Vec::new(),
        )
        .await
        .unwrap();
        assert!(matches!(hot.completion, EngineCompletion::ComputeTimeout));
        let resumed_hot = evaluate_program(
            "await Promise.resolve(); for (;;) {}",
            EngineLimits {
                compute_ms: 5.0,
                max_wall_ms: 2_000.0,
                ..limits(1_000)
            },
            Vec::new(),
        )
        .await
        .unwrap();
        assert!(matches!(
            resumed_hot.completion,
            EngineCompletion::ComputeTimeout
        ));

        let slow: CodeBindingFunction = Arc::new(|_| {
            Box::pin(async {
                tokio::time::sleep(Duration::from_millis(250)).await;
                Ok(json!("slow-done"))
            })
        });
        let waited = evaluate_program(
            "return await tools.slow({})",
            EngineLimits {
                compute_ms: 100.0,
                max_wall_ms: 2_000.0,
                ..limits(1_000)
            },
            tools(IndexMap::from([("slow".to_owned(), slow)])),
        )
        .await
        .unwrap();
        assert!(
            matches!(&waited.completion, EngineCompletion::Success(Some(value)) if value == &json!("slow-done")),
            "unexpected completion: {:?}",
            waited.completion
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancellation_interrupts_hot_loop_with_exact_reason() {
        let signal = AbortSignal::default();
        let cancelling = signal.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            cancelling.abort_with_reason(json!("user-cancel"));
        });
        let cancelled = evaluate_program(
            "for (;;) {}",
            EngineLimits {
                compute_ms: 2_000.0,
                max_wall_ms: 2_000.0,
                signal,
                ..limits(1_000)
            },
            Vec::new(),
        )
        .await
        .unwrap();
        assert!(
            matches!(cancelled.completion, EngineCompletion::Abort(reason) if reason == json!("user-cancel"))
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn boundary_operations_survive_mutated_javascript_globals() {
        let bindings = tools(IndexMap::from([
            ("echo".to_owned(), binding(Ok)),
            ("fail".to_owned(), binding(|_| anyhow::bail!("nope"))),
        ]));
        let outcome = evaluate_program(
            r"
const arrayPrototype = Array.prototype;
const objectPrototype = Object.prototype;
const setPrototype = Set.prototype;
const stringPrototype = String.prototype;
Array.isArray = () => false;
arrayPrototype.at = arrayPrototype.includes = arrayPrototype.pop = arrayPrototype.push = () => { throw new Error('mutated array method') };
Object.defineProperty = Object.getOwnPropertyDescriptor = Object.getPrototypeOf = Object.keys = () => { throw new Error('mutated object method') };
Object.hasOwn = () => false;
Object.is = () => true;
objectPrototype.propertyIsEnumerable = () => false;
Number.isFinite = Number.isSafeInteger = () => false;
Reflect.apply = Reflect.ownKeys = () => { throw new Error('mutated reflect method') };
setPrototype.add = setPrototype.delete = setPrototype.has = () => { throw new Error('mutated set method') };
stringPrototype.charCodeAt = stringPrototype.codePointAt = stringPrototype.slice = () => { throw new Error('mutated string method') };
Buffer.byteLength = () => 0;
Function.prototype.toString = () => 'mutated';
objectPrototype.get = () => undefined;
objectPrototype.constructor = arrayPrototype.constructor = null;
globalThis.Array = globalThis.Buffer = globalThis.Error = globalThis.Function = globalThis.Number = globalThis.Object = globalThis.Reflect = globalThis.Set = globalThis.String = undefined;
const echoed = await tools.echo({ request: ['€', 1] });
let failure;
try { await tools.fail({}) } catch (error) { failure = { typed: error instanceof ToolCallError, name: error.name, toolName: error.toolName, message: error.message }; }
return { echoed, failure, completion: { ok: true, amount: 42 } };
",
            limits(10_000),
            bindings,
        )
        .await
        .unwrap();
        assert!(
            matches!(&outcome.completion, EngineCompletion::Success(Some(value)) if value == &json!({
                "echoed": { "request": ["€", 1] },
                "failure": { "typed": true, "name": "ToolCallError", "toolName": "fail", "message": "nope" },
                "completion": { "ok": true, "amount": 42 }
            })),
            "unexpected completion: {:?}",
            outcome.completion
        );
    }
}
