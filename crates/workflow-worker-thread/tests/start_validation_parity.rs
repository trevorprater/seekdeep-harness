//! Synchronous `WorkflowEngine::start` rejection parity.

use std::sync::Arc;

use async_trait::async_trait;
use seekdeep_agent::{Agent, AgentOptions, Inbox, NoopInboxNotifications};
use seekdeep_cordis::Context;
use seekdeep_core::session::{Session, SessionId};
use seekdeep_scope::ScopeKey;
use seekdeep_subagent::{
    ResolvedSubagentStartRequest, SubagentCapabilities, SubagentProvider, SubagentRun,
    SubagentRuntime,
};
use seekdeep_workflow::{
    WorkflowEngine, WorkflowError, WorkflowErrorCode, WorkflowMeta, WorkflowStartRequest,
    WorkflowStopReason,
};
use seekdeep_workflow_worker_thread::{Config, WorkerThreadWorkflowEngine};

struct Provider {
    capabilities: SubagentCapabilities,
}

#[async_trait]
impl SubagentProvider for Provider {
    fn name(&self) -> &'static str {
        "spawn"
    }

    fn capabilities(&self) -> &SubagentCapabilities {
        &self.capabilities
    }

    fn inherits_parent_context(&self) -> bool {
        false
    }

    async fn start(
        &self,
        _request: ResolvedSubagentStartRequest,
    ) -> anyhow::Result<Arc<dyn SubagentRun>> {
        anyhow::bail!("provider start is not reached by validation tests")
    }
}

fn parent() -> Arc<Agent> {
    let id = SessionId::new("workflow-validation-parent");
    let session = Session::create(&id, None, None).expect("session");
    let inbox =
        Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).expect("inbox"));
    Arc::new(Agent::new(
        id,
        AgentOptions::default(),
        session,
        inbox,
        Context::new(),
        ScopeKey::new(),
    ))
}

fn request() -> WorkflowStartRequest {
    WorkflowStartRequest {
        script: "return { ok: true }".to_owned(),
        meta: WorkflowMeta {
            name: "validation".to_owned(),
            description: "Validate synchronous start failures.".to_owned(),
            when_to_use: None,
            phases: None,
        },
        args: None,
        subagent_provider: None,
        max_total_agents: None,
        parent: parent(),
        signal: None,
    }
}

fn code(error: &anyhow::Error) -> Option<WorkflowErrorCode> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<WorkflowError>())
        .map(|error| error.code)
}

fn rejected(
    engine: &WorkerThreadWorkflowEngine,
    request: WorkflowStartRequest,
    message: &str,
) -> anyhow::Error {
    engine
        .start(request)
        .err()
        .unwrap_or_else(|| panic!("{message}"))
}

#[test]
fn rejects_every_prepublication_validation_failure_without_panicking_or_returning_a_run() {
    let context = Context::new();
    let subagents = SubagentRuntime::install(&context).expect("subagents");
    subagents
        .register_provider(Arc::new(Provider {
            capabilities: SubagentCapabilities::default(),
        }))
        .expect("provider");
    let engine = WorkerThreadWorkflowEngine::new(&context, Config::default()).expect("engine");

    let mut invalid_meta = request();
    invalid_meta.meta.name.clear();
    let error = rejected(&engine, invalid_meta, "invalid meta returned a run");
    assert_eq!(code(&error), Some(WorkflowErrorCode::MetaInvalid));
    assert!(error.to_string().contains("meta.name"));

    let mut meta_statement = request();
    meta_statement.script = "export const meta = {}; return null".to_owned();
    let error = rejected(&engine, meta_statement, "meta statement returned a run");
    assert_eq!(code(&error), Some(WorkflowErrorCode::ScriptParse));
    assert!(error.to_string().contains("remove the export const meta"));

    let mut syntax = request();
    syntax.script = "return (".to_owned();
    let error = rejected(&engine, syntax, "invalid script returned a run");
    assert_eq!(code(&error), Some(WorkflowErrorCode::ScriptParse));
    assert!(error.to_string().contains("workflow script does not parse"));

    let mut empty_provider = request();
    empty_provider.subagent_provider = Some(" ".to_owned());
    let error = rejected(&engine, empty_provider, "empty provider returned a run");
    assert_eq!(code(&error), Some(WorkflowErrorCode::InvalidArgument));
    assert!(error.to_string().contains("non-empty normalized"));

    let mut missing_provider = request();
    missing_provider.subagent_provider = Some("missing".to_owned());
    let error = rejected(&engine, missing_provider, "missing provider returned a run");
    assert_eq!(code(&error), Some(WorkflowErrorCode::AgentStart));
    assert!(
        error
            .to_string()
            .contains("no subagent provider registered")
    );

    let mut zero_cap = request();
    zero_cap.max_total_agents = Some(0);
    let error = rejected(&engine, zero_cap, "zero cap returned a run");
    assert_eq!(code(&error), Some(WorkflowErrorCode::InvalidArgument));
    assert!(error.to_string().contains("positive safe integer"));

    let mut high_cap = request();
    high_cap.max_total_agents = Some(1001);
    let error = rejected(&engine, high_cap, "over-ceiling cap returned a run");
    assert_eq!(code(&error), Some(WorkflowErrorCode::InvalidArgument));
    assert!(error.to_string().contains("exceeds the engine ceiling"));
}

#[test]
fn rejects_invalid_direct_config_before_resolving_injected_services() {
    for (config, fragment) in [
        (
            Config {
                max_total_agents: 0,
                ..Config::default()
            },
            "maxTotalAgents must be a positive safe integer",
        ),
        (
            Config {
                max_items_per_call: 0,
                ..Config::default()
            },
            "maxItemsPerCall must be a positive safe integer",
        ),
        (
            Config {
                sync_timeout_ms: 0,
                ..Config::default()
            },
            "syncTimeoutMs must be a positive safe integer",
        ),
        (
            Config {
                max_concurrent_agents: 9_007_199_254_740_992,
                ..Config::default()
            },
            "maxConcurrentAgents must be a non-negative safe integer",
        ),
        (
            Config {
                dispose_grace_ms: 9_007_199_254_740_992,
                ..Config::default()
            },
            "disposeGraceMs must be a non-negative safe integer",
        ),
    ] {
        let error = WorkerThreadWorkflowEngine::new(&Context::new(), config)
            .err()
            .expect("invalid config");
        assert!(
            error.to_string().contains(fragment),
            "expected {fragment:?} in {error:?}"
        );
    }
}

#[tokio::test]
async fn compiled_default_engine_runs_a_keyless_source_compatibility_script() {
    let context = Context::new();
    let subagents = SubagentRuntime::install(&context).expect("subagents");
    subagents
        .register_provider(Arc::new(Provider {
            capabilities: SubagentCapabilities {
                output_schema: true,
                depth_limit: true,
                tool_filter: true,
                persona: true,
            },
        }))
        .expect("provider");
    let engine = WorkerThreadWorkflowEngine::new(&context, Config::default()).expect("engine");
    let mut request = request();
    request.script = "return 6 * 7".to_owned();
    request.meta = WorkflowMeta {
        name: "source-worker-compat".to_owned(),
        description: "exercise the compiled worker runtime".to_owned(),
        when_to_use: None,
        phases: None,
    };
    let run = engine.start(request).expect("start");
    let result = run.result().await;
    assert_eq!(result.value, serde_json::json!(42));
    assert_eq!(result.stop_reason, WorkflowStopReason::Completed);
    assert_eq!(result.agents_started, 0);
    run.dispose().await;
}
