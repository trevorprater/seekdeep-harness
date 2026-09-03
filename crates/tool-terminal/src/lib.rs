//! Six model-facing tools for owner-scoped persistent terminal sessions.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use seekdeep_agent::Agent;
use seekdeep_cordis::{Context, Plugin, fiber::EffectHandle};
use seekdeep_jobs::{JOBS, JobHooks, JobOutcome, JobStart, JobTerminalStatus};
use seekdeep_llm::ContentBlock;
use seekdeep_system_prompt::{PromptSection, SYSTEM_PROMPT};
use seekdeep_terminal::{
    TERMINALS, TerminalReadRequest, TerminalReadResult, TerminalSendOperationRef,
    TerminalSendRequest, TerminalSendResult, TerminalSessionId, TerminalSessionSnapshot,
    TerminalSessionStatus, TerminalSignal, TerminalSignalResult, TerminalSpawnRequest,
    TerminalSpawnResult,
};
use seekdeep_tools::{
    DefineToolOptions, DefineToolOutput, GenericCallView, TOOLS, TerminalCallView,
    TerminalResultView, ToolCallKind, ToolCallView, ToolContentFinalizer, ToolDefinition,
    ToolExecutionResult, ToolResult, ToolResultView, ToolRunContext, define_tool,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub mod render;

use render::{
    bound_terminal_text, render_list, render_read, render_send, render_send_read, render_spawn,
};

/// Loader plugin name.
pub const NAME: &str = "tool-terminal";
/// Required capability and prompt services.
pub const INJECT: &[&str] = &["terminals", "tools", "systemPrompt"];
/// Default complete model-facing result bound.
pub const DEFAULT_MAX_RESULT_BYTES: u64 = 256 * 1024;
/// Smallest cap that retains registry-issued PTY and job ids.
pub const MIN_MAX_RESULT_BYTES: u64 = 64;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
// The source's concurrent Cordis activation registers PTY guidance before the
// equal-order jobs guidance. Rust activation is serialized, so an internal
// half-step preserves the observable prompt order without crossing either
// neighboring integer order band.
const GUIDANCE_ORDER: f64 = 105.5;
const GUIDANCE: &str = "Use a terminal session only when work needs persistent terminal state or interactive stdin; prefer shell/read/write/edit for bounded one-shot operations. Track every terminal session id and close sessions that no longer matter. An inferred_idle or timeout result does not prove the foreground command exited.";

/// Terminal tool configuration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// Whether `terminal_send` exposes background execution.
    pub enable_run_in_background: Option<bool>,
    /// Complete UTF-8 byte cap for terminal results.
    pub max_result_bytes: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize)]
struct SpawnArgs {
    #[serde(rename = "type")]
    terminal_type: String,
    name: Option<String>,
    cwd: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionArgs {
    session_id: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct SendArgs {
    session_id: String,
    text: String,
    submit: Option<bool>,
    #[serde(rename = "run_in_background")]
    run_in_background: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReadArgs {
    session_id: String,
    offset: Option<f64>,
    count: Option<f64>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct SignalArgs {
    session_id: String,
    signal: TerminalSignal,
}

#[derive(Debug, Deserialize, Serialize)]
struct NoArgs {}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "lowercase",
    rename_all_fields = "camelCase"
)]
enum SendValue {
    Background {
        job_id: seekdeep_jobs::JobId,
    },
    Foreground {
        viewport: String,
        wait_reason: seekdeep_terminal::TerminalWaitReason,
        session_status: TerminalSessionStatus,
        truncated: bool,
    },
}

impl From<TerminalSendResult> for SendValue {
    fn from(value: TerminalSendResult) -> Self {
        Self::Foreground {
            viewport: value.viewport,
            wait_reason: value.wait_reason,
            session_status: value.session_status,
            truncated: value.truncated,
        }
    }
}

impl SendValue {
    fn as_send_result(&self) -> Option<TerminalSendResult> {
        match self {
            Self::Background { .. } => None,
            Self::Foreground {
                viewport,
                wait_reason,
                session_status,
                truncated,
            } => Some(TerminalSendResult {
                viewport: viewport.clone(),
                wait_reason: *wait_reason,
                session_status: session_status.clone(),
                truncated: *truncated,
            }),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CloseValue {
    session_id: TerminalSessionId,
    outcome: CloseOutcome,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum CloseOutcome {
    Closed,
    AlreadyClosing,
}

fn require_agent(agent: Option<&Arc<Agent>>) -> anyhow::Result<Arc<Agent>> {
    agent
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("terminal tools require an initiating agent"))
}

fn session_id(value: &str) -> anyhow::Result<TerminalSessionId> {
    if value.is_empty() {
        anyhow::bail!("sessionId must be a non-empty string");
    }
    Ok(TerminalSessionId::new(value))
}

fn raw_content_text(result: &ToolExecutionResult) -> Option<&str> {
    single_text(result.content())
}

fn single_text(content: &[ContentBlock]) -> Option<&str> {
    let [ContentBlock::Text { text }] = content else {
        return None;
    };
    Some(text)
}

fn generic_call(title: impl Into<String>, kind: ToolCallKind, raw_input: Value) -> ToolCallView {
    ToolCallView::Generic(GenericCallView {
        title: title.into(),
        kind: Some(kind),
        raw_input: Some(raw_input),
        content: None,
        locations: None,
    })
}

fn finalizer(max_bytes: usize) -> ToolContentFinalizer {
    Arc::new(move |_execution, result| {
        Ok(raw_content_text(result).map(|text| {
            vec![ContentBlock::Text {
                text: bound_terminal_text(text, max_bytes),
            }]
        }))
    })
}

fn status_schema() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "additionalProperties": false,
                "properties": { "kind": { "type": "string", "required": true, "const": "running" } }
            },
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "kind": { "type": "string", "required": true, "const": "exited" },
                    "exitCode": { "required": true, "oneOf": [{ "type": "integer" }, { "type": "null" }] },
                    "signal": { "required": true, "oneOf": [{ "type": "string" }, { "type": "null" }] }
                }
            }
        ]
    })
}

fn snapshot_properties() -> serde_json::Map<String, Value> {
    serde_json::Map::from_iter([
        (
            "sessionId".to_owned(),
            json!({ "type": "string", "required": true }),
        ),
        ("name".to_owned(), json!({ "type": "string" })),
        (
            "type".to_owned(),
            json!({ "type": "string", "required": true }),
        ),
        ("pid".to_owned(), json!({ "type": "integer" })),
        ("status".to_owned(), {
            let mut status = status_schema();
            status["required"] = json!(true);
            status
        }),
    ])
}

fn snapshot_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": snapshot_properties()
    })
}

fn send_output_schema() -> Value {
    let mut foreground = serde_json::Map::new();
    foreground.insert(
        "kind".to_owned(),
        json!({ "type": "string", "required": true, "const": "foreground" }),
    );
    foreground.insert(
        "viewport".to_owned(),
        json!({ "type": "string", "required": true }),
    );
    foreground.insert(
        "waitReason".to_owned(),
        json!({
            "type": "string", "required": true,
            "enum": ["stdin_read", "inferred_idle", "timeout", "session_exit"]
        }),
    );
    let mut status = status_schema();
    status["required"] = json!(true);
    foreground.insert("sessionStatus".to_owned(), status);
    foreground.insert(
        "truncated".to_owned(),
        json!({ "type": "boolean", "required": true }),
    );
    json!({
        "oneOf": [
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "kind": { "type": "string", "required": true, "const": "background" },
                    "jobId": { "type": "string", "required": true }
                }
            },
            { "type": "object", "additionalProperties": false, "properties": foreground }
        ]
    })
}

fn send_detail(result: &TerminalSendResult) -> String {
    match &result.session_status {
        TerminalSessionStatus::Running => format!(
            "wait: {}",
            serde_json::to_value(result.wait_reason)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default()
        ),
        TerminalSessionStatus::Exited { exit_code, signal } => format!(
            "session exited: {}",
            exit_code.map_or_else(
                || signal
                    .as_ref()
                    .map_or("unknown", |signal| signal.as_str())
                    .to_owned(),
                |code| code.to_string()
            )
        ),
    }
}

struct TerminalJobHooks {
    operation: TerminalSendOperationRef,
    cancel_requested: Arc<AtomicBool>,
}

async fn execute_send(
    context: Context,
    terminals: Arc<seekdeep_terminal::TerminalSessionService>,
    background_enabled: bool,
    max_bytes: usize,
    args: SendArgs,
    run: ToolRunContext,
) -> anyhow::Result<SendValue> {
    let owner = require_agent(run.execution().agent.as_ref())?;
    let id = session_id(&args.session_id)?;
    if args.run_in_background == Some(true) {
        if !background_enabled {
            anyhow::bail!("background terminal sends are disabled by tool-terminal configuration");
        }
        let jobs = context.get(JOBS).ok_or_else(|| {
            anyhow::anyhow!("background terminal sends require @seekdeep-ai/seekdeep-jobs and @seekdeep-ai/seekdeep-tool-jobs")
        })?;
        let label = format!(
            "{id}: {}",
            if args.text.is_empty() {
                "(input)"
            } else {
                args.text.as_str()
            }
        );
        let request = TerminalSendRequest {
            text: args.text,
            submit: args.submit.unwrap_or(true),
            signal: None,
        };
        let start_terminals = terminals.clone();
        let start_owner = owner.clone();
        let start_id = id.clone();
        let job_id = jobs.start(JobStart {
            kind: "pty-send".to_owned(),
            label,
            output_limit_bytes: Some(max_bytes as u64),
            owner: Some(owner),
            run: Box::new(move || {
                let operation = start_terminals
                    .start_send(&start_owner, &start_id, request)
                    .unwrap_or_else(|error| panic!("{error}"));
                Box::new(TerminalJobHooks {
                    operation,
                    cancel_requested: Arc::new(AtomicBool::new(false)),
                })
            }),
        });
        return Ok(SendValue::Background { job_id });
    }
    let operation = terminals
        .start_send(
            &owner,
            &id,
            TerminalSendRequest {
                text: args.text,
                submit: args.submit.unwrap_or(true),
                signal: Some(run.signal()),
            },
        )
        .map_err(anyhow::Error::new)?;
    let result = operation.done().await.map_err(anyhow::Error::new)?;
    if run.signal().is_aborted() {
        anyhow::bail!("terminal send aborted");
    }
    Ok(result.into())
}

impl JobHooks for TerminalJobHooks {
    fn cancel(&self, _reason: Option<&str>) {
        self.cancel_requested.store(true, Ordering::Release);
        self.operation.cancel();
    }

    fn done(&self) -> futures::future::BoxFuture<'static, anyhow::Result<JobOutcome>> {
        let operation = self.operation.clone();
        let cancelled = Arc::clone(&self.cancel_requested);
        Box::pin(async move {
            Ok(match operation.done().await {
                Ok(result) => JobOutcome {
                    status: if cancelled.load(Ordering::Acquire) {
                        JobTerminalStatus::Killed
                    } else {
                        JobTerminalStatus::Completed
                    },
                    detail: Some(send_detail(&result)),
                    output: None,
                },
                Err(error) => JobOutcome {
                    status: JobTerminalStatus::Failed,
                    detail: Some(error.to_string()),
                    output: None,
                },
            })
        })
    }

    fn read_output(&self) -> Option<String> {
        Some(render_send_read(&self.operation.read_output()))
    }
}

fn send_parameters(background_enabled: bool) -> Value {
    let mut parameters = serde_json::Map::from_iter([
        (
            "sessionId".to_owned(),
            json!({ "type": "string", "required": true, "description": "Terminal session id returned by terminal_open or terminal_list." }),
        ),
        (
            "text".to_owned(),
            json!({ "type": "string", "required": true, "description": "UTF-8 text to write to the terminal." }),
        ),
        (
            "submit".to_owned(),
            json!({ "type": "boolean", "description": "Submit Enter after text (default true). Set false for control characters or incomplete REPL input." }),
        ),
    ]);
    if background_enabled {
        parameters.insert("run_in_background".to_owned(), json!({
            "type": "boolean", "description": "Return a job id immediately; collect with job_output or stop with job_kill."
        }));
    }
    Value::Object(parameters)
}

fn open_definition(
    terminals: Arc<seekdeep_terminal::TerminalSessionService>,
    max_bytes: usize,
    finalize: ToolContentFinalizer,
) -> anyhow::Result<ToolDefinition> {
    let mut properties = snapshot_properties();
    properties.insert(
        "motd".to_owned(),
        json!({ "type": "string", "required": true }),
    );
    let options = DefineToolOptions::new(
        "terminal_open",
        "Create a persistent, owner-isolated terminal session from a registered backend type. Use this for shell or REPL state that must survive across tool calls.",
        json!({
            "type": { "type": "string", "required": true, "description": "Registered terminal backend type, usually \"shell\"." },
            "name": { "type": "string", "description": "Optional owner-local display name such as \"main\" or \"gdb\"." },
            "cwd": { "type": "string", "description": "Initial working directory. Defaults to the deployment workspace root." }
        }),
        DefineToolOutput::new(
            json!({ "type": "object", "additionalProperties": false, "properties": properties }),
            Arc::new(move |_args: &SpawnArgs, value: &TerminalSpawnResult| {
                Ok(vec![ContentBlock::Text { text: render_spawn(value, max_bytes) }])
            }),
        ),
        Arc::new(move |args: SpawnArgs, run| {
            let terminals = terminals.clone();
            Box::pin(async move {
                if args.terminal_type.is_empty() {
                    anyhow::bail!("type must be a non-empty string");
                }
                terminals
                    .spawn(
                        require_agent(run.execution().agent.as_ref())?,
                        TerminalSpawnRequest {
                            terminal_type: args.terminal_type,
                            name: args.name,
                            cwd: args.cwd,
                        },
                        Some(run.signal()),
                    )
                    .await
                    .map_err(anyhow::Error::new)
            })
        }),
    )
    .finalize_content(finalize)
    .present_call(Arc::new(|args: &SpawnArgs| {
        Some(generic_call(
            format!(
                "Open terminal {}",
                args.name.as_deref().unwrap_or(&args.terminal_type)
            ),
            ToolCallKind::Execute,
            serde_json::to_value(args).ok()?,
        ))
    }));
    define_tool(options)
}

fn send_definition(
    context: Context,
    terminals: Arc<seekdeep_terminal::TerminalSessionService>,
    background_enabled: bool,
    max_bytes: usize,
    finalize: ToolContentFinalizer,
) -> anyhow::Result<ToolDefinition> {
    let output = DefineToolOutput::new(
        send_output_schema(),
        Arc::new(move |_args: &SendArgs, value: &SendValue| {
            let text = match value {
                SendValue::Background { job_id } => format!("started background job {job_id}"),
                foreground @ SendValue::Foreground { .. } => render_send(
                    &foreground.as_send_result().expect("foreground variant"),
                    max_bytes,
                ),
            };
            Ok(vec![ContentBlock::Text { text }])
        }),
    )
    .presentation_meta(Arc::new(|_args: &SendArgs, value: &SendValue| {
        Ok(match value {
            SendValue::Background { .. } => Value::Null,
            SendValue::Foreground {
                viewport,
                wait_reason,
                session_status,
                truncated,
            } => json!({
                "viewport": viewport,
                "waitReason": wait_reason,
                "sessionStatus": session_status,
                "truncated": truncated
            }),
        })
    }));
    let options = DefineToolOptions::new(
        "terminal_send",
        format!(
            "Send text to a persistent terminal. By default Enter is submitted and the call waits for a prompt, stdin wait, output silence, timeout, or session exit.{}",
            if background_enabled {
                " Background mode returns a job id for job_output/job_kill."
            } else {
                ""
            }
        ),
        send_parameters(background_enabled),
        output,
        Arc::new(move |args: SendArgs, run| {
            let context = context.clone();
            let terminals = terminals.clone();
            Box::pin(execute_send(
                context,
                terminals,
                background_enabled,
                max_bytes,
                args,
                run,
            ))
        }),
    )
    .finalize_content(finalize)
    .present_call(Arc::new(|args: &SendArgs| {
        Some(if args.run_in_background == Some(true) {
            generic_call(
                format!("Send to terminal {} in background", args.session_id),
                ToolCallKind::Execute,
                Value::String(args.text.clone()),
            )
        } else {
            ToolCallView::Terminal(TerminalCallView {
                title: if args.text.is_empty() {
                    "(send input)".to_owned()
                } else {
                    args.text.clone()
                },
                description: Some(format!("Terminal {}", args.session_id)),
                cwd: None,
            })
        })
    }))
    .present_result(Arc::new(|args: &SendArgs, result: &ToolResult| {
        if args.run_in_background == Some(true) || result.is_error {
            return None;
        }
        Some(ToolResultView::Terminal(TerminalResultView {
            title: None,
            output: Some(single_text(&result.content)?.to_owned()),
            exit_code: None,
            signal: None,
        }))
    }));
    define_tool(options)
}

fn read_definition(
    terminals: Arc<seekdeep_terminal::TerminalSessionService>,
    max_bytes: usize,
    finalize: ToolContentFinalizer,
) -> anyhow::Result<ToolDefinition> {
    define_tool(
        DefineToolOptions::new(
            "terminal_read",
            "Read a bounded page of retained output from a persistent terminal without sending input.",
            json!({
                "sessionId": { "type": "string", "required": true, "description": "Terminal session id." },
                "offset": { "type": "number", "description": "Newest-relative line offset (default 0)." },
                "count": { "type": "number", "description": "Requested line count (default 500; backend caps apply)." }
            }),
            DefineToolOutput::new(
                json!({
                    "type": "object", "additionalProperties": false,
                    "properties": {
                        "text": { "type": "string", "required": true },
                        "totalLines": { "type": "integer", "required": true },
                        "lineBegin": { "type": "integer", "required": true },
                        "lineEnd": { "type": "integer", "required": true },
                        "truncated": { "type": "boolean", "required": true }
                    }
                }),
                Arc::new(move |_args: &ReadArgs, value: &TerminalReadResult| {
                    Ok(vec![ContentBlock::Text { text: render_read(value, max_bytes) }])
                }),
            ),
            Arc::new(move |args: ReadArgs, run| {
                let terminals = terminals.clone();
                Box::pin(async move {
                    terminals
                        .read(
                            &require_agent(run.execution().agent.as_ref())?,
                            &session_id(&args.session_id)?,
                            TerminalReadRequest { offset: args.offset, count: args.count },
                        )
                        .map_err(anyhow::Error::new)
                })
            }),
        )
        .finalize_content(finalize)
        .present_call(Arc::new(|args: &ReadArgs| {
            Some(generic_call(
                format!("Read terminal {}", args.session_id),
                ToolCallKind::Read,
                serde_json::to_value(args).ok()?,
            ))
        })),
    )
}

fn signal_definition(
    terminals: Arc<seekdeep_terminal::TerminalSessionService>,
    finalize: ToolContentFinalizer,
) -> anyhow::Result<ToolDefinition> {
    define_tool(
        DefineToolOptions::new(
            "terminal_signal",
            "Send an allowed signal to the current foreground process group of a persistent terminal.",
            json!({
                "sessionId": { "type": "string", "required": true, "description": "Terminal session id." },
                "signal": { "type": "string", "required": true, "enum": ["SIGINT", "SIGTERM", "SIGKILL", "SIGTSTP", "SIGHUP"], "description": "Signal to deliver. Shell-targeted SIGKILL is rejected; use terminal_close." }
            }),
            DefineToolOutput::new(
                json!({
                    "type": "object", "additionalProperties": false,
                    "properties": {
                        "delivered": { "type": "boolean", "required": true, "const": true },
                        "targetPgid": { "type": "integer", "required": true }
                    }
                }),
                Arc::new(|args: &SignalArgs, value: &TerminalSignalResult| {
                    Ok(vec![ContentBlock::Text {
                        text: format!(
                            "delivered {:?} to foreground process group {}",
                            args.signal,
                            value.target_pgid.as_i64()
                        ),
                    }])
                }),
            ),
            Arc::new(move |args: SignalArgs, run| {
                let terminals = terminals.clone();
                Box::pin(async move {
                    terminals
                        .signal(
                            &require_agent(run.execution().agent.as_ref())?,
                            &session_id(&args.session_id)?,
                            args.signal,
                        )
                        .await
                        .map_err(anyhow::Error::new)
                })
            }),
        )
        .finalize_content(finalize)
        .present_call(Arc::new(|args: &SignalArgs| {
            Some(generic_call(
                format!("Signal terminal {}", args.session_id),
                ToolCallKind::Execute,
                serde_json::to_value(args).ok()?,
            ))
        })),
    )
}

fn close_definition(
    terminals: Arc<seekdeep_terminal::TerminalSessionService>,
    finalize: ToolContentFinalizer,
) -> anyhow::Result<ToolDefinition> {
    define_tool(
        DefineToolOptions::new(
            "terminal_close",
            "Close one persistent terminal and wait until its captured owned process tree is gone.",
            json!({ "sessionId": { "type": "string", "required": true, "description": "Terminal session id." } }),
            DefineToolOutput::new(
                json!({
                    "type": "object", "additionalProperties": false,
                    "properties": {
                        "sessionId": { "type": "string", "required": true },
                        "outcome": { "type": "string", "required": true, "enum": ["closed", "already-closing"] }
                    }
                }),
                Arc::new(|_args: &SessionArgs, value: &CloseValue| {
                    Ok(vec![ContentBlock::Text {
                        text: match value.outcome {
                            CloseOutcome::Closed => format!("closed terminal session {}", value.session_id),
                            CloseOutcome::AlreadyClosing => format!("terminal session {} was already closing", value.session_id),
                        },
                    }])
                }),
            ),
            Arc::new(move |args: SessionArgs, run| {
                let terminals = terminals.clone();
                Box::pin(async move {
                    let id = session_id(&args.session_id)?;
                    let closed = terminals
                        .kill(&require_agent(run.execution().agent.as_ref())?, &id, None)
                        .await
                        .map_err(anyhow::Error::new)?;
                    Ok(CloseValue {
                        session_id: id,
                        outcome: if closed { CloseOutcome::Closed } else { CloseOutcome::AlreadyClosing },
                    })
                })
            }),
        )
        .finalize_content(finalize)
        .present_call(Arc::new(|args: &SessionArgs| {
            Some(generic_call(
                format!("Close terminal {}", args.session_id),
                ToolCallKind::Delete,
                serde_json::to_value(args).ok()?,
            ))
        })),
    )
}

fn list_definition(
    terminals: Arc<seekdeep_terminal::TerminalSessionService>,
    max_bytes: usize,
    finalize: ToolContentFinalizer,
) -> anyhow::Result<ToolDefinition> {
    define_tool(
        DefineToolOptions::new(
            "terminal_list",
            "List persistent terminal sessions owned by the current agent.",
            json!({}),
            DefineToolOutput::new(
                json!({ "type": "array", "items": snapshot_schema() }),
                Arc::new(
                    move |_args: &NoArgs, value: &Vec<TerminalSessionSnapshot>| {
                        Ok(vec![ContentBlock::Text {
                            text: render_list(value, max_bytes),
                        }])
                    },
                ),
            ),
            Arc::new(move |_args: NoArgs, run| {
                let terminals = terminals.clone();
                Box::pin(async move {
                    Ok(terminals.list(&require_agent(run.execution().agent.as_ref())?))
                })
            }),
        )
        .finalize_content(finalize)
        .present_call(Arc::new(|_args: &NoArgs| {
            Some(generic_call(
                "List terminal sessions",
                ToolCallKind::Read,
                json!({}),
            ))
        })),
    )
}

fn normalize_config(value: &Value) -> anyhow::Result<Value> {
    let mut config = if value.is_null() {
        Config::default()
    } else {
        serde_json::from_value::<Config>(value.clone())?
    };
    if config.enable_run_in_background.is_none() {
        config.enable_run_in_background = Some(true);
    }
    if config.max_result_bytes.is_none() {
        config.max_result_bytes = Some(DEFAULT_MAX_RESULT_BYTES);
    }
    validate_config(config)?;
    Ok(serde_json::to_value(config)?)
}

fn validate_config(config: Config) -> anyhow::Result<(bool, usize)> {
    let max = config.max_result_bytes.unwrap_or(DEFAULT_MAX_RESULT_BYTES);
    anyhow::ensure!(
        (MIN_MAX_RESULT_BYTES..=MAX_SAFE_INTEGER).contains(&max),
        "tool-terminal: maxResultBytes must be a safe integer of at least {MIN_MAX_RESULT_BYTES}"
    );
    Ok((
        config.enable_run_in_background.unwrap_or(true),
        usize::try_from(max).unwrap_or(usize::MAX),
    ))
}

fn rollback(effects: Vec<EffectHandle>, error: anyhow::Error) -> anyhow::Error {
    let failures = effects
        .into_iter()
        .rev()
        .filter_map(|effect| futures::executor::block_on(effect.dispose()).err())
        .map(|error| format!("{error:#}"))
        .collect::<Vec<_>>();
    if failures.is_empty() {
        error
    } else {
        anyhow::anyhow!(
            "{error:#}; terminal tool rollback failed: {}",
            failures.join("; ")
        )
    }
}

/// Registers all six terminal tools and their usage guidance.
///
/// # Errors
///
/// Returns invalid config, missing-service, schema, prompt, or duplicate-tool failures.
pub fn apply(context: &Context, config: Config) -> anyhow::Result<()> {
    let (background_enabled, max_bytes) = validate_config(config)?;
    let terminals = context
        .get(TERMINALS)
        .ok_or_else(|| anyhow::anyhow!("tool-terminal requires terminals"))?;
    let tools = context
        .get(TOOLS)
        .ok_or_else(|| anyhow::anyhow!("tool-terminal requires tools"))?;
    let prompt = context
        .get(SYSTEM_PROMPT)
        .ok_or_else(|| anyhow::anyhow!("tool-terminal requires systemPrompt"))?;
    let finalize = finalizer(max_bytes);
    let definitions = vec![
        open_definition(terminals.clone(), max_bytes, finalize.clone())?,
        send_definition(
            context.clone(),
            terminals.clone(),
            background_enabled,
            max_bytes,
            finalize.clone(),
        )?,
        read_definition(terminals.clone(), max_bytes, finalize.clone())?,
        signal_definition(terminals.clone(), finalize.clone())?,
        close_definition(terminals.clone(), finalize.clone())?,
        list_definition(terminals, max_bytes, finalize)?,
    ];
    let mut effects = Vec::new();
    match prompt.section(
        context,
        PromptSection::new("tool:pty", GUIDANCE_ORDER, GUIDANCE),
    ) {
        Ok(effect) => effects.push(effect),
        Err(error) => return Err(error),
    }
    for definition in definitions {
        match tools.register(context, definition) {
            Ok(effect) => effects.push(effect),
            Err(error) => return Err(rollback(effects, error)),
        }
    }
    Ok(())
}

/// Builds the Loader-compatible terminal tool plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, config| {
        Box::pin(async move {
            let config = serde_json::from_value::<Config>(config)?;
            apply(&context, config)
        })
    })
    .with_config_validator(normalize_config)
}
