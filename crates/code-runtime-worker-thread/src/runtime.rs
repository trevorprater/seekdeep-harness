//! Lifecycle-owned worker-thread runtime backend.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use async_trait::async_trait;
use futures::{StreamExt as _, future::BoxFuture, stream::FuturesUnordered};
use regex::Regex;
use seekdeep_code_runtime::{
    CodeBindingFunction, CodeBindingNamespace, CodeRunFailure, CodeRunFailureKind, CodeRunRequest,
    CodeRunResult, CodeRuntime, CodeRuntimeBackend, PORTABLE_RESERVED_WORDS,
    RESERVED_BINDING_GLOBALS, RESERVED_ERROR_MEMBERS, is_dunder_member,
};
use seekdeep_cordis::{Context, fiber::EffectHandle};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    engine::{EngineCompletion, EngineLimits, EngineOutcome, evaluate_program},
    output_ledger::OutputLedger,
};

const MAX_TIMER_DELAY_MS: f64 = 2_147_483_647.0;
const MIN_OUTPUT_BYTES: f64 = 4.0;
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

static IDENTIFIER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").expect("valid identifier regex"));

/// Configurable execution bounds. Omitted fields receive source-compatible
/// defaults.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerThreadCodeRuntimeConfig {
    /// Measured worker busy-time allowance in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compute_ms: Option<f64>,
    /// Absolute wall-clock ceiling in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_wall_ms: Option<f64>,
    /// Combined JSON bytes available to logs plus completion or diagnostic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_bytes: Option<f64>,
    /// Per-worker old-generation heap allowance in mebibytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_old_generation_size_mb: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ResolvedConfig {
    compute_ms: f64,
    max_wall_ms: f64,
    max_output_bytes: usize,
    max_old_generation_size_mb: f64,
}

impl WorkerThreadCodeRuntimeConfig {
    fn resolve(&self) -> anyhow::Result<ResolvedConfig> {
        let values = [
            ("computeMs", self.compute_ms.unwrap_or(60_000.0)),
            ("maxWallMs", self.max_wall_ms.unwrap_or(600_000.0)),
            (
                "maxOutputBytes",
                self.max_output_bytes.unwrap_or(67_108_864.0),
            ),
            (
                "maxOldGenerationSizeMb",
                self.max_old_generation_size_mb.unwrap_or(512.0),
            ),
        ];
        for (key, value) in values {
            if !value.is_finite() || value <= 0.0 {
                anyhow::bail!(
                    "seekdeep-code-runtime-worker-thread: config.{key} must be a positive number, got {}",
                    number_message(value)
                );
            }
        }
        let max_output_bytes = values[2].1;
        if max_output_bytes.fract() != 0.0
            || !(MIN_OUTPUT_BYTES..=MAX_SAFE_INTEGER).contains(&max_output_bytes)
        {
            anyhow::bail!(
                "seekdeep-code-runtime-worker-thread: config.maxOutputBytes must be a safe integer of at least 4, got {}",
                number_message(max_output_bytes)
            );
        }
        let max_wall_ms = values[1].1;
        if max_wall_ms > MAX_TIMER_DELAY_MS {
            anyhow::bail!(
                "seekdeep-code-runtime-worker-thread: config.maxWallMs must be at most 2147483647 (Node clamps a longer setTimeout delay to 1ms), got {}",
                number_message(max_wall_ms)
            );
        }
        let max_output_bytes = number_message(max_output_bytes)
            .parse::<usize>()
            .map_err(|_| anyhow::anyhow!("maxOutputBytes does not fit this platform"))?;
        Ok(ResolvedConfig {
            compute_ms: values[0].1,
            max_wall_ms,
            max_output_bytes,
            max_old_generation_size_mb: values[3].1,
        })
    }
}

fn number_message(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_owned()
    } else if value == f64::INFINITY {
        "Infinity".to_owned()
    } else if value == f64::NEG_INFINITY {
        "-Infinity".to_owned()
    } else {
        ryu_js::Buffer::new().format(value).to_owned()
    }
}

/// Rust-owned implementation of the TypeScript worker-thread runtime.
///
/// Bindings are invoked and polled on the calling Tokio runtime, independently
/// of the program worker's lifetime. Runs with bindings require an active host runtime.
#[derive(Debug)]
pub struct WorkerThreadCodeRuntime {
    config: ResolvedConfig,
    state: Arc<RuntimeState>,
}

#[derive(Debug)]
struct RuntimeState {
    disposed: AtomicBool,
    next_run: AtomicU64,
    live: parking_lot::Mutex<HashMap<u64, seekdeep_llm::AbortSignal>>,
    changed: tokio::sync::Notify,
}

struct HostBindingCall {
    function: CodeBindingFunction,
    argument: Value,
    reply: tokio::sync::oneshot::Sender<anyhow::Result<Value>>,
}

fn bridge_host_bindings(
    mut bindings: Vec<CodeBindingNamespace>,
) -> anyhow::Result<Vec<CodeBindingNamespace>> {
    if bindings
        .iter()
        .all(|namespace| namespace.functions.is_empty())
    {
        return Ok(bindings);
    }
    let host = tokio::runtime::Handle::try_current()
        .map_err(|error| anyhow::anyhow!("code binding host requires a Tokio runtime: {error}"))?;
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<HostBindingCall>();
    // First suspension is the submission-order boundary; accepted calls retain
    // their host-side lifetime after the worker drops its binding proxies.
    host.spawn(async move {
        let mut pending = FuturesUnordered::<BoxFuture<'static, ()>>::new();
        let mut receiving = true;
        while receiving || !pending.is_empty() {
            tokio::select! {
                biased;
                call = receiver.recv(), if receiving => {
                    if let Some(call) = call {
                        let mut operation: BoxFuture<'static, ()> = Box::pin(async move {
                            let result = (call.function)(call.argument).await;
                            let _ = call.reply.send(result);
                        });
                        if futures::poll!(&mut operation).is_pending() {
                            pending.push(operation);
                        }
                    } else {
                        receiving = false;
                    }
                }
                _ = pending.next(), if !pending.is_empty() => {}
            }
        }
    });
    for namespace in &mut bindings {
        for function in namespace.functions.values_mut() {
            let original = function.clone();
            let sender = sender.clone();
            *function = Arc::new(move |argument| {
                let (reply, response) = tokio::sync::oneshot::channel();
                let submitted = sender
                    .send(HostBindingCall {
                        function: original.clone(),
                        argument,
                        reply,
                    })
                    .map_err(|_| anyhow::anyhow!("code binding host dispatcher stopped"));
                Box::pin(async move {
                    submitted?;
                    response
                        .await
                        .map_err(|_| anyhow::anyhow!("code binding host reply was abandoned"))?
                })
            });
        }
    }
    Ok(bindings)
}

impl WorkerThreadCodeRuntime {
    /// Validates configuration and creates an unmounted backend.
    ///
    /// # Errors
    ///
    /// Rejects non-positive caps, an unsafe output byte count, or a wall
    /// delay above the source backend's timer boundary.
    pub fn new(config: &WorkerThreadCodeRuntimeConfig) -> anyhow::Result<Self> {
        Ok(Self {
            config: config.resolve()?,
            state: Arc::new(RuntimeState {
                disposed: AtomicBool::new(false),
                next_run: AtomicU64::new(1),
                live: parking_lot::Mutex::new(HashMap::new()),
                changed: tokio::sync::Notify::new(),
            }),
        })
    }

    fn validate_bindings(bindings: &[CodeBindingNamespace]) -> anyhow::Result<()> {
        let mut globals = HashSet::new();
        for namespace in bindings {
            if !IDENTIFIER.is_match(&namespace.global)
                || PORTABLE_RESERVED_WORDS.contains(namespace.global.as_str())
            {
                anyhow::bail!(
                    "seekdeep-code-runtime-worker-thread: binding global {:?} is not a usable identifier",
                    namespace.global
                );
            }
            if RESERVED_BINDING_GLOBALS.contains(namespace.global.as_str()) {
                anyhow::bail!(
                    "seekdeep-code-runtime-worker-thread: reserved binding global {:?}",
                    namespace.global
                );
            }
            if !globals.insert(namespace.global.as_str()) {
                anyhow::bail!(
                    "seekdeep-code-runtime-worker-thread: duplicate binding global {:?}",
                    namespace.global
                );
            }
        }

        let mut error_names = HashSet::new();
        for namespace in bindings {
            let Some(descriptor) = &namespace.error_class else {
                continue;
            };
            if !IDENTIFIER.is_match(&descriptor.name)
                || PORTABLE_RESERVED_WORDS.contains(descriptor.name.as_str())
            {
                anyhow::bail!(
                    "seekdeep-code-runtime-worker-thread: binding error class {:?} is not a usable identifier",
                    descriptor.name
                );
            }
            if RESERVED_BINDING_GLOBALS.contains(descriptor.name.as_str()) {
                anyhow::bail!(
                    "seekdeep-code-runtime-worker-thread: reserved binding global {:?}",
                    descriptor.name
                );
            }
            if globals.contains(descriptor.name.as_str())
                || !error_names.insert(descriptor.name.as_str())
            {
                anyhow::bail!(
                    "seekdeep-code-runtime-worker-thread: duplicate injected global {:?}",
                    descriptor.name
                );
            }
            let member = descriptor.member_name_property.as_str();
            if member.is_empty()
                || RESERVED_ERROR_MEMBERS.contains(member)
                || is_dunder_member(member)
            {
                anyhow::bail!(
                    "seekdeep-code-runtime-worker-thread: binding error member property {:?} is not usable",
                    descriptor.member_name_property
                );
            }
        }
        Ok(())
    }

    fn failure_before_worker(&self, kind: CodeRunFailureKind, message: String) -> CodeRunResult {
        OutputLedger::new(self.config.max_output_bytes)
            .failure(Vec::new(), CodeRunFailure { kind, message })
    }

    fn finalize_outcome(&self, outcome: EngineOutcome) -> CodeRunResult {
        let mut ledger = OutputLedger::new(self.config.max_output_bytes);
        let mut logs = Vec::with_capacity(outcome.logs.len());
        for log in &outcome.logs {
            if !ledger.admit(log, &mut logs) {
                return ledger.limit(&outcome.logs);
            }
        }
        match outcome.completion {
            EngineCompletion::Success(value) => ledger.success(logs, value),
            EngineCompletion::Exception(message) => ledger.failure(
                logs,
                CodeRunFailure {
                    kind: CodeRunFailureKind::Exception,
                    message,
                },
            ),
            EngineCompletion::InvalidOutput => ledger.failure(
                logs,
                CodeRunFailure {
                    kind: CodeRunFailureKind::InvalidOutput,
                    message: "program completion must be lossless JSON".to_owned(),
                },
            ),
            EngineCompletion::OutputLimit => ledger.limit(&logs),
            EngineCompletion::WorkerExit(code) => ledger.failure(
                logs,
                CodeRunFailure {
                    kind: CodeRunFailureKind::WorkerExit,
                    message: format!("worker exited with code {code} before completing"),
                },
            ),
            EngineCompletion::HeapLimit => ledger.failure(
                logs,
                CodeRunFailure {
                    kind: CodeRunFailureKind::WorkerExit,
                    message: "worker error: Worker terminated due to reaching memory limit: JS heap out of memory".to_owned(),
                },
            ),
            EngineCompletion::ComputeTimeout => ledger.failure(
                logs,
                CodeRunFailure {
                    kind: CodeRunFailureKind::Timeout,
                    message: format!(
                        "compute budget exhausted ({}ms busy)",
                        number_message(self.config.compute_ms)
                    ),
                },
            ),
            EngineCompletion::WallTimeout => ledger.failure(
                logs,
                CodeRunFailure {
                    kind: CodeRunFailureKind::Timeout,
                    message: format!(
                        "wall-clock ceiling reached ({}ms)",
                        number_message(self.config.max_wall_ms)
                    ),
                },
            ),
            EngineCompletion::Abort(reason) => ledger.failure(
                logs,
                CodeRunFailure {
                    kind: CodeRunFailureKind::Abort,
                    message: js_string(&reason),
                },
            ),
            EngineCompletion::ForgedFailure(kind, message) => {
                ledger.failure(logs, CodeRunFailure { kind, message })
            }
        }
    }

    async fn dispose(&self) {
        let signals = {
            let live = self.state.live.lock();
            self.state.disposed.store(true, Ordering::Release);
            live.values().cloned().collect::<Vec<_>>()
        };
        for signal in signals {
            signal.abort_with_reason(Value::String("runtime disposed".to_owned()));
        }
        loop {
            let changed = self.state.changed.notified();
            if self.state.live.lock().is_empty() {
                return;
            }
            changed.await;
        }
    }
}

struct LiveRunGuard {
    id: u64,
    state: Arc<RuntimeState>,
}

impl Drop for LiveRunGuard {
    fn drop(&mut self) {
        self.state.live.lock().remove(&self.id);
        self.state.changed.notify_waiters();
    }
}

#[async_trait]
impl CodeRuntimeBackend for WorkerThreadCodeRuntime {
    fn language(&self) -> &'static str {
        "typescript"
    }

    fn isolation(&self) -> &'static str {
        "worker-thread"
    }

    async fn run(&self, request: CodeRunRequest) -> anyhow::Result<CodeRunResult> {
        if self.state.disposed.load(Ordering::Acquire) {
            anyhow::bail!("seekdeep-code-runtime-worker-thread: run() after disposal");
        }
        Self::validate_bindings(&request.bindings)?;
        if request
            .signal
            .as_ref()
            .is_some_and(seekdeep_llm::AbortSignal::is_aborted)
        {
            let message = request
                .signal
                .as_ref()
                .and_then(seekdeep_llm::AbortSignal::reason)
                .as_ref()
                .map_or_else(|| "undefined".to_owned(), js_string);
            return Ok(self.failure_before_worker(CodeRunFailureKind::Abort, message));
        }

        let program = request.program;
        let bindings = bridge_host_bindings(request.bindings)?;
        let max_output_bytes = self.config.max_output_bytes;
        let runtime_signal = seekdeep_llm::AbortSignal::default();
        let signal = request.signal.map_or_else(
            || runtime_signal.clone(),
            |caller| seekdeep_llm::AbortSignal::fuse(&caller, &runtime_signal),
        );
        let run_id = self.state.next_run.fetch_add(1, Ordering::Relaxed);
        {
            let mut live = self.state.live.lock();
            if self.state.disposed.load(Ordering::Acquire) {
                anyhow::bail!("seekdeep-code-runtime-worker-thread: run() after disposal");
            }
            live.insert(run_id, runtime_signal);
        }
        let limits = EngineLimits {
            max_output_bytes,
            max_old_generation_size_mb: self.config.max_old_generation_size_mb,
            compute_ms: self.config.compute_ms,
            max_wall_ms: self.config.max_wall_ms,
            signal,
        };
        let (send, receive) = tokio::sync::oneshot::channel();
        let state = self.state.clone();
        let spawn = std::thread::Builder::new()
            .name("seekdeep-code-runtime-worker".to_owned())
            .spawn(move || {
                let _live = LiveRunGuard { id: run_id, state };
                let outcome = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(anyhow::Error::from)
                    .and_then(|runtime| {
                        runtime.block_on(evaluate_program(&program, limits, bindings))
                    });
                let _sent = send.send(outcome);
            });
        if let Err(error) = spawn {
            self.state.live.lock().remove(&run_id);
            self.state.changed.notify_waiters();
            return Err(error.into());
        }
        let outcome = receive.await.map_err(|_| {
            anyhow::anyhow!("seekdeep-code-runtime-worker-thread: worker exited before completing")
        })?;
        Ok(match outcome {
            Ok(outcome) => self.finalize_outcome(outcome),
            Err(error) => {
                self.failure_before_worker(CodeRunFailureKind::Exception, format!("{error:#}"))
            }
        })
    }
}

/// Mounts the runtime on `ctx.codeRuntime` and owns asynchronous teardown.
///
/// # Errors
///
/// Returns configuration, lifecycle, or duplicate-service failures.
pub fn install(
    context: &Context,
    config: &WorkerThreadCodeRuntimeConfig,
) -> anyhow::Result<Arc<WorkerThreadCodeRuntime>> {
    let backend = Arc::new(WorkerThreadCodeRuntime::new(config)?);
    Arc::new(CodeRuntime::new(backend.clone())).provide(context)?;
    let disposing = backend.clone();
    context.own(EffectHandle::new(
        "worker code-runtime teardown",
        move || {
            Box::pin(async move {
                disposing.dispose().await;
                Ok(())
            })
        },
    ))?;
    Ok(backend)
}

fn js_string(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value.clone(),
        Value::Array(values) => values
            .iter()
            .map(|value| match value {
                Value::Null => String::new(),
                value => js_string(value),
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use indexmap::IndexMap;
    use seekdeep_code_runtime::{
        CODE_RUNTIME, CodeBindingErrorClass, CodeBindingFunction, CodeRuntimeBackend,
    };
    use serde_json::json;

    use super::*;

    fn namespace(global: &str) -> CodeBindingNamespace {
        CodeBindingNamespace {
            global: global.to_owned(),
            functions: IndexMap::<String, CodeBindingFunction>::new(),
            error_class: None,
        }
    }

    async fn run_program(config: WorkerThreadCodeRuntimeConfig, program: &str) -> CodeRunResult {
        WorkerThreadCodeRuntime::new(&config)
            .unwrap()
            .run(CodeRunRequest {
                program: program.to_owned(),
                bindings: Vec::new(),
                signal: None,
            })
            .await
            .unwrap()
    }

    async fn run_program_with_bindings(
        config: WorkerThreadCodeRuntimeConfig,
        program: &str,
        bindings: Vec<CodeBindingNamespace>,
    ) -> CodeRunResult {
        WorkerThreadCodeRuntime::new(&config)
            .unwrap()
            .run(CodeRunRequest {
                program: program.to_owned(),
                bindings,
                signal: None,
            })
            .await
            .unwrap()
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

    #[tokio::test]
    async fn host_binding_tasks_survive_the_program_worker_runtime() {
        let host_thread = std::thread::current().id();
        let observed = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let spawned = Arc::new(parking_lot::Mutex::new(None));
        let release = Arc::new(tokio::sync::Notify::new());
        let binding: CodeBindingFunction = Arc::new({
            let observed = observed.clone();
            let spawned = spawned.clone();
            let release = release.clone();
            move |_| {
                observed.lock().push(std::thread::current().id());
                let observed = observed.clone();
                let spawned = spawned.clone();
                let release = release.clone();
                Box::pin(async move {
                    observed.lock().push(std::thread::current().id());
                    *spawned.lock() = Some(tokio::spawn(async move {
                        release.notified().await;
                        "host work settled"
                    }));
                    Ok(json!("accepted"))
                })
            }
        });
        let result = run_program_with_bindings(
            WorkerThreadCodeRuntimeConfig::default(),
            "return await tools.start({});",
            tools(IndexMap::from([("start".to_owned(), binding)])),
        )
        .await;
        assert_eq!(result.value, Some(json!("accepted")));
        assert!(result.error.is_none());
        let task = spawned.lock().take().expect("host task was started");
        release.notify_one();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), task)
                .await
                .expect("host task must remain runnable")
                .expect("worker exit must not abort a host-owned task"),
            "host work settled"
        );
        assert_eq!(&*observed.lock(), &[host_thread, host_thread]);
    }

    #[tokio::test]
    async fn host_bindings_start_in_order_through_their_first_await() {
        let order = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let release = Arc::new(tokio::sync::Notify::new());
        let binding: CodeBindingFunction = Arc::new({
            let order = order.clone();
            move |argument| {
                let ordinal = argument["ordinal"].as_u64().unwrap();
                order.lock().push(format!("invoke-{ordinal}"));
                let order = order.clone();
                let release = release.clone();
                Box::pin(async move {
                    order.lock().push(format!("poll-{ordinal}"));
                    if ordinal == 0 {
                        release.notified().await;
                    } else {
                        release.notify_one();
                    }
                    Ok(json!(ordinal))
                })
            }
        });
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            run_program_with_bindings(
                WorkerThreadCodeRuntimeConfig::default(),
                "return await Promise.all([tools.call({ordinal: 0}), tools.call({ordinal: 1})]);",
                tools(IndexMap::from([("call".to_owned(), binding)])),
            ),
        )
        .await
        .expect("suspended host calls must allow later submissions");
        assert_eq!(result.value, Some(json!([0, 1])));
        assert_eq!(
            &*order.lock(),
            &["invoke-0", "poll-0", "invoke-1", "poll-1"]
        );
    }

    #[tokio::test]
    async fn worker_abort_does_not_drop_an_accepted_host_binding() {
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let finished = Arc::new(tokio::sync::Notify::new());
        let binding: CodeBindingFunction = Arc::new({
            let started = started.clone();
            let release = release.clone();
            let finished = finished.clone();
            move |_| {
                let started = started.clone();
                let release = release.clone();
                let finished = finished.clone();
                Box::pin(async move {
                    started.notify_one();
                    release.notified().await;
                    finished.notify_one();
                    Ok(json!("host finished"))
                })
            }
        });
        let signal = seekdeep_llm::AbortSignal::default();
        let request = CodeRunRequest {
            program: "return await tools.wait({});".to_owned(),
            bindings: tools(IndexMap::from([("wait".to_owned(), binding)])),
            signal: Some(signal.clone()),
        };
        let backend =
            WorkerThreadCodeRuntime::new(&WorkerThreadCodeRuntimeConfig::default()).unwrap();
        let running = tokio::spawn(async move { backend.run(request).await });
        tokio::time::timeout(std::time::Duration::from_secs(2), started.notified())
            .await
            .expect("host binding started");
        signal.abort_with_reason(json!("stop program"));
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), running)
            .await
            .expect("worker abort")
            .unwrap()
            .unwrap();
        assert_eq!(result.error.unwrap().kind, CodeRunFailureKind::Abort);
        release.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(2), finished.notified())
            .await
            .expect("accepted host binding survived worker abort");
    }

    #[test]
    fn pure_programs_do_not_require_a_host_tokio_runtime() {
        let backend =
            WorkerThreadCodeRuntime::new(&WorkerThreadCodeRuntimeConfig::default()).unwrap();
        let result = futures::executor::block_on(backend.run(CodeRunRequest {
            program: "return 42".to_owned(),
            bindings: Vec::new(),
            signal: None,
        }))
        .unwrap();
        assert_eq!(result.value, Some(json!(42)));
        assert!(result.error.is_none());
    }

    #[test]
    fn resolves_defaults_and_rejects_invalid_caps() {
        let runtime =
            WorkerThreadCodeRuntime::new(&WorkerThreadCodeRuntimeConfig::default()).unwrap();
        assert!((runtime.config.compute_ms - 60_000.0).abs() < f64::EPSILON);
        assert!((runtime.config.max_wall_ms - 600_000.0).abs() < f64::EPSILON);
        assert_eq!(runtime.config.max_output_bytes, 67_108_864);
        assert!((runtime.config.max_old_generation_size_mb - 512.0).abs() < f64::EPSILON);

        for config in [
            WorkerThreadCodeRuntimeConfig {
                compute_ms: Some(-1.0),
                ..WorkerThreadCodeRuntimeConfig::default()
            },
            WorkerThreadCodeRuntimeConfig {
                max_output_bytes: Some(3.0),
                ..WorkerThreadCodeRuntimeConfig::default()
            },
            WorkerThreadCodeRuntimeConfig {
                max_output_bytes: Some(4.5),
                ..WorkerThreadCodeRuntimeConfig::default()
            },
            WorkerThreadCodeRuntimeConfig {
                max_wall_ms: Some(2_147_483_648.0),
                ..WorkerThreadCodeRuntimeConfig::default()
            },
        ] {
            assert!(WorkerThreadCodeRuntime::new(&config).is_err());
        }
        assert!(
            WorkerThreadCodeRuntime::new(&WorkerThreadCodeRuntimeConfig {
                max_wall_ms: Some(MAX_TIMER_DELAY_MS),
                ..WorkerThreadCodeRuntimeConfig::default()
            })
            .is_ok()
        );
    }

    #[test]
    fn validates_namespace_and_typed_error_contracts() {
        for global in ["not valid!", "await", "$tools", "a$b", "lambda", "console"] {
            assert!(WorkerThreadCodeRuntime::validate_bindings(&[namespace(global)]).is_err());
        }
        assert!(
            WorkerThreadCodeRuntime::validate_bindings(&[namespace("tools"), namespace("tools")])
                .is_err()
        );
        let mut typed = namespace("tools");
        typed.error_class = Some(CodeBindingErrorClass {
            name: "ToolCallError".to_owned(),
            member_name_property: "toolName".to_owned(),
        });
        assert!(WorkerThreadCodeRuntime::validate_bindings(&[typed.clone()]).is_ok());
        for (name, member) in [
            ("not valid!", "toolName"),
            ("await", "toolName"),
            ("console", "toolName"),
            ("tools", "toolName"),
            ("CallError", ""),
            ("CallError", "message"),
            ("CallError", "args"),
            ("CallError", "__dict__"),
        ] {
            let mut invalid = namespace("tools");
            invalid.error_class = Some(CodeBindingErrorClass {
                name: name.to_owned(),
                member_name_property: member.to_owned(),
            });
            assert!(WorkerThreadCodeRuntime::validate_bindings(&[invalid]).is_err());
        }
    }

    #[tokio::test]
    async fn mounts_runs_isolates_preabort_and_disposes() {
        let context = Context::new();
        let backend = install(&context, &WorkerThreadCodeRuntimeConfig::default()).unwrap();
        let runtime = context.get(CODE_RUNTIME).unwrap();
        assert_eq!(runtime.language(), "typescript");
        assert_eq!(runtime.isolation(), "worker-thread");
        assert_eq!(
            runtime
                .run(CodeRunRequest {
                    program: "globalThis.leak = 'x'; return 1".to_owned(),
                    bindings: Vec::new(),
                    signal: None,
                })
                .await
                .unwrap()
                .value,
            Some(json!(1))
        );
        assert_eq!(
            runtime
                .run(CodeRunRequest {
                    program: "return typeof globalThis.leak".to_owned(),
                    bindings: Vec::new(),
                    signal: None,
                })
                .await
                .unwrap()
                .value,
            Some(json!("undefined"))
        );

        let signal = seekdeep_llm::AbortSignal::default();
        signal.abort_with_reason(json!({ "kind": "caller" }));
        let aborted = runtime
            .run(CodeRunRequest {
                program: "return 1".to_owned(),
                bindings: Vec::new(),
                signal: Some(signal),
            })
            .await
            .unwrap();
        assert_eq!(aborted.error.unwrap().kind, CodeRunFailureKind::Abort);

        let running = runtime.clone();
        let inflight = tokio::spawn(async move {
            running
                .run(CodeRunRequest {
                    program: "for (;;) {}".to_owned(),
                    bindings: Vec::new(),
                    signal: None,
                })
                .await
                .unwrap()
        });
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        backend.dispose().await;
        let disposed = inflight.await.unwrap();
        assert_eq!(
            disposed.error,
            Some(CodeRunFailure {
                kind: CodeRunFailureKind::Abort,
                message: "runtime disposed".to_owned(),
            })
        );
        assert!(backend.state.live.lock().is_empty());
        assert!(
            backend
                .run(CodeRunRequest {
                    program: "return 1".to_owned(),
                    bindings: Vec::new(),
                    signal: None,
                })
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn applies_exact_combined_output_boundaries_end_to_end() {
        let exact_value = run_program(
            WorkerThreadCodeRuntimeConfig {
                max_output_bytes: Some(7.0),
                ..WorkerThreadCodeRuntimeConfig::default()
            },
            "return '€'",
        )
        .await;
        assert_eq!(
            exact_value,
            CodeRunResult {
                value: Some(json!("€")),
                logs: Vec::new(),
                error: None,
            }
        );
        assert_eq!(
            run_program(
                WorkerThreadCodeRuntimeConfig {
                    max_output_bytes: Some(6.0),
                    ..WorkerThreadCodeRuntimeConfig::default()
                },
                "return '€'",
            )
            .await
            .error
            .unwrap()
            .kind,
            CodeRunFailureKind::OutputLimit
        );

        let combined = run_program(
            WorkerThreadCodeRuntimeConfig {
                max_output_bytes: Some(11.0),
                ..WorkerThreadCodeRuntimeConfig::default()
            },
            "console.log('abc'); return 'xy'",
        )
        .await;
        assert_eq!(combined.logs, ["abc"]);
        assert_eq!(combined.value, Some(json!("xy")));
        assert!(combined.error.is_none());
        assert_eq!(
            run_program(
                WorkerThreadCodeRuntimeConfig {
                    max_output_bytes: Some(10.0),
                    ..WorkerThreadCodeRuntimeConfig::default()
                },
                "console.log('abc'); return 'xy'",
            )
            .await
            .error
            .unwrap()
            .kind,
            CodeRunFailureKind::OutputLimit
        );
    }

    #[tokio::test]
    async fn caps_preworker_failures_logs_and_diagnostics_end_to_end() {
        let oversized_log = run_program(
            WorkerThreadCodeRuntimeConfig {
                max_output_bytes: Some(96.0),
                ..WorkerThreadCodeRuntimeConfig::default()
            },
            "console.log(`start-${`😀\"\\\\\\n`.repeat(100)}`); return null",
        )
        .await;
        assert_eq!(
            oversized_log.error.as_ref().unwrap().kind,
            CodeRunFailureKind::OutputLimit
        );
        assert_eq!(oversized_log.logs.len(), 1);
        assert!(oversized_log.logs[0].starts_with("start-"));
        let counted = serde_json::to_vec(&oversized_log.logs).unwrap().len()
            + serde_json::to_vec(&oversized_log.error.unwrap().message)
                .unwrap()
                .len();
        assert!(counted <= 96);

        let minimal = run_program(
            WorkerThreadCodeRuntimeConfig {
                max_output_bytes: Some(4.0),
                ..WorkerThreadCodeRuntimeConfig::default()
            },
            "enum E { A }; return E.A",
        )
        .await;
        assert_eq!(
            minimal.error.as_ref().unwrap().kind,
            CodeRunFailureKind::OutputLimit
        );
        assert_eq!(
            serde_json::to_vec(&minimal.logs).unwrap().len()
                + serde_json::to_vec(&minimal.error.unwrap().message)
                    .unwrap()
                    .len(),
            4
        );

        let diagnostic = run_program(
            WorkerThreadCodeRuntimeConfig {
                max_output_bytes: Some(11.0),
                ..WorkerThreadCodeRuntimeConfig::default()
            },
            "console.log('abc'); throw 'xy'",
        )
        .await;
        assert_eq!(diagnostic.logs, ["abc"]);
        assert_eq!(
            diagnostic.error,
            Some(CodeRunFailure {
                kind: CodeRunFailureKind::Exception,
                message: "xy".to_owned(),
            })
        );
    }

    #[tokio::test]
    async fn maps_compute_wall_and_abort_controls_through_public_backend() {
        let compute = WorkerThreadCodeRuntime::new(&WorkerThreadCodeRuntimeConfig {
            compute_ms: Some(25.0),
            max_wall_ms: Some(2_000.0),
            ..WorkerThreadCodeRuntimeConfig::default()
        })
        .unwrap();
        let exhausted = compute
            .run(CodeRunRequest {
                program: "for (;;) {}".to_owned(),
                bindings: Vec::new(),
                signal: None,
            })
            .await
            .unwrap();
        assert_eq!(
            exhausted.error,
            Some(CodeRunFailure {
                kind: CodeRunFailureKind::Timeout,
                message: "compute budget exhausted (25ms busy)".to_owned(),
            })
        );

        let wall = WorkerThreadCodeRuntime::new(&WorkerThreadCodeRuntimeConfig {
            compute_ms: Some(2_000.0),
            max_wall_ms: Some(25.0),
            ..WorkerThreadCodeRuntimeConfig::default()
        })
        .unwrap();
        let idled = wall
            .run(CodeRunRequest {
                program: "return await new Promise(() => {})".to_owned(),
                bindings: Vec::new(),
                signal: None,
            })
            .await
            .unwrap();
        assert_eq!(
            idled.error,
            Some(CodeRunFailure {
                kind: CodeRunFailureKind::Timeout,
                message: "wall-clock ceiling reached (25ms)".to_owned(),
            })
        );

        let signal = seekdeep_llm::AbortSignal::default();
        let cancelling = signal.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            cancelling.abort_with_reason(json!("user-cancel"));
        });
        let aborting = WorkerThreadCodeRuntime::new(&WorkerThreadCodeRuntimeConfig {
            compute_ms: Some(2_000.0),
            max_wall_ms: Some(2_000.0),
            ..WorkerThreadCodeRuntimeConfig::default()
        })
        .unwrap();
        let aborted = aborting
            .run(CodeRunRequest {
                program: "for (;;) {}".to_owned(),
                bindings: Vec::new(),
                signal: Some(signal),
            })
            .await
            .unwrap();
        assert_eq!(
            aborted.error,
            Some(CodeRunFailure {
                kind: CodeRunFailureKind::Abort,
                message: "user-cancel".to_owned(),
            })
        );
    }

    #[tokio::test]
    async fn contains_forged_parent_port_traffic_and_honors_terminal_messages() {
        let mut functions = IndexMap::new();
        functions.insert(
            "real".to_owned(),
            Arc::new(|_| {
                Box::pin(async { Ok(json!("still-works")) })
                    as seekdeep_code_runtime::CodeBindingFuture
            }) as CodeBindingFunction,
        );
        let survived = run_program_with_bindings(
            WorkerThreadCodeRuntimeConfig::default(),
            r"
                const { parentPort } = await import('node:worker_threads');
                parentPort.postMessage({ type: 'call', id: 7777, global: 'tools', name: 'missing', args: {} });
                parentPort.postMessage({ type: 'call', id: 7777, global: 'tools', name: 'missing', args: {} });
                parentPort.postMessage({ type: 'call', id: 7778, global: 'tools', name: 'constructor', args: {} });
                for (const junk of [
                  null, 42, 'junk', [], { type: 'nope' }, { type: 'call' },
                  { type: 'call', id: 'x', global: 'tools', name: 'real', args: {} },
                  { type: 'call', id: 1e9, global: 7, name: 'real', args: {} },
                  { type: 'call', id: 1e9, global: 'tools', name: 7, args: {} },
                  { type: 'log' }, { type: 'log', text: null }, { type: 'log', text: 7 },
                  { type: 'done', error: 5 },
                  { type: 'done', error: { kind: 'exception', message: 5 } },
                  { type: 'done', error: { kind: 'invented', message: 'bad kind' } },
                ]) parentPort.postMessage(junk);
                return await tools.real({});
            ",
            tools(functions),
        )
        .await;
        assert_eq!(survived.value, Some(json!("still-works")));
        assert!(survived.error.is_none());
        assert!(survived.logs.is_empty());

        let forged_success = run_program(
            WorkerThreadCodeRuntimeConfig::default(),
            r"
                const { parentPort } = await import('node:worker_threads');
                parentPort.postMessage({ type: 'done', value: ['done'] });
                for (;;) {}
            ",
        )
        .await;
        assert_eq!(
            forged_success,
            CodeRunResult {
                value: Some(json!("done")),
                logs: Vec::new(),
                error: None,
            }
        );

        let forged_failure = run_program(
            WorkerThreadCodeRuntimeConfig::default(),
            r"
                const { parentPort } = await import('node:worker_threads');
                parentPort.postMessage({
                  type: 'done',
                  value: 'lied',
                  error: { kind: 'exception', message: 'fake failure' },
                });
                return 'honest';
            ",
        )
        .await;
        assert_eq!(
            forged_failure.error,
            Some(CodeRunFailure {
                kind: CodeRunFailureKind::Exception,
                message: "fake failure".to_owned(),
            })
        );
    }

    #[tokio::test]
    async fn recaps_forged_logs_completions_diagnostics_and_limit_signals() {
        let flooded = run_program(
            WorkerThreadCodeRuntimeConfig {
                max_output_bytes: Some(200.0),
                ..WorkerThreadCodeRuntimeConfig::default()
            },
            r"
                const { parentPort } = await import('node:worker_threads');
                for (let i = 0; i < 50; i++) {
                  parentPort.postMessage({ type: 'log', text: 'F'.repeat(100), forged: true });
                }
                parentPort.postMessage({ type: 'done', value: ['V'.repeat(100000)] });
                for (;;) {}
            ",
        )
        .await;
        assert!(flooded.value.is_none());
        assert_eq!(
            flooded.error,
            Some(CodeRunFailure {
                kind: CodeRunFailureKind::OutputLimit,
                message: "outer output exceeded 200 bytes".to_owned(),
            })
        );
        assert!(crate::output_json::json_value_bytes_up_to(&json!(flooded.logs), 199).is_some());

        let oversized_done = run_program(
            WorkerThreadCodeRuntimeConfig {
                max_output_bytes: Some(64.0),
                ..WorkerThreadCodeRuntimeConfig::default()
            },
            r"
                const { parentPort } = await import('node:worker_threads');
                parentPort.postMessage({ type: 'done', value: ['V'.repeat(100_000)] });
                for (;;) {}
            ",
        )
        .await;
        assert_eq!(
            oversized_done.error,
            Some(CodeRunFailure {
                kind: CodeRunFailureKind::OutputLimit,
                message: "outer output exceeded 64 bytes".to_owned(),
            })
        );

        let oversized_error = run_program(
            WorkerThreadCodeRuntimeConfig {
                max_output_bytes: Some(64.0),
                ..WorkerThreadCodeRuntimeConfig::default()
            },
            r"
                const { parentPort } = await import('node:worker_threads');
                parentPort.postMessage({
                  type: 'done',
                  error: { kind: 'exception', message: '€'.repeat(1000) },
                });
                for (;;) {}
            ",
        )
        .await;
        assert_eq!(oversized_error.error, oversized_done.error);

        let signalled = run_program(
            WorkerThreadCodeRuntimeConfig::default(),
            r"
                const { parentPort } = await import('node:worker_threads');
                parentPort.postMessage({ type: 'output-limit' });
                for (;;) {}
            ",
        )
        .await;
        assert_eq!(
            signalled.error,
            Some(CodeRunFailure {
                kind: CodeRunFailureKind::OutputLimit,
                message: "outer output exceeded 67108864 bytes".to_owned(),
            })
        );
    }

    #[tokio::test]
    async fn rejects_forged_lossy_calls_without_invoking_the_host() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let called = calls.clone();
        let mut functions = IndexMap::new();
        functions.insert(
            "never".to_owned(),
            Arc::new(move |_| {
                called.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Box::pin(async { Ok(Value::Null) }) as seekdeep_code_runtime::CodeBindingFuture
            }) as CodeBindingFunction,
        );
        let result = run_program_with_bindings(
            WorkerThreadCodeRuntimeConfig::default(),
            r"
                const { parentPort } = await import('node:worker_threads');
                const forged = (id, args) => new Promise((resolve) => {
                  const receive = (message) => {
                    if (message?.type !== 'reply' || message.id !== id) return;
                    parentPort.off('message', receive);
                    resolve(message);
                  };
                  parentPort.on('message', receive);
                  parentPort.postMessage({ type: 'call', id, global: 'tools', name: 'never', args });
                });
                const sparse = []; sparse.length = 1;
                const cycle = {}; cycle.self = cycle;
                return await Promise.all([
                  forged(8001, new Date()),
                  forged(8002, -0),
                  forged(8003, sparse),
                  forged(8004, cycle),
                ]);
            ",
            tools(functions),
        )
        .await;
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 0);
        assert_eq!(
            result.value,
            Some(json!([
                { "type": "reply", "id": 8001, "ok": false, "message": "binding arguments must be lossless JSON" },
                { "type": "reply", "id": 8002, "ok": false, "message": "binding arguments must be lossless JSON" },
                { "type": "reply", "id": 8003, "ok": false, "message": "binding arguments must be lossless JSON" },
                { "type": "reply", "id": 8004, "ok": false, "message": "binding arguments must be lossless JSON" },
            ]))
        );
        assert!(result.error.is_none());
    }

    #[tokio::test]
    async fn revalidates_lossy_and_deep_forged_completions() {
        let lossy = run_program(
            WorkerThreadCodeRuntimeConfig::default(),
            r"
                const { parentPort } = await import('node:worker_threads');
                parentPort.postMessage({ type: 'done', value: -0 });
                for (;;) {}
            ",
        )
        .await;
        assert_eq!(
            lossy.error,
            Some(CodeRunFailure {
                kind: CodeRunFailureKind::InvalidOutput,
                message: "program completion must be lossless JSON".to_owned(),
            })
        );

        let deep = run_program(
            WorkerThreadCodeRuntimeConfig {
                max_wall_ms: Some(2_000.0),
                ..WorkerThreadCodeRuntimeConfig::default()
            },
            r"
                const { parentPort } = await import('node:worker_threads');
                const value = [];
                for (let depth = 0; depth < 3_000; depth++) {
                  value.push({ kind: 'array', length: 1 });
                }
                value.push(null);
                setTimeout(() => { parentPort.postMessage({ type: 'done', value }) }, 25);
                await new Promise(() => {});
            ",
        )
        .await;
        assert!(deep.error.is_none());
        let mut cursor = deep.value.as_ref();
        let mut depth = 0;
        while let Some(Value::Array(values)) = cursor {
            assert_eq!(values.len(), 1);
            cursor = values.first();
            depth += 1;
        }
        assert_eq!(depth, 3_000);
        assert_eq!(cursor, Some(&Value::Null));
        std::mem::forget(deep);
    }
}
