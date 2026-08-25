//! Codec, wire negotiation, prompt, multi-session, approval, and teardown parity.

use std::{collections::VecDeque, sync::Arc};

use async_trait::async_trait;
use futures::stream;
use parking_lot::Mutex;
use seekdeep_acp::{
    AcpBridge, AcpBridgeConfig, AcpClient, AcpStopReason, PROTOCOL_VERSION, PermissionPolicy,
    acp_content_text, acp_prompt_to_text, acp_stop_reason, prompt_has_unsupported_content,
    to_acp_prompt, turn_end_to_stop_reason,
};
use seekdeep_agent::{Agent, AgentOptions, Inbox, NoopInboxNotifications};
use seekdeep_agent_loop::{AgentLoop, AgentLoopServices, DEFAULT_MAX_PARALLEL_TOOL_CALLS};
use seekdeep_cordis::Context;
use seekdeep_core::session::{AppendOptions, Session, SessionHeader, SessionId};
use seekdeep_llm::{
    AdapterStream, CallId, ContentBlock, FinishReason, GenerateOptions, LlmAdapter, LlmFailure,
    StreamChunk,
};
use seekdeep_scope::ScopeKey;
use seekdeep_sdk_protocol::JsonRpcLineTransport;
use seekdeep_subagent::SubagentStopReason;
use seekdeep_user_approval::{
    ApprovalConfig, ApprovalOutcome, ApprovalRequest, install as install_approval,
};
use serde_json::{Value, json};

#[derive(Clone, Debug)]
enum Behavior {
    Text(String),
    MaxTokens(String),
    Hang,
}

#[derive(Debug)]
struct ScriptAdapter {
    behaviors: Mutex<VecDeque<Behavior>>,
}

#[async_trait]
impl LlmAdapter for ScriptAdapter {
    fn stream(&self, options: GenerateOptions) -> AdapterStream {
        match self.behaviors.lock().pop_front().expect("script exhausted") {
            Behavior::Text(text) => AdapterStream::new(stream::iter([
                Ok(StreamChunk::TextDelta { index: 0, text }),
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
}

impl Harness {
    fn new(behaviors: Vec<Behavior>, permission: PermissionPolicy) -> Self {
        let context = Context::new();
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
        dependencies
            .agents
            .set_factory(Arc::new(loop_.clone()))
            .unwrap();
        let approval = install_approval(&context, ApprovalConfig::default()).unwrap();
        let (server_io, client_io) = tokio::io::duplex(256 * 1024);
        let (server_read, server_write) = tokio::io::split(server_io);
        let (client_read, client_write) = tokio::io::split(client_io);
        let server_transport = JsonRpcLineTransport::new(server_read, server_write);
        let client_transport = JsonRpcLineTransport::new(client_read, client_write);
        let bridge = AcpBridge::new(
            &context,
            &server_transport,
            AcpBridgeConfig {
                provider: Some("mock".to_owned()),
                model: Some("model".to_owned()),
            },
        )
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
        self.loop_.dispose().await.unwrap();
        self.approval.dispose().await.unwrap();
        self.context.fiber().dispose().await.unwrap();
    }
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
