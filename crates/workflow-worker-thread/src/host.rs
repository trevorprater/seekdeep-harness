//! Host side of one workflow run: the `WorkflowRun` handle that owns the
//! execution thread, the child bridge, and the result settlement.

use std::sync::Arc;

use futures::future::BoxFuture;
use parking_lot::Mutex;
use seekdeep_agent::{Agent, AgentOptions};
use seekdeep_cordis::{Context, EventArgs};
use seekdeep_llm::{AbortSignal, ContentBlock, ModelId, ProviderId};
use seekdeep_subagent::{SubagentRun, SubagentRuntime, SubagentStartRequest};
use seekdeep_workflow::{
    WorkflowAgentEndInfo, WorkflowAgentInfo, WorkflowEventName, WorkflowMeta, WorkflowResult,
    WorkflowRun, WorkflowRunId, WorkflowRunInfo, emit_workflow_event,
};
use tokio::sync::Notify;

use crate::{
    runtime::{ExecutionObserver, WorkflowExecution},
    types::{ChildHandle, ChildPort, ChildResult, ChildStartRequest, WorkerLimits},
};

/// The worker-side child handle: the RPC mirror of the subagent run.
struct HostChildHandle {
    run: Arc<dyn SubagentRun>,
}

impl ChildHandle for HostChildHandle {
    fn id(&self) -> &str {
        self.run.id().as_str()
    }

    fn result(&self) -> BoxFuture<'static, anyhow::Result<ChildResult>> {
        let run = Arc::clone(&self.run);
        Box::pin(async move {
            let result = run.result().await;
            Ok(ChildResult {
                output: result.output,
                structured: result.structured,
                stop_reason: result.stop_reason.as_str().to_owned(),
            })
        })
    }

    fn dispose(&self) -> BoxFuture<'static, anyhow::Result<()>> {
        self.run.dispose()
    }
}

/// The host-side child port: starts children on the configured provider with
/// the run's shared abort signal.
struct HostChildPort {
    subagents: Arc<SubagentRuntime>,
    parent: Arc<Agent>,
    provider: String,
    signal: AbortSignal,
}

impl ChildPort for HostChildPort {
    fn start_agent(
        &self,
        request: ChildStartRequest,
    ) -> BoxFuture<'static, anyhow::Result<Arc<dyn ChildHandle>>> {
        let subagents = Arc::clone(&self.subagents);
        let parent = Arc::clone(&self.parent);
        let provider = self.provider.clone();
        let signal = self.signal.clone();
        Box::pin(async move {
            let agent_options = if request.provider.is_some() || request.model.is_some() {
                Some(AgentOptions {
                    provider: request.provider.map(ProviderId::new),
                    model: request.model.map(ModelId::new),
                    max_tokens: None,
                    subagent_depth: None,
                })
            } else {
                None
            };
            let run = subagents
                .start(
                    &provider,
                    SubagentStartRequest {
                        label: None,
                        prompt: vec![ContentBlock::Text {
                            text: request.prompt,
                        }],
                        parent,
                        signal,
                        agent_options,
                        output_schema: request.schema,
                        max_depth: None,
                        tool_filter: None,
                        persona: None,
                    },
                )
                .await?;
            Ok(Arc::new(HostChildHandle { run }) as Arc<dyn ChildHandle>)
        })
    }
}

/// The observer that forwards progress to workflow lifecycle events.
struct EventObserver {
    context: Context,
    info: WorkflowRunInfo,
}

impl ExecutionObserver for EventObserver {
    fn phase(&self, title: &str) {
        let _ = emit_workflow_event(
            &self.context,
            WorkflowEventName::Phase,
            &EventArgs::from_values(vec![
                Arc::new(self.info.clone()),
                Arc::new(title.to_owned()),
            ]),
        );
    }

    fn log(&self, message: &str) {
        let _ = emit_workflow_event(
            &self.context,
            WorkflowEventName::Log,
            &EventArgs::from_values(vec![
                Arc::new(self.info.clone()),
                Arc::new(message.to_owned()),
            ]),
        );
    }

    fn agent_start(&self, info: &WorkflowAgentInfo) {
        let _ = emit_workflow_event(
            &self.context,
            WorkflowEventName::AgentStart,
            &EventArgs::from_values(vec![Arc::new(self.info.clone()), Arc::new(info.clone())]),
        );
    }

    fn agent_end(&self, info: &WorkflowAgentEndInfo) {
        let _ = emit_workflow_event(
            &self.context,
            WorkflowEventName::AgentEnd,
            &EventArgs::from_values(vec![Arc::new(self.info.clone()), Arc::new(info.clone())]),
        );
    }
}

/// Shared result settlement state.
struct ResultState {
    value: Mutex<Option<WorkflowResult>>,
    notify: Notify,
}

/// One live worker-engine run.
pub struct WorkerRun {
    id: WorkflowRunId,
    meta: WorkflowMeta,
    execution: Arc<WorkflowExecution>,
    children_signal: AbortSignal,
    result: Arc<ResultState>,
}

impl WorkerRun {
    /// Constructs one run and starts its execution thread.
    ///
    /// # Panics
    ///
    /// Panics if the execution cannot be built.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        context: &Context,
        subagents: Arc<SubagentRuntime>,
        id: WorkflowRunId,
        meta: WorkflowMeta,
        parent: Arc<Agent>,
        body: String,
        args: Option<serde_json::Value>,
        limits: WorkerLimits,
        provider: String,
        info: WorkflowRunInfo,
    ) -> Arc<Self> {
        let children_signal = AbortSignal::default();
        let child_port = Arc::new(HostChildPort {
            subagents,
            parent,
            provider,
            signal: children_signal.clone(),
        });
        let observer: Arc<dyn ExecutionObserver> = Arc::new(EventObserver {
            context: context.clone(),
            info,
        });
        let execution = WorkflowExecution::new(&meta, body, args, limits, observer, child_port)
            .expect("workflow execution");
        let execution = Arc::new(execution);

        let result = Arc::new(ResultState {
            value: Mutex::new(None),
            notify: Notify::new(),
        });

        {
            let execution = Arc::clone(&execution);
            let result = Arc::clone(&result);
            tokio::spawn(async move {
                let outcome = execution.drive().await;
                *result.value.lock() = Some(outcome);
                result.notify.notify_waiters();
            });
        }

        Arc::new(Self {
            id,
            meta,
            execution,
            children_signal,
            result,
        })
    }
}

impl WorkflowRun for WorkerRun {
    fn id(&self) -> &WorkflowRunId {
        &self.id
    }

    fn meta(&self) -> &WorkflowMeta {
        &self.meta
    }

    fn result(&self) -> BoxFuture<'static, WorkflowResult> {
        let result = Arc::clone(&self.result);
        Box::pin(async move {
            loop {
                if let Some(outcome) = result.value.lock().clone() {
                    return outcome;
                }
                result.notify.notified().await;
            }
        })
    }

    fn cancel(&self, reason: Option<&str>) {
        let reason = reason.unwrap_or("workflow cancelled");
        self.execution.cancel(reason);
        self.children_signal
            .abort_with_reason(serde_json::Value::String(reason.to_owned()));
    }

    fn dispose(&self) -> BoxFuture<'static, ()> {
        let execution = Arc::clone(&self.execution);
        let children_signal = self.children_signal.clone();
        let result = Arc::clone(&self.result);
        Box::pin(async move {
            execution.cancel("workflow disposed");
            children_signal
                .abort_with_reason(serde_json::Value::String("workflow disposed".to_owned()));
            loop {
                if result.value.lock().is_some() {
                    return;
                }
                result.notify.notified().await;
            }
        })
    }
}
