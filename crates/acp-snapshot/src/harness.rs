//! Deterministic ACP scenario scripts, workspace setup, durable waits, harvest, and cleanup.

use std::{
    collections::{BTreeMap, VecDeque},
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use futures::future::BoxFuture;
use parking_lot::Mutex;
use seekdeep_acp::{AcpPermissionHandler, AcpSessionId};
use seekdeep_loader_smoke::ExampleMode;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};
use tokio::time::Instant;
use walkdir::WalkDir;

use crate::{
    AcpTestLaunchOptions, AcpTestSignal, AcpUpdatePredicate, AgentUnderTest, LaunchedAcpTestAgent,
    launch_acp_test_agent,
};

const DEFAULT_WAIT_TIMEOUT: Duration = Duration::from_secs(10);
const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Optional file marker used before a prompt or standalone cancellation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitForFile {
    /// Cwd-relative marker path.
    pub path: PathBuf,
    /// Optional timeout override in milliseconds.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// One deterministic scenario input step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum InputStep {
    /// ACP version and capability handshake.
    Initialize,
    /// Create a fresh Session and retain its returned identity.
    NewSession,
    /// Require Session creation to fail.
    NewSessionExpectError {
        /// Optional widened workspace scope sent verbatim.
        #[serde(default, rename = "additionalDirectories")]
        additional_directories: Option<Vec<String>>,
    },
    /// Run one successful text prompt.
    Prompt {
        /// User text.
        text: String,
    },
    /// Run a prompt and wait for an exact later agent message.
    PromptAndWaitForAgentMessage {
        /// User text.
        text: String,
        /// Exact agent text chunk required before the step settles.
        #[serde(rename = "waitForText")]
        wait_for_text: String,
    },
    /// Require one prompt RPC to fail.
    PromptExpectError {
        /// User text.
        text: String,
    },
    /// Start a prompt, wait for readiness, cancel, and await settlement.
    PromptAndCancel {
        /// User text.
        text: String,
        /// Optional filesystem readiness marker; otherwise the durable turn start is used.
        #[serde(default, rename = "waitForFile")]
        wait_for_file: Option<WaitForFile>,
    },
    /// Wait for one cwd-relative file.
    WaitForFile {
        /// Cwd-relative marker path.
        path: PathBuf,
        /// Optional timeout override in milliseconds.
        #[serde(default, rename = "timeoutMs")]
        timeout_ms: Option<u64>,
    },
    /// Wait for an open durable turn.
    WaitForTurnStart {
        /// Optional minimum accepted turn number.
        #[serde(default, rename = "minimumTurn")]
        minimum_turn: Option<u64>,
        /// Optional timeout override in milliseconds.
        #[serde(default, rename = "timeoutMs")]
        timeout_ms: Option<u64>,
    },
    /// Wait for the latest durable turn to close.
    WaitForTurnEnd {
        /// Optional timeout override in milliseconds.
        #[serde(default, rename = "timeoutMs")]
        timeout_ms: Option<u64>,
    },
    /// Wait for one harvested child to close its own work turn.
    WaitForSubagentTurnEnd {
        /// One-based child fixture index.
        #[serde(default)]
        child: Option<usize>,
        /// Optional minimum accepted turn number.
        #[serde(default, rename = "minimumTurn")]
        minimum_turn: Option<u64>,
        /// Optional timeout override in milliseconds.
        #[serde(default, rename = "timeoutMs")]
        timeout_ms: Option<u64>,
    },
    /// Wait for a durable goal phase.
    WaitForGoalPhase {
        /// Required phase.
        phase: GoalPhase,
        /// Optional timeout override in milliseconds.
        #[serde(default, rename = "timeoutMs")]
        timeout_ms: Option<u64>,
    },
    /// Wait for inserted durable inbox text.
    WaitForInboxMessage {
        /// Required substring.
        text: String,
        /// Optional timeout override in milliseconds.
        #[serde(default, rename = "timeoutMs")]
        timeout_ms: Option<u64>,
    },
    /// Wait for a title record after the latest turn end.
    WaitForTitleAfterTurnEnd {
        /// Optional timeout override in milliseconds.
        #[serde(default, rename = "timeoutMs")]
        timeout_ms: Option<u64>,
    },
    /// Wait for one event kind after the latest turn end.
    WaitForEventAfterTurnEnd {
        /// Durable event kind.
        #[serde(rename = "type")]
        event_type: String,
        /// Optional timeout override in milliseconds.
        #[serde(default, rename = "timeoutMs")]
        timeout_ms: Option<u64>,
    },
    /// Send best-effort cancellation, optionally after a file marker.
    Cancel {
        /// Optional filesystem readiness marker.
        #[serde(default, rename = "waitForFile")]
        wait_for_file: Option<WaitForFile>,
    },
}

/// Durable goal phases accepted by the scenario script.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GoalPhase {
    /// Goal is running.
    Active,
    /// Goal is explicitly paused.
    Paused,
    /// Goal needs external progress.
    Blocked,
    /// Goal is complete.
    Complete,
}

impl GoalPhase {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Blocked => "blocked",
            Self::Complete => "complete",
        }
    }
}

/// Stable ACP permission-option category selected by a committed input script.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionAnswerKind {
    /// Allow only this call.
    AllowOnce,
    /// Allow this and future equivalent calls.
    AllowAlways,
    /// Reject only this call.
    RejectOnce,
    /// Reject this and future equivalent calls.
    RejectAlways,
}

impl PermissionAnswerKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::AllowOnce => "allow_once",
            Self::AllowAlways => "allow_always",
            Self::RejectOnce => "reject_once",
            Self::RejectAlways => "reject_always",
        }
    }
}

/// One queued answer to an ACP permission request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionAnswer {
    /// Offered option kind to select.
    pub kind: PermissionAnswerKind,
}

/// Ordered deterministic scenario input.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputScript {
    /// Steps interpreted in order.
    pub steps: Vec<InputStep>,
    /// Permission answers consumed FIFO.
    #[serde(default)]
    pub permission_answers: Vec<PermissionAnswer>,
}

/// Snapshot model mode passed to the child composition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotRunMode {
    /// Keyless replay.
    Replay,
    /// Real-provider recording.
    Record,
}

impl SnapshotRunMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Replay => "replay",
            Self::Record => "record",
        }
    }
}

/// Platform-dependent stable spill-root choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotPlatform {
    /// Windows drive resolution adds a prefix, so the root is two characters shorter.
    Windows,
    /// POSIX-style root.
    Other,
}

impl SnapshotPlatform {
    const fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Other
        }
    }
}

/// Final generated-workspace setup hook.
pub type PrepareWorkspace =
    Arc<dyn Fn(PathBuf) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;

/// Inputs controlling one whole scenario run.
#[derive(Clone)]
pub struct RunOptions {
    /// Agent composition to boot.
    pub agent: AgentUnderTest,
    /// Replay or record mode.
    pub mode: SnapshotRunMode,
    /// Scenario-specific child environment.
    pub environment: BTreeMap<OsString, OsString>,
    /// Primary recorded Session fixture.
    pub fixture_file: PathBuf,
    /// Optional replay override.
    pub override_file: Option<PathBuf>,
    /// Recorded child Session fixtures in replay order.
    pub child_files: Vec<PathBuf>,
    /// Optional committed workspace seed directory.
    pub workspace_dir: Option<PathBuf>,
    /// Optional generated-workspace mutation after committed seeds copy.
    pub prepare_workspace: Option<PrepareWorkspace>,
    /// Parent for the generated workspace; the harness removes only its child.
    pub workspace_parent: Option<PathBuf>,
    /// Optional alternate live Cordis config.
    pub config_path: Option<PathBuf>,
    /// Explicit compiled development/publish artifact mode.
    pub artifact_mode: Option<ExampleMode>,
}

impl std::fmt::Debug for RunOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RunOptions")
            .field("agent", &self.agent)
            .field("mode", &self.mode)
            .field("environment", &self.environment)
            .field("fixture_file", &self.fixture_file)
            .field("override_file", &self.override_file)
            .field("child_files", &self.child_files)
            .field("workspace_dir", &self.workspace_dir)
            .field(
                "prepare_workspace",
                &self.prepare_workspace.as_ref().map(|_| "<hook>"),
            )
            .field("workspace_parent", &self.workspace_parent)
            .field("config_path", &self.config_path)
            .field("artifact_mode", &self.artifact_mode)
            .finish()
    }
}

/// One harvested raw Session log and its ordering header facts.
#[derive(Clone, Debug, PartialEq)]
pub struct HarvestedLog {
    /// Header Session id or the empty fallback.
    pub id: String,
    /// Header creation time used to order children.
    pub created_at: f64,
    /// Parent Session id for a child.
    pub parent_session: Option<String>,
    /// Complete file bytes decoded as UTF-8.
    pub content: String,
}

/// Whole scenario output before normalization.
#[derive(Clone, Debug, PartialEq)]
pub struct RunResult {
    /// Complete ACP stdout transcript.
    pub raw_stdout: String,
    /// Complete diagnostic stderr.
    pub stderr: String,
    /// ACP-issued Session id when a Session was created.
    pub session_id: Option<AcpSessionId>,
    /// Generated workspace.
    pub cwd: PathBuf,
    /// Filesystem-resolved spellings of the generated workspace.
    pub cwd_aliases: Vec<PathBuf>,
    /// Primary-first durable Session logs.
    pub session_logs: Vec<HarvestedLog>,
}

/// Derives one fixed-length scenario-owned spill root.
#[must_use]
pub fn snapshot_spill_root(fixture_file: &Path, platform: SnapshotPlatform) -> PathBuf {
    let scenario = fixture_file
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let digest = format!("{:x}", Sha256::digest(scenario.as_bytes()));
    let root = match platform {
        SnapshotPlatform::Windows => Path::new("/t"),
        SnapshotPlatform::Other => Path::new("/tmp"),
    };
    root.join(format!("seekdeep-acp-snap-{}", &digest[..9]))
}

/// Runs a deterministic input script against one freshly launched ACP subprocess.
///
/// # Errors
///
/// Returns workspace, launch, protocol, scripted-permission, persistence-wait, harvest, or cleanup
/// failures. Scenario stderr is attached, and cleanup failures are never hidden by a scenario error.
pub async fn run_scenario(input: &InputScript, options: RunOptions) -> anyhow::Result<RunResult> {
    let workspace_parent = options
        .workspace_parent
        .clone()
        .unwrap_or_else(std::env::temp_dir);
    let cwd = tempfile::Builder::new()
        .prefix("acp-snap-cwd-")
        .tempdir_in(&workspace_parent)?
        .keep();
    let cwd_aliases = canonical_cwd_aliases(&cwd);
    let sessions_root = tempfile::Builder::new()
        .prefix("acp-snap-sessions-")
        .tempdir()?
        .keep();
    let spill_root = snapshot_spill_root(&options.fixture_file, SnapshotPlatform::current());
    let mut launched = None;
    let outcome = run_scenario_body(
        input,
        &options,
        &cwd,
        &cwd_aliases,
        &sessions_root,
        &spill_root,
        &mut launched,
    )
    .await
    .map_err(|error| attach_stderr(error, launched.as_ref()));

    let mut cleanup_failures = Vec::new();
    if let Some(active) = &launched
        && let Err(error) = active.close(Some(AcpTestSignal::Kill)).await
    {
        cleanup_failures.push(error);
    }
    for path in [&cwd, &sessions_root, &spill_root] {
        if let Err(error) = remove_owned_path(path).await {
            cleanup_failures.push(error);
        }
    }
    finish_with_cleanup(outcome, &cleanup_failures)
}

async fn run_scenario_body(
    input: &InputScript,
    options: &RunOptions,
    cwd: &Path,
    cwd_aliases: &[PathBuf],
    sessions_root: &Path,
    spill_root: &Path,
    launched: &mut Option<LaunchedAcpTestAgent>,
) -> anyhow::Result<RunResult> {
    if let Some(workspace) = &options.workspace_dir
        && workspace.exists()
    {
        copy_workspace(workspace, cwd).await?;
    }
    if let Some(prepare) = &options.prepare_workspace {
        prepare(cwd.to_owned()).await?;
    }
    let (permission, script_error) = scripted_permission_handler(&input.permission_answers);
    let environment = scenario_environment(options, cwd, sessions_root, spill_root)?;
    let active = launch_acp_test_agent(AcpTestLaunchOptions {
        agent: options.agent.clone(),
        cwd: cwd.to_owned(),
        config_path: options.config_path.clone(),
        mode: options.artifact_mode,
        environment,
        request_permission: Some(permission),
    })?;
    *launched = Some(active.clone());
    let mut session_id = None;
    for step in &input.steps {
        run_step(&active, step, cwd, sessions_root, &mut session_id).await?;
        if let Some(error) = script_error.lock().take() {
            anyhow::bail!(error);
        }
    }
    active.close(None).await?;
    let session_logs = harvest_session_logs(sessions_root).await?;
    Ok(RunResult {
        raw_stdout: active.raw_stdout(),
        stderr: active.stderr(),
        session_id,
        cwd: cwd.to_owned(),
        cwd_aliases: cwd_aliases.to_vec(),
        session_logs,
    })
}

fn scenario_environment(
    options: &RunOptions,
    cwd: &Path,
    sessions_root: &Path,
    spill_root: &Path,
) -> anyhow::Result<BTreeMap<OsString, OsString>> {
    let mut environment = options.environment.clone();
    environment.insert("SEEKDEEP_SNAPSHOT".into(), options.mode.as_str().into());
    environment.insert(
        "SEEKDEEP_SNAPSHOT_FILE".into(),
        options.fixture_file.clone().into_os_string(),
    );
    environment.insert(
        "SEEKDEEP_SNAPSHOT_SESSIONS_ROOT".into(),
        sessions_root.to_owned().into_os_string(),
    );
    environment.insert(
        "SEEKDEEP_SNAPSHOT_SPILL_ROOT".into(),
        spill_root.to_owned().into_os_string(),
    );
    environment.insert(
        "SEEKDEEP_HOME".into(),
        cwd.join(".seekdeep").into_os_string(),
    );
    environment.insert(
        "SEEKDEEP_AGENTS_HOME".into(),
        cwd.join(".agents").into_os_string(),
    );
    if let Some(override_file) = &options.override_file {
        environment.insert(
            "SEEKDEEP_SNAPSHOT_OVERRIDE".into(),
            override_file.clone().into_os_string(),
        );
    }
    if !options.child_files.is_empty() {
        environment.insert(
            "SEEKDEEP_SNAPSHOT_CHILD_FILES".into(),
            std::env::join_paths(&options.child_files)?,
        );
    }
    Ok(environment)
}

fn scripted_permission_handler(
    answers: &[PermissionAnswer],
) -> (AcpPermissionHandler, Arc<Mutex<Option<String>>>) {
    let queue = Arc::new(Mutex::new(VecDeque::from(answers.to_vec())));
    let script_error = Arc::new(Mutex::new(None::<String>));
    let handler_queue = queue;
    let handler_error = script_error.clone();
    let handler = Arc::new(move |params: Map<String, Value>| {
        let answer = handler_queue.lock().pop_front();
        let handler_error = handler_error.clone();
        Box::pin(async move {
            let Some(answer) = answer else {
                return Ok(json!({"outcome":{"outcome":"cancelled"}}));
            };
            let options = params
                .get("options")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let option = options.iter().find(|option| {
                option.get("kind").and_then(Value::as_str) == Some(answer.kind.as_str())
            });
            let Some(option_id) = option
                .and_then(|option| option.get("optionId"))
                .and_then(Value::as_str)
            else {
                let offered = options
                    .iter()
                    .filter_map(|option| option.get("kind").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join(", ");
                *handler_error.lock() = Some(format!(
                    "snapshot-harness: scripted permission answer {} not among the offered options [{}]",
                    answer.kind.as_str(),
                    offered
                ));
                return Ok(json!({"outcome":{"outcome":"cancelled"}}));
            };
            Ok(json!({"outcome":{"outcome":"selected","optionId":option_id}}))
        }) as BoxFuture<'static, anyhow::Result<Value>>
    }) as AcpPermissionHandler;
    (handler, script_error)
}

#[allow(clippy::too_many_lines)]
async fn run_step(
    active: &LaunchedAcpTestAgent,
    step: &InputStep,
    cwd: &Path,
    sessions_root: &Path,
    session_id: &mut Option<AcpSessionId>,
) -> anyhow::Result<()> {
    let client = active.client();
    match step {
        InputStep::Initialize => {
            client.initialize().await?;
        }
        InputStep::NewSession => {
            *session_id = Some(client.new_session(&cwd.to_string_lossy()).await?);
        }
        InputStep::NewSessionExpectError {
            additional_directories,
        } => {
            if client
                .new_session_with_additional_directories(
                    &cwd.to_string_lossy(),
                    additional_directories.as_deref(),
                )
                .await
                .is_ok()
            {
                anyhow::bail!(
                    "snapshot-harness: expected session/new to be rejected but it succeeded"
                );
            }
        }
        InputStep::Prompt { text } => {
            let session = require_session(session_id.as_ref(), "prompt")?;
            client.prompt(session, text_prompt(text)).await?;
        }
        InputStep::PromptAndWaitForAgentMessage {
            text,
            wait_for_text,
        } => {
            let session = require_session(session_id.as_ref(), "promptAndWaitForAgentMessage")?;
            let expected = wait_for_text.clone();
            let predicate: AcpUpdatePredicate = Box::new(move |update: &Value| {
                Ok(agent_message_text(update).is_some_and(|text| text == expected))
            });
            let update_done = active.wait_for_update(predicate);
            client.prompt(session, text_prompt(text)).await?;
            update_done.await?;
        }
        InputStep::PromptExpectError { text } => {
            let session = require_session(session_id.as_ref(), "promptExpectError")?;
            if client.prompt(session, text_prompt(text)).await.is_ok() {
                anyhow::bail!("snapshot-harness: expected the prompt to fail but it succeeded");
            }
        }
        InputStep::PromptAndCancel {
            text,
            wait_for_file,
        } => {
            let session = require_session(session_id.as_ref(), "promptAndCancel")?.clone();
            let prompt_client = client.clone();
            let prompt_session = session.clone();
            let prompt = text_prompt(text);
            let prompt_done =
                tokio::spawn(async move { prompt_client.prompt(&prompt_session, prompt).await });
            if let Some(marker) = wait_for_file {
                wait_for_workspace_file(cwd, &marker.path, timeout(marker.timeout_ms)).await?;
            } else {
                wait_for_persisted_turn_start(sessions_root, &session, DEFAULT_WAIT_TIMEOUT, None)
                    .await?;
            }
            client.cancel(&session).await?;
            prompt_done.await??;
        }
        InputStep::WaitForFile { path, timeout_ms } => {
            wait_for_workspace_file(cwd, path, timeout(*timeout_ms)).await?;
        }
        InputStep::WaitForTurnStart {
            minimum_turn,
            timeout_ms,
        } => {
            wait_for_persisted_turn_start(
                sessions_root,
                require_session(session_id.as_ref(), "waitForTurnStart")?,
                timeout(*timeout_ms),
                *minimum_turn,
            )
            .await?;
        }
        InputStep::WaitForTurnEnd { timeout_ms } => {
            wait_for_persisted_turn_end(
                sessions_root,
                require_session(session_id.as_ref(), "waitForTurnEnd")?,
                timeout(*timeout_ms),
            )
            .await?;
        }
        InputStep::WaitForSubagentTurnEnd {
            child,
            minimum_turn,
            timeout_ms,
        } => {
            wait_for_persisted_child_turn_end(
                sessions_root,
                child.unwrap_or(1),
                timeout(*timeout_ms),
                minimum_turn.unwrap_or(1),
            )
            .await?;
        }
        InputStep::WaitForGoalPhase { phase, timeout_ms } => {
            wait_for_persisted_goal_phase(
                sessions_root,
                require_session(session_id.as_ref(), "waitForGoalPhase")?,
                *phase,
                timeout(*timeout_ms),
            )
            .await?;
        }
        InputStep::WaitForInboxMessage { text, timeout_ms } => {
            wait_for_persisted_inbox_message(
                sessions_root,
                require_session(session_id.as_ref(), "waitForInboxMessage")?,
                text,
                timeout(*timeout_ms),
            )
            .await?;
        }
        InputStep::WaitForTitleAfterTurnEnd { timeout_ms } => {
            wait_for_persisted_title_after_turn_end(
                sessions_root,
                require_session(session_id.as_ref(), "waitForTitleAfterTurnEnd")?,
                timeout(*timeout_ms),
            )
            .await?;
        }
        InputStep::WaitForEventAfterTurnEnd {
            event_type,
            timeout_ms,
        } => {
            wait_for_persisted_event_after_turn_end(
                sessions_root,
                require_session(session_id.as_ref(), "waitForEventAfterTurnEnd")?,
                event_type,
                timeout(*timeout_ms),
            )
            .await?;
        }
        InputStep::Cancel { wait_for_file } => {
            let session = require_session(session_id.as_ref(), "cancel")?;
            if let Some(marker) = wait_for_file {
                wait_for_workspace_file(cwd, &marker.path, timeout(marker.timeout_ms)).await?;
            }
            client.cancel(session).await?;
        }
    }
    Ok(())
}

fn require_session<'a>(
    session_id: Option<&'a AcpSessionId>,
    operation: &str,
) -> anyhow::Result<&'a AcpSessionId> {
    session_id.ok_or_else(|| anyhow::anyhow!("snapshot-harness: {operation} before newSession"))
}

fn text_prompt(text: &str) -> Vec<Value> {
    vec![json!({"type":"text","text":text})]
}

fn agent_message_text(update: &Value) -> Option<&str> {
    (update.get("sessionUpdate").and_then(Value::as_str) == Some("agent_message_chunk"))
        .then(|| {
            update
                .get("content")
                .and_then(|content| content.get("type"))
                .and_then(Value::as_str)
                .filter(|kind| *kind == "text")
                .and_then(|_| {
                    update
                        .get("content")
                        .and_then(|content| content.get("text"))
                        .and_then(Value::as_str)
                })
        })
        .flatten()
}

/// Waits for an open durable turn at or above an optional minimum.
///
/// # Errors
///
/// Returns malformed boundaries, harvest failures, or the source timeout diagnostic.
pub async fn wait_for_persisted_turn_start(
    root: &Path,
    session_id: &AcpSessionId,
    timeout: Duration,
    minimum_turn: Option<u64>,
) -> anyhow::Result<()> {
    let diagnostic = minimum_turn.map_or_else(
        || "turn/start".to_owned(),
        |turn| format!("turn/start at or beyond turn {turn}"),
    );
    poll_until(timeout, || async {
        let log = harvest_session_logs(root)
            .await?
            .into_iter()
            .find(|log| log.id == session_id.as_str());
        let open = log
            .as_ref()
            .map(|log| latest_open_turn(&log.content))
            .transpose()?
            .flatten();
        Ok(open.is_some_and(|turn| minimum_turn.is_none_or(|minimum| turn >= minimum)))
    })
    .await
    .with_timeout_error(|| {
        format!(
            "snapshot-harness: session {:?} did not persist {diagnostic} within {}ms",
            session_id.as_str(),
            timeout.as_millis()
        )
    })
}

/// Waits until the selected Session's latest complete turn boundary is closed.
///
/// # Errors
///
/// Returns harvest failures or the source timeout diagnostic.
pub async fn wait_for_persisted_turn_end(
    root: &Path,
    session_id: &AcpSessionId,
    timeout: Duration,
) -> anyhow::Result<()> {
    wait_for_log_predicate(root, session_id, timeout, latest_turn_is_closed, "turn/end").await
}

/// Waits for the Nth harvested child to close model work after its descriptor.
///
/// # Errors
///
/// Returns malformed JSONL, harvest failures, or the source timeout diagnostic.
pub async fn wait_for_persisted_child_turn_end(
    root: &Path,
    child: usize,
    timeout: Duration,
    minimum_turn: u64,
) -> anyhow::Result<()> {
    poll_until(timeout, || async {
        let log = harvest_session_logs(root).await?.into_iter().nth(child);
        let Some(log) = log else {
            return Ok(false);
        };
        Ok(latest_turn_is_closed(&log.content)
            && has_request_header_after_descriptor(&log.content).unwrap_or(false)
            && has_closed_turn(&log.content, minimum_turn).unwrap_or(false))
    })
    .await
    .with_timeout_error(|| {
        format!(
            "snapshot-harness: subagent child #{child} did not persist closed turn {minimum_turn} within {}ms",
            timeout.as_millis()
        )
    })
}

/// Waits until the selected Session contains one durable goal phase.
///
/// # Errors
///
/// Returns harvest failures or the source timeout diagnostic.
pub async fn wait_for_persisted_goal_phase(
    root: &Path,
    session_id: &AcpSessionId,
    phase: GoalPhase,
    timeout: Duration,
) -> anyhow::Result<()> {
    poll_until(timeout, || async {
        let log = harvest_session_logs(root)
            .await?
            .into_iter()
            .find(|log| log.id == session_id.as_str());
        let matched = log.is_some_and(|log| {
            jsonl_values(&log.content).is_ok_and(|events| {
                events.iter().any(|event| {
                    event.get("type").and_then(Value::as_str) == Some("goal/change")
                        && event.pointer("/data/goal/phase").and_then(Value::as_str)
                            == Some(phase.as_str())
                })
            })
        });
        Ok(matched)
    })
    .await
    .with_timeout_error(|| {
        format!(
            "snapshot-harness: session {:?} did not persist goal phase {:?} within {}ms",
            session_id.as_str(),
            phase.as_str(),
            timeout.as_millis()
        )
    })
}

/// Waits for one inserted durable inbox message containing `text`.
///
/// # Errors
///
/// Returns malformed JSONL, harvest failures, or the source timeout diagnostic.
pub async fn wait_for_persisted_inbox_message(
    root: &Path,
    session_id: &AcpSessionId,
    text: &str,
    timeout: Duration,
) -> anyhow::Result<()> {
    poll_until(timeout, || async {
        let log = harvest_session_logs(root)
            .await?
            .into_iter()
            .find(|log| log.id == session_id.as_str());
        let Some(log) = log else {
            return Ok(false);
        };
        Ok(jsonl_values(&log.content).is_ok_and(|records| {
            records.iter().any(|record| {
                record.get("type").and_then(Value::as_str) == Some("agent/inbox/spliced")
                    && record
                        .pointer("/data/inserted")
                        .and_then(Value::as_array)
                        .is_some_and(|messages| {
                            messages.iter().any(|message| {
                                message
                                    .get("content")
                                    .and_then(Value::as_array)
                                    .is_some_and(|blocks| {
                                        blocks.iter().any(|block| {
                                            block.get("type").and_then(Value::as_str)
                                                == Some("text")
                                                && block
                                                    .get("text")
                                                    .and_then(Value::as_str)
                                                    .is_some_and(|value| value.contains(text))
                                        })
                                    })
                            })
                        })
            })
        }))
    })
    .await
    .with_timeout_error(|| {
        format!(
            "snapshot-harness: session {:?} did not persist expected inbox message within {}ms",
            session_id.as_str(),
            timeout.as_millis()
        )
    })
}

/// Waits until a title follows the latest complete turn end.
///
/// # Errors
///
/// Returns harvest failures or the source timeout diagnostic.
pub async fn wait_for_persisted_title_after_turn_end(
    root: &Path,
    session_id: &AcpSessionId,
    timeout: Duration,
) -> anyhow::Result<()> {
    wait_for_log_predicate(
        root,
        session_id,
        timeout,
        latest_title_follows_turn_end,
        "session/title after turn/end",
    )
    .await
}

/// Waits until `event_type` follows the latest complete turn end.
///
/// # Errors
///
/// Returns harvest failures or the source timeout diagnostic.
pub async fn wait_for_persisted_event_after_turn_end(
    root: &Path,
    session_id: &AcpSessionId,
    event_type: &str,
    timeout: Duration,
) -> anyhow::Result<()> {
    let event_type = event_type.to_owned();
    let label = format!("{event_type} after turn/end");
    let expected_type = event_type;
    wait_for_log_predicate(
        root,
        session_id,
        timeout,
        move |content| latest_event_follows_turn_end(content, &expected_type),
        &label,
    )
    .await
}

async fn wait_for_log_predicate(
    root: &Path,
    session_id: &AcpSessionId,
    timeout: Duration,
    predicate: impl Fn(&str) -> bool,
    label: &str,
) -> anyhow::Result<()> {
    poll_until(timeout, || async {
        Ok(harvest_session_logs(root)
            .await?
            .into_iter()
            .find(|log| log.id == session_id.as_str())
            .is_some_and(|log| predicate(&log.content)))
    })
    .await
    .with_timeout_error(|| {
        format!(
            "snapshot-harness: session {:?} did not persist {label} within {}ms",
            session_id.as_str(),
            timeout.as_millis()
        )
    })
}

/// Waits for one generated-workspace marker.
///
/// # Errors
///
/// Returns the source timeout diagnostic when the marker never appears.
pub async fn wait_for_workspace_file(
    cwd: &Path,
    path: &Path,
    timeout: Duration,
) -> anyhow::Result<()> {
    let target = cwd.join(path);
    poll_until(timeout, || async { Ok(target.exists()) })
        .await
        .with_timeout_error(|| {
            format!(
                "snapshot-harness: workspace file {:?} did not appear within {}ms",
                path.to_string_lossy(),
                timeout.as_millis()
            )
        })
}

/// Whether the latest complete raw turn boundary closes its turn.
#[must_use]
pub fn latest_turn_is_closed(content: &str) -> bool {
    let complete = complete_prefix(content);
    complete.rfind("\n{\"type\":\"turn/end\",") > complete.rfind("\n{\"type\":\"turn/start\",")
}

/// Whether the latest complete title follows the latest complete turn end.
#[must_use]
pub fn latest_title_follows_turn_end(content: &str) -> bool {
    let complete = complete_prefix(content);
    let turn_end = complete.rfind("\n{\"type\":\"turn/end\",");
    turn_end.is_some() && complete.rfind("\n{\"type\":\"session/title\",") > turn_end
}

/// Whether one complete event kind follows the latest complete turn end.
#[must_use]
pub fn latest_event_follows_turn_end(content: &str, event_type: &str) -> bool {
    let complete = complete_prefix(content);
    let turn_end = complete.rfind("\n{\"type\":\"turn/end\",");
    turn_end.is_some() && complete.rfind(&format!("\n{{\"type\":\"{event_type}\",")) > turn_end
}

/// Latest open turn number, validated as a positive safe integer.
///
/// # Errors
///
/// Returns malformed JSON or the source invalid-turn diagnostic.
pub fn latest_open_turn(content: &str) -> anyhow::Result<Option<u64>> {
    let complete = complete_prefix(content);
    let start = complete.rfind("\n{\"type\":\"turn/start\",");
    if start <= complete.rfind("\n{\"type\":\"turn/end\",") {
        return Ok(None);
    }
    let Some(start) = start else {
        return Ok(None);
    };
    let line_start = start + 1;
    let line_end = complete[line_start..]
        .find('\n')
        .map_or(complete.len(), |offset| line_start + offset);
    let record: Value = serde_json::from_str(&complete[line_start..line_end])?;
    let turn = record.pointer("/data/turn").and_then(safe_positive_integer);
    match turn {
        Some(turn) => Ok(Some(turn)),
        None => anyhow::bail!("snapshot-harness: invalid persisted turn/start record"),
    }
}

/// Whether a raw log contains a specified closed turn.
///
/// # Errors
///
/// Returns when any nonempty JSONL record is malformed.
pub fn has_closed_turn(content: &str, turn: u64) -> anyhow::Result<bool> {
    Ok(jsonl_values(content)?.iter().any(|event| {
        event.get("type").and_then(Value::as_str) == Some("turn/end")
            && event
                .pointer("/data/turn")
                .and_then(safe_nonnegative_integer)
                == Some(turn)
    }))
}

/// Whether a complete child log contains a request header after its last descriptor.
///
/// # Errors
///
/// Returns when any complete JSONL record is malformed.
pub fn has_request_header_after_descriptor(content: &str) -> anyhow::Result<bool> {
    let events = jsonl_values(complete_prefix(content))?;
    let descriptor = events.iter().rposition(|event| {
        event.get("type").and_then(Value::as_str) == Some("subagent/descriptor")
    });
    Ok(descriptor.is_some_and(|index| {
        events[index + 1..]
            .iter()
            .any(|event| event.get("type").and_then(Value::as_str) == Some("request/header"))
    }))
}

/// Recursively collects raw Session logs and orders the primary before children.
///
/// # Errors
///
/// Returns file read or header JSON failures. An absent root yields an empty inventory.
pub async fn harvest_session_logs(root: &Path) -> anyhow::Result<Vec<HarvestedLog>> {
    let root = root.to_owned();
    tokio::task::spawn_blocking(move || harvest_session_logs_sync(&root)).await?
}

fn harvest_session_logs_sync(root: &Path) -> anyhow::Result<Vec<HarvestedLog>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let Ok(entries) = WalkDir::new(root)
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
    else {
        return Ok(Vec::new());
    };
    let mut logs = Vec::new();
    for entry in entries {
        if !entry.file_type().is_file() || entry.file_name() != "session.jsonl" {
            continue;
        }
        let content = std::fs::read_to_string(entry.path())?;
        let first = content
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("{}");
        let header: Value = serde_json::from_str(first)?;
        logs.push(HarvestedLog {
            id: header
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            created_at: header
                .get("createdAt")
                .and_then(Value::as_f64)
                .unwrap_or(0.0),
            parent_session: header
                .get("parentSession")
                .and_then(Value::as_str)
                .map(str::to_owned),
            content,
        });
    }
    logs.sort_by(|left, right| {
        left.parent_session
            .is_some()
            .cmp(&right.parent_session.is_some())
            .then_with(|| left.created_at.total_cmp(&right.created_at))
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(logs)
}

fn jsonl_values(content: &str) -> anyhow::Result<Vec<Value>> {
    content
        .split('\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).map_err(anyhow::Error::from))
        .collect()
}

fn safe_positive_integer(value: &Value) -> Option<u64> {
    safe_nonnegative_integer(value).filter(|number| *number >= 1)
}

fn safe_nonnegative_integer(value: &Value) -> Option<u64> {
    let number = value.as_f64()?;
    if number.fract() != 0.0 || !(0.0..=9_007_199_254_740_991.0).contains(&number) {
        return None;
    }
    format!("{number:.0}").parse().ok()
}

fn complete_prefix(content: &str) -> &str {
    content
        .rfind('\n')
        .map_or("", |last_newline| &content[..=last_newline])
}

fn timeout(milliseconds: Option<u64>) -> Duration {
    milliseconds.map_or(DEFAULT_WAIT_TIMEOUT, Duration::from_millis)
}

enum PollOutcome {
    Matched,
    TimedOut,
}

trait TimeoutResult {
    fn with_timeout_error(self, message: impl FnOnce() -> String) -> anyhow::Result<()>;
}

impl TimeoutResult for anyhow::Result<PollOutcome> {
    fn with_timeout_error(self, message: impl FnOnce() -> String) -> anyhow::Result<()> {
        match self? {
            PollOutcome::Matched => Ok(()),
            PollOutcome::TimedOut => anyhow::bail!(message()),
        }
    }
}

async fn poll_until<F, Fut>(timeout: Duration, mut check: F) -> anyhow::Result<PollOutcome>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = anyhow::Result<bool>>,
{
    let deadline = Instant::now() + timeout;
    loop {
        if check().await? {
            return Ok(PollOutcome::Matched);
        }
        if Instant::now() >= deadline {
            return Ok(PollOutcome::TimedOut);
        }
        tokio::time::sleep(
            WAIT_POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())),
        )
        .await;
    }
}

fn canonical_cwd_aliases(cwd: &Path) -> Vec<PathBuf> {
    std::fs::canonicalize(cwd).map_or_else(|_| Vec::new(), |path| vec![path])
}

async fn copy_workspace(source: &Path, destination: &Path) -> anyhow::Result<()> {
    let source = source.to_owned();
    let destination = destination.to_owned();
    tokio::task::spawn_blocking(move || {
        for entry in WalkDir::new(&source).min_depth(1) {
            let entry = entry?;
            let relative = entry.path().strip_prefix(&source)?;
            let target = destination.join(relative);
            if entry.file_type().is_dir() {
                std::fs::create_dir_all(target)?;
            } else if entry.file_type().is_file() {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::copy(entry.path(), target)?;
            } else {
                anyhow::bail!(
                    "snapshot-harness: workspace seed contains unsupported entry {}",
                    relative.display()
                );
            }
        }
        Ok::<(), anyhow::Error>(())
    })
    .await??;
    Ok(())
}

async fn remove_owned_path(path: &Path) -> anyhow::Result<()> {
    match tokio::fs::remove_dir_all(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn attach_stderr(error: anyhow::Error, launched: Option<&LaunchedAcpTestAgent>) -> anyhow::Error {
    let stderr = launched.map_or_else(String::new, LaunchedAcpTestAgent::stderr);
    if stderr.is_empty() {
        error
    } else {
        anyhow::anyhow!("snapshot-harness: scenario failed: {error}\nagent stderr:\n{stderr}")
    }
}

fn finish_with_cleanup(
    outcome: anyhow::Result<RunResult>,
    cleanup_failures: &[anyhow::Error],
) -> anyhow::Result<RunResult> {
    if cleanup_failures.is_empty() {
        return outcome;
    }
    let cleanup = cleanup_failures
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ");
    match outcome {
        Ok(_) => anyhow::bail!("snapshot cleanup failed: {cleanup}"),
        Err(error) => anyhow::bail!("snapshot scenario and cleanup failed: {error}; {cleanup}"),
    }
}
