//! Credentialed mirror of the source's ACP backend with-key e2e.
//!
//! The backend spawns the real `seekdeep-acp-demo` example (the compiled counterpart of the
//! source's `acp-agent` example bin), speaks ACP over stdio, and returns its real model answer:
//! the out-of-process counterpart to the in-process spawn coverage. Both round trips are ignored
//! even when `DEEPSEEK_API_KEY` is present because they spend the account's quota; run them
//! deliberately with
//! `DEEPSEEK_API_KEY=... cargo test -p seekdeep-acp-demo --test subagent_acp_e2e -- --ignored`.
//! The child's `SEEKDEEP_HOME` and `SEEKDEEP_AGENTS_HOME` point under the temporary root so a
//! deliberate run leaves nothing in the developer's real home; the launch resolution itself runs
//! keylessly.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use seekdeep_acp::PermissionPolicy;
use seekdeep_agent::{Agent, AgentOptions, Inbox, NoopInboxNotifications};
use seekdeep_cordis::Context;
use seekdeep_core::session::{Session, SessionHeader, SessionId};
use seekdeep_llm::{AbortSignal, ContentBlock};
use seekdeep_loader_smoke::{ExampleLaunch, ExampleLaunchOptions, resolve_example_launch};
use seekdeep_scope::ScopeKey;
use seekdeep_subagent::{SubagentRuntime, SubagentStartRequest, SubagentStopReason};
use seekdeep_subagent_acp::{Config, apply};
use seekdeep_subprocess_local::LocalSubprocessRuntime;

const DEMO: &str = env!("CARGO_BIN_EXE_seekdeep-acp-demo");

fn live_key() -> Option<String> {
    std::env::var("DEEPSEEK_API_KEY")
        .ok()
        .filter(|key| !key.is_empty())
}

fn example_config() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/acp-agent/cordis.yml")
}

/// How to launch the child acp-agent: the compiled example with the live `DeepSeek` config.
///
/// The subprocess seam scrubs ambient credentials while the spec's env merges after it, so the
/// model key (and any base URL override) is forwarded explicitly, together with the permission
/// mode override and the isolated homes.
fn child_launch(root: &Path) -> anyhow::Result<ExampleLaunch> {
    let mut environment = BTreeMap::new();
    for name in ["DEEPSEEK_API_KEY", "DEEPSEEK_BASE_URL"] {
        if let Some(value) = std::env::var_os(name) {
            environment.insert(OsString::from(name), value);
        }
    }
    environment.insert(
        OsString::from("SEEKDEEP_PERMISSION_MODE"),
        OsString::from("danger-full-access"),
    );
    environment.insert(
        OsString::from("SEEKDEEP_HOME"),
        root.join(".seekdeep").into_os_string(),
    );
    environment.insert(
        OsString::from("SEEKDEEP_AGENTS_HOME"),
        root.join(".agents").into_os_string(),
    );
    resolve_example_launch(ExampleLaunchOptions {
        source_bin: PathBuf::from(DEMO),
        library_bin: Some(PathBuf::from(DEMO)),
        config_args: vec![
            OsString::from("--config"),
            example_config().into_os_string(),
        ],
        mode: None,
        environment,
    })
}

/// The ACP backend ignores the parent, but the seam requires one.
fn fake_parent(context: &Context) -> Arc<Agent> {
    let id = SessionId::new("parent");
    let header = SessionHeader::new(id.clone());
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
            ContentBlock::Text { text } => Some(text.as_str().expect("the answer is scalar text")),
            _ => None,
        })
        .collect()
}

struct Harness {
    context: Context,
    subagents: Arc<SubagentRuntime>,
    workdir: tempfile::TempDir,
}

impl Harness {
    fn new(permission: PermissionPolicy) -> anyhow::Result<Self> {
        let workdir = tempfile::Builder::new()
            .prefix("seekdeep-subagent-acp-e2e-")
            .tempdir()?;
        let launch = child_launch(workdir.path())?;
        let context = Context::new();
        let subagents = SubagentRuntime::install(&context)?;
        LocalSubprocessRuntime::install(&context)?;
        apply(
            &context,
            Config {
                provider_name: "acp".to_owned(),
                command: launch.command.to_string_lossy().into_owned(),
                args: launch
                    .args
                    .iter()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect(),
                cwd: Some(workdir.path().to_string_lossy().into_owned()),
                permission,
                env: launch
                    .environment
                    .iter()
                    .map(|(name, value)| {
                        (
                            name.to_string_lossy().into_owned(),
                            value.to_string_lossy().into_owned(),
                        )
                    })
                    .collect(),
                ..Config::default()
            },
        )?;
        Ok(Self {
            context,
            subagents,
            workdir,
        })
    }

    async fn run(&self, prompt: &str) -> anyhow::Result<seekdeep_subagent::SubagentResult> {
        let run = self
            .subagents
            .start(
                "acp",
                SubagentStartRequest {
                    label: None,
                    prompt: vec![ContentBlock::Text {
                        text: prompt.into(),
                    }],
                    parent: fake_parent(&self.context),
                    signal: AbortSignal::default(),
                    agent_options: None,
                    output_schema: None,
                    max_depth: None,
                    tool_filter: None,
                    persona: None,
                },
            )
            .await?;
        let result = tokio::time::timeout(Duration::from_secs(180), run.result()).await??;
        run.dispose().await?;
        Ok(result)
    }

    async fn dispose(self) -> anyhow::Result<()> {
        self.context.fiber().dispose().await
    }
}

#[tokio::test]
#[ignore = "drives the real acp-agent example against the real DeepSeek API and consumes account quota"]
async fn drives_the_real_acp_agent_example_process_to_answer_a_prompt() -> anyhow::Result<()> {
    if live_key().is_none() {
        return Ok(());
    }
    let harness = Harness::new(PermissionPolicy::Reject)?;
    let result = harness
        .run("Reply with exactly the word PONG and nothing else. Do not use any tools.")
        .await?;
    // The real child process completed its turn and streamed a real answer back across the ACP
    // boundary.
    assert_eq!(result.stop_reason, SubagentStopReason::Completed);
    let text = result_text(&result);
    assert!(!text.is_empty());
    assert!(text.to_uppercase().contains("PONG"));
    harness.dispose().await
}

#[tokio::test]
#[ignore = "drives the real acp-agent example against the real DeepSeek API and consumes account quota"]
async fn drives_the_child_to_do_real_file_work_via_its_own_bash_tool() -> anyhow::Result<()> {
    if live_key().is_none() {
        return Ok(());
    }
    // The child needs to act (run bash), so approve its permission prompts.
    let harness = Harness::new(PermissionPolicy::Allow)?;
    let result = harness
        .run(
            "Use the bash tool to write the text ACP_CHILD_WAS_HERE into a file named proof.txt \
             in the current directory. Then reply DONE.",
        )
        .await?;
    assert_eq!(result.stop_reason, SubagentStopReason::Completed);
    // Assert the filesystem effect independently of the model response.
    let proof = std::fs::read_to_string(harness.workdir.path().join("proof.txt"))?;
    assert!(proof.contains("ACP_CHILD_WAS_HERE"));
    harness.dispose().await
}

#[test]
fn the_child_is_the_compiled_example_with_the_live_config_and_an_explicit_environment() {
    let root = tempfile::tempdir().unwrap();
    let launch = child_launch(root.path()).unwrap();
    assert_eq!(launch.command, PathBuf::from(DEMO));
    assert_eq!(
        launch.args,
        vec![
            OsString::from("--config"),
            example_config().into_os_string()
        ]
    );
    assert!(example_config().is_file());
    assert_eq!(
        launch
            .environment
            .get(&OsString::from("SEEKDEEP_PERMISSION_MODE")),
        Some(&OsString::from("danger-full-access"))
    );
    assert_eq!(
        launch.environment.get(&OsString::from("SEEKDEEP_HOME")),
        Some(&root.path().join(".seekdeep").into_os_string())
    );
    assert_eq!(
        launch
            .environment
            .get(&OsString::from("SEEKDEEP_AGENTS_HOME")),
        Some(&root.path().join(".agents").into_os_string())
    );
    for name in ["DEEPSEEK_API_KEY", "DEEPSEEK_BASE_URL"] {
        assert_eq!(
            launch.environment.get(&OsString::from(name)),
            std::env::var_os(name).as_ref()
        );
    }
}
