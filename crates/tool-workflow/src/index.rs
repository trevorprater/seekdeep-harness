//! Model-facing workflow tool: run an orchestration script that fans out
//! subagents.

use std::sync::Arc;

use parking_lot::Mutex;
use seekdeep_cordis::{Context, EventOptions, EventReply};
use seekdeep_core::session::{AppendOptions, Session};
use seekdeep_llm::ContentBlock;
use seekdeep_system_prompt::{PromptSection, PromptText, SYSTEM_PROMPT};
use seekdeep_tools::{
    DefineToolOptions, DefineToolOutput, GenericCallView, GenericResultView, TOOLS, ToolCallView,
    ToolResult, ToolResultView, define_tool,
};
use seekdeep_workflow::{
    WORKFLOW_ENGINE, WorkflowAgentEndInfo, WorkflowAgentInfo, WorkflowMeta, WorkflowResult,
    WorkflowRun, WorkflowRunId, WorkflowStartRequest, WorkflowStopReason,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::types::{
    ToolWorkflowAgentEndData, ToolWorkflowAgentStartData, ToolWorkflowRunEndData,
    ToolWorkflowRunStartData,
};

/// Cordis plugin name.
pub const NAME: &str = "tool-workflow";
/// Services required by the workflow tool.
pub const INJECT: &[&str] = &["tools", "workflowEngine", "systemPrompt"];

/// Plugin config: the model-facing tool name plus result rendering caps.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// The model-facing tool name to register.
    pub tool_name: String,
    /// Rendered-result ceiling in characters.
    pub max_result_chars: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            tool_name: "workflow".to_owned(),
            max_result_chars: 50_000,
        }
    }
}

/// The script-authoring contract, embedded in the tool description.
const DESCRIPTION: &str = r"Run a JavaScript workflow script that orchestrates subagents at scale. Use this for work that fans out across many independent pieces — an audit over many files, a migration, multi-angle research, adversarial verification of findings — where you write the orchestration as a script instead of delegating turn by turn.

The workflow's identity rides the `meta` parameter as JSON: required `name` (short kebab-case) and `description` strings, optional `whenToUse` string and `phases` array (`{title, detail?, provider?, model?}`). The `script` parameter is the plain JavaScript body ONLY (NOT TypeScript, and NO `export const meta` statement — meta is a parameter, not code), running with top-level await; end with `return <value>` — the value must be JSON-serializable and is this tool's result.

Script-body hooks:
- `agent(prompt, opts?): Promise<any>` — run one subagent to completion. Without `opts.schema` it resolves to the child's final text; with `opts.schema` (an object-rooted JSON Schema using ONLY type/properties/required/additionalProperties/items/enum/const/oneOf — no pattern/format/numeric bounds) it resolves to the validated object. Resolves `null` when the child fails (filter with `.filter(Boolean)`). Other opts: `label` (display), `phase` (progress group), and independent `provider`/`model` LLM target overrides (either may be provided alone). Anything else (`effort`/`isolation`/`agentType`) is rejected loudly.
- `pipeline(items, ...stages): Promise<any[]>` — run each item through the stages independently with NO barrier between stages (prefer this for multi-stage work). Each stage receives `(prev, item, index)`. An ordinary stage throw drops that ITEM to `null` and skips its remaining stages.
- `parallel(thunks): Promise<any[]>` — run zero-argument functions concurrently and await ALL of them (a barrier; use only when a stage genuinely needs every prior result together). A throwing thunk resolves to `null`.
- `phase(title)` — start a progress phase; `log(message)` — narrate progress; `args` — the tool call's `args` input, verbatim.

Misused hooks (bad arguments, unknown options, unsupported schemas, tripped caps) throw errors that ALWAYS kill the script — they never dissolve into a per-item `null`.

Constraints: concurrency and total-agent caps apply; no filesystem, network, timers, or Node.js APIs are provided — the agents do the work, the script only coordinates them. The run executes in the foreground: this call returns when the whole script finishes.";

/// Raw schema-validated workflow tool arguments.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkflowCallArgs {
    script: String,
    meta: WorkflowMeta,
    #[serde(default)]
    args: Option<Value>,
}

/// Canonical workflow tool output.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkflowToolOutput {
    run_id: WorkflowRunId,
    agents_started: u64,
    result: Value,
}

/// A non-completed stop reason means the script did not finish cleanly.
fn stop_reason_error(result: &WorkflowResult) -> Option<String> {
    match result.stop_reason {
        WorkflowStopReason::Completed => None,
        WorkflowStopReason::Cancelled => Some(format!(
            "workflow run was cancelled{}",
            result
                .error
                .as_ref()
                .map_or(String::new(), |error| format!(" ({error})"))
        )),
        WorkflowStopReason::Error => Some(format!(
            "workflow run failed: {}",
            result.error.as_deref().unwrap_or("unknown error")
        )),
    }
}

/// Renders the run's outcome text: meta name, agent count, and capped JSON value.
fn render_result(name: &str, agents_started: u64, value: &Value, max_chars: usize) -> String {
    let rendered = serde_json::to_string_pretty(value).unwrap_or_else(|_| "null".to_owned());
    let clipped = if rendered.chars().count() > max_chars {
        let head: String = rendered.chars().take(max_chars).collect();
        let remainder = rendered.chars().count() - max_chars;
        format!("{head}… [truncated: {remainder} more characters]")
    } else {
        rendered
    };
    let agent_word = if agents_started == 1 {
        "agent"
    } else {
        "agents"
    };
    let header = format!("workflow {name:?} completed ({agents_started} {agent_word}).");
    let mut text = String::new();
    text.push_str(&header);
    text.push('\n');
    text.push_str("Return value:");
    text.push('\n');
    text.push_str(&clipped);
    text
}

/// Tracks active top-level workflow runs and appends durable records.
struct WorkflowRecorder {
    active: Mutex<std::collections::HashMap<String, Arc<Session>>>,
}

impl WorkflowRecorder {
    fn new(context: &Context) -> Arc<Self> {
        let recorder = Arc::new(Self {
            active: Mutex::new(std::collections::HashMap::new()),
        });
        let agent_start = Arc::clone(&recorder);
        context
            .events()
            .on_sync(
                context,
                "workflow/agent-start",
                move |_, args| {
                    let Some(info) = args.get::<seekdeep_workflow::WorkflowRunInfo>(0) else {
                        return Ok(EventReply::Undefined);
                    };
                    let Some(agent) = args.get::<WorkflowAgentInfo>(1) else {
                        return Ok(EventReply::Undefined);
                    };
                    let session = agent_start.active.lock().get(info.id.as_str()).cloned();
                    if let Some(session) = session {
                        let data = ToolWorkflowAgentStartData {
                            run_id: info.id.clone(),
                            seq: agent.seq,
                            label: agent.label.clone(),
                            phase: agent.phase.clone(),
                            child_id: agent.child_id.clone(),
                        };
                        if append_record(&session, "tool-workflow/agent-start", &data).is_err() {
                            agent_start.active.lock().remove(info.id.as_str());
                        }
                    }
                    Ok(EventReply::Undefined)
                },
                global_events(),
            )
            .expect("workflow/agent-start listener");
        let agent_end = Arc::clone(&recorder);
        context
            .events()
            .on_sync(
                context,
                "workflow/agent-end",
                move |_, args| {
                    let Some(info) = args.get::<seekdeep_workflow::WorkflowRunInfo>(0) else {
                        return Ok(EventReply::Undefined);
                    };
                    let Some(agent) = args.get::<WorkflowAgentEndInfo>(1) else {
                        return Ok(EventReply::Undefined);
                    };
                    let session = agent_end.active.lock().get(info.id.as_str()).cloned();
                    if let Some(session) = session {
                        let data = ToolWorkflowAgentEndData {
                            run_id: info.id.clone(),
                            seq: agent.info.seq,
                            outcome: agent.outcome,
                        };
                        if append_record(&session, "tool-workflow/agent-end", &data).is_err() {
                            agent_end.active.lock().remove(info.id.as_str());
                        }
                    }
                    Ok(EventReply::Undefined)
                },
                global_events(),
            )
            .expect("workflow/agent-end listener");
        recorder
    }

    fn start(&self, session: &Arc<Session>, run: &Arc<dyn WorkflowRun>) {
        let data = ToolWorkflowRunStartData {
            run_id: run.id().clone(),
            name: run.meta().name.clone(),
        };
        if append_record(session, "tool-workflow/run-start", &data).is_ok() {
            self.active
                .lock()
                .insert(run.id().as_str().to_owned(), Arc::clone(session));
        }
    }

    fn finish(&self, run_id: &WorkflowRunId, stop_reason: WorkflowStopReason) {
        let session = self.active.lock().remove(run_id.as_str());
        if let Some(session) = session {
            let data = ToolWorkflowRunEndData {
                run_id: run_id.clone(),
                stop_reason,
            };
            let _ = append_record(&session, "tool-workflow/run-end", &data);
        }
    }

    fn abandon(&self, run_id: &WorkflowRunId) {
        self.active.lock().remove(run_id.as_str());
    }
}

fn append_record(
    session: &Arc<Session>,
    event_type: &str,
    data: &impl Serialize,
) -> anyhow::Result<()> {
    match session.append(
        event_type,
        serde_json::to_value(data)?,
        AppendOptions::default(),
    ) {
        Ok(_) => Ok(()),
        Err(error) => {
            tracing::warn!(%error, "tool-workflow: disabled durable record after {event_type} append failed");
            Err(error.into())
        }
    }
}

fn global_events() -> EventOptions {
    EventOptions {
        global: true,
        ..EventOptions::default()
    }
}

const SCRIPT_DESC: &str = "The plain-JS workflow script body (top-level await allowed; NO `export const meta` statement; end with `return <json-value>`).";
const ARGS_DESC: &str = "Optional JSON input exposed to the script as the `args` global (wrap a bare list as a field, e.g. {\"files\": [...]}).";

/// Registers the workflow tool and its usage-policy prompt section.
///
/// # Errors
///
/// Returns missing-service, prompt-registration, or tool-registration failures.
///
/// # Panics
///
/// Panics if the workflow engine starts a run without a calling agent.
#[allow(clippy::too_many_lines)]
pub fn apply(context: &Context, config: &Config) -> anyhow::Result<()> {
    let tool_name = config.tool_name.clone();
    let max_result_chars = usize::try_from(config.max_result_chars).unwrap_or(usize::MAX);
    let recorder = WorkflowRecorder::new(context);

    let prompt = context
        .get(SYSTEM_PROMPT)
        .ok_or_else(|| anyhow::anyhow!("tool-workflow requires systemPrompt"))?;
    prompt.section(
        context,
        PromptSection::new(
            format!("tool:{tool_name}"),
            115.0,
            PromptText::Static(format!(
                "Use the {tool_name} tool ONLY when the user explicitly asks for a workflow or for large multi-agent orchestration: you write a JavaScript script (the tool description documents the exact format) that fans work out across many subagents with phases and structured results. For one or two delegations, prefer plain subagent calls."
            )),
        ),
    )?;

    let tools = context
        .get(TOOLS)
        .ok_or_else(|| anyhow::anyhow!("tool-workflow requires tools"))?;
    let execute_ctx = context.clone();

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
        Arc::new(move |args: &WorkflowCallArgs, value: &WorkflowToolOutput| {
            Ok(vec![ContentBlock::Text {
                text: render_result(
                    &args.meta.name,
                    value.agents_started,
                    &value.result,
                    max_result_chars,
                ),
            }])
        }),
    );

    let definition = define_tool(
        DefineToolOptions::new(
            tool_name,
            DESCRIPTION,
            json!({
                "script": {"type": "string", "required": true, "description": SCRIPT_DESC},
                "meta": {
                    "type": "object", "additionalProperties": true, "required": true,
                    "description": "The workflow identity block (plain JSON — never code).",
                    "properties": {
                        "name": {"type": "string", "required": true, "description": "Short kebab-case workflow name."},
                        "description": {"type": "string", "required": true, "description": "One-line description of what the workflow does."},
                        "whenToUse": {"type": "string", "description": "Optional guidance on when this workflow applies."},
                        "phases": {"type": "array", "description": "Optional phase declarations matched by phase() calls.", "items": {"type": "object", "additionalProperties": true, "properties": {
                            "title": {"type": "string", "required": true, "description": "The phase title phase() calls match by exact string."},
                            "detail": {"type": "string", "description": "Optional one-line description of the phase."},
                            "provider": {"type": "string", "description": "Optional provider override this phase is expected to use."},
                            "model": {"type": "string", "description": "Optional model override this phase is expected to use."},
                        }}},
                    },
                },
                "args": {"type": "object", "additionalProperties": true, "description": ARGS_DESC},
            }),
            output,
            Arc::new(move |args: WorkflowCallArgs, exec| {
                let ctx = execute_ctx.clone();
                let recorder = Arc::clone(&recorder);
                Box::pin(async move {
                    let parent = exec.agent.clone().ok_or_else(|| {
                        anyhow::anyhow!("workflow tool requires a calling agent (exec.agent was undefined)")
                    })?;
                    let engine = ctx
                        .get(WORKFLOW_ENGINE)
                        .ok_or_else(|| anyhow::anyhow!("tool-workflow requires workflowEngine"))?
                        .engine();
                    let run = engine.start(WorkflowStartRequest {
                        script: args.script,
                        meta: args.meta,
                        args: args.args,
                        subagent_provider: None,
                        max_total_agents: None,
                        parent,
                        signal: Some(exec.signal()),
                    });
                    let records_run = exec.parent.is_none();
                    if records_run {
                        let session = exec.agent.as_ref().expect("checked above").session().clone();
                        recorder.start(&session, &run);
                    }
                    let bridge_run = Arc::clone(&run);
                    let signal = exec.signal();
                    tokio::spawn(async move {
                        signal.cancelled().await;
                        bridge_run.cancel(Some("parent step aborted"));
                    });

                    let result = run.result().await;
                    let error = stop_reason_error(&result);
                    if let Some(error) = error {
                        run.dispose().await;
                        recorder.abandon(run.id());
                        anyhow::bail!("{error}");
                    }
                    let output = WorkflowToolOutput {
                        run_id: run.id().clone(),
                        agents_started: result.agents_started,
                        result: result.value,
                    };
                    run.dispose().await;
                    if records_run {
                        recorder.finish(run.id(), WorkflowStopReason::Completed);
                    }
                    recorder.abandon(run.id());
                    Ok(output)
                })
            }),
        )
        .present_call(Arc::new(|args: &WorkflowCallArgs| {
            Some(ToolCallView::Generic(GenericCallView {
                title: format!("workflow: {}", args.meta.name),
                kind: None,
                raw_input: Some(json!(args.script)),
                content: None,
                locations: None,
            }))
        }))
        .present_result(Arc::new(
            |_args: &WorkflowCallArgs, _result: &ToolResult| {
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
