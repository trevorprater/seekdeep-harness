//! One-shot direct Agent driver for the headless `SeekDeep` profile.
//!
//! The runner creates one fresh persisted Agent, waits for the creation
//! lifecycle to settle idle, submits one ordinary user message, waits for
//! quiescence, flushes the session, folds only the owned durable interval, and
//! maps its final `turn/end` reason to process output.

use std::{io::Write as _, path::Path, sync::Arc};

use parking_lot::RwLock;
use seekdeep_agent::{
    AGENTS, AgentOptions, AgentRegistry, CreateAgentOptions, ModelSelection, ModelSelectionRef,
    install_model_selection,
};
use seekdeep_agent_default_model::AGENT_DEFAULT_MODEL;
use seekdeep_cmdline::APP_EXIT;
use seekdeep_cordis::{
    FiberState, Plugin,
    fiber::{DisposeFuture, EffectHandle},
};
use seekdeep_core::{
    session::{SessionEvent, SessionId},
    session_store::{SESSIONS, SessionStore},
};
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use seekdeep_llm::{ContentBlock, MessageSource, UserMessage};
use seekdeep_loader::LOADER;
use seekdeep_schemastery::Schema;
use seekdeep_system_prompt::{SYSTEM_PROMPT, SystemPrompt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

pub mod startup;

/// Stable source-compatible plugin name.
pub const NAME: &str = "headless-runner";
/// Services the source runner requires before activation.
pub const INJECT: &[&str] = &["agentDefaultModel", "agents", "sessions"];
/// Package identity reserved in the invariant registry.
pub const INVARIANT_NAME: &str = "seekdeep-headless";
/// Cordis invariant companion plugin name.
pub const INVARIANT_PLUGIN_NAME: &str = "headless-invariant";
/// Service required before the invariant companion can register.
pub const INVARIANT_INJECT: &[&str] = &["invariants"];

/// Loader-facing one-shot runner configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// Prompt text for the single run.
    pub task: String,
}

/// Source-compatible Loader schema for the required one-shot task.
#[must_use]
pub fn config_schema() -> Schema {
    Schema::object([("task", Schema::string().required())])
}

/// Process output boundary used by the Loader-facing runner plugin.
pub trait HeadlessOutput: Send + Sync + 'static {
    /// Writes the complete standard-output payload.
    ///
    /// # Errors
    ///
    /// Returns the backing stream failure.
    fn write_stdout(&self, text: &str) -> anyhow::Result<()>;

    /// Writes the complete standard-error payload.
    ///
    /// # Errors
    ///
    /// Returns the backing stream failure.
    fn write_stderr(&self, text: &str) -> anyhow::Result<()>;
}

#[derive(Debug)]
struct ProcessOutput;

impl HeadlessOutput for ProcessOutput {
    fn write_stdout(&self, text: &str) -> anyhow::Result<()> {
        std::io::stdout().lock().write_all(text.as_bytes())?;
        Ok(())
    }

    fn write_stderr(&self, text: &str) -> anyhow::Result<()> {
        std::io::stderr().lock().write_all(text.as_bytes())?;
        Ok(())
    }
}

/// Process-facing result of one headless invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeadlessRunResult {
    /// Fresh durable identity, when Agent creation succeeded.
    pub session_id: Option<SessionId>,
    /// Complete standard-output payload.
    pub stdout: String,
    /// Complete standard-error payload.
    pub stderr: String,
    /// Requested process exit status.
    pub exit_code: i32,
}

/// Detached aggregate of one owned idle-to-idle event interval.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HeadlessOutcome {
    /// Last non-empty assistant text after the first owned turn starts.
    pub text: String,
    /// Last turn reason observed in the interval.
    pub reason: Option<Value>,
}

/// Concrete dependencies shared by one or more direct one-shot runs.
#[derive(Clone)]
pub struct HeadlessRunner {
    agents: Arc<AgentRegistry>,
    sessions: Arc<SessionStore>,
    system_prompt: Arc<SystemPrompt>,
    selection: ModelSelection,
    cwd: String,
}

impl std::fmt::Debug for HeadlessRunner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HeadlessRunner")
            .field("selection", &self.selection)
            .field("cwd", &self.cwd)
            .finish_non_exhaustive()
    }
}

impl HeadlessRunner {
    /// Builds a runner over a fully assembled core tree.
    ///
    /// # Errors
    ///
    /// Rejects a non-absolute working directory. The source obtains this value
    /// from `process.cwd()`, which is always absolute.
    pub fn new(
        agents: Arc<AgentRegistry>,
        sessions: Arc<SessionStore>,
        system_prompt: Arc<SystemPrompt>,
        selection: ModelSelection,
        cwd: impl Into<String>,
    ) -> anyhow::Result<Self> {
        let cwd = cwd.into();
        anyhow::ensure!(
            Path::new(&cwd).is_absolute(),
            "headless runner cwd must be absolute, got {cwd:?}"
        );
        Ok(Self {
            agents,
            sessions,
            system_prompt,
            selection,
            cwd,
        })
    }

    /// Runs one task and converts both durable and unexpected failures to the
    /// exact process-facing output contract.
    pub async fn run(&self, task: &str) -> HeadlessRunResult {
        match self.run_checked(task).await {
            Ok((session_id, outcome)) => render_outcome(Some(session_id), &outcome),
            Err(error) => HeadlessRunResult {
                session_id: None,
                stdout: String::new(),
                stderr: format!("seekdeep: {error}\n"),
                exit_code: 1,
            },
        }
    }

    async fn run_checked(&self, task: &str) -> anyhow::Result<(SessionId, HeadlessOutcome)> {
        let session_id = SessionId::new(format!("session-{}", Uuid::new_v4()));
        let mut options = CreateAgentOptions::new(session_id.clone());
        options.meta.cwd = Some(self.cwd.clone());
        options.agent_options = AgentOptions {
            provider: Some(self.selection.provider.clone()),
            model: Some(self.selection.model.clone()),
            max_tokens: None,
            subagent_depth: None,
        };
        let prompt = self.system_prompt.clone();
        let selection = self.selection.clone();
        let cwd = self.cwd.clone();
        options.setup = Some(Arc::new(move |agent_context| {
            let prompt = prompt.clone();
            let selection = selection.clone();
            let cwd = cwd.clone();
            Box::pin(async move {
                install_model_selection(
                    &agent_context,
                    &prompt,
                    Arc::new(RwLock::new(ModelSelectionRef {
                        current: Some(selection),
                        assembled: None,
                    })),
                )?;
                prompt.variable(
                    &agent_context,
                    "cwd",
                    Arc::new(move |_| Ok(Some(cwd.clone()))),
                )?;
                Ok(None)
            })
        }));

        let handle = self.agents.create(options).await?;
        handle.agent.when_idle()?.await;
        let first_seq = handle.agent.session().seq();
        handle.agent.followup(UserMessage::new(
            vec![ContentBlock::Text {
                text: task.to_owned(),
            }],
            MessageSource::user(),
        ))?;
        handle.agent.when_idle()?.await;
        self.sessions.flush(handle.agent.session()).await?;
        let outcome = summarize(&handle.agent.session().events(), first_seq);
        Ok((session_id, outcome))
    }
}

async fn wait_until_active(owner: &Arc<seekdeep_cordis::Fiber>) -> bool {
    loop {
        match owner.state() {
            FiberState::Active => return true,
            FiberState::Pending | FiberState::Loading => tokio::task::yield_now().await,
            FiberState::Failed | FiberState::Unloading | FiberState::Disposed => return false,
        }
    }
}

async fn run_plugin_task(
    settlement: Option<Arc<seekdeep_loader::LoaderSettlement>>,
    owner: Arc<seekdeep_cordis::Fiber>,
    runner: HeadlessRunner,
    task: String,
    output: Arc<dyn HeadlessOutput>,
    exit: Arc<seekdeep_cmdline::AppExit>,
) {
    if let Some(settlement) = settlement
        && settlement.wait().await.is_err()
    {
        return;
    }
    if !wait_until_active(&owner).await {
        return;
    }
    let result = runner.run(&task).await;
    let operation = (|| -> anyhow::Result<()> {
        output.write_stdout(&result.stdout)?;
        output.write_stderr(&result.stderr)?;
        exit.request(result.exit_code)
    })();
    if let Err(error) = operation {
        let _ = output.write_stderr(&format!("seekdeep: {error}\n"));
        let _ = exit.request(1);
    }
}

/// Builds the Loader-compatible one-shot runner using process output streams.
#[must_use]
pub fn plugin() -> Plugin {
    plugin_with_output(Arc::new(ProcessOutput))
}

/// Builds the Loader-compatible runner with an injected output boundary.
#[must_use]
pub fn plugin_with_output(output: Arc<dyn HeadlessOutput>) -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, value| {
        let output = output.clone();
        Box::pin(async move {
            let config: Config = serde_json::from_value(value)?;
            let exit = context.get(APP_EXIT).ok_or_else(|| {
                anyhow::anyhow!(
                    "headless-runner: the launcher must provide ctx.appExit before the tree mounts"
                )
            })?;
            let agents = context
                .get(AGENTS)
                .ok_or_else(|| anyhow::anyhow!("headless-runner requires agents"))?;
            let sessions = context
                .get(SESSIONS)
                .ok_or_else(|| anyhow::anyhow!("headless-runner requires sessions"))?;
            let selection = context
                .get(AGENT_DEFAULT_MODEL)
                .ok_or_else(|| anyhow::anyhow!("headless-runner requires agentDefaultModel"))?
                .current_selection();
            let prompt = context
                .get(SYSTEM_PROMPT)
                .ok_or_else(|| anyhow::anyhow!("headless-runner requires systemPrompt"))?;
            let cwd = std::env::current_dir()?;
            let runner =
                HeadlessRunner::new(agents, sessions, prompt, selection, cwd.to_string_lossy())?;
            let settlement = context.get(LOADER);
            let owner = context.fiber().clone();
            let (start, started) = tokio::sync::oneshot::channel();
            let task = tokio::spawn(async move {
                if started.await.is_ok() {
                    run_plugin_task(settlement, owner, runner, config.task, output, exit).await;
                }
            });
            let effect = EffectHandle::new("headless-runner task", move || -> DisposeFuture {
                Box::pin(async move {
                    task.abort();
                    match task.await {
                        Ok(()) => Ok(()),
                        Err(error) if error.is_cancelled() => Ok(()),
                        Err(error) => Err(error.into()),
                    }
                })
            });
            if let Err(error) = context.own(effect.clone()) {
                let _ = effect.dispose().await;
                return Err(error.into());
            }
            let _ = start.send(());
            Ok(())
        })
    })
    .with_config_validator(|value| {
        config_schema()
            .resolve(value)
            .map_err(|error| anyhow::anyhow!("{error}"))
    })
}

/// Folds the final assistant text and turn reason from one owned event interval.
#[must_use]
pub fn summarize(events: &[SessionEvent], first_seq: u64) -> HeadlessOutcome {
    let mut started = false;
    let mut text = String::new();
    let mut reason = None;
    for event in events {
        if event.seq < first_seq {
            continue;
        }
        if event.event_type == "turn/start" {
            started = true;
            continue;
        }
        if !started {
            continue;
        }
        if event.event_type == "assistant/message" {
            let joined = event
                .data
                .get("message")
                .and_then(|message| message.get("content"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<String>();
            if !joined.is_empty() {
                text = joined;
            }
        }
        if event.event_type == "turn/end" {
            reason = event.data.get("reason").cloned();
        }
    }
    HeadlessOutcome { text, reason }
}

/// Maps one durable aggregate to stdout, stderr, and exit status.
#[must_use]
pub fn render_outcome(
    session_id: Option<SessionId>,
    outcome: &HeadlessOutcome,
) -> HeadlessRunResult {
    let kind = outcome
        .reason
        .as_ref()
        .and_then(|reason| reason.get("kind"))
        .and_then(Value::as_str);
    let stderr = if kind == Some("error") {
        outcome.reason.as_ref().map_or_else(String::new, |reason| {
            let code = reason
                .pointer("/error/code")
                .and_then(Value::as_str)
                .unwrap_or("undefined");
            let message = reason
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("undefined");
            format!("seekdeep: {code}: {message}\n")
        })
    } else {
        String::new()
    };
    HeadlessRunResult {
        session_id,
        stdout: format!("{}\n", outcome.text),
        stderr,
        exit_code: i32::from(kind != Some("completed")),
    }
}

/// Registers the package's intentionally empty in-tree invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant-registry failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(INVARIANT_NAME, InvariantInstaller::noop())
}

#[cfg(test)]
mod tests {
    use seekdeep_cordis::Context;
    use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
    use serde_json::json;

    use super::*;

    fn event(event_type: &str, seq: u64, data: Value) -> SessionEvent {
        SessionEvent {
            event_type: event_type.to_owned(),
            seq,
            time: 0,
            data,
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        }
    }

    fn assistant(seq: u64, blocks: &Value) -> SessionEvent {
        event(
            "assistant/message",
            seq,
            json!({"message": {"content": blocks}}),
        )
    }

    #[test]
    fn aggregates_only_the_owned_interval_and_keeps_last_nonempty_text() {
        let events = vec![
            event("turn/start", 0, json!({"turn": 0})),
            assistant(1, &json!([{"type": "text", "text": "pre-task noise"}])),
            event(
                "turn/end",
                2,
                json!({"turn": 0, "reason": {"kind": "completed"}}),
            ),
            event("agent/inbox/spliced", 3, json!({})),
            event("turn/start", 4, json!({"turn": 1})),
            assistant(5, &json!([{"type": "text", "text": ""}])),
            event(
                "turn/end",
                6,
                json!({"turn": 1, "reason": {"kind": "completed"}}),
            ),
            event("turn/start", 7, json!({"turn": 2})),
            assistant(
                8,
                &json!([
                    {"type": "text", "text": "final "},
                    {"type": "toolCall", "name": "ignored"},
                    {"type": "text", "text": "answer"}
                ]),
            ),
            event(
                "turn/end",
                9,
                json!({"turn": 2, "reason": {"kind": "completed"}}),
            ),
        ];
        assert_eq!(
            summarize(&events, 3),
            HeadlessOutcome {
                text: "final answer".to_owned(),
                reason: Some(json!({"kind": "completed"})),
            }
        );
        assert_eq!(
            render_outcome(None, &summarize(&events, 3)),
            HeadlessRunResult {
                session_id: None,
                stdout: "final answer\n".to_owned(),
                stderr: String::new(),
                exit_code: 0,
            }
        );
    }

    #[test]
    fn maps_absent_aborted_and_error_reasons_exactly() {
        for reason in [None, Some(json!({"kind": "aborted"}))] {
            let result = render_outcome(
                None,
                &HeadlessOutcome {
                    text: String::new(),
                    reason,
                },
            );
            assert_eq!(result.exit_code, 1);
            assert_eq!(result.stdout, "\n");
            assert!(result.stderr.is_empty());
        }
        let result = render_outcome(
            None,
            &HeadlessOutcome {
                text: String::new(),
                reason: Some(json!({
                    "kind": "error",
                    "error": {"code": "SERVER", "message": "provider unavailable"}
                })),
            },
        );
        assert_eq!(result.exit_code, 1);
        assert_eq!(result.stdout, "\n");
        assert_eq!(result.stderr, "seekdeep: SERVER: provider unavailable\n");
    }

    #[tokio::test]
    async fn invariant_companion_reserves_the_exact_package_and_unwinds() {
        assert_eq!(INVARIANT_PLUGIN_NAME, "headless-invariant");
        assert_eq!(INVARIANT_INJECT, ["invariants"]);
        let context = Context::new();
        let registry = InvariantRegistry::install(&context, &InvariantConfig::default()).unwrap();
        let registration = register_invariant(&registry).unwrap();
        registration.await_ready().await.unwrap();
        assert!(registry.is_registered(INVARIANT_NAME));
        registration.dispose().await.unwrap();
        assert!(!registry.is_registered(INVARIANT_NAME));
    }
}
