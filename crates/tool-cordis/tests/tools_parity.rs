//! End-to-end registration and dynamic define/run/inspect/stop/undefine parity.

use std::sync::Arc;

use seekdeep_agent::{
    Agent, AgentEvent, AgentOptions, AgentRegistry, Inbox, NoopInboxNotifications, PreStepDecision,
};
use seekdeep_agent_loop::AgentPreStepEvent;
use seekdeep_cordis::{Context, EventArgs, EventReply};
use seekdeep_core::session::{Session, SessionId};
use seekdeep_llm::{AbortSignal, CallId, ContentBlock, MessageSource, UserMessage};
use seekdeep_scope::ScopeKey;
use seekdeep_system_prompt::{AssembleContext, SystemPromptConfig};
use seekdeep_tools::{
    ToolExecutionInput, ToolExecutionResult, ToolPresentationMode, ToolRuntime, ToolRuntimeConfig,
};
use serde_json::{Value, json};

struct Harness {
    context: Context,
    prompt: Arc<seekdeep_system_prompt::SystemPrompt>,
    tools: Arc<ToolRuntime>,
    agent: Arc<Agent>,
}

fn harness() -> Harness {
    let context = Context::new();
    let agents = Arc::new(AgentRegistry::new(context.clone()));
    agents.provide(&context).unwrap();
    let prompt = seekdeep_system_prompt::install(&context, SystemPromptConfig::default()).unwrap();
    let tools = seekdeep_tools::install(
        &context,
        &prompt,
        ToolRuntimeConfig {
            mode: ToolPresentationMode::Native,
            ..ToolRuntimeConfig::default()
        },
    )
    .unwrap();
    let _runner = seekdeep_cordis_host_runner::DynamicCordisRunner::install(&context, 5_000);
    seekdeep_tool_cordis::apply(&context).unwrap();
    let id = SessionId::new("tool-cordis-session");
    let session = Session::create(&id, None, None).unwrap();
    let inbox = Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
    let agent = Arc::new(Agent::new(
        id,
        AgentOptions::default(),
        session,
        inbox,
        context.clone(),
        ScopeKey::new(),
    ));
    agents.register(&context, &agent, None).unwrap();
    Harness {
        context,
        prompt,
        tools,
        agent,
    }
}

async fn call(harness: &Harness, name: &str, arguments: Value) -> ToolExecutionResult {
    harness
        .tools
        .execute(
            ToolExecutionInput::new(
                CallId::new(format!("{name}-call")),
                name,
                arguments,
                AbortSignal::default(),
            )
            .with_agent(harness.agent.clone()),
        )
        .await
}

fn text(result: &ToolExecutionResult) -> String {
    result
        .content()
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn assert_defined_inspection(value: &Value, plugin_id: &str, package_id: &str) {
    assert_eq!(
        value,
        &json!({
            "mode":"package",
            "plugin":{
                "pluginId":plugin_id,
                "name":"Demo Package",
                "packageCount":1,
                "state":"defined",
            },
            "packageId":package_id,
            "name":"Demo Package",
            "purpose":"Provide one temporary service.",
            "code":{
                "host":"return { apply(ctx) { ctx.provide('demoService', { ok: true }) } }",
            },
            "runtime":{
                "state":"defined",
                "host":{
                    "status":"stopped",
                    "provides":[],
                    "waitingFor":[],
                    "handlers":[],
                },
                "client":{
                    "status":"absent",
                    "waitingFor":[],
                },
            },
        })
    );
}

#[tokio::test]
async fn registers_exact_source_tools_prompt_and_inspect_directory() {
    let harness = harness();
    assert_eq!(
        harness
            .tools
            .schemas(None)
            .iter()
            .map(|schema| schema.name.as_str())
            .collect::<Vec<_>>(),
        [
            "cordis_inspect_list",
            "cordis_inspect_query",
            "cordis_inspect_self",
            "cordis_define",
            "cordis_run",
            "cordis_stop",
            "cordis_undefine",
        ]
    );
    let assembly = harness
        .prompt
        .assemble(AssembleContext::default())
        .await
        .unwrap();
    assert!(assembly.sections.iter().any(|section| {
        section.name == "tool:cordis"
            && section.text == seekdeep_tool_cordis::cordis_system_prompt()
    }));
    let list = call(&harness, "cordis_inspect_list", json!({})).await;
    assert!(!list.is_error(), "{}", text(&list));
    assert_eq!(
        list.value().unwrap()["providers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|provider| provider["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["Service", "Event", "Builtin", "Tool"]
    );
    let query = call(
        &harness,
        "cordis_inspect_query",
        json!({"platform":"host","provider":"Service","method":"listService"}),
    )
    .await;
    assert!(!query.is_error(), "{}", text(&query));
    assert_eq!(query.value().unwrap()["data"]["mode"], "catalog");
}

#[tokio::test]
async fn define_run_inspect_stop_and_undefine_keep_one_plugin_identity() {
    let harness = harness();
    let defined = call(
        &harness,
        "cordis_define",
        json!({
            "plugin":{"kind":"new","idPrefix":"demo"},
            "name":"Demo Package",
            "purpose":"Provide one temporary service.",
            "code":{"host":"return { apply(ctx) { ctx.provide('demoService', { ok: true }) } }"}
        }),
    )
    .await;
    assert!(!defined.is_error(), "{}", text(&defined));
    let plugin_id = defined.value().unwrap()["pluginId"]
        .as_str()
        .unwrap()
        .to_owned();
    let package_id = defined.value().unwrap()["packageId"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(text(&defined).contains("it is not running yet"));

    let inspected = call(
        &harness,
        "cordis_inspect_self",
        json!({"pluginId":plugin_id,"packageId":package_id}),
    )
    .await;
    assert!(!inspected.is_error(), "{}", text(&inspected));
    assert_defined_inspection(inspected.value().unwrap(), &plugin_id, &package_id);

    let run = call(
        &harness,
        "cordis_run",
        json!({"pluginId":plugin_id,"packageId":package_id,"mode":"run"}),
    )
    .await;
    assert!(!run.is_error(), "{}", text(&run));
    assert_eq!(run.value().unwrap()["status"], "running");
    assert!(harness.context.has_named("demoService"));

    let running = call(
        &harness,
        "cordis_inspect_self",
        json!({"pluginId":plugin_id,"packageId":package_id}),
    )
    .await;
    assert!(!running.is_error(), "{}", text(&running));
    assert_eq!(running.value().unwrap()["plugin"]["state"], "running");
    assert_eq!(
        running.value().unwrap()["plugin"]["currentPackageId"],
        package_id
    );
    assert_eq!(running.value().unwrap()["runtime"]["state"], "running");
    assert_eq!(
        running.value().unwrap()["runtime"]["host"],
        json!({
            "status":"running",
            "provides":["demoService"],
            "waitingFor":[],
            "handlers":[],
        })
    );
    assert_eq!(
        running.value().unwrap()["runtime"]["client"],
        json!({"status":"absent","waitingFor":[]})
    );

    let stopped = call(&harness, "cordis_stop", json!({"pluginId":plugin_id})).await;
    assert!(!stopped.is_error(), "{}", text(&stopped));
    assert!(!harness.context.has_named("demoService"));
    let stopped_again = call(&harness, "cordis_stop", json!({"pluginId":plugin_id})).await;
    assert!(!stopped_again.is_error(), "{}", text(&stopped_again));

    let removed = call(&harness, "cordis_undefine", json!({"pluginId":plugin_id})).await;
    assert!(!removed.is_error(), "{}", text(&removed));
    assert_eq!(removed.value().unwrap()["wasRunning"], false);
    let list = call(&harness, "cordis_inspect_self", json!({})).await;
    assert_eq!(list.value().unwrap()["plugins"], json!([]));
}

#[tokio::test]
async fn agentless_calls_fail_before_dynamic_session_access() {
    let harness = harness();
    let result = harness
        .tools
        .execute(ToolExecutionInput::new(
            CallId::new("agentless"),
            "cordis_inspect_self",
            json!({}),
            AbortSignal::default(),
        ))
        .await;
    assert!(result.is_error());
    assert!(text(&result).contains("Agent-backed session"));
}

#[tokio::test]
async fn explicit_plugin_references_inject_exact_base_or_unavailable_instructions() {
    let harness = harness();
    let defined = call(
        &harness,
        "cordis_define",
        json!({
            "plugin":{"kind":"new","idPrefix":"demo"},
            "name":"Demo",
            "purpose":"Reference test.",
            "code":{"host":"return { apply() {} }"}
        }),
    )
    .await;
    assert!(!defined.is_error(), "{}", text(&defined));
    let message = UserMessage::new(
        vec![ContentBlock::Text {
            text: "modify @demo-1 and explain @ghost-9".to_owned(),
        }],
        MessageSource::user(),
    );
    let event = AgentEvent {
        agent: harness.agent.clone(),
        payload: AgentPreStepEvent {
            messages: vec![message.clone()],
            turn: 1,
            step: 1,
            signal: AbortSignal::default(),
        },
    };
    let reply = harness
        .context
        .events()
        .waterfall(
            &harness.context,
            "agent/pre-step",
            &EventArgs::one(event),
            move || {
                Box::pin(async move {
                    Ok(EventReply::Value(Arc::new(PreStepDecision::Enter {
                        messages: vec![message],
                    })))
                })
            },
        )
        .await
        .unwrap();
    let decision = reply.downcast::<PreStepDecision>().unwrap();
    let PreStepDecision::Enter { messages } = decision.as_ref() else {
        panic!("expected enter")
    };
    assert_eq!(messages.len(), 3);
    let injected = messages[1]
        .content()
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    assert!(injected.contains("explicitly referenced @demo-1"));
    assert!(injected.contains("Package pkg-1 as the base"));
    assert!(injected.contains("plugin.kind=\"existing\""));
    let unavailable = messages[2]
        .content()
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    assert!(unavailable.contains("@ghost-9"));
    assert!(unavailable.contains("unavailable in the current Session"));
}
