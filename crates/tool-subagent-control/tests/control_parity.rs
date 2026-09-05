//! Parent-side tool registration, catalog projection, rendering, and revocation.

use std::sync::Arc;

use seekdeep_agent::{
    Agent, AgentOptions, AgentRegistry, AgentStatus, Inbox, NoopInboxNotifications,
};
use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{AppendOptions, SessionEvent, SessionHeader, SessionId, SessionOrigin},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_llm::{AbortSignal, CallId, ContentBlock};
use seekdeep_scope::{ScopeKey, create_scope, scope_of};
use seekdeep_session_persistence::SESSION_PERSISTENCE;
use seekdeep_session_persistence_jsonl::{JsonlCompression, JsonlConfig};
use seekdeep_session_projection::SessionProjectionRegistry;
use seekdeep_subagent::{
    SubagentDescriptorInput, SubagentRuntime, snapshot_subagent_descriptor,
    subagent_identity_projection_definition, subagent_timing_projection_definition,
};
use seekdeep_tool_subagent_control::{
    INJECT, LIST_INJECT, LIST_NAME, NAME, install_control, install_list_agents,
    list_plugin as list_agents_plugin, plugin as control_plugin,
};
use seekdeep_tools::{ToolExecutionInput, ToolRuntime, ToolRuntimeConfig};
use serde_json::json;

struct Harness {
    context: Context,
    sessions: Arc<SessionStore>,
    agents: Arc<AgentRegistry>,
    tools: Arc<ToolRuntime>,
    parent: Arc<Agent>,
    child_id: SessionId,
}

impl Harness {
    fn new() -> Self {
        Self::new_with_child(true)
    }

    fn empty() -> Self {
        Self::new_with_child(false)
    }

    fn new_with_child(with_child: bool) -> Self {
        let context = Context::new();
        let sessions = SessionStore::install(&context).unwrap();
        let agents = Arc::new(AgentRegistry::new(context.clone()));
        agents.provide(&context).unwrap();
        let projections = SessionProjectionRegistry::install(&context).unwrap();
        projections
            .register(&context, subagent_identity_projection_definition())
            .unwrap();
        projections
            .register(&context, subagent_timing_projection_definition())
            .unwrap();
        SubagentRuntime::install(&context).unwrap();
        let tools = ToolRuntime::new(context.clone(), ToolRuntimeConfig::default()).unwrap();
        tools.provide(&context).unwrap();
        let parent_id = SessionId::new("parent");
        let parent_session = sessions
            .create(
                &context,
                Some(parent_id.clone()),
                CreateSessionOptions {
                    cwd: Some("/project".to_owned()),
                    ..CreateSessionOptions::default()
                },
            )
            .unwrap();
        let parent = agent(&context, parent_session);
        agents.register(&context, &parent, None).unwrap();
        let child_id = SessionId::new("child");
        let harness = Self {
            context,
            sessions,
            agents,
            tools,
            parent,
            child_id,
        };
        if with_child {
            harness.create_child(
                harness.parent.id().clone(),
                harness.child_id.clone(),
                &SubagentDescriptorInput::Continuable {
                    provider: "spawn".to_owned(),
                    label: "worker".to_owned(),
                    agent_provider: None,
                    agent_model: None,
                    persona: None,
                    tool_filter: None,
                },
            );
        }
        harness
    }

    fn create_child(
        &self,
        parent_id: SessionId,
        child_id: SessionId,
        descriptor: &SubagentDescriptorInput,
    ) -> Arc<seekdeep_core::session::Session> {
        let child = self
            .sessions
            .create(
                &self.context,
                Some(child_id),
                CreateSessionOptions {
                    cwd: Some("/project".to_owned()),
                    parent_session: Some(parent_id),
                    origin: Some(SessionOrigin::Subagent),
                    ..CreateSessionOptions::default()
                },
            )
            .unwrap();
        child
            .append(
                "subagent/descriptor",
                serde_json::to_value(snapshot_subagent_descriptor(descriptor).unwrap()).unwrap(),
                AppendOptions::default(),
            )
            .unwrap();
        child
    }

    async fn run(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> seekdeep_tools::ToolExecutionResult {
        self.run_with_signal(name, arguments, AbortSignal::default())
            .await
    }

    async fn run_with_signal(
        &self,
        name: &str,
        arguments: serde_json::Value,
        signal: AbortSignal,
    ) -> seekdeep_tools::ToolExecutionResult {
        self.tools
            .execute(
                ToolExecutionInput::new(
                    CallId::new(format!("call-{name}")),
                    name,
                    arguments,
                    signal,
                )
                .with_agent(self.parent.clone()),
            )
            .await
    }
}

fn agent(context: &Context, session: Arc<seekdeep_core::session::Session>) -> Arc<Agent> {
    let scope = create_scope(context, ScopeKey::new(), None).unwrap();
    let scope_key = scope_of(&scope.context).unwrap();
    let inbox = Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
    Arc::new(Agent::new(
        session.id().clone(),
        AgentOptions::default(),
        session,
        inbox,
        scope.context,
        scope_key,
    ))
}

fn text(result: &seekdeep_tools::ToolExecutionResult) -> String {
    result
        .content()
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn all_three_global_tools_register_and_revoke_with_exact_parameters() {
    let harness = Harness::new();
    let controls = install_control(&harness.context).unwrap();
    let list = install_list_agents(&harness.context).unwrap();
    let schemas = harness.tools.schemas(None);
    for name in ["send_message", "interrupt_agent", "list_agents"] {
        assert!(schemas.iter().any(|schema| schema.name == name));
    }
    assert_eq!(
        schemas
            .iter()
            .find(|schema| schema.name == "send_message")
            .unwrap()
            .parameters["required"],
        json!(["subagent_id", "message"])
    );
    let send = harness.tools.get("send_message", None).unwrap();
    assert_eq!(
        send.parameters["properties"]["subagent_id"]["description"],
        "The subagent id returned when the background subagent was started."
    );
    assert!(
        send.description
            .contains("becomes the subagent's next turn")
    );
    assert!(send.description.contains("message was NOT delivered"));

    let interrupt = harness.tools.get("interrupt_agent", None).unwrap();
    assert_eq!(interrupt.parameters["required"], json!(["agent_id"]));
    assert!(interrupt.description.contains("current turn"));
    assert!(interrupt.description.contains("until a later send_message"));

    let listed = harness.tools.get("list_agents", None).unwrap();
    assert!(listed.parameters.get("required").is_none());
    assert_eq!(
        listed.parameters["properties"]["scope"]["enum"],
        json!(["children", "descendants"])
    );
    assert!(listed.description.contains("resumable, not terminal"));
    assert!(listed.description.contains("interrupt_agent"));
    let branches = listed.output.schema.as_value()["items"]["oneOf"]
        .as_array()
        .unwrap();
    assert_eq!(branches.len(), 2);
    assert_eq!(
        branches[0]["properties"]["status"]["enum"],
        json!(["running", "idle", "ready"])
    );
    assert_eq!(
        branches[1]["properties"]["reason"]["enum"],
        json!(["corrupt", "unsupported", "unavailable"])
    );
    list.dispose().await.unwrap();
    for control in controls.into_iter().rev() {
        control.dispose().await.unwrap();
    }
    assert!(harness.tools.schemas(None).is_empty());
}

#[tokio::test]
async fn loader_plugins_preserve_names_dependencies_and_revoke_owned_tools() {
    let harness = Harness::new();
    let control_definition = control_plugin();
    assert_eq!(control_definition.name(), NAME);
    assert_eq!(control_definition.inject(), INJECT);
    let list_definition = list_agents_plugin();
    assert_eq!(list_definition.name(), LIST_NAME);
    assert_eq!(list_definition.inject(), LIST_INJECT);

    let control = harness
        .context
        .plugin(control_definition, serde_json::Value::Null)
        .unwrap();
    let list = harness
        .context
        .plugin(list_definition, serde_json::Value::Null)
        .unwrap();
    control.await_settled().await.unwrap();
    list.await_settled().await.unwrap();
    assert_eq!(harness.tools.schemas(None).len(), 3);

    list.dispose().await.unwrap();
    assert!(harness.tools.get("list_agents", None).is_none());
    assert!(harness.tools.get("send_message", None).is_some());
    control.dispose().await.unwrap();
    assert!(harness.tools.schemas(None).is_empty());
}

#[tokio::test]
async fn list_agents_projects_ready_then_running_and_renders_the_status_vocabulary() {
    let harness = Harness::new();
    install_list_agents(&harness.context).unwrap();
    let ready = harness.run("list_agents", json!({})).await;
    assert!(!ready.is_error());
    assert!(text(&ready).contains("child [ready] — worker"));
    let child_session = harness.sessions.get(&harness.child_id).unwrap();
    let child = agent(&harness.context, child_session);
    child.set_status(AgentStatus::Running);
    harness
        .agents
        .register(&harness.context, &child, None)
        .unwrap();
    let running = harness.run("list_agents", json!({})).await;
    assert!(text(&running).contains("child [running] — worker"));
    child.set_status(AgentStatus::Idle);
    let idle = harness.run("list_agents", json!({})).await;
    assert!(text(&idle).contains("child [idle] — worker"));
}

#[tokio::test]
async fn list_agents_renders_empty_omits_one_shot_and_traverses_through_it() {
    let harness = Harness::empty();
    install_list_agents(&harness.context).unwrap();
    let empty = harness.run("list_agents", json!({})).await;
    assert!(!empty.is_error());
    assert_eq!(text(&empty), "(no subagents)");

    let bridge_id = SessionId::new("bridge");
    harness.create_child(
        harness.parent.id().clone(),
        bridge_id.clone(),
        &SubagentDescriptorInput::OneShot {
            provider: "spawn".to_owned(),
            label: Some("one shot".to_owned()),
        },
    );
    harness.create_child(
        bridge_id.clone(),
        SessionId::new("nested"),
        &SubagentDescriptorInput::Continuable {
            provider: "spawn".to_owned(),
            label: "nested worker".to_owned(),
            agent_provider: None,
            agent_model: None,
            persona: None,
            tool_filter: None,
        },
    );

    let children = harness.run("list_agents", json!({})).await;
    assert_eq!(text(&children), "(no subagents)");
    let descendants = harness
        .run("list_agents", json!({ "scope": "descendants" }))
        .await;
    assert!(!descendants.is_error());
    assert_eq!(
        text(&descendants),
        "nested [ready] parent=bridge depth=2 — nested worker"
    );
}

#[tokio::test]
async fn list_agents_and_send_message_fail_loud_for_invalid_callers_or_targets() {
    let harness = Harness::new();
    install_control(&harness.context).unwrap();
    install_list_agents(&harness.context).unwrap();

    let missing = harness
        .run(
            "send_message",
            json!({ "subagent_id": "missing", "message": "hello?" }),
        )
        .await;
    assert!(missing.is_error());
    assert!(!text(&missing).is_empty());

    for (call_id, name, arguments) in [
        (
            "agentless-send",
            "send_message",
            json!({ "subagent_id": "child", "message": "x" }),
        ),
        (
            "agentless-interrupt",
            "interrupt_agent",
            json!({ "agent_id": "child" }),
        ),
        ("agentless-list", "list_agents", json!({})),
    ] {
        let result = harness
            .tools
            .execute(ToolExecutionInput::new(
                CallId::new(call_id),
                name,
                arguments,
                AbortSignal::default(),
            ))
            .await;
        assert!(result.is_error());
        assert!(text(&result).contains("requires a calling agent"));
    }
}

#[tokio::test]
async fn list_agents_observes_tool_cancellation() {
    let harness = Harness::new();
    install_list_agents(&harness.context).unwrap();
    let signal = AbortSignal::default();
    signal.abort();
    let result = harness
        .run_with_signal("list_agents", json!({ "scope": "descendants" }), signal)
        .await;
    assert!(result.is_error());
}

#[tokio::test]
async fn list_agents_renders_a_cold_descriptorless_child_as_corrupt() {
    let harness = Harness::empty();
    let temporary = tempfile::tempdir().unwrap();
    let mut config = JsonlConfig::new(temporary.path());
    config.compression = JsonlCompression::None;
    let persistence =
        seekdeep_session_persistence_jsonl::install(&harness.context, config).unwrap();
    persistence.await_settled().await.unwrap();
    let persistence = harness.context.get(SESSION_PERSISTENCE).unwrap();
    let child_id = SessionId::new("descriptorless-child");
    let mut header = SessionHeader::new(child_id.clone());
    header.cwd = Some("/project".to_owned());
    header.parent_session = Some(harness.parent.id().clone());
    header.origin = Some(SessionOrigin::Subagent);
    header.delegation_depth = Some(1);
    persistence.persistence().create(&header).await.unwrap();
    persistence
        .persistence()
        .append(
            &child_id,
            &[
                SessionEvent {
                    event_type: "turn/start".to_owned(),
                    seq: 0,
                    time: 1,
                    data: json!({"turn":1}),
                    source_event_seqs: None,
                    surface_op: None,
                    ignorable: None,
                },
                SessionEvent {
                    event_type: "turn/end".to_owned(),
                    seq: 1,
                    time: 2,
                    data: json!({"turn":1,"reason":{"kind":"interrupted"}}),
                    source_event_seqs: None,
                    surface_op: None,
                    ignorable: None,
                },
            ],
        )
        .await
        .unwrap();
    install_list_agents(&harness.context).unwrap();
    let result = harness.run("list_agents", json!({})).await;
    assert!(!result.is_error(), "{}", text(&result));
    assert_eq!(text(&result), "descriptorless-child [diagnostic: corrupt]");
    harness.context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn interrupt_absent_target_is_accepted_and_agentless_calls_fail_loud() {
    let harness = Harness::new();
    install_control(&harness.context).unwrap();
    let accepted = harness
        .run("interrupt_agent", json!({ "agent_id": "absent" }))
        .await;
    assert!(!accepted.is_error());
    assert!(text(&accepted).contains("interrupt requested for agent absent"));
    let agentless = harness
        .tools
        .execute(ToolExecutionInput::new(
            CallId::new("agentless"),
            "send_message",
            json!({ "subagent_id": "child", "message": "x" }),
            AbortSignal::default(),
        ))
        .await;
    assert!(agentless.is_error());
    assert!(text(&agentless).contains("requires a calling agent"));
}
