//! Scenario ownership stays in Rust; Python only calls and serializes its public SDK API.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use serde_json::{Value, json};
use tokio::{process::Command, sync::watch};

use crate::{
    constants::{
        CODE_PROMPT, CODE_WORKER_TEXT, CUSTOM_CORDIS, EXPECTED_TEXT, MINIMAL_EDITOR_PATH_PREFIX,
        MINIMAL_PROMPT, MINIMAL_TEXT, SNAPSHOT_PROMPT, SNAPSHOT_SESSION_ID, WORKFLOW_PROMPT,
        WORKFLOW_WORKER_TEXT,
    },
    peer::{Interrupted, Peer},
    server::MockModel,
    snapshot,
};

const PYTHON_ADAPTER: &str = r"
import json, sys, threading, traceback
from dataclasses import asdict
from deepseek_harness import DeepSeekHarness
plan = json.loads(sys.stdin.readline())
harness = DeepSeekHarness(**plan['config'])
output_lock = threading.Lock()
def emit(value):
    with output_lock:
        print(json.dumps(value), flush=True)
def invoke():
    try:
        with harness:
            results = [harness.run(**request) for request in plan['runs']]
        emit({'results': [asdict(result) for result in results]})
    except BaseException:
        emit({'error': traceback.format_exc()})
worker = threading.Thread(target=invoke)
worker.start()
try:
    for line in sys.stdin:
        try:
            {'close': harness.client.close}[json.loads(line)['op']]()
            emit({'control': 'closed'})
        except Exception:
            emit({'control_error': traceback.format_exc()})
finally:
    harness.client.close()
    worker.join()
";

/// Source smoke selection, including its ordered all-scenario aggregate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Scenario {
    /// Default, custom, minimal, snapshot, then direct.
    All,
    /// SDK default packaged configuration and Zstandard persistence.
    SdkDefault,
    /// Explicit SDK text, code, and workflow turns.
    SdkCustom,
    /// Persistent Bash and the string-replacement editor.
    SdkMinimal,
    /// Dynamic plugins, code dispatch, and both child paths with exact snapshots.
    SdkSnapshot,
    /// Direct executable JSON-RPC without the SDK.
    Direct,
}

impl Scenario {
    fn name(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::SdkDefault => "sdk-default",
            Self::SdkCustom => "sdk-custom",
            Self::SdkMinimal => "sdk-minimal",
            Self::SdkSnapshot => "sdk-snapshot",
            Self::Direct => "direct",
        }
    }
}

/// Native smoke inputs; no provider credentials or external model endpoint are used.
pub struct Options {
    /// Ordered scenario selection.
    pub scenario: Scenario,
    /// Required for custom, minimal, snapshot, direct, and aggregate runs.
    pub executable: Option<PathBuf>,
    /// Explicit authorization to replace the four repository snapshot files.
    pub update_snapshots: bool,
    /// Python interpreter exposing the SDK under test.
    pub python: PathBuf,
    /// Owning checkout for the minimal YAML and advanced snapshots.
    pub root: PathBuf,
}

impl Options {
    /// Checks source flag dependencies before opening a listener or starting processes.
    ///
    /// # Errors
    /// Rejects missing executables, invalid snapshot-update selection, and absent executable files.
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.scenario == Scenario::SdkDefault || self.executable.is_some(),
            "--exe is required for custom, minimal, snapshot, and direct scenarios"
        );
        anyhow::ensure!(
            !self.update_snapshots
                || matches!(self.scenario, Scenario::All | Scenario::SdkSnapshot),
            "--update-snapshots requires --scenario sdk-snapshot or all"
        );
        if let Some(executable) = &self.executable {
            anyhow::ensure!(
                executable.is_file(),
                "runtime executable does not exist: {}",
                executable.display()
            );
        }
        Ok(())
    }
}

/// Runs selected scenarios against an owned loopback model and joins all child teardown.
/// A Ctrl-C listener cancels pending transport operations and runs the same close grace.
///
/// # Errors
/// Reports invalid inputs, Python/runtime failures, model assertions, and snapshot mismatches.
pub async fn run(mut options: Options) -> anyhow::Result<()> {
    options.validate()?;
    options.root = options.root.canonicalize()?;
    options.executable = options
        .executable
        .as_ref()
        .map(|path| path.canonicalize())
        .transpose()?;
    if options.python.is_file() {
        // Resolving the interpreter's virtual-environment symlink changes sys.prefix.
        options.python = std::path::absolute(&options.python)?;
    }
    let model = MockModel::start().await?;
    let scenarios = if options.scenario == Scenario::All {
        vec![
            Scenario::SdkDefault,
            Scenario::SdkCustom,
            Scenario::SdkMinimal,
            Scenario::SdkSnapshot,
            Scenario::Direct,
        ]
    } else {
        vec![options.scenario]
    };
    let (cancel, cancellation) = watch::channel(false);
    let result = {
        let work = async {
            for scenario in scenarios {
                if *cancellation.borrow() {
                    return Err(Interrupted.into());
                }
                if scenario == Scenario::Direct {
                    direct(&options, &model.url, &cancellation).await?;
                } else {
                    sdk(&options, &model.url, scenario, &cancellation).await?;
                }
            }
            Ok::<_, anyhow::Error>(())
        };
        tokio::pin!(work);
        tokio::select! {
          biased;
          signal = tokio::signal::ctrl_c() => match signal {
              Ok(()) => { let _ = cancel.send(true); work.await }
              Err(error) => Err(error.into()),
          },
          result = &mut work => result,
        }
    };
    let cleanup = model.close(result.is_ok()).await;
    after_close(result, cleanup)?;
    println!("smoke-python-runtime: {} passed", options.scenario.name());
    Ok(())
}

fn temporary(scenario: Scenario) -> anyhow::Result<(tempfile::TempDir, PathBuf)> {
    let temporary = tempfile::Builder::new()
        .prefix(&format!("seekdeep-{}-", scenario.name()))
        .tempdir()?;
    let root = temporary.path().canonicalize()?;
    Ok((temporary, root))
}

async fn sdk(
    options: &Options,
    url: &str,
    scenario: Scenario,
    cancellation: &watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let (_temporary, root) = temporary(scenario)?;
    let sessions = root.join("sessions");
    let mut config = json!({"provider":"deepseek-official","model":"smoke-model","cwd":root,"session_root":sessions,
        "api_key":"sk-keyless-smoke","base_url":url,"request_timeout_seconds":60});
    if scenario != Scenario::SdkDefault {
        config["runtime_bin"] = json!(options.executable);
        let cordis = if scenario == Scenario::SdkMinimal {
            options
                .root
                .join("examples/jsonrpc-agent/minimal.cordis.yml")
        } else {
            let path = root.join("cordis.yml");
            std::fs::write(&path, CUSTOM_CORDIS)?;
            path
        };
        config["cordis"] = json!(cordis);
    }
    let runs = match scenario {
        Scenario::SdkDefault => {
            json!([{"input":"reply with the smoke text","session_id":"default-smoke"}])
        }
        Scenario::SdkCustom => json!([
            {"input":"reply with the smoke text","session_id":"custom-smoke"},
            {"input":CODE_PROMPT,"session_id":"custom-smoke"},
            {"input":WORKFLOW_PROMPT,"session_id":"custom-smoke"}]),
        Scenario::SdkMinimal => {
            json!([{"input":format!("{MINIMAL_PROMPT}\n{MINIMAL_EDITOR_PATH_PREFIX}{}", root.join("created.txt").display()),"session_id":"minimal-agent-smoke"}])
        }
        Scenario::SdkSnapshot => {
            json!([{"input":SNAPSHOT_PROMPT,"session_id":SNAPSHOT_SESSION_ID}])
        }
        Scenario::All | Scenario::Direct => unreachable!("only SDK scenarios reach this driver"),
    };
    let results = python_results(
        options,
        &root,
        &json!({"config":config,"runs":runs}),
        cancellation,
    )
    .await?;
    verify_sdk_results(options, scenario, &root, &sessions, &results)
}

async fn python_results(
    options: &Options,
    root: &Path,
    plan: &Value,
    cancellation: &watch::Receiver<bool>,
) -> anyhow::Result<Vec<Value>> {
    let mut command = Command::new(&options.python);
    command.args(["-I", "-c", PYTHON_ADAPTER]).current_dir(root);
    let mut peer = Peer::spawn(&mut command)?;
    peer.cancel_on(cancellation.clone());
    let result = async {
        peer.send(plan).await?;
        let messages = peer.read_adapter().await?;
        if let Some(error) = messages
            .last()
            .and_then(|message| message["error"].as_str())
        {
            anyhow::bail!("Python SDK adapter failed:\n{error}");
        }
        Ok::<_, anyhow::Error>(
            messages.last().expect("matching adapter response")["results"]
                .as_array()
                .expect("validated results")
                .clone(),
        )
    }
    .await;
    let result = if result.as_ref().is_err_and(anyhow::Error::is::<Interrupted>) {
        after_close(result, stop_adapter(&mut peer).await)
    } else {
        result
    };
    let cleanup = peer.close().await;
    after_close(result, cleanup)
}

async fn stop_adapter(peer: &mut Peer) -> anyhow::Result<()> {
    peer.begin_interrupt_cleanup();
    let stopping = async {
        loop {
            peer.send(&json!({"op":"close"})).await?;
            let messages = peer
                .read_until(|message| {
                    message["control"] == "closed"
                        || message["results"].is_array()
                        || message["error"].is_string()
                        || message["control_error"].is_string()
                })
                .await?;
            if messages
                .iter()
                .any(|message| message["results"].is_array() || message["error"].is_string())
            {
                return Ok(());
            }
            if let Some(error) = messages
                .iter()
                .find_map(|message| message["control_error"].as_str())
            {
                anyhow::bail!("Python SDK close failed:\n{error}");
            }
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), stopping)
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "Python SDK adapter did not complete cancellation within its close grace"
            )
        })?
}

fn verify_sdk_results(
    options: &Options,
    scenario: Scenario,
    root: &Path,
    sessions: &Path,
    results: &[Value],
) -> anyhow::Result<()> {
    let count = if scenario == Scenario::SdkCustom {
        3
    } else {
        1
    };
    anyhow::ensure!(
        results.len() == count,
        "SDK returned {} runs instead of {count}",
        results.len()
    );
    match scenario {
        Scenario::SdkDefault => {
            require_response(&results[0], EXPECTED_TEXT)?;
            let paths = log_paths(sessions, "jsonl.zstd")?;
            anyhow::ensure!(
                paths.len() == 1,
                "expected one Zstandard JSONL session log under {}, found {paths:?}",
                sessions.display()
            );
            anyhow::ensure!(
                std::fs::read(&paths[0])?.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]),
                "session log has no Zstandard magic: {}",
                paths[0].display()
            );
        }
        Scenario::SdkCustom => {
            let expected = [EXPECTED_TEXT, CODE_WORKER_TEXT, WORKFLOW_WORKER_TEXT];
            for (result, expected) in results.iter().zip(expected) {
                require_response(result, expected)?;
            }
            assert_session_log(sessions, root, &expected)?;
        }
        Scenario::SdkMinimal => {
            anyhow::ensure!(
                results[0]["events"].to_string().contains(MINIMAL_TEXT),
                "minimal agent run emitted no final response: {}",
                results[0]["events"]
            );
            let text = std::fs::read_to_string(root.join("created.txt"))?;
            anyhow::ensure!(
                text == "created by packaged editor\n",
                "packaged editor wrote unexpected content: {text:?}"
            );
            assert_session_log(
                sessions,
                root,
                &[MINIMAL_TEXT, "COUNT=1", "COUNT=2 CWD=/tmp"],
            )?;
        }
        Scenario::SdkSnapshot => {
            let logs = read_session_logs(sessions)?;
            let files = snapshot::build_snapshot_files(&results[0], &logs, root)?;
            snapshot::compare_snapshot_files(
                &options
                    .root
                    .join("scripts/snapshots/python-sdk-single-exe/advanced"),
                &files,
                options.update_snapshots,
            )?;
        }
        Scenario::All | Scenario::Direct => unreachable!("only SDK scenarios reach this verifier"),
    }
    Ok(())
}

fn require_response(result: &Value, expected: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        result["final_response"] == expected,
        "unexpected final response: {}",
        result["final_response"]
    );
    Ok(())
}

async fn direct(
    options: &Options,
    url: &str,
    cancellation: &watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let (_temporary, root) = temporary(Scenario::Direct)?;
    let sessions = root.join("sessions");
    let cordis = root.join("cordis.yml");
    std::fs::write(&cordis, CUSTOM_CORDIS)?;
    let mut command = Command::new(
        options
            .executable
            .as_ref()
            .expect("validated direct executable"),
    );
    command
        .current_dir(&root)
        .env("SEEKDEEP_CORDIS_CONFIG", cordis)
        .env("SEEKDEEP_SESSION_ROOT", &sessions)
        .env("SEEKDEEP_CWD", &root)
        .env("DEEPSEEK_API_KEY", "sk-keyless-smoke")
        .env("DEEPSEEK_BASE_URL", url);
    let mut peer = Peer::spawn(&mut command)?;
    peer.cancel_on(cancellation.clone());
    let result = async {
        peer.send(&json!({"jsonrpc":"2.0","id":"initialize","method":"initialize","params":{"cwd":root,"provider":"deepseek-official","model":"smoke-model"}})).await?;
        peer.read_until(|message| message["id"] == "initialize").await?;
        peer.send(&json!({"jsonrpc":"2.0","id":"prompt","method":"session/prompt","params":{"sessionId":"direct-smoke","contentBlocks":[{"type":"text","text":"reply with the smoke text"}]}})).await?;
        let mut messages = peer.read_until(|message| message["id"] == "prompt").await?;
        if !messages.iter().any(is_idle) { messages.extend(peer.read_until(is_idle).await?); }
        anyhow::ensure!(serde_json::to_string(&messages)?.contains(EXPECTED_TEXT), "direct runtime emitted no final response: {messages:?}");
        peer.send(&json!({"jsonrpc":"2.0","id":"shutdown","method":"shutdown"})).await?;
        peer.read_until(|message| message["id"] == "shutdown").await?;
        Ok::<_, anyhow::Error>(())
    }.await;
    let cleanup = peer.close().await;
    after_close(result, cleanup)?;
    assert_session_log(&sessions, &root, &[EXPECTED_TEXT])
}

fn after_close<T>(result: anyhow::Result<T>, cleanup: anyhow::Result<()>) -> anyhow::Result<T> {
    match (result, cleanup) {
        (Err(error), Err(cleanup)) => Err(cleanup.context(format!("while handling: {error:#}"))),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
    }
}

fn is_idle(message: &Value) -> bool {
    message["method"] == "session.status" && message["params"]["status"] == "idle"
}

fn log_paths(root: &Path, suffix: &str) -> anyhow::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    if !root.exists() {
        return Ok(paths);
    }
    for entry in walkdir::WalkDir::new(root) {
        let entry = entry?;
        if entry.path().is_file()
            && entry
                .file_name()
                .to_string_lossy()
                .ends_with(&format!(".{suffix}"))
        {
            paths.push(entry.into_path());
        }
    }
    paths.sort();
    Ok(paths)
}

fn assert_session_log(sessions: &Path, cwd: &Path, expected: &[&str]) -> anyhow::Result<()> {
    let paths = log_paths(sessions, "jsonl")?;
    anyhow::ensure!(
        paths.len() == 1,
        "expected one JSONL session log under {}, found {paths:?}",
        sessions.display()
    );
    let text = std::fs::read_to_string(&paths[0])?;
    let header: Value = serde_json::from_str(text.lines().next().unwrap_or_default())?;
    anyhow::ensure!(
        header["cwd"].as_str() == cwd.to_str(),
        "session header cwd is not absolute/canonical: {header}"
    );
    for expected in expected {
        anyhow::ensure!(
            text.contains(expected),
            "session log has no {expected:?} response: {}",
            paths[0].display()
        );
    }
    Ok(())
}

fn read_session_logs(sessions: &Path) -> anyhow::Result<Value> {
    let mut logs = serde_json::Map::new();
    let mut ids = BTreeSet::new();
    for path in log_paths(sessions, "jsonl")? {
        let text = std::fs::read_to_string(&path)?;
        let records = text
            .lines()
            .filter(|line| !line.is_empty())
            .map(serde_json::from_str::<Value>)
            .collect::<Result<Vec<_>, _>>()?;
        let header = records
            .first()
            .filter(|record| record["type"] == "session")
            .ok_or_else(|| anyhow::anyhow!("session log has no header: {}", path.display()))?;
        let id = header["id"]
            .as_str()
            .ok_or_else(|| {
                anyhow::anyhow!("session log header has no string id: {}", path.display())
            })?
            .to_owned();
        anyhow::ensure!(
            ids.insert(id.clone()),
            "duplicate persisted session id: {id}"
        );
        logs.insert(id, Value::Array(records));
    }
    Ok(Value::Object(logs))
}
