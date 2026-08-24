//! Model-facing foreground Ralph loop over the workflow and subagent seams. A
//! fixed script starts one fresh structured-output child per round, carrying
//! only the immutable objective and the previous bounded handoff between them.

use std::sync::Arc;

use seekdeep_cordis::Context;
use seekdeep_llm::ContentBlock;
use seekdeep_subagent::{SUBAGENTS, SubagentProvider, SubagentRuntime};
use seekdeep_system_prompt::{PromptSection, PromptText, SYSTEM_PROMPT};
use seekdeep_tools::{
    DefineToolOptions, DefineToolOutput, GenericCallView, GenericResultView, TOOLS, ToolCallView,
    ToolResult, ToolResultView, define_tool,
};
use seekdeep_workflow::{
    WORKFLOW_ENGINE, WorkflowMeta, WorkflowPhase, WorkflowResult, WorkflowRunId,
    WorkflowStartRequest, WorkflowStopReason,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// Cordis plugin name.
pub const NAME: &str = "tool-ralph";
/// Services required by the Ralph tool.
pub const INJECT: &[&str] = &["tools", "workflowEngine", "subagents", "systemPrompt"];

/// Largest value the source accepts as a safe integer.
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Deployment policy for the fixed Ralph workflow.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// Fresh structured-output provider used for every round.
    pub subagent_provider: String,
    /// Default and deployment ceiling for one call's round count.
    pub max_rounds: u64,
    /// Maximum serialized characters in one structured handoff.
    pub max_handoff_chars: u64,
    /// Maximum characters in a successful parent-facing terminal text.
    pub max_result_chars: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            subagent_provider: "spawn".to_owned(),
            max_rounds: 256,
            max_handoff_chars: 16_384,
            max_result_chars: 16_384,
        }
    }
}

/// Validated deployment policy.
#[derive(Clone, Debug)]
struct ResolvedConfig {
    subagent_provider: String,
    max_rounds: u64,
    max_handoff_chars: u64,
    max_result_chars: u64,
}

/// One structured round report's status.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum RalphRoundStatus {
    Continue,
    Complete,
    Blocked,
}

impl RalphRoundStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Continue => "continue",
            Self::Complete => "complete",
            Self::Blocked => "blocked",
        }
    }
}

/// The bounded structured report one round hands to the next.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RalphRoundReport {
    status: RalphRoundStatus,
    summary: String,
    evidence: Vec<String>,
    next_steps: Vec<String>,
    blocker: String,
}

/// How a run settled after its bounded report sequence.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "status",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
enum RalphTerminalResult {
    /// The worker reported completion.
    Complete {
        /// Rounds started.
        rounds_started: u64,
        /// Final report.
        report: RalphRoundReport,
    },
    /// The worker reported a concrete blocker.
    Blocked {
        /// Rounds started.
        rounds_started: u64,
        /// Final report.
        report: RalphRoundReport,
    },
    /// The round limit was reached with work remaining.
    BudgetLimited {
        /// Rounds started.
        rounds_started: u64,
        /// Final report.
        report: RalphRoundReport,
    },
    /// A round's child failed before producing a report.
    RoundFailed {
        /// Rounds started.
        rounds_started: u64,
        /// The last successful handoff, when a prior round completed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_report: Option<RalphRoundReport>,
    },
}

/// Raw schema-validated Ralph tool arguments.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RalphCallArgs {
    objective: String,
    #[serde(default)]
    max_rounds: Option<u64>,
}

/// Canonical Ralph tool output.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RalphToolOutput {
    run_id: WorkflowRunId,
    agents_started: u64,
    result: RalphTerminalResult,
}

/// Fixed, deployment-owned workflow identity block.
fn ralph_meta() -> WorkflowMeta {
    WorkflowMeta {
        name: "ralph-loop".to_owned(),
        description:
            "Iterate toward one objective with a fresh child and bounded structured handoff per round."
                .to_owned(),
        when_to_use: None,
        phases: Some(vec![WorkflowPhase {
            title: "Fresh-agent rounds".to_owned(),
            detail: Some("One clean child context per Ralph round.".to_owned()),
            provider: None,
            model: None,
        }]),
    }
}

/// Fixed, deployment-owned orchestration. The model supplies data only; it
/// cannot alter the loop, provider route, schema, or handoff validation.
const RALPH_SCRIPT: &str = r"
const reportSchema = {
  type: 'object',
  properties: {
    status: { type: 'string', enum: ['continue', 'complete', 'blocked'] },
    summary: { type: 'string' },
    evidence: { type: 'array', items: { type: 'string' } },
    nextSteps: { type: 'array', items: { type: 'string' } },
    blocker: { type: 'string' },
  },
  required: ['status', 'summary', 'evidence', 'nextSteps', 'blocker'],
  additionalProperties: false,
}

function normalizedText(value) {
  return typeof value === 'string' && value.length > 0 && value === value.trim()
}

function normalizedList(value) {
  return Array.isArray(value) && value.every(normalizedText)
}

function validateReport(report) {
  if (report === null || typeof report !== 'object' || Array.isArray(report)) {
    throw new Error('Ralph child returned no structured round report')
  }
  if (!normalizedText(report.summary)) {
    throw new Error('Ralph round report summary must be non-empty and normalized')
  }
  if (!normalizedList(report.evidence) || !normalizedList(report.nextSteps)) {
    throw new Error('Ralph round report evidence and nextSteps must contain only non-empty normalized strings')
  }
  if (typeof report.blocker !== 'string' || report.blocker !== report.blocker.trim()) {
    throw new Error('Ralph round report blocker must be a normalized string')
  }
  switch (report.status) {
    case 'continue':
      if (report.nextSteps.length === 0 || report.blocker !== '') {
        throw new Error('a continuing Ralph report needs nextSteps and an empty blocker')
      }
      break
    case 'complete':
      if (report.evidence.length === 0 || report.nextSteps.length !== 0 || report.blocker !== '') {
        throw new Error('a complete Ralph report needs evidence, no nextSteps, and an empty blocker')
      }
      break
    case 'blocked':
      if (!normalizedText(report.blocker)) {
        throw new Error('a blocked Ralph report needs a concrete blocker')
      }
      break
    default:
      throw new Error('Ralph round report status is invalid')
  }
  const serialized = JSON.stringify(report)
  if (serialized.length > args.maxHandoffChars) {
    throw new Error('Ralph round report exceeds maxHandoffChars (' + serialized.length + ' > ' + args.maxHandoffChars + ')')
  }
  return report
}

let previous
phase('Fresh-agent rounds')
for (let round = 1; round <= args.maxRounds; round += 1) {
  const prior = previous === undefined ? '(none — this is the first round)' : JSON.stringify(previous)
  const prompt = [
    'You are one fresh worker in a foreground Ralph loop. You receive no parent conversation and no prior child session. Do not call the ralph tool: this round already is its worker.',
    'Immutable objective:\n' + args.objective,
    'Ralph round: ' + round + ' of ' + args.maxRounds + '.',
    'The shared workspace and its current working tree are the long-term memory and source of truth. Inspect them before acting, preserve existing work, perform concrete in-scope work, and verify what you change. Treat the previous report only as a bounded handoff; confirm it against the workspace.',
    'Previous structured handoff:\n' + prior,
    'Return one report with exact normalized strings. Use status continue with at least one nextSteps entry while useful work remains; complete only with concrete evidence and no nextSteps; blocked only when no meaningful progress is possible without human input or an external-state change. blocker must be empty unless blocked.',
  ].join('\n\n')
  const rawReport = await agent(prompt, {
    label: 'Ralph round ' + round,
    phase: 'Fresh-agent rounds',
    schema: reportSchema,
  })
  if (rawReport === null) {
    return { status: 'round-failed', roundsStarted: round, lastReport: previous ?? null }
  }
  const report = validateReport(rawReport)
  if (report.status === 'complete') return { status: 'complete', roundsStarted: round, report }
  if (report.status === 'blocked') return { status: 'blocked', roundsStarted: round, report }
  previous = report
}
return { status: 'budget-limited', roundsStarted: args.maxRounds, report: previous }
";

const DESCRIPTION: &str = "Run a foreground fresh-agent Ralph loop toward one immutable objective. Use only when the direct human explicitly asks for Ralph or fresh-agent iteration. Each round opens a new child with no parent conversation or prior child session; the shared workspace is long-term memory, and only a bounded structured report crosses rounds. The call returns when a worker reports completion or a concrete blocker, or at the round limit. Ordinary long-running same-session work belongs to goal tools.";
/// Validate defaults even when a caller invokes `apply()` without Loader normalization.
fn resolve_config(config: &Config) -> anyhow::Result<ResolvedConfig> {
    let subagent_provider = &config.subagent_provider;
    if subagent_provider.is_empty() || *subagent_provider != subagent_provider.trim() {
        anyhow::bail!("subagentProvider must be a non-empty normalized string");
    }
    for (name, value) in [
        ("maxRounds", config.max_rounds),
        ("maxHandoffChars", config.max_handoff_chars),
        ("maxResultChars", config.max_result_chars),
    ] {
        if !(1..=MAX_SAFE_INTEGER).contains(&value) {
            anyhow::bail!("{name} must be a positive safe integer");
        }
    }
    Ok(ResolvedConfig {
        subagent_provider: subagent_provider.clone(),
        max_rounds: config.max_rounds,
        max_handoff_chars: config.max_handoff_chars,
        max_result_chars: config.max_result_chars,
    })
}

/// Resolve one model-selected cap against the deployment ceiling.
fn resolve_max_rounds(requested: Option<u64>, ceiling: u64) -> anyhow::Result<u64> {
    let value = requested.unwrap_or(ceiling);
    if !(1..=MAX_SAFE_INTEGER).contains(&value) {
        anyhow::bail!("Ralph maxRounds must be a positive safe integer");
    }
    if value > ceiling {
        anyhow::bail!("Ralph maxRounds {value} exceeds the deployment ceiling {ceiling}");
    }
    Ok(value)
}

/// Require the configured route to mean a genuinely fresh structured child.
fn require_fresh_provider(
    context: &Context,
    name: &str,
) -> anyhow::Result<Arc<dyn SubagentProvider>> {
    let runtime: Arc<SubagentRuntime> = context
        .get(SUBAGENTS)
        .ok_or_else(|| anyhow::anyhow!("tool-ralph requires subagents"))?;
    let provider = runtime
        .get_provider(name)
        .ok_or_else(|| anyhow::anyhow!("Ralph subagent provider \"{name}\" is not registered"))?;
    if !provider.capabilities().output_schema {
        anyhow::bail!("Ralph subagent provider \"{name}\" does not support structured output");
    }
    if provider.inherits_parent_context() {
        anyhow::bail!(
            "Ralph subagent provider \"{name}\" inherits parent context; Ralph requires a fresh provider"
        );
    }
    Ok(provider)
}

fn normalized_text(value: &str) -> bool {
    !value.is_empty() && value == value.trim()
}

fn normalized_list(array: &[Value]) -> bool {
    array
        .iter()
        .all(|value| value.as_str().is_some_and(normalized_text))
}

fn sorted_keys(object: &Map<String, Value>) -> String {
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    keys.join(",")
}

/// Defensively decode the fixed script's report across a provider boundary.
fn read_report(
    value: &Value,
    expected_status: RalphRoundStatus,
    max_chars: u64,
) -> anyhow::Result<RalphRoundReport> {
    let Some(object) = value.as_object() else {
        anyhow::bail!("Ralph workflow returned a malformed round report");
    };
    if sorted_keys(object) != "blocker,evidence,nextSteps,status,summary" {
        anyhow::bail!("Ralph workflow returned a malformed round report");
    }
    if object.get("status").and_then(Value::as_str) != Some(expected_status.as_str()) {
        anyhow::bail!("Ralph workflow returned a malformed round report");
    }
    let summary = object
        .get("summary")
        .and_then(Value::as_str)
        .filter(|value| normalized_text(value))
        .ok_or_else(|| anyhow::anyhow!("Ralph workflow returned a malformed round report"))?;
    let evidence = object
        .get("evidence")
        .and_then(Value::as_array)
        .filter(|value| normalized_list(value))
        .ok_or_else(|| anyhow::anyhow!("Ralph workflow returned a malformed round report"))?;
    let next_steps = object
        .get("nextSteps")
        .and_then(Value::as_array)
        .filter(|value| normalized_list(value))
        .ok_or_else(|| anyhow::anyhow!("Ralph workflow returned a malformed round report"))?;
    let blocker = object
        .get("blocker")
        .and_then(Value::as_str)
        .filter(|value| *value == value.trim())
        .ok_or_else(|| anyhow::anyhow!("Ralph workflow returned a malformed round report"))?;

    let report = RalphRoundReport {
        status: expected_status,
        summary: summary.to_owned(),
        evidence: evidence
            .iter()
            .map(|value| value.as_str().expect("checked").to_owned())
            .collect(),
        next_steps: next_steps
            .iter()
            .map(|value| value.as_str().expect("checked").to_owned())
            .collect(),
        blocker: blocker.to_owned(),
    };
    match expected_status {
        RalphRoundStatus::Continue => {
            if report.next_steps.is_empty() || !report.blocker.is_empty() {
                anyhow::bail!("Ralph workflow returned an invalid continuing report");
            }
        }
        RalphRoundStatus::Complete => {
            if report.evidence.is_empty()
                || !report.next_steps.is_empty()
                || !report.blocker.is_empty()
            {
                anyhow::bail!("Ralph workflow returned an invalid completion report");
            }
        }
        RalphRoundStatus::Blocked => {
            if !normalized_text(&report.blocker) {
                anyhow::bail!("Ralph workflow returned an invalid blocked report");
            }
        }
    }
    let chars = serde_json::to_string(&report).map_or(0, |value| value.chars().count());
    if chars as u64 > max_chars {
        anyhow::bail!("Ralph workflow returned an oversized handoff ({chars} > {max_chars})");
    }
    Ok(report)
}

/// Defensively decode the fixed script's terminal value.
fn read_run_result(
    value: &Value,
    max_rounds: u64,
    max_handoff_chars: u64,
) -> anyhow::Result<RalphTerminalResult> {
    let Some(object) = value.as_object() else {
        anyhow::bail!("Ralph workflow returned a malformed terminal result");
    };
    let Some(rounds_started) = object.get("roundsStarted").and_then(Value::as_u64) else {
        anyhow::bail!("Ralph workflow returned a malformed terminal result");
    };
    if rounds_started < 1 || rounds_started > max_rounds {
        anyhow::bail!("Ralph workflow returned a malformed terminal result");
    }
    match object.get("status").and_then(Value::as_str) {
        Some("complete" | "blocked" | "budget-limited") => {
            if sorted_keys(object) != "report,roundsStarted,status" {
                anyhow::bail!("Ralph workflow returned a malformed terminal result");
            }
            let report = object.get("report").ok_or_else(|| {
                anyhow::anyhow!("Ralph workflow returned a malformed terminal result")
            })?;
            match object.get("status").and_then(Value::as_str) {
                Some("complete") => Ok(RalphTerminalResult::Complete {
                    rounds_started,
                    report: read_report(report, RalphRoundStatus::Complete, max_handoff_chars)?,
                }),
                Some("blocked") => Ok(RalphTerminalResult::Blocked {
                    rounds_started,
                    report: read_report(report, RalphRoundStatus::Blocked, max_handoff_chars)?,
                }),
                _ => {
                    if rounds_started != max_rounds {
                        anyhow::bail!(
                            "Ralph workflow returned budget-limited before the round limit"
                        );
                    }
                    Ok(RalphTerminalResult::BudgetLimited {
                        rounds_started,
                        report: read_report(report, RalphRoundStatus::Continue, max_handoff_chars)?,
                    })
                }
            }
        }
        Some("round-failed") => {
            if sorted_keys(object) != "lastReport,roundsStarted,status" {
                anyhow::bail!("Ralph workflow returned a malformed terminal result");
            }
            let last_report = object.get("lastReport").ok_or_else(|| {
                anyhow::anyhow!("Ralph workflow returned a malformed terminal result")
            })?;
            if rounds_started == 1 {
                if !last_report.is_null() {
                    anyhow::bail!("Ralph workflow returned an invalid first-round failure");
                }
                Ok(RalphTerminalResult::RoundFailed {
                    rounds_started,
                    last_report: None,
                })
            } else {
                if last_report.is_null() {
                    anyhow::bail!(
                        "Ralph workflow returned a round failure without its last handoff"
                    );
                }
                Ok(RalphTerminalResult::RoundFailed {
                    rounds_started,
                    last_report: Some(read_report(
                        last_report,
                        RalphRoundStatus::Continue,
                        max_handoff_chars,
                    )?),
                })
            }
        }
        _ => anyhow::bail!("Ralph workflow returned an unknown terminal status"),
    }
}

/// A non-clean workflow finish is an error, never a partial Ralph success.
fn stop_reason_error(result: &WorkflowResult) -> Option<String> {
    match result.stop_reason {
        WorkflowStopReason::Completed => None,
        WorkflowStopReason::Cancelled => Some(format!(
            "Ralph workflow was cancelled{}",
            result
                .error
                .as_ref()
                .map_or(String::new(), |error| format!(" ({error})"))
        )),
        WorkflowStopReason::Error => Some(format!(
            "Ralph workflow failed: {}",
            result.error.as_deref().unwrap_or("unknown error")
        )),
    }
}

const TRUNCATION_NOTICE: &str = "\n… [truncated]";

/// Bound complete parent-facing text, including its envelope and truncation marker.
fn bound_result(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_owned();
    }
    let notice_len = TRUNCATION_NOTICE.chars().count();
    if max_chars <= notice_len {
        return TRUNCATION_NOTICE.chars().take(max_chars).collect();
    }
    let head: String = text.chars().take(max_chars - notice_len).collect();
    format!("{head}{TRUNCATION_NOTICE}")
}

/// Render the fixed terminal envelope without presenting self-report as certification.
fn render_run_result(terminal: &RalphTerminalResult, max_chars: usize) -> String {
    let (rounds_started, report) = match terminal {
        RalphTerminalResult::Complete {
            rounds_started,
            report,
        }
        | RalphTerminalResult::Blocked {
            rounds_started,
            report,
        }
        | RalphTerminalResult::BudgetLimited {
            rounds_started,
            report,
        } => (*rounds_started, report),
        RalphTerminalResult::RoundFailed { .. } => {
            return render_round_failure(terminal, max_chars);
        }
    };
    let rounds = format!(
        "{rounds_started} round{}",
        if rounds_started == 1 { "" } else { "s" }
    );
    let pretty = serde_json::to_string_pretty(report).unwrap_or_else(|_| "null".to_owned());
    let text = match terminal {
        RalphTerminalResult::Complete { .. } => {
            format!("Ralph worker reported completion after {rounds}.\nFinal report:\n{pretty}")
        }
        RalphTerminalResult::Blocked { .. } => {
            format!("Ralph worker reported a blocker after {rounds}.\nFinal report:\n{pretty}")
        }
        RalphTerminalResult::BudgetLimited { .. } => format!(
            "Ralph reached its {rounds} limit; the worker reported work remaining.\nFinal report:\n{pretty}"
        ),
        RalphTerminalResult::RoundFailed { .. } => unreachable!(),
    };
    bound_result(&text, max_chars)
}

/// Render an ordinary child failure with the most recent durable handoff.
fn render_round_failure(terminal: &RalphTerminalResult, max_chars: usize) -> String {
    let (rounds_started, last_report) = match terminal {
        RalphTerminalResult::RoundFailed {
            rounds_started,
            last_report,
        } => (*rounds_started, last_report.as_ref()),
        _ => return String::new(),
    };
    let header =
        format!("Ralph round {rounds_started} child failed before producing a structured report.");
    let text = match last_report {
        None => format!("{header}\nNo previous handoff was available."),
        Some(report) => format!(
            "{header}\nLast successful handoff:\n{}",
            serde_json::to_string_pretty(report).unwrap_or_else(|_| "null".to_owned())
        ),
    };
    bound_result(&text, max_chars)
}
/// Registers the fixed Ralph tool and its explicit-ask usage policy.
///
/// # Errors
///
/// Returns missing-service, prompt-registration, or tool-registration failures.
#[allow(clippy::too_many_lines)]
pub fn apply(context: &Context, config: &Config) -> anyhow::Result<()> {
    let resolved = resolve_config(config)?;

    let prompt = context
        .get(SYSTEM_PROMPT)
        .ok_or_else(|| anyhow::anyhow!("tool-ralph requires systemPrompt"))?;
    prompt.section(
        context,
        PromptSection::new(
            "tool:ralph",
            116.0,
            PromptText::Static(
                "Use the ralph tool ONLY when the direct human explicitly asks for a Ralph loop or fresh-agent iterative execution. Each Ralph round starts a fresh child with no conversation seed and uses the shared workspace as durable memory. Completion and blockers are worker reports, not independent evaluation. Use same-session goal tools for ordinary long-running objectives, and plain subagents or workflows for bounded delegation and fan-out."
                    .to_owned(),
            ),
        ),
    )?;

    let tools = context
        .get(TOOLS)
        .ok_or_else(|| anyhow::anyhow!("tool-ralph requires tools"))?;
    let execute_ctx = context.clone();

    let max_result_chars = usize::try_from(resolved.max_result_chars).unwrap_or(usize::MAX);
    let output = DefineToolOutput::new(
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "runId": {"type": "string", "required": true},
                "agentsStarted": {"type": "integer", "required": true},
                "result": {"type": "json", "required": true},
            },
        }),
        Arc::new(move |_args: &RalphCallArgs, value: &RalphToolOutput| {
            Ok(vec![ContentBlock::Text {
                text: render_run_result(&value.result, max_result_chars),
            }])
        }),
    );

    let resolved_execute = resolved.clone();
    let definition = define_tool(
        DefineToolOptions::new(
            "ralph",
            DESCRIPTION,
            json!({
                "objective": {
                    "type": "string",
                    "required": true,
                    "description": "The immutable completion objective for every fresh Ralph round.",
                },
                "maxRounds": {
                    "type": "number",
                    "description": "Optional positive safe-integer round cap, bounded by the deployment ceiling.",
                },
            }),
            output,
            Arc::new(move |args: RalphCallArgs, exec| {
                let ctx = execute_ctx.clone();
                let resolved = resolved_execute.clone();
                Box::pin(async move {
                    let parent = exec.agent.clone().ok_or_else(|| {
                        anyhow::anyhow!(
                            "Ralph tool requires a calling agent (exec.agent was undefined)"
                        )
                    })?;
                    let objective = args.objective.trim().to_owned();
                    if objective.is_empty() {
                        anyhow::bail!("Ralph objective must be a non-empty string");
                    }
                    let max_rounds = resolve_max_rounds(args.max_rounds, resolved.max_rounds)?;
                    require_fresh_provider(&ctx, &resolved.subagent_provider)?;

                    let engine = ctx
                        .get(WORKFLOW_ENGINE)
                        .ok_or_else(|| anyhow::anyhow!("tool-ralph requires workflowEngine"))?
                        .engine();
                    let run = engine.start(WorkflowStartRequest {
                        script: RALPH_SCRIPT.to_owned(),
                        meta: ralph_meta(),
                        args: Some(json!({
                            "objective": objective,
                            "maxRounds": max_rounds,
                            "maxHandoffChars": resolved.max_handoff_chars,
                        })),
                        subagent_provider: Some(resolved.subagent_provider.clone()),
                        max_total_agents: Some(max_rounds),
                        parent,
                        signal: Some(exec.signal()),
                    })?;

                    let signal = exec.signal();
                    let cancel_task = if signal.is_aborted() {
                        run.cancel(Some("parent step aborted"));
                        None
                    } else {
                        let bridge_run = Arc::clone(&run);
                        Some(tokio::spawn(async move {
                            signal.cancelled().await;
                            bridge_run.cancel(Some("parent step aborted"));
                        }))
                    };

                    let outcome = async {
                        let result = run.result().await;
                        if let Some(error) = stop_reason_error(&result) {
                            anyhow::bail!("{error}");
                        }
                        let terminal = read_run_result(
                            &result.value,
                            max_rounds,
                            resolved.max_handoff_chars,
                        )?;
                        if matches!(terminal, RalphTerminalResult::RoundFailed { .. }) {
                            anyhow::bail!("{}", render_round_failure(
                                &terminal,
                                usize::try_from(resolved.max_result_chars).unwrap_or(usize::MAX),
                            ));
                        }
                        Ok(RalphToolOutput {
                            run_id: run.id().clone(),
                            agents_started: result.agents_started,
                            result: terminal,
                        })
                    }
                    .await;
                    if let Some(cancel_task) = cancel_task {
                        cancel_task.abort();
                    }
                    run.dispose().await;
                    outcome
                })
            }),
        )
        .present_call(Arc::new(|args: &RalphCallArgs| {
            Some(ToolCallView::Generic(GenericCallView {
                title: "ralph".to_owned(),
                kind: None,
                raw_input: Some(json!(args.objective)),
                content: None,
                locations: None,
            }))
        }))
        .present_result(Arc::new(
            |_args: &RalphCallArgs, _result: &ToolResult| {
                Some(ToolResultView::Generic(GenericResultView {
                    title: None,
                    content: None,
                }))
            },
        )),
    )?;
    tools.register(context, definition)?;
    Ok(())
}
