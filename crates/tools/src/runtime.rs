//! Scope-aware tool registry and staged execution pipeline.

use std::{
    collections::HashSet,
    future::Future,
    panic::{AssertUnwindSafe, catch_unwind},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use futures::FutureExt;
use indexmap::IndexMap;
use parking_lot::Mutex;
use seekdeep_cordis::{
    Context, CordisError, EventArgs, EventOptions, EventReply, events::Next, fiber::EffectHandle,
};
use seekdeep_llm::{AbortSignal, CallId, ContentBlock, Message, ToolSchema};
use seekdeep_scope::{
    ScopeKey, scope_of, scope_target,
    store::{
        AnonymousEntries, EntryUndo, LayerEffectOptions, NamedEntries, ScopeLayer, ScopedLayers,
    },
};
use serde_json::{Map, Value};
use thiserror::Error;
use uuid::Uuid;

use crate::{JsonSchemaNode, ToolSdkSchema, validate_json_schema_value_at};

/// Canonical error code for cancellation after a body was invoked.
pub const TOOL_ABORTED: &str = "ABORTED";
/// Canonical error code for cancellation before a body was invoked.
pub const TOOL_ABORTED_BEFORE_DISPATCH: &str = "ABORTED_BEFORE_DISPATCH";
/// Reserved Code Mode transport name.
pub const RUN_CODE_NAME: &str = "run_code";

/// Boxed asynchronous tool body.
pub type ToolExecuteFuture = Pin<Box<dyn Future<Output = anyhow::Result<Value>> + Send + 'static>>;
/// Tool body callback.
pub type ToolExecute =
    Arc<dyn Fn(Value, ToolExecution) -> ToolExecuteFuture + Send + Sync + 'static>;
/// Pure successful-value renderer.
pub type ToolRender =
    Arc<dyn Fn(&Value, &Value) -> anyhow::Result<Vec<ContentBlock>> + Send + Sync + 'static>;
/// Pure presentation-metadata projector.
pub type ToolPresentationMeta =
    Arc<dyn Fn(&Value, &Value) -> anyhow::Result<Value> + Send + Sync + 'static>;
/// Final content transform snapshotted when an execution starts.
pub type ToolContentFinalizer = Arc<
    dyn Fn(&ToolExecution, &ToolExecutionResult) -> anyhow::Result<Option<Vec<ContentBlock>>>
        + Send
        + Sync
        + 'static,
>;
/// Fail-closed sibling-overlap classifier.
pub type ToolConcurrencyClassifier = Arc<dyn Fn(&Value) -> bool + Send + Sync + 'static>;
/// Monotonic execution guard. A returned reason denies the call.
pub type ToolGuard = Arc<dyn Fn(&ToolExecution) -> Option<String> + Send + Sync + 'static>;

/// Typed continuation for `tools/pre-execute` middleware.
pub struct PreToolNext(Next);

impl PreToolNext {
    /// Delegates to the remaining policy chain.
    ///
    /// # Errors
    ///
    /// Returns a downstream failure or invalid reply type.
    pub async fn run(self) -> anyhow::Result<PreToolDecision> {
        self.0
            .run()
            .await?
            .downcast::<PreToolDecision>()
            .map(|decision| (*decision).clone())
            .ok_or_else(|| anyhow::anyhow!("tools/pre-execute returned an invalid decision"))
    }
}

/// Typed continuation for `tools/execute` middleware.
pub struct ExecuteToolNext(Next);

impl ExecuteToolNext {
    /// Delegates to the remaining around-dispatch chain or tool body.
    ///
    /// # Errors
    ///
    /// Returns a downstream failure or invalid reply type.
    pub async fn run(self) -> anyhow::Result<ToolExecutionResult> {
        self.0
            .run()
            .await?
            .downcast::<ToolExecutionResult>()
            .map(|result| (*result).clone())
            .ok_or_else(|| anyhow::anyhow!("tools/execute returned an invalid result"))
    }
}

/// Typed continuation for `tools/post-execute` middleware.
pub struct PostToolNext(Next);

impl PostToolNext {
    /// Delegates to the remaining post-policy chain.
    ///
    /// # Errors
    ///
    /// Returns a downstream failure or invalid reply type.
    pub async fn run(self) -> anyhow::Result<PostToolDecision> {
        self.0
            .run()
            .await?
            .downcast::<PostToolDecision>()
            .map(|decision| (*decision).clone())
            .ok_or_else(|| anyhow::anyhow!("tools/post-execute returned an invalid decision"))
    }
}

/// Tool-owned canonical output contract.
#[derive(Clone)]
pub struct ToolOutputDefinition {
    /// Supported JSON Schema enforced against every successful value.
    pub schema: Arc<JsonSchemaNode>,
    /// Pure Native/model content projection.
    pub render: ToolRender,
    /// Optional pure replayable UI metadata projection.
    pub presentation_meta: Option<ToolPresentationMeta>,
}

impl std::fmt::Debug for ToolOutputDefinition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolOutputDefinition")
            .field("schema", &self.schema)
            .field("presentation_meta", &self.presentation_meta.is_some())
            .finish_non_exhaustive()
    }
}

impl ToolOutputDefinition {
    /// Builds a canonical output declaration.
    #[must_use]
    pub fn new(schema: Arc<JsonSchemaNode>, render: ToolRender) -> Self {
        Self {
            schema,
            render,
            presentation_meta: None,
        }
    }

    /// Adds the top-level presentation metadata projector.
    #[must_use]
    pub fn presentation_meta(mut self, projector: ToolPresentationMeta) -> Self {
        self.presentation_meta = Some(projector);
        self
    }
}

/// A registered tool: model schema, output contract, and execution behavior.
#[derive(Clone)]
pub struct ToolDefinition {
    /// Registered name.
    pub name: String,
    /// Model-facing purpose.
    pub description: String,
    /// Object-rooted model arguments schema.
    pub parameters: Map<String, Value>,
    /// Canonical successful output contract.
    pub output: ToolOutputDefinition,
    /// Accepted-call body.
    pub execute: ToolExecute,
    /// Optional final content transform.
    pub finalize_content: Option<ToolContentFinalizer>,
    /// Cooperative timeout declaration in milliseconds.
    pub timeout_ms: Option<f64>,
    /// Optional overlap classifier.
    pub is_concurrency_safe: Option<ToolConcurrencyClassifier>,
}

impl std::fmt::Debug for ToolDefinition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolDefinition")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("parameters", &self.parameters)
            .field("output", &self.output)
            .field("finalize_content", &self.finalize_content.is_some())
            .field("timeout_ms", &self.timeout_ms)
            .field("is_concurrency_safe", &self.is_concurrency_safe.is_some())
            .finish_non_exhaustive()
    }
}

impl ToolDefinition {
    /// Builds the mandatory portion of a tool definition.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: Map<String, Value>,
        output: ToolOutputDefinition,
        execute: ToolExecute,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
            output,
            execute,
            finalize_content: None,
            timeout_ms: None,
            is_concurrency_safe: None,
        }
    }

    /// Adds a final content transform.
    #[must_use]
    pub fn finalize_content(mut self, finalizer: ToolContentFinalizer) -> Self {
        self.finalize_content = Some(finalizer);
        self
    }

    /// Declares a cooperative timeout budget in milliseconds.
    #[must_use]
    pub fn timeout_ms(mut self, timeout_ms: f64) -> Self {
        self.timeout_ms = Some(timeout_ms);
        self
    }

    /// Adds the fail-closed overlap classifier.
    #[must_use]
    pub fn concurrency_safe(mut self, classifier: ToolConcurrencyClassifier) -> Self {
        self.is_concurrency_safe = Some(classifier);
        self
    }
}

/// How a scope presents registered tools to its model.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToolPresentationMode {
    /// Every visible tool is a native model tool.
    #[default]
    Native,
    /// Only the reserved Code Mode transport is callable directly.
    Code,
    /// Native tools and the Code Mode transport are both visible.
    Both,
}

/// Tool runtime configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolRuntimeConfig {
    /// Deployment-default presentation.
    pub mode: ToolPresentationMode,
    /// Overlap cap used by Code Mode sub-dispatches.
    pub max_parallel_sub_calls: usize,
}

impl Default for ToolRuntimeConfig {
    fn default() -> Self {
        Self {
            mode: ToolPresentationMode::Native,
            max_parallel_sub_calls: 10,
        }
    }
}

/// Per-scope intersection filter over inherited tools.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolRestriction {
    /// Inherited names retained by this restriction.
    pub allow: Option<Vec<String>>,
    /// Inherited names removed by this restriction.
    pub deny: Option<Vec<String>>,
}

#[derive(Clone, Debug)]
struct CompiledToolRestriction {
    allow: Option<HashSet<String>>,
    deny: Option<HashSet<String>>,
}

/// Opaque same-process execution correlation identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ToolExecutionToken(Uuid);

impl ToolExecutionToken {
    fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

/// Caller-authored tool invocation before registry identity is assigned.
#[derive(Clone, Debug)]
pub struct ToolExecutionInput {
    /// Provider/model call identity.
    pub call_id: CallId,
    /// Root model-requested call, omitted for a root execution.
    pub root_call_id: Option<CallId>,
    /// Requested tool name.
    pub name: String,
    /// Parsed lossless JSON arguments.
    pub arguments: Value,
    /// Calling agent scope.
    pub agent: Option<ScopeKey>,
    /// Enclosing transport execution for nested dispatch.
    pub parent: Option<ToolExecutionToken>,
    /// Required caller-owned cancellation.
    pub signal: AbortSignal,
}

struct ExecutionState {
    caller_signal: AbortSignal,
    signal: Mutex<AbortSignal>,
    body_invoked: AtomicBool,
    deferred_contexts: Mutex<Vec<Message>>,
    concludes_turn: AtomicBool,
}

/// Registry-owned immutable call identity plus execution-local controls.
#[derive(Clone)]
pub struct ToolExecution {
    /// Provider/model call identity.
    pub call_id: CallId,
    /// Root model-requested call.
    pub root_call_id: CallId,
    /// Requested name.
    pub name: String,
    /// Snapshotted parsed arguments.
    pub arguments: Value,
    /// Calling agent scope.
    pub agent: Option<ScopeKey>,
    /// Enclosing transport token.
    pub parent: Option<ToolExecutionToken>,
    /// Registry-owned identity.
    pub token: ToolExecutionToken,
    state: Arc<ExecutionState>,
}

impl std::fmt::Debug for ToolExecution {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolExecution")
            .field("call_id", &self.call_id)
            .field("root_call_id", &self.root_call_id)
            .field("name", &self.name)
            .field("arguments", &self.arguments)
            .field("agent", &self.agent)
            .field("parent", &self.parent)
            .field("token", &self.token)
            .finish_non_exhaustive()
    }
}

impl ToolExecution {
    /// Current dispatch signal. Around middleware may replace it temporarily.
    #[must_use]
    pub fn signal(&self) -> AbortSignal {
        self.state.signal.lock().clone()
    }

    /// Replaces the around-dispatch signal and returns the prior signal.
    ///
    /// The body always fuses the replacement with the captured caller signal.
    #[must_use]
    pub fn replace_dispatch_signal(&self, signal: AbortSignal) -> AbortSignal {
        std::mem::replace(&mut *self.state.signal.lock(), signal)
    }

    /// Defers one plugin/user message until this result reaches the loop.
    pub fn defer_context(&self, context: Message) {
        self.state.deferred_contexts.lock().push(context);
    }

    /// Marks a successful result as terminal for the current agent turn.
    pub fn conclude_turn(&self) {
        self.state.concludes_turn.store(true, Ordering::Release);
    }
}

/// Structured error metadata for a failed tool call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolErrorInfo {
    /// Error class.
    pub name: String,
    /// Stable machine-readable code.
    pub code: String,
}

/// Canonical failure detail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolFailure {
    /// Human-readable message without the `Error: ` envelope.
    pub message: String,
    /// Optional stable routing information.
    pub info: Option<ToolErrorInfo>,
}

/// Successful canonical execution.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolExecutionSuccess {
    /// Execution-local canonical value.
    pub value: Value,
    /// Native/model projection.
    pub content: Vec<ContentBlock>,
    /// Optional top-level presentation projection.
    pub meta: Option<Value>,
    /// Contexts queued for the next request.
    pub additional_contexts: Vec<Message>,
    /// Whether the agent loop should stop after committing this batch.
    pub concludes_turn: bool,
    canonical_for: Option<ToolExecutionToken>,
}

/// Failed canonical execution.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolExecutionFailure {
    /// Normalized error detail.
    pub error: ToolFailure,
    /// Model-facing error content.
    pub content: Vec<ContentBlock>,
    /// Optional presentation metadata authored by policy.
    pub meta: Option<Value>,
    /// Contexts queued for the next request.
    pub additional_contexts: Vec<Message>,
}

/// Discriminated execution-local outcome.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolExecutionResult {
    /// Validated successful value and projections.
    Success(ToolExecutionSuccess),
    /// Valueless normalized failure.
    Failure(ToolExecutionFailure),
}

impl ToolExecutionResult {
    /// Authors a success for an around-dispatch wrapper. The registry will
    /// revalidate the value and recompute definition-owned projections.
    #[must_use]
    pub fn success(value: Value, content: Vec<ContentBlock>) -> Self {
        Self::Success(ToolExecutionSuccess {
            value,
            content,
            meta: None,
            additional_contexts: Vec::new(),
            concludes_turn: false,
            canonical_for: None,
        })
    }

    /// Authors a normalized failure for an around-dispatch wrapper.
    #[must_use]
    pub fn failure(message: impl Into<String>) -> Self {
        let message = message.into();
        Self::Failure(ToolExecutionFailure {
            content: error_content(&message),
            error: ToolFailure {
                message,
                info: None,
            },
            meta: None,
            additional_contexts: Vec::new(),
        })
    }

    /// Whether this result failed.
    #[must_use]
    pub fn is_error(&self) -> bool {
        matches!(self, Self::Failure(_))
    }

    /// Final model-facing content.
    #[must_use]
    pub fn content(&self) -> &[ContentBlock] {
        match self {
            Self::Success(result) => &result.content,
            Self::Failure(result) => &result.content,
        }
    }

    fn additional_contexts(&self) -> &[Message] {
        match self {
            Self::Success(result) => &result.additional_contexts,
            Self::Failure(result) => &result.additional_contexts,
        }
    }
}

/// Pre-dispatch policy decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreToolDecision {
    /// Run the call.
    Allow,
    /// Deny with model-facing reason.
    Deny {
        /// Denial reason.
        reason: String,
    },
    /// Ask the optional approval service, degrading to denial when absent.
    Ask {
        /// Optional request reason.
        reason: Option<String>,
    },
}

/// Post-dispatch policy decision.
#[derive(Clone, Debug, PartialEq)]
pub enum PostToolDecision {
    /// Accept, optionally replacing content and appending contexts.
    Accept {
        /// Replacement model-facing content.
        content: Option<Vec<ContentBlock>>,
        /// Contexts appended after existing result contexts.
        additional_contexts: Vec<Message>,
    },
    /// Replace a successful canonical value and recompute projections.
    ReplaceValue {
        /// Replacement canonical value.
        value: Value,
        /// Contexts appended after existing result contexts.
        additional_contexts: Vec<Message>,
    },
    /// Turn corrective feedback into a valueless failure.
    Block {
        /// Model-facing correction.
        feedback: Vec<ContentBlock>,
        /// Only contexts explicitly supplied by the blocking policy survive.
        additional_contexts: Vec<Message>,
    },
}

impl Default for PostToolDecision {
    fn default() -> Self {
        Self::Accept {
            content: None,
            additional_contexts: Vec::new(),
        }
    }
}

/// Scheduling classification for one pending call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolExecutionMode {
    /// May overlap with compatible siblings.
    Parallel,
    /// Runs alone and forms an ordering barrier.
    Exclusive,
}

/// Stable registry and output-contract failures.
#[derive(Debug, Error)]
pub enum ToolRuntimeError {
    /// No executable definition is visible.
    #[error("{message}")]
    ToolNotFound {
        /// Stable human-readable error.
        message: String,
    },
    /// A body or post-policy value violated its output declaration.
    #[error("tool {tool_name:?} returned invalid output: {violations}")]
    InvalidToolOutput {
        /// Owning registered name.
        tool_name: String,
        /// Semicolon-separated violations.
        violations: String,
    },
    /// A presentation projector failed.
    #[error("tool {tool_name:?} returned invalid output: output.{projector} failed: {message}")]
    Projection {
        /// Owning registered name.
        tool_name: String,
        /// Projector name.
        projector: &'static str,
        /// Original failure message.
        message: String,
    },
}

impl ToolRuntimeError {
    fn info(&self) -> ToolErrorInfo {
        match self {
            Self::ToolNotFound { .. } => ToolErrorInfo {
                name: "ToolNotFoundError".to_owned(),
                code: "UNKNOWN_TOOL".to_owned(),
            },
            Self::InvalidToolOutput { .. } | Self::Projection { .. } => ToolErrorInfo {
                name: "ToolOutputError".to_owned(),
                code: "INVALID_TOOL_OUTPUT".to_owned(),
            },
        }
    }
}

struct ToolLayer {
    tools: NamedEntries<Arc<ToolDefinition>>,
    restrictions: AnonymousEntries<CompiledToolRestriction>,
    guards: AnonymousEntries<ToolGuard>,
    mode: Arc<Mutex<Option<ToolPresentationMode>>>,
}

impl ToolLayer {
    fn new(scope: Option<ScopeKey>) -> Self {
        Self {
            tools: NamedEntries::new(move |name| {
                if scope.is_some() {
                    anyhow::anyhow!("tool {name:?} is already registered in this scope")
                } else {
                    anyhow::anyhow!(
                        "tool {name:?} is already registered (for a per-agent variant, register through that agent's `agent.ctx` instead)"
                    )
                }
            }),
            restrictions: AnonymousEntries::default(),
            guards: AnonymousEntries::default(),
            mode: Arc::new(Mutex::new(None)),
        }
    }

    fn admits(&self, name: &str) -> bool {
        self.restrictions.values().all(|restriction| {
            restriction
                .allow
                .as_ref()
                .is_none_or(|allow| allow.contains(name))
                && restriction
                    .deny
                    .as_ref()
                    .is_none_or(|deny| !deny.contains(name))
        })
    }

    fn guard_reason(&self, execution: &ToolExecution) -> Option<String> {
        self.guards.values().find_map(|guard| guard(execution))
    }
}

impl ScopeLayer for ToolLayer {
    fn is_empty(&self) -> bool {
        self.tools.is_empty()
            && self.restrictions.is_empty()
            && self.guards.is_empty()
            && self.mode.lock().is_none()
    }
}

struct ToolView {
    visible: IndexMap<String, Arc<ToolDefinition>>,
    known_names: HashSet<String>,
    restrictable_names: HashSet<String>,
}

/// Scope-aware registry and execution pipeline.
pub struct ToolRuntime {
    context: Context,
    layers: ScopedLayers<ToolLayer>,
    default_mode: ToolPresentationMode,
    max_parallel_sub_calls: usize,
}

impl std::fmt::Debug for ToolRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolRuntime")
            .field("default_mode", &self.default_mode)
            .field("max_parallel_sub_calls", &self.max_parallel_sub_calls)
            .finish_non_exhaustive()
    }
}

impl ToolRuntime {
    /// Creates a registry and eagerly constructs its global layer.
    ///
    /// # Errors
    ///
    /// Returns when the Code Mode overlap cap is zero.
    pub fn new(context: Context, config: ToolRuntimeConfig) -> anyhow::Result<Arc<Self>> {
        anyhow::ensure!(
            config.max_parallel_sub_calls > 0,
            "maxParallelSubCalls must be a positive integer"
        );
        let change_context = context.clone();
        let layers = ScopedLayers::try_new(
            |scope| Ok(ToolLayer::new(scope)),
            move || {
                change_context
                    .events()
                    .emit(&change_context, "tools/change", &EventArgs::new())
            },
        )?;
        Ok(Arc::new(Self {
            context,
            layers,
            default_mode: config.mode,
            max_parallel_sub_calls: config.max_parallel_sub_calls,
        }))
    }

    /// Root Cordis context used by registry-wide notifications.
    #[must_use]
    pub fn context(&self) -> &Context {
        &self.context
    }

    /// Registers typed pre-execute waterfall middleware.
    ///
    /// # Errors
    ///
    /// Returns when the owning context is inactive.
    pub fn on_pre_execute<F, Fut>(
        &self,
        context: &Context,
        middleware: F,
        options: EventOptions,
    ) -> Result<EffectHandle, CordisError>
    where
        F: Fn(ToolExecution, PreToolNext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<PreToolDecision>> + Send + 'static,
    {
        self.context.events().on_waterfall(
            context,
            "tools/pre-execute",
            move |_, args, next| {
                let Some(execution) = args.get::<ToolExecution>(0) else {
                    return Box::pin(async {
                        Err(anyhow::anyhow!(
                            "tools/pre-execute is missing its execution"
                        ))
                    });
                };
                let future = middleware((*execution).clone(), PreToolNext(next));
                Box::pin(async move {
                    let decision = AssertUnwindSafe(future)
                        .catch_unwind()
                        .await
                        .map_err(|panic| anyhow::anyhow!(panic_message(&panic)))??;
                    Ok(EventReply::Value(Arc::new(decision)))
                })
            },
            options,
        )
    }

    /// Registers typed around-dispatch waterfall middleware.
    ///
    /// # Errors
    ///
    /// Returns when the owning context is inactive.
    pub fn on_execute<F, Fut>(
        &self,
        context: &Context,
        middleware: F,
        options: EventOptions,
    ) -> Result<EffectHandle, CordisError>
    where
        F: Fn(ToolExecution, ExecuteToolNext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<ToolExecutionResult>> + Send + 'static,
    {
        self.context.events().on_waterfall(
            context,
            "tools/execute",
            move |_, args, next| {
                let Some(execution) = args.get::<ToolExecution>(0) else {
                    return Box::pin(async {
                        Err(anyhow::anyhow!("tools/execute is missing its execution"))
                    });
                };
                let future = middleware((*execution).clone(), ExecuteToolNext(next));
                Box::pin(async move {
                    let result = AssertUnwindSafe(future)
                        .catch_unwind()
                        .await
                        .map_err(|panic| anyhow::anyhow!(panic_message(&panic)))??;
                    Ok(EventReply::Value(Arc::new(result)))
                })
            },
            options,
        )
    }

    /// Registers typed post-execute waterfall middleware.
    ///
    /// # Errors
    ///
    /// Returns when the owning context is inactive.
    pub fn on_post_execute<F, Fut>(
        &self,
        context: &Context,
        middleware: F,
        options: EventOptions,
    ) -> Result<EffectHandle, CordisError>
    where
        F: Fn(ToolExecution, ToolExecutionResult, PostToolNext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<PostToolDecision>> + Send + 'static,
    {
        self.context.events().on_waterfall(
            context,
            "tools/post-execute",
            move |_, args, next| {
                let Some(execution) = args.get::<ToolExecution>(0) else {
                    return Box::pin(async {
                        Err(anyhow::anyhow!(
                            "tools/post-execute is missing its execution"
                        ))
                    });
                };
                let Some(result) = args.get::<ToolExecutionResult>(1) else {
                    return Box::pin(async {
                        Err(anyhow::anyhow!("tools/post-execute is missing its result"))
                    });
                };
                let future =
                    middleware((*execution).clone(), (*result).clone(), PostToolNext(next));
                Box::pin(async move {
                    let decision = AssertUnwindSafe(future)
                        .catch_unwind()
                        .await
                        .map_err(|panic| anyhow::anyhow!(panic_message(&panic)))??;
                    Ok(EventReply::Value(Arc::new(decision)))
                })
            },
            options,
        )
    }

    /// Registers a typed final-result observer.
    ///
    /// Observer failures are contained by execution-time result dispatch.
    ///
    /// # Errors
    ///
    /// Returns when the owning context is inactive.
    pub fn on_result<F, Fut>(
        &self,
        context: &Context,
        observer: F,
        options: EventOptions,
    ) -> Result<EffectHandle, CordisError>
    where
        F: Fn(ToolExecution, ToolExecutionResult) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        self.context.events().on(
            context,
            "tools/result",
            move |_, args| {
                let execution = args.get::<ToolExecution>(0);
                let result = args.get::<ToolExecutionResult>(1);
                let future = match (execution, result) {
                    (Some(execution), Some(result)) => {
                        observer((*execution).clone(), (*result).clone())
                    }
                    _ => {
                        return Box::pin(async {
                            Err(anyhow::anyhow!("tools/result is missing its arguments"))
                        });
                    }
                };
                Box::pin(async move {
                    AssertUnwindSafe(future)
                        .catch_unwind()
                        .await
                        .map_err(|panic| anyhow::anyhow!(panic_message(&panic)))??;
                    Ok(EventReply::Undefined)
                })
            },
            options,
        )
    }

    /// Effective presentation for one scope, nearest declaration winning.
    #[must_use]
    pub fn mode_for(&self, scope: Option<ScopeKey>) -> ToolPresentationMode {
        self.layers
            .chain_layers(scope)
            .into_iter()
            .rev()
            .find_map(|layer| *layer.mode.lock())
            .unwrap_or(self.default_mode)
    }

    /// Declares a scoped presentation override.
    ///
    /// # Errors
    ///
    /// Returns for an unscoped context or conflicting declaration.
    pub fn present_as(
        &self,
        context: &Context,
        mode: ToolPresentationMode,
    ) -> anyhow::Result<EffectHandle> {
        anyhow::ensure!(
            scope_of(context).is_some(),
            "tools.presentAs() requires a scoped context (agent.ctx): a context-global presentation is the `mode` config field on the tools row"
        );
        self.layers.effect(
            context,
            move |layer| {
                let mode_cell = layer.mode.clone();
                {
                    let mut selected = mode_cell.lock();
                    if let Some(existing) = *selected {
                        anyhow::bail!(
                            "tools.presentAs({mode:?}) conflicts with {existing:?} already declared for this scope; one composition selects one presentation"
                        );
                    }
                    *selected = Some(mode);
                }
                Ok(EntryUndo::new(move || *mode_cell.lock() = None))
            },
            LayerEffectOptions::new("tools.presentAs()"),
        )
    }

    /// Registers a global or exact-scope definition.
    ///
    /// # Errors
    ///
    /// Returns for invalid timeout metadata, the reserved transport name, or
    /// a duplicate within the selected layer.
    pub fn register(
        &self,
        context: &Context,
        definition: ToolDefinition,
    ) -> anyhow::Result<EffectHandle> {
        if let Some(timeout) = definition.timeout_ms {
            anyhow::ensure!(
                timeout.is_finite() && timeout > 0.0,
                "tool {:?} timeoutMs must be a positive finite number",
                definition.name
            );
        }
        anyhow::ensure!(
            definition.name != RUN_CODE_NAME,
            "tool name {RUN_CODE_NAME:?} is reserved for the Code Mode presentation transport and cannot be registered or shadowed"
        );
        let definition = Arc::new(definition);
        let name = definition.name.clone();
        self.layers.effect(
            context,
            move |layer| layer.tools.insert(name, definition),
            LayerEffectOptions::new("tools.register()"),
        )
    }

    /// Restricts inherited tools for an exact scope.
    ///
    /// # Errors
    ///
    /// Returns for an unscoped call, empty filter, reserved name, or a name
    /// outside the current inherited surface.
    pub fn restrict(
        &self,
        context: &Context,
        filter: ToolRestriction,
    ) -> anyhow::Result<EffectHandle> {
        let scope = scope_of(context).ok_or_else(|| {
            anyhow::anyhow!(
                "tools.restrict() requires a scoped context (agent.ctx): a context-global restriction would mask every agent"
            )
        })?;
        anyhow::ensure!(
            filter.allow.is_some() || filter.deny.is_some(),
            "tools.restrict({{}}) is a no-op: pass `allow` and/or `deny`"
        );
        let named = filter
            .allow
            .iter()
            .flatten()
            .chain(filter.deny.iter().flatten())
            .cloned()
            .collect::<Vec<_>>();
        anyhow::ensure!(
            !named.iter().any(|name| name == RUN_CODE_NAME),
            "tools.restrict() cannot name reserved Code Mode presentation transport {RUN_CODE_NAME:?}; restrict end-capability tools instead"
        );
        let known = self.view(Some(scope)).restrictable_names;
        let unknown = named
            .iter()
            .filter(|name| !known.contains(*name))
            .cloned()
            .collect::<Vec<_>>();
        if !unknown.is_empty() {
            let mut known = known.into_iter().collect::<Vec<_>>();
            known.sort();
            anyhow::bail!(
                "tools.restrict() names unknown global tool{} {}; known global tools: {}",
                if unknown.len() == 1 { "" } else { "s" },
                unknown
                    .iter()
                    .map(|name| format!("{name:?}"))
                    .collect::<Vec<_>>()
                    .join(", "),
                if known.is_empty() {
                    "(none)".to_owned()
                } else {
                    known.join(", ")
                }
            );
        }
        let compiled = CompiledToolRestriction {
            allow: filter.allow.map(|values| values.into_iter().collect()),
            deny: filter.deny.map(|values| values.into_iter().collect()),
        };
        self.layers.effect(
            context,
            move |layer| Ok(layer.restrictions.append(compiled)),
            LayerEffectOptions::new("tools.restrict()"),
        )
    }

    /// Registers a global or scoped monotonic guard without change notification.
    ///
    /// # Errors
    ///
    /// Returns when the owning context is inactive.
    pub fn guard(&self, context: &Context, guard: ToolGuard) -> anyhow::Result<EffectHandle> {
        self.layers.effect(
            context,
            move |layer| Ok(layer.guards.append(guard)),
            LayerEffectOptions::new("tools.guard()").notify(false),
        )
    }

    /// Resolves the definition visible to one scope.
    #[must_use]
    pub fn get(&self, name: &str, scope: Option<ScopeKey>) -> Option<Arc<ToolDefinition>> {
        self.view(scope).visible.get(name).cloned()
    }

    /// Projects the allowlisted model-facing fields for every visible tool.
    #[must_use]
    pub fn schemas(&self, scope: Option<ScopeKey>) -> Vec<ToolSchema> {
        self.view(scope)
            .visible
            .values()
            .map(|definition| ToolSchema {
                name: definition.name.clone(),
                description: definition.description.clone(),
                parameters: definition.parameters.clone(),
            })
            .collect()
    }

    /// Projects visible native definitions onto the Code Mode SDK contract.
    #[must_use]
    pub fn sdk_schemas(&self, scope: Option<ScopeKey>) -> Vec<ToolSdkSchema> {
        self.view(scope)
            .visible
            .values()
            .filter(|definition| definition.name != RUN_CODE_NAME)
            .map(|definition| ToolSdkSchema {
                name: definition.name.clone(),
                description: definition.description.clone(),
                parameters: Value::Object(definition.parameters.clone()),
                output: definition.output.schema.as_value().clone(),
            })
            .collect()
    }

    /// Returns every pre-restriction capability name known to a scope.
    #[must_use]
    pub fn known_names(&self, scope: Option<ScopeKey>) -> HashSet<String> {
        self.view(scope).known_names
    }

    /// Fail-closed scheduling classification.
    #[must_use]
    pub fn execution_mode(&self, input: &ToolExecutionInput) -> ToolExecutionMode {
        let Some(tool) = self.resolve_execution(&input.name, input.agent, input.parent.is_some())
        else {
            return ToolExecutionMode::Exclusive;
        };
        let Some(classifier) = &tool.is_concurrency_safe else {
            return ToolExecutionMode::Exclusive;
        };
        catch_unwind(AssertUnwindSafe(|| classifier(&input.arguments)))
            .ok()
            .filter(|safe| *safe)
            .map_or(ToolExecutionMode::Exclusive, |_| {
                ToolExecutionMode::Parallel
            })
    }

    /// Executes through pre-policy, guards, around-dispatch, post-policy,
    /// final content transformation, materialization, and result notification.
    pub async fn execute(self: &Arc<Self>, input: ToolExecutionInput) -> ToolExecutionResult {
        let execution = Self::create_execution(input);
        let mut captured_finalizer = self
            .get(&execution.name, execution.agent)
            .and_then(|tool| tool.finalize_content.clone());
        let collapsed = self.get(&execution.name, execution.agent).is_some_and(|_| {
            self.collapses(&execution.name, execution.agent, execution.parent.is_some())
        });

        let candidate = if collapsed {
            if Self::caller_cancelled(&execution) {
                aborted_before_result()
            } else {
                captured_finalizer = None;
                tool_error_result(anyhow::Error::new(ToolRuntimeError::ToolNotFound {
                    message: format!(
                        "unknown tool {:?}: only `{RUN_CODE_NAME}` is callable directly — call `{}` from inside a `{RUN_CODE_NAME}` program instead",
                        execution.name, execution.name
                    ),
                }))
            }
        } else {
            self.run_pipeline(&execution).await
        };
        self.finish(&execution, candidate, captured_finalizer)
    }

    async fn run_pipeline(self: &Arc<Self>, execution: &ToolExecution) -> ToolExecutionResult {
        if Self::caller_cancelled(execution) {
            return aborted_before_result();
        }
        let pre = match AssertUnwindSafe(self.pre_execute(execution))
            .catch_unwind()
            .await
        {
            Ok(Ok(decision)) => decision,
            Ok(Err(error)) => return tool_error_result(error),
            Err(panic) => return tool_error_result(anyhow::anyhow!(panic_message(&panic))),
        };
        let denial = match pre {
            PreToolDecision::Allow => {
                match catch_unwind(AssertUnwindSafe(|| self.guard_reason(execution))) {
                    Ok(reason) => reason,
                    Err(panic) => {
                        return tool_error_result(anyhow::anyhow!(panic_message(&panic)));
                    }
                }
            }
            PreToolDecision::Deny { reason } => Some(reason),
            PreToolDecision::Ask { reason } => Some(reason.unwrap_or_else(|| {
                format!(
                    "tool {:?} requires approval (not yet supported)",
                    execution.name
                )
            })),
        };
        if let Some(reason) = denial {
            let denied = ToolExecutionResult::Failure(ToolExecutionFailure {
                content: error_content(&reason),
                error: ToolFailure {
                    message: reason,
                    info: None,
                },
                meta: None,
                additional_contexts: Vec::new(),
            });
            return self.post_or_error(execution, denied).await;
        }
        if Self::caller_cancelled(execution) {
            return self.post_or_error(execution, aborted_before_result()).await;
        }
        let dispatched = match AssertUnwindSafe(self.around_execute(execution))
            .catch_unwind()
            .await
        {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => return tool_error_result(error),
            Err(panic) => return tool_error_result(anyhow::anyhow!(panic_message(&panic))),
        };
        let normalized = match self.normalize_dispatch_result(execution, dispatched) {
            Ok(result) => result,
            Err(error) => return tool_error_result(error),
        };
        let normalized = Self::attach_deferred(execution, normalized);
        let normalized = if Self::caller_cancelled(execution) && !normalized.is_error() {
            Self::cancellation_result(execution, Some(&normalized))
        } else {
            normalized
        };
        self.post_or_error(execution, normalized).await
    }

    async fn post_or_error(
        &self,
        execution: &ToolExecution,
        result: ToolExecutionResult,
    ) -> ToolExecutionResult {
        match AssertUnwindSafe(self.post_execute(execution, &result))
            .catch_unwind()
            .await
        {
            Ok(Ok(post)) if Self::caller_cancelled(execution) && !post.is_error() => {
                Self::cancellation_result(execution, Some(&post))
            }
            Ok(Ok(post)) => post,
            Ok(Err(error)) => tool_error_result(error),
            Err(panic) => tool_error_result(anyhow::anyhow!(panic_message(&panic))),
        }
    }

    async fn pre_execute(&self, execution: &ToolExecution) -> anyhow::Result<PreToolDecision> {
        let args = EventArgs::one(execution.clone());
        let reply = self
            .context
            .events()
            .waterfall(
                &scope_target(&self.context, execution.agent),
                "tools/pre-execute",
                &args,
                || Box::pin(async { Ok(EventReply::Value(Arc::new(PreToolDecision::Allow))) }),
            )
            .await?;
        reply
            .downcast::<PreToolDecision>()
            .map(|decision| (*decision).clone())
            .ok_or_else(|| anyhow::anyhow!("tools/pre-execute returned an invalid decision"))
    }

    async fn around_execute(
        self: &Arc<Self>,
        execution: &ToolExecution,
    ) -> anyhow::Result<ToolExecutionResult> {
        let args = EventArgs::one(execution.clone());
        let runtime = self.clone();
        let inner_execution = execution.clone();
        let reply = self
            .context
            .events()
            .waterfall(
                &scope_target(&self.context, execution.agent),
                "tools/execute",
                &args,
                move || {
                    Box::pin(async move {
                        Ok(EventReply::Value(Arc::new(
                            runtime.dispatch_tool_body(&inner_execution).await,
                        )))
                    })
                },
            )
            .await?;
        reply
            .downcast::<ToolExecutionResult>()
            .map(|result| (*result).clone())
            .ok_or_else(|| anyhow::anyhow!("tools/execute returned an invalid result"))
    }

    async fn post_execute(
        &self,
        execution: &ToolExecution,
        result: &ToolExecutionResult,
    ) -> anyhow::Result<ToolExecutionResult> {
        let args =
            EventArgs::from_values(vec![Arc::new(execution.clone()), Arc::new(result.clone())]);
        let reply = self
            .context
            .events()
            .waterfall(
                &scope_target(&self.context, execution.agent),
                "tools/post-execute",
                &args,
                || Box::pin(async { Ok(EventReply::Value(Arc::new(PostToolDecision::default()))) }),
            )
            .await?;
        let decision = reply
            .downcast::<PostToolDecision>()
            .map(|decision| (*decision).clone())
            .ok_or_else(|| anyhow::anyhow!("tools/post-execute returned an invalid decision"))?;
        self.apply_post_decision(execution, result, decision)
    }

    async fn dispatch_tool_body(&self, execution: &ToolExecution) -> ToolExecutionResult {
        let wrapper_signal = execution.signal();
        let fused = AbortSignal::fuse(&execution.state.caller_signal, &wrapper_signal);
        if fused.is_aborted() {
            return aborted_before_result();
        }
        let _prior_signal = execution.replace_dispatch_signal(fused.clone());
        let outcome = self.invoke_tool_body(execution).await;
        let _fused_signal = execution.replace_dispatch_signal(wrapper_signal);
        match outcome {
            Ok(result) if fused.is_aborted() => aborted_result(Some(&result)),
            Ok(result) => result,
            Err(error) => tool_error_result(error),
        }
    }

    async fn invoke_tool_body(
        &self,
        execution: &ToolExecution,
    ) -> anyhow::Result<ToolExecutionResult> {
        let tool = self
            .resolve_execution(&execution.name, execution.agent, execution.parent.is_some())
            .ok_or_else(|| tool_not_found(&execution.name, None))?;
        execution.state.body_invoked.store(true, Ordering::Release);
        let arguments = execution.arguments.clone();
        let body = tool.execute.clone();
        let body_execution = execution.clone();
        let future = catch_unwind(AssertUnwindSafe(|| body(arguments, body_execution)))
            .map_err(|panic| anyhow::anyhow!(panic_message(&panic)))?;
        let candidate = AssertUnwindSafe(future)
            .catch_unwind()
            .await
            .map_err(|panic| anyhow::anyhow!(panic_message(&panic)))??;
        Self::create_success_result(execution, &tool, candidate)
    }

    fn create_success_result(
        execution: &ToolExecution,
        tool: &ToolDefinition,
        value: Value,
    ) -> anyhow::Result<ToolExecutionResult> {
        let violations =
            validate_json_schema_value_at(tool.output.schema.as_ref(), &value, "value");
        if !violations.is_empty() {
            return Err(anyhow::Error::new(ToolRuntimeError::InvalidToolOutput {
                tool_name: tool.name.clone(),
                violations: violations.join("; "),
            }));
        }
        let rendered = catch_unwind(AssertUnwindSafe(|| {
            (tool.output.render)(&execution.arguments, &value)
        }))
        .map_err(|panic| projection_error(&tool.name, "render", panic_message(&panic)))?
        .map_err(|error| projection_error(&tool.name, "render", format!("{error:#}")))?;
        let content = snapshot_content(&rendered)
            .map_err(|error| projection_error(&tool.name, "render", format!("{error:#}")))?;
        let meta = if execution.parent.is_none() {
            tool.output
                .presentation_meta
                .as_ref()
                .map(|projector| {
                    catch_unwind(AssertUnwindSafe(|| projector(&execution.arguments, &value)))
                        .map_err(|panic| {
                            projection_error(&tool.name, "presentationMeta", panic_message(&panic))
                        })?
                        .map_err(|error| {
                            projection_error(&tool.name, "presentationMeta", format!("{error:#}"))
                        })
                })
                .transpose()?
        } else {
            None
        };
        Ok(ToolExecutionResult::Success(ToolExecutionSuccess {
            value,
            content,
            meta,
            additional_contexts: Vec::new(),
            concludes_turn: execution.state.concludes_turn.load(Ordering::Acquire),
            canonical_for: Some(execution.token),
        }))
    }

    fn normalize_dispatch_result(
        &self,
        execution: &ToolExecution,
        result: ToolExecutionResult,
    ) -> anyhow::Result<ToolExecutionResult> {
        match result {
            ToolExecutionResult::Success(result)
                if result.canonical_for == Some(execution.token) =>
            {
                Ok(ToolExecutionResult::Success(result))
            }
            ToolExecutionResult::Success(result) => {
                let tool = self
                    .resolve_execution(&execution.name, execution.agent, execution.parent.is_some())
                    .ok_or_else(|| tool_not_found(&execution.name, None))?;
                let mut normalized = Self::create_success_result(execution, &tool, result.value)?;
                if let ToolExecutionResult::Success(normalized) = &mut normalized {
                    normalized.additional_contexts = result.additional_contexts;
                }
                Ok(normalized)
            }
            ToolExecutionResult::Failure(result) => Ok(ToolExecutionResult::Failure(result)),
        }
    }

    fn apply_post_decision(
        &self,
        execution: &ToolExecution,
        result: &ToolExecutionResult,
        decision: PostToolDecision,
    ) -> anyhow::Result<ToolExecutionResult> {
        match decision {
            PostToolDecision::Block {
                feedback,
                additional_contexts,
            } => {
                let message = failure_message_from_content(&feedback);
                Ok(ToolExecutionResult::Failure(ToolExecutionFailure {
                    error: ToolFailure {
                        message,
                        info: None,
                    },
                    content: feedback,
                    meta: None,
                    additional_contexts,
                }))
            }
            PostToolDecision::ReplaceValue {
                value,
                additional_contexts,
            } => {
                anyhow::ensure!(
                    !result.is_error(),
                    "tools/post-execute cannot replace the value of a failed result"
                );
                let tool = self
                    .resolve_execution(&execution.name, execution.agent, execution.parent.is_some())
                    .ok_or_else(|| tool_not_found(&execution.name, None))?;
                let mut replaced = Self::create_success_result(execution, &tool, value)?;
                if let ToolExecutionResult::Success(success) = &mut replaced {
                    let mut contexts = result.additional_contexts().to_vec();
                    contexts.extend(additional_contexts);
                    success.additional_contexts = contexts;
                }
                Ok(replaced)
            }
            PostToolDecision::Accept {
                content,
                additional_contexts,
            } => {
                let mut accepted = result.clone();
                match &mut accepted {
                    ToolExecutionResult::Success(success) => {
                        if let Some(content) = content {
                            success.content = content;
                        }
                        success.additional_contexts.extend(additional_contexts);
                        success.canonical_for = Some(execution.token);
                    }
                    ToolExecutionResult::Failure(failure) => {
                        if let Some(content) = content {
                            failure.content = content;
                        }
                        failure.additional_contexts.extend(additional_contexts);
                    }
                }
                Ok(accepted)
            }
        }
    }

    fn finish(
        &self,
        execution: &ToolExecution,
        result: ToolExecutionResult,
        finalizer: Option<ToolContentFinalizer>,
    ) -> ToolExecutionResult {
        let materialized = materialize_result(result).unwrap_or_else(|error| {
            materialize_result(tool_error_result(error)).expect("safe error")
        });
        let transformed_result = match finalizer {
            Some(finalizer) => {
                let transformed =
                    catch_unwind(AssertUnwindSafe(|| finalizer(execution, &materialized)));
                match transformed {
                    Ok(Ok(Some(content))) => replace_result_content(materialized, content),
                    Ok(Ok(None)) => materialized,
                    Ok(Err(error)) => tool_error_result(error),
                    Err(panic) => tool_error_result(anyhow::anyhow!(panic_message(&panic))),
                }
            }
            None => materialized,
        };
        let final_result = materialize_result(transformed_result).unwrap_or_else(|error| {
            materialize_result(tool_error_result(error)).expect("safe error")
        });
        self.notify_result(execution, &final_result);
        final_result
    }

    fn notify_result(&self, execution: &ToolExecution, result: &ToolExecutionResult) {
        let arguments =
            EventArgs::from_values(vec![Arc::new(execution.clone()), Arc::new(result.clone())]);
        match self.context.events().prepare_emit(
            &scope_target(&self.context, execution.agent),
            "tools/result",
            &arguments,
        ) {
            Ok(emission) => emission.emit_contained(|error| {
                tracing::warn!(
                    tool = %execution.name,
                    call_id = %execution.call_id,
                    %error,
                    "tools/result observer failed"
                );
            }),
            Err(error) => tracing::warn!(
                tool = %execution.name,
                call_id = %execution.call_id,
                %error,
                "tools/result dispatch preparation failed"
            ),
        }
    }

    fn create_execution(input: ToolExecutionInput) -> ToolExecution {
        let root_call_id = input.root_call_id.unwrap_or_else(|| input.call_id.clone());
        let signal = input.signal;
        ToolExecution {
            call_id: input.call_id,
            root_call_id,
            name: input.name,
            arguments: input.arguments,
            agent: input.agent,
            parent: input.parent,
            token: ToolExecutionToken::new(),
            state: Arc::new(ExecutionState {
                caller_signal: signal.clone(),
                signal: Mutex::new(signal),
                body_invoked: AtomicBool::new(false),
                deferred_contexts: Mutex::new(Vec::new()),
                concludes_turn: AtomicBool::new(false),
            }),
        }
    }

    fn attach_deferred(
        execution: &ToolExecution,
        mut result: ToolExecutionResult,
    ) -> ToolExecutionResult {
        let deferred = execution.state.deferred_contexts.lock().clone();
        if deferred.is_empty() {
            return result;
        }
        match &mut result {
            ToolExecutionResult::Success(success) => {
                let existing = std::mem::take(&mut success.additional_contexts);
                success.additional_contexts = deferred.into_iter().chain(existing).collect();
            }
            ToolExecutionResult::Failure(failure) => {
                let existing = std::mem::take(&mut failure.additional_contexts);
                failure.additional_contexts = deferred.into_iter().chain(existing).collect();
            }
        }
        result
    }

    fn caller_cancelled(execution: &ToolExecution) -> bool {
        execution.state.caller_signal.is_aborted()
    }

    fn cancellation_result(
        execution: &ToolExecution,
        prior: Option<&ToolExecutionResult>,
    ) -> ToolExecutionResult {
        if execution.state.body_invoked.load(Ordering::Acquire) {
            aborted_result(prior)
        } else {
            aborted_before_result_with_prior(prior)
        }
    }

    fn guard_reason(&self, execution: &ToolExecution) -> Option<String> {
        self.layers.global.guard_reason(execution).or_else(|| {
            execution.agent.and_then(|agent| {
                self.layers
                    .chain_layers(Some(agent))
                    .into_iter()
                    .find_map(|layer| layer.guard_reason(execution))
            })
        })
    }

    fn view(&self, scope: Option<ScopeKey>) -> ToolView {
        let layers = self.layers.chain_layers(scope);
        let own = self.layers.peek(scope);
        let mut inherited = self
            .layers
            .global
            .tools
            .entries()
            .collect::<IndexMap<_, _>>();
        for layer in &layers {
            if own.as_ref().is_some_and(|own| Arc::ptr_eq(own, layer)) {
                continue;
            }
            inherited.extend(layer.tools.entries());
        }
        let mut visible = IndexMap::new();
        let mut known_names = HashSet::new();
        let mut restrictable_names = HashSet::new();
        for (name, definition) in inherited {
            known_names.insert(name.clone());
            restrictable_names.insert(name.clone());
            if layers.iter().all(|layer| layer.admits(&name)) {
                visible.insert(name, definition);
            }
        }
        if let Some(own) = own {
            for (name, definition) in own.tools.entries() {
                known_names.insert(name.clone());
                visible.insert(name, definition);
            }
        }
        ToolView {
            visible,
            known_names,
            restrictable_names,
        }
    }

    fn resolve_execution(
        &self,
        name: &str,
        scope: Option<ScopeKey>,
        nested: bool,
    ) -> Option<Arc<ToolDefinition>> {
        let tool = self.get(name, scope)?;
        (!self.collapses(name, scope, nested)).then_some(tool)
    }

    fn collapses(&self, name: &str, scope: Option<ScopeKey>, nested: bool) -> bool {
        !nested && self.mode_for(scope) == ToolPresentationMode::Code && name != RUN_CODE_NAME
    }
}

fn tool_not_found(name: &str, reachable_from: Option<&str>) -> anyhow::Error {
    let message = reachable_from.map_or_else(
        || format!("unknown tool {name:?}"),
        |route| format!("unknown tool {name:?}: {route}"),
    );
    anyhow::Error::new(ToolRuntimeError::ToolNotFound { message })
}

fn projection_error(tool_name: &str, projector: &'static str, message: String) -> anyhow::Error {
    anyhow::Error::new(ToolRuntimeError::Projection {
        tool_name: tool_name.to_owned(),
        projector,
        message,
    })
}

fn tool_error_result(error: anyhow::Error) -> ToolExecutionResult {
    let info = error
        .downcast_ref::<ToolRuntimeError>()
        .map(ToolRuntimeError::info);
    let message = format!("{error:#}");
    drop(error);
    ToolExecutionResult::Failure(ToolExecutionFailure {
        content: error_content(&message),
        error: ToolFailure { message, info },
        meta: None,
        additional_contexts: Vec::new(),
    })
}

fn error_content(message: &str) -> Vec<ContentBlock> {
    vec![ContentBlock::Text {
        text: format!("Error: {message}"),
    }]
}

fn aborted_result(prior: Option<&ToolExecutionResult>) -> ToolExecutionResult {
    cancellation_failure(
        "tool call aborted",
        TOOL_ABORTED,
        prior.map_or_else(Vec::new, |result| result.additional_contexts().to_vec()),
    )
}

fn aborted_before_result() -> ToolExecutionResult {
    aborted_before_result_with_prior(None)
}

fn aborted_before_result_with_prior(prior: Option<&ToolExecutionResult>) -> ToolExecutionResult {
    cancellation_failure(
        "tool call aborted before dispatch",
        TOOL_ABORTED_BEFORE_DISPATCH,
        prior.map_or_else(Vec::new, |result| result.additional_contexts().to_vec()),
    )
}

fn cancellation_failure(
    message: &str,
    code: &str,
    additional_contexts: Vec<Message>,
) -> ToolExecutionResult {
    ToolExecutionResult::Failure(ToolExecutionFailure {
        content: error_content(message),
        error: ToolFailure {
            message: message.to_owned(),
            info: Some(ToolErrorInfo {
                name: "AbortError".to_owned(),
                code: code.to_owned(),
            }),
        },
        meta: None,
        additional_contexts,
    })
}

fn failure_message_from_content(content: &[ContentBlock]) -> String {
    let text = content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text } => text.clone(),
            other => format!("[{} content]", other.block_type()),
        })
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty() {
        "tool result blocked by post-execute policy".to_owned()
    } else {
        text
    }
}

fn snapshot_content(content: &[ContentBlock]) -> anyhow::Result<Vec<ContentBlock>> {
    Ok(serde_json::from_value(serde_json::to_value(content)?)?)
}

fn materialize_result(result: ToolExecutionResult) -> anyhow::Result<ToolExecutionResult> {
    match result {
        ToolExecutionResult::Success(mut success) => {
            success.content = snapshot_content(&success.content)?;
            success.additional_contexts =
                serde_json::from_value(serde_json::to_value(&success.additional_contexts)?)?;
            success.meta = success
                .meta
                .map(|meta| serde_json::from_value(serde_json::to_value(meta)?))
                .transpose()?;
            Ok(ToolExecutionResult::Success(success))
        }
        ToolExecutionResult::Failure(mut failure) => {
            failure.content = snapshot_content(&failure.content)?;
            failure.additional_contexts =
                serde_json::from_value(serde_json::to_value(&failure.additional_contexts)?)?;
            failure.meta = failure
                .meta
                .map(|meta| serde_json::from_value(serde_json::to_value(meta)?))
                .transpose()?;
            Ok(ToolExecutionResult::Failure(failure))
        }
    }
}

fn replace_result_content(
    mut result: ToolExecutionResult,
    content: Vec<ContentBlock>,
) -> ToolExecutionResult {
    match &mut result {
        ToolExecutionResult::Success(success) => success.content = content,
        ToolExecutionResult::Failure(failure) => failure.content = content,
    }
    result
}

fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    payload.downcast_ref::<String>().map_or_else(
        || {
            payload
                .downcast_ref::<&'static str>()
                .map_or_else(|| "panic".to_owned(), |message| (*message).to_owned())
        },
        Clone::clone,
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::assert_supported_json_schema;
    use seekdeep_scope::create_scope;

    fn definition(name: &str, value: Value) -> ToolDefinition {
        ToolDefinition::new(
            name,
            format!("{name} description"),
            Map::from_iter([("type".to_owned(), Value::String("object".to_owned()))]),
            ToolOutputDefinition::new(
                Arc::new(
                    assert_supported_json_schema(json!({"type": "string"})).expect("output schema"),
                ),
                Arc::new(|_, value| {
                    Ok(vec![ContentBlock::Text {
                        text: value.as_str().unwrap_or_default().to_owned(),
                    }])
                }),
            ),
            Arc::new(move |_, _| {
                let value = value.clone();
                Box::pin(async move { Ok(value) })
            }),
        )
    }

    fn input(name: &str, agent: Option<ScopeKey>) -> ToolExecutionInput {
        ToolExecutionInput {
            call_id: CallId::new("call-1"),
            root_call_id: None,
            name: name.to_owned(),
            arguments: json!({}),
            agent,
            parent: None,
            signal: AbortSignal::default(),
        }
    }

    #[tokio::test]
    async fn scoped_registration_shadowing_restriction_and_disposal() {
        let root = Context::new();
        let runtime =
            ToolRuntime::new(root.clone(), ToolRuntimeConfig::default()).expect("runtime");
        let key = ScopeKey::new();
        let scope = create_scope(&root, key, None).expect("scope");
        runtime
            .register(&root, definition("shared", json!("global")))
            .expect("global");
        runtime
            .register(&root, definition("hidden", json!("hidden")))
            .expect("hidden");
        runtime
            .register(&scope.context, definition("shared", json!("scoped")))
            .expect("scoped");
        runtime
            .restrict(
                &scope.context,
                ToolRestriction {
                    allow: Some(vec!["shared".to_owned()]),
                    deny: None,
                },
            )
            .expect("restrict");

        assert_eq!(
            runtime
                .schemas(Some(key))
                .into_iter()
                .map(|schema| schema.name)
                .collect::<Vec<_>>(),
            ["shared"]
        );
        let ToolExecutionResult::Success(success) =
            runtime.execute(input("shared", Some(key))).await
        else {
            panic!("success")
        };
        assert_eq!(success.value, "scoped");
        scope.dispose().await.expect("dispose");
        assert!(runtime.get("hidden", Some(key)).is_some());
    }

    #[tokio::test]
    async fn executes_pipeline_output_validation_and_cancellation() {
        let root = Context::new();
        let runtime =
            ToolRuntime::new(root.clone(), ToolRuntimeConfig::default()).expect("runtime");
        runtime
            .register(&root, definition("echo", json!("ok")))
            .expect("register");
        let ToolExecutionResult::Success(success) = runtime.execute(input("echo", None)).await
        else {
            panic!("success")
        };
        assert_eq!(
            success.content,
            [ContentBlock::Text {
                text: "ok".to_owned()
            }]
        );

        let signal = AbortSignal::default();
        signal.abort();
        let mut aborted = input("echo", None);
        aborted.signal = signal;
        let ToolExecutionResult::Failure(failure) = runtime.execute(aborted).await else {
            panic!("failure")
        };
        assert_eq!(
            failure.error.info.expect("info").code,
            TOOL_ABORTED_BEFORE_DISPATCH
        );

        runtime
            .register(&root, definition("bad", json!(3)))
            .expect("bad register");
        let ToolExecutionResult::Failure(failure) = runtime.execute(input("bad", None)).await
        else {
            panic!("failure")
        };
        assert_eq!(
            failure.error.info.expect("info").code,
            "INVALID_TOOL_OUTPUT"
        );
    }

    #[test]
    fn execution_mode_is_fail_closed() {
        let root = Context::new();
        let runtime =
            ToolRuntime::new(root.clone(), ToolRuntimeConfig::default()).expect("runtime");
        let mut parallel = definition("parallel", json!("ok"));
        parallel.is_concurrency_safe = Some(Arc::new(|_| true));
        runtime.register(&root, parallel).expect("parallel");
        assert_eq!(
            runtime.execution_mode(&input("parallel", None)),
            ToolExecutionMode::Parallel
        );
        assert_eq!(
            runtime.execution_mode(&input("missing", None)),
            ToolExecutionMode::Exclusive
        );
    }
}
