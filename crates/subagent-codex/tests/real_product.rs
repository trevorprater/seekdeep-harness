//! Required keyless real Codex 0.147.0 integration against loopback Responses.

#![cfg(unix)]

mod support;

use std::{collections::BTreeMap, sync::Arc};

use seekdeep_agent::{Agent, AgentOptions, Inbox, NoopInboxNotifications};
use seekdeep_cordis::Context;
use seekdeep_core::session::{Session, SessionHeader, SessionId};
use seekdeep_llm::{AbortSignal, ContentBlock};
use seekdeep_scope::ScopeKey;
use seekdeep_subagent::{SubagentRuntime, SubagentStartRequest, SubagentStopReason};
use seekdeep_subagent_codex::{Config, apply};
use seekdeep_subprocess_local::LocalSubprocessRuntime;
use serde_json::{Map, Value, json};

use support::responses_fixture::{Behavior, ResponsesFixture, response_input_texts};

struct RealHarness {
    context: Context,
    subagents: Arc<SubagentRuntime>,
    subprocess: Arc<LocalSubprocessRuntime>,
    parent: Arc<Agent>,
    workspace: tempfile::TempDir,
    _root: tempfile::TempDir,
}

impl RealHarness {
    fn new(fixture: &ResponsesFixture) -> Self {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir_in(root.path()).unwrap();
        let codex_home = root.path().join("codex-home");
        std::fs::create_dir_all(&codex_home).unwrap();
        std::fs::write(
            codex_home.join("config.toml"),
            format!(
                concat!(
                    "model = \"fixture-model\"\n",
                    "model_provider = \"fixture\"\n",
                    "approval_policy = \"on-request\"\n",
                    "sandbox_mode = \"read-only\"\n",
                    "disable_response_storage = true\n",
                    "check_for_update_on_startup = false\n\n",
                    "[model_providers.fixture]\n",
                    "name = \"Fixture Responses\"\n",
                    "base_url = \"{}\"\n",
                    "env_key = \"OPENAI_API_KEY\"\n",
                    "wire_api = \"responses\"\n",
                    "requires_openai_auth = false\n\n",
                    "[analytics]\n",
                    "enabled = false\n",
                ),
                fixture.base_url
            ),
        )
        .unwrap();
        let env = BTreeMap::from([
            (
                "OPENAI_API_KEY".to_owned(),
                "seekdeep-fake-openai-key".to_owned(),
            ),
            (
                "CODEX_HOME".to_owned(),
                codex_home.to_string_lossy().into_owned(),
            ),
            (
                "HOME".to_owned(),
                root.path().to_string_lossy().into_owned(),
            ),
            (
                "XDG_CONFIG_HOME".to_owned(),
                root.path().join("xdg").to_string_lossy().into_owned(),
            ),
            ("PATH".to_owned(), std::env::var("PATH").unwrap_or_default()),
            ("HTTP_PROXY".to_owned(), String::new()),
            ("HTTPS_PROXY".to_owned(), String::new()),
            ("ALL_PROXY".to_owned(), String::new()),
            ("NO_PROXY".to_owned(), "127.0.0.1,localhost".to_owned()),
        ]);
        let context = Context::new();
        let subagents = SubagentRuntime::install(&context).unwrap();
        let subprocess = LocalSubprocessRuntime::install(&context).unwrap();
        apply(
            &context,
            Config {
                env,
                dispose_grace_ms: 2_000.0,
            },
        )
        .unwrap();
        let parent = agent(&context, &workspace.path().to_string_lossy());
        Self {
            context,
            subagents,
            subprocess,
            parent,
            workspace,
            _root: root,
        }
    }

    async fn run(
        &self,
        task: &str,
        signal: AbortSignal,
    ) -> Arc<dyn seekdeep_subagent::SubagentRun> {
        self.subagents
            .start(
                "codex",
                SubagentStartRequest {
                    label: None,
                    prompt: vec![ContentBlock::Text {
                        text: task.to_owned(),
                    }],
                    parent: Arc::clone(&self.parent),
                    signal,
                    agent_options: None,
                    output_schema: None,
                    max_depth: None,
                    tool_filter: None,
                    persona: None,
                },
            )
            .await
            .unwrap()
    }

    async fn quiescent(&self) {
        for _ in 0..200 {
            if self.subprocess.live_process_count() == 0 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(self.subprocess.live_process_count(), 0);
    }

    async fn dispose(self) {
        self.context.fiber().dispose().await.unwrap();
    }
}

fn agent(context: &Context, cwd: &str) -> Arc<Agent> {
    let id = SessionId::new("real-parent");
    let mut header = SessionHeader::new(id.clone());
    header.cwd = Some(cwd.to_owned());
    let session = Session::create(&id, None, Some(header)).unwrap();
    let inbox = Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
    Arc::new(Agent::new(
        id,
        AgentOptions::default(),
        session,
        inbox,
        context.clone(),
        ScopeKey::new(),
    ))
}

fn result_text(result: &seekdeep_subagent::SubagentResult) -> String {
    result
        .output
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn advertised_functions(body: &Map<String, Value>) -> Vec<String> {
    body.get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|tool| tool.get("type") == Some(&json!("function")))
        .filter_map(|tool| tool.get("name").and_then(Value::as_str).map(str::to_owned))
        .collect()
}

#[tokio::test]
async fn passes_exact_task_and_authentication_to_real_codex_and_returns_exact_text() {
    let version = std::process::Command::new("codex")
        .arg("--version")
        .output()
        .unwrap();
    assert!(version.status.success());
    assert_eq!(
        String::from_utf8(version.stdout).unwrap().trim(),
        "codex-cli 0.147.0"
    );
    let sentinel = "REAL_CODEX_SENTINEL_0_147_0";
    let task = "Return the fixture sentinel exactly.";
    let fixture = ResponsesFixture::start(vec![Behavior::Complete {
        text: sentinel.to_owned(),
    }])
    .await
    .unwrap();
    let harness = RealHarness::new(&fixture);
    let run = harness.run(task, AbortSignal::default()).await;
    let result = tokio::time::timeout(std::time::Duration::from_secs(30), run.result())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.stop_reason, SubagentStopReason::Completed);
    assert_eq!(result_text(&result), sentinel);
    run.dispose().await.unwrap();
    harness.quiescent().await;
    {
        let requests = fixture.requests.lock();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].path, "/v1/responses");
        assert_eq!(
            requests[0].authorization.as_deref(),
            Some("Bearer seekdeep-fake-openai-key")
        );
        assert!(
            response_input_texts(&requests[0].body)
                .iter()
                .any(|text| text == task)
        );
    }
    harness.dispose().await;
    fixture.close();
}

#[tokio::test]
async fn cancels_real_codex_command_approval_without_executing_the_command() {
    let choices = vec![
        (
            "exec_command".to_owned(),
            Map::from_iter([
                ("cmd".to_owned(), json!("touch approval-side-effect")),
                ("sandbox_permissions".to_owned(), json!("require_escalated")),
                (
                    "justification".to_owned(),
                    json!("exercise approval boundary"),
                ),
            ]),
        ),
        (
            "shell_command".to_owned(),
            Map::from_iter([
                ("command".to_owned(), json!("touch approval-side-effect")),
                ("sandbox_permissions".to_owned(), json!("require_escalated")),
                (
                    "justification".to_owned(),
                    json!("exercise approval boundary"),
                ),
            ]),
        ),
    ];
    let fixture = ResponsesFixture::start(vec![Behavior::AdvertisedFunctionCall { choices }])
        .await
        .unwrap();
    let harness = RealHarness::new(&fixture);
    let side_effect = harness.workspace.path().join("approval-side-effect");
    let run = harness
        .run("Attempt the fixture command.", AbortSignal::default())
        .await;
    let result = tokio::time::timeout(std::time::Duration::from_secs(30), run.result())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.stop_reason, SubagentStopReason::Error);
    run.dispose().await.unwrap();
    assert!(!side_effect.exists());
    harness.quiescent().await;
    {
        let requests = fixture.requests.lock();
        assert_eq!(requests.len(), 1);
        let names = advertised_functions(&requests[0].body);
        assert!(
            names
                .iter()
                .any(|name| matches!(name.as_str(), "exec_command" | "shell_command"))
        );
        assert_eq!(
            requests[0].authorization.as_deref(),
            Some("Bearer seekdeep-fake-openai-key")
        );
    }
    harness.dispose().await;
    fixture.close();
}

#[tokio::test]
async fn settles_real_product_cancellation_locally_and_leaves_no_process_tree() {
    let fixture = ResponsesFixture::start(vec![Behavior::Hold]).await.unwrap();
    let harness = RealHarness::new(&fixture);
    let signal = AbortSignal::default();
    let run = harness.run("Wait for cancellation.", signal.clone()).await;
    fixture.wait_started().await;
    signal.abort_with_reason(json!("real product cancellation"));
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), run.result())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.stop_reason, SubagentStopReason::Aborted);
    run.dispose().await.unwrap();
    harness.quiescent().await;
    harness.dispose().await;
    fixture.close();
}
