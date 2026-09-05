//! Retry execution through the shipping durable agent loop.

use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use futures::stream;
use parking_lot::Mutex;
use seekdeep_agent::{
    AgentCancelCause, AgentEvent, AgentOptions, AgentRegistry, CancelOptions, RequestErrorAction,
};
use seekdeep_agent_loop::{AgentLoopServices, AgentRequestErrorEvent, LoopAgent};
use seekdeep_cordis::{Context, EventOptions, EventReply};
use seekdeep_core::{
    session::{Session, SessionEvent, SessionId},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_llm::{
    AdapterStream, CallId, ContentBlock, FinishReason, GenerateOptions, LlmAdapter, LlmFailure,
    LlmRuntime, MessageSource, ResolvedRetryPolicy, StreamChunk, UserMessage, resolve_retry_policy,
};
use seekdeep_llm_retry::{RetryConfig, RetryId, RetryInternals, install_with_internals};
use seekdeep_system_prompt::{SystemPrompt, SystemPromptConfig};
use seekdeep_tools::{
    ToolDefinition, ToolOutputDefinition, ToolRuntime, ToolRuntimeConfig,
    assert_supported_json_schema,
};
use serde_json::{Map, Value, json};

#[derive(Debug)]
struct ScriptedAdapter {
    requests: Arc<Mutex<Vec<GenerateOptions>>>,
    responses: Mutex<VecDeque<Vec<StreamChunk>>>,
    policy: ResolvedRetryPolicy,
}

#[async_trait]
impl LlmAdapter for ScriptedAdapter {
    fn provider_retry_policy(&self, _provider: &str) -> Option<ResolvedRetryPolicy> {
        Some(self.policy.clone())
    }

    fn stream(&self, options: GenerateOptions) -> AdapterStream {
        self.requests.lock().push(options);
        let chunks = self
            .responses
            .lock()
            .pop_front()
            .expect("scripted response");
        AdapterStream::new(stream::iter(chunks.into_iter().map(Ok)))
    }
}

fn retry_policy() -> ResolvedRetryPolicy {
    resolve_retry_policy(
        Some(&json!({
            "mode":"normal",
            "maxRetries":2,
            "retryableCodes":["RATE_LIMIT","TRANSPORT"],
            "backoff":{"initialDelayMs":1,"maxDelayMs":1,"jitterRatio":1}
        })),
        "retryPolicy",
    )
    .unwrap()
}

fn long_retry_policy(delay_ms: u64) -> ResolvedRetryPolicy {
    resolve_retry_policy(
        Some(&json!({
            "mode":"normal",
            "maxRetries":2,
            "retryableCodes":["RATE_LIMIT"],
            "backoff":{
                "initialDelayMs":delay_ms,
                "maxDelayMs":delay_ms,
                "jitterRatio":0
            }
        })),
        "retryPolicy",
    )
    .unwrap()
}

fn zero_retry_policy() -> ResolvedRetryPolicy {
    resolve_retry_policy(
        Some(&json!({
            "mode":"normal",
            "maxRetries":2,
            "retryableCodes":["RATE_LIMIT"],
            "backoff":{"initialDelayMs":1,"maxDelayMs":1,"jitterRatio":1}
        })),
        "retryPolicy",
    )
    .unwrap()
}

fn always_retry_policy() -> ResolvedRetryPolicy {
    resolve_retry_policy(
        Some(&json!({
            "mode":"always",
            "backoff":{"initialDelayMs":1,"maxDelayMs":1,"jitterRatio":0}
        })),
        "retryPolicy",
    )
    .unwrap()
}

fn failed_with_partial() -> Vec<StreamChunk> {
    vec![
        StreamChunk::BlockStart {
            index: 0,
            block_type: "text".to_owned(),
        },
        StreamChunk::TextDelta {
            index: 0,
            text: "failed partial".to_owned(),
        },
        StreamChunk::BlockEnd {
            index: 0,
            block: ContentBlock::Text {
                text: "failed partial".to_owned(),
            },
        },
        StreamChunk::BlockStart {
            index: 1,
            block_type: "tool-call".to_owned(),
        },
        StreamChunk::ToolCallDelta {
            index: 1,
            id: CallId::new("discarded-call"),
            name: Some("danger".to_owned()),
            arguments_delta: "{}".to_owned(),
        },
        StreamChunk::BlockEnd {
            index: 1,
            block: ContentBlock::ToolCall {
                id: CallId::new("discarded-call"),
                name: "danger".to_owned(),
                arguments: "{}".to_owned(),
            },
        },
        StreamChunk::Finish {
            reason: FinishReason::Error {
                failure: LlmFailure {
                    message: "busy".to_owned(),
                    code: "RATE_LIMIT".to_owned(),
                    status: Some(429),
                    provider_retry_after_ms: None,
                    request_id: None,
                },
            },
            replay_state: None,
        },
    ]
}

fn failed(code: &str) -> Vec<StreamChunk> {
    vec![StreamChunk::Finish {
        reason: FinishReason::Error {
            failure: LlmFailure {
                message: "failed request".to_owned(),
                code: code.to_owned(),
                status: None,
                provider_retry_after_ms: None,
                request_id: None,
            },
        },
        replay_state: None,
    }]
}

fn success(text: &str) -> Vec<StreamChunk> {
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

fn user(text: &str) -> UserMessage {
    UserMessage::new(
        vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        MessageSource::user(),
    )
}

fn register_danger_tool(context: &Context, tools: &Arc<ToolRuntime>) -> Arc<AtomicUsize> {
    let tool_executions = Arc::new(AtomicUsize::new(0));
    let executions = tool_executions.clone();
    tools
        .register(
            context,
            ToolDefinition::new(
                "danger",
                "must not run for a failed provider attempt",
                Map::from_iter([("type".to_owned(), Value::String("object".to_owned()))]),
                ToolOutputDefinition::new(
                    Arc::new(assert_supported_json_schema(json!({"type":"string"})).unwrap()),
                    Arc::new(|_, value| {
                        Ok(vec![ContentBlock::Text {
                            text: value.as_str().unwrap_or_default().to_owned(),
                        }])
                    }),
                ),
                Arc::new(move |_, _| {
                    let executions = executions.clone();
                    Box::pin(async move {
                        executions.fetch_add(1, Ordering::AcqRel);
                        Ok(Value::String("unexpected".to_owned()))
                    })
                }),
            ),
        )
        .unwrap();
    tool_executions
}

fn assert_failed_attempt_is_diagnostic_only(session: &Session, tool_executions: &AtomicUsize) {
    let events = session.events();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "step/start")
            .count(),
        1
    );
    let retry_seq = events
        .iter()
        .find(|event| event.event_type == "llm/retry")
        .unwrap()
        .seq;
    let failed_chunks = events
        .iter()
        .filter(|event| event.event_type == "assistant/chunk" && event.seq < retry_seq)
        .collect::<Vec<_>>();
    assert_eq!(failed_chunks.len(), 7);
    let assistant_messages = events
        .iter()
        .filter(|event| event.event_type == "assistant/message")
        .collect::<Vec<_>>();
    assert_eq!(assistant_messages.len(), 1);
    assert!(failed_chunks.iter().all(|chunk| {
        !assistant_messages[0]
            .source_event_seqs
            .as_ref()
            .is_some_and(|sources| sources.contains(&chunk.seq))
    }));
    assert!(!events.iter().any(|event| event.event_type == "tool/call"));
    assert_eq!(tool_executions.load(Ordering::Acquire), 0);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "llm/retry")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "llm/retry-started")
            .count(),
        1
    );
    let messages = session.derive_messages();
    assert_eq!(messages.len(), 2);
    assert!(messages.iter().all(|message| {
        message
            .content()
            .iter()
            .all(|block| !matches!(block, ContentBlock::Text { text } if text == "failed partial"))
    }));
    assert!(matches!(
        &messages[1].content()[0],
        ContentBlock::Text { text } if text == "recovered"
    ));
}

#[tokio::test]
async fn failed_partial_attempt_retries_inside_one_step_and_never_enters_model_history() {
    let context = Context::new();
    let agents = Arc::new(AgentRegistry::new(context.clone()));
    agents.provide(&context).unwrap();
    let plugin = install_with_internals(
        &context,
        RetryConfig::default(),
        RetryInternals::new(|| 0.0, || RetryId::new("loop-retry-chain")),
    )
    .unwrap();
    plugin.await_settled().await.unwrap();

    let llm = LlmRuntime::install(&context).unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    llm.register_adapter(
        &["mock".to_owned()],
        Arc::new(ScriptedAdapter {
            requests: requests.clone(),
            responses: Mutex::new(VecDeque::from([
                failed_with_partial(),
                success("recovered"),
            ])),
            policy: retry_policy(),
        }),
    )
    .unwrap();
    let system_prompt = SystemPrompt::new(&context, SystemPromptConfig::default()).unwrap();
    let tools =
        ToolRuntime::new_with_system_prompt(&context, &system_prompt, ToolRuntimeConfig::default())
            .unwrap();
    let tool_executions = register_danger_tool(&context, &tools);
    let services = AgentLoopServices {
        llm,
        system_prompt,
        tools,
        max_parallel_tool_calls: 10,
    };
    let session = Session::create(&SessionId::new("loop-retry"), None, None).unwrap();
    let (loop_agent, _driver) = LoopAgent::new_default(
        &context,
        &session,
        AgentOptions {
            provider: Some("mock".into()),
            model: Some("model".into()),
            max_tokens: None,
            subagent_depth: None,
        },
        None,
        services,
    )
    .unwrap();
    loop_agent.agent.followup(user("go")).unwrap();
    loop_agent.agent.when_idle().unwrap().await.unwrap();

    {
        let requests = requests.lock();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].messages, requests[1].messages);
    }
    assert_failed_attempt_is_diagnostic_only(&session, &tool_executions);

    plugin.dispose().await.unwrap();
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn turn_cancellation_during_backoff_reaches_idle_without_another_attempt_or_step() {
    let context = Context::new();
    let agents = Arc::new(AgentRegistry::new(context.clone()));
    agents.provide(&context).unwrap();
    let plugin = install_with_internals(
        &context,
        RetryConfig::default(),
        RetryInternals::new(|| 0.5, || RetryId::new("cancel-chain")),
    )
    .unwrap();
    plugin.await_settled().await.unwrap();

    let llm = LlmRuntime::install(&context).unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    llm.register_adapter(
        &["mock".to_owned()],
        Arc::new(ScriptedAdapter {
            requests: requests.clone(),
            responses: Mutex::new(VecDeque::from([
                failed_with_partial(),
                success("must not run"),
            ])),
            policy: long_retry_policy(60_000),
        }),
    )
    .unwrap();
    let system_prompt = SystemPrompt::new(&context, SystemPromptConfig::default()).unwrap();
    let tools =
        ToolRuntime::new_with_system_prompt(&context, &system_prompt, ToolRuntimeConfig::default())
            .unwrap();
    let services = AgentLoopServices {
        llm,
        system_prompt,
        tools,
        max_parallel_tool_calls: 10,
    };
    let session = Session::create(&SessionId::new("loop-cancel"), None, None).unwrap();
    let (loop_agent, _driver) = LoopAgent::new_default(
        &context,
        &session,
        AgentOptions {
            provider: Some("mock".into()),
            model: Some("model".into()),
            max_tokens: None,
            subagent_depth: None,
        },
        None,
        services,
    )
    .unwrap();
    loop_agent.agent.followup(user("go")).unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if session
                .events()
                .iter()
                .any(|event| event.event_type == "llm/retry")
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    loop_agent
        .agent
        .cancel(AgentCancelCause::User, CancelOptions::default())
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(1),
        loop_agent.agent.when_idle().unwrap(),
    )
    .await
    .unwrap()
    .unwrap();

    assert_eq!(requests.lock().len(), 1);
    let events = session.events();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "step/start")
            .count(),
        1
    );
    assert!(
        !events
            .iter()
            .any(|event| event.event_type == "llm/retry-started")
    );
    assert_eq!(events.last().unwrap().event_type, "turn/end");
    assert_eq!(events.last().unwrap().data["reason"]["kind"], "aborted");

    plugin.dispose().await.unwrap();
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn synchronous_cancellation_from_the_retry_status_event_beats_zero_delay() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).unwrap();
    let agents = Arc::new(AgentRegistry::new(context.clone()));
    agents.provide(&context).unwrap();
    let plugin = install_with_internals(
        &context,
        RetryConfig::default(),
        RetryInternals::new(|| 0.0, || RetryId::new("status-cancel-chain")),
    )
    .unwrap();
    plugin.await_settled().await.unwrap();

    let llm = LlmRuntime::install(&context).unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    llm.register_adapter(
        &["mock".to_owned()],
        Arc::new(ScriptedAdapter {
            requests: requests.clone(),
            responses: Mutex::new(VecDeque::from([
                failed_with_partial(),
                success("must not run"),
            ])),
            policy: zero_retry_policy(),
        }),
    )
    .unwrap();
    let system_prompt = SystemPrompt::new(&context, SystemPromptConfig::default()).unwrap();
    let tools =
        ToolRuntime::new_with_system_prompt(&context, &system_prompt, ToolRuntimeConfig::default())
            .unwrap();
    let services = AgentLoopServices {
        llm,
        system_prompt,
        tools,
        max_parallel_tool_calls: 10,
    };
    let session = sessions
        .create(
            &context,
            Some(SessionId::new("loop-status-cancel")),
            CreateSessionOptions::default(),
        )
        .unwrap();
    let (loop_agent, _driver) = LoopAgent::new_default(
        &context,
        &session,
        AgentOptions {
            provider: Some("mock".into()),
            model: Some("model".into()),
            max_tokens: None,
            subagent_depth: None,
        },
        None,
        services,
    )
    .unwrap();
    let target = session.id().clone();
    let agent_for_event = loop_agent.agent.clone();
    context
        .events()
        .on_sync(
            &context,
            "session/event",
            move |_, args| {
                let published_session = args
                    .get::<Session>(0)
                    .ok_or_else(|| anyhow::anyhow!("session/event lacks session"))?;
                let event = args
                    .get::<SessionEvent>(1)
                    .ok_or_else(|| anyhow::anyhow!("session/event lacks event"))?;
                if published_session.id() == &target && event.event_type == "llm/retry" {
                    agent_for_event.cancel(AgentCancelCause::User, CancelOptions::default())?;
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    loop_agent.agent.followup(user("go")).unwrap();
    tokio::time::timeout(
        Duration::from_secs(1),
        loop_agent.agent.when_idle().unwrap(),
    )
    .await
    .unwrap()
    .unwrap();

    assert_eq!(requests.lock().len(), 1);
    let events = session.events();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "llm/retry")
            .count(),
        1
    );
    assert!(
        !events
            .iter()
            .any(|event| event.event_type == "llm/retry-started")
    );

    plugin.dispose().await.unwrap();
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn turn_cancellation_waits_for_delegated_always_recovery_before_becoming_idle() {
    let context = Context::new();
    let agents = Arc::new(AgentRegistry::new(context.clone()));
    agents.provide(&context).unwrap();
    let plugin = install_with_internals(
        &context,
        RetryConfig::default(),
        RetryInternals::new(|| 0.5, || RetryId::new("delegated-cancel-chain")),
    )
    .unwrap();
    plugin.await_settled().await.unwrap();
    let entered = Arc::new(tokio::sync::Semaphore::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let entered_listener = entered.clone();
    let release_listener = release.clone();
    context
        .events()
        .on_waterfall(
            &context,
            "agent/request-error",
            move |_, _, _| {
                let entered = entered_listener.clone();
                let release = release_listener.clone();
                Box::pin(async move {
                    entered.add_permits(1);
                    release.acquire().await.unwrap().forget();
                    Ok(EventReply::Value(Arc::new(RequestErrorAction::Retry)))
                })
            },
            EventOptions::default(),
        )
        .unwrap();

    let llm = LlmRuntime::install(&context).unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    llm.register_adapter(
        &["mock".to_owned()],
        Arc::new(ScriptedAdapter {
            requests: requests.clone(),
            responses: Mutex::new(VecDeque::from([failed("AUTH")])),
            policy: always_retry_policy(),
        }),
    )
    .unwrap();
    let system_prompt = SystemPrompt::new(&context, SystemPromptConfig::default()).unwrap();
    let tools =
        ToolRuntime::new_with_system_prompt(&context, &system_prompt, ToolRuntimeConfig::default())
            .unwrap();
    let services = AgentLoopServices {
        llm,
        system_prompt,
        tools,
        max_parallel_tool_calls: 10,
    };
    let session = Session::create(&SessionId::new("loop-delegated-cancel"), None, None).unwrap();
    let (loop_agent, _driver) = LoopAgent::new_default(
        &context,
        &session,
        AgentOptions {
            provider: Some("mock".into()),
            model: Some("model".into()),
            max_tokens: None,
            subagent_depth: None,
        },
        None,
        services,
    )
    .unwrap();
    loop_agent.agent.followup(user("go")).unwrap();
    entered.acquire().await.unwrap().forget();
    loop_agent
        .agent
        .cancel(AgentCancelCause::User, CancelOptions::default())
        .unwrap();
    let mut idle = loop_agent.agent.when_idle().unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut idle)
            .await
            .is_err()
    );
    release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(1), idle)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(requests.lock().len(), 1);
    assert!(
        !session
            .events()
            .iter()
            .any(|event| event.event_type == "llm/retry")
    );
    let end = session.events().into_iter().last().unwrap();
    assert_eq!(end.event_type, "turn/end");
    assert_eq!(end.data["reason"]["kind"], "aborted");

    plugin.dispose().await.unwrap();
    context.fiber().dispose().await.unwrap();
}

async fn assert_earlier_listener_cancellation(policy: ResolvedRetryPolicy, id: &str) {
    let context = Context::new();
    let agents = Arc::new(AgentRegistry::new(context.clone()));
    agents.provide(&context).unwrap();
    context
        .events()
        .on_waterfall(
            &context,
            "agent/request-error",
            move |_, args, next| {
                Box::pin(async move {
                    let event = args
                        .get::<AgentEvent<AgentRequestErrorEvent>>(0)
                        .ok_or_else(|| anyhow::anyhow!("request-error event missing"))?;
                    event
                        .agent
                        .cancel(AgentCancelCause::User, CancelOptions::default())?;
                    next.run().await
                })
            },
            EventOptions::default(),
        )
        .unwrap();
    let plugin = install_with_internals(
        &context,
        RetryConfig::default(),
        RetryInternals::new(|| 0.5, || RetryId::new("pre-cancel-chain")),
    )
    .unwrap();
    plugin.await_settled().await.unwrap();

    let llm = LlmRuntime::install(&context).unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    llm.register_adapter(
        &["mock".to_owned()],
        Arc::new(ScriptedAdapter {
            requests: requests.clone(),
            responses: Mutex::new(VecDeque::from([failed("SERVER"), success("must not run")])),
            policy,
        }),
    )
    .unwrap();
    let system_prompt = SystemPrompt::new(&context, SystemPromptConfig::default()).unwrap();
    let tools =
        ToolRuntime::new_with_system_prompt(&context, &system_prompt, ToolRuntimeConfig::default())
            .unwrap();
    let services = AgentLoopServices {
        llm,
        system_prompt,
        tools,
        max_parallel_tool_calls: 10,
    };
    let session = Session::create(&SessionId::new(id), None, None).unwrap();
    let (loop_agent, _driver) = LoopAgent::new_default(
        &context,
        &session,
        AgentOptions {
            provider: Some("mock".into()),
            model: Some("model".into()),
            max_tokens: None,
            subagent_depth: None,
        },
        None,
        services,
    )
    .unwrap();
    loop_agent.agent.followup(user("go")).unwrap();
    tokio::time::timeout(
        Duration::from_secs(1),
        loop_agent.agent.when_idle().unwrap(),
    )
    .await
    .unwrap()
    .unwrap();

    assert_eq!(requests.lock().len(), 1);
    assert!(
        !session
            .events()
            .iter()
            .any(|event| event.event_type == "llm/retry")
    );
    let end = session.events().into_iter().last().unwrap();
    assert_eq!(end.data["reason"]["kind"], "aborted");

    plugin.dispose().await.unwrap();
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn an_earlier_recovery_listener_can_cancel_before_either_retry_mode_runs() {
    assert_earlier_listener_cancellation(retry_policy(), "loop-pre-cancel-normal").await;
    assert_earlier_listener_cancellation(always_retry_policy(), "loop-pre-cancel-always").await;
}
