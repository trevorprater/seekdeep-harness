//! Per-run worker-side script hooks, child RPC, concurrency/caps,
//! cancellation, and result serialization.

use std::{cell::RefCell, sync::Arc};

use boa_engine::{
    Context, JsError, JsObject, JsResult, JsValue, NativeFunction, Source, context::ContextBuilder,
    job::JobExecutor, js_string, object::builtins::JsFunction, object::builtins::JsPromise,
    script::Script,
};
use parking_lot::Mutex;
use seekdeep_llm::{AbortSignal, ContentBlock};
use seekdeep_workflow::{
    WorkflowAgentEndInfo, WorkflowAgentInfo, WorkflowAgentOutcome, WorkflowError,
    WorkflowErrorCode, WorkflowMeta, WorkflowResult, WorkflowStopReason,
};
use serde_json::Value;

use crate::{
    job_executor::WorkflowJobExecutor,
    realm::{materialize_from_realm, render_thrown},
    types::{ChildPort, WorkerLimits},
};

/// The observers the execution reports progress through.
pub trait ExecutionObserver: Send + Sync {
    /// A phase call.
    fn phase(&self, title: &str);
    /// A log call.
    fn log(&self, message: &str);
    /// One agent call started a child.
    fn agent_start(&self, info: &WorkflowAgentInfo);
    /// One agent call settled.
    fn agent_end(&self, info: &WorkflowAgentEndInfo);
}

/// Shared mutable state for one run.
struct ExecutionState {
    started: u64,
    current_phase: Option<String>,
    cancel_reason: Option<String>,
}

/// Per-run shared data the script hooks reach through a thread-local slot.
struct ExecutionShared {
    limits: WorkerLimits,
    observer: Arc<dyn ExecutionObserver>,
    children: Arc<dyn ChildPort>,
    state: Mutex<ExecutionState>,
    cancel: AbortSignal,
    slots: tokio::sync::Semaphore,
}

thread_local! {
    static EXECUTION: RefCell<Option<Arc<ExecutionShared>>> = const { RefCell::new(None) };
}

fn with_shared<R>(function: impl FnOnce(&Arc<ExecutionShared>) -> R) -> R {
    EXECUTION.with(|slot| function(slot.borrow().as_ref().expect("execution installed")))
}

fn cancelled_message(shared: &ExecutionShared) -> String {
    shared
        .state
        .lock()
        .cancel_reason
        .clone()
        .unwrap_or_else(|| "workflow cancelled".to_owned())
}

fn cancelled_error(reason: &str) -> WorkflowError {
    WorkflowError::new(
        format!("workflow run cancelled: {reason}"),
        WorkflowErrorCode::Cancelled,
    )
}

fn js_error(message: impl Into<String>) -> JsError {
    boa_engine::JsNativeError::error()
        .with_message(message.into())
        .into()
}

#[allow(clippy::needless_pass_by_value)]
fn to_js_error(error: WorkflowError) -> JsError {
    js_error(error.message.clone())
}

/// Flatten a child's final output blocks to text.
fn output_text(blocks: &[ContentBlock]) -> String {
    let mut text = String::new();
    for block in blocks {
        if let ContentBlock::Text { text: block_text } = block {
            text.push_str(block_text);
        }
    }
    text
}

/// A short display label derived from the prompt when the script passes none.
fn default_label(prompt: &str) -> String {
    let line = prompt.split('\n').next().unwrap_or_default();
    if line.chars().count() <= 48 {
        line.to_owned()
    } else {
        let head: String = line.chars().take(47).collect();
        format!("{head}…")
    }
}

/// The vm timeout maps to the boa loop-iteration limit.
fn loop_iteration_limit(timeout_ms: u64) -> u64 {
    let value = u128::from(timeout_ms) * 100_000;
    let capped = value.clamp(1, u128::from(u64::MAX));
    u64::try_from(capped).unwrap_or(u64::MAX)
}

const SUPPORTED_AGENT_OPTIONS: [&str; 5] = ["label", "phase", "schema", "provider", "model"];
const DEFERRED_AGENT_OPTIONS: [&str; 3] = ["effort", "isolation", "agentType"];

/// Validated `agent()` options.
struct AgentOptions {
    label: Option<String>,
    phase: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    schema: Option<Value>,
}

/// One live script execution.
pub struct WorkflowExecution {
    shared: Arc<ExecutionShared>,
    body: String,
    meta_name: String,
    args: Option<Value>,
}

impl WorkflowExecution {
    /// Constructs one execution, compiling the body first so a syntax error
    /// throws before any realm state exists.
    ///
    /// # Errors
    ///
    /// Returns a script-parse failure.
    pub fn new(
        meta: &WorkflowMeta,
        body: String,
        args: Option<Value>,
        limits: WorkerLimits,
        observer: Arc<dyn ExecutionObserver>,
        children: Arc<dyn ChildPort>,
    ) -> anyhow::Result<Self> {
        let source = format!("(async () => {{\n{body}\n}})()");
        // Parse-only: the script object is discarded; nothing executes.
        let mut context = ContextBuilder::new()
            .build()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        Script::parse(Source::from_bytes(&source), None, &mut context)
            .map_err(|error| anyhow::anyhow!("workflow script does not parse: {error}"))?;

        let shared = Arc::new(ExecutionShared {
            limits,
            observer,
            children,
            state: Mutex::new(ExecutionState {
                started: 0,
                current_phase: None,
                cancel_reason: None,
            }),
            cancel: AbortSignal::default(),
            slots: tokio::sync::Semaphore::new(0),
        });
        // Initialize the semaphore with the concurrency ceiling.
        shared.slots.add_permits(
            usize::try_from(shared.limits.max_concurrent_agents).unwrap_or(usize::MAX),
        );

        Ok(Self {
            shared,
            body,
            meta_name: meta.name.clone(),
            args,
        })
    }

    /// Cancel the run: every future hook call throws the cancelled error.
    pub fn cancel(&self, reason: &str) {
        let mut state = self.shared.state.lock();
        if state.cancel_reason.is_some() {
            return;
        }
        state.cancel_reason = Some(reason.to_owned());
        drop(state);
        self.shared
            .cancel
            .abort_with_reason(serde_json::Value::String(reason.to_owned()));
    }

    /// Run the script to settlement on a dedicated worker thread. Resolves -
    /// never rejects - with the run's outcome.
    pub async fn drive(&self) -> WorkflowResult {
        let shared = Arc::clone(&self.shared);
        let body = self.body.clone();
        let meta_name = self.meta_name.clone();
        let args = self.args.clone();
        let (send, receive) = tokio::sync::oneshot::channel();
        let spawn = std::thread::Builder::new()
            .name("seekdeep-workflow-worker".to_owned())
            .spawn(move || {
                let outcome = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(anyhow::Error::from)
                    .and_then(|runtime| {
                        runtime.block_on(run_script(shared, body, meta_name, args))
                    });
                let _ = send.send(outcome);
            });
        if let Err(error) = spawn {
            return error_result(&error.to_string());
        }
        match receive.await {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => error_result(&format!("{error:#}")),
            Err(_) => error_result("workflow worker exited before completing"),
        }
    }
}

fn error_result(message: &str) -> WorkflowResult {
    WorkflowResult {
        value: Value::Null,
        stop_reason: WorkflowStopReason::Error,
        error: Some(message.to_owned()),
        agents_started: 0,
    }
}

fn cancelled_result(shared: &ExecutionShared, agents_started: u64) -> WorkflowResult {
    WorkflowResult {
        value: Value::Null,
        stop_reason: WorkflowStopReason::Cancelled,
        error: Some(format!(
            "workflow run cancelled: {}",
            cancelled_message(shared)
        )),
        agents_started,
    }
}

/// Materialize the script's return value; violations become `RESULT_UNSERIALIZABLE`.
fn materialize_result(raw: &JsValue, context: &mut Context) -> Result<Value, WorkflowError> {
    match materialize_from_realm(raw, context, "workflow result") {
        Ok(Some(value)) => Ok(value),
        Ok(None) => Ok(Value::Null),
        Err(error) => Err(WorkflowError::new(
            format!(
                "the workflow's return value is not plain JSON data — {error}. Return only JSON-serializable objects/arrays/scalars."
            ),
            WorkflowErrorCode::ResultUnserializable,
        )),
    }
}

/// Install the script hooks on the worker context.
fn install_hooks(context: &mut Context) -> anyhow::Result<()> {
    context
        .register_global_builtin_callable(
            js_string!("agent"),
            2,
            NativeFunction::from_async_fn(agent_hook),
        )
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    context
        .register_global_builtin_callable(
            js_string!("parallel"),
            1,
            NativeFunction::from_async_fn(parallel_hook),
        )
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    context
        .register_global_builtin_callable(
            js_string!("pipeline"),
            2,
            NativeFunction::from_async_fn(pipeline_hook),
        )
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    context
        .register_global_builtin_callable(
            js_string!("phase"),
            1,
            NativeFunction::from_fn_ptr(phase_hook),
        )
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    context
        .register_global_builtin_callable(
            js_string!("log"),
            1,
            NativeFunction::from_fn_ptr(log_hook),
        )
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    Ok(())
}

/// Run the compiled script to settlement.
async fn run_script(
    shared: Arc<ExecutionShared>,
    body: String,
    _meta_name: String,
    args: Option<Value>,
) -> anyhow::Result<WorkflowResult> {
    let executor = std::rc::Rc::new(WorkflowJobExecutor::new());
    let mut context = ContextBuilder::new()
        .job_executor(executor.clone())
        .build()
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;

    EXECUTION.with(|slot| *slot.borrow_mut() = Some(Arc::clone(&shared)));

    install_hooks(&mut context)?;
    if let Some(args) = &args {
        let value = JsValue::from_json(args, &mut context)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        context
            .register_global_property(
                js_string!("args"),
                value,
                boa_engine::property::Attribute::all(),
            )
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    }
    context
        .runtime_limits_mut()
        .set_loop_iteration_limit(loop_iteration_limit(shared.limits.sync_timeout_ms));

    let source = format!("(async () => {{\n{body}\n}})()");
    let script = Script::parse(Source::from_bytes(&source), None, &mut context)
        .map_err(|error| anyhow::anyhow!("workflow script does not parse: {error}"))?;

    let returned = {
        let evaluation = script.evaluate_async_with_budget(&mut context, 256);
        tokio::pin!(evaluation);
        match tokio::time::timeout(
            std::time::Duration::from_millis(shared.limits.sync_timeout_ms),
            &mut evaluation,
        )
        .await
        {
            Ok(Ok(value)) => value,
            Ok(Err(error)) => {
                return Ok(error_result(&error.to_string()));
            }
            Err(_) => {
                return Ok(error_result(
                    "workflow script exceeded its synchronous timeout",
                ));
            }
        }
    };

    let Some(promise) = returned
        .as_object()
        .and_then(|object| JsPromise::from_object(object).ok())
    else {
        return Ok(error_result("program wrapper did not return a promise"));
    };

    let jobs = {
        let context = RefCell::new(&mut context);
        executor.run_jobs_async(&context).await
    };
    if let Err(error) = jobs {
        return Ok(error_result(&format!("{error}")));
    }

    match promise.state() {
        boa_engine::builtins::promise::PromiseState::Fulfilled(value) if value.is_undefined() => {
            if shared.cancel.is_aborted() {
                return Ok(cancelled_result(&shared, shared.state.lock().started));
            }
            Ok(WorkflowResult {
                value: Value::Null,
                stop_reason: WorkflowStopReason::Completed,
                error: None,
                agents_started: shared.state.lock().started,
            })
        }
        boa_engine::builtins::promise::PromiseState::Fulfilled(value) => {
            if shared.cancel.is_aborted() {
                return Ok(cancelled_result(&shared, shared.state.lock().started));
            }
            match materialize_result(&value, &mut context) {
                Ok(value) => Ok(WorkflowResult {
                    value,
                    stop_reason: WorkflowStopReason::Completed,
                    error: None,
                    agents_started: shared.state.lock().started,
                }),
                Err(error) => Ok(error_result(&error.to_string())),
            }
        }
        boa_engine::builtins::promise::PromiseState::Rejected(value) => {
            if shared.cancel.is_aborted() {
                return Ok(cancelled_result(&shared, shared.state.lock().started));
            }
            Ok(error_result(&render_thrown(&value, &mut context)))
        }
        boa_engine::builtins::promise::PromiseState::Pending => {
            if shared.cancel.is_aborted() {
                return Ok(cancelled_result(&shared, shared.state.lock().started));
            }
            Ok(error_result("workflow script did not settle"))
        }
    }
}

fn array_length(array: &JsObject, _context: &mut Context) -> JsResult<usize> {
    let length_key = boa_engine::property::PropertyKey::from(js_string!("length"));
    let descriptor = array
        .borrow()
        .properties()
        .get(&length_key)
        .ok_or_else(|| js_error("array length"))?;
    let length = descriptor
        .value()
        .and_then(JsValue::as_number)
        .ok_or_else(|| js_error("array length"))?;
    if !length.is_finite() || length < 0.0 || length.fract() != 0.0 {
        return Err(js_error("array length"));
    }
    ryu_js::Buffer::new()
        .format(length)
        .parse::<usize>()
        .map_err(|_| js_error("array length"))
}

/// Whether a rejection is a fatal workflow error (thrown by the native hooks).
fn is_fatal(error: &JsError) -> bool {
    error.as_native().is_some()
}

/// Read and validate the `agent()` options bag.
fn read_agent_options(
    raw: Option<&JsValue>,
    context: &RefCell<&mut Context>,
) -> Result<AgentOptions, WorkflowError> {
    let Some(raw) = raw else {
        return Ok(AgentOptions {
            label: None,
            phase: None,
            provider: None,
            model: None,
            schema: None,
        });
    };
    if raw.is_undefined() {
        return Ok(AgentOptions {
            label: None,
            phase: None,
            provider: None,
            model: None,
            schema: None,
        });
    }
    let opts = materialize_from_realm(raw, &mut context.borrow_mut(), "agent() options").map_err(
        |error| {
            WorkflowError::new(
                format!("agent() options must be plain JSON data — {error}"),
                WorkflowErrorCode::InvalidArgument,
            )
        },
    )?;
    let Some(object) = opts else {
        return Err(WorkflowError::new(
            "agent() options must be an object",
            WorkflowErrorCode::InvalidArgument,
        ));
    };
    let Some(object) = object.as_object() else {
        return Err(WorkflowError::new(
            "agent() options must be an object",
            WorkflowErrorCode::InvalidArgument,
        ));
    };
    for key in object.keys() {
        if SUPPORTED_AGENT_OPTIONS.contains(&key.as_str()) {
            continue;
        }
        if DEFERRED_AGENT_OPTIONS.contains(&key.as_str()) {
            return Err(WorkflowError::new(
                format!(
                    "agent() option \"{key}\" is deferred and not supported by this engine (supported: label, phase, schema, provider, model)"
                ),
                WorkflowErrorCode::UnsupportedOption,
            ));
        }
        return Err(WorkflowError::new(
            format!(
                "agent() option \"{key}\" is not recognized (supported: label, phase, schema, provider, model)"
            ),
            WorkflowErrorCode::UnsupportedOption,
        ));
    }
    for key in ["label", "phase", "provider", "model"] {
        if let Some(value) = object.get(key)
            && !value.is_string()
        {
            return Err(WorkflowError::new(
                format!("agent() option \"{key}\" must be a string"),
                WorkflowErrorCode::InvalidArgument,
            ));
        }
    }
    let label = object
        .get("label")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let phase = object
        .get("phase")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let provider = object
        .get("provider")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let model = object
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let schema = object.get("schema").cloned();
    if let Some(schema) = &schema {
        seekdeep_tools::assert_object_json_schema(schema.clone()).map_err(|error| {
            WorkflowError::new(
                format!("agent() schema is outside the supported subset — {error}"),
                WorkflowErrorCode::UnsupportedSchema,
            )
        })?;
    }
    Ok(AgentOptions {
        label,
        phase,
        provider,
        model,
        schema,
    })
}

/// The agent(prompt, opts) hook.
async fn agent_hook(
    _this: &JsValue,
    args: &[JsValue],
    context: &RefCell<&mut Context>,
) -> JsResult<JsValue> {
    let shared = with_shared(Arc::clone);
    if shared.cancel.is_aborted() {
        return Err(to_js_error(cancelled_error(&cancelled_message(&shared))));
    }
    let prompt = args
        .first()
        .and_then(JsValue::as_string)
        .map(|text| text.to_std_string_escaped());
    let Some(prompt) = prompt else {
        return Err(js_error("agent() requires a non-empty prompt string"));
    };
    if prompt.is_empty() {
        return Err(js_error("agent() requires a non-empty prompt string"));
    }
    let opts = read_agent_options(args.get(1), context).map_err(to_js_error)?;
    let (seq, label, phase) = {
        let mut state = shared.state.lock();
        if state.started >= shared.limits.max_total_agents {
            return Err(js_error(format!(
                "this run reached its total agent cap ({}) — a runaway-loop backstop; raise the applicable maxTotalAgents limit if the scale is intentional",
                shared.limits.max_total_agents
            )));
        }
        state.started += 1;
        let label = opts.label.clone().unwrap_or_else(|| default_label(&prompt));
        let phase = opts.phase.clone().or_else(|| state.current_phase.clone());
        (state.started, label, phase)
    };
    let permit = tokio::select! {
        permit = shared.slots.acquire() => permit.map_err(|_| js_error("concurrency slot closed"))?,
        () = shared.cancel.cancelled() => {
            return Err(to_js_error(cancelled_error(&cancelled_message(&shared))));
        }
    };
    let result = run_agent(&shared, &prompt, &opts, seq, label, phase, context).await;
    drop(permit);
    result
}

#[allow(clippy::too_many_lines)]
async fn run_agent(
    shared: &ExecutionShared,
    prompt: &str,
    opts: &AgentOptions,
    seq: u64,
    label: String,
    phase: Option<String>,
    context: &RefCell<&mut Context>,
) -> JsResult<JsValue> {
    if shared.cancel.is_aborted() {
        return Err(to_js_error(cancelled_error(&cancelled_message(shared))));
    }
    let request = crate::types::ChildStartRequest {
        prompt: prompt.to_owned(),
        schema: opts.schema.clone(),
        provider: opts.provider.clone(),
        model: opts.model.clone(),
    };
    let run = match shared.children.start_agent(request).await {
        Ok(run) => run,
        Err(error) => {
            if shared.cancel.is_aborted() {
                return Err(to_js_error(cancelled_error(&cancelled_message(shared))));
            }
            return Err(to_js_error(WorkflowError::new(
                format!("agent() could not start a child: {error}"),
                WorkflowErrorCode::AgentStart,
            )));
        }
    };
    if shared.cancel.is_aborted() {
        let _ = run.dispose().await;
        return Err(to_js_error(cancelled_error(&cancelled_message(shared))));
    }
    let info = WorkflowAgentInfo {
        seq,
        label,
        phase,
        child_id: seekdeep_core::session::SessionId::new(run.id()),
    };
    shared.observer.agent_start(&info);
    let settled = match run.result().await {
        Ok(result) => result,
        Err(error) => {
            if shared.cancel.is_aborted() {
                shared.observer.agent_end(&WorkflowAgentEndInfo {
                    info,
                    outcome: WorkflowAgentOutcome::Cancelled,
                });
                let _ = run.dispose().await;
                return Err(to_js_error(cancelled_error(&cancelled_message(shared))));
            }
            shared.observer.agent_end(&WorkflowAgentEndInfo {
                info,
                outcome: WorkflowAgentOutcome::Failed,
            });
            let _ = run.dispose().await;
            return Err(to_js_error(WorkflowError::new(
                format!("child agent run failed: {error}"),
                WorkflowErrorCode::AgentResult,
            )));
        }
    };
    let value = if settled.stop_reason == "completed" {
        if opts.schema.is_some() {
            if settled.structured.is_none() {
                shared.observer.agent_end(&WorkflowAgentEndInfo {
                    info,
                    outcome: WorkflowAgentOutcome::Failed,
                });
                let _ = run.dispose().await;
                return Ok(JsValue::null());
            }
            shared.observer.agent_end(&WorkflowAgentEndInfo {
                info,
                outcome: WorkflowAgentOutcome::Completed,
            });
            let _ = run.dispose().await;
            JsValue::from_json(
                &settled.structured.expect("checked"),
                &mut context.borrow_mut(),
            )
            .map_err(|error| js_error(error.to_string()))?
        } else {
            shared.observer.agent_end(&WorkflowAgentEndInfo {
                info,
                outcome: WorkflowAgentOutcome::Completed,
            });
            let _ = run.dispose().await;
            JsValue::from(js_string!(output_text(&settled.output)))
        }
    } else if shared.cancel.is_aborted() {
        shared.observer.agent_end(&WorkflowAgentEndInfo {
            info,
            outcome: WorkflowAgentOutcome::Cancelled,
        });
        let _ = run.dispose().await;
        return Err(to_js_error(cancelled_error(&cancelled_message(shared))));
    } else {
        shared.observer.agent_end(&WorkflowAgentEndInfo {
            info,
            outcome: WorkflowAgentOutcome::Failed,
        });
        let _ = run.dispose().await;
        JsValue::null()
    };
    Ok(value)
}

/// Await one value-or-promise as a future.
fn as_future(
    value: JsValue,
    context: &RefCell<&mut Context>,
) -> boa_engine::object::builtins::JsFuture {
    if let Some(promise) = value
        .as_object()
        .and_then(|object| JsPromise::from_object(object).ok())
    {
        let mut ctx = context.borrow_mut();
        promise.into_js_future(&mut ctx)
    } else {
        let mut ctx = context.borrow_mut();
        JsPromise::resolve(value, &mut ctx).into_js_future(&mut ctx)
    }
}

/// The parallel(thunks) hook.
async fn parallel_hook(
    _this: &JsValue,
    args: &[JsValue],
    context: &RefCell<&mut Context>,
) -> JsResult<JsValue> {
    let shared = with_shared(Arc::clone);
    if shared.cancel.is_aborted() {
        return Err(to_js_error(cancelled_error(&cancelled_message(&shared))));
    }
    let Some(array) = args
        .first()
        .and_then(JsValue::as_object)
        .filter(|object| object.is_array())
    else {
        return Err(js_error(
            "parallel() requires an array of zero-argument functions",
        ));
    };
    let length = {
        let mut ctx = context.borrow_mut();
        array_length(&array, &mut ctx)?
    };
    if length > usize::try_from(shared.limits.max_items_per_call).unwrap_or(usize::MAX) {
        return Err(js_error(format!(
            "parallel() received {length} items — over the per-call cap ({}); split the work or raise maxItemsPerCall in the engine config",
            shared.limits.max_items_per_call
        )));
    }
    let mut futures = Vec::with_capacity(length);
    for index in 0..length {
        let key = boa_engine::property::PropertyKey::from(
            u32::try_from(index).map_err(|_| js_error("item index"))?,
        );
        let thunk = array.get(key, &mut context.borrow_mut())?;
        if !thunk.is_callable() {
            return Err(js_error(format!(
                "parallel() item {index} is not a function"
            )));
        }
        let thunk_object = thunk
            .as_object()
            .ok_or_else(|| js_error("not a function"))?;
        let function = JsFunction::from_object(thunk_object.clone())
            .ok_or_else(|| js_error("not a function"))?;
        let value: JsValue = function
            .typed::<(JsValue,), JsValue>()
            .call(&mut context.borrow_mut(), (JsValue::undefined(),))?;
        futures.push(as_future(value, context));
    }
    let mut out = Vec::with_capacity(futures.len());
    for future in futures {
        match future.await {
            Ok(value) => out.push(value),
            Err(error) => {
                if shared.cancel.is_aborted() {
                    return Err(to_js_error(cancelled_error(&cancelled_message(&shared))));
                }
                if is_fatal(&error) {
                    return Err(error);
                }
                out.push(JsValue::null());
            }
        }
    }
    let array = boa_engine::object::builtins::JsArray::from_iter(out, &mut context.borrow_mut());
    Ok(array.into())
}

/// The pipeline(items, ...stages) hook.
async fn pipeline_hook(
    _this: &JsValue,
    args: &[JsValue],
    context: &RefCell<&mut Context>,
) -> JsResult<JsValue> {
    let shared = with_shared(Arc::clone);
    if shared.cancel.is_aborted() {
        return Err(to_js_error(cancelled_error(&cancelled_message(&shared))));
    }
    let Some(items) = args
        .first()
        .and_then(JsValue::as_object)
        .filter(|object| object.is_array())
    else {
        return Err(js_error("pipeline() requires an items array"));
    };
    let item_count = {
        let mut ctx = context.borrow_mut();
        array_length(&items, &mut ctx)?
    };
    if item_count > usize::try_from(shared.limits.max_items_per_call).unwrap_or(usize::MAX) {
        return Err(js_error(format!(
            "pipeline() received {item_count} items — over the per-call cap ({}); split the work or raise maxItemsPerCall in the engine config",
            shared.limits.max_items_per_call
        )));
    }
    if args.len() < 2 {
        return Err(js_error("pipeline() requires at least one stage function"));
    }
    let stages = &args[1..];
    for (index, stage) in stages.iter().enumerate() {
        if !stage.is_callable() {
            return Err(js_error(format!(
                "pipeline() stage {index} is not a function"
            )));
        }
    }
    let mut futures = Vec::with_capacity(item_count);
    for item_index in 0..item_count {
        let key = boa_engine::property::PropertyKey::from(
            u32::try_from(item_index).map_err(|_| js_error("item index"))?,
        );
        let item = items.get(key, &mut context.borrow_mut())?;
        let stages = stages.to_vec();
        futures.push(run_pipeline_item(
            &shared, item, stages, item_index, context,
        ));
    }
    let mut out = Vec::with_capacity(futures.len());
    for future in futures {
        match future.await {
            Ok(value) => out.push(value),
            Err(error) => {
                if shared.cancel.is_aborted() {
                    return Err(to_js_error(cancelled_error(&cancelled_message(&shared))));
                }
                if is_fatal(&error) {
                    return Err(error);
                }
                out.push(JsValue::null());
            }
        }
    }
    let array = boa_engine::object::builtins::JsArray::from_iter(out, &mut context.borrow_mut());
    Ok(array.into())
}

#[allow(clippy::cast_precision_loss)]
async fn run_pipeline_item(
    _shared: &ExecutionShared,
    item: JsValue,
    stages: Vec<JsValue>,
    item_index: usize,
    context: &RefCell<&mut Context>,
) -> Result<JsValue, JsError> {
    let mut value = item.clone();
    for stage in stages {
        let stage_object = stage
            .as_object()
            .ok_or_else(|| js_error("not a function"))?;
        let function = JsFunction::from_object(stage_object.clone())
            .ok_or_else(|| js_error("not a function"))?;
        let result: JsValue = function
            .typed::<(JsValue, JsValue, JsValue), JsValue>()
            .call(
                &mut context.borrow_mut(),
                (
                    value.clone(),
                    item.clone(),
                    JsValue::from(item_index as f64),
                ),
            )?;
        value = as_future(result, context).await?;
    }
    Ok(value)
}

/// The phase(title) hook.
fn phase_hook(_this: &JsValue, args: &[JsValue], _context: &mut Context) -> JsResult<JsValue> {
    let shared = with_shared(Arc::clone);
    if shared.cancel.is_aborted() {
        return Err(to_js_error(cancelled_error(&cancelled_message(&shared))));
    }
    let title = args
        .first()
        .and_then(JsValue::as_string)
        .map(|text| text.to_std_string_escaped());
    let Some(title) = title else {
        return Err(js_error("phase() requires a non-empty title string"));
    };
    if title.is_empty() {
        return Err(js_error("phase() requires a non-empty title string"));
    }
    shared.state.lock().current_phase = Some(title.clone());
    shared.observer.phase(&title);
    Ok(JsValue::undefined())
}

/// The log(message) hook.
fn log_hook(_this: &JsValue, args: &[JsValue], _context: &mut Context) -> JsResult<JsValue> {
    let shared = with_shared(Arc::clone);
    if shared.cancel.is_aborted() {
        return Err(to_js_error(cancelled_error(&cancelled_message(&shared))));
    }
    let message = args
        .first()
        .and_then(JsValue::as_string)
        .map(|text| text.to_std_string_escaped());
    let Some(message) = message else {
        return Err(js_error("log() requires a message string"));
    };
    shared.observer.log(&message);
    Ok(JsValue::undefined())
}
