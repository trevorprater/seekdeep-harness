//! Production interaction cases ported from the question and approval `ApiProxy` suites.

use std::{sync::Arc, time::Duration};

use futures::{FutureExt as _, StreamExt as _, future::BoxFuture};
use seekdeep_agent::{Agent, AgentOptions, AgentRegistry, Inbox, NoopInboxNotifications};
use seekdeep_client_connection::{HttpResponse, RpcError, RpcResult};
use seekdeep_cordis::{Context, EventArgs, EventReply, Fiber};
use seekdeep_core::session::{AppendOptions, Session, SessionId};
use seekdeep_host_apiproxy::{
    ApiDownlinkStream, ApiProxyRuntime, ClientResponse, InteractionApiProxyRuntime, RpcId,
    RpcMethod, RpcReceipt, RpcReceiptReason, RpcRequest, RpcResponse,
    api::{
        downloads::SessionLogQuery,
        events::{HostFrame, MuxFrame},
    },
};
use seekdeep_llm::{AbortSignal, CallId};
use seekdeep_scope::{ScopeKey, scope_target, scoped_event_args};
use seekdeep_user_approval::{
    ApprovalAnswer, ApprovalConfig, ApprovalOutcome, ApprovalRequest, ApprovalRequestId,
    ApprovalService,
};
use seekdeep_user_questions::{
    AskUserQuestionAnswerItem, AskUserQuestionItem, AskUserQuestionOption, AskUserQuestionRequest,
    UserQuestionError, UserQuestionService, install as install_questions,
};
use serde_json::{Value, json};

#[derive(Debug)]
struct TerminalDomains;

impl ApiProxyRuntime for TerminalDomains {
    fn unary(
        &self,
        _method: RpcMethod,
        request: RpcRequest<Value>,
        _signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<RpcResponse<Value>>> {
        async move {
            Ok(RpcResponse::new(
                request.rpc_id,
                RpcResult::Success { value: None },
            ))
        }
        .boxed()
    }

    fn respond(
        &self,
        _message: ClientResponse,
        _signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<RpcReceipt>> {
        async {
            Ok(RpcReceipt::Rejected {
                reason: RpcReceiptReason::NotPending,
            })
        }
        .boxed()
    }

    fn mux(
        &self,
        _request: RpcRequest<Value>,
        _signal: AbortSignal,
    ) -> ApiDownlinkStream<MuxFrame> {
        futures::stream::empty().boxed()
    }

    fn host(
        &self,
        _request: RpcRequest<Value>,
        _signal: AbortSignal,
    ) -> ApiDownlinkStream<HostFrame> {
        futures::stream::empty().boxed()
    }

    fn session_log(
        &self,
        _query: SessionLogQuery,
        _signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<HttpResponse>> {
        async { Ok(HttpResponse::text(501, "not used")) }.boxed()
    }
}

struct Harness {
    context: Context,
    agents: Arc<AgentRegistry>,
    questions: Arc<UserQuestionService>,
    approval: Arc<ApprovalService>,
    runtime: Arc<InteractionApiProxyRuntime>,
}

impl Harness {
    fn new() -> Self {
        let context = Context::new();
        let agents = Arc::new(AgentRegistry::new(context.clone()));
        agents.provide(&context).unwrap();
        let questions = install_questions(&context).unwrap();
        let approval = ApprovalService::new(context.clone(), ApprovalConfig::default());
        approval.provide(&context).unwrap();
        let runtime =
            InteractionApiProxyRuntime::from_context(&context, Arc::new(TerminalDomains)).unwrap();
        Self {
            context,
            agents,
            questions,
            approval,
            runtime,
        }
    }

    fn agent(&self, id: &str) -> Arc<Agent> {
        let id = SessionId::new(id);
        let session = Session::create(&id, None, None).unwrap();
        let inbox =
            Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
        let agent = Arc::new(Agent::new(
            id,
            AgentOptions::default(),
            session,
            inbox,
            self.context.clone(),
            ScopeKey::new(),
        ));
        self.agents.register(&self.context, &agent, None).unwrap();
        agent
    }
}

fn open_turn(agent: &Agent) {
    agent
        .session()
        .append("turn/start", json!({ "turn": 1 }), AppendOptions::default())
        .unwrap();
}

fn question(id: &str, multi_select: bool) -> AskUserQuestionItem {
    AskUserQuestionItem {
        id: id.to_owned(),
        question: "Choose a target".to_owned(),
        detail: None,
        header: None,
        options: Some(vec![
            AskUserQuestionOption {
                label: "Code".to_owned(),
                description: None,
            },
            AskUserQuestionOption {
                label: "Docs".to_owned(),
                description: None,
            },
        ]),
        multi_select: Some(multi_select),
        intent: None,
    }
}

fn question_request(
    agent: Arc<Agent>,
    question: AskUserQuestionItem,
    signal: Option<AbortSignal>,
) -> AskUserQuestionRequest {
    AskUserQuestionRequest {
        questions: vec![question],
        agent: Some(agent),
        signal,
    }
}

fn open_mux(
    runtime: &InteractionApiProxyRuntime,
    signal: AbortSignal,
) -> ApiDownlinkStream<MuxFrame> {
    runtime.mux(RpcRequest::new(RpcId::new("mux-test"), json!({})), signal)
}

async fn next_matching(
    stream: &mut ApiDownlinkStream<MuxFrame>,
    predicate: impl Fn(&MuxFrame) -> bool,
) -> RpcRequest<MuxFrame> {
    tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(envelope) = stream.next().await {
            let envelope = envelope.unwrap();
            if predicate(&envelope.payload) {
                return envelope;
            }
        }
        panic!("mux stream ended before expected frame")
    })
    .await
    .expect("mux frame timeout")
}

fn success_response(rpc_id: RpcId, value: Value) -> ClientResponse {
    ClientResponse::new(rpc_id, RpcResult::Success { value: Some(value) })
}

fn approval_answer(
    rpc_id: RpcId,
    session_id: &SessionId,
    approval_id: &ApprovalRequestId,
    outcome: &str,
) -> ClientResponse {
    success_response(
        rpc_id,
        json!({
            "sessionId": session_id,
            "approvalId": approval_id,
            "outcome": outcome,
        }),
    )
}

fn question_answer(
    rpc_id: RpcId,
    session_id: &SessionId,
    id: &str,
    selected: &[&str],
    custom: Option<&str>,
) -> ClientResponse {
    let answer = AskUserQuestionAnswerItem {
        id: id.to_owned(),
        selected: selected.iter().map(ToString::to_string).collect(),
        custom: custom.map(ToOwned::to_owned),
    };
    success_response(
        rpc_id,
        json!({ "sessionId": session_id, "answer": { "answers": [answer] } }),
    )
}

fn approval_requested(envelope: &RpcRequest<MuxFrame>) -> (SessionId, ApprovalRequestId) {
    match &envelope.payload {
        MuxFrame::ApprovalRequested {
            session_id,
            approval_id,
            ..
        } => (session_id.clone(), approval_id.clone()),
        other => panic!("expected approval request, got {other:?}"),
    }
}

fn user_question_error(error: &anyhow::Error, code: &str) {
    assert_eq!(
        error.downcast_ref::<UserQuestionError>().unwrap().code(),
        code
    );
}

#[tokio::test]
async fn multi_select_question_round_trips_selected_and_custom_text() {
    let harness = Harness::new();
    let agent = harness.agent("question-multi");
    let signal = AbortSignal::default();
    let mut mux = open_mux(&harness.runtime, signal.clone());
    let questions = harness.questions.clone();
    let asking_agent = agent.clone();
    let asked = tokio::spawn(async move {
        questions
            .ask(question_request(
                asking_agent,
                question("targets", true),
                None,
            ))
            .await
    });
    let envelope = next_matching(&mut mux, |frame| {
        matches!(frame, MuxFrame::QuestionRequested { .. })
    })
    .await;
    assert_eq!(
        harness
            .runtime
            .respond(
                question_answer(
                    envelope.rpc_id,
                    agent.id(),
                    "targets",
                    &["Code", "Docs"],
                    Some("Release notes"),
                ),
                AbortSignal::default(),
            )
            .await
            .unwrap(),
        RpcReceipt::Accepted
    );
    assert_eq!(
        asked.await.unwrap().unwrap().answers[0],
        AskUserQuestionAnswerItem {
            id: "targets".to_owned(),
            selected: vec!["Code".to_owned(), "Docs".to_owned()],
            custom: Some("Release notes".to_owned()),
        }
    );
    next_matching(&mut mux, |frame| {
        matches!(frame, MuxFrame::QuestionResolved { .. })
    })
    .await;
    signal.abort();
}

#[tokio::test]
async fn single_select_question_rejects_mixed_custom_and_selection() {
    let harness = Harness::new();
    let agent = harness.agent("question-single");
    let signal = AbortSignal::default();
    let mut mux = open_mux(&harness.runtime, signal.clone());
    let questions = harness.questions.clone();
    let asking_agent = agent.clone();
    let asked = tokio::spawn(async move {
        questions
            .ask(question_request(
                asking_agent,
                question("target", false),
                None,
            ))
            .await
    });
    let envelope = next_matching(&mut mux, |frame| {
        matches!(frame, MuxFrame::QuestionRequested { .. })
    })
    .await;
    let invalid = question_answer(
        envelope.rpc_id.clone(),
        agent.id(),
        "target",
        &["Code"],
        Some("Release notes"),
    );
    assert_eq!(
        harness
            .runtime
            .respond(invalid, AbortSignal::default())
            .await
            .unwrap(),
        RpcReceipt::Rejected {
            reason: RpcReceiptReason::BadResponse
        }
    );
    assert_eq!(
        harness
            .runtime
            .respond(
                question_answer(
                    envelope.rpc_id,
                    agent.id(),
                    "target",
                    &[],
                    Some("Release notes"),
                ),
                AbortSignal::default(),
            )
            .await
            .unwrap(),
        RpcReceipt::Accepted
    );
    assert_eq!(
        asked.await.unwrap().unwrap().answers[0].custom.as_deref(),
        Some("Release notes")
    );
    signal.abort();
}

#[tokio::test]
async fn pending_question_replays_and_client_cancellation_settles_it() {
    let harness = Harness::new();
    let agent = harness.agent("question-replay");
    let first_signal = AbortSignal::default();
    let mut first_mux = open_mux(&harness.runtime, first_signal.clone());
    let questions = harness.questions.clone();
    let asking_agent = agent.clone();
    let asked = tokio::spawn(async move {
        questions
            .ask(question_request(
                asking_agent,
                question("target", false),
                None,
            ))
            .await
    });
    let first = next_matching(&mut first_mux, |frame| {
        matches!(frame, MuxFrame::QuestionRequested { .. })
    })
    .await;
    first_signal.abort();
    drop(first_mux);
    let second_signal = AbortSignal::default();
    let mut second_mux = open_mux(&harness.runtime, second_signal.clone());
    let replay = next_matching(&mut second_mux, |frame| {
        matches!(frame, MuxFrame::QuestionRequested { .. })
    })
    .await;
    assert_eq!(replay.rpc_id, first.rpc_id);
    assert_eq!(
        harness
            .runtime
            .respond(
                ClientResponse::new(
                    replay.rpc_id,
                    RpcResult::Failure {
                        error: RpcError {
                            code: "cancelled".to_owned(),
                            message: "user cancelled".to_owned(),
                            details: serde_json::Map::new(),
                        },
                    },
                ),
                AbortSignal::default(),
            )
            .await
            .unwrap(),
        RpcReceipt::Accepted
    );
    let failure = asked.await.unwrap().unwrap_err();
    user_question_error(&failure, "ASK_CANCELLED");
    next_matching(&mut second_mux, |frame| {
        matches!(frame, MuxFrame::QuestionResolved { .. })
    })
    .await;
    second_signal.abort();
}

#[tokio::test]
async fn question_abort_withdraws_the_pending_response_address() {
    let harness = Harness::new();
    let agent = harness.agent("question-abort");
    let mux_signal = AbortSignal::default();
    let mut mux = open_mux(&harness.runtime, mux_signal.clone());
    let ask_signal = AbortSignal::default();
    let questions = harness.questions.clone();
    let asking_agent = agent.clone();
    let request_signal = ask_signal.clone();
    let asked = tokio::spawn(async move {
        questions
            .ask(question_request(
                asking_agent,
                question("target", false),
                Some(request_signal),
            ))
            .await
    });
    let requested = next_matching(&mut mux, |frame| {
        matches!(frame, MuxFrame::QuestionRequested { .. })
    })
    .await;
    ask_signal.abort();
    let failure = asked.await.unwrap().unwrap_err();
    user_question_error(&failure, "ASK_ABORTED");
    next_matching(&mut mux, |frame| {
        matches!(frame, MuxFrame::QuestionResolved { .. })
    })
    .await;
    assert_eq!(
        harness
            .runtime
            .respond(
                question_answer(requested.rpc_id, agent.id(), "target", &["Code"], None,),
                AbortSignal::default(),
            )
            .await
            .unwrap(),
        RpcReceipt::Rejected {
            reason: RpcReceiptReason::NotPending
        }
    );
    mux_signal.abort();
}

#[tokio::test]
async fn approval_round_trip_replay_and_duplicate_receipts_are_exact() {
    let harness = Harness::new();
    let agent = harness.agent("approval-roundtrip");
    open_turn(&agent);
    let first_signal = AbortSignal::default();
    let mut first_mux = open_mux(&harness.runtime, first_signal.clone());
    let approval = harness.approval.clone();
    let request_agent = agent.clone();
    let asked = tokio::spawn(async move {
        approval
            .request(ApprovalRequest::new(request_agent, "bash").with_reason("sandbox escalation"))
            .await
    });
    let first = next_matching(&mut first_mux, |frame| {
        matches!(frame, MuxFrame::ApprovalRequested { .. })
    })
    .await;
    let (session_id, approval_id) = approval_requested(&first);
    assert_eq!(&session_id, agent.id());
    first_signal.abort();
    drop(first_mux);

    let second_signal = AbortSignal::default();
    let mut second_mux = open_mux(&harness.runtime, second_signal.clone());
    let replay = next_matching(&mut second_mux, |frame| {
        matches!(frame, MuxFrame::ApprovalRequested { .. })
    })
    .await;
    assert_eq!(replay.rpc_id, first.rpc_id);
    assert_eq!(approval_requested(&replay).1, approval_id);
    assert_eq!(
        harness
            .runtime
            .respond(
                approval_answer(
                    replay.rpc_id.clone(),
                    agent.id(),
                    &approval_id,
                    "allowed-once",
                ),
                AbortSignal::default(),
            )
            .await
            .unwrap(),
        RpcReceipt::Accepted
    );
    assert_eq!(asked.await.unwrap().unwrap(), ApprovalOutcome::AllowedOnce);
    next_matching(&mut second_mux, |frame| {
        matches!(frame, MuxFrame::ApprovalResolved { .. })
    })
    .await;
    assert_eq!(
        harness
            .runtime
            .respond(
                approval_answer(replay.rpc_id, agent.id(), &approval_id, "rejected"),
                AbortSignal::default(),
            )
            .await
            .unwrap(),
        RpcReceipt::Rejected {
            reason: RpcReceiptReason::NotPending
        }
    );
    second_signal.abort();
}

#[tokio::test]
async fn approval_rejects_unknown_error_malformed_and_mismatched_responses() {
    let harness = Harness::new();
    let agent = harness.agent("approval-invalid");
    open_turn(&agent);
    let signal = AbortSignal::default();
    let mut mux = open_mux(&harness.runtime, signal.clone());
    let approval = harness.approval.clone();
    let request_agent = agent.clone();
    let asked = tokio::spawn(async move {
        approval
            .request(ApprovalRequest::new(request_agent, "bash"))
            .await
    });
    let envelope = next_matching(&mut mux, |frame| {
        matches!(frame, MuxFrame::ApprovalRequested { .. })
    })
    .await;
    let (_, approval_id) = approval_requested(&envelope);
    assert_eq!(
        harness
            .runtime
            .respond(
                approval_answer(RpcId::new("ghost"), agent.id(), &approval_id, "rejected",),
                AbortSignal::default(),
            )
            .await
            .unwrap(),
        RpcReceipt::Rejected {
            reason: RpcReceiptReason::NotPending
        }
    );
    for response in [
        ClientResponse::new(
            envelope.rpc_id.clone(),
            RpcResult::Failure {
                error: RpcError {
                    code: "internal".to_owned(),
                    message: "x".to_owned(),
                    details: serde_json::Map::new(),
                },
            },
        ),
        approval_answer(
            envelope.rpc_id.clone(),
            agent.id(),
            &ApprovalRequestId::new("other"),
            "rejected",
        ),
        success_response(envelope.rpc_id.clone(), json!({ "nonsense": 1 })),
    ] {
        assert_eq!(
            harness
                .runtime
                .respond(response, AbortSignal::default())
                .await
                .unwrap(),
            RpcReceipt::Rejected {
                reason: RpcReceiptReason::BadResponse
            }
        );
    }
    signal.abort();
    drop(asked);
}

#[tokio::test]
async fn approval_signal_withdraws_once_and_late_response_is_not_pending() {
    let harness = Harness::new();
    let agent = harness.agent("approval-cancel");
    open_turn(&agent);
    let mux_signal = AbortSignal::default();
    let mut mux = open_mux(&harness.runtime, mux_signal.clone());
    let ask_signal = AbortSignal::default();
    let approval = harness.approval.clone();
    let request_agent = agent.clone();
    let request_signal = ask_signal.clone();
    let asked = tokio::spawn(async move {
        approval
            .request(ApprovalRequest::new(request_agent, "bash").with_signal(request_signal))
            .await
    });
    let envelope = next_matching(&mut mux, |frame| {
        matches!(frame, MuxFrame::ApprovalRequested { .. })
    })
    .await;
    let (_, approval_id) = approval_requested(&envelope);
    ask_signal.abort();
    assert_eq!(asked.await.unwrap().unwrap(), ApprovalOutcome::Cancelled);
    let resolved = next_matching(&mut mux, |frame| {
        matches!(frame, MuxFrame::ApprovalResolved { .. })
    })
    .await;
    assert!(matches!(
        resolved.payload,
        MuxFrame::ApprovalResolved {
            outcome: ApprovalOutcome::Cancelled,
            ..
        }
    ));
    assert_eq!(
        harness
            .runtime
            .respond(
                approval_answer(envelope.rpc_id, agent.id(), &approval_id, "allowed-once",),
                AbortSignal::default(),
            )
            .await
            .unwrap(),
        RpcReceipt::Rejected {
            reason: RpcReceiptReason::NotPending
        }
    );
    mux_signal.abort();
}

#[tokio::test]
async fn approval_carries_call_id_and_late_abort_cannot_resettle() {
    let harness = Harness::new();
    let agent = harness.agent("approval-call");
    open_turn(&agent);
    let mux_signal = AbortSignal::default();
    let mut mux = open_mux(&harness.runtime, mux_signal.clone());
    let ask_signal = AbortSignal::default();
    let approval = harness.approval.clone();
    let request_agent = agent.clone();
    let request_signal = ask_signal.clone();
    let asked = tokio::spawn(async move {
        approval
            .request(
                ApprovalRequest::new(request_agent, "bash")
                    .with_call_id(CallId::new("call-9"))
                    .with_signal(request_signal),
            )
            .await
    });
    let envelope = next_matching(&mut mux, |frame| {
        matches!(frame, MuxFrame::ApprovalRequested { .. })
    })
    .await;
    let (_, approval_id) = approval_requested(&envelope);
    assert!(matches!(
        &envelope.payload,
        MuxFrame::ApprovalRequested {
            call_id: Some(call_id),
            ..
        } if call_id.as_str() == "call-9"
    ));
    assert_eq!(
        harness
            .runtime
            .respond(
                approval_answer(envelope.rpc_id, agent.id(), &approval_id, "allowed-once",),
                AbortSignal::default(),
            )
            .await
            .unwrap(),
        RpcReceipt::Accepted
    );
    assert_eq!(asked.await.unwrap().unwrap(), ApprovalOutcome::AllowedOnce);
    ask_signal.abort();
    tokio::task::yield_now().await;
    let mut resolved = 0;
    while let Ok(Some(Ok(envelope))) =
        tokio::time::timeout(Duration::from_millis(10), mux.next()).await
    {
        if matches!(envelope.payload, MuxFrame::ApprovalResolved { .. }) {
            resolved += 1;
        }
    }
    assert_eq!(resolved, 1);
    mux_signal.abort();
}

#[tokio::test]
async fn parallel_approvals_claim_distinct_audit_ids_by_call_id() {
    let harness = Harness::new();
    let agent = harness.agent("approval-parallel");
    open_turn(&agent);
    let signal = AbortSignal::default();
    let mut mux = open_mux(&harness.runtime, signal.clone());
    let approval_a = harness.approval.clone();
    let agent_a = agent.clone();
    let ask_a = tokio::spawn(async move {
        approval_a
            .request(ApprovalRequest::new(agent_a, "alpha").with_call_id(CallId::new("call-a")))
            .await
    });
    let approval_b = harness.approval.clone();
    let agent_b = agent.clone();
    let ask_b = tokio::spawn(async move {
        approval_b
            .request(ApprovalRequest::new(agent_b, "beta").with_call_id(CallId::new("call-b")))
            .await
    });
    let first = next_matching(&mut mux, |frame| {
        matches!(frame, MuxFrame::ApprovalRequested { .. })
    })
    .await;
    let second = next_matching(&mut mux, |frame| {
        matches!(frame, MuxFrame::ApprovalRequested { .. })
    })
    .await;
    let (_, first_id) = approval_requested(&first);
    let (_, second_id) = approval_requested(&second);
    assert_ne!(first_id, second_id);
    for (envelope, outcome) in [(first, "allowed-once"), (second, "rejected")] {
        let (_, approval_id) = approval_requested(&envelope);
        assert_eq!(
            harness
                .runtime
                .respond(
                    approval_answer(envelope.rpc_id, agent.id(), &approval_id, outcome),
                    AbortSignal::default(),
                )
                .await
                .unwrap(),
            RpcReceipt::Accepted
        );
    }
    let outcomes = [ask_a.await.unwrap().unwrap(), ask_b.await.unwrap().unwrap()];
    assert!(outcomes.contains(&ApprovalOutcome::AllowedOnce));
    assert!(outcomes.contains(&ApprovalOutcome::Rejected));
    signal.abort();
}

#[tokio::test]
async fn parallel_call_idless_approvals_claim_distinct_ids_and_remain_answerable() {
    let harness = Harness::new();
    let agent = harness.agent("approval-parallel-call-idless");
    open_turn(&agent);
    let signal = AbortSignal::default();
    let mut mux = open_mux(&harness.runtime, signal.clone());
    let approval_a = harness.approval.clone();
    let agent_a = agent.clone();
    let ask_a = tokio::spawn(async move {
        approval_a
            .request(ApprovalRequest::new(agent_a, "alpha"))
            .await
    });
    let approval_b = harness.approval.clone();
    let agent_b = agent.clone();
    let ask_b = tokio::spawn(async move {
        approval_b
            .request(ApprovalRequest::new(agent_b, "beta"))
            .await
    });
    let first = next_matching(&mut mux, |frame| {
        matches!(frame, MuxFrame::ApprovalRequested { .. })
    })
    .await;
    let second = next_matching(&mut mux, |frame| {
        matches!(frame, MuxFrame::ApprovalRequested { .. })
    })
    .await;
    let (_, first_id) = approval_requested(&first);
    let (_, second_id) = approval_requested(&second);
    assert_ne!(first_id, second_id);
    for (envelope, outcome) in [(first, "allowed-once"), (second, "rejected")] {
        let (_, approval_id) = approval_requested(&envelope);
        assert_eq!(
            harness
                .runtime
                .respond(
                    approval_answer(envelope.rpc_id, agent.id(), &approval_id, outcome),
                    AbortSignal::default(),
                )
                .await
                .unwrap(),
            RpcReceipt::Accepted
        );
    }
    let outcomes = [ask_a.await.unwrap().unwrap(), ask_b.await.unwrap().unwrap()];
    assert!(outcomes.contains(&ApprovalOutcome::AllowedOnce));
    assert!(outcomes.contains(&ApprovalOutcome::Rejected));
    signal.abort();
}

#[tokio::test]
async fn stale_foreign_and_preaborted_approval_dispatches_delegate_without_frames() {
    let harness = Harness::new();
    let agent = harness.agent("approval-delegate");
    open_turn(&agent);
    agent
        .session()
        .append(
            "approval/asked",
            json!({ "id": "stale", "toolName": "bash" }),
            AppendOptions::default(),
        )
        .unwrap();
    agent
        .session()
        .append(
            "approval/decided",
            json!({ "id": "stale", "outcome": "rejected" }),
            AppendOptions::default(),
        )
        .unwrap();
    assert_eq!(
        direct_approval_dispatch(
            &harness.context,
            ApprovalRequest::new(agent.clone(), "bash")
        )
        .await,
        ApprovalOutcome::Unavailable
    );

    let foreign = harness.agent("approval-foreign");
    open_turn(&foreign);
    assert_eq!(
        direct_approval_dispatch(&harness.context, ApprovalRequest::new(foreign, "x")).await,
        ApprovalOutcome::Unavailable
    );

    let cancelled = AbortSignal::default();
    cancelled.abort();
    agent
        .session()
        .append(
            "approval/asked",
            json!({ "id": "pre-aborted", "toolName": "bash" }),
            AppendOptions::default(),
        )
        .unwrap();
    assert_eq!(
        direct_approval_dispatch(
            &harness.context,
            ApprovalRequest::new(agent, "bash").with_signal(cancelled),
        )
        .await,
        ApprovalOutcome::Cancelled
    );
}

async fn direct_approval_dispatch(context: &Context, request: ApprovalRequest) -> ApprovalOutcome {
    let dispatch = scope_target(context, Some(request.agent.scope_key()));
    let args = scoped_event_args(request.agent.scope_key(), EventArgs::one(request));
    let reply = context
        .events()
        .waterfall(&dispatch, "approval/request", &args, || {
            async {
                Ok(EventReply::Value(Arc::new(ApprovalAnswer::Outcome(
                    ApprovalOutcome::Unavailable,
                ))))
            }
            .boxed()
        })
        .await
        .unwrap();
    reply
        .downcast::<ApprovalAnswer>()
        .unwrap()
        .as_ref()
        .clone()
        .into_outcome()
}

trait ApprovalAnswerTestExt {
    fn into_outcome(self) -> ApprovalOutcome;
}

impl ApprovalAnswerTestExt for ApprovalAnswer {
    fn into_outcome(self) -> ApprovalOutcome {
        match self {
            ApprovalAnswer::Outcome(outcome) => outcome,
            ApprovalAnswer::Unknown(_) => ApprovalOutcome::Unavailable,
        }
    }
}

#[tokio::test]
async fn gateway_teardown_settles_pending_question_and_approval() {
    let context = Context::new();
    let agents = Arc::new(AgentRegistry::new(context.clone()));
    agents.provide(&context).unwrap();
    let questions = install_questions(&context).unwrap();
    let approval = ApprovalService::new(context.clone(), ApprovalConfig::default());
    approval.provide(&context).unwrap();
    let fiber = Fiber::active_child("api-proxy interactions");
    let child = context.with_fiber(fiber.clone());
    let runtime =
        InteractionApiProxyRuntime::from_context(&child, Arc::new(TerminalDomains)).unwrap();
    let id = SessionId::new("teardown");
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
    open_turn(&agent);
    let mux_signal = AbortSignal::default();
    let mut mux = open_mux(&runtime, mux_signal.clone());
    let approval_agent = agent.clone();
    let approval_wait = tokio::spawn(async move {
        approval
            .request(ApprovalRequest::new(approval_agent, "bash"))
            .await
    });
    let question_agent = agent.clone();
    let question_wait = tokio::spawn(async move {
        questions
            .ask(question_request(
                question_agent,
                question("teardown", false),
                None,
            ))
            .await
    });
    next_matching(&mut mux, |frame| {
        matches!(frame, MuxFrame::ApprovalRequested { .. })
    })
    .await;
    next_matching(&mut mux, |frame| {
        matches!(frame, MuxFrame::QuestionRequested { .. })
    })
    .await;
    fiber.dispose().await.unwrap();
    assert_eq!(
        approval_wait.await.unwrap().unwrap(),
        ApprovalOutcome::Cancelled
    );
    let question_error = question_wait.await.unwrap().unwrap_err();
    user_question_error(&question_error, "ASK_ABORTED");
    mux_signal.abort();
}
