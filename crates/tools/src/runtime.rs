//! Scope-aware tool registry and staged execution pipeline.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    future::Future,
    panic::{AssertUnwindSafe, catch_unwind},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use futures::FutureExt;
use indexmap::IndexMap;
use parking_lot::Mutex;
use seekdeep_agent::Agent;
use seekdeep_code_runtime::{
    CODE_RUNTIME, CodeBindingErrorClass, CodeBindingFunction, CodeBindingNamespace,
    CodeRunFailureKind, CodeRunRequest, CodeRuntime,
};
use seekdeep_cordis::{
    Context, CordisError, EventArgs, EventOptions, EventReply, Fiber, ServiceKey, events::Next,
    fiber::EffectHandle,
};
use seekdeep_core::session::{AppendOptions, Session};
use seekdeep_llm::{AbortSignal, CallId, ContentBlock, HarnessError, ToolSchema, UserMessage};
use seekdeep_scope::{
    ScopeKey, scope_of, scope_target, scoped_event_args,
    store::{
        AnonymousEntries, EntryUndo, LayerEffectOptions, NamedEntries, ScopeLayer, ScopedLayers,
    },
};
use seekdeep_system_prompt::{PromptSection, PromptText, SystemPrompt, ToolProviderResult};
use seekdeep_user_approval::{APPROVAL, ApprovalOutcome, ApprovalRequest};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use thiserror::Error;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use uuid::Uuid;

use crate::{
    JsonSchemaNode, ToolArgsError, ToolCallView, ToolResult, ToolResultView, ToolSdkSchema,
    assert_supported_json_schema, render_tools_sdk, render_tools_sdk_py,
    validate_json_schema_value_at,
};

/// Canonical error code for cancellation after a body was invoked.
pub const TOOL_ABORTED: &str = "ABORTED";
/// Canonical error code for cancellation before a body was invoked.
pub const TOOL_ABORTED_BEFORE_DISPATCH: &str = "ABORTED_BEFORE_DISPATCH";

fn scoped_args(scope: Option<ScopeKey>, args: EventArgs) -> EventArgs {
    match scope {
        Some(scope) => scoped_event_args(scope, args),
        None => args,
    }
}
/// Reserved Code Mode transport name.
pub const RUN_CODE_NAME: &str = "run_code";
/// Prompt order of the direct-call collapse statement.
pub const CODE_ONLY_SECTION_ORDER: f64 = 99.0;
/// Prompt order of the generated Code Mode SDK.
pub const SDK_SECTION_ORDER: f64 = 150.0;
/// Typed Cordis slot corresponding to `ctx.tools`.
pub const TOOLS: ServiceKey<ToolRuntime> = ServiceKey::new("tools");

const CODE_ONLY_INSTRUCTION: &str = "`run_code` is the only tool you can call directly — a tool call naming any other tool fails. Reach every tool the SDK declares below from inside the program.";

/// Boxed asynchronous tool body.
pub type ToolExecuteFuture = Pin<Box<dyn Future<Output = anyhow::Result<Value>> + Send + 'static>>;
/// Tool body callback.
pub type ToolExecute =
    Arc<dyn Fn(Value, ToolRunContext) -> ToolExecuteFuture + Send + Sync + 'static>;
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
/// Pure replay-safe pending-call presenter.
pub type ToolCallPresenter = Arc<dyn Fn(&Value) -> Option<ToolCallView> + Send + Sync + 'static>;
/// Pure replay-safe completed-call presenter.
pub type ToolResultPresenter =
    Arc<dyn Fn(&Value, &ToolResult) -> Option<ToolResultView> + Send + Sync + 'static>;
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

/// Typed continuation for `tools/code-dispatch-log` middleware.
pub struct CodeDispatchLogNext(Next);

impl CodeDispatchLogNext {
    /// Delegates to the remaining durable-log shaping chain.
    ///
    /// # Errors
    ///
    /// Returns a downstream failure or invalid reply type.
    pub async fn run(self) -> anyhow::Result<Vec<ContentBlock>> {
        self.0
            .run()
            .await?
            .downcast::<Vec<ContentBlock>>()
            .map(|content| (*content).clone())
            .ok_or_else(|| anyhow::anyhow!("tools/code-dispatch-log returned invalid content"))
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
    /// Optional replay-safe pending-call presenter.
    pub present_call: Option<ToolCallPresenter>,
    /// Optional replay-safe completed-call presenter.
    pub present_result: Option<ToolResultPresenter>,
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
            .field("present_call", &self.present_call.is_some())
            .field("present_result", &self.present_result.is_some())
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
            present_call: None,
            present_result: None,
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

    /// Adds a replay-safe pending-call presenter.
    #[must_use]
    pub fn present_call(mut self, presenter: ToolCallPresenter) -> Self {
        self.present_call = Some(presenter);
        self
    }

    /// Adds a replay-safe completed-call presenter.
    #[must_use]
    pub fn present_result(mut self, presenter: ToolResultPresenter) -> Self {
        self.present_result = Some(presenter);
        self
    }
}

/// How a scope presents registered tools to its model.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolPresentationMode {
    /// Every visible tool is a native model tool.
    #[default]
    Native,
    /// Only the reserved Code Mode transport is callable directly.
    Code,
    /// Native tools and the Code Mode transport are both visible.
    Both,
}

impl ToolPresentationMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Code => "code",
            Self::Both => "both",
        }
    }
}

/// Tool runtime configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
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
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolRestriction {
    /// Inherited names retained by this restriction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow: Option<Vec<String>>,
    /// Inherited names removed by this restriction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
///
/// The caller signal is constructor-required and cannot be replaced afterward.
///
/// ```compile_fail
/// use seekdeep_llm::{AbortSignal, CallId};
/// use seekdeep_tools::ToolExecutionInput;
/// let mut input = ToolExecutionInput::new(
///     CallId::new("c1"), "probe", serde_json::json!({}), AbortSignal::default()
/// );
/// input.signal = AbortSignal::default();
/// ```
///
/// JavaScript `Map` and class-instance argument hazards cannot cross the Rust
/// API without an explicit conversion into structural JSON:
///
/// ```compile_fail
/// use std::collections::HashMap;
/// use seekdeep_llm::{AbortSignal, CallId};
/// use seekdeep_tools::ToolExecutionInput;
/// let arguments = HashMap::from([("mutable", true)]);
/// ToolExecutionInput::new(CallId::new("c1"), "probe", arguments, AbortSignal::default());
/// ```
///
/// ```compile_fail
/// use seekdeep_llm::{AbortSignal, CallId};
/// use seekdeep_tools::ToolExecutionInput;
/// struct Arguments { value: i32 }
/// let arguments = Arguments { value: 1 };
/// ToolExecutionInput::new(CallId::new("c1"), "probe", arguments, AbortSignal::default());
/// ```
///
/// ```compile_fail
/// use seekdeep_llm::{AbortSignal, CallId};
/// use seekdeep_tools::ToolExecutionInput;
/// let arguments = || true;
/// ToolExecutionInput::new(CallId::new("c1"), "probe", arguments, AbortSignal::default());
/// ```
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
    /// Exact live calling agent.
    pub agent: Option<Arc<Agent>>,
    /// Low-level scope fallback for synthetic or replay-only dispatches.
    pub agent_scope: Option<ScopeKey>,
    /// Low-level durable-session fallback paired with `agent_scope`.
    pub agent_session: Option<Arc<Session>>,
    /// Enclosing transport execution for nested dispatch.
    pub parent: Option<ToolExecutionToken>,
    /// Required caller-owned cancellation.
    signal: AbortSignal,
}

impl ToolExecutionInput {
    /// Creates one caller-authored invocation with its mandatory signal.
    #[must_use]
    pub fn new(
        call_id: CallId,
        name: impl Into<String>,
        arguments: Value,
        signal: AbortSignal,
    ) -> Self {
        Self {
            call_id,
            root_call_id: None,
            name: name.into(),
            arguments,
            agent: None,
            agent_scope: None,
            agent_session: None,
            parent: None,
            signal,
        }
    }

    /// Required caller-owned cancellation signal.
    #[must_use]
    pub fn signal(&self) -> &AbortSignal {
        &self.signal
    }

    /// Effective routing scope from the live agent or low-level fallback.
    #[must_use]
    pub fn scope_key(&self) -> Option<ScopeKey> {
        self.agent
            .as_ref()
            .map(|agent| agent.scope_key())
            .or(self.agent_scope)
    }

    /// Effective durable session from the live agent or low-level fallback.
    #[must_use]
    pub fn session(&self) -> Option<Arc<Session>> {
        self.agent
            .as_ref()
            .map(|agent| agent.session().clone())
            .or_else(|| self.agent_session.clone())
    }

    /// Sets the enclosing root call identity for a nested invocation.
    #[must_use]
    pub fn with_root_call_id(mut self, root_call_id: CallId) -> Self {
        self.root_call_id = Some(root_call_id);
        self
    }

    /// Sets the exact live calling agent and its derived routing/audit fields.
    #[must_use]
    pub fn with_agent(mut self, agent: Arc<Agent>) -> Self {
        self.agent_scope = Some(agent.scope_key());
        self.agent_session = Some(agent.session().clone());
        self.agent = Some(agent);
        self
    }

    /// Sets only a synthetic/replay routing scope.
    #[must_use]
    pub fn with_agent_scope(mut self, agent_scope: ScopeKey) -> Self {
        self.agent_scope = Some(agent_scope);
        self
    }

    /// Sets the calling agent's durable session subject.
    #[must_use]
    pub fn with_agent_session(mut self, session: Arc<Session>) -> Self {
        self.agent_session = Some(session);
        self
    }

    /// Marks this invocation as nested under an enclosing transport.
    #[must_use]
    pub fn with_parent(mut self, parent: ToolExecutionToken) -> Self {
        self.parent = Some(parent);
        self
    }
}

struct ExecutionState {
    caller_signal: AbortSignal,
    signal: Mutex<AbortSignal>,
    body_invoked: AtomicBool,
    deferred_contexts: Mutex<Vec<UserMessage>>,
    concludes_turn: AtomicBool,
    finalizer: Option<ToolContentFinalizer>,
}

/// Registry-owned immutable call identity plus execution-local controls.
///
/// Policy/result observers cannot reach around-dispatch signal replacement.
///
/// ```compile_fail
/// use seekdeep_llm::AbortSignal;
/// use seekdeep_tools::ToolExecution;
/// fn replace(execution: &ToolExecution) {
///     execution.replace_dispatch_signal(AbortSignal::default());
/// }
/// ```
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
    /// Exact live calling agent.
    pub agent: Option<Arc<Agent>>,
    /// Low-level scope fallback for synthetic or replay-only dispatches.
    pub agent_scope: Option<ScopeKey>,
    /// Low-level durable-session fallback paired with `agent_scope`.
    pub agent_session: Option<Arc<Session>>,
    /// Enclosing transport token.
    pub parent: Option<ToolExecutionToken>,
    /// Registry-owned identity.
    pub token: ToolExecutionToken,
    state: Arc<ExecutionState>,
}

/// One settled Code Mode sub-dispatch about to be copied into the durable log.
///
/// The program has already received the canonical value (or failure message),
/// so this waterfall may reshape only the rendered log projection.
#[derive(Clone, Debug)]
pub struct CodeDispatchLog {
    /// Enclosing `run_code` execution and its durable session owner.
    pub execution: ToolExecution,
    /// Calling agent scope, when present.
    pub agent: Option<ScopeKey>,
    /// Deterministic nested call identity (`<parent>:code:<n>`).
    pub sub_call_id: CallId,
    /// Dispatched sub-tool name.
    pub name: String,
    /// Whether the settled sub-call failed.
    pub is_error: bool,
    /// Complete rendered content before durable-log shaping.
    pub content: Vec<ContentBlock>,
}

/// Durable payload recorded when a nested Code Mode dispatch starts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CodeDispatchStartEventData {
    /// Root model-requested call identity.
    pub root_call_id: CallId,
    /// Enclosing `run_code` call identity.
    pub parent_call_id: CallId,
    /// Deterministic nested call identity (`<parent>:code:<n>`).
    pub sub_call_id: CallId,
    /// Dispatched tool name.
    pub name: String,
    /// Lossless JSON argument snapshot dispatched to the nested tool.
    pub arguments: Value,
}

/// Durable payload recorded when a started Code Mode dispatch settles.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CodeDispatchEventData {
    /// Root model-requested call identity.
    pub root_call_id: CallId,
    /// Enclosing `run_code` call identity.
    pub parent_call_id: CallId,
    /// Deterministic nested call identity (`<parent>:code:<n>`).
    pub sub_call_id: CallId,
    /// Dispatched tool name.
    pub name: String,
    /// Lossless JSON argument snapshot used by the nested dispatch.
    pub arguments: Value,
    /// Whether the settled nested call failed.
    pub is_error: bool,
    /// Complete model-facing settled result content.
    pub content: Vec<ContentBlock>,
}

impl std::fmt::Debug for ToolExecution {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolExecution")
            .field("call_id", &self.call_id)
            .field("root_call_id", &self.root_call_id)
            .field("name", &self.name)
            .field("arguments", &self.arguments)
            .field("agent", &self.agent.as_ref().map(|agent| agent.id()))
            .field("agent_scope", &self.agent_scope)
            .field(
                "agent_session",
                &self.agent_session.as_ref().map(|session| session.id()),
            )
            .field("parent", &self.parent)
            .field("token", &self.token)
            .finish_non_exhaustive()
    }
}

impl ToolExecution {
    /// Effective routing scope from the live agent or low-level fallback.
    #[must_use]
    pub fn scope_key(&self) -> Option<ScopeKey> {
        self.agent
            .as_ref()
            .map(|agent| agent.scope_key())
            .or(self.agent_scope)
    }

    /// Effective durable session from the live agent or low-level fallback.
    #[must_use]
    pub fn session(&self) -> Option<Arc<Session>> {
        self.agent
            .as_ref()
            .map(|agent| agent.session().clone())
            .or_else(|| self.agent_session.clone())
    }

    /// Calling Agent's immutable session workspace, without synthetic fallback.
    #[must_use]
    pub fn session_cwd(&self) -> Option<&str> {
        self.agent
            .as_ref()
            .and_then(|agent| agent.session().header().cwd.as_deref())
    }

    /// Current dispatch signal. Around middleware may replace it temporarily.
    #[must_use]
    pub fn signal(&self) -> AbortSignal {
        self.state.signal.lock().clone()
    }

    /// Replaces the around-dispatch signal and returns the prior signal.
    ///
    /// The body always fuses the replacement with the captured caller signal.
    #[must_use]
    fn replace_dispatch_signal(&self, signal: AbortSignal) -> AbortSignal {
        std::mem::replace(&mut *self.state.signal.lock(), signal)
    }
}

/// Around-dispatch execution view: identity is readonly, while the required
/// signal may be replaced only for the delegated middleware lifetime.
///
/// ```compile_fail
/// use seekdeep_tools::ToolDispatchExecution;
/// fn misuse(execution: &ToolDispatchExecution) {
///     execution.conclude_turn();
/// }
/// ```
#[derive(Clone)]
pub struct ToolDispatchExecution(ToolExecution);

impl std::fmt::Debug for ToolDispatchExecution {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("ToolDispatchExecution")
            .field(&self.0)
            .finish()
    }
}

impl std::ops::Deref for ToolDispatchExecution {
    type Target = ToolExecution;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl ToolDispatchExecution {
    fn new(execution: ToolExecution) -> Self {
        Self(execution)
    }

    /// Replaces the required dispatch signal and returns the prior signal.
    #[must_use]
    pub fn replace_dispatch_signal(&self, signal: AbortSignal) -> AbortSignal {
        self.0.replace_dispatch_signal(signal)
    }
}

/// Runtime context handed only to an accepted tool body.
///
/// Tool bodies can defer context and conclude a turn, but cannot replace the
/// required signal.
///
/// ```compile_fail
/// use seekdeep_llm::AbortSignal;
/// use seekdeep_tools::ToolRunContext;
/// fn replace(run: &ToolRunContext) {
///     run.replace_dispatch_signal(AbortSignal::default());
/// }
/// ```
#[derive(Clone)]
pub struct ToolRunContext(ToolExecution);

impl std::fmt::Debug for ToolRunContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("ToolRunContext")
            .field(&self.0)
            .finish()
    }
}

impl std::ops::Deref for ToolRunContext {
    type Target = ToolExecution;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl ToolRunContext {
    fn new(execution: ToolExecution) -> Self {
        Self(execution)
    }

    /// Returns the immutable pipeline execution view.
    #[must_use]
    pub fn execution(&self) -> &ToolExecution {
        &self.0
    }

    /// Defers one plugin/user message until this result reaches the loop.
    pub fn defer_context(&self, context: UserMessage) {
        self.0.state.deferred_contexts.lock().push(context);
    }

    /// Marks a successful result as terminal for the current agent turn.
    pub fn conclude_turn(&self) {
        self.0.state.concludes_turn.store(true, Ordering::Release);
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
    pub additional_contexts: Vec<UserMessage>,
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
    pub additional_contexts: Vec<UserMessage>,
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

    /// Canonical successful value, absent for failures.
    #[must_use]
    pub fn value(&self) -> Option<&Value> {
        match self {
            Self::Success(result) => Some(&result.value),
            Self::Failure(_) => None,
        }
    }

    /// Final model-facing content.
    #[must_use]
    pub fn content(&self) -> &[ContentBlock] {
        match self {
            Self::Success(result) => &result.content,
            Self::Failure(result) => &result.content,
        }
    }

    /// Context messages to commit after the durable result.
    #[must_use]
    pub fn additional_contexts(&self) -> &[UserMessage] {
        match self {
            Self::Success(result) => &result.additional_contexts,
            Self::Failure(result) => &result.additional_contexts,
        }
    }

    /// Whether a successful result authoritatively concludes the current turn.
    #[must_use]
    pub fn concludes_turn(&self) -> bool {
        matches!(self, Self::Success(result) if result.concludes_turn)
    }

    /// Optional replayable presentation metadata.
    #[must_use]
    pub fn meta(&self) -> Option<&Value> {
        match self {
            Self::Success(result) => result.meta.as_ref(),
            Self::Failure(result) => result.meta.as_ref(),
        }
    }

    /// Structured failure detail, when this result failed.
    #[must_use]
    pub fn error(&self) -> Option<&ToolFailure> {
        match self {
            Self::Success(_) => None,
            Self::Failure(result) => Some(&result.error),
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
        additional_contexts: Vec<UserMessage>,
    },
    /// Replace a successful canonical value and recompute projections.
    ReplaceValue {
        /// Replacement canonical value.
        value: Value,
        /// Contexts appended after existing result contexts.
        additional_contexts: Vec<UserMessage>,
    },
    /// Turn corrective feedback into a valueless failure.
    Block {
        /// Model-facing correction.
        feedback: Vec<ContentBlock>,
        /// Only contexts explicitly supplied by the blocking policy survive.
        additional_contexts: Vec<UserMessage>,
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ToolExecutionMode {
    /// May overlap with compatible siblings.
    Parallel,
    /// Runs alone and forms an ordering barrier.
    Exclusive,
}

/// Scheduler result after ordered pre-execution policy and guards.
#[derive(Clone, Debug)]
pub enum ScheduledToolPreparation {
    /// Around-dispatch and the tool body remain to run.
    Dispatch {
        /// Registry-minted execution context.
        execution: ToolExecution,
    },
    /// This result still receives ordered post-execution policy.
    PostResult {
        /// Registry-minted execution context.
        execution: ToolExecution,
        /// Candidate result.
        result: ToolExecutionResult,
    },
    /// This result bypasses post-execution policy.
    FinalResult {
        /// Registry-minted execution context.
        execution: ToolExecution,
        /// Candidate result.
        result: ToolExecutionResult,
    },
}

impl ScheduledToolPreparation {
    fn final_result(execution: &ToolExecution, result: ToolExecutionResult) -> Self {
        Self::FinalResult {
            execution: execution.clone(),
            result,
        }
    }

    fn post_result(execution: &ToolExecution, result: ToolExecutionResult) -> Self {
        Self::PostResult {
            execution: execution.clone(),
            result,
        }
    }
}

/// Scheduler result after only the overlap-safe dispatch stage.
#[derive(Clone, Debug)]
pub enum ScheduledToolDispatch {
    /// Candidate still requiring ordered post-execution policy.
    PostResult(ToolExecutionResult),
    /// Pipeline failure that bypasses post-execution policy.
    FinalResult(ToolExecutionResult),
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
    /// The hostile code runtime resolved a program-level failure.
    #[error("{message}")]
    CodeRunFailed {
        /// Failure kind, message, and captured output for model correction.
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
            Self::CodeRunFailed { .. } => ToolErrorInfo {
                name: "CodeRunFailedError".to_owned(),
                code: "CODE_RUN_FAILED".to_owned(),
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
    system_prompt: Option<std::sync::Weak<SystemPrompt>>,
    self_weak: std::sync::Weak<ToolRuntime>,
    code_transports: Mutex<HashMap<String, Arc<ToolDefinition>>>,
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
        Self::build(context, config, None)
    }

    fn build(
        context: Context,
        config: ToolRuntimeConfig,
        system_prompt: Option<std::sync::Weak<SystemPrompt>>,
    ) -> anyhow::Result<Arc<Self>> {
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
        Ok(Arc::new_cyclic(|self_weak| Self {
            context,
            layers,
            default_mode: config.mode,
            max_parallel_sub_calls: config.max_parallel_sub_calls,
            system_prompt,
            self_weak: self_weak.clone(),
            code_transports: Mutex::new(HashMap::new()),
        }))
    }

    /// Creates a registry and wires its live view into system-prompt assembly.
    ///
    /// This is the production composition boundary corresponding to the
    /// source service's mandatory `systemPrompt` injection. [`Self::new`] is
    /// retained for isolated registry tests and lower-level embeddings.
    ///
    /// # Errors
    ///
    /// Returns runtime configuration or provider-registration failures.
    pub fn new_with_system_prompt(
        context: &Context,
        system_prompt: &Arc<SystemPrompt>,
        config: ToolRuntimeConfig,
    ) -> anyhow::Result<Arc<Self>> {
        let runtime = Self::build(context.clone(), config, Some(Arc::downgrade(system_prompt)))?;
        let fiber = Fiber::active_child("tools:system-prompt");
        let child = context.with_fiber(fiber.clone());
        let weak = Arc::downgrade(&runtime);
        let install_result = (|| {
            system_prompt.tools(
                &child,
                Arc::new(move |assemble_context| {
                    let runtime = weak.upgrade().ok_or_else(|| {
                        anyhow::anyhow!("tool runtime was disposed before prompt assembly")
                    })?;
                    runtime.wire_schemas(assemble_context.scope)
                }),
            )?;
            if config.mode != ToolPresentationMode::Native {
                Self::register_code_sections(&runtime, &child, system_prompt)?;
            }
            Ok::<(), anyhow::Error>(())
        })();
        if let Err(error) = install_result {
            return match futures::executor::block_on(fiber.dispose()) {
                Ok(()) => Err(error),
                Err(cleanup) => Err(anyhow::anyhow!("{error:#}: cleanup failed: {cleanup:#}")),
            };
        }
        let cleanup_fiber = fiber.clone();
        let effect = EffectHandle::new("tools:system-prompt", move || {
            Box::pin(async move { cleanup_fiber.dispose().await })
        });
        if let Err(error) = context.own(effect) {
            return match futures::executor::block_on(fiber.dispose()) {
                Ok(()) => Err(error.into()),
                Err(cleanup) => Err(anyhow::anyhow!("{error}: cleanup failed: {cleanup:#}")),
            };
        }
        Ok(runtime)
    }

    /// Provides this registry on `ctx.tools` for the mounting fiber.
    ///
    /// # Errors
    ///
    /// Returns standard duplicate-service or inactive-owner failures.
    pub fn provide(
        self: &Arc<Self>,
        context: &Context,
    ) -> Result<EffectHandle, seekdeep_cordis::CordisError> {
        context.provide(TOOLS, self.clone())
    }

    fn register_code_sections(
        runtime: &Arc<Self>,
        context: &Context,
        system_prompt: &SystemPrompt,
    ) -> anyhow::Result<()> {
        let weak = Arc::downgrade(runtime);
        system_prompt.section(
            context,
            PromptSection::new(
                "tools:code-only",
                CODE_ONLY_SECTION_ORDER,
                PromptText::Dynamic(Arc::new(move |assemble_context| {
                    let runtime = weak.upgrade().ok_or_else(|| {
                        anyhow::anyhow!("tool runtime was disposed before prompt assembly")
                    })?;
                    Ok(
                        if runtime.mode_for(assemble_context.scope) == ToolPresentationMode::Code {
                            CODE_ONLY_INSTRUCTION.to_owned()
                        } else {
                            String::new()
                        },
                    )
                })),
            ),
        )?;

        let weak = Arc::downgrade(runtime);
        system_prompt.section(
            context,
            PromptSection::new(
                "tools:sdk",
                SDK_SECTION_ORDER,
                PromptText::Dynamic(Arc::new(move |assemble_context| {
                    let runtime = weak.upgrade().ok_or_else(|| {
                        anyhow::anyhow!("tool runtime was disposed before prompt assembly")
                    })?;
                    let mode = runtime.mode_for(assemble_context.scope);
                    if mode == ToolPresentationMode::Native {
                        return Ok(String::new());
                    }
                    let code_runtime = runtime.require_code_runtime(mode)?;
                    let schemas = runtime.sdk_schemas(assemble_context.scope);
                    Ok(match code_runtime.language() {
                        "typescript" => render_tools_sdk(&schemas),
                        "python" => render_tools_sdk_py(&schemas),
                        _ => unreachable!("require_code_runtime validates the renderer table"),
                    })
                })),
            ),
        )?;
        Ok(())
    }

    fn wire_schemas(&self, scope: Option<ScopeKey>) -> anyhow::Result<ToolProviderResult> {
        let view = self.view(scope);
        let mode = self.mode_for(scope);
        if mode == ToolPresentationMode::Native {
            return Ok(ToolProviderResult {
                schemas: Self::project_schemas(view.visible.values()),
                known_names: Some(view.known_names.into_iter().collect()),
            });
        }
        self.require_code_runtime(mode)?;
        let schemas = Self::project_schemas(view.visible.values());
        if mode == ToolPresentationMode::Code {
            return Ok(ToolProviderResult {
                schemas: schemas
                    .into_iter()
                    .filter(|schema| schema.name == RUN_CODE_NAME)
                    .collect(),
                known_names: Some(vec![RUN_CODE_NAME.to_owned()]),
            });
        }
        let mut known_names = view.known_names.into_iter().collect::<Vec<_>>();
        known_names.push(RUN_CODE_NAME.to_owned());
        Ok(ToolProviderResult {
            schemas,
            known_names: Some(known_names),
        })
    }

    fn project_schemas<'a>(
        definitions: impl IntoIterator<Item = &'a Arc<ToolDefinition>>,
    ) -> Vec<ToolSchema> {
        definitions
            .into_iter()
            .map(|definition| ToolSchema {
                name: definition.name.clone(),
                description: definition.description.clone(),
                parameters: definition.parameters.clone(),
            })
            .collect()
    }

    fn require_code_runtime(&self, mode: ToolPresentationMode) -> anyhow::Result<Arc<CodeRuntime>> {
        let runtime = self.context.get(CODE_RUNTIME).ok_or_else(|| {
            anyhow::anyhow!(
                "seekdeep-tools: mode {:?} requires a code runtime — load a ctx.codeRuntime implementation (e.g. seekdeep-code-runtime-worker-thread) or set tools mode to \"native\"",
                mode.as_str()
            )
        })?;
        anyhow::ensure!(
            matches!(runtime.language(), "typescript" | "python"),
            "seekdeep-tools: no SDK renderer registered for runtime language {:?} (known: \"typescript\", \"python\")",
            runtime.language()
        );
        Ok(runtime)
    }

    fn code_transport(&self) -> Arc<ToolDefinition> {
        let language = self.context.get(CODE_RUNTIME).map_or_else(
            || "typescript".to_owned(),
            |runtime| runtime.language().to_owned(),
        );
        if let Some(transport) = self.code_transports.lock().get(&language).cloned() {
            return transport;
        }
        let transport = Arc::new(run_code_definition(
            &language,
            self.self_weak.clone(),
            self.max_parallel_sub_calls,
        ));
        self.code_transports
            .lock()
            .insert(language, transport.clone());
        transport
    }

    async fn execute_run_code(
        self: &Arc<Self>,
        arguments: Value,
        execution: ToolRunContext,
        max_parallel: usize,
    ) -> anyhow::Result<Value> {
        let code = arguments
            .get("code")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("invalid code: expected a string"))?
            .to_owned();
        let description = arguments
            .get("description")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("invalid description: expected a string"))?;
        anyhow::ensure!(
            !description.trim().is_empty(),
            "invalid description: expected a non-empty string"
        );
        let code_runtime = self.require_code_runtime(self.default_mode)?;
        let settled = AbortSignal::default();
        let run_signal = AbortSignal::fuse(&execution.signal(), &settled);
        let scheduler = RunCodeScheduler::new(
            self.clone(),
            execution.execution().clone(),
            run_signal.clone(),
            max_parallel,
        );
        let mut functions = IndexMap::new();
        for schema in self.schemas(execution.scope_key()) {
            if schema.name == RUN_CODE_NAME {
                continue;
            }
            let name = schema.name;
            let binding_scheduler = scheduler.clone();
            functions.insert(
                name.clone(),
                Arc::new(move |arguments| binding_scheduler.call(name.clone(), arguments))
                    as CodeBindingFunction,
            );
        }
        let request = CodeRunRequest {
            program: code,
            bindings: vec![CodeBindingNamespace {
                global: "tools".to_owned(),
                functions,
                error_class: Some(CodeBindingErrorClass {
                    name: "ToolCallError".to_owned(),
                    member_name_property: "toolName".to_owned(),
                }),
            }],
            signal: Some(run_signal),
        };
        let run_result = code_runtime.run(request).await;
        settled.abort_with_reason(Value::String("run_code settled".to_owned()));
        let drain_result = scheduler.close_and_drain().await;
        let result = run_result?;
        drain_result?;
        if let Some(error) = result.error {
            let captured = if result.logs.is_empty() {
                String::new()
            } else {
                format!("\nCaptured output:\n{}", result.logs.join("\n"))
            };
            return Err(anyhow::Error::new(ToolRuntimeError::CodeRunFailed {
                message: format!(
                    "code run failed ({}): {}{captured}",
                    code_failure_kind(error.kind),
                    error.message
                ),
            }));
        }
        let mut output = Map::from_iter([("logs".to_owned(), json!(result.logs))]);
        if let Some(value) = result.value {
            output.insert("result".to_owned(), value);
        }
        Ok(Value::Object(output))
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
        F: Fn(ToolDispatchExecution, ExecuteToolNext) -> Fut + Send + Sync + 'static,
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
                let future = middleware(
                    ToolDispatchExecution::new((*execution).clone()),
                    ExecuteToolNext(next),
                );
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

    /// Registers typed `tools/code-dispatch-log` waterfall middleware.
    ///
    /// # Errors
    ///
    /// Returns when the owning context is inactive.
    pub fn on_code_dispatch_log<F, Fut>(
        &self,
        context: &Context,
        middleware: F,
        options: EventOptions,
    ) -> Result<EffectHandle, CordisError>
    where
        F: Fn(CodeDispatchLog, CodeDispatchLogNext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<Vec<ContentBlock>>> + Send + 'static,
    {
        self.context.events().on_waterfall(
            context,
            "tools/code-dispatch-log",
            move |_, args, next| {
                let Some(dispatch) = args.get::<CodeDispatchLog>(0) else {
                    return Box::pin(async {
                        Err(anyhow::anyhow!(
                            "tools/code-dispatch-log is missing its dispatch"
                        ))
                    });
                };
                let future = middleware((*dispatch).clone(), CodeDispatchLogNext(next));
                Box::pin(async move {
                    let shaped = AssertUnwindSafe(future)
                        .catch_unwind()
                        .await
                        .map_err(|panic| anyhow::anyhow!(panic_message(&panic)))??;
                    Ok(EventReply::Value(Arc::new(shaped)))
                })
            },
            options,
        )
    }

    /// Shapes the durable copy of one settled Code Mode sub-dispatch.
    ///
    /// Listener errors and panics are contained: the unmodified settled
    /// content is returned so a policy cannot fail the sub-dispatch or omit its
    /// log event.
    pub async fn shape_code_dispatch_log(&self, dispatch: &CodeDispatchLog) -> Vec<ContentBlock> {
        let original = dispatch.content.clone();
        let args = scoped_args(dispatch.agent, EventArgs::one(dispatch.clone()));
        let reply = self
            .context
            .events()
            .waterfall(
                &scope_target(&self.context, dispatch.agent),
                "tools/code-dispatch-log",
                &args,
                {
                    let original = original.clone();
                    move || Box::pin(async move { Ok(EventReply::Value(Arc::new(original))) })
                },
            )
            .await;
        match reply {
            Ok(reply) => {
                if let Some(content) = reply.downcast::<Vec<ContentBlock>>() {
                    (*content).clone()
                } else {
                    tracing::warn!(
                        tool = %dispatch.name,
                        "tools: code-dispatch-log listener returned invalid content; logging the original settled content"
                    );
                    original
                }
            }
            Err(error) => {
                tracing::warn!(
                    tool = %dispatch.name,
                    error = %error,
                    "tools: code-dispatch-log listener failed; logging the original settled content"
                );
                original
            }
        }
    }

    /// Registers a typed synchronous final-result observer.
    ///
    /// Observer failures are contained by execution-time result dispatch. An
    /// async observer is rejected at compile time, matching the source event
    /// contract rather than silently detaching its work.
    ///
    /// ```compile_fail
    /// use seekdeep_cordis::{Context, EventOptions};
    /// use seekdeep_tools::ToolRuntime;
    /// fn invalid(runtime: &ToolRuntime, context: &Context) {
    ///     runtime.on_result(context, |_, _| async { Ok(()) }, EventOptions::default());
    /// }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns when the owning context is inactive.
    pub fn on_result<F>(
        &self,
        context: &Context,
        observer: F,
        options: EventOptions,
    ) -> Result<EffectHandle, CordisError>
    where
        F: Fn(ToolExecution, ToolExecutionResult) -> anyhow::Result<()> + Send + Sync + 'static,
    {
        self.context.events().on_sync(
            context,
            "tools/result",
            move |_, args| {
                let execution = args
                    .get::<ToolExecution>(0)
                    .ok_or_else(|| anyhow::anyhow!("tools/result is missing its execution"))?;
                let result = args
                    .get::<ToolExecutionResult>(1)
                    .ok_or_else(|| anyhow::anyhow!("tools/result is missing its result"))?;
                observer((*execution).clone(), (*result).clone())?;
                Ok(EventReply::Undefined)
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
        self: &Arc<Self>,
        context: &Context,
        mode: ToolPresentationMode,
    ) -> anyhow::Result<EffectHandle> {
        anyhow::ensure!(
            scope_of(context).is_some(),
            "tools.presentAs() requires a scoped context (agent.ctx): a context-global presentation is the `mode` config field on the tools row"
        );
        let fiber = Fiber::active_child("tools.presentAs()");
        let child = context.with_fiber(fiber.clone());
        let install_result = (|| {
            self.layers.effect(
            &child,
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
            )?;
            if mode != ToolPresentationMode::Native
                && let Some(system_prompt) = self
                    .system_prompt
                    .as_ref()
                    .and_then(std::sync::Weak::upgrade)
            {
                Self::register_code_sections(self, &child, &system_prompt)?;
            }
            Ok::<(), anyhow::Error>(())
        })();
        if let Err(error) = install_result {
            return match futures::executor::block_on(fiber.dispose()) {
                Ok(()) => Err(error),
                Err(cleanup) => Err(anyhow::anyhow!("{error:#}: cleanup failed: {cleanup:#}")),
            };
        }
        let cleanup_fiber = fiber.clone();
        let effect = EffectHandle::new("tools.presentAs()", move || {
            Box::pin(async move { cleanup_fiber.dispose().await })
        });
        match context.own(effect.clone()) {
            Ok(effect) => Ok(effect),
            Err(error) => match futures::executor::block_on(fiber.dispose()) {
                Ok(()) => Err(error.into()),
                Err(cleanup) => Err(anyhow::anyhow!("{error}: cleanup failed: {cleanup:#}")),
            },
        }
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
        let Some(tool) =
            self.resolve_execution(&input.name, input.scope_key(), input.parent.is_some())
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
        match self.prepare_scheduled(input).await {
            ScheduledToolPreparation::Dispatch { execution } => {
                match self.dispatch_scheduled(&execution).await {
                    ScheduledToolDispatch::PostResult(result) => {
                        self.finalize_scheduled(&execution, result).await
                    }
                    ScheduledToolDispatch::FinalResult(result) => {
                        self.finish_scheduled(&execution, result)
                    }
                }
            }
            ScheduledToolPreparation::PostResult { execution, result } => {
                self.finalize_scheduled(&execution, result).await
            }
            ScheduledToolPreparation::FinalResult { execution, result } => {
                self.finish_scheduled(&execution, result)
            }
        }
    }

    /// Materializes one input and runs only ordered pre-execution policy.
    ///
    /// Agent-loop schedulers use this with [`Self::dispatch_scheduled`] and
    /// [`Self::finalize_scheduled`] so tool bodies may overlap while policy and
    /// result publication remain model ordered.
    pub async fn prepare_scheduled(
        self: &Arc<Self>,
        input: ToolExecutionInput,
    ) -> ScheduledToolPreparation {
        let name = input.name.clone();
        let agent = input.scope_key();
        let nested = input.parent.is_some();
        let visible = self.get(&name, agent);
        let collapsed = visible.is_some() && self.collapses(&name, agent, nested);
        let caller_aborted = input.signal.is_aborted();
        let finalizer = if collapsed && !caller_aborted {
            None
        } else {
            visible.and_then(|tool| tool.finalize_content.clone())
        };
        let execution = Self::create_execution(input, finalizer);
        if collapsed {
            let result = if caller_aborted {
                aborted_before_result()
            } else {
                tool_error_result(anyhow::Error::new(ToolRuntimeError::ToolNotFound {
                    message: format!(
                        "unknown tool {:?}: only `{RUN_CODE_NAME}` is callable directly — call `{}` from inside a `{RUN_CODE_NAME}` program instead",
                        execution.name, execution.name
                    ),
                }))
            };
            return ScheduledToolPreparation::final_result(&execution, result);
        }
        if Self::caller_cancelled(&execution) {
            return ScheduledToolPreparation::final_result(&execution, aborted_before_result());
        }
        let pre = match AssertUnwindSafe(self.pre_execute(&execution))
            .catch_unwind()
            .await
        {
            Ok(Ok(decision)) => decision,
            Ok(Err(error)) => {
                return ScheduledToolPreparation::final_result(
                    &execution,
                    tool_error_result(error),
                );
            }
            Err(panic) => {
                return ScheduledToolPreparation::final_result(
                    &execution,
                    tool_error_result(anyhow::anyhow!(panic_message(&panic))),
                );
            }
        };
        let (pre, approval_cancelled) = match pre {
            PreToolDecision::Ask { reason } => match self.resolve_ask(&execution, reason).await {
                Ok(resolution) => resolution,
                Err(error) => {
                    return ScheduledToolPreparation::final_result(
                        &execution,
                        tool_error_result(error),
                    );
                }
            },
            decision => (decision, false),
        };
        if Self::caller_cancelled(&execution) && approval_cancelled {
            return ScheduledToolPreparation::post_result(&execution, aborted_before_result());
        }
        let denial = match pre {
            PreToolDecision::Allow => {
                match catch_unwind(AssertUnwindSafe(|| self.guard_reason(&execution))) {
                    Ok(reason) => reason,
                    Err(panic) => {
                        return ScheduledToolPreparation::final_result(
                            &execution,
                            tool_error_result(anyhow::anyhow!(panic_message(&panic))),
                        );
                    }
                }
            }
            PreToolDecision::Deny { reason } => Some(reason),
            PreToolDecision::Ask { .. } => unreachable!("ask is resolved before guard policy"),
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
            return ScheduledToolPreparation::post_result(&execution, denied);
        }
        if Self::caller_cancelled(&execution) {
            return ScheduledToolPreparation::post_result(&execution, aborted_before_result());
        }
        ScheduledToolPreparation::Dispatch {
            execution: execution.clone(),
        }
    }

    async fn resolve_ask(
        &self,
        execution: &ToolExecution,
        reason: Option<String>,
    ) -> anyhow::Result<(PreToolDecision, bool)> {
        let Some(approval) = self.context.get(APPROVAL) else {
            return Ok((
                PreToolDecision::Deny {
                    reason: reason.unwrap_or_else(|| {
                        format!(
                            "tool {:?} requires approval (not yet supported)",
                            execution.name
                        )
                    }),
                },
                false,
            ));
        };
        let Some(agent) = execution.agent.clone() else {
            return Ok((
                PreToolDecision::Deny {
                    reason: format!(
                        "tool {:?} requires approval, but the call has no agent to route it through",
                        execution.name
                    ),
                },
                false,
            ));
        };
        let mut request = ApprovalRequest::new(agent, &execution.name)
            .with_call_id(execution.call_id.clone())
            .with_signal(execution.signal());
        if let Some(reason) = reason {
            request = request.with_reason(reason);
        }
        let outcome = approval.request(request).await?;
        Ok(match outcome {
            ApprovalOutcome::AllowedOnce => (PreToolDecision::Allow, false),
            ApprovalOutcome::Rejected => (
                PreToolDecision::Deny {
                    reason: format!("the user rejected tool {:?}", execution.name),
                },
                false,
            ),
            ApprovalOutcome::Cancelled => (
                PreToolDecision::Deny {
                    reason: format!("approval for tool {:?} was cancelled", execution.name),
                },
                true,
            ),
            ApprovalOutcome::Unavailable => (
                PreToolDecision::Deny {
                    reason: format!(
                        "tool {:?} requires approval, but no approval channel is available",
                        execution.name
                    ),
                },
                false,
            ),
        })
    }

    /// Runs only around-dispatch middleware and the tool body.
    pub async fn dispatch_scheduled(
        self: &Arc<Self>,
        execution: &ToolExecution,
    ) -> ScheduledToolDispatch {
        let dispatched = match AssertUnwindSafe(self.around_execute(execution))
            .catch_unwind()
            .await
        {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => {
                return ScheduledToolDispatch::FinalResult(tool_error_result(error));
            }
            Err(panic) => {
                return ScheduledToolDispatch::FinalResult(tool_error_result(anyhow::anyhow!(
                    panic_message(&panic)
                )));
            }
        };
        let normalized = match self.normalize_dispatch_result(execution, dispatched) {
            Ok(result) => result,
            Err(error) => return ScheduledToolDispatch::FinalResult(tool_error_result(error)),
        };
        let normalized = Self::attach_deferred(execution, normalized);
        let normalized = if Self::caller_cancelled(execution) && !normalized.is_error() {
            Self::cancellation_result(execution, Some(&normalized))
        } else {
            normalized
        };
        ScheduledToolDispatch::PostResult(normalized)
    }

    /// Runs ordered post-execution policy, then finalizes and publishes.
    pub async fn finalize_scheduled(
        &self,
        execution: &ToolExecution,
        result: ToolExecutionResult,
    ) -> ToolExecutionResult {
        let candidate = match AssertUnwindSafe(self.post_execute(execution, &result))
            .catch_unwind()
            .await
        {
            Ok(Ok(post)) if Self::caller_cancelled(execution) && !post.is_error() => {
                Self::cancellation_result(execution, Some(&post))
            }
            Ok(Ok(post)) => post,
            Ok(Err(error)) => tool_error_result(error),
            Err(panic) => tool_error_result(anyhow::anyhow!(panic_message(&panic))),
        };
        self.finish_scheduled(execution, candidate)
    }

    /// Applies the snapshotted definition finalizer and publishes the result.
    pub fn finish_scheduled(
        &self,
        execution: &ToolExecution,
        result: ToolExecutionResult,
    ) -> ToolExecutionResult {
        self.finish(execution, result, execution.state.finalizer.clone())
    }

    async fn pre_execute(&self, execution: &ToolExecution) -> anyhow::Result<PreToolDecision> {
        let args = scoped_args(execution.scope_key(), EventArgs::one(execution.clone()));
        let reply = self
            .context
            .events()
            .waterfall(
                &scope_target(&self.context, execution.scope_key()),
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
        let args = scoped_args(execution.scope_key(), EventArgs::one(execution.clone()));
        let runtime = self.clone();
        let inner_execution = execution.clone();
        let reply = self
            .context
            .events()
            .waterfall(
                &scope_target(&self.context, execution.scope_key()),
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
        let args = scoped_args(
            execution.scope_key(),
            EventArgs::from_values(vec![Arc::new(execution.clone()), Arc::new(result.clone())]),
        );
        let reply = self
            .context
            .events()
            .waterfall(
                &scope_target(&self.context, execution.scope_key()),
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
            .resolve_execution(
                &execution.name,
                execution.scope_key(),
                execution.parent.is_some(),
            )
            .ok_or_else(|| tool_not_found(&execution.name, None))?;
        execution.state.body_invoked.store(true, Ordering::Release);
        let arguments = execution.arguments.clone();
        let body = tool.execute.clone();
        let body_execution = execution.clone();
        let future = catch_unwind(AssertUnwindSafe(|| {
            body(arguments, ToolRunContext::new(body_execution))
        }))
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
                    .resolve_execution(
                        &execution.name,
                        execution.scope_key(),
                        execution.parent.is_some(),
                    )
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
                    .resolve_execution(
                        &execution.name,
                        execution.scope_key(),
                        execution.parent.is_some(),
                    )
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
        let arguments = scoped_args(
            execution.scope_key(),
            EventArgs::from_values(vec![Arc::new(execution.clone()), Arc::new(result.clone())]),
        );
        match self.context.events().prepare_emit(
            &scope_target(&self.context, execution.scope_key()),
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

    fn create_execution(
        input: ToolExecutionInput,
        finalizer: Option<ToolContentFinalizer>,
    ) -> ToolExecution {
        let root_call_id = input.root_call_id.unwrap_or_else(|| input.call_id.clone());
        let signal = input.signal;
        ToolExecution {
            call_id: input.call_id,
            root_call_id,
            name: input.name,
            arguments: input.arguments,
            agent: input.agent,
            agent_scope: input.agent_scope,
            agent_session: input.agent_session,
            parent: input.parent,
            token: ToolExecutionToken::new(),
            state: Arc::new(ExecutionState {
                caller_signal: signal.clone(),
                signal: Mutex::new(signal),
                body_invoked: AtomicBool::new(false),
                deferred_contexts: Mutex::new(Vec::new()),
                concludes_turn: AtomicBool::new(false),
                finalizer,
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
            execution.scope_key().and_then(|agent| {
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
        if self.mode_for(scope) != ToolPresentationMode::Native {
            visible.insert(RUN_CODE_NAME.to_owned(), self.code_transport());
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

#[derive(Clone)]
struct RunCodeScheduler {
    sender: mpsc::UnboundedSender<RunDriverMessage>,
    signal: AbortSignal,
    next_dispatch: Arc<AtomicUsize>,
    driver: Arc<tokio::sync::Mutex<Option<JoinHandle<anyhow::Result<()>>>>>,
}

impl RunCodeScheduler {
    fn new(
        runtime: Arc<ToolRuntime>,
        outer: ToolExecution,
        signal: AbortSignal,
        max_parallel: usize,
    ) -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        let driver_sender = sender.clone();
        let driver_signal = signal.clone();
        let driver = tokio::spawn(async move {
            run_code_driver(
                runtime,
                outer,
                driver_signal,
                max_parallel,
                receiver,
                driver_sender,
            )
            .await
        });
        Self {
            sender,
            signal,
            next_dispatch: Arc::new(AtomicUsize::new(0)),
            driver: Arc::new(tokio::sync::Mutex::new(Some(driver))),
        }
    }

    fn call(&self, name: String, arguments: Value) -> seekdeep_code_runtime::CodeBindingFuture {
        let scheduler = self.clone();
        Box::pin(async move {
            if scheduler.signal.is_aborted() {
                anyhow::bail!(
                    "run_code run is over ({}); {name} not dispatched",
                    abort_reason(&scheduler.signal)
                );
            }
            let sequence = scheduler.next_dispatch.fetch_add(1, Ordering::AcqRel) + 1;
            let (respond, response) = oneshot::channel();
            scheduler
                .sender
                .send(RunDriverMessage::Submit(RunDispatchRequest {
                    sequence,
                    name: name.clone(),
                    arguments,
                    respond,
                }))
                .map_err(|_| anyhow::anyhow!("run_code dispatch scheduler stopped"))?;
            let result = response
                .await
                .map_err(|_| anyhow::anyhow!("run_code dispatch scheduler stopped"))??;
            if scheduler.signal.is_aborted() {
                anyhow::bail!(
                    "run_code run is over ({}); {name} result discarded",
                    abort_reason(&scheduler.signal)
                );
            }
            Ok(result)
        })
    }

    async fn close_and_drain(&self) -> anyhow::Result<()> {
        let _ = self.sender.send(RunDriverMessage::Close);
        let Some(driver) = self.driver.lock().await.take() else {
            return Ok(());
        };
        driver
            .await
            .map_err(|error| anyhow::anyhow!("run_code dispatch driver failed: {error}"))?
    }
}

enum RunDriverMessage {
    Submit(RunDispatchRequest),
    Settled {
        id: usize,
        execution: ToolExecution,
        outcome: Box<ScheduledToolDispatch>,
    },
    Close,
}

struct RunDispatchRequest {
    sequence: usize,
    name: String,
    arguments: Value,
    respond: oneshot::Sender<anyhow::Result<Value>>,
}

enum ParkedDispatch {
    Post {
        execution: ToolExecution,
        result: ToolExecutionResult,
    },
    Final {
        execution: ToolExecution,
        result: ToolExecutionResult,
    },
}

struct RunDispatchEntry {
    id: usize,
    sub_call_id: CallId,
    name: String,
    arguments: Value,
    input: ToolExecutionInput,
    respond: Option<oneshot::Sender<anyhow::Result<Value>>>,
    mode: Option<ToolExecutionMode>,
    parked: Option<ParkedDispatch>,
}

impl RunDispatchEntry {
    fn is_settled(&self) -> bool {
        self.parked.is_some()
    }
}

struct RunCodeDriver {
    runtime: Arc<ToolRuntime>,
    outer: ToolExecution,
    signal: AbortSignal,
    max_parallel: usize,
    receiver: mpsc::UnboundedReceiver<RunDriverMessage>,
    sender: mpsc::UnboundedSender<RunDriverMessage>,
    pending: VecDeque<RunDispatchEntry>,
    commits: VecDeque<RunDispatchEntry>,
    in_flight: usize,
    exclusive_active: bool,
    closed: bool,
    observed_abort: bool,
    log_tasks: VecDeque<JoinHandle<()>>,
}

async fn run_code_driver(
    runtime: Arc<ToolRuntime>,
    outer: ToolExecution,
    signal: AbortSignal,
    max_parallel: usize,
    receiver: mpsc::UnboundedReceiver<RunDriverMessage>,
    sender: mpsc::UnboundedSender<RunDriverMessage>,
) -> anyhow::Result<()> {
    RunCodeDriver {
        runtime,
        outer,
        signal,
        max_parallel,
        receiver,
        sender,
        pending: VecDeque::new(),
        commits: VecDeque::new(),
        in_flight: 0,
        exclusive_active: false,
        closed: false,
        observed_abort: false,
        log_tasks: VecDeque::new(),
    }
    .run()
    .await
}

impl RunCodeDriver {
    async fn run(mut self) -> anyhow::Result<()> {
        loop {
            self.reap_finished_logs().await;
            self.observe_abort();
            if self.commit_head().await {
                continue;
            }
            if self.start_head().await {
                continue;
            }
            if self.is_quiescent() {
                break;
            }
            let message = self.receive().await;
            self.handle_message(message);
        }
        while let Some(task) = self.log_tasks.pop_front() {
            let _ = task.await;
        }
        Ok(())
    }

    async fn reap_finished_logs(&mut self) {
        while self.log_tasks.front().is_some_and(JoinHandle::is_finished) {
            if let Some(task) = self.log_tasks.pop_front() {
                let _ = task.await;
            }
        }
    }

    fn observe_abort(&mut self) {
        if !self.signal.is_aborted() {
            return;
        }
        self.observed_abort = true;
        while let Some(mut entry) = self.pending.pop_front() {
            if let Some(respond) = entry.respond.take() {
                let _ = respond.send(Err(anyhow::anyhow!(
                    "run_code run is over ({}); {} tool call abandoned",
                    abort_reason(&self.signal),
                    entry.name
                )));
            }
        }
    }

    async fn commit_head(&mut self) -> bool {
        if !self
            .commits
            .front()
            .is_some_and(RunDispatchEntry::is_settled)
        {
            return false;
        }
        let mut entry = self.commits.pop_front().expect("commit head exists");
        let parked = entry.parked.take().expect("settled entry is parked");
        let result = match parked {
            ParkedDispatch::Post { execution, result } => {
                self.runtime.finalize_scheduled(&execution, result).await
            }
            ParkedDispatch::Final { execution, result } => {
                self.runtime.finish_scheduled(&execution, result)
            }
        };
        self.forward_nested_effects(&result);
        if let Some(respond) = entry.respond.take() {
            let _ = respond.send(binding_result(&result));
        }
        self.spawn_log(&entry, result);
        if entry.mode == Some(ToolExecutionMode::Exclusive) {
            self.exclusive_active = false;
        }
        if self.log_tasks.len() > self.max_parallel
            && let Some(task) = self.log_tasks.pop_front()
        {
            let _ = task.await;
        }
        true
    }

    fn forward_nested_effects(&self, result: &ToolExecutionResult) {
        for context in result.additional_contexts().iter().cloned() {
            self.outer.state.deferred_contexts.lock().push(context);
        }
        if result.concludes_turn() {
            self.outer
                .state
                .concludes_turn
                .store(true, Ordering::Release);
        }
    }

    fn spawn_log(&mut self, entry: &RunDispatchEntry, result: ToolExecutionResult) {
        if self.outer.scope_key().is_none() {
            return;
        }
        let Some(session) = self.outer.session() else {
            return;
        };
        let runtime = self.runtime.clone();
        let outer = self.outer.clone();
        let sub_call_id = entry.sub_call_id.clone();
        let name = entry.name.clone();
        let arguments = entry.arguments.clone();
        self.log_tasks.push_back(tokio::spawn(async move {
            append_code_dispatch_log(
                &runtime,
                &session,
                &outer,
                sub_call_id,
                name,
                arguments,
                result,
            )
            .await;
        }));
    }

    async fn start_head(&mut self) -> bool {
        let Some(head) = self.pending.front() else {
            return false;
        };
        let mode = self.runtime.execution_mode(&head.input);
        let has_capacity = !self.exclusive_active
            && match mode {
                ToolExecutionMode::Exclusive => self.in_flight == 0,
                ToolExecutionMode::Parallel => self.in_flight < self.max_parallel,
            };
        if !has_capacity {
            return false;
        }
        let mut entry = self.pending.pop_front().expect("pending head exists");
        entry.mode = Some(mode);
        self.exclusive_active |= mode == ToolExecutionMode::Exclusive;
        if let Err(error) = self.append_start(&entry) {
            if let Some(respond) = entry.respond.take() {
                let _ = respond.send(Err(error));
            }
            self.exclusive_active &= mode != ToolExecutionMode::Exclusive;
            return true;
        }
        self.prepare_and_launch(entry).await;
        true
    }

    fn append_start(&self, entry: &RunDispatchEntry) -> anyhow::Result<()> {
        let Some(session) = self.outer.session() else {
            return Ok(());
        };
        if self.outer.scope_key().is_none() {
            return Ok(());
        }
        session.append(
            "tool/code-dispatch-start",
            serde_json::to_value(CodeDispatchStartEventData {
                root_call_id: self.outer.root_call_id.clone(),
                parent_call_id: self.outer.call_id.clone(),
                sub_call_id: entry.sub_call_id.clone(),
                name: entry.name.clone(),
                arguments: entry.arguments.clone(),
            })?,
            AppendOptions::default(),
        )?;
        Ok(())
    }

    async fn prepare_and_launch(&mut self, mut entry: RunDispatchEntry) {
        match self.runtime.prepare_scheduled(entry.input.clone()).await {
            ScheduledToolPreparation::Dispatch { execution } => {
                let id = entry.id;
                let runtime = self.runtime.clone();
                let sender = self.sender.clone();
                let body_execution = execution.clone();
                self.commits.push_back(entry);
                self.in_flight += 1;
                tokio::spawn(async move {
                    let outcome = runtime.dispatch_scheduled(&body_execution).await;
                    let _ = sender.send(RunDriverMessage::Settled {
                        id,
                        execution: body_execution,
                        outcome: Box::new(outcome),
                    });
                });
            }
            ScheduledToolPreparation::PostResult { execution, result } => {
                entry.parked = Some(ParkedDispatch::Post { execution, result });
                self.commits.push_back(entry);
            }
            ScheduledToolPreparation::FinalResult { execution, result } => {
                entry.parked = Some(ParkedDispatch::Final { execution, result });
                self.commits.push_back(entry);
            }
        }
    }

    fn is_quiescent(&self) -> bool {
        self.closed && self.pending.is_empty() && self.commits.is_empty() && self.in_flight == 0
    }

    async fn receive(&mut self) -> Option<RunDriverMessage> {
        if self.observed_abort {
            return self.receiver.recv().await;
        }
        tokio::select! {
            message = self.receiver.recv() => message,
            () = self.signal.cancelled() => {
                self.observed_abort = true;
                None
            }
        }
    }

    fn handle_message(&mut self, message: Option<RunDriverMessage>) {
        let Some(message) = message else {
            if self.observed_abort {
                return;
            }
            self.closed = true;
            return;
        };
        match message {
            RunDriverMessage::Submit(request) => self.submit(request),
            RunDriverMessage::Settled {
                id,
                execution,
                outcome,
            } => self.settle(id, execution, *outcome),
            RunDriverMessage::Close => self.closed = true,
        }
    }

    fn submit(&mut self, request: RunDispatchRequest) {
        if self.closed || self.signal.is_aborted() {
            let _ = request.respond.send(Err(anyhow::anyhow!(
                "run_code run is over ({}); {} not dispatched",
                abort_reason(&self.signal),
                request.name
            )));
            return;
        }
        let sub_call_id = CallId::new(format!("{}:code:{}", self.outer.call_id, request.sequence));
        self.pending.push_back(RunDispatchEntry {
            id: request.sequence,
            sub_call_id: sub_call_id.clone(),
            name: request.name.clone(),
            arguments: request.arguments.clone(),
            input: ToolExecutionInput {
                call_id: sub_call_id,
                root_call_id: Some(self.outer.root_call_id.clone()),
                name: request.name,
                arguments: request.arguments,
                agent: self.outer.agent.clone(),
                agent_scope: self.outer.agent_scope,
                agent_session: self.outer.session(),
                parent: Some(self.outer.token),
                signal: self.signal.clone(),
            },
            respond: Some(request.respond),
            mode: None,
            parked: None,
        });
    }

    fn settle(&mut self, id: usize, execution: ToolExecution, outcome: ScheduledToolDispatch) {
        if let Some(entry) = self.commits.iter_mut().find(|entry| entry.id == id) {
            entry.parked = Some(match outcome {
                ScheduledToolDispatch::PostResult(result) => {
                    ParkedDispatch::Post { execution, result }
                }
                ScheduledToolDispatch::FinalResult(result) => {
                    ParkedDispatch::Final { execution, result }
                }
            });
        }
        self.in_flight = self.in_flight.saturating_sub(1);
    }
}

fn binding_result(result: &ToolExecutionResult) -> anyhow::Result<Value> {
    match result {
        ToolExecutionResult::Success(success) => Ok(success.value.clone()),
        ToolExecutionResult::Failure(failure) => {
            Err(anyhow::anyhow!(failure.error.message.clone()))
        }
    }
}

async fn append_code_dispatch_log(
    runtime: &ToolRuntime,
    session: &Session,
    outer: &ToolExecution,
    sub_call_id: CallId,
    name: String,
    arguments: Value,
    result: ToolExecutionResult,
) {
    let content = runtime
        .shape_code_dispatch_log(&CodeDispatchLog {
            execution: outer.clone(),
            agent: outer.scope_key(),
            sub_call_id: sub_call_id.clone(),
            name: name.clone(),
            is_error: result.is_error(),
            content: result.content().to_vec(),
        })
        .await;
    let event_data = serde_json::to_value(CodeDispatchEventData {
        root_call_id: outer.root_call_id.clone(),
        parent_call_id: outer.call_id.clone(),
        sub_call_id,
        name,
        arguments,
        is_error: result.is_error(),
        content,
    });
    let append_result = event_data.map_err(anyhow::Error::from).and_then(|data| {
        session
            .append("tool/code-dispatch", data, AppendOptions::default())
            .map_err(anyhow::Error::from)
    });
    if let Err(error) = append_result {
        tracing::warn!(%error, "tools: failed to append code dispatch log");
    }
}

fn abort_reason(signal: &AbortSignal) -> String {
    match signal.reason().unwrap_or(Value::Null) {
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value,
        Value::Array(values) => values
            .into_iter()
            .map(|value| match value {
                Value::Null => String::new(),
                other => abort_reason_value(other),
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".to_owned(),
    }
}

fn abort_reason_value(value: Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value,
        Value::Array(_) => String::new(),
        Value::Object(_) => "[object Object]".to_owned(),
    }
}

const fn code_failure_kind(kind: CodeRunFailureKind) -> &'static str {
    match kind {
        CodeRunFailureKind::Exception => "exception",
        CodeRunFailureKind::Timeout => "timeout",
        CodeRunFailureKind::Abort => "abort",
        CodeRunFailureKind::WorkerExit => "worker-exit",
        CodeRunFailureKind::InvalidOutput => "invalid-output",
        CodeRunFailureKind::OutputLimit => "output-limit",
    }
}

fn run_code_definition(
    language: &str,
    runtime: std::sync::Weak<ToolRuntime>,
    max_parallel: usize,
) -> ToolDefinition {
    let (description, code_description) = if language == "python" {
        (
            "Execute a Python program against the available tools. Takes two required arguments: `code`, the BODY of an async function (top-level `await` and `return` work), and `description`, a short summary of what the program does. Call tools as `await tools.name(args)` per the declarations in the system prompt. Answer with `print(...)` and/or `return <value>` — only that comes back, so curate it.",
            "The program: the body of an async Python function.",
        )
    } else {
        (
            "Execute a TypeScript program against the available tools. Takes two required arguments: `code`, the BODY of an async function (erasable syntax only; top-level `await` and `return` work), and `description`, a short summary of what the program does. Call tools as `await tools.name(args)` per the declarations in the system prompt. Only what you print or return comes back — curate it.",
            "The program: the body of an async TypeScript function.",
        )
    };
    let parameters = json!({
        "type": "object",
        "properties": {
            "code": {
                "type": "string",
                "description": code_description,
            },
            "description": {
                "type": "string",
                "description": "Clear, concise description of what this program does in active voice, 5-10 words (shown in the UI). Examples: \"Count TODO markers across packages\"; \"Read failing test and its fixture\"; \"Rename config key in every cordis.yml\".",
            },
        },
        "required": ["code", "description"],
    })
    .as_object()
    .expect("run_code parameters are an object")
    .clone();
    let output_schema = assert_supported_json_schema(json!({
        "type": "object",
        "properties": {
            "logs": { "type": "array", "items": { "type": "string" } },
            "result": {},
        },
        "required": ["logs"],
        "additionalProperties": false,
    }))
    .expect("run_code output schema is supported");
    ToolDefinition::new(
        RUN_CODE_NAME,
        description,
        parameters,
        ToolOutputDefinition::new(
            Arc::new(output_schema),
            Arc::new(|_, value| {
                let logs = value
                    .get("logs")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n");
                let rendered = value.get("result").map_or_else(String::new, |result| {
                    result.as_str().map_or_else(
                        || serde_json::to_string_pretty(result).unwrap_or_default(),
                        str::to_owned,
                    )
                });
                let text = [logs, rendered]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                Ok(vec![ContentBlock::Text {
                    text: if text.is_empty() {
                        "(run_code completed with no output)".to_owned()
                    } else {
                        text
                    },
                }])
            }),
        ),
        Arc::new(move |arguments, execution| {
            let runtime = runtime.clone();
            Box::pin(async move {
                let runtime = runtime.upgrade().ok_or_else(|| {
                    anyhow::anyhow!("tool runtime was disposed before run_code execution")
                })?;
                runtime
                    .execute_run_code(arguments, execution, max_parallel)
                    .await
            })
        }),
    )
}

/// Constructs, prompt-wires, and lifecycle-publishes `ctx.tools`.
///
/// # Errors
///
/// Returns configuration, prompt-registration, or service-publication failures.
pub fn install(
    context: &Context,
    system_prompt: &Arc<SystemPrompt>,
    config: ToolRuntimeConfig,
) -> anyhow::Result<Arc<ToolRuntime>> {
    let runtime = ToolRuntime::new_with_system_prompt(context, system_prompt, config)?;
    runtime.provide(context)?;
    Ok(runtime)
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
    let harness_error = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<HarnessError>());
    let fs_error = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<seekdeep_fs::FsError>());
    let info = error.downcast_ref::<ToolRuntimeError>().map_or_else(
        || {
            error.downcast_ref::<ToolArgsError>().map_or_else(
                || {
                    harness_error
                        .map(|error| ToolErrorInfo {
                            name: error.name().to_owned(),
                            code: error.code().to_owned(),
                        })
                        .or_else(|| {
                            fs_error.map(|error| ToolErrorInfo {
                                name: error.name().to_owned(),
                                code: error.code.as_str().to_owned(),
                            })
                        })
                },
                |error| {
                    Some(ToolErrorInfo {
                        name: "ToolArgsError".to_owned(),
                        code: error.code.to_owned(),
                    })
                },
            )
        },
        |error| Some(error.info()),
    );
    let message = harness_error.map_or_else(
        || fs_error.map_or_else(|| format!("{error:#}"), |error| error.message.clone()),
        |error| error.message().to_owned(),
    );
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
    additional_contexts: Vec<UserMessage>,
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
    use async_trait::async_trait;
    use seekdeep_code_runtime::{CodeRunRequest, CodeRunResult, CodeRuntimeBackend};
    use seekdeep_code_runtime_worker_thread::{
        WorkerThreadCodeRuntimeConfig, install as install_worker_runtime,
    };
    use seekdeep_core::session::{SessionHeader, SessionId};
    use serde_json::json;

    use super::*;
    use crate::assert_supported_json_schema;
    use seekdeep_scope::create_scope;
    use seekdeep_system_prompt::{AssembleContext, SystemPromptConfig};

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
            agent: None,
            agent_scope: agent,
            agent_session: None,
            parent: None,
            signal: AbortSignal::default(),
        }
    }

    fn agent_with_cwd(context: &Context, id: &str, cwd: Option<&str>) -> Arc<Agent> {
        let id = SessionId::new(id);
        let mut header = SessionHeader::new(id.clone());
        header.cwd = cwd.map(str::to_owned);
        let session = Session::create(&id, None, Some(header)).unwrap();
        let inbox = Arc::new(
            seekdeep_agent::Inbox::new(
                session.clone(),
                Arc::new(seekdeep_agent::NoopInboxNotifications),
            )
            .unwrap(),
        );
        Arc::new(Agent::new(
            id,
            seekdeep_agent::AgentOptions::default(),
            session,
            inbox,
            context.clone(),
            ScopeKey::new(),
        ))
    }

    #[test]
    fn session_cwd_reads_only_the_live_calling_agent_workspace() {
        let context = Context::new();
        let agent = agent_with_cwd(&context, "workspace", Some("/work/project"));
        let fallback = agent_with_cwd(&context, "fallback", Some("/wrong/fallback"));
        let execution = ToolRuntime::create_execution(
            ToolExecutionInput::new(CallId::new("cwd"), "lsp", json!({}), AbortSignal::default())
                .with_agent(agent)
                .with_agent_session(fallback.session().clone()),
            None,
        );
        assert_eq!(execution.session_cwd(), Some("/work/project"));

        let synthetic = ToolRuntime::create_execution(
            ToolExecutionInput::new(
                CallId::new("synthetic"),
                "lsp",
                json!({}),
                AbortSignal::default(),
            )
            .with_agent_session(fallback.session().clone()),
            None,
        );
        assert_eq!(synthetic.session_cwd(), None);
        let missing = agent_with_cwd(&context, "missing", None);
        let missing = ToolRuntime::create_execution(
            ToolExecutionInput::new(
                CallId::new("missing"),
                "lsp",
                json!({}),
                AbortSignal::default(),
            )
            .with_agent(missing),
            None,
        );
        assert_eq!(missing.session_cwd(), None);
    }

    #[test]
    fn code_dispatch_event_types_pin_the_durable_session_shapes() {
        let start = CodeDispatchStartEventData {
            root_call_id: CallId::new("root"),
            parent_call_id: CallId::new("parent"),
            sub_call_id: CallId::new("parent:code:1"),
            name: "read".to_owned(),
            arguments: json!({"path": "README.md"}),
        };
        assert_eq!(
            serde_json::to_value(&start).expect("start event"),
            json!({
                "rootCallId": "root",
                "parentCallId": "parent",
                "subCallId": "parent:code:1",
                "name": "read",
                "arguments": {"path": "README.md"},
            })
        );

        let settled = CodeDispatchEventData {
            root_call_id: start.root_call_id,
            parent_call_id: start.parent_call_id,
            sub_call_id: start.sub_call_id,
            name: start.name,
            arguments: start.arguments,
            is_error: false,
            content: vec![ContentBlock::Text {
                text: "contents".to_owned(),
            }],
        };
        let encoded = serde_json::to_value(&settled).expect("settled event");
        assert_eq!(
            encoded,
            json!({
                "rootCallId": "root",
                "parentCallId": "parent",
                "subCallId": "parent:code:1",
                "name": "read",
                "arguments": {"path": "README.md"},
                "isError": false,
                "content": [{"type": "text", "text": "contents"}],
            })
        );
        assert_eq!(
            serde_json::from_value::<CodeDispatchEventData>(encoded).expect("decode"),
            settled
        );
    }

    #[derive(Debug)]
    struct StubCodeRuntime;

    #[async_trait]
    impl CodeRuntimeBackend for StubCodeRuntime {
        fn language(&self) -> &'static str {
            "typescript"
        }

        fn isolation(&self) -> &'static str {
            "stub"
        }

        async fn run(&self, _request: CodeRunRequest) -> anyhow::Result<CodeRunResult> {
            Ok(CodeRunResult::default())
        }
    }

    #[derive(Debug)]
    struct PythonCodeRuntime;

    #[async_trait]
    impl CodeRuntimeBackend for PythonCodeRuntime {
        fn language(&self) -> &'static str {
            "python"
        }

        fn isolation(&self) -> &'static str {
            "python-test"
        }

        async fn run(&self, _request: CodeRunRequest) -> anyhow::Result<CodeRunResult> {
            Ok(CodeRunResult::default())
        }
    }

    #[derive(Debug)]
    struct BridgeCodeRuntime;

    #[async_trait]
    impl CodeRuntimeBackend for BridgeCodeRuntime {
        fn language(&self) -> &'static str {
            "typescript"
        }

        fn isolation(&self) -> &'static str {
            "bridge-test"
        }

        async fn run(&self, request: CodeRunRequest) -> anyhow::Result<CodeRunResult> {
            let tools = request.bindings.first().expect("tools namespace");
            let echo = tools.functions.get("echo").expect("echo binding");
            let first = echo(json!({ "value": "one" })).await?;
            let second = echo(json!({ "value": "two" })).await?;
            Ok(CodeRunResult {
                logs: vec![format!("saw {}", first.as_str().expect("string result"))],
                value: Some(second),
                error: None,
            })
        }
    }

    #[derive(Debug)]
    struct FailingCodeRuntime;

    #[async_trait]
    impl CodeRuntimeBackend for FailingCodeRuntime {
        fn language(&self) -> &'static str {
            "typescript"
        }

        fn isolation(&self) -> &'static str {
            "failure-test"
        }

        async fn run(&self, _request: CodeRunRequest) -> anyhow::Result<CodeRunResult> {
            Ok(CodeRunResult {
                logs: vec!["before failure".to_owned()],
                value: None,
                error: Some(seekdeep_code_runtime::CodeRunFailure {
                    kind: CodeRunFailureKind::Exception,
                    message: "boom".to_owned(),
                }),
            })
        }
    }

    #[derive(Debug)]
    struct ConcurrentCodeRuntime;

    #[async_trait]
    impl CodeRuntimeBackend for ConcurrentCodeRuntime {
        fn language(&self) -> &'static str {
            "typescript"
        }

        fn isolation(&self) -> &'static str {
            "concurrency-test"
        }

        async fn run(&self, request: CodeRunRequest) -> anyhow::Result<CodeRunResult> {
            let probe = request.bindings[0]
                .functions
                .get("probe")
                .expect("probe binding");
            let (first, second) = tokio::join!(
                probe(json!({ "id": "first" })),
                probe(json!({ "id": "second" })),
            );
            Ok(CodeRunResult {
                logs: Vec::new(),
                value: Some(json!([first?, second?])),
                error: None,
            })
        }
    }

    fn provide_code_runtime(context: &Context) -> EffectHandle {
        let runtime = Arc::new(CodeRuntime::new(Arc::new(StubCodeRuntime)));
        runtime.provide(context).expect("provide code runtime")
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

    #[tokio::test]
    async fn production_constructor_feeds_live_schemas_to_prompt_assembly() {
        let root = Context::new();
        let prompt = SystemPrompt::new(
            &root,
            SystemPromptConfig {
                include_harness_identity: false,
                ..SystemPromptConfig::default()
            },
        )
        .expect("prompt");
        let runtime =
            ToolRuntime::new_with_system_prompt(&root, &prompt, ToolRuntimeConfig::default())
                .expect("runtime");
        let registration = runtime
            .register(&root, definition("live", json!("ok")))
            .expect("register");
        assert_eq!(
            prompt
                .assemble(AssembleContext::default())
                .await
                .expect("assembly")
                .tools
                .into_iter()
                .map(|schema| schema.name)
                .collect::<Vec<_>>(),
            ["live"]
        );
        registration.dispose().await.expect("unregister");
        assert!(
            prompt
                .assemble(AssembleContext::default())
                .await
                .expect("assembly")
                .tools
                .is_empty()
        );
    }

    #[tokio::test]
    async fn scoped_code_presentation_changes_wire_and_dispatch_surfaces_then_unwinds() {
        let root = Context::new();
        let prompt = SystemPrompt::new(
            &root,
            SystemPromptConfig {
                include_harness_identity: false,
                ..SystemPromptConfig::default()
            },
        )
        .expect("prompt");
        let runtime =
            ToolRuntime::new_with_system_prompt(&root, &prompt, ToolRuntimeConfig::default())
                .expect("runtime");
        runtime
            .register(&root, definition("echo", json!("ok")))
            .expect("register");
        let _code_runtime = provide_code_runtime(&root);
        let key = ScopeKey::new();
        let scope = create_scope(&root, key, None).expect("scope");
        let presentation = runtime
            .present_as(&scope.context, ToolPresentationMode::Code)
            .expect("present code");

        let coded = prompt
            .assemble(AssembleContext {
                scope: Some(key),
                ..AssembleContext::default()
            })
            .await
            .expect("coded assembly");
        assert_eq!(
            coded
                .tools
                .iter()
                .map(|schema| schema.name.as_str())
                .collect::<Vec<_>>(),
            [RUN_CODE_NAME]
        );
        assert!(
            coded
                .sections
                .iter()
                .find(|section| section.name == "tools:sdk")
                .is_some_and(|section| section.text.contains("echo"))
        );
        assert_eq!(
            runtime
                .get(RUN_CODE_NAME, Some(key))
                .expect("transport")
                .name,
            RUN_CODE_NAME
        );
        let ToolExecutionResult::Failure(denied) = runtime.execute(input("echo", Some(key))).await
        else {
            panic!("direct native tool must be collapsed")
        };
        assert_eq!(
            denied.error.info.expect("structured error").code,
            "UNKNOWN_TOOL"
        );

        presentation.dispose().await.expect("restore native");
        let restored = prompt
            .assemble(AssembleContext {
                scope: Some(key),
                ..AssembleContext::default()
            })
            .await
            .expect("restored assembly");
        assert_eq!(
            restored
                .tools
                .iter()
                .map(|schema| schema.name.as_str())
                .collect::<Vec<_>>(),
            ["echo"]
        );
        assert!(
            restored
                .sections
                .iter()
                .all(|section| section.name != "tools:sdk")
        );
        assert!(runtime.get(RUN_CODE_NAME, Some(key)).is_none());
    }

    #[tokio::test]
    async fn both_mode_and_scoped_native_override_match_prompt_contract() {
        let root = Context::new();
        let prompt = SystemPrompt::new(&root, SystemPromptConfig::default()).expect("prompt");
        let _code_runtime = provide_code_runtime(&root);
        let runtime = ToolRuntime::new_with_system_prompt(
            &root,
            &prompt,
            ToolRuntimeConfig {
                mode: ToolPresentationMode::Both,
                ..ToolRuntimeConfig::default()
            },
        )
        .expect("runtime");
        runtime
            .register(&root, definition("echo", json!("ok")))
            .expect("register");

        let global = prompt
            .assemble(AssembleContext::default())
            .await
            .expect("both assembly");
        assert_eq!(
            global
                .tools
                .iter()
                .map(|schema| schema.name.as_str())
                .collect::<Vec<_>>(),
            ["echo", RUN_CODE_NAME]
        );
        assert_eq!(
            global
                .sections
                .iter()
                .find(|section| section.name == "tools:code-only")
                .expect("registered collapse section")
                .text,
            ""
        );

        let key = ScopeKey::new();
        let scope = create_scope(&root, key, None).expect("scope");
        let _native = runtime
            .present_as(&scope.context, ToolPresentationMode::Native)
            .expect("opt out");
        let scoped = prompt
            .assemble(AssembleContext {
                scope: Some(key),
                ..AssembleContext::default()
            })
            .await
            .expect("native assembly");
        assert_eq!(
            scoped
                .tools
                .iter()
                .map(|schema| schema.name.as_str())
                .collect::<Vec<_>>(),
            ["echo"]
        );
        assert_eq!(
            scoped
                .sections
                .iter()
                .find(|section| section.name == "tools:sdk")
                .expect("global SDK registration remains")
                .text,
            ""
        );
    }

    #[tokio::test]
    async fn python_runtime_selects_matching_sdk_and_run_code_schema_flavor() {
        let root = Context::new();
        let prompt = SystemPrompt::new(&root, SystemPromptConfig::default()).expect("prompt");
        let code_runtime = Arc::new(CodeRuntime::new(Arc::new(PythonCodeRuntime)));
        code_runtime.provide(&root).expect("provide code runtime");
        let runtime = ToolRuntime::new_with_system_prompt(
            &root,
            &prompt,
            ToolRuntimeConfig {
                mode: ToolPresentationMode::Code,
                ..ToolRuntimeConfig::default()
            },
        )
        .expect("runtime");
        runtime
            .register(&root, definition("echo", json!("ok")))
            .expect("register echo");
        let assembly = prompt
            .assemble(AssembleContext::default())
            .await
            .expect("python assembly");
        let sdk = assembly
            .sections
            .iter()
            .find(|section| section.name == "tools:sdk")
            .expect("SDK section");
        assert!(sdk.text.contains("class Tools(Protocol):"));
        assert!(sdk.text.contains("```python"));
        let run_code = assembly
            .tools
            .iter()
            .find(|tool| tool.name == RUN_CODE_NAME)
            .expect("run_code schema");
        assert!(run_code.description.contains("Execute a Python program"));
        assert_eq!(
            run_code.parameters["properties"]["code"]["description"],
            json!("The program: the body of an async Python function.")
        );
        assert!(
            !run_code.parameters.contains_key("additionalProperties"),
            "source run_code arguments remain open to undeclared keys"
        );
    }

    #[tokio::test]
    async fn run_code_bridges_nested_tools_curates_output_and_logs_dispatches() {
        let root = Context::new();
        let prompt = SystemPrompt::new(&root, SystemPromptConfig::default()).expect("prompt");
        let code_runtime = Arc::new(CodeRuntime::new(Arc::new(BridgeCodeRuntime)));
        code_runtime.provide(&root).expect("provide code runtime");
        let runtime = ToolRuntime::new_with_system_prompt(
            &root,
            &prompt,
            ToolRuntimeConfig {
                mode: ToolPresentationMode::Code,
                ..ToolRuntimeConfig::default()
            },
        )
        .expect("runtime");
        let mut echo = definition("echo", Value::Null);
        echo.execute = Arc::new(|arguments, _| {
            Box::pin(async move {
                Ok(Value::String(format!(
                    "echo:{}",
                    arguments
                        .get("value")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                )))
            })
        });
        runtime.register(&root, echo).expect("register echo");
        let scope = ScopeKey::new();
        let session = Session::create(&SessionId::new("session"), None, None).expect("session");
        let result = runtime
            .execute(ToolExecutionInput {
                call_id: CallId::new("call-1"),
                root_call_id: None,
                name: RUN_CODE_NAME.to_owned(),
                arguments: json!({
                    "code": "const result = await tools.echo({ value: 'one' })",
                    "description": "Run the test program",
                }),
                agent: None,
                agent_scope: Some(scope),
                agent_session: Some(session.clone()),
                parent: None,
                signal: AbortSignal::default(),
            })
            .await;
        let ToolExecutionResult::Success(success) = result else {
            panic!("run_code should succeed")
        };
        assert_eq!(
            success.value,
            json!({ "logs": ["saw echo:one"], "result": "echo:two" })
        );
        assert_eq!(
            success.content,
            [ContentBlock::Text {
                text: "saw echo:one\necho:two".to_owned(),
            }]
        );
        let events = session.events();
        assert_eq!(
            events
                .iter()
                .map(|event| event.event_type.as_str())
                .collect::<Vec<_>>(),
            [
                "tool/code-dispatch-start",
                "tool/code-dispatch",
                "tool/code-dispatch-start",
                "tool/code-dispatch",
            ]
        );
        assert_eq!(events[1].data["subCallId"], json!("call-1:code:1"));
        assert_eq!(events[1].data["arguments"], json!({ "value": "one" }));
        assert_eq!(events[3].data["subCallId"], json!("call-1:code:2"));
        assert_eq!(
            events[3].data["content"],
            json!([{ "type": "text", "text": "echo:two" }])
        );
    }

    #[tokio::test]
    async fn run_code_failure_is_structured_and_includes_captured_output() {
        let root = Context::new();
        let prompt = SystemPrompt::new(&root, SystemPromptConfig::default()).expect("prompt");
        let code_runtime = Arc::new(CodeRuntime::new(Arc::new(FailingCodeRuntime)));
        code_runtime.provide(&root).expect("provide code runtime");
        let runtime = ToolRuntime::new_with_system_prompt(
            &root,
            &prompt,
            ToolRuntimeConfig {
                mode: ToolPresentationMode::Code,
                ..ToolRuntimeConfig::default()
            },
        )
        .expect("runtime");
        let result = runtime
            .execute(ToolExecutionInput {
                call_id: CallId::new("call-1"),
                root_call_id: None,
                name: RUN_CODE_NAME.to_owned(),
                arguments: json!({ "code": "throw new Error()", "description": "Fail" }),
                agent: None,
                agent_scope: None,
                agent_session: None,
                parent: None,
                signal: AbortSignal::default(),
            })
            .await;
        let ToolExecutionResult::Failure(failure) = result else {
            panic!("run_code should fail")
        };
        assert_eq!(
            failure.error.info,
            Some(ToolErrorInfo {
                name: "CodeRunFailedError".to_owned(),
                code: "CODE_RUN_FAILED".to_owned(),
            })
        );
        assert_eq!(
            failure.error.message,
            "code run failed (exception): boom\nCaptured output:\nbefore failure"
        );
    }

    #[tokio::test]
    async fn run_code_scheduler_serializes_exclusive_calls_in_submission_order() {
        let root = Context::new();
        let prompt = SystemPrompt::new(&root, SystemPromptConfig::default()).expect("prompt");
        let code_runtime = Arc::new(CodeRuntime::new(Arc::new(ConcurrentCodeRuntime)));
        code_runtime.provide(&root).expect("provide code runtime");
        let runtime = ToolRuntime::new_with_system_prompt(
            &root,
            &prompt,
            ToolRuntimeConfig {
                mode: ToolPresentationMode::Code,
                ..ToolRuntimeConfig::default()
            },
        )
        .expect("runtime");
        let active = Arc::new(AtomicUsize::new(0));
        let intervals = Arc::new(Mutex::new(Vec::new()));
        let body_active = active.clone();
        let body_intervals = intervals.clone();
        let mut probe = definition("probe", Value::Null);
        probe.execute = Arc::new(move |arguments, _| {
            let active = body_active.clone();
            let intervals = body_intervals.clone();
            Box::pin(async move {
                let id = arguments["id"].as_str().unwrap_or_default().to_owned();
                assert_eq!(active.fetch_add(1, Ordering::AcqRel), 0);
                intervals.lock().push(format!("enter:{id}"));
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                intervals.lock().push(format!("exit:{id}"));
                active.fetch_sub(1, Ordering::AcqRel);
                Ok(Value::String(id))
            })
        });
        runtime.register(&root, probe).expect("register probe");
        let result = runtime
            .execute(ToolExecutionInput {
                call_id: CallId::new("call-1"),
                root_call_id: None,
                name: RUN_CODE_NAME.to_owned(),
                arguments: json!({ "code": "Promise.all([])", "description": "Probe" }),
                agent: None,
                agent_scope: None,
                agent_session: None,
                parent: None,
                signal: AbortSignal::default(),
            })
            .await;
        assert!(!result.is_error());
        assert_eq!(
            *intervals.lock(),
            ["enter:first", "exit:first", "enter:second", "exit:second"]
        );
    }

    #[tokio::test]
    async fn run_code_scheduler_overlaps_safe_calls_and_honors_parallel_cap() {
        async fn exercise(max_parallel: usize) -> usize {
            let root = Context::new();
            let prompt = SystemPrompt::new(&root, SystemPromptConfig::default()).expect("prompt");
            let code_runtime = Arc::new(CodeRuntime::new(Arc::new(ConcurrentCodeRuntime)));
            code_runtime.provide(&root).expect("provide code runtime");
            let runtime = ToolRuntime::new_with_system_prompt(
                &root,
                &prompt,
                ToolRuntimeConfig {
                    mode: ToolPresentationMode::Code,
                    max_parallel_sub_calls: max_parallel,
                },
            )
            .expect("runtime");
            let active = Arc::new(AtomicUsize::new(0));
            let maximum = Arc::new(AtomicUsize::new(0));
            let body_active = active.clone();
            let body_maximum = maximum.clone();
            let mut probe = definition("probe", Value::Null);
            probe.is_concurrency_safe = Some(Arc::new(|_| true));
            probe.execute = Arc::new(move |arguments, _| {
                let active = body_active.clone();
                let maximum = body_maximum.clone();
                Box::pin(async move {
                    let count = active.fetch_add(1, Ordering::AcqRel) + 1;
                    maximum.fetch_max(count, Ordering::AcqRel);
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    active.fetch_sub(1, Ordering::AcqRel);
                    Ok(arguments["id"].clone())
                })
            });
            runtime.register(&root, probe).expect("register probe");
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                runtime.execute(ToolExecutionInput {
                    call_id: CallId::new("call-1"),
                    root_call_id: None,
                    name: RUN_CODE_NAME.to_owned(),
                    arguments: json!({ "code": "Promise.all([])", "description": "Probe" }),
                    agent: None,
                    agent_scope: None,
                    agent_session: None,
                    parent: None,
                    signal: AbortSignal::default(),
                }),
            )
            .await
            .expect("scheduler quiescence");
            assert!(!result.is_error());
            maximum.load(Ordering::Acquire)
        }

        assert_eq!(exercise(10).await, 2);
        assert_eq!(exercise(1).await, 1);
    }

    #[tokio::test]
    async fn run_code_executes_end_to_end_through_the_rust_typescript_worker() {
        let root = Context::new();
        let prompt = SystemPrompt::new(&root, SystemPromptConfig::default()).expect("prompt");
        let _worker = install_worker_runtime(
            &root,
            &WorkerThreadCodeRuntimeConfig {
                compute_ms: Some(5_000.0),
                max_wall_ms: Some(5_000.0),
                max_output_bytes: Some(1_000_000.0),
                max_old_generation_size_mb: Some(64.0),
            },
        )
        .expect("worker runtime");
        let runtime = ToolRuntime::new_with_system_prompt(
            &root,
            &prompt,
            ToolRuntimeConfig {
                mode: ToolPresentationMode::Code,
                ..ToolRuntimeConfig::default()
            },
        )
        .expect("tools runtime");
        let mut echo = definition("echo", Value::Null);
        echo.execute = Arc::new(|arguments, _| {
            Box::pin(async move {
                Ok(json!(format!(
                    "echo:{}",
                    arguments["value"].as_str().unwrap_or_default()
                )))
            })
        });
        runtime.register(&root, echo).expect("register echo");

        let result = runtime
            .execute(ToolExecutionInput {
                call_id: CallId::new("real-worker"),
                root_call_id: None,
                name: RUN_CODE_NAME.to_owned(),
                arguments: json!({
                    "code": "const first = await tools.echo({ value: 'one' }); console.log('saw', first); return await tools.echo({ value: 'two' });",
                    "description": "Exercise the real worker",
                }),
                agent: None,
                agent_scope: None,
                agent_session: None,
                parent: None,
                signal: AbortSignal::default(),
            })
            .await;
        let ToolExecutionResult::Success(success) = result else {
            panic!("real worker run_code failed: {result:?}")
        };
        assert_eq!(
            success.value,
            json!({"logs": ["saw echo:one"], "result": "echo:two"})
        );
    }

    #[tokio::test]
    async fn typed_waterfalls_wrap_in_source_order_and_observers_are_contained() {
        let root = Context::new();
        let runtime =
            ToolRuntime::new(root.clone(), ToolRuntimeConfig::default()).expect("runtime");
        let events = Arc::new(Mutex::new(Vec::new()));
        let body_events = events.clone();
        let mut tool = definition("pipeline", json!("body"));
        tool.execute = Arc::new(move |_, _| {
            let body_events = body_events.clone();
            Box::pin(async move {
                body_events.lock().push("body");
                Ok(json!("body"))
            })
        });
        runtime.register(&root, tool).expect("register");

        let pre_events = events.clone();
        runtime
            .on_pre_execute(
                &root,
                move |_, next| {
                    let pre_events = pre_events.clone();
                    async move {
                        pre_events.lock().push("pre:before");
                        let decision = next.run().await?;
                        pre_events.lock().push("pre:after");
                        Ok(decision)
                    }
                },
                EventOptions::default(),
            )
            .expect("pre");
        let around_events = events.clone();
        runtime
            .on_execute(
                &root,
                move |_, next| {
                    let around_events = around_events.clone();
                    async move {
                        around_events.lock().push("around:before");
                        let result = next.run().await?;
                        around_events.lock().push("around:after");
                        Ok(result)
                    }
                },
                EventOptions::default(),
            )
            .expect("around");
        let post_events = events.clone();
        runtime
            .on_post_execute(
                &root,
                move |_, _, next| {
                    let post_events = post_events.clone();
                    async move {
                        post_events.lock().push("post:before");
                        let decision = next.run().await?;
                        post_events.lock().push("post:after");
                        Ok(decision)
                    }
                },
                EventOptions::default(),
            )
            .expect("post");
        let result_events = events.clone();
        runtime
            .on_result(
                &root,
                move |_, _| {
                    result_events.lock().push("result");
                    anyhow::bail!("contained observer failure")
                },
                EventOptions::default(),
            )
            .expect("result observer");

        assert!(!runtime.execute(input("pipeline", None)).await.is_error());
        assert_eq!(
            *events.lock(),
            [
                "pre:before",
                "pre:after",
                "around:before",
                "body",
                "around:after",
                "post:before",
                "post:after",
                "result",
            ]
        );
    }

    #[tokio::test]
    async fn scoped_pre_policy_and_guards_route_only_to_their_scope() {
        let root = Context::new();
        let runtime =
            ToolRuntime::new(root.clone(), ToolRuntimeConfig::default()).expect("runtime");
        runtime
            .register(&root, definition("echo", json!("ok")))
            .expect("register");
        let key = ScopeKey::new();
        let other_key = ScopeKey::new();
        let scope = create_scope(&root, key, None).expect("scope");
        let other = create_scope(&root, other_key, None).expect("other");
        runtime
            .on_pre_execute(
                &scope.context,
                |_, _| async {
                    Ok(PreToolDecision::Deny {
                        reason: "scoped pre denial".to_owned(),
                    })
                },
                EventOptions::default(),
            )
            .expect("pre");
        runtime
            .guard(
                &other.context,
                Arc::new(|_| Some("other guard denial".to_owned())),
            )
            .expect("guard");

        let ToolExecutionResult::Failure(denied) = runtime.execute(input("echo", Some(key))).await
        else {
            panic!("scope denied")
        };
        assert_eq!(denied.error.message, "scoped pre denial");
        let ToolExecutionResult::Failure(other_denied) =
            runtime.execute(input("echo", Some(other_key))).await
        else {
            panic!("other denied")
        };
        assert_eq!(other_denied.error.message, "other guard denial");
        assert!(!runtime.execute(input("echo", None)).await.is_error());
        scope.dispose().await.expect("scope dispose");
        other.dispose().await.expect("other dispose");
    }
}
