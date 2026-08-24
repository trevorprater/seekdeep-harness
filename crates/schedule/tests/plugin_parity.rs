//! Loader shape and future-root lifecycle mirror of the source plugin suite.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use seekdeep_agent::{AgentEvents, AgentOptions, CreateAgentOptions};
use seekdeep_agent_loop::{
    AgentLoop, AgentLoopServices, AgentStatusChanged, DEFAULT_MAX_PARALLEL_TOOL_CALLS,
};
use seekdeep_agent_loop_testkit::{
    AgentLoopTestDependenciesOptions, mount_agent_loop_test_dependencies,
};
use seekdeep_cordis::{Context, EventOptions, EventReply};
use seekdeep_core::session::SessionId;
use seekdeep_llm::{AbortSignal, CallId};
use seekdeep_schedule::{INJECT, PACKAGE_NAME, plugin};
use seekdeep_tools::ToolExecutionInput;
use serde_json::{Value, json};

struct PluginHarness {
    context: Context,
    dependencies: seekdeep_agent_loop_testkit::AgentLoopTestDependencies,
    agent_loop: AgentLoop,
    factory: seekdeep_agent::AgentFactoryRegistration,
    flushes: Arc<AtomicUsize>,
}

impl PluginHarness {
    fn new() -> Self {
        let context = Context::new();
        let dependencies = mount_agent_loop_test_dependencies(
            &context,
            AgentLoopTestDependenciesOptions::default(),
        )
        .unwrap();
        context
            .provide_named("sessionPersistence", Arc::new(()))
            .unwrap();
        let flushes = Arc::new(AtomicUsize::new(0));
        let observed = flushes.clone();
        context
            .events()
            .on_sync(
                &context,
                "session/flush",
                move |_, _| {
                    observed.fetch_add(1, Ordering::AcqRel);
                    Ok(EventReply::Undefined)
                },
                EventOptions::default(),
            )
            .unwrap();
        let agent_loop = AgentLoop::new(
            context.clone(),
            dependencies.sessions.clone(),
            (*dependencies.agents).clone(),
            AgentLoopServices {
                llm: dependencies.llm.clone(),
                system_prompt: dependencies.system_prompt.clone(),
                tools: dependencies.tools.clone(),
                max_parallel_tool_calls: DEFAULT_MAX_PARALLEL_TOOL_CALLS,
            },
        )
        .unwrap();
        let factory = dependencies
            .agents
            .set_factory(Arc::new(agent_loop.clone()))
            .unwrap();
        Self {
            context,
            dependencies,
            agent_loop,
            factory,
            flushes,
        }
    }

    async fn create(
        &self,
        id: &str,
        owner: Option<Arc<seekdeep_agent::Agent>>,
    ) -> seekdeep_agent::AgentHandle {
        let mut options = CreateAgentOptions::new(SessionId::new(id));
        options.agent_options = AgentOptions::default();
        options.owner_agent = owner;
        self.dependencies.agents.create(options).await.unwrap()
    }

    async fn dispose(self) {
        let _ = self.agent_loop.dispose().await;
        let _ = self.factory.dispose().await;
        self.dependencies.agents.dispose_initiators().await;
        let _ = self.context.fiber().dispose().await;
    }
}

async fn settle() {
    for _ in 0..12 {
        tokio::task::yield_now().await;
    }
}

#[test]
fn exports_loader_safe_function_plugin_shape() {
    let plugin = plugin();
    assert_eq!(plugin.name(), PACKAGE_NAME);
    assert_eq!(plugin.inject(), INJECT);
    assert_eq!(
        INJECT,
        ["agents", "sessions", "tools", "sessionPersistence"]
    );
}

#[tokio::test]
async fn installs_only_on_future_roots_and_unwinds_tools_on_plugin_disposal() {
    let test = PluginHarness::new();
    let existing = test.create("schedule-existing", None).await;
    let mounted = test
        .context
        .plugin(plugin(), Value::Null)
        .expect("schedule plugin");
    mounted.await_settled().await.unwrap();
    assert!(
        test.dependencies
            .tools
            .get("schedule_create", Some(existing.agent.scope_key()))
            .is_none()
    );
    assert!(
        test.dependencies
            .tools
            .get("schedule_create", None)
            .is_none()
    );

    let root = test.create("schedule-root", None).await;
    for name in ["schedule_create", "schedule_list", "schedule_delete"] {
        assert!(
            test.dependencies
                .tools
                .get(name, Some(root.agent.scope_key()))
                .is_some(),
            "missing {name}"
        );
    }
    assert!(
        test.dependencies
            .tools
            .get("schedule_create", None)
            .is_none()
    );

    let result = test
        .dependencies
        .tools
        .execute(
            ToolExecutionInput::new(
                CallId::new("schedule-plugin-create"),
                "schedule_create",
                json!({"prompt": "future reminder", "after_seconds": 3600}),
                AbortSignal::default(),
            )
            .with_agent(root.agent.clone()),
        )
        .await;
    assert!(!result.is_error(), "{result:?}");
    assert_eq!(result.value().unwrap()["id"], "schedule-1");
    assert_eq!(result.value().unwrap()["deliveryMode"], "session-local");

    let child = test
        .create("schedule-child", Some(root.agent.clone()))
        .await;
    assert!(
        test.dependencies
            .tools
            .get("schedule_create", Some(child.agent.scope_key()))
            .is_none()
    );

    let departing = test.create("schedule-departing", None).await;
    assert!(
        test.dependencies
            .tools
            .get("schedule_create", Some(departing.agent.scope_key()))
            .is_some()
    );
    departing.dispose().await.unwrap();
    assert!(
        test.dependencies
            .tools
            .get("schedule_create", Some(departing.agent.scope_key()))
            .is_none()
    );

    mounted.dispose().await.unwrap();
    for name in ["schedule_create", "schedule_list", "schedule_delete"] {
        assert!(
            test.dependencies
                .tools
                .get(name, Some(root.agent.scope_key()))
                .is_none(),
            "{name} survived plugin disposal"
        );
    }

    child.dispose().await.unwrap();
    root.dispose().await.unwrap();
    existing.dispose().await.unwrap();
    test.dispose().await;
}

#[tokio::test]
async fn unrelated_idle_edges_do_not_checkpoint_again() {
    let test = PluginHarness::new();
    let mounted = test.context.plugin(plugin(), Value::Null).unwrap();
    mounted.await_settled().await.unwrap();
    let root = test.create("schedule-unrelated-idle", None).await;
    settle().await;
    let baseline = test.flushes.load(Ordering::Acquire);
    AgentEvents::new(test.context.clone(), root.agent.clone()).emit(
        "agent/status",
        AgentStatusChanged {
            status: seekdeep_agent::AgentStatus::Running,
        },
    );
    AgentEvents::new(test.context.clone(), root.agent.clone()).emit(
        "agent/status",
        AgentStatusChanged {
            status: seekdeep_agent::AgentStatus::Idle,
        },
    );
    settle().await;
    assert_eq!(test.flushes.load(Ordering::Acquire), baseline);

    root.dispose().await.unwrap();
    mounted.dispose().await.unwrap();
    test.dispose().await;
}
