//! Production question and approval interactions over the mux and response carriers.

use std::{
    collections::HashSet,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context as TaskContext, Poll},
};

use async_trait::async_trait;
use futures::{FutureExt as _, StreamExt as _, future::BoxFuture};
use indexmap::IndexMap;
use parking_lot::Mutex;
use seekdeep_agent::AGENTS;
use seekdeep_client_connection::{HttpResponse, RpcResult};
use seekdeep_cordis::{Context, EventOptions, fiber::EffectHandle};
use seekdeep_core::session::SessionId;
use seekdeep_llm::{AbortSignal, CallId};
use seekdeep_user_approval::{
    APPROVAL, ApprovalAnswer, ApprovalNext, ApprovalOutcome, ApprovalRequest, ApprovalRequestId,
};
use seekdeep_user_questions::{
    AskUserQuestionAnswer, AskUserQuestionItem, AskUserQuestionRequest, USER_QUESTIONS,
    UserQuestionError, UserQuestionProvider,
};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::{
    ApiDownlinkStream, ApiProxyRuntime, ClientResponse, RpcId, RpcMethod, RpcReceipt,
    RpcReceiptReason, RpcRequest, RpcResponse,
    api::{
        approvals::ApprovalResponsePayload,
        downloads::SessionLogQuery,
        events::{HostFrame, MuxFrame, QuestionResolutionOutcome},
        questions::QuestionResponsePayload,
        sessions::is_ecmascript_whitespace,
    },
};

static NEXT_INTERACTION_ID: AtomicU64 = AtomicU64::new(1);

/// Question and approval decorator over the remaining API Proxy domains.
pub struct InteractionApiProxyRuntime {
    shared: Arc<InteractionShared>,
    domains: Arc<dyn ApiProxyRuntime>,
}

impl std::fmt::Debug for InteractionApiProxyRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.shared.state.lock();
        formatter
            .debug_struct("InteractionApiProxyRuntime")
            .field("pending_questions", &state.questions.len())
            .field("pending_approvals", &state.approvals.len())
            .field("mux_subscribers", &state.subscribers.len())
            .finish_non_exhaustive()
    }
}

impl InteractionApiProxyRuntime {
    /// Registers the web question provider and optional approval answerer.
    ///
    /// # Errors
    ///
    /// Returns when the required user-question service is absent, another
    /// provider is already active, or lifecycle ownership fails.
    pub fn from_context(
        context: &Context,
        domains: Arc<dyn ApiProxyRuntime>,
    ) -> anyhow::Result<Arc<Self>> {
        let questions = context
            .get(USER_QUESTIONS)
            .ok_or_else(|| anyhow::anyhow!("userQuestions service is required"))?;
        anyhow::ensure!(context.get(AGENTS).is_some(), "agents service is required");
        let shared = Arc::new(InteractionShared::default());
        let question_effect = questions.register_provider(
            context,
            Arc::new(ApiQuestionProvider {
                shared: shared.clone(),
            }),
        )?;
        let mut effects = vec![question_effect];
        if let Some(approval) = context.get(APPROVAL) {
            let answerer = shared.clone();
            match approval.on_request(
                context,
                move |request, next| {
                    let answerer = answerer.clone();
                    async move { answerer.answer_approval(request, next).await }
                },
                EventOptions::default(),
            ) {
                Ok(effect) => effects.push(effect),
                Err(error) => {
                    dispose_effects(effects);
                    return Err(error.into());
                }
            }
        }
        let cleanup_state = shared.clone();
        let cleanup = EffectHandle::synchronous("api-proxy interactions", move || {
            cleanup_state.shutdown();
            Ok(())
        });
        if let Err(error) = context.own(cleanup.clone()) {
            shared.shutdown();
            effects.push(cleanup);
            dispose_effects(effects);
            return Err(error.into());
        }
        Ok(Arc::new(Self { shared, domains }))
    }

    fn interaction_mux(&self, signal: AbortSignal) -> ApiDownlinkStream<MuxFrame> {
        let (subscriber_id, baseline, mut receiver) = self.shared.subscribe();
        let shared = self.shared.clone();
        let inner = async_stream::stream! {
            for envelope in baseline {
                yield Ok(envelope);
            }
            loop {
                tokio::select! {
                    () = signal.cancelled() => break,
                    envelope = receiver.recv() => match envelope {
                        Some(envelope) => yield Ok(envelope),
                        None => break,
                    },
                }
            }
        }
        .boxed();
        InteractionMuxStream {
            inner,
            _guard: MuxSubscriberGuard {
                shared,
                subscriber_id,
            },
        }
        .boxed()
    }
}

impl ApiProxyRuntime for InteractionApiProxyRuntime {
    fn unary(
        &self,
        method: RpcMethod,
        request: RpcRequest<Value>,
        signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<RpcResponse<Value>>> {
        self.domains.unary(method, request, signal)
    }

    fn respond(
        &self,
        message: ClientResponse,
        signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<RpcReceipt>> {
        if self.shared.has_approval(&message.rpc_id) {
            return futures::future::ready(Ok(self.shared.respond_approval(message))).boxed();
        }
        if self.shared.has_question(&message.rpc_id) {
            return futures::future::ready(Ok(self.shared.respond_question(message))).boxed();
        }
        self.domains.respond(message, signal)
    }

    fn mux(&self, request: RpcRequest<Value>, signal: AbortSignal) -> ApiDownlinkStream<MuxFrame> {
        futures::stream::select(
            self.domains.mux(request, signal.clone()),
            self.interaction_mux(signal),
        )
        .boxed()
    }

    fn host(
        &self,
        request: RpcRequest<Value>,
        signal: AbortSignal,
    ) -> ApiDownlinkStream<HostFrame> {
        self.domains.host(request, signal)
    }

    fn session_log(
        &self,
        query: SessionLogQuery,
        signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<HttpResponse>> {
        self.domains.session_log(query, signal)
    }
}

#[derive(Default)]
struct InteractionShared {
    state: Mutex<InteractionState>,
}

#[derive(Default)]
struct InteractionState {
    questions: IndexMap<RpcId, PendingQuestion>,
    approvals: IndexMap<RpcId, PendingApproval>,
    subscribers: IndexMap<u64, mpsc::UnboundedSender<RpcRequest<MuxFrame>>>,
    next_subscriber_id: u64,
    shutting_down: bool,
}

struct PendingQuestion {
    session_id: SessionId,
    questions: Vec<AskUserQuestionItem>,
    settle: oneshot::Sender<QuestionSettlement>,
}

enum QuestionSettlement {
    Answer(AskUserQuestionAnswer),
    Error(UserQuestionError),
}

struct PendingApproval {
    session_id: SessionId,
    approval_id: ApprovalRequestId,
    tool_name: String,
    call_id: Option<CallId>,
    reason: Option<String>,
    settle: oneshot::Sender<ApprovalOutcome>,
}

#[derive(Clone)]
struct ApprovalDescriptor {
    session_id: SessionId,
    approval_id: ApprovalRequestId,
}

impl InteractionShared {
    fn begin_question(
        &self,
        session_id: SessionId,
        questions: Vec<AskUserQuestionItem>,
    ) -> anyhow::Result<(RpcId, oneshot::Receiver<QuestionSettlement>)> {
        let rpc_id = next_interaction_id("question");
        let (settle, receiver) = oneshot::channel();
        let envelope = RpcRequest::new(
            rpc_id.clone(),
            MuxFrame::QuestionRequested {
                session_id: session_id.clone(),
                questions: questions.clone(),
            },
        );
        let subscribers = {
            let mut state = self.state.lock();
            if state.shutting_down {
                return Err(UserQuestionError::new(
                    "web user-questions provider was disposed",
                    "ASK_ABORTED",
                )
                .into());
            }
            state.questions.insert(
                rpc_id.clone(),
                PendingQuestion {
                    session_id,
                    questions,
                    settle,
                },
            );
            state.subscribers.values().cloned().collect::<Vec<_>>()
        };
        send_to_subscribers(subscribers, &envelope);
        Ok((rpc_id, receiver))
    }

    fn reject_question(&self, rpc_id: &RpcId, error: UserQuestionError) {
        let Some(pending) = self.take_question(rpc_id, QuestionResolutionOutcome::Cancelled) else {
            return;
        };
        let _ = pending.settle.send(QuestionSettlement::Error(error));
    }

    fn take_question(
        &self,
        rpc_id: &RpcId,
        outcome: QuestionResolutionOutcome,
    ) -> Option<PendingQuestion> {
        let (pending, subscribers) = {
            let mut state = self.state.lock();
            let pending = state.questions.shift_remove(rpc_id)?;
            let subscribers = state.subscribers.values().cloned().collect::<Vec<_>>();
            (pending, subscribers)
        };
        send_to_subscribers(
            subscribers,
            &RpcRequest::new(
                next_interaction_id("frame"),
                MuxFrame::QuestionResolved {
                    session_id: pending.session_id.clone(),
                    question_rpc_id: rpc_id.clone(),
                    outcome,
                },
            ),
        );
        Some(pending)
    }

    fn has_question(&self, rpc_id: &RpcId) -> bool {
        self.state.lock().questions.contains_key(rpc_id)
    }

    fn has_approval(&self, rpc_id: &RpcId) -> bool {
        self.state.lock().approvals.contains_key(rpc_id)
    }

    fn respond_question(&self, message: ClientResponse) -> RpcReceipt {
        let descriptor = {
            let state = self.state.lock();
            state
                .questions
                .get(&message.rpc_id)
                .map(|pending| (pending.session_id.clone(), pending.questions.clone()))
        };
        let Some((session_id, questions)) = descriptor else {
            return rejected(RpcReceiptReason::NotPending);
        };
        match message.result {
            RpcResult::Failure { error } if error.code == "cancelled" => {
                self.reject_question(
                    &message.rpc_id,
                    UserQuestionError::new("the user cancelled ask_user_question", "ASK_CANCELLED"),
                );
                RpcReceipt::Accepted
            }
            RpcResult::Failure { .. } | RpcResult::Success { value: None } => {
                rejected(RpcReceiptReason::BadResponse)
            }
            RpcResult::Success { value: Some(value) } => {
                let Ok(payload) = QuestionResponsePayload::parse(&value) else {
                    return rejected(RpcReceiptReason::BadResponse);
                };
                if !matches_questions(&payload, &session_id, &questions) {
                    return rejected(RpcReceiptReason::BadResponse);
                }
                let Some(pending) =
                    self.take_question(&message.rpc_id, QuestionResolutionOutcome::Answered)
                else {
                    return rejected(RpcReceiptReason::NotPending);
                };
                let _ = pending
                    .settle
                    .send(QuestionSettlement::Answer(payload.answer));
                RpcReceipt::Accepted
            }
        }
    }

    fn approval_descriptor(&self, rpc_id: &RpcId) -> Option<ApprovalDescriptor> {
        self.state
            .lock()
            .approvals
            .get(rpc_id)
            .map(|pending| ApprovalDescriptor {
                session_id: pending.session_id.clone(),
                approval_id: pending.approval_id.clone(),
            })
    }

    fn respond_approval(&self, message: ClientResponse) -> RpcReceipt {
        let Some(descriptor) = self.approval_descriptor(&message.rpc_id) else {
            return rejected(RpcReceiptReason::NotPending);
        };
        let RpcResult::Success { value: Some(value) } = message.result else {
            return rejected(RpcReceiptReason::BadResponse);
        };
        let Ok(payload) = ApprovalResponsePayload::parse(&value) else {
            return rejected(RpcReceiptReason::BadResponse);
        };
        if payload.session_id != descriptor.session_id
            || payload.approval_id != descriptor.approval_id
        {
            return rejected(RpcReceiptReason::BadResponse);
        }
        if self.settle_approval(&message.rpc_id, payload.outcome.into()) {
            RpcReceipt::Accepted
        } else {
            rejected(RpcReceiptReason::NotPending)
        }
    }

    async fn answer_approval(
        self: Arc<Self>,
        request: ApprovalRequest,
        next: ApprovalNext,
    ) -> anyhow::Result<ApprovalAnswer> {
        if request.signal.as_ref().is_some_and(AbortSignal::is_aborted) {
            return Ok(ApprovalOutcome::Cancelled.into());
        }
        let Some((rpc_id, receiver)) = self.begin_approval(&request) else {
            return next.run().await;
        };
        let signal = request.signal.clone();
        let guard = ApprovalWaitGuard {
            shared: self.clone(),
            rpc_id: rpc_id.clone(),
        };
        let outcome = if let Some(signal) = signal {
            match futures::future::select(receiver, signal.cancelled()).await {
                futures::future::Either::Left((outcome, _)) => outcome,
                futures::future::Either::Right(((), receiver)) => {
                    self.settle_approval(&rpc_id, ApprovalOutcome::Cancelled);
                    receiver.await
                }
            }
        } else {
            receiver.await
        }
        .unwrap_or(ApprovalOutcome::Cancelled);
        drop(guard);
        Ok(outcome.into())
    }

    fn begin_approval(
        &self,
        request: &ApprovalRequest,
    ) -> Option<(RpcId, oneshot::Receiver<ApprovalOutcome>)> {
        let events = request.agent.session().events();
        let rpc_id = next_interaction_id("approval");
        let (settle, receiver) = oneshot::channel();
        let (pending, subscribers) = {
            let mut state = self.state.lock();
            if state.shutting_down {
                return None;
            }
            let claimed = state
                .approvals
                .values()
                .map(|pending| pending.approval_id.clone())
                .collect::<HashSet<_>>();
            let mut decided = HashSet::new();
            let mut approval_id = None;
            for event in events.iter().rev() {
                match event.event_type.as_str() {
                    "approval/decided" => {
                        if let Some(id) = event.data.get("id").and_then(Value::as_str) {
                            decided.insert(ApprovalRequestId::new(id));
                        }
                    }
                    "approval/asked" => {
                        let Some(id) = event.data.get("id").and_then(Value::as_str) else {
                            continue;
                        };
                        let id = ApprovalRequestId::new(id);
                        if decided.contains(&id) || claimed.contains(&id) {
                            continue;
                        }
                        let event_call_id = event.data.get("callId").and_then(Value::as_str);
                        let request_call_id = request.call_id.as_ref().map(CallId::as_str);
                        if event_call_id != request_call_id {
                            continue;
                        }
                        approval_id = Some(id);
                        break;
                    }
                    _ => {}
                }
            }
            let approval_id = approval_id?;
            let pending = PendingApproval {
                session_id: request.agent.session().id().clone(),
                approval_id,
                tool_name: request.tool_name.clone(),
                call_id: request.call_id.clone(),
                reason: request.reason.clone(),
                settle,
            };
            let subscribers = state.subscribers.values().cloned().collect::<Vec<_>>();
            state.approvals.insert(rpc_id.clone(), pending);
            let pending = state.approvals.get(&rpc_id).expect("inserted approval");
            (approval_requested(&rpc_id, pending), subscribers)
        };
        send_to_subscribers(subscribers, &pending);
        Some((rpc_id, receiver))
    }

    fn settle_approval(&self, rpc_id: &RpcId, outcome: ApprovalOutcome) -> bool {
        let (pending, subscribers) = {
            let mut state = self.state.lock();
            let Some(pending) = state.approvals.shift_remove(rpc_id) else {
                return false;
            };
            let subscribers = state.subscribers.values().cloned().collect::<Vec<_>>();
            (pending, subscribers)
        };
        send_to_subscribers(
            subscribers,
            &RpcRequest::new(
                next_interaction_id("frame"),
                MuxFrame::ApprovalResolved {
                    session_id: pending.session_id.clone(),
                    approval_id: pending.approval_id.clone(),
                    outcome,
                },
            ),
        );
        let _ = pending.settle.send(outcome);
        true
    }

    fn subscribe(
        &self,
    ) -> (
        u64,
        Vec<RpcRequest<MuxFrame>>,
        mpsc::UnboundedReceiver<RpcRequest<MuxFrame>>,
    ) {
        let (sender, receiver) = mpsc::unbounded_channel();
        let mut state = self.state.lock();
        let subscriber_id = state.next_subscriber_id;
        state.next_subscriber_id = state.next_subscriber_id.wrapping_add(1);
        let mut baseline = state
            .questions
            .iter()
            .map(|(rpc_id, pending)| {
                RpcRequest::new(
                    rpc_id.clone(),
                    MuxFrame::QuestionRequested {
                        session_id: pending.session_id.clone(),
                        questions: pending.questions.clone(),
                    },
                )
            })
            .collect::<Vec<_>>();
        baseline.extend(
            state
                .approvals
                .iter()
                .map(|(rpc_id, pending)| approval_requested(rpc_id, pending)),
        );
        state.subscribers.insert(subscriber_id, sender);
        (subscriber_id, baseline, receiver)
    }

    fn unsubscribe(&self, subscriber_id: u64) {
        self.state.lock().subscribers.shift_remove(&subscriber_id);
    }

    fn shutdown(&self) {
        let (questions, approvals, subscribers) = {
            let mut state = self.state.lock();
            if state.shutting_down {
                return;
            }
            state.shutting_down = true;
            (
                std::mem::take(&mut state.questions),
                std::mem::take(&mut state.approvals),
                state.subscribers.values().cloned().collect::<Vec<_>>(),
            )
        };
        for (rpc_id, pending) in questions {
            send_to_subscribers(
                subscribers.clone(),
                &RpcRequest::new(
                    next_interaction_id("frame"),
                    MuxFrame::QuestionResolved {
                        session_id: pending.session_id,
                        question_rpc_id: rpc_id,
                        outcome: QuestionResolutionOutcome::Cancelled,
                    },
                ),
            );
            let _ = pending
                .settle
                .send(QuestionSettlement::Error(UserQuestionError::new(
                    "web user-questions provider was disposed",
                    "ASK_ABORTED",
                )));
        }
        for (_rpc_id, pending) in approvals {
            send_to_subscribers(
                subscribers.clone(),
                &RpcRequest::new(
                    next_interaction_id("frame"),
                    MuxFrame::ApprovalResolved {
                        session_id: pending.session_id,
                        approval_id: pending.approval_id,
                        outcome: ApprovalOutcome::Cancelled,
                    },
                ),
            );
            let _ = pending.settle.send(ApprovalOutcome::Cancelled);
        }
    }
}

struct ApiQuestionProvider {
    shared: Arc<InteractionShared>,
}

#[async_trait]
impl UserQuestionProvider for ApiQuestionProvider {
    async fn ask(&self, request: AskUserQuestionRequest) -> anyhow::Result<AskUserQuestionAnswer> {
        let Some(agent) = &request.agent else {
            return Err(UserQuestionError::new(
                "web user interaction requires an agent-owned session",
                "ASK_MISSING_AGENT",
            )
            .into());
        };
        let (rpc_id, receiver) = self
            .shared
            .begin_question(agent.id().clone(), request.questions)?;
        let settlement = if let Some(signal) = request.signal {
            match futures::future::select(receiver, signal.cancelled()).await {
                futures::future::Either::Left((settlement, _)) => settlement,
                futures::future::Either::Right(((), receiver)) => {
                    self.shared.reject_question(
                        &rpc_id,
                        UserQuestionError::new(
                            "ask_user_question was aborted before the user answered",
                            "ASK_ABORTED",
                        ),
                    );
                    receiver.await
                }
            }
        } else {
            receiver.await
        };
        match settlement {
            Ok(QuestionSettlement::Answer(answer)) => Ok(answer),
            Ok(QuestionSettlement::Error(error)) => Err(error.into()),
            Err(_) => Err(UserQuestionError::new(
                "web user-questions provider was disposed",
                "ASK_ABORTED",
            )
            .into()),
        }
    }
}

fn matches_questions(
    payload: &QuestionResponsePayload,
    session_id: &SessionId,
    questions: &[AskUserQuestionItem],
) -> bool {
    if &payload.session_id != session_id || payload.answer.answers.len() != questions.len() {
        return false;
    }
    payload
        .answer
        .answers
        .iter()
        .zip(questions)
        .all(|(answer, question)| {
            if answer.id != question.id {
                return false;
            }
            let selected = answer.selected.iter().collect::<HashSet<_>>();
            if selected.len() != answer.selected.len() {
                return false;
            }
            let custom = answer
                .custom
                .as_deref()
                .map(|value| value.trim_matches(is_ecmascript_whitespace));
            if custom == Some("") {
                return false;
            }
            if question.multi_select != Some(true)
                && ((custom.is_some() && !answer.selected.is_empty()) || answer.selected.len() > 1)
            {
                return false;
            }
            let labels = question
                .options
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|option| option.label.as_str())
                .collect::<HashSet<_>>();
            answer
                .selected
                .iter()
                .all(|label| labels.contains(label.as_str()))
        })
}

fn approval_requested(rpc_id: &RpcId, pending: &PendingApproval) -> RpcRequest<MuxFrame> {
    RpcRequest::new(
        rpc_id.clone(),
        MuxFrame::ApprovalRequested {
            session_id: pending.session_id.clone(),
            approval_id: pending.approval_id.clone(),
            tool_name: pending.tool_name.clone(),
            call_id: pending.call_id.clone(),
            reason: pending.reason.clone(),
        },
    )
}

fn rejected(reason: RpcReceiptReason) -> RpcReceipt {
    RpcReceipt::Rejected { reason }
}

fn send_to_subscribers(
    subscribers: Vec<mpsc::UnboundedSender<RpcRequest<MuxFrame>>>,
    envelope: &RpcRequest<MuxFrame>,
) {
    for subscriber in subscribers {
        let _ = subscriber.send(envelope.clone());
    }
}

fn next_interaction_id(kind: &str) -> RpcId {
    RpcId::new(format!(
        "interaction-{kind}-{}",
        NEXT_INTERACTION_ID.fetch_add(1, Ordering::Relaxed)
    ))
}

fn dispose_effects(effects: Vec<EffectHandle>) {
    let dispose = async move {
        for effect in effects.into_iter().rev() {
            let _ = effect.dispose().await;
        }
    };
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn(dispose);
    } else {
        std::thread::spawn(move || futures::executor::block_on(dispose));
    }
}

struct ApprovalWaitGuard {
    shared: Arc<InteractionShared>,
    rpc_id: RpcId,
}

impl Drop for ApprovalWaitGuard {
    fn drop(&mut self) {
        self.shared
            .settle_approval(&self.rpc_id, ApprovalOutcome::Cancelled);
    }
}

struct MuxSubscriberGuard {
    shared: Arc<InteractionShared>,
    subscriber_id: u64,
}

impl Drop for MuxSubscriberGuard {
    fn drop(&mut self) {
        self.shared.unsubscribe(self.subscriber_id);
    }
}

struct InteractionMuxStream {
    inner: ApiDownlinkStream<MuxFrame>,
    _guard: MuxSubscriberGuard,
}

impl futures::Stream for InteractionMuxStream {
    type Item = anyhow::Result<RpcRequest<MuxFrame>>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
    ) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(context)
    }
}
