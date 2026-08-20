//! Model-facing `job_output`, `job_list`, and `job_kill` tools over
//! `ctx.jobs`. Loading the plugin attaches the controller required by
//! producers. It also delivers unreported completions to the owning agent:
//! injected into a busy owner's next step, or opening a turn on an idle one
//! under the default `wakeup` delivery, bounded per owner.

use std::{collections::HashMap, sync::Arc};

use parking_lot::Mutex;
use seekdeep_agent::{Agent, AgentEvent, AgentStatus};
use seekdeep_agent_loop::AgentInboxClaimed;
use seekdeep_cordis::{Context, EventOptions, EventReply, Plugin};
use seekdeep_jobs::{JOBS, JobId, JobKillOutcome, JobSnapshot, JobStatus};
use seekdeep_llm::{ContentBlock, MessageSource, UserMessage, bound_context_summary};
use seekdeep_schemastery::Schema;
use seekdeep_system_prompt::{PromptSection, PromptText, SYSTEM_PROMPT};
use seekdeep_tools::{
    DefineToolOptions, DefineToolOutput, GenericCallView, TOOLS, ToolCallKind, ToolCallView,
    ToolContentFinalizer, ToolExecution, ToolExecutionResult, define_tool,
};
use seekdeep_util::output_retention::{TextRetainer, TextRetentionStrategy};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// Cordis plugin name.
pub const NAME: &str = "tool-jobs";

/// Services required by the job control tools.
pub const INJECT: &[&str] = &["tools", "jobs", "systemPrompt"];

/// Largest value the source accepts as a safe integer.
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// Default bounded wait applied when `job_output` sets `wait` without
/// `timeout_ms`.
const DEFAULT_WAIT_TIMEOUT_MS: f64 = 30_000.0;

/// Default hard cap on any single wait.
const DEFAULT_MAX_WAIT_TIMEOUT_MS: f64 = 600_000.0;

/// Default budget of completion wakes per owner.
const DEFAULT_MAX_CONSECUTIVE_WAKES: f64 = 3.0;

/// How an unreported completion reaches an owner that is already idle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompletionDelivery {
    /// Leave the completion pending until something else wakes the owner.
    Quiet,
    /// Open a turn for the idle owner.
    #[default]
    Wakeup,
}

/// Configures bounded `job_output` waits and completion-notice delivery.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    /// Wait duration applied when `job_output` sets `wait` without
    /// `timeout_ms`.
    pub wait_timeout_ms: Option<f64>,
    /// Hard cap on any single wait.
    pub max_wait_timeout_ms: Option<f64>,
    /// Whether a completion opens a turn on an idle owner.
    pub completion_delivery: Option<CompletionDelivery>,
    /// Turns one owner may have opened by completion wakes before notices
    /// degrade to injection.
    pub max_consecutive_wakes: Option<f64>,
}

/// The source-compatible admission schema for [`Config`].
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn config_schema() -> Schema {
    Schema::object([
        (
            "waitTimeoutMs",
            Schema::number()
                .min(1.0)
                .with_default(DEFAULT_WAIT_TIMEOUT_MS),
        ),
        (
            "maxWaitTimeoutMs",
            Schema::number()
                .min(1.0)
                .with_default(DEFAULT_MAX_WAIT_TIMEOUT_MS),
        ),
        (
            "completionDelivery",
            Schema::union([Schema::constant("quiet"), Schema::constant("wakeup")])
                .with_default("wakeup"),
        ),
        (
            "maxConsecutiveWakes",
            Schema::number()
                .min(1.0)
                .with_default(DEFAULT_MAX_CONSECUTIVE_WAKES),
        ),
    ])
}

/// Task state safe for model-authored programs; ownership/bookkeeping fields
/// are omitted.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PublicJobSnapshot {
    id: JobId,
    kind: String,
    label: String,
    status: JobStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    started_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    finished_at: Option<u64>,
}

/// Shared schema for job-control outputs.
fn public_task_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "id": {"type": "string", "required": true},
            "kind": {"type": "string", "required": true},
            "label": {"type": "string", "required": true},
            "status": {"type": "string", "required": true, "enum": [
                "running", "stopping", "completed", "killed", "failed"
            ]},
            "detail": {"type": "string"},
            "startedAt": {"type": "integer", "required": true},
            "finishedAt": {"type": "integer"}
        }
    })
}

/// Removes job ownership and notification bookkeeping from a snapshot.
fn public_job(snapshot: &JobSnapshot) -> PublicJobSnapshot {
    PublicJobSnapshot {
        id: snapshot.id.clone(),
        kind: snapshot.kind.clone(),
        label: snapshot.label.clone(),
        status: snapshot.status,
        detail: snapshot.detail.clone(),
        started_at: snapshot.started_at,
        finished_at: snapshot.finished_at,
    }
}

fn job_status_str(status: JobStatus) -> &'static str {
    match status {
        JobStatus::Running => "running",
        JobStatus::Stopping => "stopping",
        JobStatus::Completed => "completed",
        JobStatus::Killed => "killed",
        JobStatus::Failed => "failed",
    }
}

/// Renders generic status with optional producer detail.
#[must_use]
pub fn status_line(status: JobStatus, detail: Option<&str>) -> String {
    match detail {
        Some(detail) => format!("[status: {}, {detail}]", job_status_str(status)),
        None => format!("[status: {}]", job_status_str(status)),
    }
}

fn retain_tail(text: &str, max_bytes: usize) -> String {
    let mut retainer = TextRetainer::new(TextRetentionStrategy::Tail { max_bytes });
    retainer.push_str(text);
    retainer.finish().text
}

fn retain_head(text: &str, max_bytes: usize) -> String {
    let mut retainer = TextRetainer::new(TextRetentionStrategy::Head { max_bytes });
    retainer.push_str(text);
    retainer.finish().text
}

fn fit_with_suffix(content: &str, suffix: &str, max_bytes: Option<usize>, omitted: &str) -> String {
    let complete = format!("{content}{suffix}");
    if max_bytes.is_none_or(|max| complete.len() <= max) {
        return complete;
    }
    let max_bytes = max_bytes.expect("checked above");
    let fixed = format!(
        "{}{suffix}",
        if content.ends_with(omitted.trim_start()) {
            ""
        } else {
            omitted
        }
    );
    let fixed_bytes = fixed.len();
    if fixed_bytes >= max_bytes {
        return retain_tail(&fixed, max_bytes);
    }
    format!("{}{}", retain_tail(content, max_bytes - fixed_bytes), fixed)
}

/// One-line account of a settled job for the notice form's collapsed row.
fn completion_summary(snapshot: &JobSnapshot) -> String {
    bound_context_summary(&format!(
        "{} {} {}",
        snapshot.kind,
        snapshot.label,
        status_line(snapshot.status, snapshot.detail.as_deref())
    ))
}

fn fit_completion_notice(snapshot: &JobSnapshot) -> String {
    let prefix = format!("background job {}", snapshot.id);
    let detail = format!(
        " ({}: {}) finished {}",
        snapshot.kind,
        snapshot.label,
        status_line(snapshot.status, snapshot.detail.as_deref())
    );
    let action = "\nDone; job_output.";
    let complete = format!("{prefix}{detail}. Read its output with job_output.");
    let Some(max_bytes) = snapshot.output_limit_bytes.map(usize_from_u64) else {
        return complete;
    };
    if complete.len() <= max_bytes {
        return complete;
    }
    let omitted = "\n[notice truncated]";
    let fixed = format!("{prefix}{omitted}{action}");
    let fixed_bytes = fixed.len();
    if fixed_bytes <= max_bytes {
        if fixed_bytes == max_bytes {
            return fixed;
        }
        return format!(
            "{prefix}{}{omitted}{action}",
            retain_head(&detail, max_bytes - fixed_bytes)
        );
    }
    let compact = format!("{prefix}{action}");
    let compact_bytes = compact.len();
    if compact_bytes <= max_bytes {
        return compact;
    }
    let action_bytes = action.len();
    if action_bytes >= max_bytes {
        return retain_tail(action, max_bytes);
    }
    format!(
        "{}{}",
        retain_head(&prefix, max_bytes - action_bytes),
        action
    )
}

fn raw_single_text(content: &[ContentBlock]) -> Option<&str> {
    if content.len() != 1 {
        return None;
    }
    match &content[0] {
        ContentBlock::Text { text } => Some(text),
        _ => None,
    }
}

fn bound_single_text(content: &[ContentBlock], max_bytes: usize) -> Option<Vec<ContentBlock>> {
    let text = raw_single_text(content)?;
    Some(vec![ContentBlock::Text {
        text: fit_with_suffix(text, "", Some(max_bytes), "\n[result truncated]"),
    }])
}

fn visible_output_limit(ctx: &Context, exec: &ToolExecution) -> Option<usize> {
    if exec.name != "job_output" && exec.name != "job_kill" {
        return None;
    }
    let job_id = exec.arguments.get("job_id")?.as_str()?;
    if job_id.is_empty() {
        return None;
    }
    let jobs = ctx.get(JOBS)?;
    jobs.list(exec.agent.as_ref())
        .into_iter()
        .find(|snapshot| snapshot.id.as_str() == job_id)
        .and_then(|snapshot| snapshot.output_limit_bytes.map(usize_from_u64))
}

/// Converts a positive safe byte budget to a `usize`, saturating on overflow.
fn usize_from_u64(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

fn agent_key(agent: &Arc<Agent>) -> usize {
    Arc::as_ptr(agent) as usize
}

fn notice_source(summary: &str) -> MessageSource {
    let mut fields = Map::new();
    fields.insert("plugin".to_owned(), json!("tool-jobs"));
    fields.insert("form".to_owned(), json!("notice"));
    fields.insert("summary".to_owned(), json!(summary));
    MessageSource {
        kind: "plugin".to_owned(),
        fields,
    }
}

/// Validates a non-empty `job_id` that the parameter schema cannot express.
fn validate_job_id(value: &str) -> anyhow::Result<JobId> {
    if value.is_empty() {
        anyhow::bail!(
            "invalid job_id: expected a non-empty string, got {}",
            json!(value)
        );
    }
    Ok(JobId::new(value))
}

fn present_task_call(
    title: impl Into<String>,
    kind: ToolCallKind,
    raw_input: Option<Value>,
) -> ToolCallView {
    ToolCallView::Generic(GenericCallView {
        title: title.into(),
        kind: Some(kind),
        raw_input,
        content: None,
        locations: None,
    })
}

/// Applies the shared content bounder for `job_output` and `job_kill`.
fn finalize_task_content(
    ctx: &Context,
    exec: &ToolExecution,
    result: &ToolExecutionResult,
) -> Option<Vec<ContentBlock>> {
    let max_bytes = visible_output_limit(ctx, exec)?;
    let (is_error, value, content) = match result {
        ToolExecutionResult::Success(success) => (false, Some(&success.value), &success.content),
        ToolExecutionResult::Failure(failure) => (true, None, &failure.content),
    };
    if exec.name == "job_output"
        && !is_error
        && let Some(value) = value
    {
        let text = value
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if let Some(job) = value
            .get("job")
            .and_then(|job| serde_json::from_value::<PublicJobSnapshot>(job.clone()).ok())
        {
            let body = if text.is_empty() {
                "(no new output)"
            } else {
                text
            };
            let content_str = body.strip_suffix('\n').unwrap_or(body);
            let suffix = format!("\n{}", status_line(job.status, job.detail.as_deref()));
            let expected = format!("{content_str}{suffix}");
            if raw_single_text(content).is_some_and(|rendered| rendered == expected) {
                return Some(vec![ContentBlock::Text {
                    text: fit_with_suffix(
                        content_str,
                        &suffix,
                        Some(max_bytes),
                        "\n[output truncated]",
                    ),
                }]);
            }
        }
    }
    bound_single_text(content, max_bytes)
}

/// Raw schema-validated `job_output` arguments.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobOutputArgs {
    job_id: String,
    #[serde(default)]
    wait: Option<bool>,
    #[serde(default)]
    timeout_ms: Option<f64>,
}

/// Canonical `job_output` value.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobOutputValue {
    text: String,
    job: PublicJobSnapshot,
}

/// Empty argument set for `job_list`.
#[derive(Clone, Debug, Deserialize)]
struct NoArgs {}

/// Raw schema-validated `job_kill` arguments.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobKillArgs {
    job_id: String,
    #[serde(default)]
    reason: Option<String>,
}

/// Canonical `job_kill` value.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobKillValue {
    outcome: String,
    job: PublicJobSnapshot,
}

/// Registers the three job control tools and the completion-notice delivery.
///
/// # Errors
///
/// Returns missing-service, prompt-registration, listener, or tool
/// registration failures, and invalid admission configuration.
#[allow(clippy::too_many_lines)]
pub fn apply(context: &Context, config: &Config) -> anyhow::Result<()> {
    let wait_default = config.wait_timeout_ms.unwrap_or(DEFAULT_WAIT_TIMEOUT_MS);
    let wait_cap = config
        .max_wait_timeout_ms
        .unwrap_or(DEFAULT_MAX_WAIT_TIMEOUT_MS);
    let delivery = config.completion_delivery.unwrap_or_default();
    let wake_budget = config
        .max_consecutive_wakes
        .unwrap_or(DEFAULT_MAX_CONSECUTIVE_WAKES);

    if wait_default > wait_cap {
        anyhow::bail!(
            "tool-jobs: waitTimeoutMs ({wait_default}) exceeds maxWaitTimeoutMs ({wait_cap})"
        );
    }
    if !wake_budget.is_finite()
        || wake_budget.fract() != 0.0
        || wake_budget.abs() > MAX_SAFE_INTEGER
    {
        anyhow::bail!(
            "tool-jobs: maxConsecutiveWakes ({wake_budget}) must be a whole number of turns"
        );
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let wake_budget = wake_budget as u64;

    let jobs = context
        .get(JOBS)
        .ok_or_else(|| anyhow::anyhow!("tool-jobs requires jobs"))?;
    let spent_wakes: Arc<Mutex<HashMap<usize, u64>>> = Arc::new(Mutex::new(HashMap::new()));

    if delivery == CompletionDelivery::Wakeup {
        let spent_wakes_claimed = spent_wakes.clone();
        context.events().on_sync(
            context,
            "agent/inbox/claimed",
            move |_, args| {
                let Some(event) = args.get::<AgentEvent<AgentInboxClaimed>>(0) else {
                    return Ok(EventReply::Undefined);
                };
                if event.payload.message.source().kind.as_str() == "user" {
                    spent_wakes_claimed.lock().remove(&agent_key(&event.agent));
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )?;
    }

    // Producers may start work only while a controller is attached.
    let _ = jobs.attach_controller("tool-jobs");

    let prompt = context
        .get(SYSTEM_PROMPT)
        .ok_or_else(|| anyhow::anyhow!("tool-jobs requires systemPrompt"))?;
    prompt.section(
        context,
        PromptSection::new(
            "tool:jobs",
            106.0,
            PromptText::Static(
                "Track every background job id you start. You are notified in-session when a job finishes — do not busy-poll or sleep on one; keep working on independent steps and do not duplicate a running job's work. Before giving a final answer, collect every still-relevant job with job_output (set wait: true only when you are genuinely blocked on it), and job_kill jobs that stopped mattering."
                    .to_owned(),
            ),
        ),
    )?;

    let spent_wakes_done = spent_wakes.clone();
    jobs.on_job_done(Arc::new(move |snapshot, owner| {
        if snapshot.reported {
            return;
        }
        let Some(owner) = owner else {
            return;
        };
        let message = UserMessage::new(
            vec![ContentBlock::Text {
                text: fit_completion_notice(snapshot),
            }],
            notice_source(&completion_summary(snapshot)),
        );
        let key = agent_key(owner);
        let spent = spent_wakes_done.lock().get(&key).copied().unwrap_or(0);
        if delivery == CompletionDelivery::Wakeup
            && owner.status() == AgentStatus::Idle
            && spent < wake_budget
        {
            spent_wakes_done.lock().insert(key, spent + 1);
            let _ = owner.followup(message);
            return;
        }
        let _ = owner.inject(message);
    }));

    let tools = context
        .get(TOOLS)
        .ok_or_else(|| anyhow::anyhow!("tool-jobs requires tools"))?;

    let finalize_ctx = context.clone();
    let finalize: ToolContentFinalizer =
        Arc::new(move |exec, result| Ok(finalize_task_content(&finalize_ctx, exec, result)));

    let execute_ctx = context.clone();
    let output_wait_default = wait_default;
    let output_wait_cap = wait_cap;

    let job_output = define_tool(
        DefineToolOptions::new(
            "job_output",
            "Read a background job. Stream jobs return only output since the previous read; final-output jobs return their result after settlement. Every response ends with `[status: ...]`. Reads are non-blocking unless `wait: true`, which waits up to the configured cap.",
            json!({
                "job_id": {"type": "string", "required": true, "description": "Job id returned by the tool that started the background work."},
                "wait": {"type": "boolean", "description": "Block until the job reaches a terminal status or the timeout expires. A timed-out wait returns [status: running] and leaves the job alive."},
                "timeout_ms": {"type": "number", "description": "Max wait in milliseconds (only meaningful with wait: true). Defaults to the configured wait timeout; capped by the configured maximum."}
            }),
            DefineToolOutput::new(
                {
                    let mut job = public_task_schema();
                    job["required"] = json!(true);
                    json!({
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "text": {"type": "string", "required": true},
                            "job": job
                        }
                    })
                },
                Arc::new(|_args: &JobOutputArgs, value: &JobOutputValue| {
                    let body = if value.text.is_empty() {
                        "(no new output)"
                    } else {
                        value.text.as_str()
                    };
                    let separator = if body.ends_with('\n') { "" } else { "\n" };
                    Ok(vec![ContentBlock::Text {
                        text: format!(
                            "{body}{separator}{}",
                            status_line(value.job.status, value.job.detail.as_deref())
                        ),
                    }])
                }),
            ),
            Arc::new(move |args: JobOutputArgs, exec| {
                let ctx = execute_ctx.clone();
                let wait_default = output_wait_default;
                let wait_cap = output_wait_cap;
                Box::pin(async move {
                    let id = validate_job_id(&args.job_id)?;
                    if args.wait == Some(true) {
                        let timeout = args.timeout_ms.unwrap_or(wait_default).min(wait_cap);
                        let jobs = ctx
                            .get(JOBS)
                            .ok_or_else(|| anyhow::anyhow!("tool-jobs requires jobs"))?;
                        jobs.wait(&id, timeout, exec.agent.as_ref(), Some(exec.signal())).await?;
                    }
                    let jobs = ctx
                        .get(JOBS)
                        .ok_or_else(|| anyhow::anyhow!("tool-jobs requires jobs"))?;
                    let read = jobs.read(&id, exec.agent.as_ref())?;
                    Ok(JobOutputValue {
                        text: read.text,
                        job: public_job(&read.snapshot),
                    })
                })
            }),
        )
        .finalize_content(finalize.clone())
        .present_call(Arc::new(|args: &JobOutputArgs| {
            Some(present_task_call(
                format!("Read output from background job {}", args.job_id),
                ToolCallKind::Read,
                Some(json!(args.job_id)),
            ))
        })),
    )?;
    tools.register(context, job_output)?;

    let list_ctx = context.clone();
    let job_list = define_tool(
        DefineToolOptions::new(
            "job_list",
            "List your background jobs (running and finished) with their ids, kinds, and statuses.",
            json!({}),
            DefineToolOutput::new(
                json!({ "type": "array", "items": public_task_schema() }),
                Arc::new(|_args: &NoArgs, jobs: &Vec<PublicJobSnapshot>| {
                    let text = if jobs.is_empty() {
                        "(no background jobs)".to_owned()
                    } else {
                        jobs.iter()
                            .map(|t| {
                                format!(
                                    "{} [{}] {} — {}",
                                    t.id,
                                    t.kind,
                                    job_status_str(t.status),
                                    t.label
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    };
                    Ok(vec![ContentBlock::Text { text }])
                }),
            ),
            Arc::new(move |_args: NoArgs, exec| {
                let ctx = list_ctx.clone();
                Box::pin(async move {
                    let jobs = ctx
                        .get(JOBS)
                        .ok_or_else(|| anyhow::anyhow!("tool-jobs requires jobs"))?;
                    Ok(jobs
                        .list(exec.agent.as_ref())
                        .iter()
                        .map(public_job)
                        .collect::<Vec<_>>())
                })
            }),
        )
        .present_call(Arc::new(|_args: &NoArgs| {
            Some(present_task_call(
                "List background jobs",
                ToolCallKind::Read,
                None,
            ))
        })),
    )?;
    tools.register(context, job_list)?;

    let kill_ctx = context.clone();
    let job_kill = define_tool(
        DefineToolOptions::new(
            "job_kill",
            "Request cancellation of a running background job by job id. Returns immediately; the job settles as killed once its work actually stops.",
            json!({
                "job_id": {"type": "string", "required": true, "description": "Job id returned by the tool that started the background work."},
                "reason": {"type": "string", "description": "Optional short reason, recorded in the log and forwarded to the job."}
            }),
            DefineToolOutput::new(
                {
                    let mut job = public_task_schema();
                    job["required"] = json!(true);
                    json!({
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "outcome": {"type": "string", "required": true, "enum": ["cancellation-requested", "already-finished"]},
                            "job": job
                        }
                    })
                },
                Arc::new(|_args: &JobKillArgs, value: &JobKillValue| {
                    let text = if value.outcome == "already-finished" {
                        format!(
                            "job {} had already finished {}",
                            value.job.id,
                            status_line(value.job.status, value.job.detail.as_deref())
                        )
                    } else {
                        format!("requested cancellation of job {}", value.job.id)
                    };
                    Ok(vec![ContentBlock::Text { text }])
                }),
            ),
            Arc::new(move |args: JobKillArgs, exec| {
                let ctx = kill_ctx.clone();
                Box::pin(async move {
                    let id = validate_job_id(&args.job_id)?;
                    let jobs = ctx
                        .get(JOBS)
                        .ok_or_else(|| anyhow::anyhow!("tool-jobs requires jobs"))?;
                    let result = jobs.kill(&id, exec.agent.as_ref(), args.reason.as_deref())?;
                    let snapshot = public_job(&jobs.get(&id, exec.agent.as_ref())?);
                    Ok(JobKillValue {
                        outcome: match result {
                            JobKillOutcome::AlreadyFinished => "already-finished".to_owned(),
                            JobKillOutcome::Requested => "cancellation-requested".to_owned(),
                        },
                        job: snapshot,
                    })
                })
            }),
        )
        .finalize_content(finalize)
        .present_call(Arc::new(|args: &JobKillArgs| {
            Some(present_task_call(
                format!("Kill background job {}", args.job_id),
                ToolCallKind::Execute,
                Some(json!(args.job_id)),
            ))
        })),
    )?;
    tools.register(context, job_kill)?;

    Ok(())
}

/// Builds the loader-compatible job control tools plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, config| {
        Box::pin(async move {
            let config: Config = serde_json::from_value(config)?;
            apply(&context, &config)?;
            Ok(())
        })
    })
    .with_config_validator(|value: &Value| {
        config_schema()
            .resolve(value)
            .map_err(|error| anyhow::anyhow!("{error}"))
    })
}
