//! Codec, wire negotiation, prompt, multi-session, approval, and teardown parity.

use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use futures::{future::BoxFuture, stream};
use parking_lot::Mutex;
use seekdeep_acp::{
    AcpBridge, AcpBridgeConfig, AcpClient, AcpContinuableDrainHook, AcpStopReason,
    PROTOCOL_VERSION, PermissionPolicy, acp_content_text, acp_prompt_to_text, acp_stop_reason,
    prompt_has_unsupported_content, to_acp_prompt, turn_end_to_stop_reason, types::client_methods,
};
use seekdeep_agent::{
    Agent, AgentCancelCause, AgentEvent, AgentEvents, AgentFactory, AgentHandle, AgentOptions,
    CancelOptions, CreateAgentOptions, Inbox, NoopInboxNotifications, PreStepDecision,
    RequestErrorAction, ResumeAgentOptions,
};
use seekdeep_agent_loop::{
    AgentErrorEvent, AgentInboxMessage, AgentLoop, AgentLoopServices,
    DEFAULT_MAX_PARALLEL_TOOL_CALLS,
};
use seekdeep_attachment::{AttachmentId, ImageAttachmentRef, ImageMediaType};
use seekdeep_cordis::{Context, EventOptions, EventReply};
use seekdeep_core::session::{AppendOptions, Session, SessionEvent, SessionHeader, SessionId};
use seekdeep_llm::{
    AdapterStream, CallId, ContentBlock, FinishReason, GenerateOptions, LlmAdapter, LlmFailure,
    MessageSource, StreamChunk, UserMessage,
};
use seekdeep_scope::ScopeKey;
use seekdeep_sdk_protocol::JsonRpcLineTransport;
use seekdeep_subagent::SubagentStopReason;
use seekdeep_tools::{ContentToolFixtureOptions, define_content_tool_fixture};
use seekdeep_user_approval::{
    ApprovalConfig, ApprovalOutcome, ApprovalRequest, install as install_approval,
};
use serde_json::{Value, json};

#[derive(Clone, Debug)]
enum Behavior {
    Text(String),
    ReasoningText {
        reasoning: String,
        text: String,
    },
    MaxTokens(String),
    Image(String),
    ToolCall,
    Error {
        partial: Option<String>,
        message: String,
    },
    Hang,
}

#[derive(Debug)]
struct ScriptAdapter {
    behaviors: Mutex<VecDeque<Behavior>>,
    requests: Arc<Mutex<Vec<GenerateOptions>>>,
}

type AgentDisposeHook =
    Arc<dyn Fn(usize, Arc<AgentHandle>) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;

#[derive(Clone)]
struct DecoratingFactory {
    inner: AgentLoop,
    next: Arc<AtomicUsize>,
    dispose: AgentDisposeHook,
}

impl DecoratingFactory {
    fn wrap(&self, handle: AgentHandle) -> AgentHandle {
        let index = self.next.fetch_add(1, Ordering::AcqRel);
        let handle = Arc::new(handle);
        let agent = Arc::clone(&handle.agent);
        let dispose = Arc::clone(&self.dispose);
        AgentHandle::new(agent, Box::new(move || dispose(index, Arc::clone(&handle))))
    }
}

#[async_trait]
impl AgentFactory for DecoratingFactory {
    async fn create_agent(
        &self,
        owner_context: &Context,
        options: CreateAgentOptions,
    ) -> anyhow::Result<AgentHandle> {
        self.inner
            .create_agent(owner_context, options)
            .await
            .map(|handle| self.wrap(handle))
    }

    async fn resume(
        &self,
        owner_context: &Context,
        options: ResumeAgentOptions,
    ) -> anyhow::Result<AgentHandle> {
        self.inner
            .resume_agent(owner_context, options)
            .await
            .map(|handle| self.wrap(handle))
    }
}

#[async_trait]
impl LlmAdapter for ScriptAdapter {
    fn stream(&self, options: GenerateOptions) -> AdapterStream {
        self.requests
            .lock()
            .push(options.clone_preserving_agent_loop_request());
        match self.behaviors.lock().pop_front().expect("script exhausted") {
            Behavior::Text(text) => AdapterStream::new(stream::iter([
                Ok(StreamChunk::TextDelta { index: 0, text }),
                Ok(StreamChunk::Finish {
                    reason: FinishReason::Stop,
                    replay_state: None,
                }),
            ])),
            Behavior::ReasoningText { reasoning, text } => AdapterStream::new(stream::iter([
                Ok(StreamChunk::BlockEnd {
                    index: 0,
                    block: ContentBlock::Reasoning { text: reasoning },
                }),
                Ok(StreamChunk::TextDelta { index: 1, text }),
                Ok(StreamChunk::Finish {
                    reason: FinishReason::Stop,
                    replay_state: None,
                }),
            ])),
            Behavior::MaxTokens(text) => AdapterStream::new(stream::iter([
                Ok(StreamChunk::TextDelta { index: 0, text }),
                Ok(StreamChunk::Finish {
                    reason: FinishReason::MaxTokens,
                    replay_state: None,
                }),
            ])),
            Behavior::Image(attachment_id) => AdapterStream::new(stream::iter([
                Ok(StreamChunk::BlockEnd {
                    index: 0,
                    block: ContentBlock::Image {
                        attachment: ImageAttachmentRef {
                            attachment_id: AttachmentId::new(attachment_id),
                            media_type: ImageMediaType::Png,
                            bytes: 1,
                            width: 1,
                            height: 1,
                            name: None,
                        },
                    },
                }),
                Ok(StreamChunk::Finish {
                    reason: FinishReason::Stop,
                    replay_state: None,
                }),
            ])),
            Behavior::ToolCall => AdapterStream::new(stream::iter([
                Ok(StreamChunk::BlockEnd {
                    index: 0,
                    block: ContentBlock::ToolCall {
                        id: CallId::new("call-1"),
                        name: "echo".to_owned(),
                        arguments: "{}".to_owned(),
                    },
                }),
                Ok(StreamChunk::Finish {
                    reason: FinishReason::ToolCalls,
                    replay_state: None,
                }),
            ])),
            Behavior::Error { partial, message } => {
                let mut chunks = Vec::new();
                if let Some(text) = partial {
                    chunks.push(Ok(StreamChunk::TextDelta { index: 0, text }));
                }
                chunks.push(Ok(StreamChunk::Finish {
                    reason: FinishReason::Error {
                        failure: LlmFailure {
                            message,
                            code: "PROVIDER_ERROR".to_owned(),
                            status: None,
                            provider_retry_after_ms: None,
                            request_id: None,
                        },
                    },
                    replay_state: None,
                }));
                AdapterStream::new(stream::iter(chunks))
            }
            Behavior::Hang => {
                let signal = options.signal.expect("agent request signal");
                AdapterStream::new(stream::once(async move {
                    signal.cancelled().await;
                    Ok(StreamChunk::Finish {
                        reason: FinishReason::Aborted {
                            failure: LlmFailure {
                                message: "cancelled".to_owned(),
                                code: "ABORTED".to_owned(),
                                status: None,
                                provider_retry_after_ms: None,
                                request_id: None,
                            },
                        },
                        replay_state: None,
                    })
                }))
            }
        }
    }
}

struct Harness {
    context: Context,
    dependencies: seekdeep_agent_loop_testkit::AgentLoopTestDependencies,
    loop_: AgentLoop,
    bridge: Arc<AcpBridge>,
    client: Arc<AcpClient>,
    updates: Arc<Mutex<Vec<seekdeep_acp::AcpSessionUpdate>>>,
    approval: seekdeep_user_approval::ApprovalInstallation,
    client_transport: Arc<JsonRpcLineTransport>,
    requests: Arc<Mutex<Vec<GenerateOptions>>>,
}

impl Harness {
    fn new(behaviors: Vec<Behavior>, permission: PermissionPolicy) -> Self {
        Self::new_with_drain(behaviors, permission, None)
    }

    fn new_with_drain(
        behaviors: Vec<Behavior>,
        permission: PermissionPolicy,
        drain: Option<AcpContinuableDrainHook>,
    ) -> Self {
        Self::new_with_hooks(behaviors, permission, drain, None)
    }

    fn new_with_hooks(
        behaviors: Vec<Behavior>,
        permission: PermissionPolicy,
        drain: Option<AcpContinuableDrainHook>,
        dispose_hook: Option<AgentDisposeHook>,
    ) -> Self {
        let context = Context::new();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let dependencies = seekdeep_agent_loop_testkit::mount_agent_loop_test_dependencies(
            &context,
            seekdeep_agent_loop_testkit::AgentLoopTestDependenciesOptions::default(),
        )
        .unwrap();
        dependencies
            .llm
            .register_adapter(
                &["mock".to_owned()],
                Arc::new(ScriptAdapter {
                    behaviors: Mutex::new(behaviors.into()),
                    requests: Arc::clone(&requests),
                }),
            )
            .unwrap();
        let loop_ = AgentLoop::new(
            context.clone(),
            Arc::clone(&dependencies.sessions),
            (*dependencies.agents).clone(),
            AgentLoopServices {
                llm: Arc::clone(&dependencies.llm),
                system_prompt: Arc::clone(&dependencies.system_prompt),
                tools: Arc::clone(&dependencies.tools),
                max_parallel_tool_calls: DEFAULT_MAX_PARALLEL_TOOL_CALLS,
            },
        )
        .unwrap();
        let factory: Arc<dyn AgentFactory> = dispose_hook.map_or_else(
            || Arc::new(loop_.clone()) as Arc<dyn AgentFactory>,
            |dispose| {
                Arc::new(DecoratingFactory {
                    inner: loop_.clone(),
                    next: Arc::new(AtomicUsize::new(0)),
                    dispose,
                })
            },
        );
        dependencies.agents.set_factory(factory).unwrap();
        let approval = install_approval(&context, ApprovalConfig::default()).unwrap();
        let (server_io, client_io) = tokio::io::duplex(256 * 1024);
        let (server_read, server_write) = tokio::io::split(server_io);
        let (client_read, client_write) = tokio::io::split(client_io);
        let server_transport = JsonRpcLineTransport::new(server_read, server_write);
        let client_transport = JsonRpcLineTransport::new(client_read, client_write);
        let bridge_config = AcpBridgeConfig {
            provider: Some("mock".to_owned()),
            model: Some("model".to_owned()),
        };
        let bridge = match drain {
            Some(drain) => AcpBridge::new_with_continuable_drain(
                &context,
                &server_transport,
                bridge_config,
                drain,
            ),
            None => AcpBridge::new(&context, &server_transport, bridge_config),
        }
        .unwrap();
        let client = AcpClient::new(&client_transport, permission);
        let updates = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&updates);
        client.on_update(Arc::new(move |update| {
            observed.lock().push(update.clone());
        }));
        bridge.start();
        client.start();
        Self {
            context,
            dependencies,
            loop_,
            bridge,
            client,
            updates,
            approval,
            client_transport,
            requests,
        }
    }

    async fn initialize(&self) -> Value {
        self.client.initialize().await.unwrap()
    }

    async fn session(&self) -> seekdeep_acp::AcpSessionId {
        self.client
            .new_session(&std::env::current_dir().unwrap().to_string_lossy())
            .await
            .unwrap()
    }

    async fn dispose(self) {
        self.bridge.shutdown().await.unwrap();
        self.client.close();
        self.loop_.dispose().await.unwrap();
        self.approval.dispose().await.unwrap();
        self.context.fiber().dispose().await.unwrap();
    }

    fn agent(&self, session: &seekdeep_acp::AcpSessionId) -> Arc<Agent> {
        self.dependencies
            .agents
            .get(&SessionId::new(session.as_str()))
            .expect("owned ACP agent")
    }
}

async fn wait_running(agent: &Agent) {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while agent.status() != seekdeep_agent::AgentStatus::Running {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("agent reached running state");
}

async fn wait_message_text(harness: &Harness, expected: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let text = harness
                .updates
                .lock()
                .iter()
                .filter_map(|update| {
                    update
                        .update
                        .pointer("/content/text")
                        .and_then(Value::as_str)
                })
                .collect::<String>();
            if text == expected {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("expected committed ACP message text");
}

#[test]
fn codec_preserves_baseline_text_links_and_closed_reason_maps() {
    for (kind, expected) in [
        ("completed", AcpStopReason::EndTurn),
        ("max-tokens", AcpStopReason::EndTurn),
        ("aborted", AcpStopReason::EndTurn),
        ("interrupted", AcpStopReason::Cancelled),
        ("blocked", AcpStopReason::EndTurn),
        ("error", AcpStopReason::EndTurn),
    ] {
        assert_eq!(turn_end_to_stop_reason(kind), expected);
    }
    let prompt = vec![
        json!({"type":"text","text":"one"}),
        json!({"type":"resource_link","name":"a b","uri":"file:///x"}),
        json!({"type":"text","text":"two"}),
    ];
    assert_eq!(
        acp_prompt_to_text(&prompt),
        "one\n[resource_link name=\"a b\" uri=\"file:///x\"]\ntwo"
    );
    assert!(!prompt_has_unsupported_content(&prompt));
    assert!(prompt_has_unsupported_content(&[json!({"type":"image"})]));
    assert_eq!(acp_content_text(&json!({"type":"image"})), "");
    assert_eq!(
        to_acp_prompt(&[
            ContentBlock::Text { text: "x".into() },
            ContentBlock::Reasoning {
                text: "hidden".into()
            }
        ]),
        [json!({"type":"text","text":"x"})]
    );
    for (reason, expected) in [
        (AcpStopReason::EndTurn, SubagentStopReason::Completed),
        (AcpStopReason::MaxTokens, SubagentStopReason::MaxTokens),
        (AcpStopReason::Refusal, SubagentStopReason::Refusal),
        (AcpStopReason::Cancelled, SubagentStopReason::Aborted),
        (AcpStopReason::MaxTurnRequests, SubagentStopReason::Error),
        (
            AcpStopReason::Unknown("future".into()),
            SubagentStopReason::Error,
        ),
    ] {
        assert_eq!(acp_stop_reason(&reason), expected);
    }
}

#[tokio::test]
async fn negotiates_validates_and_runs_committed_text_to_whole_agent_idle() {
    let harness = Harness::new(
        vec![
            Behavior::Text("answer".to_owned()),
            Behavior::MaxTokens("partial".to_owned()),
        ],
        PermissionPolicy::Reject,
    );
    let init = harness.initialize().await;
    assert_eq!(init["protocolVersion"], json!(PROTOCOL_VERSION));
    assert_eq!(init["agentInfo"]["name"], "seekdeep-harness-acp");
    assert_eq!(
        init["agentCapabilities"],
        json!({"promptCapabilities":{"image":false,"audio":false,"embeddedContext":false}})
    );
    let invalid = harness.client.new_session("relative").await.unwrap_err();
    assert!(invalid.to_string().contains("absolute"));
    let session = harness.session().await;
    let first = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        harness.client.prompt(
            &session,
            vec![
                json!({"type":"text","text":"one"}),
                json!({"type":"text","text":"two"}),
            ],
        ),
    )
    .await
    .unwrap_or_else(|_| {
        let agent = harness
            .dependencies
            .agents
            .get(&SessionId::new(session.as_str()))
            .unwrap();
        panic!(
            "prompt timed out: status={:?} events={:?}",
            agent.status(),
            agent
                .session()
                .events()
                .iter()
                .map(|event| (&event.event_type, &event.data))
                .collect::<Vec<_>>()
        )
    })
    .unwrap();
    assert_eq!(first, AcpStopReason::EndTurn);
    let second = harness
        .client
        .prompt(&session, vec![json!({"type":"text","text":"again"})])
        .await
        .unwrap();
    assert_eq!(second, AcpStopReason::EndTurn);
    for _ in 0..100 {
        if harness.updates.lock().len() >= 2 {
            break;
        }
        tokio::task::yield_now().await;
    }
    {
        let updates = harness.updates.lock();
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].session_id, session);
        assert_eq!(
            updates[0].update.pointer("/content/text"),
            Some(&json!("answer"))
        );
        assert_eq!(
            updates[1].update.pointer("/content/text"),
            Some(&json!("partial"))
        );
    }
    let agent = harness
        .dependencies
        .agents
        .get(&SessionId::new(session.as_str()))
        .unwrap();
    let events = agent.session().events();
    let user_texts = events
        .iter()
        .filter(|event| {
            event.event_type == "user/message"
                && event.data.pointer("/source/kind").and_then(Value::as_str) == Some("user")
        })
        .filter_map(|event| {
            event
                .data
                .pointer("/content/0/text")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect::<Vec<_>>();
    assert_eq!(user_texts, ["onetwo".to_owned(), "again".to_owned()]);
    harness.dispose().await;
}

#[tokio::test]
async fn isolates_concurrent_sessions_one_prompt_slot_and_cancellation() {
    let harness = Harness::new(
        vec![Behavior::Hang, Behavior::Text("B done".to_owned())],
        PermissionPolicy::Reject,
    );
    harness.initialize().await;
    let a = harness.session().await;
    let b = harness.session().await;
    let client = Arc::clone(&harness.client);
    let a_for_task = a.clone();
    let pending_a = tokio::spawn(async move {
        client
            .prompt(&a_for_task, vec![json!({"type":"text","text":"hang"})])
            .await
    });
    for _ in 0..1_000 {
        if harness
            .dependencies
            .agents
            .get(&SessionId::new(a.as_str()))
            .is_some_and(|agent| agent.status() == seekdeep_agent::AgentStatus::Running)
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    assert_eq!(
        harness
            .dependencies
            .agents
            .get(&SessionId::new(a.as_str()))
            .map(|agent| agent.status()),
        Some(seekdeep_agent::AgentStatus::Running)
    );
    let duplicate = harness
        .client
        .prompt(&a, vec![json!({"type":"text","text":"again"})])
        .await
        .unwrap_err();
    assert!(duplicate.to_string().contains("already in flight"));
    assert_eq!(
        harness
            .client
            .prompt(&b, vec![json!({"type":"text","text":"go B"})])
            .await
            .unwrap(),
        AcpStopReason::EndTurn
    );
    harness.client.cancel(&a).await.unwrap();
    assert_eq!(pending_a.await.unwrap().unwrap(), AcpStopReason::Cancelled);
    assert_eq!(
        harness
            .updates
            .lock()
            .iter()
            .find(|update| update.session_id == b)
            .and_then(|update| update.update.pointer("/content/text")),
        Some(&json!("B done"))
    );
    harness.dispose().await;
}

#[tokio::test]
async fn routes_one_shot_permissions_only_for_exact_owned_agents_and_call_ids() {
    for (policy, expected) in [
        (PermissionPolicy::Reject, ApprovalOutcome::Cancelled),
        (PermissionPolicy::Allow, ApprovalOutcome::AllowedOnce),
    ] {
        let harness = Harness::new(Vec::new(), policy);
        harness.initialize().await;
        let session_id = harness.session().await;
        let agent = harness
            .dependencies
            .agents
            .get(&SessionId::new(session_id.as_str()))
            .unwrap();
        agent
            .session()
            .append("turn/start", json!({"turn":0}), AppendOptions::default())
            .unwrap();
        let outcome = harness
            .approval
            .request(
                ApprovalRequest::new(Arc::clone(&agent), "bash")
                    .with_call_id(CallId::new("call-1")),
            )
            .await
            .unwrap();
        assert_eq!(outcome, expected);
        let without_call = harness
            .approval
            .request(ApprovalRequest::new(Arc::clone(&agent), "bash"))
            .await
            .unwrap();
        assert_eq!(without_call, ApprovalOutcome::Unavailable);
        let foreign_session = Session::create(
            agent.id(),
            None,
            Some(SessionHeader::new(agent.id().clone())),
        )
        .unwrap();
        foreign_session
            .append("turn/start", json!({"turn":0}), AppendOptions::default())
            .unwrap();
        let foreign = Arc::new(Agent::new(
            agent.id().clone(),
            AgentOptions::default(),
            Arc::clone(&foreign_session),
            Arc::new(Inbox::new(foreign_session, Arc::new(NoopInboxNotifications)).unwrap()),
            Context::new(),
            ScopeKey::new(),
        ));
        let foreign_outcome = harness
            .approval
            .request(ApprovalRequest::new(foreign, "bash").with_call_id(CallId::new("call-1")))
            .await
            .unwrap();
        assert_eq!(foreign_outcome, ApprovalOutcome::Unavailable);
        harness.dispose().await;
    }
}

#[tokio::test]
async fn permission_choices_cancel_unknown_and_client_failure_all_fail_closed() {
    let harness = Harness::new(Vec::new(), PermissionPolicy::Reject);
    harness.initialize().await;
    let session_id = harness.session().await;
    let agent = harness.agent(&session_id);
    agent
        .session()
        .append("turn/start", json!({"turn":0}), AppendOptions::default())
        .unwrap();

    let observed = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = Arc::clone(&observed);
    harness
        .client_transport
        .on_request(Arc::new(move |method, params| {
            assert_eq!(method, client_methods::SESSION_REQUEST_PERMISSION);
            captured.lock().push(Value::Object(params));
            Box::pin(async {
                Ok(json!({"outcome":{"outcome":"selected","optionId":"reject-once"}}))
            })
        }));
    let request =
        || ApprovalRequest::new(Arc::clone(&agent), "bash").with_call_id(CallId::new("call-9"));
    assert_eq!(
        harness.approval.request(request()).await.unwrap(),
        ApprovalOutcome::Rejected
    );
    {
        let request_params = observed.lock();
        assert_eq!(request_params.len(), 1);
        assert_eq!(request_params[0]["sessionId"], session_id.as_str());
        assert_eq!(
            request_params[0]["toolCall"],
            json!({"toolCallId":"call-9"})
        );
        assert_eq!(
            request_params[0]["options"],
            json!([
                {"optionId":"allow-once","name":"Allow once","kind":"allow_once"},
                {"optionId":"reject-once","name":"Reject","kind":"reject_once"}
            ])
        );
    }

    harness.client_transport.on_request(Arc::new(|_, _| {
        Box::pin(async { Ok(json!({"outcome":{"outcome":"cancelled"}})) })
    }));
    assert_eq!(
        harness.approval.request(request()).await.unwrap(),
        ApprovalOutcome::Cancelled
    );

    harness.client_transport.on_request(Arc::new(|_, _| {
        Box::pin(async { Ok(json!({"outcome":{"outcome":"selected","optionId":"unknown-grant"}})) })
    }));
    assert_eq!(
        harness.approval.request(request()).await.unwrap(),
        ApprovalOutcome::Rejected
    );

    harness.client_transport.on_request(Arc::new(|_, _| {
        Box::pin(async { anyhow::bail!("client gone") })
    }));
    assert_eq!(
        harness.approval.request(request()).await.unwrap(),
        ApprovalOutcome::Unavailable
    );
    harness.dispose().await;
}

#[tokio::test]
async fn automation_output_excludes_tools_reasoning_trace_and_foreign_agents() {
    let harness = Harness::new(
        vec![
            Behavior::ToolCall,
            Behavior::ReasoningText {
                reasoning: "hidden chain".to_owned(),
                text: "done".to_owned(),
            },
        ],
        PermissionPolicy::Reject,
    );
    harness
        .dependencies
        .tools
        .register(
            &harness.context,
            define_content_tool_fixture(ContentToolFixtureOptions::new(
                "echo",
                "Return a deterministic result.",
                json!({}),
                Arc::new(|_: Value, _| {
                    Box::pin(async {
                        Ok(vec![ContentBlock::Text {
                            text: "tool result".to_owned(),
                        }])
                    })
                }),
            ))
            .unwrap(),
        )
        .unwrap();
    harness.initialize().await;
    let session = harness.session().await;
    let agent = harness.agent(&session);
    for event_type in ["terminal/output", "plan/update", "session/title"] {
        agent
            .session()
            .append(
                event_type,
                json!({"presentation":"hidden"}),
                AppendOptions::default(),
            )
            .unwrap();
    }
    assert!(harness.updates.lock().is_empty());
    assert_eq!(
        harness
            .client
            .prompt(&session, vec![json!({"type":"text","text":"go"})])
            .await
            .unwrap(),
        AcpStopReason::EndTurn
    );
    wait_message_text(&harness, "done").await;
    assert_eq!(harness.updates.lock().len(), 1);
    let events = agent.session().events();
    assert!(events.iter().any(|event| event.event_type == "tool/result"));
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "assistant/chunk")
    );
    harness.dispose().await;

    let foreign_harness = Harness::new(
        vec![Behavior::Text("foreign".to_owned())],
        PermissionPolicy::Reject,
    );
    foreign_harness.initialize().await;
    let _owned = foreign_harness.session().await;
    let mut options = seekdeep_agent::CreateAgentOptions::new(SessionId::new("foreign"));
    options.agent_options = AgentOptions {
        provider: Some("mock".into()),
        model: Some("model".into()),
        max_tokens: None,
        subagent_depth: None,
    };
    let foreign = foreign_harness
        .dependencies
        .agents
        .create(options)
        .await
        .unwrap();
    foreign
        .agent
        .followup(UserMessage::new(
            vec![ContentBlock::Text {
                text: "autonomous".to_owned(),
            }],
            MessageSource::plugin("test"),
        ))
        .unwrap();
    foreign.agent.when_idle().unwrap().await.unwrap();
    assert!(foreign_harness.updates.lock().is_empty());
    foreign.dispose().await.unwrap();
    foreign_harness.dispose().await;
}

#[tokio::test]
async fn notification_consumers_cannot_fail_prompt_settlement() {
    let harness = Harness::new(
        vec![Behavior::Text("answer".to_owned())],
        PermissionPolicy::Reject,
    );
    harness.initialize().await;
    let session = harness.session().await;
    let notification_seen = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&notification_seen);
    harness
        .client_transport
        .on_notification(Arc::new(move |method, _| {
            assert_eq!(method, client_methods::SESSION_UPDATE);
            seen.store(true, Ordering::Release);
        }));
    assert_eq!(
        harness
            .client
            .prompt(&session, vec![json!({"type":"text","text":"go"})])
            .await
            .unwrap(),
        AcpStopReason::EndTurn
    );
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !notification_seen.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("session/update delivered as a notification");
    harness.dispose().await;
}

#[tokio::test]
async fn committed_images_are_explicit_and_failed_partial_turns_publish_nothing() {
    let attachment_id = format!("sha256:{}", "a".repeat(64));
    let harness = Harness::new(
        vec![Behavior::Image(attachment_id.clone())],
        PermissionPolicy::Reject,
    );
    harness.initialize().await;
    let session = harness.session().await;
    assert_eq!(
        harness
            .client
            .prompt(&session, vec![json!({"type":"text","text":"show it"})])
            .await
            .unwrap(),
        AcpStopReason::EndTurn
    );
    wait_message_text(&harness, &format!("[image attachment {attachment_id}]")).await;
    harness.dispose().await;

    let failed = Harness::new(
        vec![Behavior::Error {
            partial: Some("must not escape".to_owned()),
            message: "provider boom".to_owned(),
        }],
        PermissionPolicy::Reject,
    );
    failed.initialize().await;
    let session = failed.session().await;
    let error = failed
        .client
        .prompt(&session, vec![json!({"type":"text","text":"go"})])
        .await
        .unwrap_err();
    assert!(error.to_string().contains("turn failed: provider boom"));
    assert!(failed.updates.lock().is_empty());
    failed.dispose().await;
}

#[tokio::test]
async fn pre_step_rewrite_preserves_prompt_correlation() {
    let rewritten = Harness::new(
        vec![Behavior::Text("rewritten answer".to_owned())],
        PermissionPolicy::Reject,
    );
    rewritten
        .context
        .events()
        .on_waterfall(
            &rewritten.context,
            "agent/pre-step",
            |_, _, _| {
                Box::pin(async {
                    Ok(EventReply::Value(Arc::new(PreStepDecision::Enter {
                        messages: vec![UserMessage::new(
                            vec![ContentBlock::Text {
                                text: "rewritten prompt".to_owned(),
                            }],
                            MessageSource::plugin("test"),
                        )],
                    })))
                })
            },
            EventOptions::default(),
        )
        .unwrap();
    rewritten.initialize().await;
    let session = rewritten.session().await;
    assert_eq!(
        rewritten
            .client
            .prompt(
                &session,
                vec![json!({"type":"text","text":"original prompt"})],
            )
            .await
            .unwrap(),
        AcpStopReason::EndTurn
    );
    {
        let requests = rewritten.requests.lock();
        assert_eq!(requests.len(), 1);
        let request_text = requests[0]
            .messages
            .iter()
            .flat_map(seekdeep_llm::Message::content)
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(request_text.contains("rewritten prompt"));
        assert!(!request_text.contains("original prompt"));
    }
    rewritten.dispose().await;
}

#[tokio::test]
async fn pre_step_rejection_settles_without_streaming() {
    let rejected = Harness::new(Vec::new(), PermissionPolicy::Reject);
    rejected
        .context
        .events()
        .on_waterfall(
            &rejected.context,
            "agent/pre-step",
            |_, _, _| Box::pin(async { Ok(EventReply::Value(Arc::new(PreStepDecision::Reject))) }),
            EventOptions::default(),
        )
        .unwrap();
    rejected.initialize().await;
    let session = rejected.session().await;
    assert_eq!(
        rejected
            .client
            .prompt(&session, vec![json!({"type":"text","text":"blocked"})])
            .await
            .unwrap(),
        AcpStopReason::EndTurn
    );
    assert!(rejected.updates.lock().is_empty());
    rejected.dispose().await;
}

#[tokio::test]
async fn pre_step_failure_rejects_the_correlated_prompt() {
    let failed = Harness::new(
        vec![Behavior::Text("must not run".to_owned())],
        PermissionPolicy::Reject,
    );
    failed
        .context
        .events()
        .on_waterfall(
            &failed.context,
            "agent/pre-step",
            |_, _, _| Box::pin(async { anyhow::bail!("plugin pre-step failed") }),
            EventOptions::default(),
        )
        .unwrap();
    failed.initialize().await;
    let session = failed.session().await;
    let error = failed
        .client
        .prompt(&session, vec![json!({"type":"text","text":"go"})])
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("turn failed: plugin pre-step failed")
    );
    failed.dispose().await;
}

#[tokio::test]
async fn retry_adopts_the_prompt_and_terminal_failure_rejects_once() {
    let retried = Harness::new(
        vec![
            Behavior::Error {
                partial: None,
                message: "transient boom".to_owned(),
            },
            Behavior::Text("recovered".to_owned()),
        ],
        PermissionPolicy::Reject,
    );
    let offered = Arc::new(AtomicBool::new(false));
    let once = Arc::clone(&offered);
    retried
        .context
        .events()
        .on_waterfall(
            &retried.context,
            "agent/request-error",
            move |_, _, next| {
                let once = Arc::clone(&once);
                Box::pin(async move {
                    if once.swap(true, Ordering::AcqRel) {
                        next.run().await
                    } else {
                        Ok(EventReply::Value(Arc::new(RequestErrorAction::Retry)))
                    }
                })
            },
            EventOptions::default(),
        )
        .unwrap();
    retried.initialize().await;
    let session = retried.session().await;
    assert_eq!(
        retried
            .client
            .prompt(&session, vec![json!({"type":"text","text":"go"})])
            .await
            .unwrap(),
        AcpStopReason::EndTurn
    );
    assert!(offered.load(Ordering::Acquire));
    wait_message_text(&retried, "recovered").await;
    retried.dispose().await;
}

#[tokio::test]
async fn cancellation_distinguishes_client_hook_idle_and_autonomous_work() {
    let client_cancelled = Harness::new(vec![Behavior::Hang], PermissionPolicy::Reject);
    client_cancelled.initialize().await;
    let session = client_cancelled.session().await;
    let agent = client_cancelled.agent(&session);
    let client = Arc::clone(&client_cancelled.client);
    let id = session.clone();
    let prompt = tokio::spawn(async move {
        client
            .prompt(&id, vec![json!({"type":"text","text":"go"})])
            .await
    });
    wait_running(&agent).await;
    client_cancelled.client.cancel(&session).await.unwrap();
    assert_eq!(prompt.await.unwrap().unwrap(), AcpStopReason::Cancelled);
    agent.when_idle().unwrap().await.unwrap();
    assert_eq!(
        agent
            .session()
            .events()
            .iter()
            .rev()
            .find(|event| event.event_type == "turn/end")
            .unwrap()
            .data["reason"],
        json!({"kind":"aborted","reason":{"kind":"user"}})
    );
    client_cancelled.dispose().await;

    let hook_cancelled = Harness::new(vec![Behavior::Hang], PermissionPolicy::Reject);
    hook_cancelled.initialize().await;
    let session = hook_cancelled.session().await;
    let agent = hook_cancelled.agent(&session);
    let client = Arc::clone(&hook_cancelled.client);
    let id = session.clone();
    let prompt = tokio::spawn(async move {
        client
            .prompt(&id, vec![json!({"type":"text","text":"go"})])
            .await
    });
    wait_running(&agent).await;
    agent
        .cancel(
            AgentCancelCause::Hook {
                reason: "owner intervention".to_owned(),
            },
            CancelOptions::default(),
        )
        .unwrap();
    assert_eq!(prompt.await.unwrap().unwrap(), AcpStopReason::EndTurn);
    hook_cancelled.dispose().await;

    let autonomous = Harness::new(vec![Behavior::Hang], PermissionPolicy::Reject);
    autonomous.initialize().await;
    let session = autonomous.session().await;
    let agent = autonomous.agent(&session);
    agent
        .followup(UserMessage::new(
            vec![ContentBlock::Text {
                text: "autonomous work".to_owned(),
            }],
            MessageSource::plugin("test"),
        ))
        .unwrap();
    wait_running(&agent).await;
    autonomous.client.cancel(&session).await.unwrap();
    agent.when_idle().unwrap().await.unwrap();
    assert_eq!(
        agent
            .session()
            .events()
            .iter()
            .rev()
            .find(|event| event.event_type == "turn/end")
            .unwrap()
            .data["reason"],
        json!({"kind":"aborted","reason":{"kind":"user"}})
    );
    autonomous.dispose().await;

    let idle = Harness::new(
        vec![Behavior::Text("answer".to_owned())],
        PermissionPolicy::Reject,
    );
    idle.initialize().await;
    let session = idle.session().await;
    idle.client.cancel(&session).await.unwrap();
    assert_eq!(
        idle.client
            .prompt(&session, vec![json!({"type":"text","text":"go"})])
            .await
            .unwrap(),
        AcpStopReason::EndTurn
    );
    wait_message_text(&idle, "answer").await;
    idle.dispose().await;
}

#[tokio::test]
async fn cancelled_turn_cannot_settle_the_next_prompt_generation() {
    let harness = Harness::new(
        vec![Behavior::Hang, Behavior::Text("next".to_owned())],
        PermissionPolicy::Reject,
    );
    harness.initialize().await;
    let session = harness.session().await;
    let agent = harness.agent(&session);
    let client = Arc::clone(&harness.client);
    let id = session.clone();
    let first = tokio::spawn(async move {
        client
            .prompt(&id, vec![json!({"type":"text","text":"one"})])
            .await
    });
    wait_running(&agent).await;
    harness.client.cancel(&session).await.unwrap();
    assert_eq!(first.await.unwrap().unwrap(), AcpStopReason::Cancelled);
    assert_eq!(
        harness
            .client
            .prompt(&session, vec![json!({"type":"text","text":"two"})])
            .await
            .unwrap(),
        AcpStopReason::EndTurn
    );
    wait_message_text(&harness, "next").await;
    harness.dispose().await;
}

#[tokio::test]
async fn removed_and_preclaim_failed_prompts_settle_without_wedging_the_slot() {
    let removed = Harness::new(
        vec![Behavior::Text("unclaimed work".to_owned())],
        PermissionPolicy::Reject,
    );
    removed
        .context
        .events()
        .on_sync(
            &removed.context,
            "agent/inbox/inserted",
            |_, args| {
                let event = args
                    .get::<AgentEvent<AgentInboxMessage>>(0)
                    .ok_or_else(|| anyhow::anyhow!("missing inbox event"))?;
                event.agent.inbox().remove(event.payload.message.id())?;
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    removed.initialize().await;
    let session = removed.session().await;
    assert_eq!(
        removed
            .client
            .prompt(&session, vec![json!({"type":"text","text":"go"})])
            .await
            .unwrap(),
        AcpStopReason::Cancelled
    );
    removed.dispose().await;

    let failed = Harness::new(
        vec![Behavior::Text("unclaimed work".to_owned())],
        PermissionPolicy::Reject,
    );
    let context = failed.context.clone();
    failed
        .context
        .events()
        .on_sync(
            &failed.context,
            "agent/inbox/inserted",
            move |_, args| {
                let event = args
                    .get::<AgentEvent<AgentInboxMessage>>(0)
                    .ok_or_else(|| anyhow::anyhow!("missing inbox event"))?;
                event.agent.inbox().remove(event.payload.message.id())?;
                AgentEvents::new(context.clone(), Arc::clone(&event.agent)).emit(
                    "agent/error",
                    AgentErrorEvent {
                        turn: 1,
                        step: 0,
                        error: "turn start unavailable".to_owned(),
                    },
                );
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    failed.initialize().await;
    let session = failed.session().await;
    let error = failed
        .client
        .prompt(&session, vec![json!({"type":"text","text":"go"})])
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("turn failed: turn start unavailable")
    );
    failed.dispose().await;
}

#[tokio::test]
async fn synchronous_injection_and_autonomous_work_do_not_steal_prompt_identity() {
    let injected = Harness::new(
        vec![Behavior::Text("real answer".to_owned())],
        PermissionPolicy::Reject,
    );
    let inserted = Arc::new(AtomicBool::new(false));
    let once = Arc::clone(&inserted);
    injected
        .context
        .events()
        .on_sync(
            &injected.context,
            "agent/inbox/inserted",
            move |_, args| {
                let event = args
                    .get::<AgentEvent<AgentInboxMessage>>(0)
                    .ok_or_else(|| anyhow::anyhow!("missing inbox event"))?;
                if event.payload.message.source().kind == "user"
                    && !once.swap(true, Ordering::AcqRel)
                {
                    event.agent.inject(UserMessage::new(
                        vec![ContentBlock::Text {
                            text: "context".to_owned(),
                        }],
                        MessageSource::plugin("test"),
                    ))?;
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    injected.initialize().await;
    let session = injected.session().await;
    assert_eq!(
        injected
            .client
            .prompt(&session, vec![json!({"type":"text","text":"go"})])
            .await
            .unwrap(),
        AcpStopReason::EndTurn
    );
    assert!(inserted.load(Ordering::Acquire));
    wait_message_text(&injected, "real answer").await;
    injected.dispose().await;

    let autonomous = Harness::new(vec![Behavior::Hang], PermissionPolicy::Reject);
    autonomous.initialize().await;
    let session = autonomous.session().await;
    let agent = autonomous.agent(&session);
    agent
        .followup(UserMessage::new(
            vec![ContentBlock::Text {
                text: "autonomous work".to_owned(),
            }],
            MessageSource::plugin("test"),
        ))
        .unwrap();
    wait_running(&agent).await;
    let client = Arc::clone(&autonomous.client);
    let id = session.clone();
    let prompt = tokio::spawn(async move {
        client
            .prompt(&id, vec![json!({"type":"text","text":"go"})])
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while agent
            .session()
            .events()
            .iter()
            .filter(|event| event.event_type == "agent/inbox/spliced")
            .count()
            < 2
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("client prompt queued behind autonomous work");
    assert!(!prompt.is_finished());
    autonomous.client.cancel(&session).await.unwrap();
    assert_eq!(prompt.await.unwrap().unwrap(), AcpStopReason::Cancelled);
    autonomous.dispose().await;
}

#[tokio::test]
async fn peer_session_listener_failures_do_not_hide_turn_settlement() {
    let harness = Harness::new(
        vec![Behavior::Text("answer".to_owned())],
        PermissionPolicy::Reject,
    );
    let failures = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = Arc::clone(&failures);
    harness
        .context
        .events()
        .on_sync(
            &harness.context,
            "session/event",
            move |_, args| {
                let event = args
                    .get::<SessionEvent>(1)
                    .ok_or_else(|| anyhow::anyhow!("missing session event"))?;
                if matches!(event.event_type.as_str(), "turn/start" | "turn/end") {
                    observed.fetch_add(1, Ordering::AcqRel);
                    anyhow::bail!("peer listener boom");
                }
                Ok(EventReply::Undefined)
            },
            EventOptions {
                prepend: true,
                global: false,
            },
        )
        .unwrap();
    harness.initialize().await;
    let session = harness.session().await;
    assert_eq!(
        harness
            .client
            .prompt(&session, vec![json!({"type":"text","text":"go"})])
            .await
            .unwrap(),
        AcpStopReason::EndTurn
    );
    assert!(failures.load(Ordering::Acquire) >= 2);
    harness.dispose().await;
}

#[tokio::test]
async fn externally_disposed_agents_fail_each_send_without_retaining_the_slot() {
    let harness = Harness::new(Vec::new(), PermissionPolicy::Reject);
    harness.initialize().await;
    let session = harness.session().await;
    harness.loop_.dispose().await.unwrap();
    for text in ["one", "two"] {
        let error = harness
            .client
            .prompt(&session, vec![json!({"type":"text","text":text})])
            .await
            .unwrap_err();
        assert!(error.to_string().contains("prompt was not queued"));
    }
    harness.dispose().await;
}

#[tokio::test]
async fn explicit_reload_is_idempotent_and_rejects_new_sessions_without_orphans() {
    let harness = Harness::new(Vec::new(), PermissionPolicy::Reject);
    harness.initialize().await;
    let (first, second) = tokio::join!(harness.bridge.shutdown(), harness.bridge.shutdown());
    first.unwrap();
    second.unwrap();
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        harness
            .client
            .new_session(&std::env::current_dir().unwrap().to_string_lossy()),
    )
    .await
    .expect("disposed bridge still answers the old connection")
    .unwrap_err();
    assert!(error.to_string().contains("disposed"));
    assert!(harness.dependencies.agents.list().is_empty());
    harness.dispose().await;
}

#[tokio::test]
async fn client_eof_cancels_and_disposes_owned_sessions_without_root_disposal() {
    let harness = Harness::new(vec![Behavior::Hang], PermissionPolicy::Reject);
    harness.initialize().await;
    let session = harness.session().await;
    let agent = harness.agent(&session);
    let client = Arc::clone(&harness.client);
    let id = session.clone();
    let prompt = tokio::spawn(async move {
        client
            .prompt(&id, vec![json!({"type":"text","text":"go"})])
            .await
    });
    wait_running(&agent).await;
    harness.client.shutdown_output().await.unwrap();
    assert_eq!(prompt.await.unwrap().unwrap(), AcpStopReason::Cancelled);
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while harness
            .dependencies
            .agents
            .get(&SessionId::new(session.as_str()))
            .is_some()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("connection close disposed the owned agent");
    assert_eq!(agent.status(), seekdeep_agent::AgentStatus::Idle);
    assert!(
        harness
            .dependencies
            .sessions
            .get(&SessionId::new(session.as_str()))
            .is_none()
    );

    let unrelated = harness
        .dependencies
        .agents
        .create(seekdeep_agent::CreateAgentOptions::new(SessionId::new(
            "unrelated-after-acp-close",
        )))
        .await
        .unwrap();
    unrelated.dispose().await.unwrap();
    harness.dispose().await;
}

#[tokio::test]
async fn disconnect_and_explicit_disposal_share_one_quiescence_boundary() {
    let harness = Harness::new(vec![Behavior::Hang], PermissionPolicy::Reject);
    harness.initialize().await;
    let session = harness.session().await;
    let agent = harness.agent(&session);
    let client = Arc::clone(&harness.client);
    let id = session.clone();
    let prompt = tokio::spawn(async move {
        client
            .prompt(&id, vec![json!({"type":"text","text":"go"})])
            .await
    });
    wait_running(&agent).await;
    let (closed, disposed) =
        tokio::join!(harness.client.shutdown_output(), harness.bridge.shutdown());
    closed.unwrap();
    disposed.unwrap();
    assert_eq!(prompt.await.unwrap().unwrap(), AcpStopReason::Cancelled);
    assert_eq!(agent.status(), seekdeep_agent::AgentStatus::Idle);
    assert!(
        harness
            .dependencies
            .agents
            .get(&SessionId::new(session.as_str()))
            .is_none()
    );
    harness.dispose().await;
}

#[tokio::test]
async fn shutdown_cancels_before_descendant_drain_and_contains_drain_failure() {
    let drain_started = Arc::new(AtomicBool::new(false));
    let release = Arc::new(tokio::sync::Notify::new());
    let drained_parents = Arc::new(Mutex::new(Vec::<Arc<Agent>>::new()));
    let started = Arc::clone(&drain_started);
    let release_drain = Arc::clone(&release);
    let observed_parents = Arc::clone(&drained_parents);
    let drain: AcpContinuableDrainHook = Arc::new(move |parents| {
        observed_parents.lock().extend(parents);
        let started = Arc::clone(&started);
        let release = Arc::clone(&release_drain);
        Box::pin(async move {
            started.store(true, Ordering::Release);
            release.notified().await;
            Ok(())
        })
    });
    let harness =
        Harness::new_with_drain(vec![Behavior::Hang], PermissionPolicy::Reject, Some(drain));
    harness.initialize().await;
    let session = harness.session().await;
    let agent = harness.agent(&session);
    let client = Arc::clone(&harness.client);
    let id = session.clone();
    let prompt = tokio::spawn(async move {
        client
            .prompt(&id, vec![json!({"type":"text","text":"go"})])
            .await
    });
    wait_running(&agent).await;
    let bridge = Arc::clone(&harness.bridge);
    let shutdown = tokio::spawn(async move { bridge.shutdown().await });
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !drain_started.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("descendant drain started");
    assert_eq!(agent.status(), seekdeep_agent::AgentStatus::Idle);
    assert!(
        harness
            .dependencies
            .agents
            .get(&SessionId::new(session.as_str()))
            .is_some()
    );
    assert!(Arc::ptr_eq(&drained_parents.lock()[0], &agent));
    assert_eq!(prompt.await.unwrap().unwrap(), AcpStopReason::Cancelled);
    release.notify_waiters();
    shutdown.await.unwrap().unwrap();
    assert!(
        harness
            .dependencies
            .agents
            .get(&SessionId::new(session.as_str()))
            .is_none()
    );
    harness.dispose().await;

    let failing_drain: AcpContinuableDrainHook =
        Arc::new(|_| Box::pin(async { anyhow::bail!("activation teardown failed") }));
    let failed = Harness::new_with_drain(Vec::new(), PermissionPolicy::Reject, Some(failing_drain));
    failed.initialize().await;
    let session = failed.session().await;
    failed.bridge.shutdown().await.unwrap();
    assert!(
        failed
            .dependencies
            .agents
            .get(&SessionId::new(session.as_str()))
            .is_none()
    );
    failed.dispose().await;
}

#[tokio::test]
async fn shutdown_awaits_parallel_session_disposal_and_aggregates_nested_diagnostics() {
    let second_started = Arc::new(AtomicBool::new(false));
    let release = Arc::new(tokio::sync::Notify::new());
    let started = Arc::clone(&second_started);
    let release_second = Arc::clone(&release);
    let dispose: AgentDisposeHook = Arc::new(move |index, handle| {
        let started = Arc::clone(&started);
        let release = Arc::clone(&release_second);
        Box::pin(async move {
            if index == 0 {
                handle.dispose().await?;
                anyhow::bail!(
                    "first session cleanup failed [scope cleanup failed: sqlite busy; hook cleanup failed]"
                );
            }
            started.store(true, Ordering::Release);
            release.notified().await;
            handle.dispose().await
        })
    });
    let harness =
        Harness::new_with_hooks(Vec::new(), PermissionPolicy::Reject, None, Some(dispose));
    harness.initialize().await;
    let first = harness.session().await;
    let second = harness.session().await;
    let bridge = Arc::clone(&harness.bridge);
    let shutdown = tokio::spawn(async move { bridge.shutdown().await });
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !second_started.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("second session disposal started in parallel");
    assert!(!shutdown.is_finished());
    release.notify_waiters();
    let error = shutdown.await.unwrap().unwrap_err();
    let detail = error.to_string();
    assert!(detail.contains("ACP agent teardown failed for 1 session(s)"));
    assert!(detail.contains("first session cleanup failed"));
    assert!(detail.contains("scope cleanup failed: sqlite busy"));
    assert!(detail.contains("hook cleanup failed"));
    for id in [first, second] {
        assert!(
            harness
                .dependencies
                .agents
                .get(&SessionId::new(id.as_str()))
                .is_none()
        );
    }
    harness.client.close();
    harness.loop_.dispose().await.unwrap();
    harness.approval.dispose().await.unwrap();
    harness.context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn bridge_shutdown_cancels_prompts_and_disposes_every_owned_session() {
    let harness = Harness::new(
        vec![Behavior::Hang, Behavior::Hang],
        PermissionPolicy::Reject,
    );
    harness.initialize().await;
    let a = harness.session().await;
    let b = harness.session().await;
    let first_client = Arc::clone(&harness.client);
    let first_id = a.clone();
    let first = tokio::spawn(async move {
        first_client
            .prompt(&first_id, vec![json!({"type":"text","text":"A"})])
            .await
    });
    let second_client = Arc::clone(&harness.client);
    let second_id = b.clone();
    let second = tokio::spawn(async move {
        second_client
            .prompt(&second_id, vec![json!({"type":"text","text":"B"})])
            .await
    });
    for _ in 0..100 {
        if harness.dependencies.agents.list().len() == 2
            && harness
                .dependencies
                .agents
                .list()
                .iter()
                .all(|agent| agent.status() == seekdeep_agent::AgentStatus::Running)
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    harness.bridge.shutdown().await.unwrap();
    assert_eq!(first.await.unwrap().unwrap(), AcpStopReason::Cancelled);
    assert_eq!(second.await.unwrap().unwrap(), AcpStopReason::Cancelled);
    assert!(
        harness
            .dependencies
            .agents
            .get(&SessionId::new(a.as_str()))
            .is_none()
    );
    assert!(
        harness
            .dependencies
            .agents
            .get(&SessionId::new(b.as_str()))
            .is_none()
    );
    harness.loop_.dispose().await.unwrap();
    harness.approval.dispose().await.unwrap();
    harness.context.fiber().dispose().await.unwrap();
}
