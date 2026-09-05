//! Full-loop parity for model-driven `todo_write` calls.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use futures::stream;
use seekdeep_agent::AgentOptions;
use seekdeep_agent_loop::{AgentLoopServices, DefaultAgentDriver, LoopAgent};
use seekdeep_cordis::Context;
use seekdeep_core::session::{Session, SessionEvent, SessionId};
use seekdeep_llm::{
    AdapterStream, CallId, ContentBlock, FinishReason, GenerateOptions, LlmAdapter, MessageSource,
    StreamChunk, UserMessage,
};
use seekdeep_tool_todo::{Config, apply};
use serde_json::{Value, json};

#[derive(Debug)]
struct ScriptedAdapter {
    responses: Mutex<VecDeque<Vec<StreamChunk>>>,
}

impl ScriptedAdapter {
    fn new(responses: impl IntoIterator<Item = Vec<StreamChunk>>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().collect()),
        }
    }
}

#[async_trait]
impl LlmAdapter for ScriptedAdapter {
    fn stream(&self, _options: GenerateOptions) -> AdapterStream {
        let response = self
            .responses
            .lock()
            .expect("script mutex poisoned")
            .pop_front()
            .expect("model requested more responses than the test supplied");
        AdapterStream::new(stream::iter(response.into_iter().map(Ok)))
    }
}

fn tool_response(call_id: &str, todos: &Value, text: Option<&str>) -> Vec<StreamChunk> {
    let mut chunks = Vec::new();
    if let Some(text) = text {
        chunks.push(StreamChunk::TextDelta {
            index: 0,
            text: text.to_owned(),
        });
    }
    chunks.extend([
        StreamChunk::BlockEnd {
            index: u64::from(text.is_some()),
            block: ContentBlock::ToolCall {
                id: CallId::new(call_id),
                name: "todo_write".to_owned(),
                arguments: json!({"todos": todos}).to_string(),
            },
        },
        StreamChunk::Finish {
            reason: FinishReason::ToolCalls,
            replay_state: None,
        },
    ]);
    chunks
}

fn text_response(text: &str) -> Vec<StreamChunk> {
    vec![
        StreamChunk::TextDelta {
            index: 0,
            text: text.to_owned(),
        },
        StreamChunk::Finish {
            reason: FinishReason::Stop,
            replay_state: None,
        },
    ]
}

struct Harness {
    context: Context,
    session: Arc<Session>,
    loop_agent: LoopAgent,
    _driver: Arc<DefaultAgentDriver>,
}

fn harness(id: &str, responses: Vec<Vec<StreamChunk>>) -> Harness {
    let context = Context::new();
    let dependencies = seekdeep_agent_loop_testkit::mount_agent_loop_test_dependencies(
        &context,
        seekdeep_agent_loop_testkit::AgentLoopTestDependenciesOptions::default(),
    )
    .expect("test dependencies");
    dependencies
        .llm
        .register_adapter(
            &["mock".to_owned()],
            Arc::new(ScriptedAdapter::new(responses)),
        )
        .expect("mock adapter");
    apply(
        &context,
        Config {
            allow_parallel_in_progress: true,
        },
    )
    .expect("todo tool");

    let session = Session::create(&SessionId::new(id), None, None).expect("session");
    let (loop_agent, driver) = LoopAgent::new_default(
        &context,
        &session,
        AgentOptions {
            provider: Some("mock".into()),
            model: Some("mock".into()),
            max_tokens: None,
            subagent_depth: None,
        },
        None,
        AgentLoopServices {
            llm: dependencies.llm,
            system_prompt: dependencies.system_prompt,
            tools: dependencies.tools,
            max_parallel_tool_calls: 10,
        },
    )
    .expect("loop agent");
    Harness {
        context,
        session,
        loop_agent,
        _driver: driver,
    }
}

async fn run(harness: &Harness, task: &str) {
    harness
        .loop_agent
        .agent
        .followup(UserMessage::new(
            vec![ContentBlock::Text {
                text: task.to_owned(),
            }],
            MessageSource::user(),
        ))
        .expect("followup");
    harness
        .loop_agent
        .agent
        .when_idle()
        .expect("idle waiter")
        .await
        .unwrap();
}

fn event<'a>(events: &'a [SessionEvent], event_type: &str) -> &'a SessionEvent {
    events
        .iter()
        .find(|event| event.event_type == event_type)
        .unwrap_or_else(|| panic!("no {event_type} event in session log"))
}

#[tokio::test]
async fn model_call_records_call_success_and_whole_todo_snapshot() {
    let todos = json!([
        {"content": "read the code", "status": "in_progress"},
        {"content": "write the fix", "status": "pending"},
    ]);
    let harness = harness(
        "it-todo",
        vec![
            tool_response("call-1", &todos, Some("Planning the work.")),
            text_response("Plan recorded."),
        ],
    );
    run(&harness, "plan a two-step task").await;

    let events = harness.session.events();
    assert_eq!(event(&events, "tool/call").data["name"], "todo_write");
    assert_eq!(
        event(&events, "tool/result")
            .data
            .pointer("/message/content/0/isError"),
        Some(&Value::Bool(false))
    );
    assert_eq!(event(&events, "todo/write").data["todos"], todos);

    harness.context.fiber().dispose().await.expect("dispose");
}

#[tokio::test]
async fn a_second_model_call_replaces_the_projected_whole_list() {
    let first = json!([{"content": "step one", "status": "in_progress"}]);
    let second = json!([
        {"content": "step one", "status": "completed"},
        {"content": "step two", "status": "in_progress"},
    ]);
    let harness = harness(
        "it-todo-2",
        vec![
            tool_response("call-1", &first, None),
            tool_response("call-2", &second, None),
            text_response("Done planning."),
        ],
    );
    run(&harness, "plan then update").await;

    let writes = harness
        .session
        .events()
        .into_iter()
        .filter(|event| event.event_type == "todo/write")
        .collect::<Vec<_>>();
    assert_eq!(writes.len(), 2);
    assert_eq!(writes.last().expect("last write").data["todos"], second);

    harness.context.fiber().dispose().await.expect("dispose");
}
