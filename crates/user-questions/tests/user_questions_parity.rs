//! Behavioral mirror of `packages/interaction/user-questions/tests/user-questions.spec.ts`.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use parking_lot::Mutex;
use seekdeep_agent::{Agent, AgentOptions, AgentRegistry, Inbox, NoopInboxNotifications};
use seekdeep_cordis::Context;
use seekdeep_core::session::{Session, SessionId};
use seekdeep_llm::AbortSignal;
use seekdeep_scope::ScopeKey;
use seekdeep_user_questions::{
    AskUserQuestionAnswer, AskUserQuestionAnswerItem, AskUserQuestionIntent, AskUserQuestionItem,
    AskUserQuestionOption, AskUserQuestionRequest, UserQuestionError, UserQuestionProvider,
    UserQuestionService, install,
};
use serde_json::{Map, json};

fn question(id: &str) -> AskUserQuestionItem {
    AskUserQuestionItem {
        id: id.to_owned(),
        question: "Proceed?".to_owned(),
        detail: None,
        header: None,
        options: None,
        multi_select: None,
        intent: None,
    }
}

fn request(questions: Vec<AskUserQuestionItem>) -> AskUserQuestionRequest {
    AskUserQuestionRequest {
        questions,
        agent: None,
        signal: None,
    }
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

fn error<'a>(value: &'a anyhow::Error, code: &str) -> &'a UserQuestionError {
    let error = value
        .downcast_ref::<UserQuestionError>()
        .expect("UserQuestionError");
    assert_eq!(error.code(), code);
    error
}

#[derive(Debug)]
struct RecordingProvider {
    seen: Mutex<Vec<AskUserQuestionRequest>>,
    answer: String,
    calls: AtomicUsize,
}

impl RecordingProvider {
    fn new(answer: &str) -> Arc<Self> {
        Arc::new(Self {
            seen: Mutex::new(Vec::new()),
            answer: answer.to_owned(),
            calls: AtomicUsize::new(0),
        })
    }
}

#[async_trait]
impl UserQuestionProvider for RecordingProvider {
    async fn ask(&self, request: AskUserQuestionRequest) -> anyhow::Result<AskUserQuestionAnswer> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let id = request
            .questions
            .first()
            .map_or_else(|| "missing".to_owned(), |question| question.id.clone());
        self.seen.lock().push(request);
        Ok(AskUserQuestionAnswer {
            answers: vec![AskUserQuestionAnswerItem {
                id,
                selected: vec![self.answer.clone()],
                custom: None,
            }],
        })
    }
}

fn mounted() -> (Context, Arc<UserQuestionService>) {
    let context = Context::new();
    let service = install(&context).expect("user questions");
    (context, service)
}

fn mounted_with_agents() -> (Context, Arc<AgentRegistry>, Arc<UserQuestionService>) {
    let context = Context::new();
    let agents = Arc::new(AgentRegistry::new(context.clone()));
    agents.provide(&context).expect("agents");
    let service = install(&context).expect("user questions");
    (context, agents, service)
}

#[tokio::test]
async fn delegates_ask_requests_to_the_registered_provider() {
    let (context, service) = mounted();
    let provider = RecordingProvider::new("yes");
    service
        .register_provider(&context, provider.clone())
        .expect("provider");
    let result = service
        .ask(request(vec![question("confirm")]))
        .await
        .unwrap();
    assert_eq!(result.answers[0].selected, ["yes"]);
    assert_eq!(provider.seen.lock()[0].questions, [question("confirm")]);
}

#[tokio::test]
async fn rejects_without_a_provider() {
    let (_, service) = mounted();
    let failure = service
        .ask(request(vec![question("confirm")]))
        .await
        .unwrap_err();
    error(&failure, "NO_PROVIDER");
}

#[tokio::test]
async fn provider_disposal_is_idempotent_and_hmr_safe() {
    let (context, service) = mounted();
    let effect = service
        .register_provider(&context, RecordingProvider::new("yes"))
        .unwrap();
    effect.dispose().await.unwrap();
    effect.dispose().await.unwrap();
    let failure = service
        .ask(request(vec![question("confirm")]))
        .await
        .unwrap_err();
    error(&failure, "NO_PROVIDER");
}

#[test]
fn duplicate_provider_is_rejected_without_replacement() {
    let (context, service) = mounted();
    service
        .register_provider(&context, RecordingProvider::new("first"))
        .unwrap();
    let failure = service
        .register_provider(&context, RecordingProvider::new("second"))
        .unwrap_err();
    error(&failure, "DUPLICATE_PROVIDER");
}

#[tokio::test]
async fn preaborted_request_fails_before_provider() {
    let (context, service) = mounted();
    let provider = RecordingProvider::new("late");
    service
        .register_provider(&context, provider.clone())
        .unwrap();
    let signal = AbortSignal::default();
    signal.abort();
    let mut pending = request(vec![question("confirm")]);
    pending.signal = Some(signal);
    let failure = service.ask(pending).await.unwrap_err();
    error(&failure, "ASK_ABORTED");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn empty_batch_fails_before_provider() {
    let (context, service) = mounted();
    let provider = RecordingProvider::new("late");
    service
        .register_provider(&context, provider.clone())
        .unwrap();
    let failure = service.ask(request(Vec::new())).await.unwrap_err();
    error(&failure, "EMPTY_QUESTIONS");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn runtime_owned_child_is_rejected_before_provider() {
    let (context, agents, service) = mounted_with_agents();
    let provider = RecordingProvider::new("late");
    service
        .register_provider(&context, provider.clone())
        .unwrap();
    let root = agent("root");
    let child = agent("child");
    agents.enter(root.clone(), None).unwrap();
    agents.enter(child.clone(), Some(root)).unwrap();
    let mut pending = request(vec![question("confirm")]);
    pending.agent = Some(child);
    let failure = service.ask(pending).await.unwrap_err();
    let error = error(&failure, "DELEGATED_CALLER");
    assert_eq!(
        error.message(),
        "human interaction is unavailable while the calling agent is owned by another live agent; include the unresolved question or decision in the child agent's final result"
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn lineage_bearing_runtime_root_reaches_provider() {
    let (context, agents, service) = mounted_with_agents();
    let provider = RecordingProvider::new("yes");
    service
        .register_provider(&context, provider.clone())
        .unwrap();
    let resumed = agent("resumed-root");
    agents.enter(resumed.clone(), None).unwrap();
    let mut pending = request(vec![question("confirm")]);
    pending.agent = Some(resumed);
    assert_eq!(
        service.ask(pending).await.unwrap().answers[0].selected,
        ["yes"]
    );
}

#[tokio::test]
async fn supplied_agent_without_registry_is_not_live() {
    let (context, service) = mounted();
    let provider = RecordingProvider::new("late");
    service
        .register_provider(&context, provider.clone())
        .unwrap();
    let mut pending = request(vec![question("confirm")]);
    pending.agent = Some(agent("unattested"));
    let failure = service.ask(pending).await.unwrap_err();
    error(&failure, "CALLER_NOT_LIVE");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn stale_same_id_agent_is_not_the_exact_live_instance() {
    let (context, agents, service) = mounted_with_agents();
    let provider = RecordingProvider::new("late");
    service
        .register_provider(&context, provider.clone())
        .unwrap();
    agents.enter(agent("same-id"), None).unwrap();
    let mut pending = request(vec![question("confirm")]);
    pending.agent = Some(agent("same-id"));
    let failure = service.ask(pending).await.unwrap_err();
    error(&failure, "CALLER_NOT_LIVE");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn intent_approve_must_name_an_offered_option() {
    let (context, service) = mounted();
    let provider = RecordingProvider::new("late");
    service
        .register_provider(&context, provider.clone())
        .unwrap();
    for options in [
        Some(vec![AskUserQuestionOption {
            label: "Approve".to_owned(),
            description: None,
        }]),
        None,
    ] {
        let mut item = question("plan-review");
        item.detail = Some("# Plan".to_owned());
        item.options = options;
        item.intent = Some(AskUserQuestionIntent {
            kind: "plan-review".to_owned(),
            approve: "Ship it".to_owned(),
            extra: Map::new(),
        });
        let failure = service.ask(request(vec![item])).await.unwrap_err();
        error(&failure, "BAD_INTENT");
    }
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn plan_review_requires_visible_detail() {
    let (context, service) = mounted();
    let provider = RecordingProvider::new("late");
    service
        .register_provider(&context, provider.clone())
        .unwrap();
    let mut item = question("plan-review");
    item.options = Some(vec![AskUserQuestionOption {
        label: "Approve".to_owned(),
        description: None,
    }]);
    item.intent = Some(AskUserQuestionIntent {
        kind: "plan-review".to_owned(),
        approve: "Approve".to_owned(),
        extra: Map::new(),
    });
    let failure = service.ask(request(vec![item])).await.unwrap_err();
    error(&failure, "BAD_INTENT");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn valid_and_unknown_intents_pass_through_losslessly() {
    let (context, service) = mounted();
    let provider = RecordingProvider::new("Approve");
    service
        .register_provider(&context, provider.clone())
        .unwrap();
    let mut item = question("plan-review");
    item.detail = Some("# Plan".to_owned());
    item.options = Some(vec![AskUserQuestionOption {
        label: "Approve".to_owned(),
        description: None,
    }]);
    item.intent = Some(AskUserQuestionIntent {
        kind: "future-review".to_owned(),
        approve: "Approve".to_owned(),
        extra: Map::from_iter([("future".to_owned(), json!({"kept": true}))]),
    });
    service
        .ask(request(vec![question("plain"), item.clone()]))
        .await
        .unwrap();
    assert_eq!(provider.seen.lock()[0].questions[1], item);
}
