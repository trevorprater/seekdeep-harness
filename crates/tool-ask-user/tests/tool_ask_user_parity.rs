//! Behavioral mirror of `packages/interaction/tool-ask-user/tests/tool-ask-user.spec.ts`.

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use seekdeep_agent::{Agent, AgentOptions, AgentRegistry, Inbox, NoopInboxNotifications};
use seekdeep_cordis::Context;
use seekdeep_core::session::{Session, SessionId};
use seekdeep_llm::{AbortSignal, CallId, ContentBlock};
use seekdeep_scope::ScopeKey;
use seekdeep_system_prompt::{SystemPrompt, SystemPromptConfig};
use seekdeep_tool_ask_user::{TOOL_NAME, apply};
use seekdeep_tools::{ToolExecutionInput, ToolExecutionResult, ToolRuntime, ToolRuntimeConfig};
use seekdeep_user_questions::{
    AskUserQuestionAnswer, AskUserQuestionAnswerItem, AskUserQuestionRequest, UserQuestionProvider,
    UserQuestionService, install as install_questions,
};
use serde_json::{Value, json};

struct Harness {
    context: Context,
    tools: Arc<ToolRuntime>,
    questions: Arc<UserQuestionService>,
    agents: Arc<AgentRegistry>,
}

fn setup(install_tool: bool) -> Harness {
    let context = Context::new();
    let agents = Arc::new(AgentRegistry::new(context.clone()));
    agents.provide(&context).expect("agents");
    let prompt = SystemPrompt::new(&context, SystemPromptConfig::default()).expect("prompt");
    let tools =
        seekdeep_tools::install(&context, &prompt, ToolRuntimeConfig::default()).expect("tools");
    let questions = install_questions(&context).expect("questions");
    if install_tool {
        apply(&context).expect("ask tool");
    }
    Harness {
        context,
        tools,
        questions,
        agents,
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

fn input(call: &str, arguments: Value, signal: AbortSignal) -> ToolExecutionInput {
    ToolExecutionInput::new(CallId::new(call), TOOL_NAME, arguments, signal)
}

#[derive(Debug)]
struct Provider {
    seen: Mutex<Vec<AskUserQuestionRequest>>,
    answer: Mutex<AskUserQuestionAnswer>,
}

impl Provider {
    fn new(answer: AskUserQuestionAnswer) -> Arc<Self> {
        Arc::new(Self {
            seen: Mutex::new(Vec::new()),
            answer: Mutex::new(answer),
        })
    }
}

#[async_trait]
impl UserQuestionProvider for Provider {
    async fn ask(&self, request: AskUserQuestionRequest) -> anyhow::Result<AskUserQuestionAnswer> {
        self.seen.lock().push(request);
        Ok(self.answer.lock().clone())
    }
}

fn answer(id: &str, selected: &[&str], custom: Option<&str>) -> AskUserQuestionAnswerItem {
    AskUserQuestionAnswerItem {
        id: id.to_owned(),
        selected: selected.iter().map(|value| (*value).to_owned()).collect(),
        custom: custom.map(str::to_owned),
    }
}

#[test]
fn registers_the_exact_model_facing_schema() {
    let harness = setup(true);
    let schema = harness
        .tools
        .schemas(None)
        .into_iter()
        .find(|schema| schema.name == TOOL_NAME)
        .expect("schema");
    assert_eq!(schema.parameters["type"], "object");
    assert_eq!(schema.parameters["required"], json!(["questions"]));
    let item = &schema.parameters["properties"]["questions"]["items"]["properties"];
    assert_eq!(item["id"]["type"], "string");
    assert_eq!(item["question"]["type"], "string");
    assert_eq!(item["header"]["type"], "string");
    assert_eq!(item["options"]["type"], "array");
    assert_eq!(item["multi_select"]["type"], "boolean");
    let option = &item["options"]["items"]["properties"];
    assert_eq!(option["label"]["type"], "string");
    assert_eq!(option["description"]["type"], "string");
    assert!(option.get("value").is_none());
    assert!(option.get("recommended").is_none());
    assert!(option.get("preview").is_none());
}

#[tokio::test]
async fn asks_provider_and_projects_structured_answer_to_compact_json_text() {
    let harness = setup(true);
    let provider = Provider::new(AskUserQuestionAnswer {
        answers: vec![answer("pkg", &["pnpm"], None)],
    });
    harness
        .questions
        .register_provider(&harness.context, provider.clone())
        .unwrap();
    let result = harness
        .tools
        .execute(input(
            "ask-1",
            json!({"questions": [{
                "id": "pkg",
                "question": "Which package manager should I use?",
                "options": [{"label": "pnpm", "description": "Use pnpm workspaces."}]
            }]}),
            AbortSignal::default(),
        ))
        .await;
    assert!(!result.is_error());
    assert_eq!(
        result.content(),
        [ContentBlock::Text {
            text: "{\"answers\":[{\"id\":\"pkg\",\"selected\":[\"pnpm\"]}]}".to_owned(),
        }]
    );
    let seen = provider.seen.lock();
    assert_eq!(seen[0].questions[0].id, "pkg");
    assert_eq!(
        seen[0].questions[0].options.as_ref().unwrap()[0].label,
        "pnpm"
    );
}

#[tokio::test]
async fn recommended_labels_pass_through_without_private_schema_fields() {
    let harness = setup(true);
    let provider = Provider::new(AskUserQuestionAnswer {
        answers: vec![answer("pkg", &["pnpm (Recommended)"], None)],
    });
    harness
        .questions
        .register_provider(&harness.context, provider.clone())
        .unwrap();
    harness
        .tools
        .execute(input(
            "recommended",
            json!({"questions": [{
                "id": "pkg", "question": "Which?",
                "options": [{"label": "pnpm (Recommended)"}, {"label": "npm"}]
            }]}),
            AbortSignal::default(),
        ))
        .await;
    let seen = provider.seen.lock();
    let labels = seen[0].questions[0]
        .options
        .as_ref()
        .unwrap()
        .iter()
        .map(|option| option.label.as_str())
        .collect::<Vec<_>>();
    assert_eq!(labels, ["pnpm (Recommended)", "npm"]);
}

#[tokio::test]
async fn projects_custom_and_multiselect_answers_exactly() {
    let harness = setup(true);
    let expected = AskUserQuestionAnswer {
        answers: vec![
            answer("targets", &["tests", "docs"], Some("release notes")),
            answer("labels-only", &["tests"], None),
            answer("notes", &[], Some("ship today")),
        ],
    };
    harness
        .questions
        .register_provider(&harness.context, Provider::new(expected.clone()))
        .unwrap();
    let result = harness
        .tools
        .execute(input(
            "multi",
            json!({"questions": [
                {"id": "targets", "question": "What?", "options": [{"label": "tests"}, {"label": "docs"}], "multi_select": true},
                {"id": "labels-only", "question": "Which?", "options": [{"label": "tests"}], "multi_select": true},
                {"id": "notes", "question": "Any note?"}
            ]}),
            AbortSignal::default(),
        ))
        .await;
    let ToolExecutionResult::Success(success) = result else {
        panic!("expected success")
    };
    assert_eq!(success.value, serde_json::to_value(&expected).unwrap());
    assert_eq!(
        success.content,
        [ContentBlock::Text {
            text: serde_json::to_string(&expected).unwrap(),
        }]
    );
}

#[tokio::test]
async fn forwards_the_exact_tool_abort_signal() {
    let harness = setup(true);
    let provider = Provider::new(AskUserQuestionAnswer {
        answers: vec![answer("continue", &["ok"], None)],
    });
    harness
        .questions
        .register_provider(&harness.context, provider.clone())
        .unwrap();
    let signal = AbortSignal::default();
    harness
        .tools
        .execute(input(
            "signal",
            json!({"questions": [{"id": "continue", "question": "Continue?"}]}),
            signal.clone(),
        ))
        .await;
    assert_eq!(provider.seen.lock()[0].signal.as_ref(), Some(&signal));
}

#[tokio::test]
async fn forwards_optional_header_and_exact_resumed_runtime_root() {
    let harness = setup(true);
    let provider = Provider::new(AskUserQuestionAnswer {
        answers: vec![answer("continue", &["ok"], None)],
    });
    harness
        .questions
        .register_provider(&harness.context, provider.clone())
        .unwrap();
    let resumed = agent("resumed-root");
    harness.agents.enter(resumed.clone(), None).unwrap();
    let result = harness
        .tools
        .execute(
            input(
                "resumed",
                json!({"questions": [{"id": "continue", "header": "Confirm", "question": "Continue?"}]}),
                AbortSignal::default(),
            )
            .with_agent(resumed.clone()),
        )
        .await;
    assert!(!result.is_error());
    let seen = provider.seen.lock();
    assert_eq!(seen[0].questions[0].header.as_deref(), Some("Confirm"));
    assert!(Arc::ptr_eq(seen[0].agent.as_ref().unwrap(), &resumed));
}

#[tokio::test]
async fn user_question_errors_keep_their_structured_name_and_code() {
    let harness = setup(true);
    let result = harness
        .tools
        .execute(input(
            "no-provider",
            json!({"questions": [{"id": "continue", "question": "Continue?"}]}),
            AbortSignal::default(),
        ))
        .await;
    let info = result.error().unwrap().info.as_ref().unwrap();
    assert_eq!(info.name, "UserQuestionError");
    assert_eq!(info.code, "NO_PROVIDER");
}

#[tokio::test]
async fn delegated_live_agent_returns_the_exact_structured_failure() {
    let harness = setup(true);
    let provider = Provider::new(AskUserQuestionAnswer {
        answers: vec![answer("continue", &["ok"], None)],
    });
    harness
        .questions
        .register_provider(&harness.context, provider.clone())
        .unwrap();
    let root = agent("root");
    let child = agent("child");
    harness.agents.enter(root.clone(), None).unwrap();
    harness.agents.enter(child.clone(), Some(root)).unwrap();
    let result = harness
        .tools
        .execute(
            input(
                "delegated",
                json!({"questions": [{"id": "continue", "question": "Continue?"}]}),
                AbortSignal::default(),
            )
            .with_agent(child),
        )
        .await;
    let failure = result.error().unwrap();
    let info = failure.info.as_ref().unwrap();
    assert_eq!(info.name, "UserQuestionError");
    assert_eq!(info.code, "DELEGATED_CALLER");
    assert_eq!(
        failure.message,
        "human interaction is unavailable while the calling agent is owned by another live agent; include the unresolved question or decision in the child agent's final result"
    );
    assert!(provider.seen.lock().is_empty());
}

#[tokio::test]
async fn empty_questions_return_structured_failure() {
    let harness = setup(true);
    let result = harness
        .tools
        .execute(input(
            "empty",
            json!({"questions": []}),
            AbortSignal::default(),
        ))
        .await;
    let info = result.error().unwrap().info.as_ref().unwrap();
    assert_eq!(info.name, "UserQuestionError");
    assert_eq!(info.code, "EMPTY_QUESTIONS");
}

#[tokio::test]
async fn disposing_registration_removes_the_tool() {
    let harness = setup(false);
    let effect = apply(&harness.context).unwrap();
    assert!(harness.tools.get(TOOL_NAME, None).is_some());
    effect.dispose().await.unwrap();
    assert!(harness.tools.get(TOOL_NAME, None).is_none());
}
