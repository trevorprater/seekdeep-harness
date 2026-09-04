//! Headless source-snapshot replays through the compiled Loader plugin catalog.

#![cfg(not(windows))]

#[path = "support/retry_snapshot_backend.rs"]
mod retry_backend;
#[path = "support/settlement_fence.rs"]
mod settlement_fence;

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use parking_lot::Mutex;
use seekdeep_acp_snapshot::{
    NormalizeContext, NormalizeOptions, normalize_session_log, normalize_stdout,
    scrub_request_headers,
};
use seekdeep_cordis::{Context, EventOptions, EventReply, Plugin, ServiceKey, fiber::EffectHandle};
use seekdeep_core::{
    session::{Session, SessionHeader},
    session_store::SessionStore,
};
use seekdeep_llm_replay::{ReplayConfig, ReplayHandle, install_llm_replay};
use seekdeep_loader::profile_patch::{
    ProfileEntry, ProfileEntryId, ProfileNode, ProfilePatch, apply_entry_patches,
    parse_entry_list_yaml, render_entry_list_yaml,
};
use seekdeep_loader::{ExpressionEnvironment, LoadedComposition, PluginCatalog};
use seekdeep_loader_smoke::{FixtureTurnOptions, FixtureTurnResult, run_fixture_turn};
use seekdeep_session_persistence::SessionPersistence as _;
use seekdeep_session_persistence_jsonl::{JsonlCompression, JsonlConfig, JsonlSessionPersistence};
use seekdeep_util::launch_environment::{
    LaunchEnvironmentLayerInput, LaunchEnvironmentSource, SEEKDEEP_LAUNCH_ENVIRONMENT,
    create_launch_environment_snapshot,
};
use serde_json::{Value, json};

const REPLAY: ServiceKey<ReplayHandle> = ServiceKey::new("headlessSnapshotReplay");

#[derive(Clone, Copy)]
enum Scenario {
    Advanced,
    Goal,
    Ralph,
    Pty,
    Compaction,
    Retry,
    MissingCredential,
    InvalidCredential,
    Settlement,
}

impl Scenario {
    fn name(self) -> &'static str {
        match self {
            Self::Advanced => "advanced-toolchain",
            Self::Goal => "goal-tools",
            Self::Ralph => "ralph-loop",
            Self::Pty => "pty-tools",
            Self::Compaction => "compaction-recovery",
            Self::Retry => "provider-retry",
            Self::MissingCredential => "missing-credential",
            Self::InvalidCredential => "invalid-credential",
            Self::Settlement => "subagent-settlement",
        }
    }
    fn config(self) -> &'static str {
        match self {
            Self::Advanced => "advanced.cordis.snapshot.yml",
            Self::Goal => "goal.cordis.snapshot.yml",
            Self::Ralph => "ralph.cordis.snapshot.yml",
            Self::Pty => "pty.cordis.snapshot.yml",
            Self::Compaction => "compaction.cordis.snapshot.yml",
            Self::Retry => "retry.cordis.snapshot.yml",
            Self::MissingCredential | Self::InvalidCredential => "credentials.cordis.snapshot.yml",
            Self::Settlement => "subagent-settlement.cordis.snapshot.yml",
        }
    }
    fn children(self) -> usize {
        match self {
            Self::Advanced | Self::Ralph => 2,
            Self::Settlement => 1,
            Self::Goal
            | Self::Pty
            | Self::Compaction
            | Self::Retry
            | Self::MissingCredential
            | Self::InvalidCredential => 0,
        }
    }

    fn uses_replay(self) -> bool {
        !matches!(
            self,
            Self::Retry | Self::MissingCredential | Self::InvalidCredential
        )
    }
}

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn replay_plugin(scenario: Scenario, fixtures: PathBuf) -> Plugin {
    Plugin::new(
        "headless-fixture-replay",
        ["llm"],
        move |context, config| {
            let fixtures = fixtures.clone();
            Box::pin(async move {
                let config: seekdeep_llm_replay::Config = serde_json::from_value(config)?;
                let handle = Arc::new(install_llm_replay(
                    &context,
                    ReplayConfig {
                        file: fixtures.join(if matches!(scenario, Scenario::Settlement) {
                            "parent.replay.jsonl"
                        } else {
                            "session.jsonl"
                        }),
                        override_file: if matches!(scenario, Scenario::Settlement) {
                            Some(fixtures.join("parent.override.json"))
                        } else {
                            matches!(scenario, Scenario::Goal | Scenario::Ralph)
                                .then(|| fixtures.join("replay.override.json"))
                        },
                        child_files: if matches!(scenario, Scenario::Settlement) {
                            vec![fixtures.join("child.replay.jsonl")]
                        } else {
                            (1..=scenario.children())
                                .map(|index| fixtures.join(format!("session.{index}.jsonl")))
                                .collect()
                        },
                        providers: config.providers.unwrap_or_default(),
                        pace_ms: 0.0,
                    },
                )?);
                context.provide(REPLAY, handle.clone())?;
                context.own(EffectHandle::new(
                    "headless replay registration",
                    move || Box::pin(async move { handle.dispose().await }),
                ))?;
                Ok(())
            })
        },
    )
}

fn catalog(
    scenario: Scenario,
    fixtures: &Path,
    workspace: &Path,
    home: &Path,
) -> anyhow::Result<PluginCatalog> {
    let catalog = PluginCatalog::new().with_expression_environment(ExpressionEnvironment::new(
        BTreeMap::from([
            (
                "SEEKDEEP_HOME".to_owned(),
                home.to_string_lossy().into_owned(),
            ),
            ("SEEKDEEP_SNAPSHOT".to_owned(), "replay".to_owned()),
        ]),
        workspace.to_owned(),
        std::env::current_exe()?,
        if cfg!(target_os = "macos") {
            "darwin"
        } else {
            "linux"
        },
        "v24.0.0",
        home.to_owned(),
    ));
    seekdeep_acp_demo::register_compiled_plugins(&catalog)?;
    for (name, plugin) in [
        (
            "seekdeep-agent-spine-demo",
            seekdeep_agent_spine_demo::plugin(),
        ),
        (
            "seekdeep-session-persistence-jsonl",
            seekdeep_session_persistence_jsonl::plugin(),
        ),
        (
            "seekdeep-session-checkpoint-policy",
            seekdeep_session_checkpoint_policy::plugin(),
        ),
        ("seekdeep-settings-file", seekdeep_settings_file::plugin()),
        (
            "seekdeep-credentials-local",
            seekdeep_credentials_local::plugin(),
        ),
        ("seekdeep-fs-local", seekdeep_fs_local::plugin()),
        ("seekdeep-goal", seekdeep_goal::plugin()),
        ("seekdeep-tool-goal", seekdeep_tool_goal::index::plugin()),
    ] {
        catalog.register_named(&format!("@seekdeep-ai/{name}"), plugin)?;
    }
    catalog.register_named(
        "headless-fixture-replay",
        replay_plugin(scenario, fixtures.to_owned()),
    )?;
    catalog.register_named(
        "./tests/fixtures/retry-snapshot-backend.mjs",
        retry_backend::plugin(),
    )?;
    catalog.register_named(
        "./tests/fixtures/subagent-settlement-fence.ts",
        settlement_fence::plugin(),
    )?;
    Ok(catalog)
}

fn isolated_config(
    scenario: Scenario,
    workspace: &Path,
    home: &Path,
    sessions: &Path,
) -> anyhow::Result<String> {
    let example = repository().join("examples/headless-agent");
    let original = std::fs::read_to_string(example.join(scenario.config()))?;
    let entries = parse_entry_list_yaml(&original)?;
    anyhow::ensure!(!entries.is_empty(), "snapshot must have an owning include");
    let patches = entries[0]
        .config()
        .and_then(ProfileNode::as_mapping)
        .and_then(|config| config.get("patches"))
        .and_then(ProfileNode::as_sequence)
        .ok_or_else(|| anyhow::anyhow!("missing include patches"))?
        .iter()
        .map(|patch| {
            patch
                .as_mapping()
                .cloned()
                .map(ProfilePatch::from_fields)
                .ok_or_else(|| anyhow::anyhow!("include patch is not a mapping"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let base = parse_entry_list_yaml(&std::fs::read_to_string(example.join("cordis.yml"))?)?;
    let composed = apply_entry_patches(&base, &patches)?;
    anyhow::ensure!(
        composed.warnings().is_empty(),
        "source include warnings: {:?}",
        composed.warnings()
    );
    // Source patches replace whole configs. Isolate the composed entries so agents,
    // model options and tool policy survive unchanged. Settings and skill roots are static.
    let overrides = parse_entry_list_yaml(&serde_json::to_string(&[
        json!({"id":"settings","config":{"path":home.join("settings.yaml"),"watch":false}}),
        json!({"id":"credentials","config":{"path":home.join(".credentials.yaml"),"watch":false}}),
        json!({"id":"agent-spine","config":{
            "seekdeepHome":home,
            "workspaceContext":{"maxBytes":65536,"seekdeepHome":home},
            "skills":{"filesystem":{"agentsHome":home.join("agents"),"watch":false}}
        }}),
        json!({"id":"persistence","config":{"root":sessions,"compression":"none"}}),
        json!({"id":"bash","config":{"cwd":workspace}}),
        json!({"id":"fs-local","config":{"cwd":workspace}}),
    ])?)?;
    let mut isolated = Vec::new();
    for entry in composed.entries() {
        let mut fields = entry.fields().clone();
        if let Some(overrides) = overrides
            .iter()
            .find(|overrides| overrides.id() == entry.id())
        {
            let overrides = overrides.config().unwrap().as_mapping().unwrap();
            let config = fields
                .entry("config".to_owned())
                .or_insert_with(|| ProfileNode::Mapping(overrides.clone()));
            let ProfileNode::Mapping(config) = config else {
                anyhow::bail!("fixture entry {:?} has non-mapping config", entry.id());
            };
            config.extend(overrides.clone());
        }
        let alias = match entry.id().as_ref().map(ProfileEntryId::as_str) {
            Some("llm-replay") => Some("headless-fixture-replay"),
            Some("pty-snapshot-backend") => Some("./pty-snapshot-backend.mjs"),
            _ => None,
        };
        if let Some(alias) = alias {
            fields.insert("name".to_owned(), ProfileNode::String(alias.to_owned()));
        }
        isolated.push(ProfileEntry::from_fields(fields));
    }
    isolated.extend_from_slice(&entries[1..]);
    Ok(render_entry_list_yaml(&isolated)?)
}

#[test]
fn fixture_isolation_preserves_agents_model_and_tool_policy() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let home = temporary.path().join("home");
    for scenario in [
        Scenario::Advanced,
        Scenario::Goal,
        Scenario::Ralph,
        Scenario::Pty,
        Scenario::Compaction,
        Scenario::Retry,
        Scenario::MissingCredential,
        Scenario::InvalidCredential,
        Scenario::Settlement,
    ] {
        let entries = parse_entry_list_yaml(&isolated_config(
            scenario,
            temporary.path(),
            &home,
            &temporary.path().join("sessions"),
        )?)?;
        let spine = entries
            .iter()
            .find(|entry| entry.id().unwrap().as_str() == "agent-spine")
            .unwrap();
        let config = spine.config().unwrap().as_mapping().unwrap();
        let agents = config["agents"].as_sequence().unwrap();
        assert_eq!(agents.len(), 1);
        let agent = agents[0].as_mapping().unwrap();
        assert_eq!(agent["id"].as_str(), Some("main"));
        assert_eq!(agent["model"].as_str(), Some("deepseek-v4-flash"));
        assert!(agent["cwd"].as_javascript().is_some());
        assert!(
            config["persona"]
                .as_str()
                .unwrap()
                .contains("headless-agent")
        );
        if matches!(scenario, Scenario::Advanced) {
            assert_eq!(
                config["tools"].as_mapping().unwrap()["mode"].as_str(),
                Some("both")
            );
        }
        if scenario.uses_replay() {
            assert!(
                entries
                    .iter()
                    .any(|entry| entry.name() == Some("headless-fixture-replay"))
            );
        }
    }
    Ok(())
}

struct Capture {
    result: FixtureTurnResult,
    events: Vec<Value>,
    logs: Vec<String>,
    context: NormalizeContext,
}

fn isolate_launch(context: &Context, scenario: Scenario, home: &Path) -> anyhow::Result<()> {
    let mut launch_values = BTreeMap::from([(
        "SEEKDEEP_HOME".to_owned(),
        home.to_string_lossy().into_owned(),
    )]);
    if matches!(
        scenario,
        Scenario::MissingCredential | Scenario::InvalidCredential
    ) {
        launch_values.insert(
            "DEEPSEEK_API_KEY".to_owned(),
            if matches!(scenario, Scenario::InvalidCredential) {
                "sk-😀pasted-from-a-chat-window".to_owned()
            } else {
                String::new()
            },
        );
    }
    context.provide(
        SEEKDEEP_LAUNCH_ENVIRONMENT,
        Arc::new(create_launch_environment_snapshot(&[
            LaunchEnvironmentLayerInput {
                source: LaunchEnvironmentSource::Process,
                path: None,
                values: launch_values,
            },
        ])),
    )?;
    Ok(())
}

fn observe_headers(context: &Context) -> anyhow::Result<Arc<Mutex<Vec<SessionHeader>>>> {
    let headers = Arc::new(Mutex::new(Vec::<SessionHeader>::new()));
    let captured = headers.clone();
    context.events().on_sync(
        context,
        "session/created",
        move |_, args| {
            captured
                .lock()
                .push(args.get::<Session>(0).unwrap().header().clone());
            Ok(EventReply::Undefined)
        },
        EventOptions {
            global: true,
            prepend: false,
        },
    )?;
    Ok(headers)
}

async fn load_composition(
    context: &Context,
    catalog: &PluginCatalog,
    source: &str,
    label: &str,
) -> anyhow::Result<LoadedComposition> {
    let loaded =
        tokio::time::timeout(Duration::from_secs(20), catalog.load_yaml(context, source)).await;
    match loaded {
        Ok(composition) => Ok(composition?),
        Err(error) => {
            let states = context
                .registry()
                .values()
                .into_iter()
                .flat_map(|runtime| {
                    runtime
                        .fibers
                        .iter()
                        .map(|fiber| {
                            format!(
                                "{}: {:?}, missing={:?}, error={:?}",
                                runtime.name,
                                fiber.fiber().state(),
                                fiber.missing_inject(),
                                fiber.error()
                            )
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            let cleanup =
                tokio::time::timeout(Duration::from_secs(10), context.root_fiber().dispose()).await;
            anyhow::bail!(
                "{label} Loader timed out: {error}; states={states:?}; cleanup={cleanup:?}"
            );
        }
    }
}

fn scenario_task(scenario: Scenario, fixtures: &Path) -> anyhow::Result<String> {
    Ok(
        if matches!(
            scenario,
            Scenario::MissingCredential | Scenario::InvalidCredential
        ) {
            "say pong".to_owned()
        } else if matches!(scenario, Scenario::Settlement) {
            "Start one continuable background subagent and answer from its completion notice. Do not call list_agents, send_message, job_output, or job_list.".to_owned()
        } else {
            let input: Value =
                serde_json::from_str(&std::fs::read_to_string(fixtures.join("input.json"))?)?;
            input["steps"]
                .as_array()
                .unwrap()
                .iter()
                .find(|step| step["op"] == "prompt")
                .and_then(|step| step["text"].as_str())
                .unwrap()
                .to_owned()
        },
    )
}

async fn execute_scenario(
    scenario: Scenario,
    fixtures: &Path,
    workspace: &Path,
    home: &Path,
    sessions: &Path,
) -> anyhow::Result<Capture> {
    let context = Context::new();
    isolate_launch(&context, scenario, home)?;
    let headers = observe_headers(&context)?;
    let composition = load_composition(
        &context,
        &catalog(scenario, fixtures, workspace, home)?,
        &isolated_config(scenario, workspace, home, sessions)?,
        scenario.name(),
    )
    .await?;
    let task = scenario_task(scenario, fixtures)?;
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed = events.clone();
    let outcome = tokio::time::timeout(
        Duration::from_secs(60),
        run_fixture_turn(
            &context,
            FixtureTurnOptions {
                task,
                on_event: Some(Arc::new(move |_, event| {
                    observed.lock().push(serde_json::to_value(event).unwrap());
                })),
            },
        ),
    )
    .await;
    let consumed = if matches!(scenario, Scenario::Retry) {
        context.get(retry_backend::PROBE).unwrap().assert_consumed()
    } else if scenario.uses_replay() {
        context.get(REPLAY).unwrap().assert_consumed()
    } else {
        Ok(())
    };
    let headers = headers.lock().clone();
    let cleanup = tokio::time::timeout(Duration::from_secs(10), composition.dispose()).await;
    tokio::time::timeout(Duration::from_secs(10), context.root_fiber().dispose()).await??;
    let result = outcome??;
    consumed?;
    cleanup??;
    let cold_context = Context::new();
    let mut cold_config = JsonlConfig::new(sessions);
    cold_config.compression = JsonlCompression::None;
    let cold = JsonlSessionPersistence::new(SessionStore::install(&cold_context)?, cold_config)?;
    let mut logs = Vec::new();
    for header in &headers {
        logs.push(
            cold.read_raw(&header.id, None)
                .await?
                .expect("persisted snapshot log")
                .content,
        );
    }
    let persisted = cold.inspect(&result.session_id, None).await;
    cold_context.root_fiber().dispose().await?;
    let events = events.lock().clone();
    let first = events.first().expect("owned event interval")["seq"]
        .as_u64()
        .unwrap();
    let last = events.last().unwrap()["seq"].as_u64().unwrap();
    let persisted = persisted?
        .events
        .into_iter()
        .filter(|event| (first..=last).contains(&event.seq))
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(
        persisted, events,
        "every streamed event must survive cold reopening"
    );
    Ok(Capture {
        result,
        events,
        logs,
        context: NormalizeContext {
            session_ids: headers
                .iter()
                .map(|header| header.id.as_str().to_owned())
                .collect(),
            cwd: workspace.to_string_lossy().into_owned(),
            cwd_aliases: Vec::new(),
        },
    })
}

fn normalize_goal_times(value: &mut Value) {
    match value {
        Value::Array(values) => values.iter_mut().for_each(normalize_goal_times),
        Value::Object(fields) => {
            for (key, value) in fields {
                if matches!(key.as_str(), "createdAt" | "updatedAt" | "clearedAt")
                    && value.is_number()
                {
                    *value = json!(0);
                } else {
                    normalize_goal_times(value);
                }
            }
        }
        Value::String(text) => {
            let pattern =
                regex::Regex::new(r#"("(?:createdAt|updatedAt|clearedAt)":)\d+"#).unwrap();
            *text = pattern.replace_all(text, "${1}0").into_owned();
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn normalized_stream(capture: &Capture, scenario: Scenario) -> anyhow::Result<String> {
    let raw = capture
        .events
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()?
        .join("\n");
    let normalized = scrub_request_headers(&normalize_session_log(
        &raw,
        &capture.context,
        NormalizeOptions::default(),
    )?)?;
    let mut records = normalized
        .lines()
        .map(|line| {
            Ok(json!({
                "type":"session_event", "sessionId":capture.result.session_id,
                "event":serde_json::from_str::<Value>(line)?
            }))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    records.push(serde_json::to_value(&capture.result)?);
    if matches!(scenario, Scenario::Goal) {
        records.iter_mut().for_each(normalize_goal_times);
    }
    let raw = records
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()?
        .join("\n");
    Ok(normalize_stdout(
        &raw,
        &capture.context,
        NormalizeOptions::default(),
    )?)
}

fn assert_snapshot(actual: &str, expected: &str, label: &str) {
    let actual = actual.lines().collect::<Vec<_>>();
    let expected = expected.lines().collect::<Vec<_>>();
    let differences = actual
        .iter()
        .zip(&expected)
        .enumerate()
        .filter(|(_, (actual, expected))| actual != expected)
        .map(|(index, (actual, expected))| {
            format!(
                "line {}:\nactual: {actual}\nexpected: {expected}",
                index + 1
            )
        })
        .collect::<Vec<_>>();
    assert!(
        differences.is_empty(),
        "{label}:\n{}",
        differences.join("\n")
    );
    assert_eq!(actual.len(), expected.len(), "{label} line count");
}

fn assert_ralph_logs(logs: &[String]) -> anyhow::Result<()> {
    let logs = logs
        .iter()
        .map(|log| {
            log.lines()
                .map(serde_json::from_str::<Value>)
                .collect::<Result<Vec<_>, _>>()
        })
        .collect::<Result<Vec<_>, _>>()?;
    let parent = &logs[0][0];
    assert_eq!(parent["delegationDepth"], 0);
    assert_ne!(logs[1][0]["id"], logs[2][0]["id"]);
    for (round, records) in logs[1..].iter().enumerate() {
        let child = &records[0];
        assert_eq!(child["parentSession"], parent["id"]);
        assert_eq!(child["cwd"], parent["cwd"]);
        assert_eq!(child["delegationDepth"], 1);
        assert!(child.get("seedLength").is_none());
        let prompt = records
            .iter()
            .find(|record| record["type"] == "user/message")
            .unwrap()["data"]["content"]
            .to_string();
        assert!(prompt.contains(&format!("Ralph round: {} of 2.", round + 1)));
        assert!(prompt.contains("Prove two fresh Ralph rounds through the shipped headless app."));
        assert!(!prompt.contains("Run a two-round fresh-agent Ralph loop"));
        if round == 0 {
            assert!(prompt.contains("(none — this is the first round)"));
            assert!(!prompt.contains("ROUND_ONE_HANDOFF"));
        } else {
            assert!(prompt.contains("ROUND_ONE_HANDOFF"));
        }
        let calls = records
            .iter()
            .filter(|record| record["type"] == "tool/call")
            .map(|record| record["data"]["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(calls, ["structured_output"]);
    }
    Ok(())
}

async fn run_scenario(scenario: Scenario) -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("workspace");
    let home = temporary.path().join("home");
    std::fs::create_dir(&workspace)?;
    std::fs::create_dir(&home)?;
    let fixtures = repository()
        .join("examples/headless-agent/tests/snapshots")
        .join(scenario.name());
    let capture = execute_scenario(
        scenario,
        &fixtures,
        &workspace,
        &home,
        &temporary.path().join("sessions"),
    )
    .await?;
    assert_eq!(capture.logs.len(), scenario.children() + 1);
    if matches!(scenario, Scenario::Ralph) {
        assert_ralph_logs(&capture.logs)?;
    }
    if matches!(scenario, Scenario::Settlement) {
        let child = scrub_request_headers(&normalize_session_log(
            &capture.logs[1],
            &capture.context,
            NormalizeOptions::default(),
        )?)?;
        assert_snapshot(
            &child,
            &std::fs::read_to_string(fixtures.join("child.expected.jsonl"))?,
            "settlement child",
        );
    }
    let expected = std::fs::read_to_string(fixtures.join("stream-json.expected.jsonl"))?;
    assert_snapshot(
        &normalized_stream(&capture, scenario)?,
        &expected,
        scenario.name(),
    );
    if matches!(
        scenario,
        Scenario::Advanced | Scenario::Pty | Scenario::Compaction
    ) {
        let expected_logs = (0..capture.logs.len())
            .map(|index| {
                std::fs::read_to_string(fixtures.join(if index == 0 {
                    "session.jsonl".to_owned()
                } else {
                    format!("session.{index}.jsonl")
                }))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let expected_context = NormalizeContext {
            session_ids: expected_logs
                .iter()
                .map(|log| {
                    serde_json::from_str::<Value>(log.lines().next().unwrap()).unwrap()["id"]
                        .as_str()
                        .unwrap()
                        .to_owned()
                })
                .collect(),
            cwd: "{{cwd}}".to_owned(),
            cwd_aliases: Vec::new(),
        };
        for (actual, expected) in capture.logs.iter().zip(expected_logs) {
            let actual = scrub_request_headers(&normalize_session_log(
                actual,
                &capture.context,
                NormalizeOptions::default(),
            )?)?;
            let expected = scrub_request_headers(&normalize_session_log(
                &expected,
                &expected_context,
                NormalizeOptions::default(),
            )?)?;
            assert_snapshot(&actual, &expected, scenario.name());
        }
    }
    Ok(())
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn thread_fallback_keeps_child_session_writers_alive() -> anyhow::Result<()> {
    let executable = std::env::current_exe()?;
    let probe_name = "headless-thread-fallback-probe";
    if executable.file_name() == Some(std::ffi::OsStr::new(probe_name)) {
        run_scenario(Scenario::Advanced).await?;
        return run_scenario(Scenario::Ralph).await;
    }
    // A sibling compiled helper selects process mode. The isolated executable
    // requires the thread fallback to persist the same complete child logs.
    let isolated = tempfile::tempdir()?;
    let probe = isolated.path().join(probe_name);
    std::fs::copy(executable, &probe)?;
    let mut command = tokio::process::Command::new(probe);
    command
        .args([
            "--exact",
            "thread_fallback_keeps_child_session_writers_alive",
            "--nocapture",
        ])
        .env_clear()
        .current_dir(isolated.path())
        .kill_on_drop(true);
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    let output = tokio::time::timeout(Duration::from_secs(30), command.output()).await??;
    anyhow::ensure!(
        output.status.success(),
        "thread fallback failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[tokio::test]
async fn advanced_toolchain_matches_source_stream_and_three_durable_logs() -> anyhow::Result<()> {
    run_scenario(Scenario::Advanced).await
}
#[tokio::test]
async fn goal_tools_match_source_stream_and_strict_missing_goal_failure() -> anyhow::Result<()> {
    run_scenario(Scenario::Goal).await
}
#[tokio::test]
async fn ralph_rounds_match_source_stream_with_fresh_child_logs() -> anyhow::Result<()> {
    run_scenario(Scenario::Ralph).await
}
#[tokio::test]
async fn persistent_pty_tools_match_source_stream_and_durable_log() -> anyhow::Result<()> {
    run_scenario(Scenario::Pty).await
}
#[tokio::test]
async fn overflow_compaction_matches_source_stream_and_repaired_history() -> anyhow::Result<()> {
    run_scenario(Scenario::Compaction).await
}

#[tokio::test]
async fn transient_retry_matches_source_stream_and_preserves_request_messages() -> anyhow::Result<()>
{
    run_scenario(Scenario::Retry).await
}

#[tokio::test]
async fn missing_credential_matches_the_complete_source_stream() -> anyhow::Result<()> {
    run_scenario(Scenario::MissingCredential).await
}

#[tokio::test]
async fn invalid_credential_matches_the_complete_source_stream() -> anyhow::Result<()> {
    run_scenario(Scenario::InvalidCredential).await
}

#[tokio::test]
async fn continuable_settlement_matches_source_parent_stream_and_child_log() -> anyhow::Result<()> {
    run_scenario(Scenario::Settlement).await
}
