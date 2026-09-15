#![cfg(all(feature = "crash-test", not(windows)))]
//! Hard-crash evidence for request and tool semantic durability boundaries.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use seekdeep_cordis::Context;
use seekdeep_core::{
    repair::TOOL_OUTCOME_UNKNOWN, session::SessionId, session_store::SessionStore,
};
use seekdeep_session_persistence::SessionPersistence;
use seekdeep_session_persistence_jsonl::{JsonlCompression, JsonlConfig, JsonlSessionPersistence};

const SESSION_ID: &str = "semantic-checkpoint-crash";
const TIMEOUT: Duration = Duration::from_secs(30);

fn crash_at(mode: &str, expected: &str) -> anyhow::Result<(tempfile::TempDir, ExitStatus)> {
    let root = tempfile::tempdir()?;
    let marker = root.path().join("failpoint");
    fs::write(&marker, "")?;
    let mut child = Command::new(env!("CARGO_BIN_EXE_seekdeep-checkpoint-crash-child"))
        .args([mode])
        .arg(root.path())
        .arg(&marker)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    wait_for_marker(&mut child, &marker, expected)?;
    child.kill()?;
    let status = child.wait()?;
    Ok((root, status))
}

fn wait_for_marker(child: &mut Child, marker: &Path, expected: &str) -> anyhow::Result<()> {
    let started = Instant::now();
    loop {
        let content = fs::read_to_string(marker).unwrap_or_default();
        if content == expected {
            return Ok(());
        }
        if !expected.starts_with(&content) {
            anyhow::bail!("crash child wrote unexpected failpoint {content:?}");
        }
        if let Some(status) = child.try_wait()? {
            let stderr = child
                .stderr
                .take()
                .map(|stderr| std::io::read_to_string(stderr).unwrap_or_default())
                .unwrap_or_default();
            anyhow::bail!("crash child exited early with {status}: {stderr}");
        }
        anyhow::ensure!(
            started.elapsed() < TIMEOUT,
            "crash child failpoint timed out"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

async fn load(root: PathBuf) -> anyhow::Result<Vec<seekdeep_core::session::SessionEvent>> {
    let context = Context::new();
    let sessions = SessionStore::install(&context)?;
    let mut config = JsonlConfig::new(root);
    config.compression = JsonlCompression::None;
    let persistence = JsonlSessionPersistence::new(sessions, config)?;
    Ok(persistence.load(&SessionId::new(SESSION_ID)).await?.events)
}

#[cfg(unix)]
fn assert_sigkill(status: ExitStatus) {
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(status.signal(), Some(9));
}

#[tokio::test]
async fn complete_request_prefix_is_durable_before_model_dispatch() {
    let (root, status) = crash_at("request", "request-dispatched").expect("crash request child");
    assert_sigkill(status);
    let events = load(root.path().to_owned())
        .await
        .expect("load repaired request");
    assert_eq!(
        events
            .iter()
            .map(|event| event.event_type.as_str())
            .collect::<Vec<_>>(),
        [
            "turn/start",
            "step/start",
            "checkpoint/request-ready",
            "step/end",
            "turn/end",
        ]
    );
    assert_eq!(events[2].data["complete"], true);
    assert_eq!(
        events.last().expect("turn end").data["reason"]["kind"],
        "interrupted"
    );
}

#[tokio::test]
async fn tool_intent_is_durable_before_side_effect_and_repairs_unknown_outcome() {
    let (root, status) = crash_at("tool", "tool-side-effect").expect("crash tool child");
    assert_sigkill(status);
    let events = load(root.path().to_owned())
        .await
        .expect("load repaired tool");
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "assistant/message")
    );
    assert!(events.iter().any(|event| event.event_type == "tool/call"));
    let result = events
        .iter()
        .find(|event| event.event_type == "tool/result")
        .expect("repaired tool result");
    assert_eq!(result.data["error"]["name"], "ToolOutcomeUnknownError");
    assert_eq!(result.data["error"]["code"], TOOL_OUTCOME_UNKNOWN);
    assert!(
        result.data["message"]["content"][0]["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("Do not retry blindly."))
    );
}
