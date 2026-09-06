//! Recorded model exchange through the production Web profile, browser, and durable log.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use seekdeep::profile_boot::{
    boot_profile, compose_profile_at, framework_profile_catalog, shipped_preset_root,
};
use seekdeep_app_boot::BootPrepare;
use seekdeep_cmdline::{CmdlineHost, provide_cmdline};
use seekdeep_cordis::{Context, EventOptions, EventReply};
use seekdeep_core::{
    session::{Session, SessionHeader},
    session_store::SessionStore,
};
use seekdeep_host_webserver::WEB_SERVER;
use seekdeep_llm_replay::{
    ReplayConfig, ReplayProviderConfig, install_llm_replay, parse_session_log,
};
use seekdeep_session_persistence::SessionPersistence as _;
use seekdeep_session_persistence_jsonl::{JsonlConfig, JsonlSessionPersistence};
use seekdeep_typert_loader::TypertArtifactRegistry;
use seekdeep_util::launch_environment::{
    LaunchEnvironmentLayerInput, LaunchEnvironmentSnapshot, LaunchEnvironmentSource,
    SEEKDEEP_LAUNCH_ENVIRONMENT, create_launch_environment_snapshot,
};
use serde_json::json;

#[path = "support/web_replay_browser.rs"]
mod browser_driver;

const PROMPT: &str = "Reply with the single word LIGHTHOUSE and stop.";

struct WorkingDirectory(PathBuf);

impl WorkingDirectory {
    fn enter(path: &Path) -> std::io::Result<Self> {
        let previous = std::env::current_dir()?;
        std::env::set_current_dir(path)?;
        Ok(Self(previous))
    }
}

impl Drop for WorkingDirectory {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.0).expect("restore browser fixture working directory");
    }
}

fn pinned_source(repository: &Path) -> anyhow::Result<PathBuf> {
    let source = std::env::var_os("SEEKDEEP_PARITY_SOURCE").map_or_else(
        || PathBuf::from("/Users/trevor/ws/deepseek-harness"),
        PathBuf::from,
    );
    let snapshot = std::fs::read_to_string(repository.join("SOURCE_SNAPSHOT"))?;
    let commit = snapshot
        .lines()
        .find_map(|line| line.strip_prefix("commit="))
        .ok_or_else(|| anyhow::anyhow!("SOURCE_SNAPSHOT has no commit"))?;
    let head = std::process::Command::new("git")
        .arg("-C")
        .arg(&source)
        .args(["rev-parse", "HEAD"])
        .output()?;
    anyhow::ensure!(
        head.status.success() && String::from_utf8_lossy(&head.stdout).trim() == commit,
        "browser oracle is not at SOURCE_SNAPSHOT"
    );
    Ok(source)
}

fn artifact_directory(repository: &Path) -> anyhow::Result<PathBuf> {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map_or_else(|| repository.join("target"), PathBuf::from);
    let output = if target.is_absolute() {
        target
    } else {
        repository.join(target)
    }
    .join("xtask/web-replay");
    std::fs::create_dir_all(&output)?;
    std::fs::write(output.join("browser.mjs"), browser_driver::DRIVER)?;
    Ok(output)
}

fn replay_config(source: &Path) -> anyhow::Result<ReplayConfig> {
    let file = source.join("apps/web/tests/snapshots/feedback-command/session.jsonl");
    let events = parse_session_log(&std::fs::read_to_string(&file)?)?;
    let prompts = events
        .iter()
        .filter(|event| event.event_type == "user/message")
        .map(|event| {
            event.data["content"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("source user message has no content"))
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter_map(|block| block["text"].as_str())
                        .collect::<String>()
                })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    anyhow::ensure!(
        prompts == [PROMPT],
        "source fixture prompt inventory changed"
    );
    Ok(ReplayConfig {
        file,
        override_file: None,
        child_files: Vec::new(),
        providers: serde_json::from_value::<Vec<ReplayProviderConfig>>(json!([{
            "id":"deepseek-official","name":"DeepSeek",
            "models":[{"id":"deepseek-v4-flash","name":"DeepSeek-V4-Flash","contextWindow":128_000}]
        }]))?,
        pace_ms: 100.0,
    })
}

fn isolated_environment(home: &Path) -> LaunchEnvironmentSnapshot {
    create_launch_environment_snapshot(&[LaunchEnvironmentLayerInput {
        source: LaunchEnvironmentSource::Process,
        path: None,
        values: BTreeMap::from([
            (
                "SEEKDEEP_HOME".to_owned(),
                home.to_string_lossy().into_owned(),
            ),
            (
                "SEEKDEEP_AGENTS_HOME".to_owned(),
                home.join("agents").to_string_lossy().into_owned(),
            ),
            ("SEEKDEEP_TELEMETRY_DISABLED".to_owned(), "1".to_owned()),
        ]),
    }])
}

fn write_overlay(directory: &Path, home: &Path) -> anyhow::Result<PathBuf> {
    let overlay = directory.join("browser.patch.yml");
    std::fs::write(
        &overlay,
        serde_json::to_string_pretty(&json!([
            {"id":"webserver","config":{"host":"127.0.0.1","port":0}},
            {"id":"web-runtime","config":{"printUrl":false,"surfaceContext":true}},
            {"id":"llm-deepseek","disabled":true},
            {"id":"agent-instructions","disabled":true},
            {"id":"session-title-llm","disabled":true},
            {"id":"settings","config":{"seekdeepHome":home}},
            {"id":"credentials","config":{"seekdeepHome":home}},
            {"id":"storage-json","config":{"root":home.join("storages")}},
            {"id":"session-persistence-jsonl","config":{"root":home.join("sessions")}},
            {"id":"agent-presets","config":{"default":"standard","roots":[{"path":shipped_preset_root(),"trust":"system"}],"includeUserRoot":false}},
            {"id":"skill-filesystem","config":{"seekdeepHome":home,"agentsHome":home.join("agents"),"bundledSkillDir":home.join("bundled-skills"),"watch":false}},
            {"id":"directory-picker","disabled":true},
            {"insert":[
                {"id":"directory-picker-browse","name":"@seekdeep-ai/seekdeep-host-directory-picker-browse"},
                {"id":"ui-directory-picker-browse","name":"@seekdeep-ai/seekdeep-client-ui-directory-picker-browse"}
            ]}
        ]))?,
    )?;
    Ok(overlay)
}

fn prepare_profile(
    environment: LaunchEnvironmentSnapshot,
    headers: &Arc<Mutex<Vec<SessionHeader>>>,
) -> BootPrepare {
    let captured = headers.clone();
    Arc::new(move |context| {
        let environment = environment.clone();
        let captured = captured.clone();
        Box::pin(async move {
            context.provide(SEEKDEEP_LAUNCH_ENVIRONMENT, Arc::new(environment))?;
            TypertArtifactRegistry::install(&context)?;
            provide_cmdline(
                &context,
                CmdlineHost::new(Vec::<String>::new(), |code| {
                    anyhow::bail!("Web fixture requested unexpected exit {code}")
                }),
            )?;
            context.events().on_sync(
                &context,
                "session/created",
                move |_, args| {
                    let session = args
                        .get::<Session>(0)
                        .ok_or_else(|| anyhow::anyhow!("session/created has no Session"))?;
                    captured
                        .lock()
                        .expect("Session headers")
                        .push(session.header().clone());
                    Ok(EventReply::Undefined)
                },
                EventOptions {
                    global: true,
                    prepend: false,
                },
            )?;
            Ok(())
        })
    })
}

async fn audit_cold_log(
    home: &Path,
    output: &Path,
    headers: &Mutex<Vec<SessionHeader>>,
) -> anyhow::Result<()> {
    let cold_context = Context::new();
    let cold = JsonlSessionPersistence::new(
        SessionStore::install(&cold_context)?,
        JsonlConfig::new(home.join("sessions")),
    )?;
    let headers = headers.lock().expect("Session headers").clone();
    let audit = async {
        let mut driven = 0;
        for header in headers {
            let Some(raw) = cold.read_raw(&header.id, None).await? else {
                continue;
            };
            let events = parse_session_log(&raw.content)?;
            if !events
                .iter()
                .any(|event| event.event_type == "user/message")
            {
                continue;
            }
            driven += 1;
            anyhow::ensure!(
                events
                    .iter()
                    .filter(|event| event.event_type == "turn/start")
                    .count()
                    == 1,
                "expected one active turn"
            );
            anyhow::ensure!(
                events
                    .iter()
                    .filter(|event| event.event_type == "turn/end")
                    .count()
                    == 1,
                "active turn did not settle durably"
            );
            anyhow::ensure!(
                events.iter().any(|event| event.event_type == "turn/end"
                    && event.data["reason"]["kind"].as_str() == Some("completed")),
                "active turn ended without successful completion"
            );
            anyhow::ensure!(
                events
                    .iter()
                    .any(|event| event.event_type == "assistant/message"
                        && event.data["message"]["content"]
                            .as_array()
                            .is_some_and(|blocks| blocks
                                .iter()
                                .any(|block| block["type"].as_str() == Some("text")
                                    && block["text"].as_str() == Some("LIGHTHOUSE")))),
                "final assistant message is absent from the durable log"
            );
            anyhow::ensure!(
                raw.content.contains(PROMPT),
                "durable prompt differs from the source fixture"
            );
            std::fs::write(output.join("session.jsonl"), raw.content)?;
        }
        anyhow::ensure!(driven == 1, "expected one driven Session, got {driven}");
        Ok::<_, anyhow::Error>(())
    }
    .await;
    cold_context.root_fiber().dispose().await?;
    audit?;
    Ok(())
}

// A dedicated executable gives the fixture exclusive ownership of process cwd.
#[tokio::test]
#[ignore = "requires built Web/Client bundles and the pinned source's installed Playwright"]
async fn active_turn_streams_and_survives_browser_reload() -> anyhow::Result<()> {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let source = pinned_source(&repository)?;
    let replay_config = replay_config(&source)?;
    anyhow::ensure!(
        repository.join("apps/web/dist/index.html").is_file(),
        "run cargo xtask web-build first"
    );
    let output = artifact_directory(&repository)?;
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("world");
    let home = temporary.path().join("home");
    std::fs::create_dir_all(&workspace)?;
    std::fs::create_dir_all(&home)?;
    let workspace = workspace.canonicalize()?;
    let _cwd = WorkingDirectory::enter(&workspace)?;
    let environment = isolated_environment(&home);
    let catalog = framework_profile_catalog(&workspace, &home, &environment)?;
    let overlay = write_overlay(temporary.path(), &home)?;
    let plan = compose_profile_at(
        "web",
        &[overlay],
        &workspace,
        &home,
        &home.join("profiles/.seekdeep-installation/package.json"),
        &shipped_preset_root(),
        Some("1"),
    )?;
    let headers = Arc::new(Mutex::new(Vec::<SessionHeader>::new()));
    let prepare = prepare_profile(environment, &headers);
    let application = boot_profile(plan, &catalog, Some(prepare)).await?;
    let replay = install_llm_replay(application.context(), replay_config);
    let replay = match replay {
        Ok(replay) => replay,
        Err(error) => {
            application.dispose().await?;
            return Err(error);
        }
    };
    let run = async {
        let server = application
            .context()
            .get(WEB_SERVER)
            .ok_or_else(|| anyhow::anyhow!("Web profile did not publish webServer"))?;
        let status = tokio::time::timeout(
            Duration::from_secs(120),
            tokio::process::Command::new("node")
                .arg(output.join("browser.mjs"))
                .arg(format!("http://127.0.0.1:{}", server.port()))
                .arg(&source)
                .arg(&output)
                .arg(&workspace)
                .arg(PROMPT)
                .kill_on_drop(true)
                .status(),
        )
        .await??;
        anyhow::ensure!(status.success(), "active Web replay browser failed");
        replay.assert_consumed()?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let replay_cleanup = replay.dispose().await;
    let cleanup = tokio::time::timeout(Duration::from_secs(20), application.dispose()).await;
    run?;
    replay_cleanup?;
    cleanup??;
    audit_cold_log(&home, &output, &headers).await?;
    println!(
        "active Web replay: streamed in Chromium, fixture fully consumed, cold Session log verified"
    );
    Ok(())
}
