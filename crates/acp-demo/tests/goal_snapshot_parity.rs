//! Goal-round and goal-wrap-up snapshots through the compiled ACP application.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use regex::Regex;
use seekdeep_acp_snapshot::{
    AgentUnderTest, InputScript, NormalizeContext, NormalizeOptions, RunOptions, SnapshotRunMode,
    normalize_session_log, normalize_stdout, run_scenario, scrub_request_headers,
};
use seekdeep_core::session::SessionEvent;
use seekdeep_goal::{GoalPhase, fold::fold_goal};
use serde_json::Value;

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn compiled_agent(root: &Path) -> AgentUnderTest {
    AgentUnderTest {
        source_bin: PathBuf::from(env!("CARGO_BIN_EXE_seekdeep-acp-demo")),
        library_bin: Some(PathBuf::from(env!("CARGO_BIN_EXE_seekdeep-acp-demo"))),
        config_path: root.join("examples/acp-agent/cordis.yml"),
        tsconfig_path: root.join("tsconfig.json"),
    }
}

struct GoalScenarioResult {
    events: Vec<SessionEvent>,
    stdout: String,
    session: String,
}

async fn run_goal_scenario(root: &Path, name: &str) -> GoalScenarioResult {
    let scenario = root
        .join("examples/acp-agent/tests/goal-snapshots")
        .join(name);
    let fixture_file = scenario.join("session.jsonl");
    let input: InputScript =
        serde_json::from_slice(&std::fs::read(scenario.join("input.json")).unwrap()).unwrap();
    let result = run_scenario(
        &input,
        RunOptions {
            agent: compiled_agent(root),
            mode: SnapshotRunMode::Replay,
            environment: BTreeMap::new(),
            fixture_file,
            override_file: Some(scenario.join("replay.override.json")),
            child_files: Vec::new(),
            workspace_dir: None,
            prepare_workspace: None,
            workspace_parent: None,
            config_path: Some(root.join("examples/acp-agent/cordis.yml")),
            artifact_mode: None,
        },
    )
    .await
    .unwrap();
    assert!(result.stderr.is_empty(), "{}", result.stderr);
    assert_eq!(result.session_logs.len(), 1);
    let log = &result.session_logs[0];
    let events = log
        .content
        .lines()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect::<Vec<_>>();
    let context = NormalizeContext {
        session_ids: result
            .session_id
            .iter()
            .map(|id| id.as_str().to_owned())
            .chain(result.session_logs.iter().map(|entry| entry.id.clone()))
            .collect(),
        cwd: result.cwd.to_string_lossy().into_owned(),
        cwd_aliases: result
            .cwd_aliases
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect(),
    };
    let stdout =
        normalize_stdout(&result.raw_stdout, &context, NormalizeOptions::default()).unwrap();
    let normalized =
        normalize_session_log(&log.content, &context, NormalizeOptions::default()).unwrap();
    let session = normalize_goal_log(&scrub_request_headers(&normalized).unwrap());
    if std::env::var("SEEKDEEP_SNAPSHOT").as_deref() == Ok("refresh") {
        std::fs::write(scenario.join("stdout.expected.jsonl"), &stdout).unwrap();
        std::fs::write(scenario.join("session.expected.jsonl"), &session).unwrap();
    }
    assert_eq!(
        stdout,
        std::fs::read_to_string(scenario.join("stdout.expected.jsonl")).unwrap()
    );
    assert_eq!(
        session,
        std::fs::read_to_string(scenario.join("session.expected.jsonl")).unwrap()
    );
    GoalScenarioResult {
        events,
        stdout,
        session,
    }
}

fn normalize_goal_log(content: &str) -> String {
    let timestamp = Regex::new(r#"("(?:createdAt|updatedAt|clearedAt)":)\d+"#).unwrap();
    let mut output = String::new();
    for line in content.lines().filter(|line| !line.trim().is_empty()) {
        let mut value: Value = serde_json::from_str(line).unwrap();
        normalize_goal_timestamps(&mut value, &timestamp);
        output.push_str(&serde_json::to_string(&value).unwrap());
        output.push('\n');
    }
    output
}

fn normalize_goal_timestamps(value: &mut Value, timestamp: &Regex) {
    match value {
        Value::String(text) => {
            *text = timestamp.replace_all(text, "${1}0").into_owned();
        }
        Value::Array(values) => {
            for value in values {
                normalize_goal_timestamps(value, timestamp);
            }
        }
        Value::Object(values) => {
            for (name, value) in values {
                if matches!(name.as_str(), "createdAt" | "updatedAt" | "clearedAt")
                    && value.is_number()
                {
                    *value = Value::from(0);
                } else {
                    normalize_goal_timestamps(value, timestamp);
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn tool_call_names(events: &[SessionEvent]) -> Vec<&str> {
    events
        .iter()
        .filter(|event| event.event_type == "tool/call")
        .filter_map(|event| event.data.get("name").and_then(Value::as_str))
        .collect()
}

#[tokio::test]
async fn compiled_goal_rounds_persist_pause_after_the_exact_automatic_budget() {
    let result = run_goal_scenario(&repository_root(), "goal-round-driver").await;
    assert!(!result.stdout.is_empty());
    assert!(!result.session.is_empty());
    assert_eq!(tool_call_names(&result.events), ["create_goal", "get_goal"]);
    let rounds = result
        .events
        .iter()
        .filter(|event| event.event_type == "user/message")
        .filter_map(|event| {
            let source = event.data.get("source")?;
            (source.get("kind").and_then(Value::as_str) == Some("goal"))
                .then(|| source.get("round").and_then(Value::as_u64))
                .flatten()
        })
        .collect::<Vec<_>>();
    assert_eq!(rounds, [1, 2]);
    let folded = fold_goal(&result.events).unwrap();
    let goal = folded.goal.unwrap();
    assert_eq!(
        goal.objective,
        "Finish the ACP goal-round-driver snapshot proof"
    );
    assert_eq!(goal.phase, GoalPhase::Paused);
    assert_eq!(goal.revision, 2);
    assert_eq!(goal.max_goal_rounds, 2);
    assert_eq!(folded.rounds_started, 2);
}

#[tokio::test]
async fn compiled_goal_completion_injects_one_wrap_up_and_closes_the_same_turn() {
    let result = run_goal_scenario(&repository_root(), "goal-wrapup").await;
    assert_eq!(
        tool_call_names(&result.events),
        ["create_goal", "update_goal"]
    );
    let folded = fold_goal(&result.events).unwrap();
    let goal = folded.goal.unwrap();
    assert_eq!(goal.objective, "Finish the ACP goal wrap-up snapshot proof");
    assert_eq!(goal.phase, GoalPhase::Complete);
    assert_eq!(goal.revision, 2);
    assert_eq!(folded.rounds_started, 1);

    let wrapups = result
        .events
        .iter()
        .filter(|event| {
            event.event_type == "user/message"
                && event.data.pointer("/source/kind").and_then(Value::as_str) == Some("plugin")
                && event.data.pointer("/source/plugin").and_then(Value::as_str) == Some("tool-goal")
        })
        .collect::<Vec<_>>();
    assert_eq!(wrapups.len(), 1);
    assert!(
        wrapups[0].data["content"]
            .to_string()
            .contains("<goal_complete>")
    );

    let closing = result
        .events
        .iter()
        .filter(|event| event.event_type == "assistant/message")
        .filter_map(|event| {
            event
                .data
                .pointer("/message/content")
                .and_then(Value::as_array)
        })
        .flatten()
        .filter(|block| {
            block.get("type").and_then(Value::as_str) == Some("text")
                && block
                    .get("text")
                    .and_then(Value::as_str)
                    .is_some_and(|text| text.starts_with("GOAL WRAP-UP"))
        })
        .count();
    assert_eq!(closing, 1);
    let turn_two_ends = result
        .events
        .iter()
        .filter(|event| {
            event.event_type == "turn/end"
                && event.data.get("turn").and_then(Value::as_u64) == Some(2)
        })
        .collect::<Vec<_>>();
    assert_eq!(turn_two_ends.len(), 1);
    assert_eq!(
        turn_two_ends[0]
            .data
            .pointer("/reason/kind")
            .and_then(Value::as_str),
        Some("completed")
    );
}
