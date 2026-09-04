//! Drive a relocated native SDK process with the pinned source's keyless model oracle.

use std::{
    collections::BTreeMap,
    io::{BufRead as _, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Arc,
    time::Duration,
};

use seekdeep_core::session::SessionId;
use seekdeep_sdk_client::{
    DeepSeekHarness, DeepSeekHarnessOptions, HarnessClientOptions, RunOptions,
};
use serde_json::Value;

const ORACLE: &str = r"
import http.server, json, pathlib, runpy, sys
source = runpy.run_path(str(pathlib.Path(sys.argv[1]) / 'scripts/smoke-python-runtime.py'))
server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), source['MockModelHandler'])
print(json.dumps({
    'url': 'http://127.0.0.1:' + str(server.server_port),
    'config': source['CUSTOM_CORDIS'],
    'turns': [
        ['reply with the smoke text', source['EXPECTED_TEXT']],
        [source['CODE_PROMPT'], source['CODE_WORKER_TEXT']],
        [source['WORKFLOW_PROMPT'], source['WORKFLOW_WORKER_TEXT']],
    ],
}), flush=True)
server.serve_forever()
";

struct OracleProcess(Child);

impl Drop for OracleProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    anyhow::ensure!(
        arguments.len() == 2,
        "usage: packaged_source_smoke <pinned-source> <compiled-packaged-runtime>"
    );
    let source = Path::new(&arguments[0]);
    verify_pin(source)?;
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().canonicalize()?;
    let binary = root.join(if cfg!(windows) {
        "runtime.exe"
    } else {
        "runtime"
    });
    std::fs::copy(&arguments[1], &binary)?;
    let (_oracle, fixture) = start_oracle(source, &root)?;
    let harness = create_harness(&root, &binary, &fixture)?;
    let turns = fixture["turns"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("oracle turns absent"))?;
    let result = run_turns(&harness, turns).await;
    let cleanup = harness.close().await;
    result?;
    cleanup?;
    verify_log(&root, turns)?;
    println!(
        "relocated packaged runtime completed all {} source-model turns and durable log checks without Node or sibling helpers",
        turns.len()
    );
    Ok(())
}

fn verify_pin(source: &Path) -> anyhow::Result<()> {
    let head = Command::new("git")
        .arg("-C")
        .arg(source)
        .args(["rev-parse", "HEAD"])
        .output()?;
    let pin = include_str!("../../../SOURCE_SNAPSHOT")
        .lines()
        .find_map(|line| line.strip_prefix("commit="))
        .ok_or_else(|| anyhow::anyhow!("source pin missing"))?;
    anyhow::ensure!(
        head.status.success() && String::from_utf8_lossy(&head.stdout).trim() == pin,
        "oracle differs from SOURCE_SNAPSHOT"
    );
    Ok(())
}

fn start_oracle(source: &Path, root: &Path) -> anyhow::Result<(OracleProcess, Value)> {
    let mut oracle = OracleProcess(
        Command::new("python3")
            .args(["-u", "-c", ORACLE])
            .arg(source)
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?,
    );
    let mut ready = String::new();
    BufReader::new(
        oracle
            .0
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("oracle stdout absent"))?,
    )
    .read_line(&mut ready)?;
    let fixture: Value = serde_json::from_str(&ready)?;
    Ok((oracle, fixture))
}

fn create_harness(
    root: &Path,
    binary: &Path,
    fixture: &Value,
) -> anyhow::Result<Arc<DeepSeekHarness>> {
    let config = fixture["config"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("oracle config absent"))?
        .replace("@deepseek-ai/dsh-", "@seekdeep-ai/seekdeep-")
        .replace("process.env.DSH_", "process.env.SEEKDEEP_");
    let config_path = root.join("cordis.yml");
    std::fs::write(&config_path, config)?;
    let environment = BTreeMap::from([
        ("PATH".to_owned(), String::new()),
        (
            "SEEKDEEP_CORDIS_CONFIG".to_owned(),
            config_path.to_string_lossy().into_owned(),
        ),
        (
            "SEEKDEEP_HOME".to_owned(),
            root.join("home").to_string_lossy().into_owned(),
        ),
        (
            "SEEKDEEP_AGENTS_HOME".to_owned(),
            root.join("agents").to_string_lossy().into_owned(),
        ),
        (
            "SEEKDEEP_CWD".to_owned(),
            root.to_string_lossy().into_owned(),
        ),
        (
            "SEEKDEEP_SESSION_ROOT".to_owned(),
            root.join("sessions").to_string_lossy().into_owned(),
        ),
        ("SEEKDEEP_TELEMETRY_DISABLED".to_owned(), "1".to_owned()),
        ("DEEPSEEK_API_KEY".to_owned(), "sk-keyless-smoke".to_owned()),
        (
            "DEEPSEEK_BASE_URL".to_owned(),
            fixture["url"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("oracle URL absent"))?
                .to_owned(),
        ),
    ]);
    let mut launch = HarnessClientOptions::new(binary.to_string_lossy());
    launch.cwd = Some(root.to_string_lossy().into_owned());
    launch.env = Some(environment);
    launch.request_timeout_ms = Some(30_000.0);
    let harness = DeepSeekHarness::new(DeepSeekHarnessOptions {
        launch,
        cwd: Some(root.to_string_lossy().into_owned()),
        provider: Some("deepseek-official".to_owned()),
        model: Some("smoke-model".to_owned()),
        max_tokens: None,
    })?;
    Ok(harness)
}

async fn run_turns(harness: &Arc<DeepSeekHarness>, turns: &[Value]) -> anyhow::Result<()> {
    for turn in turns {
        let prompt = turn[0]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("prompt absent"))?;
        let expected = turn[1]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("expected response absent"))?;
        let result = tokio::time::timeout(
            Duration::from_secs(40),
            harness.run(
                prompt,
                RunOptions {
                    session_id: Some(SessionId::new("custom-smoke")),
                    on_notification: None,
                },
            ),
        )
        .await??;
        anyhow::ensure!(
            result.final_response == expected,
            "{prompt}: expected {expected:?}, got {:?}; events: {}",
            result.final_response,
            serde_json::to_string(&result.events)?
        );
        println!("source model accepted: {expected}");
    }
    Ok(())
}

fn verify_log(root: &Path, turns: &[Value]) -> anyhow::Result<()> {
    let files = jsonl_files(&root.join("sessions"))?;
    anyhow::ensure!(
        files.len() == 1,
        "expected one durable session log, found {}",
        files.len()
    );
    let log = std::fs::read_to_string(&files[0])?;
    for turn in turns {
        anyhow::ensure!(
            log.contains(turn[1].as_str().unwrap_or_default()),
            "durable log is missing a completed response"
        );
    }
    anyhow::ensure!(
        log.contains("tool-workflow/run-end"),
        "durable workflow result absent"
    );
    Ok(())
}

fn jsonl_files(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory)? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "jsonl")
            {
                files.push(path);
            }
        }
    }
    Ok(files)
}
