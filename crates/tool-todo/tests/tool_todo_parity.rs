//! Behavioral mirror of `packages/todo/tool-todo/tests/tool-todo.spec.ts`.

use std::sync::Arc;

use seekdeep_agent::{Agent, AgentOptions, AgentRegistry, Inbox, NoopInboxNotifications};
use seekdeep_cordis::Context;
use seekdeep_core::session::{Session, SessionId};
use seekdeep_llm::AbortSignal;
use seekdeep_scope::ScopeKey;
use seekdeep_system_prompt::{SystemPrompt, SystemPromptConfig};
use seekdeep_tool_todo::{Config, TOOL_NAME, apply};
use seekdeep_tools::{ToolExecutionInput, ToolRuntime, ToolRuntimeConfig};
use serde_json::{Value, json};

fn setup(
    allow_parallel: bool,
) -> (
    Context,
    Arc<ToolRuntime>,
    seekdeep_cordis::fiber::EffectHandle,
) {
    let context = Context::new();
    let agents = Arc::new(AgentRegistry::new(context.clone()));
    agents.provide(&context).expect("agents");
    let prompt = SystemPrompt::new(&context, SystemPromptConfig::default()).expect("prompt");
    let tools =
        seekdeep_tools::install(&context, &prompt, ToolRuntimeConfig::default()).expect("tools");
    let effect = apply(
        &context,
        Config {
            allow_parallel_in_progress: allow_parallel,
        },
    )
    .expect("todo tool");
    (context, tools, effect)
}

fn agent(id: &str) -> Arc<Agent> {
    let id = SessionId::new(id);
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

fn input(call: &str, arguments: Value, agent: Option<&Arc<Agent>>) -> ToolExecutionInput {
    let mut input = ToolExecutionInput::new(
        seekdeep_llm::CallId::new(call),
        TOOL_NAME,
        arguments,
        AbortSignal::default(),
    );
    input.agent = agent.cloned();
    input.agent_session = agent.map(|agent| agent.session().clone());
    input
}

fn todos(args: &Value) -> Value {
    json!({"todos": args})
}

#[test]
fn registers_the_exact_model_facing_schema() {
    let (_, tools, _effect) = setup(true);
    let schema = tools
        .schemas(None)
        .into_iter()
        .find(|schema| schema.name == TOOL_NAME)
        .expect("schema");
    assert!(schema.description.contains("Record and update"));
    let items = &schema.parameters["properties"]["todos"]["items"];
    assert_eq!(items["additionalProperties"], false);
    assert_eq!(items["properties"]["content"]["type"], "string");
    assert_eq!(
        items["properties"]["status"]["enum"],
        json!(["pending", "in_progress", "completed"])
    );
}

#[tokio::test]
async fn successful_call_writes_the_todo_write_event_and_counts() {
    let (_, tools, _effect) = setup(true);
    let owner = agent("owner");
    let session = owner.session().clone();
    let list = json!([
        {"content": "run subagent a", "status": "in_progress"},
        {"content": "run subagent b", "status": "in_progress"},
        {"content": "merge results", "status": "pending"}
    ]);
    let result = tools
        .execute(input("parallel", todos(&list), Some(&owner)))
        .await;
    assert!(!result.is_error(), "error: {:?}", result.error());
    assert_eq!(
        result.value(),
        Some(&json!({
            "todos": list,
            "counts": {"pending": 1, "inProgress": 2, "completed": 0}
        }))
    );
    let events = session.events();
    let write = events
        .iter()
        .rev()
        .find(|event| event.event_type == "todo/write")
        .expect("todo/write");
    assert_eq!(write.data["todos"], list);
}

#[tokio::test]
async fn rejects_parallel_in_progress_when_disallowed() {
    let (_, tools, _effect) = setup(false);
    let owner = agent("single-active");
    let session = owner.session().clone();
    let list = json!([
        {"content": "run subagent a", "status": "in_progress"},
        {"content": "run subagent b", "status": "in_progress"}
    ]);
    let result = tools
        .execute(input("parallel", todos(&list), Some(&owner)))
        .await;
    assert!(result.is_error());
    assert!(
        !session
            .events()
            .iter()
            .any(|event| event.event_type == "todo/write")
    );
}

#[tokio::test]
async fn rejects_a_non_agent_caller() {
    let (_, tools, _effect) = setup(true);
    let result = tools
        .execute(input(
            "no-agent",
            todos(&json!([{"content": "a", "status": "pending"}])),
            None,
        ))
        .await;
    assert!(result.is_error());
}

#[tokio::test]
async fn rejects_empty_and_duplicate_content() {
    let (_, tools, _effect) = setup(true);
    let owner = agent("owner");
    let empty = tools
        .execute(input(
            "empty",
            todos(&json!([{"content": "   ", "status": "pending"}])),
            Some(&owner),
        ))
        .await;
    assert!(empty.is_error());

    let duplicate = tools
        .execute(input(
            "dup",
            todos(&json!([
                {"content": "dup", "status": "pending"},
                {"content": "dup", "status": "completed"}
            ])),
            Some(&owner),
        ))
        .await;
    assert!(duplicate.is_error());
}

#[tokio::test]
async fn unregisters_the_tool_when_its_effect_is_disposed() {
    let (_context, tools, effect) = setup(true);
    assert!(
        tools
            .schemas(None)
            .iter()
            .any(|schema| schema.name == TOOL_NAME)
    );
    effect.dispose().await.expect("dispose");
    assert!(
        !tools
            .schemas(None)
            .iter()
            .any(|schema| schema.name == TOOL_NAME)
    );
}
