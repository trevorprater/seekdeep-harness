//! Workflow capability seam: the engine contract, error taxonomy, and
//! lifecycle event vocabulary.

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
};

use seekdeep_cordis::{Context, EventArgs, Service, ServiceKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::runtime_types::{WorkflowRun, WorkflowStartRequest};

pub use crate::types::{
    WorkflowAgentEndInfo, WorkflowAgentInfo, WorkflowAgentOutcome, WorkflowMeta, WorkflowPhase,
    WorkflowResult, WorkflowResultInfo, WorkflowRunId, WorkflowRunInfo, WorkflowStopReason,
};

/// The full set of `workflow/*` event names the engine dispatches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkflowEventName {
    /// A run started.
    Start,
    /// The script entered a phase.
    Phase,
    /// The script emitted a narration line.
    Log,
    /// One `agent()` call established a published child run.
    AgentStart,
    /// One `agent()` call settled.
    AgentEnd,
    /// A run settled.
    End,
}

impl WorkflowEventName {
    /// Exact wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Start => "workflow/start",
            Self::Phase => "workflow/phase",
            Self::Log => "workflow/log",
            Self::AgentStart => "workflow/agent-start",
            Self::AgentEnd => "workflow/agent-end",
            Self::End => "workflow/end",
        }
    }
}

/// Machine-routable fatal workflow failure codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WorkflowErrorCode {
    /// Script parse failure.
    ScriptParse,
    /// Meta validation failure.
    MetaInvalid,
    /// Invalid argument.
    InvalidArgument,
    /// Unsupported option.
    UnsupportedOption,
    /// Unsupported schema.
    UnsupportedSchema,
    /// Child-agent cap tripped.
    AgentCap,
    /// Item cap tripped.
    ItemCap,
    /// Child-agent start failure.
    AgentStart,
    /// Child-agent result failure.
    AgentResult,
    /// Result not serializable.
    ResultUnserializable,
    /// Cancelled.
    Cancelled,
}

/// Typed error for workflow-seam failures.
#[derive(Debug, Error)]
#[error("{message}")]
pub struct WorkflowError {
    /// Machine-routable taxonomy.
    pub code: WorkflowErrorCode,
    /// Human-readable failure.
    pub message: String,
    /// Whether combinators must propagate instead of nulling the item.
    pub fatal: bool,
    #[source]
    cause: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl WorkflowError {
    /// Creates one fatal workflow failure.
    #[must_use]
    pub fn new(message: impl Into<String>, code: WorkflowErrorCode) -> Self {
        Self {
            code,
            message: message.into(),
            fatal: true,
            cause: None,
        }
    }

    /// Creates a workflow failure with an explicit fatality flag.
    #[must_use]
    pub fn with_fatal(message: impl Into<String>, code: WorkflowErrorCode, fatal: bool) -> Self {
        Self {
            code,
            message: message.into(),
            fatal,
            cause: None,
        }
    }

    /// Chains the host failure that caused this workflow error.
    #[must_use]
    pub fn with_cause(mut self, cause: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.cause = Some(Box::new(cause));
        self
    }

    /// JavaScript error-class name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        "WorkflowError"
    }
}

/// Whether combinators must re-throw an error instead of mapping the item to null.
#[must_use]
pub fn is_fatal_workflow_error(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<WorkflowError>()
        .is_some_and(|error| error.fatal)
}

/// Typed Cordis slot for the workflow engine.
pub const WORKFLOW_ENGINE: ServiceKey<WorkflowEngineService> = ServiceKey::new("workflowEngine");

/// Wrapper publishing a concrete workflow engine on the seam slot.
#[derive(Clone)]
pub struct WorkflowEngineService(Arc<dyn WorkflowEngine>);

impl WorkflowEngineService {
    /// Wraps one concrete engine.
    #[must_use]
    pub fn new(engine: Arc<dyn WorkflowEngine>) -> Arc<Self> {
        Arc::new(Self(engine))
    }

    /// Returns the wrapped object-safe engine.
    #[must_use]
    pub fn engine(&self) -> Arc<dyn WorkflowEngine> {
        self.0.clone()
    }

    /// Publishes this engine on the seam slot.
    ///
    /// # Errors
    ///
    /// Returns duplicate-service or inactive-owner failures.
    pub fn provide(
        self: &Arc<Self>,
        context: &Context,
    ) -> Result<seekdeep_cordis::fiber::EffectHandle, seekdeep_cordis::CordisError> {
        context.provide(WORKFLOW_ENGINE, self.clone())
    }
}

impl std::ops::Deref for WorkflowEngineService {
    type Target = dyn WorkflowEngine;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}

/// Workflow engine contract. Invalid requests throw before publication; a live
/// run is holder-owned, its result never rejects.
pub trait WorkflowEngine: Service + Send + Sync {
    /// Parses and executes a workflow script.
    ///
    /// # Errors
    ///
    /// Returns request-validation or synchronous engine-start failures before
    /// any live run is published.
    fn start(&self, request: WorkflowStartRequest) -> anyhow::Result<Arc<dyn WorkflowRun>>;
}

/// Emits a workflow lifecycle event while containing each listener failure.
///
/// # Errors
///
/// Returns a dispatch-interception failure; listener failures are contained.
pub fn emit_workflow_event(
    context: &Context,
    name: WorkflowEventName,
    args: &EventArgs,
) -> anyhow::Result<()> {
    let emission = context
        .events()
        .prepare_emit(context, name.as_str(), args)?;
    let event = name.as_str();
    let async_error: Arc<dyn Fn(anyhow::Error) + Send + Sync> = Arc::new(move |error| {
        let error = render_listener_error(&error);
        tracing::warn!(event, %error, "workflow listener rejected");
    });
    emission.emit_contained_with_async_errors(
        |error| {
            let error = render_listener_error(&error);
            tracing::warn!(event, %error, "workflow listener threw");
        },
        &async_error,
    );
    Ok(())
}

fn render_listener_error(error: &anyhow::Error) -> String {
    catch_unwind(AssertUnwindSafe(|| error.to_string()))
        .unwrap_or_else(|_| "[unrenderable listener error]".to_owned())
}
