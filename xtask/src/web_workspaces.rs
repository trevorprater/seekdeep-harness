//! Keyless workspace-management browser scenarios backed by the normal Rust Host.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use seekdeep_core::{
    chunk_rows::decode_storage_record,
    session::{SessionEvent, SessionHeader, SessionId},
    session_store::SessionStore,
};
use seekdeep_session_persistence::SessionPersistence as _;
use seekdeep_session_persistence_jsonl::{JsonlConfig, JsonlSessionPersistence};

pub(super) fn run(source: &Path) -> anyhow::Result<()> {
    super::verify_source(source)?;
    let metadata = super::cargo_metadata()?;
    let host = metadata
        .target_directory
        .join("debug/examples/keyless_web_host");
    anyhow::ensure!(
        host.is_file(),
        "build the native fixture together with xtask: cargo build --locked -p xtask -p seekdeep --bin xtask --example keyless_web_host"
    );
    super::node_runtime::stage(&metadata, "debug")?;
    let temporary = tempfile::tempdir()?;
    let home = temporary.path().canonicalize()?;
    let workspace = home.join("fixture");
    std::fs::create_dir_all(workspace.join("workspace"))?;
    std::fs::write(workspace.join("workspace/a.txt"), "alpha\n")?;
    std::fs::write(workspace.join("workspace/b.txt"), "beta\n")?;
    let id = SessionId::new("workspace-management-web-e2e");
    let runtime = tokio::runtime::Runtime::new()?;
    let (seed, artifact) = runtime.block_on(seed_fixture(source, &home, &workspace, &id))?;
    let output = metadata.target_directory.join("xtask/web-workspaces");
    std::fs::create_dir_all(&output)?;
    std::fs::write(output.join("expected-session.jsonl"), &seed)?;
    let driver = output.join("browser.mjs");
    std::fs::write(&driver, super::web_workspaces_driver::DRIVER)?;
    let status = Command::new("node")
        .arg(driver)
        .arg(source)
        .arg(host)
        .arg(&home)
        .arg(&workspace)
        .arg(&output)
        .arg(id.as_str())
        .arg(&artifact)
        .current_dir(&metadata.workspace_root)
        .status()?;
    anyhow::ensure!(status.success(), "workspace-management browser path failed");
    let audit: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.join("model-call-audit.json"))?)?;
    anyhow::ensure!(audit["calls"] == 0, "keyless adapter observed a model call");
    runtime.block_on(audit_model_calls(&home, &seed))?;
    Ok(())
}

async fn seed_fixture(
    source: &Path,
    home: &Path,
    workspace: &Path,
    id: &SessionId,
) -> anyhow::Result<(String, PathBuf)> {
    let fixture =
        std::fs::read_to_string(source.join("apps/web/tests/snapshots/seeded-history/seed.jsonl"))?
            .replace("{{sessionId}}", id.as_str())
            .replace("{{cwd}}/workspace", &workspace.to_string_lossy())
            .replace("{{cwd}}", &workspace.to_string_lossy());
    let mut events = Vec::<SessionEvent>::new();
    for line in fixture.lines().skip(1) {
        for event in decode_storage_record(serde_json::from_str(line)?)? {
            events.push(serde_json::from_value(event)?);
        }
    }
    anyhow::ensure!(
        events
            .last()
            .is_some_and(|event| event.event_type == "turn/end"),
        "workspace seed must end in turn/end"
    );
    let context = seekdeep_cordis::Context::new();
    let persistence = JsonlSessionPersistence::new(
        SessionStore::install(&context)?,
        JsonlConfig::new(home.join("sessions")),
    )?;
    let result = async {
        let mut header = SessionHeader::new(id.clone());
        header.created_at = header.created_at.saturating_sub(60_000);
        header.cwd = Some(workspace.to_string_lossy().into_owned());
        header.delegation_depth = Some(0);
        persistence.create(&header).await?;
        persistence.append(id, &events).await?;
        let artifact = persistence
            .read_raw(id, None)
            .await?
            .ok_or_else(|| anyhow::anyhow!("seed has no raw artifact"))?;
        let location = persistence
            .locate(&header)
            .ok_or_else(|| anyhow::anyhow!("seed has no artifact location"))?;
        Ok((artifact.content, location.path))
    }
    .await;
    context.root_fiber().dispose().await?;
    result
}

async fn audit_model_calls(home: &Path, seed: &str) -> anyhow::Result<()> {
    let mut expected = 0;
    for line in seed.lines().skip(1) {
        for event in decode_storage_record(serde_json::from_str(line)?)? {
            if event["type"] == "request/header" {
                expected += 1;
            }
        }
    }
    let context = seekdeep_cordis::Context::new();
    let persistence = JsonlSessionPersistence::new(
        SessionStore::install(&context)?,
        JsonlConfig::new(home.join("sessions")),
    )?;
    let result = async {
        let headers = persistence.list(None).await?;
        let mut actual = 0;
        for header in &headers {
            actual += persistence.inspect(&header.id, None).await?.events.iter()
                .filter(|event| event.event_type == "request/header").count();
        }
        anyhow::ensure!(actual == expected, "workspace workflow produced model requests: expected {expected} recorded requests, got {actual}");
        println!("workspace cold audit: {} durable sessions, no model requests beyond the pinned seed", headers.len());
        Ok(())
    }.await;
    context.root_fiber().dispose().await?;
    result
}
