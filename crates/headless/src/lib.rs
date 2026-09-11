//! One-shot direct Agent driver for the headless `SeekDeep` profile.
//!
//! The runner creates one fresh persisted Agent, waits for the creation
//! lifecycle to settle idle, submits one ordinary user message, waits for
//! quiescence, flushes the session, folds only the owned durable interval, and
//! maps its final `turn/end` reason to process output.

use std::{io::Write as _, path::Path, sync::Arc};

use parking_lot::RwLock;
use seekdeep_agent::{
    AGENTS, Agent, AgentCancelCause, AgentOptions, AgentRegistry, CancelOptions,
    CreateAgentOptions, ModelSelection, ModelSelectionRef, install_model_selection,
};
use seekdeep_agent_default_model::AGENT_DEFAULT_MODEL;
use seekdeep_cmdline::APP_EXIT;
use seekdeep_cordis::{
    Context, FiberState, Plugin,
    fiber::{DisposeFuture, EffectHandle},
};
use seekdeep_core::{
    session::{JsonValue, SessionEvent, SessionId},
    session_store::{SESSIONS, SessionStore},
};
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use seekdeep_llm::{AbortSignal, ContentBlock, JsonString, MessageSource, UserMessage};
use seekdeep_loader::LOADER;
use seekdeep_schemastery::Schema;
use seekdeep_system_prompt::{SYSTEM_PROMPT, SystemPrompt};
use serde::{Deserialize, Serialize};
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
    pub text: JsonString,
    /// Last turn reason observed in the interval.
    pub reason: Option<JsonValue>,
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
        self.run_with_stop(task, &AbortSignal::default()).await
    }

    async fn run_with_stop(&self, task: &str, stopping: &AbortSignal) -> HeadlessRunResult {
        match self.run_checked(task, stopping).await {
            Ok((session_id, outcome)) => render_outcome(Some(session_id), &outcome),
            Err(error) => HeadlessRunResult {
                session_id: None,
                stdout: String::new(),
                stderr: format!("seekdeep: {error}\n"),
                exit_code: 1,
            },
        }
    }

    async fn run_checked(
        &self,
        task: &str,
        stopping: &AbortSignal,
    ) -> anyhow::Result<(SessionId, HeadlessOutcome)> {
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
        wait_for_run_idle(&handle.agent, stopping).await?;
        let first_seq = handle.agent.session().seq();
        if !stopping.is_aborted() {
            handle.agent.followup(UserMessage::new(
                vec![ContentBlock::Text { text: task.into() }],
                MessageSource::user(),
            ))?;
            wait_for_run_idle(&handle.agent, stopping).await?;
        }
        self.sessions.flush(handle.agent.session()).await?;
        let outcome = summarize(&handle.agent.session().events(), first_seq);
        Ok((session_id, outcome))
    }
}

async fn wait_for_run_idle(agent: &Agent, stopping: &AbortSignal) -> anyhow::Result<()> {
    let idle = agent.when_idle()?;
    tokio::select! {
        biased;
        () = stopping.cancelled() => {
            agent.cancel(AgentCancelCause::Disposed, CancelOptions::default())?;
            agent.when_idle()?.await
        }
        result = idle => result,
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

async fn start_admitted(
    started: tokio::sync::oneshot::Receiver<()>,
    stopping: &AbortSignal,
) -> bool {
    tokio::select! {
        biased;
        () = stopping.cancelled() => false,
        result = started => result.is_ok(),
    }
}

async fn run_plugin_task(
    settlement: Option<Arc<seekdeep_loader::LoaderSettlement>>,
    context: Context,
    task: String,
    output: Arc<dyn HeadlessOutput>,
    exit: Arc<seekdeep_cmdline::AppExit>,
    stopping: AbortSignal,
) {
    if let Some(settlement) = settlement {
        tokio::select! {
            biased;
            () = stopping.cancelled() => return,
            result = settlement.wait() => if result.is_err() { return; },
        }
    }
    let active = tokio::select! {
        biased;
        () = stopping.cancelled() => false,
        active = wait_until_active(context.fiber()) => active,
    };
    if !active {
        return;
    }
    let runner = (|| -> anyhow::Result<Option<HeadlessRunner>> {
        let (Some(agents), Some(sessions), Some(default_model)) = (
            context.get(AGENTS),
            context.get(SESSIONS),
            context.get(AGENT_DEFAULT_MODEL),
        ) else {
            return Ok(None);
        };
        let prompt = context
            .get(SYSTEM_PROMPT)
            .ok_or_else(|| anyhow::anyhow!("headless-runner requires systemPrompt"))?;
        let cwd = std::env::current_dir()?;
        Ok(Some(HeadlessRunner::new(
            agents,
            sessions,
            prompt,
            default_model.current_selection(),
            cwd.to_string_lossy(),
        )?))
    })();
    let result = match runner {
        Ok(Some(runner)) => runner.run_with_stop(&task, &stopping).await,
        Ok(None) => return,
        Err(error) => HeadlessRunResult {
            session_id: None,
            stdout: String::new(),
            stderr: format!("seekdeep: {error}\n"),
            exit_code: 1,
        },
    };
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
            let settlement = context.get(LOADER);
            let task_context = context.clone();
            let stopping = AbortSignal::default();
            let task_stopping = stopping.clone();
            let (start, started) = tokio::sync::oneshot::channel();
            let task = tokio::spawn(async move {
                if start_admitted(started, &task_stopping).await {
                    run_plugin_task(
                        settlement,
                        task_context,
                        config.task,
                        output,
                        exit,
                        task_stopping,
                    )
                    .await;
                }
            });
            let effect = EffectHandle::new("headless-runner task", move || -> DisposeFuture {
                Box::pin(async move {
                    // Let the owned Agent settle and flush before the runner writes its
                    // final result; aborting the task would discard signal-exit output.
                    stopping.abort();
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
    let mut text = JsonString::default();
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
            let parts = event
                .data
                .get_value("message")
                .and_then(|message| message.get_value("content"))
                .and_then(JsonValue::as_array)
                .into_iter()
                .flatten()
                .filter(|block| block.get_value("type").and_then(JsonValue::as_str) == Some("text"))
                .filter_map(|block| {
                    block
                        .get_value("text")
                        .and_then(|value| value.deserialize::<JsonString>().ok())
                })
                .collect::<Vec<_>>();
            let joined = JsonString::join(&parts, "");
            if !joined.is_empty() {
                text = joined;
            }
        }
        if event.event_type == "turn/end" {
            reason = event.data.get_value("reason").cloned();
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
        .and_then(|reason| reason.get_value("kind"))
        .and_then(JsonValue::as_str);
    let stderr = if kind == Some("error") {
        outcome.reason.as_ref().map_or_else(String::new, |reason| {
            let code = reason
                .pointer("/error/code")
                .and_then(|value| value.deserialize::<JsonString>().ok())
                .unwrap_or_else(|| "undefined".into());
            let message = reason
                .pointer("/error/message")
                .and_then(|value| value.deserialize::<JsonString>().ok())
                .unwrap_or_else(|| "undefined".into());
            let mut text = JsonString::from("seekdeep: ");
            text.push_utf16(code.utf16_units());
            text.push_str(": ");
            text.push_utf16(message.utf16_units());
            text.push_str("\n");
            process_text(&text)
        })
    } else {
        String::new()
    };
    HeadlessRunResult {
        session_id,
        stdout: format!("{}\n", process_text(&outcome.text)),
        stderr,
        exit_code: i32::from(kind != Some("completed")),
    }
}

fn process_text(text: &JsonString) -> String {
    // Node's UTF-8 stream encoder replaces lone surrogates at the process boundary.
    String::from_utf16_lossy(text.utf16_units())
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
    use futures::FutureExt as _;
    use seekdeep_cordis::Context;
    use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
    use serde_json::{Value, json};

    use super::*;

    #[test]
    fn stopping_before_start_releases_the_task_even_with_a_live_sender() {
        let (start, started) = tokio::sync::oneshot::channel();
        let stopping = AbortSignal::default();
        stopping.abort();
        assert_eq!(
            start_admitted(started, &stopping).now_or_never(),
            Some(false)
        );
        drop(start);
    }

    fn event(event_type: &str, seq: u64, data: impl Into<JsonValue>) -> SessionEvent {
        SessionEvent {
            event_type: event_type.to_owned(),
            seq,
            time: 0,
            data: data.into(),
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
                text: "final answer".into(),
                reason: Some(json!({"kind": "completed"}).into()),
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
        for reason in [None, Some(json!({"kind": "aborted"}).into())] {
            let result = render_outcome(
                None,
                &HeadlessOutcome {
                    text: JsonString::default(),
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
                text: JsonString::default(),
                reason: Some(
                    json!({
                        "kind": "error",
                        "error": {"code": "SERVER", "message": "provider unavailable"}
                    })
                    .into(),
                ),
            },
        );
        assert_eq!(result.exit_code, 1);
        assert_eq!(result.stdout, "\n");
        assert_eq!(result.stderr, "seekdeep: SERVER: provider unavailable\n");
    }

    #[test]
    fn summarizes_utf16_text_and_opaque_reason_without_narrowing() {
        let message = JsonValue::parse(r#"{"message":{"content":[{"type":"text","text":"\ud800"},{"type":"future","\udfff":"ignored"},{"type":"text","text":"\udc00\udfff"}],"opaque":{"\ud800":"\udfff"}}}"#.to_owned()).unwrap();
        let end = JsonValue::parse(r#"{"reason":{"kind":"error","error":{"code":"E\ud800","message":"failed \udfff"},"opaque":{"\ud800":"\udfff"}}}"#.to_owned()).unwrap();
        let events = [
            event("turn/start", 1, json!({})),
            event("assistant/message", 2, message),
            assistant(3, &json!([{"type":"text","text":""}])),
            event("turn/end", 4, end.clone()),
        ];
        let outcome = summarize(&events, 1);
        assert_eq!(outcome.text.to_utf16(), [0xd800, 0xdc00, 0xdfff]);
        assert_eq!(outcome.reason.as_ref(), end.get_value("reason"));
        assert_eq!(
            outcome
                .reason
                .as_ref()
                .unwrap()
                .get_value("opaque")
                .unwrap()
                .as_raw(),
            r#"{"\ud800":"\udfff"}"#
        );
        let process = render_outcome(None, &outcome);
        assert_eq!(process.stdout, "𐀀�\n");
        assert_eq!(process.stderr, "seekdeep: E�: failed �\n");
        assert_eq!(process.exit_code, 1);
    }

    #[test]
    fn process_output_bytes_match_node_utf8_stream_encoding() {
        let output = std::process::Command::new("node")
            .args(["-e", r"process.stdout.write('\ud800\udc00\udfff\n'); process.stderr.write('seekdeep: E\ud800: failed \udfff\n')"])
            .output()
            .expect("Node UTF-8 stream oracle");
        assert!(output.status.success());
        let outcome = HeadlessOutcome {
            text: JsonString::from_utf16(&[0xd800, 0xdc00, 0xdfff]),
            reason: Some(
                JsonValue::parse(
                    r#"{"kind":"error","error":{"code":"E\ud800","message":"failed \udfff"}}"#
                        .to_owned(),
                )
                .unwrap(),
            ),
        };
        let actual = render_outcome(None, &outcome);
        assert_eq!(actual.stdout.as_bytes(), output.stdout);
        assert_eq!(actual.stderr.as_bytes(), output.stderr);
        assert_eq!(outcome.text.to_utf16(), [0xd800, 0xdc00, 0xdfff]);
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
