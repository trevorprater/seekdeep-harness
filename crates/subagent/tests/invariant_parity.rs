//! Provider and run lifecycle invariant parity.

use std::sync::Arc;

use async_trait::async_trait;
use seekdeep_cordis::{Context, EventArgs};
use seekdeep_core::session::SessionId;
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use seekdeep_subagent::{
    ResolvedSubagentStartRequest, SubagentCapabilities, SubagentProvider, SubagentRun,
    SubagentRunEndInfo, SubagentRunId, SubagentRunInfo, SubagentRuntime, SubagentStopReason,
    invariant::register_invariant,
};

#[derive(Debug)]
struct Provider {
    name: String,
    capabilities: SubagentCapabilities,
}

impl Provider {
    fn new(name: &str) -> Arc<Self> {
        Arc::new(Self {
            name: name.to_owned(),
            capabilities: SubagentCapabilities::default(),
        })
    }
}

#[async_trait]
impl SubagentProvider for Provider {
    fn name(&self) -> &str {
        &self.name
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
        anyhow::bail!("invariant fixture provider is never started")
    }
}

async fn setup() -> Context {
    let context = Context::new();
    SubagentRuntime::install(&context).unwrap();
    let registry = InvariantRegistry::install(&context, &InvariantConfig::default()).unwrap();
    register_invariant(&registry)
        .unwrap()
        .await_ready()
        .await
        .unwrap();
    context
}

fn start_with(provider: &str, run: &str, child: &str) -> SubagentRunInfo {
    SubagentRunInfo {
        run_id: SubagentRunId::new(run),
        provider: provider.to_owned(),
        id: SessionId::new(child),
        local: false,
    }
}

fn end_with(provider: &str, run: &str, child: &str) -> SubagentRunEndInfo {
    SubagentRunEndInfo {
        run_id: SubagentRunId::new(run),
        provider: provider.to_owned(),
        id: SessionId::new(child),
        local: false,
        stop_reason: SubagentStopReason::Completed,
        last_assistant_message: None,
    }
}

#[allow(clippy::needless_pass_by_value)]
fn emit(context: &Context, name: &str, args: EventArgs) -> anyhow::Result<()> {
    context.events().emit(context, name, &args)
}

#[tokio::test]
async fn accepts_provider_and_run_lifecycle_pairs_and_unrelated_events() {
    let context = setup().await;
    let provider: Arc<dyn SubagentProvider> = Provider::new("mock");
    emit(
        &context,
        "subagent/provider-added",
        EventArgs::one(provider),
    )
    .unwrap();
    emit(
        &context,
        "subagent/start",
        EventArgs::one(start_with("mock", "run-1", "child-1")),
    )
    .unwrap();
    emit(
        &context,
        "subagent/end",
        EventArgs::one(end_with("mock", "run-1", "child-1")),
    )
    .unwrap();
    emit(
        &context,
        "subagent/provider-removed",
        EventArgs::one("mock".to_owned()),
    )
    .unwrap();
    emit(&context, "tools/change", EventArgs::new()).unwrap();
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn rejects_empty_repeated_and_unknown_provider_transitions() {
    let context = setup().await;
    let empty: Arc<dyn SubagentProvider> = Provider::new("");
    assert!(
        emit(&context, "subagent/provider-added", EventArgs::one(empty),)
            .unwrap_err()
            .to_string()
            .contains("names must be non-empty")
    );
    let provider: Arc<dyn SubagentProvider> = Provider::new("mock");
    emit(
        &context,
        "subagent/provider-added",
        EventArgs::one(Arc::clone(&provider)),
    )
    .unwrap();
    assert!(
        emit(
            &context,
            "subagent/provider-added",
            EventArgs::one(provider),
        )
        .unwrap_err()
        .to_string()
        .contains("repeated \"mock\"")
    );
    assert!(
        emit(
            &context,
            "subagent/provider-removed",
            EventArgs::one("missing".to_owned()),
        )
        .unwrap_err()
        .to_string()
        .contains("unknown provider")
    );
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn rejects_malformed_duplicate_unpaired_and_divergent_run_transitions() {
    let context = setup().await;
    for malformed in [
        start_with("", "run-1", "child-1"),
        start_with("mock", "", "child-1"),
        start_with("mock", "run-1", ""),
    ] {
        assert!(
            emit(&context, "subagent/start", EventArgs::one(malformed))
                .unwrap_err()
                .to_string()
                .contains("provider, runId, and child id must be non-empty")
        );
    }
    emit(
        &context,
        "subagent/start",
        EventArgs::one(start_with("mock", "run-1", "child-1")),
    )
    .unwrap();
    assert!(
        emit(
            &context,
            "subagent/start",
            EventArgs::one(start_with("mock", "run-1", "child-1")),
        )
        .unwrap_err()
        .to_string()
        .contains("repeated run id")
    );
    assert!(
        emit(
            &context,
            "subagent/end",
            EventArgs::one(end_with("mock", "missing", "child-1")),
        )
        .unwrap_err()
        .to_string()
        .contains("no matching subagent/start")
    );
    assert!(
        emit(
            &context,
            "subagent/end",
            EventArgs::one(end_with("mock", "run-1", "other")),
        )
        .unwrap_err()
        .to_string()
        .contains("identity diverges")
    );
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn accepts_recorded_provider_identity_after_registration_ends() {
    let context = setup().await;
    let provider: Arc<dyn SubagentProvider> = Provider::new("historical");
    emit(
        &context,
        "subagent/provider-added",
        EventArgs::one(provider),
    )
    .unwrap();
    emit(
        &context,
        "subagent/provider-removed",
        EventArgs::one("historical".to_owned()),
    )
    .unwrap();
    emit(
        &context,
        "subagent/start",
        EventArgs::one(start_with("historical", "run-1", "child-1")),
    )
    .unwrap();
    emit(
        &context,
        "subagent/end",
        EventArgs::one(end_with("historical", "run-1", "child-1")),
    )
    .unwrap();
    context.fiber().dispose().await.unwrap();
}
