//! Model-facing Bash tool parity over the real local process and Jobs providers.

#![cfg(not(windows))]

use std::{path::Path, sync::Arc, time::Duration};

use seekdeep_agent::{AGENTS, Agent, AgentOptions, AgentRegistry, Inbox, NoopInboxNotifications};
use seekdeep_bash_local::{Config as BashConfig, LocalBashExecutor};
use seekdeep_cordis::Context;
use seekdeep_core::session::{Session, SessionHeader, SessionId};
use seekdeep_jobs_local::{Config as JobsConfig, LocalJobRegistry};
use seekdeep_llm::{AbortSignal, CallId, ContentBlock};
use seekdeep_scope::ScopeKey;
use seekdeep_shell::ShellExecutor;
use seekdeep_shell_env::ShellEnvConfig;
use seekdeep_subprocess_local::LocalSubprocessRuntime;
use seekdeep_system_prompt::{AssembleContext, SystemPrompt, SystemPromptConfig};
use seekdeep_tool_bash::{Config, apply};
use seekdeep_tools::{
    ToolExecutionInput, ToolExecutionResult, ToolPresentationMode, ToolRuntime, ToolRuntimeConfig,
};
use serde_json::{Value, json};
use tempfile::TempDir;

struct Harness {
    context: Context,
    _spill: TempDir,
    prompt: Arc<SystemPrompt>,
    tools: Arc<ToolRuntime>,
    subprocess: Arc<LocalSubprocessRuntime>,
    bash: Arc<LocalBashExecutor>,
}

async fn harness(config: Config, jobs: bool, controller: bool) -> Harness {
    let context = Context::new();
    let agents = Arc::new(AgentRegistry::new(context.clone()));
    agents.provide(&context).expect("agents");
    let prompt = seekdeep_system_prompt::install(&context, SystemPromptConfig::default())
        .expect("system prompt");
    let tools = seekdeep_tools::install(
        &context,
        &prompt,
        ToolRuntimeConfig {
            mode: ToolPresentationMode::Native,
            ..ToolRuntimeConfig::default()
        },
    )
    .expect("tools");
    let spill = tempfile::tempdir().expect("spill");
    let subprocess = LocalSubprocessRuntime::install_runtime(
        &context,
        Arc::new(LocalSubprocessRuntime::with_spill_dir(spill.path())),
    )
    .expect("subprocess");
    seekdeep_shell_env::apply(&context, &ShellEnvConfig::default()).expect("shell env");
    let bash = seekdeep_bash_local::apply(
        &context,
        BashConfig {
            timeout_ms: 10_000.0,
            grace_ms: 200.0,
            ..BashConfig::default()
        },
    )
    .await
    .expect("bash provider");
    if jobs {
        LocalJobRegistry::new(&context, JobsConfig::default()).expect("jobs");
        if controller {
            seekdeep_tool_jobs::apply(&context, &seekdeep_tool_jobs::Config::default())
                .expect("tool jobs");
        }
    }
    apply(&context, config).expect("tool bash");
    Harness {
        context,
        _spill: spill,
        prompt,
        tools,
        subprocess,
        bash,
    }
}

async fn call_with(
    harness: &Harness,
    name: &str,
    arguments: Value,
    signal: AbortSignal,
    agent: Option<Arc<Agent>>,
) -> ToolExecutionResult {
    let input = ToolExecutionInput::new(CallId::new("tool-bash-call"), name, arguments, signal);
    let input = match agent {
        Some(agent) => input.with_agent(agent),
        None => input,
    };
    harness.tools.execute(input).await
}

async fn call(harness: &Harness, name: &str, arguments: Value) -> ToolExecutionResult {
    call_with(harness, name, arguments, AbortSignal::default(), None).await
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

fn agent(context: &Context, id: &str, cwd: Option<&str>) -> Arc<Agent> {
    let id = SessionId::new(id);
    let mut header = SessionHeader::new(id.clone());
    header.cwd = cwd.map(str::to_owned);
    let session = Session::create(&id, None, Some(header)).expect("session");
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

async fn call_until(harness: &Harness, name: &str, arguments: Value, expected: &str) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut last = String::new();
    while tokio::time::Instant::now() < deadline {
        let result = call(harness, name, arguments.clone()).await;
        last = text(&result);
        if last.contains(expected) {
            return last;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("{name} never returned {expected:?}; last output: {last:?}");
}

#[tokio::test]
async fn foreground_commands_render_output_exit_timeout_abort_workdir_and_failures() {
    let harness = harness(Config::default(), false, false).await;
    for (command, expected) in [
        ("echo hello", "hello\n"),
        ("true", "(no output)"),
        ("echo out; echo err >&2", "out\n[stderr]\nerr\n"),
        ("echo failing; exit 3", "failing\n[exit code: 3]"),
    ] {
        let result = call(
            &harness,
            "bash",
            json!({"command": command, "description": "test command"}),
        )
        .await;
        assert!(!result.is_error(), "{}", text(&result));
        assert_eq!(text(&result), expected);
    }

    let timeout = call(
        &harness,
        "bash",
        json!({"command": "sleep 60", "description": "test command", "timeoutMs": 100}),
    )
    .await;
    assert!(!timeout.is_error());
    assert_eq!(
        text(&timeout),
        "(no output)\n[timed out after 100ms]\n[killed by signal: SIGTERM]"
    );

    let workdir = call(
        &harness,
        "bash",
        json!({"command": "pwd", "description": "print directory", "workdir": "/tmp"}),
    )
    .await;
    assert!(text(&workdir).trim().ends_with("/tmp"));

    let missing = call(
        &harness,
        "bash",
        json!({
            "command": "true",
            "description": "test command",
            "workdir": "/nonexistent-seekdeep-tool-bash"
        }),
    )
    .await;
    assert!(missing.is_error());
    assert!(text(&missing).contains("No such file") || text(&missing).contains("os error 2"));

    let signal = AbortSignal::default();
    let pending = call_with(
        &harness,
        "bash",
        json!({"command": "sleep 60", "description": "test command"}),
        signal.clone(),
        None,
    );
    tokio::pin!(pending);
    tokio::select! {
        result = &mut pending => panic!("command settled before abort: {result:?}"),
        () = tokio::time::sleep(Duration::from_millis(50)) => signal.abort(),
    }
    let aborted = pending.await;
    assert!(aborted.is_error());
    let ToolExecutionResult::Failure(failure) = aborted else {
        panic!("expected failure");
    };
    assert_eq!(failure.error.message, "tool call aborted");
    assert_eq!(
        failure.error.info.as_ref().map(|info| info.name.as_str()),
        Some("AbortError")
    );
    assert_eq!(
        failure.error.info.as_ref().map(|info| info.code.as_str()),
        Some(seekdeep_tools::TOOL_ABORTED)
    );
}

#[tokio::test]
async fn schemas_value_validation_prompt_and_background_opt_out_stay_in_lockstep() {
    let default_harness = harness(Config::default(), false, false).await;
    let schemas = default_harness.tools.schemas(None);
    assert_eq!(schemas.len(), 1);
    assert_eq!(schemas[0].name, "bash");
    assert_eq!(
        schemas[0].parameters["required"],
        json!(["command", "description"])
    );
    assert!(
        schemas[0].parameters["properties"]
            .get("run_in_background")
            .is_some()
    );
    assert!(schemas[0].description.contains("job_output"));
    assert!(schemas[0].description.contains("$SEEKDEEP_*"));
    assert!(!schemas[0].description.contains("SEEKDEEP_SESSION_JSONL"));
    let assembly = default_harness
        .prompt
        .assemble(AssembleContext::default())
        .await
        .expect("prompt");
    assert!(
        assembly.sections.iter().any(|section| {
            section.name == "tool:bash" && section.text.contains("[exit code: N]")
        })
    );

    for arguments in [
        json!({}),
        json!({"command": 42, "description": "d"}),
        json!({"command": "x"}),
        json!({"command": "x", "description": 7}),
        json!({"command": "x", "description": "d", "timeoutMs": "soon"}),
        json!({"command": "x", "description": "d", "workdir": 7}),
        json!({"command": "x", "description": "d", "run_in_background": "yes"}),
        json!({"command": "  ", "description": "d"}),
        json!({"command": "x", "description": "   "}),
        json!({"command": "x", "description": "d", "timeoutMs": -1}),
    ] {
        let result = call(&default_harness, "bash", arguments).await;
        assert!(result.is_error(), "{}", text(&result));
    }

    let fractional = call(
        &default_harness,
        "bash",
        json!({"command": "true", "description": "fractional timeout", "timeoutMs": 10.5}),
    )
    .await;
    assert!(!fractional.is_error(), "{}", text(&fractional));

    let disabled = harness(
        Config {
            enable_run_in_background: Some(false),
        },
        false,
        false,
    )
    .await;
    let schema = &disabled.tools.schemas(None)[0];
    assert!(
        schema.parameters["properties"]
            .get("run_in_background")
            .is_none()
    );
    assert!(
        schema
            .description
            .contains("Background execution is not available")
    );
    assert!(!schema.description.contains("run_in_background"));
    let forced = call(
        &disabled,
        "bash",
        json!({"command": "true", "description": "forced background", "run_in_background": true}),
    )
    .await;
    assert!(forced.is_error());
    assert!(text(&forced).contains("run_in_background is disabled"));
}

#[tokio::test]
async fn background_jobs_ack_stream_complete_kill_and_preflight_without_orphans() {
    let full_harness = harness(Config::default(), true, true).await;
    let started = call(
        &full_harness,
        "bash",
        json!({"command": "echo bg-ok", "description": "test command", "run_in_background": true}),
    )
    .await;
    assert!(!started.is_error(), "{}", text(&started));
    assert_eq!(
        started.value().cloned(),
        Some(json!({"kind":"background", "jobId":"bash-1"}))
    );
    assert_eq!(text(&started), "started background job bash-1");
    let output = call_until(
        &full_harness,
        "job_output",
        json!({"job_id": "bash-1"}),
        "bg-ok",
    )
    .await;
    assert!(output.contains("bg-ok"));
    let final_output = call_until(
        &full_harness,
        "job_output",
        json!({"job_id": "bash-1"}),
        "[status: completed, exit code: 0]",
    )
    .await;
    assert!(final_output.contains("[status: completed, exit code: 0]"));

    let started = call(
        &full_harness,
        "bash",
        json!({"command": "sleep 60", "description": "test command", "run_in_background": true}),
    )
    .await;
    assert_eq!(text(&started), "started background job bash-2");
    let killed = call(&full_harness, "job_kill", json!({"job_id": "bash-2"})).await;
    assert_eq!(text(&killed), "requested cancellation of job bash-2");
    let killed_output = call_until(
        &full_harness,
        "job_output",
        json!({"job_id": "bash-2", "wait": true}),
        "[status: killed, signal: SIGTERM]",
    )
    .await;
    assert!(killed_output.contains("[status: killed, signal: SIGTERM]"));

    let without_jobs = harness(Config::default(), false, false).await;
    let unavailable = call(
        &without_jobs,
        "bash",
        json!({"command": "sleep 60", "description": "test command", "run_in_background": true}),
    )
    .await;
    assert!(unavailable.is_error());
    assert!(text(&unavailable).contains(
        "background jobs unavailable: load @seekdeep-ai/seekdeep-jobs and @seekdeep-ai/seekdeep-tool-jobs"
    ));

    let no_controller = harness(Config::default(), true, false).await;
    let preflight = call(
        &no_controller,
        "bash",
        json!({"command": "sleep 60", "description": "test command", "run_in_background": true}),
    )
    .await;
    assert!(preflight.is_error());
    assert!(text(&preflight).contains("no job controller serves this agent"));
}

#[tokio::test]
async fn background_preabort_never_spawns_and_owned_jobs_are_session_isolated() {
    let full_harness = harness(Config::default(), true, true).await;
    let signal = AbortSignal::default();
    signal.abort();
    let pre_aborted = call_with(
        &full_harness,
        "bash",
        json!({"command":"sleep 60", "description":"test command", "run_in_background":true}),
        signal,
        None,
    )
    .await;
    let ToolExecutionResult::Failure(failure) = pre_aborted else {
        panic!("pre-aborted call must fail");
    };
    assert_eq!(
        failure.error.info.as_ref().map(|info| info.code.as_str()),
        Some(seekdeep_tools::TOOL_ABORTED_BEFORE_DISPATCH)
    );
    assert_eq!(full_harness.subprocess.live_process_count(), 0);

    let owner_agent = agent(&full_harness.context, "job-owner", None);
    let owned_start = call_with(
        &full_harness,
        "bash",
        json!({"command":"sleep 60", "description":"owned command", "run_in_background":true}),
        AbortSignal::default(),
        Some(owner_agent.clone()),
    )
    .await;
    assert_eq!(text(&owned_start), "started background job bash-1");
    let anonymous = call(&full_harness, "job_output", json!({"job_id":"bash-1"})).await;
    assert!(anonymous.is_error());
    assert!(text(&anonymous).contains("belongs to another session"));
    let owner_kill = call_with(
        &full_harness,
        "job_kill",
        json!({"job_id":"bash-1"}),
        AbortSignal::default(),
        Some(owner_agent.clone()),
    )
    .await;
    assert!(!owner_kill.is_error(), "{}", text(&owner_kill));
    let owner_output = call_with(
        &full_harness,
        "job_output",
        json!({"job_id":"bash-1", "wait":true}),
        AbortSignal::default(),
        Some(owner_agent),
    )
    .await;
    assert!(!owner_output.is_error(), "{}", text(&owner_output));
}

#[tokio::test]
async fn session_workdir_and_trusted_environment_are_per_agent_and_unknown_fields_do_not_forward() {
    let harness = harness(Config::default(), false, false).await;
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::create_dir(workspace.path().join("sub")).expect("subdir");
    let owner = agent(
        &harness.context,
        "session-one",
        Some(workspace.path().to_str().unwrap()),
    );
    let result = call_with(
        &harness,
        "bash",
        json!({
            "command": "printf '%s\\n%s\\n%s\\n' \"$PWD\" \"$SEEKDEEP_SESSION_ID\" \"$SEEKDEEP_SHELL\"",
            "description": "inspect trusted environment",
            "workdir": "sub",
            "env": {"SEEKDEEP_SESSION_ID": "spoofed"},
            "stdin": "ignored",
            "stdoutMaxBytes": 1
        }),
        AbortSignal::default(),
        Some(owner),
    )
    .await;
    assert!(!result.is_error(), "{}", text(&result));
    let lines = text(&result).lines().map(str::to_owned).collect::<Vec<_>>();
    assert_eq!(
        Path::new(&lines[0]).canonicalize().unwrap(),
        workspace.path().join("sub").canonicalize().unwrap()
    );
    assert_eq!(lines[1], "session-one");
    assert_eq!(lines[2], "1");

    let no_cwd = agent(&harness.context, "session-two", None);
    let result = call_with(
        &harness,
        "bash",
        json!({"command": "printf '%s' \"$SEEKDEEP_SESSION_ID\"", "description": "print session id"}),
        AbortSignal::default(),
        Some(no_cwd),
    )
    .await;
    assert_eq!(text(&result), "session-two");

    let spec = harness
        .bash
        .resolve(seekdeep_shell::ShellExecRequest::new("true"))
        .expect("provider remains usable");
    assert!(spec.env.is_none());
    assert!(spec.stdin.is_none());
    assert!(spec.stdout_max_bytes > 1.0);
}
