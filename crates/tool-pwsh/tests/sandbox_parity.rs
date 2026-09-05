//! Pwsh-tool sandbox composition, policy, and approval parity.

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use async_trait::async_trait;
use parking_lot::Mutex;
use seekdeep_agent::{AGENTS, Agent, AgentOptions, AgentRegistry, Inbox, NoopInboxNotifications};
use seekdeep_cordis::{Context, EventOptions};
use seekdeep_core::session::{AppendOptions, Session, SessionHeader, SessionId};
use seekdeep_jobs_local::{Config as JobsConfig, LocalJobRegistry};
use seekdeep_llm::{AbortSignal, CallId, ContentBlock};
use seekdeep_sandbox::{SandboxEnforcement, SandboxMode};
use seekdeep_sandbox_policy::SandboxPolicyConfig;
use seekdeep_scope::ScopeKey;
use seekdeep_shell::{
    CollectedOutput, ShellExecRequest, ShellExecSpec, ShellExecutor, ShellProcess,
    ShellProcessHandle, ShellProcessRead, ShellProcessStatus, ShellRunResult, ShellSandboxInfo,
    ShellService,
};
use seekdeep_shell_env::ShellEnvConfig;
use seekdeep_system_prompt::SystemPromptConfig;
use seekdeep_tool_pwsh::{Config, apply};
use seekdeep_tools::{
    ToolExecutionInput, ToolExecutionResult, ToolPresentationMode, ToolRuntime, ToolRuntimeConfig,
};
use seekdeep_user_approval::{ApprovalAnswer, ApprovalConfig, ApprovalOutcome, ApprovalService};
use serde_json::{Value, json};

#[derive(Debug)]
struct SettledProcess;

#[async_trait]
impl ShellProcess for SettledProcess {
    fn status(&self) -> ShellProcessStatus {
        ShellProcessStatus::Completed
    }

    fn exit_code(&self) -> Option<i32> {
        Some(0)
    }

    fn signal(&self) -> Option<seekdeep_shell::ProcessSignal> {
        None
    }

    fn sandbox(&self) -> Option<ShellSandboxInfo> {
        None
    }

    async fn done(&self) {}

    fn read_output(&self) -> ShellProcessRead {
        ShellProcessRead::default()
    }

    fn kill(&self) -> bool {
        false
    }
}

#[derive(Debug, Default)]
struct RecordingSandboxExecutor {
    specs: Mutex<Vec<ShellExecSpec>>,
}

impl RecordingSandboxExecutor {
    fn modes(&self) -> Vec<Option<SandboxMode>> {
        self.specs
            .lock()
            .iter()
            .map(|spec| spec.sandbox_policy.as_ref().map(|policy| policy.mode))
            .collect()
    }
}

#[async_trait]
impl ShellExecutor for RecordingSandboxExecutor {
    fn sandbox_mode(&self) -> Option<SandboxMode> {
        Some(SandboxMode::ReadOnly)
    }

    fn resolve(&self, request: ShellExecRequest) -> anyhow::Result<ShellExecSpec> {
        Ok(ShellExecSpec {
            command: request.command,
            workdir: request
                .workdir
                .unwrap_or_else(|| PathBuf::from("/sandbox-workspace")),
            timeout_ms: request.timeout_ms.unwrap_or(1_000.0),
            stdout_max_bytes: request.stdout_max_bytes.unwrap_or(64_000.0),
            signal: request.signal,
            stdin: request.stdin,
            env: request.env,
            seekdeep_env: request.seekdeep_env,
            sandbox_policy: request.sandbox_policy,
        })
    }

    async fn run(&self, spec: ShellExecSpec) -> anyhow::Result<ShellRunResult> {
        self.specs.lock().push(spec.clone());
        let policy = spec
            .sandbox_policy
            .as_ref()
            .expect("confining tool passes policy");
        Ok(ShellRunResult {
            exit_code: Some(0),
            signal: None,
            timed_out: false,
            aborted: false,
            timeout_ms: spec.timeout_ms,
            stdout: CollectedOutput {
                text: "ok".to_owned(),
                truncated: false,
                spill_path: None,
            },
            stderr: CollectedOutput::default(),
            sandbox: Some(ShellSandboxInfo {
                mode: policy.mode,
                denied: false,
                enforcement: (spec.command != "without optional sandbox facts")
                    .then_some(SandboxEnforcement::Full),
                runner_failed: (spec.command != "without optional sandbox facts").then_some(false),
            }),
        })
    }

    fn start(&self, spec: ShellExecSpec) -> anyhow::Result<ShellProcessHandle> {
        self.specs.lock().push(spec);
        Ok(Arc::new(SettledProcess))
    }
}

struct Harness {
    context: Context,
    tools: Arc<ToolRuntime>,
    shell: Arc<RecordingSandboxExecutor>,
    approval: Option<Arc<ApprovalService>>,
}

fn base_context() -> (Context, Arc<ToolRuntime>) {
    let context = Context::new();
    let agents = Arc::new(AgentRegistry::new(context.clone()));
    agents.provide(&context).expect("agents");
    let prompt =
        seekdeep_system_prompt::install(&context, SystemPromptConfig::default()).expect("prompt");
    let tools = seekdeep_tools::install(
        &context,
        &prompt,
        ToolRuntimeConfig {
            mode: ToolPresentationMode::Native,
            ..ToolRuntimeConfig::default()
        },
    )
    .expect("tools");
    seekdeep_shell_env::apply(&context, &ShellEnvConfig::default()).expect("shell env");
    (context, tools)
}

fn sandbox_harness(with_approval: bool, jobs: bool) -> Harness {
    let (context, tools) = base_context();
    seekdeep_sandbox_policy::install(
        &context,
        SandboxPolicyConfig {
            workspace_root: Some(PathBuf::from("/sandbox-workspace")),
            ..SandboxPolicyConfig::default()
        },
    )
    .expect("sandbox policy");
    let shell = Arc::new(RecordingSandboxExecutor::default());
    let erased: Arc<dyn ShellExecutor> = shell.clone();
    ShellService::new(erased)
        .provide(&context)
        .expect("shell service");
    if jobs {
        LocalJobRegistry::new(&context, JobsConfig::default()).expect("jobs");
        seekdeep_tool_jobs::apply(&context, &seekdeep_tool_jobs::Config::default())
            .expect("tool jobs");
    }
    let approval = with_approval.then(|| {
        let installation =
            seekdeep_user_approval::install(&context, ApprovalConfig::default()).expect("approval");
        installation.service()
    });
    apply(&context, Config::default()).expect("tool pwsh");
    Harness {
        context,
        tools,
        shell,
        approval,
    }
}

fn agent(context: &Context, mode: Option<SandboxMode>) -> Arc<Agent> {
    static NEXT_AGENT: AtomicU64 = AtomicU64::new(0);
    let id = SessionId::new(format!(
        "sandbox-session-{}",
        NEXT_AGENT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut header = SessionHeader::new(id.clone());
    header.cwd = Some("/session-workspace".to_owned());
    let session = Session::create(&id, None, Some(header)).expect("session");
    session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .expect("turn start");
    if let Some(mode) = mode {
        session
            .append(
                "sandbox/mode",
                json!({"mode": mode}),
                AppendOptions::default(),
            )
            .expect("sandbox mode");
    }
    let inbox =
        Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).expect("inbox"));
    let agent = Arc::new(Agent::new(
        id,
        AgentOptions::default(),
        session,
        inbox,
        context.clone(),
        ScopeKey::new(),
    ));
    context
        .get(AGENTS)
        .expect("agent registry")
        .register(context, &agent, None)
        .expect("register agent");
    agent
}

async fn call(
    harness: &Harness,
    arguments: Value,
    owner: Option<Arc<Agent>>,
) -> ToolExecutionResult {
    let input = ToolExecutionInput::new(
        CallId::new("sandbox-call"),
        "pwsh",
        arguments,
        AbortSignal::default(),
    );
    let input = match owner {
        Some(owner) => input.with_agent(owner),
        None => input,
    };
    harness.tools.execute(input).await
}

fn text(result: &ToolExecutionResult) -> String {
    result
        .content()
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn escalation(mode: &str) -> Value {
    json!({
        "command": "true",
        "description": "test escalation",
        "sandbox_permissions": mode,
        "justification": "the command needs wider access"
    })
}

#[test]
fn split_sandbox_composition_fails_before_tool_registration() {
    let (context, tools) = base_context();
    let shell: Arc<dyn ShellExecutor> = Arc::new(RecordingSandboxExecutor::default());
    ShellService::new(shell).provide(&context).expect("shell");
    let error = apply(&context, Config::default()).expect_err("must fail");
    assert!(error.to_string().contains("ctx.sandboxPolicy is missing"));
    assert!(tools.get("pwsh", None).is_none());
}

#[tokio::test]
async fn fields_pair_and_unroutable_or_nonwidening_escalations_fail_before_execution() {
    let harness = sandbox_harness(false, false);
    let schema = harness.tools.schemas(None).remove(0);
    assert_eq!(
        schema.parameters["properties"]["sandbox_permissions"]["enum"],
        json!(["workspace-write", "danger-full-access"])
    );
    assert!(schema.description.contains("approval prompt"));
    for arguments in [
        json!({"command":"true", "description":"d", "sandbox_permissions":"workspace-write"}),
        json!({"command":"true", "description":"d", "justification":"why"}),
        json!({
            "command":"true",
            "description":"d",
            "sandbox_permissions":"workspace-write",
            "justification":" "
        }),
    ] {
        assert!(call(&harness, arguments, None).await.is_error());
    }

    let owner = agent(&harness.context, None);
    let no_service = call(&harness, escalation("workspace-write"), Some(owner)).await;
    assert!(text(&no_service).contains("no approval service"));
    assert!(harness.shell.modes().is_empty());

    let with_approval = sandbox_harness(true, false);
    let no_agent = call(&with_approval, escalation("workspace-write"), None).await;
    assert!(text(&no_agent).contains("no agent to route"));
    let owner = agent(&with_approval.context, None);
    let no_channel = call(&with_approval, escalation("workspace-write"), Some(owner)).await;
    assert!(text(&no_channel).contains("no approval channel"));

    let nonwidening_owner = agent(&with_approval.context, Some(SandboxMode::WorkspaceWrite));
    let nonwidening = call(
        &with_approval,
        escalation("workspace-write"),
        Some(nonwidening_owner),
    )
    .await;
    assert!(text(&nonwidening).contains("not strictly wider"));
    assert!(with_approval.shell.modes().is_empty());
}

#[tokio::test]
async fn approval_outcomes_map_distinctly_and_grants_stamp_foreground_and_background_policy() {
    for (outcome, expected) in [
        (ApprovalOutcome::Rejected, "user rejected"),
        (ApprovalOutcome::Cancelled, "was cancelled"),
    ] {
        let harness = sandbox_harness(true, false);
        harness
            .approval
            .as_ref()
            .unwrap()
            .on_request(
                &harness.context,
                move |_, _| async move { Ok(ApprovalAnswer::Outcome(outcome)) },
                EventOptions::default(),
            )
            .expect("answerer");
        let result = call(
            &harness,
            escalation("workspace-write"),
            Some(agent(&harness.context, None)),
        )
        .await;
        assert!(text(&result).contains(expected), "{}", text(&result));
        assert!(harness.shell.modes().is_empty());
    }

    let harness = sandbox_harness(true, true);
    harness
        .approval
        .as_ref()
        .unwrap()
        .on_request(
            &harness.context,
            |_, _| async { Ok(ApprovalOutcome::AllowedOnce.into()) },
            EventOptions::default(),
        )
        .expect("answerer");
    let owner = agent(&harness.context, None);
    let foreground = call(&harness, escalation("workspace-write"), Some(owner.clone())).await;
    assert!(!foreground.is_error(), "{}", text(&foreground));
    let mut background = escalation("workspace-write");
    background["run_in_background"] = json!(true);
    let background = call(&harness, background, Some(owner)).await;
    assert_eq!(text(&background), "started background job pwsh-1");
    assert_eq!(
        harness.shell.modes(),
        vec![
            Some(SandboxMode::WorkspaceWrite),
            Some(SandboxMode::WorkspaceWrite)
        ]
    );
}

#[tokio::test]
async fn session_override_sets_root_and_widening_floor_while_optional_facts_stay_absent() {
    let harness = sandbox_harness(true, false);
    harness
        .approval
        .as_ref()
        .unwrap()
        .on_request(
            &harness.context,
            |_, _| async { Ok(ApprovalOutcome::AllowedOnce.into()) },
            EventOptions::default(),
        )
        .expect("answerer");
    let owner = agent(&harness.context, Some(SandboxMode::WorkspaceWrite));
    let ordinary = call(
        &harness,
        json!({"command":"without optional sandbox facts", "description":"ordinary"}),
        Some(owner.clone()),
    )
    .await;
    assert!(!ordinary.is_error());
    let sandbox = ordinary
        .value()
        .and_then(|value| value.get("sandbox"))
        .expect("sandbox value");
    assert_eq!(sandbox["mode"], json!("workspace-write"));
    assert!(sandbox.get("enforcement").is_none());
    assert!(sandbox.get("runnerFailed").is_none());

    let widened = call(&harness, escalation("danger-full-access"), Some(owner)).await;
    assert!(!widened.is_error(), "{}", text(&widened));
    let specs = harness.shell.specs.lock();
    assert_eq!(specs[0].workdir, PathBuf::from("/session-workspace"));
    assert_eq!(
        specs
            .iter()
            .map(|spec| spec.sandbox_policy.as_ref().unwrap().mode)
            .collect::<Vec<_>>(),
        vec![SandboxMode::WorkspaceWrite, SandboxMode::DangerFullAccess]
    );
}
