//! End-to-end request-history coverage through the real Rust agent driver.

use std::sync::{
    Arc,
    atomic::{AtomicI64, AtomicUsize, Ordering},
};

use async_trait::async_trait;
use futures::stream;
use parking_lot::Mutex;
use seekdeep_agent::AgentOptions;
use seekdeep_agent_loop::{AgentLoopServices, LoopAgent};
use seekdeep_cordis::Context;
use seekdeep_core::session::{Session, SessionId, SurfaceOp};
use seekdeep_llm::{
    AdapterStream, CallId, ContentBlock, FinishReason, GenerateOptions, LlmAdapter, LlmRuntime,
    MessageSource, StreamChunk, UserMessage,
};
use seekdeep_system_prompt::{SystemPrompt, SystemPromptConfig};
use seekdeep_time_context::{TimeContextConfig, apply_with_clock};
use seekdeep_tools::{
    ToolDefinition, ToolOutputDefinition, ToolRuntime, ToolRuntimeConfig,
    assert_supported_json_schema,
};
use serde_json::{Map, Value, json};

#[derive(Debug)]
struct ScriptedAdapter {
    calls: AtomicUsize,
    requests: Arc<Mutex<Vec<GenerateOptions>>>,
}

#[async_trait]
impl LlmAdapter for ScriptedAdapter {
    fn stream(&self, options: GenerateOptions) -> AdapterStream {
        self.requests.lock().push(options);
        let chunks = if self.calls.fetch_add(1, Ordering::AcqRel) == 0 {
            vec![
                Ok(StreamChunk::BlockEnd {
                    index: 0,
                    block: ContentBlock::ToolCall {
                        id: CallId::new("tick-1"),
                        name: "tick".to_owned(),
                        arguments: "{}".to_owned(),
                    },
                }),
                Ok(StreamChunk::Finish {
                    reason: FinishReason::ToolCalls,
                    replay_state: None,
                }),
            ]
        } else {
            vec![
                Ok(StreamChunk::TextDelta {
                    index: 0,
                    text: "done".to_owned(),
                }),
                Ok(StreamChunk::Finish {
                    reason: FinishReason::Stop,
                    replay_state: None,
                }),
            ]
        };
        AdapterStream::new(stream::iter(chunks))
    }
}

fn request_text(request: &GenerateOptions) -> String {
    request
        .messages
        .iter()
        .flat_map(seekdeep_llm::Message::content)
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn real_driver_persists_one_ordered_reading_per_request_without_header_leakage() {
    let context = Context::new();
    let session =
        Session::create(&SessionId::new("time-context-loop"), None, None).expect("session");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let llm = LlmRuntime::install(&context).expect("llm");
    llm.register_adapter(
        &["mock".to_owned()],
        Arc::new(ScriptedAdapter {
            calls: AtomicUsize::new(0),
            requests: requests.clone(),
        }),
    )
    .expect("adapter");
    let prompt = SystemPrompt::new(&context, SystemPromptConfig::default()).expect("prompt");
    let tools =
        ToolRuntime::new_with_system_prompt(&context, &prompt, ToolRuntimeConfig::default())
            .expect("tools");
    let clock = Arc::new(AtomicI64::new(1_783_987_200_000));
    apply_with_clock(
        &context,
        &TimeContextConfig {
            time_zone: Some("UTC".to_owned()),
            ..TimeContextConfig::default()
        },
        {
            let clock = clock.clone();
            Arc::new(move || clock.load(Ordering::Acquire))
        },
    )
    .expect("time context");

    let output = ToolOutputDefinition::new(
        Arc::new(assert_supported_json_schema(json!({"type": "string"})).expect("schema")),
        Arc::new(|_, value| {
            Ok(vec![ContentBlock::Text {
                text: value.as_str().unwrap_or_default().to_owned(),
            }])
        }),
    );
    let session_for_tool = session.clone();
    let clock_for_tool = clock.clone();
    tools
        .register(
            &context,
            ToolDefinition::new(
                "tick",
                "advance deterministic time",
                Map::from_iter([("type".to_owned(), Value::String("object".to_owned()))]),
                output,
                Arc::new(move |_, _| {
                    let session = session_for_tool.clone();
                    let clock = clock_for_tool.clone();
                    Box::pin(async move {
                        let reading_time = session
                            .events()
                            .iter()
                            .rev()
                            .find(|event| {
                                event.event_type == "user/message"
                                    && event.data["source"]["plugin"] == "time-context"
                            })
                            .expect("first reading persisted before tool execution")
                            .time;
                        clock.store(reading_time + 61_000, Ordering::Release);
                        Ok(Value::String("advanced".to_owned()))
                    })
                }),
            ),
        )
        .expect("tool");

    let services = AgentLoopServices {
        llm,
        system_prompt: prompt,
        tools,
        max_parallel_tool_calls: 10,
    };
    let (loop_agent, _driver) = LoopAgent::new_default(
        &context,
        &session,
        AgentOptions {
            provider: Some("mock".into()),
            model: Some("model".into()),
            max_tokens: None,
        },
        None,
        services,
    )
    .expect("loop agent");
    loop_agent
        .agent
        .followup(UserMessage::new(
            vec![ContentBlock::Text {
                text: "start".to_owned(),
            }],
            MessageSource::user(),
        ))
        .expect("followup");
    loop_agent.agent.when_idle().expect("idle").await;

    let requests = requests.lock();
    assert_eq!(requests.len(), 2);
    let events = session.events();
    let contexts = events
        .iter()
        .filter(|event| {
            event.event_type == "user/message"
                && event.data["source"]["kind"] == "plugin"
                && event.data["source"]["plugin"] == "time-context"
        })
        .collect::<Vec<_>>();
    let starts = events
        .iter()
        .filter(|event| event.event_type == "step/start")
        .collect::<Vec<_>>();
    assert_eq!(contexts.len(), requests.len());
    assert_eq!(starts.len(), requests.len());
    for (context, start) in contexts.iter().zip(&starts) {
        assert!(context.seq > start.seq);
        assert_eq!(context.surface_op, Some(SurfaceOp::append()));
        assert_eq!(context.data["source"]["form"], "snapshot");
        assert_eq!(
            context.data["source"]["sections"][0]["name"],
            "time-context"
        );
        assert_eq!(
            context.data["source"]["sections"][0]["text"],
            context.data["content"][0]["text"]
        );
    }

    let first = request_text(&requests[0]);
    let second = request_text(&requests[1]);
    assert!(first.contains("Time sampled while preparing turn 1, step 1:"));
    assert!(first.contains("preceding model-visible message: unavailable"));
    assert!(!first.contains("Time sampled while preparing turn 1, step 2:"));
    assert!(second.contains("Time sampled while preparing turn 1, step 1:"));
    assert!(second.contains("Time sampled while preparing turn 1, step 2:"));
    assert!(second.contains("preceding step context: 1m 1s"));
    assert!(requests.iter().all(|request| {
        request
            .system
            .as_deref()
            .is_none_or(|system| !system.contains("Time sampled while preparing"))
    }));
    assert!(
        events
            .iter()
            .filter(|event| event.event_type == "request/header")
            .all(|event| !event
                .data
                .to_string()
                .contains("Time sampled while preparing"))
    );
}
