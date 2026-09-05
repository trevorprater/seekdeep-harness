//! Model-facing `PowerShell` tool over foreground shell and detached Jobs seams.

use std::{path::PathBuf, sync::Arc};

use path_clean::PathClean as _;
use seekdeep_agent::Agent;
use seekdeep_cordis::{Context, Plugin};
use seekdeep_jobs::{JOBS, JobHooks, JobStart};
use seekdeep_llm::{AbortSignal, CallId, ContentBlock, HarnessError};
use seekdeep_sandbox::{
    ESCALATION_TARGETS, EscalationApproval, EscalationApprover, EscalationRequest,
    SandboxExecutionPolicy, SandboxMode, approve_escalation, canonical_path,
    validate_escalation_args,
};
use seekdeep_sandbox_policy::{SANDBOX_POLICY, SandboxPolicyRequest, SandboxPolicyService};
use seekdeep_shell::{
    SEEKDEEP_ENV_PREFIX, SHELL, SeekDeepEnvironment, ShellExecRequest, ShellProcessHandle,
};
use seekdeep_shell_env::SHELL_ENV;
use seekdeep_system_prompt::{PromptSection, PromptText, SYSTEM_PROMPT};
use seekdeep_tools::{
    DefineToolOptions, DefineToolOutput, GenericCallView, GenericResultView, TOOL_ABORTED, TOOLS,
    TerminalCallView, TerminalResultView, ToolCallKind, ToolCallView, ToolResult, ToolResultView,
    ToolRunContext, define_tool,
};
use seekdeep_user_approval::APPROVAL;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use seekdeep_jobs::{JobOutcome, JobTerminalStatus};
use seekdeep_sandbox::{escalation_hint_marker, sandbox_denial_marker};
use seekdeep_shell::{
    CollectedOutput, ShellProcess, ShellProcessRead, ShellProcessStatus, ShellRunResult,
    ShellSandboxInfo,
};

pub use seekdeep_shell::{ParsedExitStatus, parse_exit_status};

fn stream_text(output: &CollectedOutput) -> String {
    if !output.truncated {
        return output.text.clone();
    }
    format!(
        "{}\n[output truncated; full output: {}]",
        output.text,
        output.spill_path.as_ref().map_or_else(
            || "(unavailable)".to_owned(),
            |path| path.to_string_lossy().into_owned()
        )
    )
}

/// Renders one finished foreground run into model-facing text.
#[must_use]
pub fn render_pwsh_result(result: &ShellRunResult, escalation_modes: &[SandboxMode]) -> String {
    let out = stream_text(&result.stdout);
    let err = stream_text(&result.stderr);
    let mut body = out;
    if !err.is_empty() {
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str("[stderr]\n");
        body.push_str(&err);
    }
    if body.is_empty() {
        body.push_str("(no output)");
    }

    let mut markers = Vec::new();
    if let Some(sandbox) = result.sandbox.as_ref().filter(|sandbox| sandbox.denied) {
        markers.push(sandbox_denial_marker(sandbox.mode));
        if !escalation_modes.is_empty() {
            markers.push(escalation_hint_marker("command"));
        }
    }
    if result.timed_out {
        markers.push(format!("[timed out after {}ms]", result.timeout_ms));
    }
    if let Some(signal) = &result.signal {
        markers.push(format!("[killed by signal: {}]", signal.as_str()));
    } else if result.exit_code != Some(0) {
        markers.push(format!(
            "[exit code: {}]",
            result
                .exit_code
                .map_or_else(|| "null".to_owned(), |code| code.to_string())
        ));
    }
    if markers.is_empty() {
        return body;
    }
    if !body.ends_with('\n') {
        body.push('\n');
    }
    body.push_str(&markers.join("\n"));
    body
}

/// Renders one incremental background-process read and its settled notices.
#[must_use]
pub fn render_pwsh_process_read(
    read: &ShellProcessRead,
    sandbox: Option<&ShellSandboxInfo>,
    escalation_modes: &[SandboxMode],
) -> String {
    let mut notices = Vec::new();
    if read.lossy {
        let paths = [
            read.stdout_spill_path.as_ref(),
            read.stderr_spill_path.as_ref(),
        ]
        .into_iter()
        .flatten()
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
        notices.push(format!(
            "[some output was dropped from memory; full output: {}]",
            if paths.is_empty() {
                "(unavailable)".to_owned()
            } else {
                paths.join(", ")
            }
        ));
    }
    if let Some(sandbox) = sandbox.filter(|sandbox| sandbox.runner_failed == Some(true)) {
        notices.push(format!(
            "[sandbox: the sandbox runner itself failed under {} mode — the command did not run; this is a sandbox problem, not a command failure]",
            sandbox.mode
        ));
    } else if let Some(sandbox) = sandbox.filter(|sandbox| sandbox.denied) {
        notices.push(sandbox_denial_marker(sandbox.mode));
        if !escalation_modes.is_empty() {
            notices.push(escalation_hint_marker("command"));
        }
    }
    if notices.is_empty() {
        return read.delta.clone();
    }
    let separator = if !read.delta.is_empty() && !read.delta.ends_with('\n') {
        "\n"
    } else {
        ""
    };
    format!("{}{separator}{}", read.delta, notices.join("\n"))
}

/// Maps a settled background process onto the generic job outcome vocabulary.
///
/// A nonzero command exit is completed, not failed, matching foreground shell
/// rendering. Infrastructure failures remain represented by the Shell handle's
/// current signal-less killed state until that provider contract is widened.
#[must_use]
pub fn process_outcome(process: &dyn ShellProcess) -> JobOutcome {
    if process.status() == ShellProcessStatus::Killed {
        return JobOutcome {
            status: JobTerminalStatus::Killed,
            detail: Some(process.signal().map_or_else(
                || "killed before exit".to_owned(),
                |signal| format!("signal: {}", signal.as_str()),
            )),
            output: None,
        };
    }
    JobOutcome {
        status: JobTerminalStatus::Completed,
        detail: Some(format!("exit code: {}", process.exit_code().unwrap_or(0))),
        output: None,
    }
}

/// Cordis plugin name.
pub const NAME: &str = "tool-pwsh";
/// Required services.
pub const INJECT: &[&str] = &["tools", "shell", "systemPrompt", "shellEnv"];

/// `PowerShell` tool configuration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    /// Whether detached job starts are exposed and accepted.
    pub enable_run_in_background: Option<bool>,
}

/// Schema-decoded `PowerShell` tool arguments.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PwshArgs {
    /// Shell program body.
    pub command: String,
    /// Active-voice UI description.
    pub description: String,
    /// Foreground timeout override.
    #[serde(default)]
    pub timeout_ms: Option<f64>,
    /// Working directory override.
    #[serde(default)]
    pub workdir: Option<String>,
    /// Detach into Jobs.
    #[serde(default, rename = "run_in_background")]
    pub run_in_background: Option<bool>,
    /// One-call wider sandbox mode.
    #[serde(default, rename = "sandbox_permissions")]
    pub sandbox_permissions: Option<String>,
    /// Human-facing escalation reason.
    #[serde(default)]
    pub justification: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OutputSnapshot {
    text: String,
    truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    spill_path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SandboxSnapshot {
    mode: SandboxMode,
    denied: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    enforcement: Option<seekdeep_sandbox::SandboxEnforcement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    runner_failed: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum PwshValue {
    Background {
        #[serde(rename = "jobId")]
        job_id: seekdeep_jobs::JobId,
    },
    Foreground {
        #[serde(rename = "exitCode")]
        exit_code: Option<i32>,
        signal: Option<String>,
        #[serde(rename = "timedOut")]
        timed_out: bool,
        aborted: bool,
        #[serde(rename = "timeoutMs")]
        timeout_ms: f64,
        stdout: OutputSnapshot,
        stderr: OutputSnapshot,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sandbox: Option<SandboxSnapshot>,
    },
}

struct ShellJobHooks {
    process: ShellProcessHandle,
    escalation_modes: Vec<SandboxMode>,
}

impl JobHooks for ShellJobHooks {
    fn cancel(&self, _reason: Option<&str>) {
        self.process.kill();
    }

    fn done(&self) -> futures::future::BoxFuture<'static, anyhow::Result<JobOutcome>> {
        let process = self.process.clone();
        Box::pin(async move {
            process.done().await;
            Ok(process_outcome(process.as_ref()))
        })
    }

    fn read_output(&self) -> Option<String> {
        Some(render_pwsh_process_read(
            &self.process.read_output(),
            self.process.sandbox().as_ref(),
            &self.escalation_modes,
        ))
    }
}

/// Registers the `PowerShell` tool and prompt guidance.
///
/// # Errors
///
/// Returns missing dependencies, split sandbox composition, or registration failures.
pub fn apply(context: &Context, config: Config) -> anyhow::Result<()> {
    let background_enabled = config.enable_run_in_background.unwrap_or(true);
    let shell = context
        .get(SHELL)
        .ok_or_else(|| anyhow::anyhow!("tool-pwsh requires shell"))?;
    let tools = context
        .get(TOOLS)
        .ok_or_else(|| anyhow::anyhow!("tool-pwsh requires tools"))?;
    let prompt = context
        .get(SYSTEM_PROMPT)
        .ok_or_else(|| anyhow::anyhow!("tool-pwsh requires systemPrompt"))?;
    let shell_env = context
        .get(SHELL_ENV)
        .ok_or_else(|| anyhow::anyhow!("tool-pwsh requires shellEnv"))?;
    let (escalation_modes, sandbox_policy) = sandbox_composition(context, &shell)?;

    prompt.section(
        context,
        PromptSection::new(
            "tool:pwsh",
            105.0,
            PromptText::Static(
                "Non-zero exits are reported as `[exit code: N]` markers; investigate failures before moving on. On Windows a killed process settles as `[exit code: 1]` without a signal marker; treat a bare exit 1 after an interruption as a termination, not a command failure."
                    .to_owned(),
            ),
        ),
    )?;

    let output = DefineToolOutput::new(
        output_schema(),
        Arc::new({
            let escalation_modes = escalation_modes.clone();
            move |_args: &PwshArgs, value: &PwshValue| {
                let text = match value {
                    PwshValue::Background { job_id } => {
                        format!("started background job {job_id}")
                    }
                    PwshValue::Foreground {
                        exit_code,
                        signal,
                        timed_out,
                        aborted,
                        timeout_ms,
                        stdout,
                        stderr,
                        sandbox,
                    } => render_pwsh_result(
                        &ShellRunResult {
                            exit_code: *exit_code,
                            signal: signal.as_deref().map(seekdeep_shell::ProcessSignal::new),
                            timed_out: *timed_out,
                            aborted: *aborted,
                            timeout_ms: *timeout_ms,
                            stdout: snapshot_output(stdout),
                            stderr: snapshot_output(stderr),
                            sandbox: sandbox.as_ref().map(snapshot_sandbox),
                        },
                        &escalation_modes,
                    ),
                };
                Ok(vec![ContentBlock::Text { text }])
            }
        }),
    );

    let execute_context = context.clone();
    let execute_shell = shell;
    let execute_env = shell_env;
    let execute_policy = sandbox_policy;
    let execute_escalation_modes = escalation_modes.clone();
    let definition = define_tool(
        DefineToolOptions::new(
            "pwsh",
            pwsh_description(background_enabled, &escalation_modes),
            parameter_schema(background_enabled, &escalation_modes),
            output,
            Arc::new(move |args: PwshArgs, execution| {
                let context = execute_context.clone();
                let shell = execute_shell.clone();
                let shell_env = execute_env.clone();
                let sandbox_policy = execute_policy.clone();
                let escalation_modes = execute_escalation_modes.clone();
                Box::pin(async move {
                    execute_pwsh(
                        &context,
                        &shell,
                        &shell_env,
                        sandbox_policy.as_ref(),
                        &escalation_modes,
                        background_enabled,
                        args,
                        execution,
                    )
                    .await
                })
            }),
        )
        .present_call(Arc::new(|args: &PwshArgs| Some(present_pwsh_call(args))))
        .present_result(Arc::new(|args: &PwshArgs, result: &ToolResult| {
            present_pwsh_result(args, result)
        })),
    )?;
    tools.register(context, definition)?;
    Ok(())
}

fn sandbox_composition(
    context: &Context,
    shell: &Arc<seekdeep_shell::ShellService>,
) -> anyhow::Result<(Vec<SandboxMode>, Option<Arc<SandboxPolicyService>>)> {
    if shell.sandbox_mode().is_none() {
        return Ok((Vec::new(), None));
    }
    let policy = context.get(SANDBOX_POLICY).ok_or_else(|| {
        anyhow::anyhow!(
            "tool-pwsh: the mounted bash executor confines but ctx.sandboxPolicy is missing"
        )
    })?;
    Ok((ESCALATION_TARGETS.to_vec(), Some(policy)))
}

/// Builds the Loader-compatible plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, config| {
        Box::pin(async move {
            let config = if config.is_null() {
                Config::default()
            } else {
                serde_json::from_value(config)?
            };
            apply(&context, config)
        })
    })
    .with_config_validator(|value| {
        let config = if value.is_null() {
            Config::default()
        } else {
            serde_json::from_value(value.clone())?
        };
        Ok(json!({
            "enableRunInBackground": config.enable_run_in_background.unwrap_or(true)
        }))
    })
}

#[allow(clippy::too_many_arguments)]
async fn execute_pwsh(
    context: &Context,
    shell: &Arc<seekdeep_shell::ShellService>,
    shell_env: &Arc<seekdeep_shell_env::ShellEnvRegistry>,
    sandbox_policy: Option<&Arc<SandboxPolicyService>>,
    escalation_modes: &[SandboxMode],
    background_enabled: bool,
    args: PwshArgs,
    execution: ToolRunContext,
) -> anyhow::Result<PwshValue> {
    validate_args(&args)?;
    let standing_policy = sandbox_policy
        .map(|policy| {
            policy.resolve(SandboxPolicyRequest {
                session: execution
                    .agent
                    .as_ref()
                    .map(|agent| agent.session().as_ref()),
                mode: None,
            })
        })
        .transpose()?;
    let approved_mode = match (&args.sandbox_permissions, &args.justification) {
        (Some(mode), Some(justification)) => Some(
            approve_pwsh_escalation(
                context,
                mode,
                justification,
                &execution,
                standing_policy.as_ref(),
                escalation_modes,
            )
            .await?,
        ),
        _ => None,
    };
    let policy = approved_mode.map_or_else(
        || standing_policy.clone(),
        |mode| {
            standing_policy
                .clone()
                .map(|policy| SandboxExecutionPolicy { mode, ..policy })
        },
    );
    let workdir = resolve_workdir(
        args.workdir.as_deref(),
        execution.agent.as_ref(),
        standing_policy
            .as_ref()
            .map(|policy| policy.workspace_root.as_path()),
    );
    let timeout_ms = args.timeout_ms;
    let seekdeep_env = shell_env.collect(execution.execution())?;
    let request = shell_request(
        &args.command,
        workdir,
        timeout_ms,
        seekdeep_env,
        policy,
        None,
    );

    if args.run_in_background == Some(true) {
        anyhow::ensure!(
            background_enabled,
            "run_in_background is disabled for this deployment (enableRunInBackground: false)"
        );
        let jobs = context.get(JOBS).ok_or_else(|| {
            anyhow::anyhow!(
                "background jobs unavailable: load @seekdeep-ai/seekdeep-jobs and @seekdeep-ai/seekdeep-tool-jobs"
            )
        })?;
        if execution.signal().is_aborted() {
            return Err(aborted_error());
        }
        let command = args.command.clone();
        let owner = execution.agent.clone();
        let shell = shell.clone();
        let modes = escalation_modes.to_vec();
        let job_id = jobs.start(JobStart {
            kind: "pwsh".to_owned(),
            label: command,
            output_limit_bytes: None,
            owner,
            run: Box::new(move || {
                let spec = shell
                    .resolve(request)
                    .unwrap_or_else(|error| panic!("{error:#}"));
                let process = shell
                    .start(spec)
                    .unwrap_or_else(|error| panic!("{error:#}"));
                Box::new(ShellJobHooks {
                    process,
                    escalation_modes: modes,
                })
            }),
        });
        return Ok(PwshValue::Background { job_id });
    }

    let mut request = request;
    request.signal = Some(execution.signal());
    let result = shell.run(shell.resolve(request)?).await?;
    if result.aborted {
        return Err(aborted_error());
    }
    Ok(canonical_pwsh_result(&result))
}

async fn approve_pwsh_escalation(
    context: &Context,
    mode: &str,
    justification: &str,
    execution: &ToolRunContext,
    standing_policy: Option<&SandboxExecutionPolicy>,
    escalation_modes: &[SandboxMode],
) -> anyhow::Result<SandboxMode> {
    anyhow::ensure!(
        !escalation_modes.is_empty(),
        "sandbox_permissions is not available in this composition (no sandboxing executor to escalate)"
    );
    let standing_policy =
        standing_policy.expect("confining executor always resolves a standing sandbox policy");
    let approver = context
        .get(APPROVAL)
        .map(|service| service as Arc<dyn EscalationApprover<Arc<Agent>, CallId>>);
    approve_escalation(
        EscalationRequest {
            requested_mode: mode.to_owned(),
            justification: justification.to_owned(),
            effective_mode: standing_policy.mode,
            subject: "command".to_owned(),
        },
        EscalationApproval {
            approver: approver.as_deref(),
            agent: execution.agent.clone(),
            call_id: execution.call_id.clone(),
            tool_name: "pwsh".to_owned(),
            signal: Some(execution.signal()),
        },
    )
    .await
}

fn validate_args(args: &PwshArgs) -> anyhow::Result<()> {
    anyhow::ensure!(
        !args.command.trim().is_empty(),
        "invalid command: expected a non-empty string"
    );
    anyhow::ensure!(
        !args.description.trim().is_empty(),
        "invalid description: expected a non-empty string"
    );
    if let Some(timeout) = args.timeout_ms {
        anyhow::ensure!(
            timeout.is_finite() && timeout > 0.0,
            "invalid timeoutMs: expected a positive number, got {}",
            js_number(timeout)
        );
    }
    validate_escalation_args(
        args.sandbox_permissions.as_deref(),
        args.justification.as_deref(),
    )
}

fn resolve_workdir(
    model_workdir: Option<&str>,
    agent: Option<&Arc<Agent>>,
    policy_workspace_root: Option<&std::path::Path>,
) -> Option<PathBuf> {
    let session_cwd = policy_workspace_root.map(PathBuf::from).or_else(|| {
        agent
            .and_then(|agent| agent.session().header().cwd.as_deref())
            .map(canonical_path)
    });
    match model_workdir {
        None => session_cwd,
        Some(workdir) if PathBuf::from(workdir).is_relative() && session_cwd.is_some() => {
            Some(session_cwd.expect("checked").join(workdir).clean())
        }
        Some(workdir) => Some(PathBuf::from(workdir)),
    }
}

fn shell_request(
    command: &str,
    workdir: Option<PathBuf>,
    timeout_ms: Option<f64>,
    seekdeep_env: SeekDeepEnvironment,
    sandbox_policy: Option<SandboxExecutionPolicy>,
    signal: Option<AbortSignal>,
) -> ShellExecRequest {
    let mut request = ShellExecRequest::new(command);
    request.workdir = workdir;
    request.timeout_ms = timeout_ms;
    request.seekdeep_env = Some(seekdeep_env);
    request.sandbox_policy = sandbox_policy;
    request.signal = signal;
    request
}

fn aborted_error() -> anyhow::Error {
    anyhow::Error::new(HarnessError::named(
        "AbortError",
        "tool call aborted",
        TOOL_ABORTED,
    ))
}

fn snapshot_output(output: &OutputSnapshot) -> CollectedOutput {
    CollectedOutput {
        text: output.text.clone(),
        truncated: output.truncated,
        spill_path: output.spill_path.as_ref().map(PathBuf::from),
    }
}

fn snapshot_sandbox(sandbox: &SandboxSnapshot) -> ShellSandboxInfo {
    ShellSandboxInfo {
        mode: sandbox.mode,
        denied: sandbox.denied,
        enforcement: sandbox.enforcement,
        runner_failed: sandbox.runner_failed,
    }
}

fn canonical_pwsh_result(result: &ShellRunResult) -> PwshValue {
    let output = |stream: &CollectedOutput| OutputSnapshot {
        text: stream.text.clone(),
        truncated: stream.truncated,
        spill_path: stream
            .spill_path
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned()),
    };
    PwshValue::Foreground {
        exit_code: result.exit_code,
        signal: result
            .signal
            .as_ref()
            .map(|signal| signal.as_str().to_owned()),
        timed_out: result.timed_out,
        aborted: result.aborted,
        timeout_ms: result.timeout_ms,
        stdout: output(&result.stdout),
        stderr: output(&result.stderr),
        sandbox: result.sandbox.as_ref().map(|sandbox| SandboxSnapshot {
            mode: sandbox.mode,
            denied: sandbox.denied,
            enforcement: sandbox.enforcement,
            runner_failed: sandbox.runner_failed,
        }),
    }
}

fn js_number(value: f64) -> String {
    if !value.is_finite() {
        return "null".to_owned();
    }
    if value == 0.0 {
        return "0".to_owned();
    }
    ryu_js::Buffer::new().format(value).to_owned()
}

fn pwsh_description(background_enabled: bool, escalation_modes: &[SandboxMode]) -> String {
    let background = if background_enabled {
        "Set `run_in_background: true` for long-running commands: the call returns a job id immediately; read its output with `job_output` and stop it with `job_kill`."
    } else {
        "Background execution is not available; long-running commands must finish within the timeout."
    };
    let mut description = format!(
        "Execute a PowerShell command (`pwsh -Command`) and return its stdout/stderr. Each call runs in a fresh pwsh process: no state (cwd, variables, functions) persists between calls — pass `workdir` instead of using `cd`. Paths use native Windows form (`C:\\...`); read environment variables with `$env:NAME`. Non-zero exits are reported as `[exit code: N]`. Current harness environment facts are exposed through managed `$env:{SEEKDEEP_ENV_PREFIX}*` variables; inspect them when needed. Commands may run under a file sandbox; a blocked file operation is reported as `[sandbox: file access denied under <mode> mode]` — a policy denial, not a bug in the command; do not retry another way. Long output is truncated to its tail; the full output is saved to a file whose path is reported when available. On Windows a force-killed command settles as `[exit code: 1]` without a signal marker — treat it as an interruption, not a command failure. {background}"
    );
    if !escalation_modes.is_empty() {
        description.push_str(
            " Under the Windows sandbox, read-only pwsh runs in PowerShell ConstrainedLanguage mode, while workspace-write stays in FullLanguage unless host policy says otherwise. In read-only, prefer cmdlets and core types (`[string]`, `[datetime]`, `[regex]`, `[guid]`); .NET static calls (`[System.IO.*]::`, `[math]::`), `Add-Type`, COM objects, and reflection fail with \"only core types\" errors. `-f` formatting, property access, and core cmdlets work. In both confined modes, programs cannot open named pipes, so a command that captures another program's output through piped stdio (Node.js `child_process.spawn`/`exec` with the default `stdio: 'pipe'`) fails with EPERM, while `stdio: 'inherit'` and `stdio: 'ignore'` spawns work and PowerShell's own pipelines are unaffected. That EPERM is the documented boundary: do not retry the command another way — escalate the exact command once or restructure it to avoid capturing output. Attempting a command the sandbox may deny is safe and expected: run it and read the marker rather than assuming the denial. When a command is denied and a wider mode would let it succeed, escalate immediately in the same turn — the one sanctioned exception to a denial: retry the exact same command once with `sandbox_permissions` (the narrowest wider mode that suffices) plus a one-sentence `justification`. Do not detour through chat to ask permission first — the approval prompt raised by that retry is how the user consents. If the session states approval prompts are disabled, there is no exception: a denial is final — do not set `sandbox_permissions`. Never escalate speculatively: ground the request in a real denial — normally the one this command just hit; escalating up front is fine only when this session already denied the same access. A rejected escalation is final for that command — stop and explain, never work around it — but it does not forbid attempting or escalating other commands later.",
        );
    }
    description
}

fn parameter_schema(background_enabled: bool, escalation_modes: &[SandboxMode]) -> Value {
    let mut properties = Map::from_iter([
        (
            "command".to_owned(),
            json!({
                "type": "string",
                "required": true,
                "description": "The PowerShell command to execute."
            }),
        ),
        (
            "description".to_owned(),
            json!({
                "type": "string",
                "required": true,
                "description": "Clear, concise description of what this command does in active voice, 5-10 words (shown in the UI). Examples: \"ls\" → \"List files in current directory\"; \"git status\" → \"Show working tree status\"; \"Get-Process\" → \"List running processes\"."
            }),
        ),
        (
            "timeoutMs".to_owned(),
            json!({
                "type": "number",
                "description": "Timeout in milliseconds. The executor applies its configured default and cap, and kills the command on expiry."
            }),
        ),
        (
            "workdir".to_owned(),
            json!({
                "type": "string",
                "description": "Working directory for this command. Defaults to the session workspace; a relative path is resolved against it."
            }),
        ),
    ]);
    if background_enabled {
        properties.insert(
            "run_in_background".to_owned(),
            json!({
                "type": "boolean",
                "description": "Run in the background and return a job id immediately (collect with job_output, stop with job_kill). No timeout applies."
            }),
        );
    }
    if !escalation_modes.is_empty() {
        properties.insert(
            "sandbox_permissions".to_owned(),
            json!({
                "type": "string",
                "enum": escalation_modes,
                "description": "The wider sandbox mode this command needs. Only valid as a one-shot retry of a command the sandbox just denied; requires justification and user approval."
            }),
        );
        properties.insert(
            "justification".to_owned(),
            json!({
                "type": "string",
                "description": "Required with sandbox_permissions: one sentence for the user explaining why this exact command needs the wider access."
            }),
        );
    }
    Value::Object(properties)
}

fn output_schema() -> Value {
    let output = || {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": true,
            "properties": {
                "text": {"type": "string", "required": true},
                "truncated": {"type": "boolean", "required": true},
                "spillPath": {"type": "string"}
            }
        })
    };
    json!({
        "oneOf": [
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "kind": {"type": "string", "required": true, "const": "background"},
                    "jobId": {"type": "string", "required": true}
                }
            },
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "kind": {"type": "string", "required": true, "const": "foreground"},
                    "exitCode": {"required": true, "oneOf": [{"type": "integer"}, {"type": "null"}]},
                    "signal": {"required": true, "oneOf": [{"type": "string"}, {"type": "null"}]},
                    "timedOut": {"type": "boolean", "required": true},
                    "aborted": {"type": "boolean", "required": true},
                    "timeoutMs": {"type": "number", "required": true},
                    "stdout": output(),
                    "stderr": output(),
                    "sandbox": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "mode": {"type": "string", "required": true},
                            "denied": {"type": "boolean", "required": true},
                            "enforcement": {"type": "string"},
                            "runnerFailed": {"type": "boolean"}
                        }
                    }
                }
            }
        ]
    })
}

fn present_pwsh_call(args: &PwshArgs) -> ToolCallView {
    if args.run_in_background == Some(true) {
        return ToolCallView::Generic(GenericCallView {
            title: args.command.clone(),
            kind: Some(ToolCallKind::Execute),
            raw_input: Some(json!(args.command)),
            content: Some(vec![ContentBlock::Text {
                text: args.description.clone(),
            }]),
            locations: None,
        });
    }
    ToolCallView::Terminal(TerminalCallView {
        title: args.command.clone(),
        description: Some(args.description.clone()),
        cwd: args.workdir.clone(),
    })
}

fn present_pwsh_result(args: &PwshArgs, result: &ToolResult) -> Option<ToolResultView> {
    let [ContentBlock::Text { text: raw }] = result.content.as_slice() else {
        return None;
    };
    if args.run_in_background == Some(true) || result.is_error {
        return Some(ToolResultView::Generic(GenericResultView {
            title: None,
            content: Some(vec![ContentBlock::Text {
                text: format!("```console\n{}\n```", raw.trim_end_matches('\n')),
            }]),
        }));
    }
    let view = match parse_exit_status(raw) {
        ParsedExitStatus::Exit { body, exit_code } => TerminalResultView {
            title: None,
            output: Some(body),
            exit_code: js_number(exit_code).parse().ok(),
            signal: None,
        },
        ParsedExitStatus::Signal { body, signal } => TerminalResultView {
            title: None,
            output: Some(body),
            exit_code: None,
            signal: Some(signal),
        },
    };
    Some(ToolResultView::Terminal(view))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use seekdeep_shell::{
        CollectedOutput, ProcessSignal, ShellProcessRead, ShellRunResult, ShellSandboxInfo,
    };

    use super::*;

    #[derive(Debug)]
    struct Process {
        status: ShellProcessStatus,
        exit_code: Option<i32>,
        signal: Option<ProcessSignal>,
    }

    #[async_trait::async_trait]
    impl ShellProcess for Process {
        fn status(&self) -> ShellProcessStatus {
            self.status
        }

        fn exit_code(&self) -> Option<i32> {
            self.exit_code
        }

        fn signal(&self) -> Option<ProcessSignal> {
            self.signal.clone()
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

    fn process(
        status: ShellProcessStatus,
        exit_code: Option<i32>,
        signal: Option<&str>,
    ) -> Process {
        Process {
            status,
            exit_code,
            signal: signal.map(ProcessSignal::new),
        }
    }

    #[test]
    fn background_process_outcomes_match_job_status_and_detail_vocabulary() {
        for (process, expected) in [
            (
                process(ShellProcessStatus::Killed, None, Some("SIGKILL")),
                JobOutcome {
                    status: JobTerminalStatus::Killed,
                    detail: Some("signal: SIGKILL".to_owned()),
                    output: None,
                },
            ),
            (
                process(ShellProcessStatus::Killed, None, None),
                JobOutcome {
                    status: JobTerminalStatus::Killed,
                    detail: Some("killed before exit".to_owned()),
                    output: None,
                },
            ),
            (
                process(ShellProcessStatus::Completed, Some(7), None),
                JobOutcome {
                    status: JobTerminalStatus::Completed,
                    detail: Some("exit code: 7".to_owned()),
                    output: None,
                },
            ),
            (
                process(ShellProcessStatus::Running, None, None),
                JobOutcome {
                    status: JobTerminalStatus::Completed,
                    detail: Some("exit code: 0".to_owned()),
                    output: None,
                },
            ),
        ] {
            assert_eq!(process_outcome(&process), expected);
        }
    }

    fn run() -> ShellRunResult {
        ShellRunResult {
            exit_code: Some(0),
            signal: None,
            timed_out: false,
            aborted: false,
            timeout_ms: 1_000.0,
            stdout: CollectedOutput::default(),
            stderr: CollectedOutput::default(),
            sandbox: None,
        }
    }

    fn sandbox(mode: SandboxMode, denied: bool, runner_failed: Option<bool>) -> ShellSandboxInfo {
        ShellSandboxInfo {
            mode,
            denied,
            enforcement: None,
            runner_failed,
        }
    }

    #[test]
    fn foreground_rendering_preserves_sections_markers_and_round_trip_order() {
        let mut result = run();
        result.stderr.text = "err\n".to_owned();
        assert_eq!(render_pwsh_result(&result, &[]), "[stderr]\nerr\n");
        result.stdout.text = "out".to_owned();
        result.stderr.text = "err".to_owned();
        assert_eq!(render_pwsh_result(&result, &[]), "out\n[stderr]\nerr");

        let mut exited = run();
        exited.exit_code = Some(7);
        exited.stdout.text = "x".to_owned();
        assert_eq!(render_pwsh_result(&exited, &[]), "x\n[exit code: 7]");
        assert_eq!(
            parse_exit_status(&render_pwsh_result(&exited, &[])),
            ParsedExitStatus::Exit {
                body: "x".to_owned(),
                exit_code: 7.0,
            }
        );

        let mut killed = run();
        killed.exit_code = None;
        killed.signal = Some(ProcessSignal::new("SIGTERM"));
        killed.timed_out = true;
        assert_eq!(
            render_pwsh_result(&killed, &[]),
            "(no output)\n[timed out after 1000ms]\n[killed by signal: SIGTERM]"
        );
        assert_eq!(
            parse_exit_status(&render_pwsh_result(&killed, &[])),
            ParsedExitStatus::Signal {
                body: "(no output)\n[timed out after 1000ms]".to_owned(),
                signal: "SIGTERM".to_owned(),
            }
        );

        let mut truncated = run();
        truncated.stdout = CollectedOutput {
            text: "tail".to_owned(),
            truncated: true,
            spill_path: None,
        };
        assert_eq!(
            render_pwsh_result(&truncated, &[]),
            "tail\n[output truncated; full output: (unavailable)]"
        );

        let mut denied = run();
        denied.exit_code = Some(1);
        denied.stderr.text = "denied".to_owned();
        denied.sandbox = Some(sandbox(SandboxMode::ReadOnly, true, None));
        let without_escalation = render_pwsh_result(&denied, &[]);
        assert!(
            without_escalation
                .ends_with("[sandbox: file access denied under read-only mode]\n[exit code: 1]")
        );
        assert!(
            render_pwsh_result(&denied, &[SandboxMode::WorkspaceWrite])
                .contains("[sandbox: escalation available")
        );
    }

    #[test]
    fn background_read_rendering_preserves_loss_and_sandbox_notices() {
        let base = ShellProcessRead {
            delta: "out\n".to_owned(),
            lossy: false,
            stdout_spill_path: None,
            stderr_spill_path: None,
        };
        assert_eq!(render_pwsh_process_read(&base, None, &[]), "out\n");
        let lossy = ShellProcessRead {
            lossy: true,
            stdout_spill_path: Some(PathBuf::from("/spill/out.log")),
            stderr_spill_path: Some(PathBuf::from("/spill/err.log")),
            ..base.clone()
        };
        assert_eq!(
            render_pwsh_process_read(&lossy, None, &[]),
            "out\n[some output was dropped from memory; full output: /spill/out.log, /spill/err.log]"
        );
        let unavailable = ShellProcessRead {
            delta: "tail".to_owned(),
            lossy: true,
            stdout_spill_path: None,
            stderr_spill_path: None,
        };
        assert_eq!(
            render_pwsh_process_read(&unavailable, None, &[]),
            "tail\n[some output was dropped from memory; full output: (unavailable)]"
        );
        assert_eq!(
            render_pwsh_process_read(
                &base,
                Some(&sandbox(SandboxMode::ReadOnly, true, None)),
                &[]
            ),
            "out\n[sandbox: file access denied under read-only mode]"
        );
        let runner = render_pwsh_process_read(
            &ShellProcessRead::default(),
            Some(&sandbox(SandboxMode::WorkspaceWrite, true, Some(true))),
            &[SandboxMode::DangerFullAccess],
        );
        assert!(runner.contains("sandbox runner itself failed under workspace-write mode"));
        assert!(!runner.contains("file access denied"));
    }

    fn args(background: bool) -> PwshArgs {
        PwshArgs {
            command: "printf hi".to_owned(),
            description: "Print a greeting".to_owned(),
            timeout_ms: None,
            workdir: None,
            run_in_background: background.then_some(true),
            sandbox_permissions: None,
            justification: None,
        }
    }

    fn result(text: &str, is_error: bool) -> ToolResult {
        ToolResult {
            content: vec![ContentBlock::Text {
                text: text.to_owned(),
            }],
            is_error,
            meta: None,
        }
    }

    #[test]
    fn presentation_distinguishes_terminal_runs_background_acks_and_errors() {
        let mut foreground = args(false);
        assert_eq!(
            serde_json::to_value(present_pwsh_call(&foreground)).unwrap(),
            json!({
                "card":"terminal",
                "title":"printf hi",
                "description":"Print a greeting"
            })
        );
        foreground.workdir = Some("sub".to_owned());
        assert_eq!(
            serde_json::to_value(present_pwsh_call(&foreground)).unwrap()["cwd"],
            json!("sub")
        );

        let background = args(true);
        assert_eq!(
            serde_json::to_value(present_pwsh_call(&background)).unwrap(),
            json!({
                "card":"generic",
                "title":"printf hi",
                "kind":"execute",
                "rawInput":"printf hi",
                "content":[{"type":"text","text":"Print a greeting"}]
            })
        );
        assert_eq!(
            serde_json::to_value(
                present_pwsh_result(
                    &background,
                    &result("started background job pwsh-1\n\n", false)
                )
                .unwrap()
            )
            .unwrap(),
            json!({
                "card":"generic",
                "content":[{"type":"text","text":"```console\nstarted background job pwsh-1\n```"}]
            })
        );
        assert!(matches!(
            present_pwsh_result(&foreground, &result("tool call aborted", true)),
            Some(ToolResultView::Generic(_))
        ));
    }

    #[test]
    fn terminal_presenter_consumes_only_real_final_exit_markers() {
        let foreground = args(false);
        for (raw, expected) in [
            (
                "hi\n\n",
                json!({"card":"terminal", "output":"hi\n\n", "exitCode":0}),
            ),
            (
                "oops\n[exit code: 3]",
                json!({"card":"terminal", "output":"oops", "exitCode":3}),
            ),
            (
                "gone\n[killed by signal: SIGKILL]",
                json!({"card":"terminal", "output":"gone", "signal":"SIGKILL"}),
            ),
            (
                "slow\n[timed out after 100ms]\n[exit code: 143]",
                json!({
                    "card":"terminal",
                    "output":"slow\n[timed out after 100ms]",
                    "exitCode":143
                }),
            ),
            (
                "[exit code: 5]",
                json!({"card":"terminal", "output":"[exit code: 5]", "exitCode":0}),
            ),
            (
                "[killed by signal: SIGKILL]",
                json!({
                    "card":"terminal",
                    "output":"[killed by signal: SIGKILL]",
                    "exitCode":0
                }),
            ),
        ] {
            assert_eq!(
                serde_json::to_value(
                    present_pwsh_result(&foreground, &result(raw, false)).unwrap()
                )
                .unwrap(),
                expected
            );
        }

        let reasoning = ToolResult {
            content: vec![ContentBlock::Reasoning {
                text: "unexpected".to_owned(),
            }],
            is_error: false,
            meta: None,
        };
        assert!(present_pwsh_result(&foreground, &reasoning).is_none());
        let empty = ToolResult {
            content: Vec::new(),
            is_error: false,
            meta: None,
        };
        assert!(present_pwsh_result(&foreground, &empty).is_none());
        let multiple = ToolResult {
            content: vec![
                ContentBlock::Text {
                    text: "a".to_owned(),
                },
                ContentBlock::Text {
                    text: "b".to_owned(),
                },
            ],
            is_error: false,
            meta: None,
        };
        assert!(present_pwsh_result(&foreground, &multiple).is_none());
    }

    #[test]
    fn workdir_resolution_uses_policy_identity_and_preserves_absolute_overrides() {
        assert_eq!(resolve_workdir(None, None, None), None);
        assert_eq!(
            resolve_workdir(
                Some("sub/../child"),
                None,
                Some(PathBuf::from("/workspace").as_path())
            ),
            Some(PathBuf::from("/workspace/child"))
        );
        assert_eq!(
            resolve_workdir(
                Some("/tmp"),
                None,
                Some(PathBuf::from("/workspace").as_path())
            ),
            Some(PathBuf::from("/tmp"))
        );
    }
}
