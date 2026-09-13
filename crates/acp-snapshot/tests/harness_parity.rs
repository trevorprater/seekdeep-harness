//! Data-driven scenario, durable-wait, harvest, workspace, and cleanup parity.

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use seekdeep_acp::AcpSessionId;
use seekdeep_acp_snapshot::{
    AgentUnderTest, GoalPhase, InputScript, InputStep, PermissionAnswer, PermissionAnswerKind,
    RunOptions, SnapshotPlatform, SnapshotRunMode, WaitForFile, harvest_session_logs,
    has_closed_turn, has_request_header_after_descriptor, latest_event_follows_turn_end,
    latest_open_turn, latest_title_follows_turn_end, latest_turn_is_closed, run_scenario,
    snapshot_spill_root, wait_for_persisted_child_turn_end,
    wait_for_persisted_event_after_turn_end, wait_for_persisted_goal_phase,
    wait_for_persisted_inbox_message, wait_for_persisted_title_after_turn_end,
    wait_for_persisted_turn_end, wait_for_persisted_turn_start, wait_for_workspace_file,
};
use serde_json::{Value, json};

fn fixture_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_acp-snapshot-launcher-fixture"))
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/test-support/acp-snapshot/tests/fixtures/suite")
}

fn options(fixture_file: PathBuf) -> RunOptions {
    RunOptions {
        agent: AgentUnderTest {
            source_bin: fixture_binary(),
            library_bin: Some(fixture_binary()),
            config_path: fixture_file.with_file_name("cordis.yml"),
            tsconfig_path: fixture_file.with_file_name("tsconfig.json"),
        },
        mode: SnapshotRunMode::Replay,
        environment: BTreeMap::new(),
        fixture_file,
        override_file: None,
        child_files: Vec::new(),
        workspace_dir: None,
        prepare_workspace: None,
        workspace_parent: None,
        config_path: None,
        artifact_mode: None,
    }
}

fn input(name: &str) -> InputScript {
    serde_json::from_slice(&fs::read(fixture_root().join(name).join("input.json")).unwrap())
        .unwrap()
}

fn fixture_options(name: &str) -> RunOptions {
    let scenario = fixture_root().join(name);
    let mut options = options(scenario.join("session.jsonl"));
    let workspace = scenario.join("workspace");
    options.workspace_dir = workspace.is_dir().then_some(workspace);
    options
}

fn write_behavior(root: &Path, value: &Value) -> PathBuf {
    fs::write(
        root.join("behavior.json"),
        serde_json::to_vec(value).unwrap(),
    )
    .unwrap();
    root.join("session.jsonl")
}

#[test]
fn input_scripts_deserialize_every_closed_operation_shape() {
    let script: InputScript = serde_json::from_value(json!({
        "permissionAnswers":[{"kind":"allow_once"}],
        "steps":[
            {"op":"initialize"},
            {"op":"newSession"},
            {"op":"newSessionExpectError","additionalDirectories":["/x"]},
            {"op":"prompt","text":"a"},
            {"op":"promptAndWaitForAgentMessage","text":"a","waitForText":"b"},
            {"op":"promptExpectError","text":"a"},
            {"op":"promptAndCancel","text":"a","waitForFile":{"path":"ready","timeoutMs":5}},
            {"op":"waitForFile","path":"ready","timeoutMs":5},
            {"op":"waitForTurnStart","minimumTurn":2,"timeoutMs":5},
            {"op":"waitForTurnEnd","timeoutMs":5},
            {"op":"waitForSubagentTurnEnd","child":2,"minimumTurn":3,"timeoutMs":5},
            {"op":"waitForGoalPhase","phase":"paused","timeoutMs":5},
            {"op":"waitForInboxMessage","text":"marker","timeoutMs":5},
            {"op":"waitForTitleAfterTurnEnd","timeoutMs":5},
            {"op":"waitForEventAfterTurnEnd","type":"goal/change","timeoutMs":5},
            {"op":"cancel","waitForFile":{"path":"ready"}}
        ]
    }))
    .unwrap();
    assert_eq!(script.steps.len(), 16);
    assert_eq!(
        script.permission_answers[0].kind,
        PermissionAnswerKind::AllowOnce
    );
    assert!(
        serde_json::from_value::<InputScript>(json!({"steps":[{"op":"reticulate"}]}))
            .unwrap_err()
            .to_string()
            .contains("unknown variant")
    );
}

#[test]
fn spill_roots_are_scenario_keyed_fixed_length_and_platform_adjusted() {
    let fixture = Path::new("/repo/snapshots/text-turn/session.jsonl");
    let other = Path::new("/repo/snapshots/other-turn/session.jsonl");
    let posix = snapshot_spill_root(fixture, SnapshotPlatform::Other);
    let windows = snapshot_spill_root(fixture, SnapshotPlatform::Windows);
    assert!(
        posix
            .to_string_lossy()
            .starts_with("/tmp/seekdeep-acp-snap-")
    );
    assert!(
        windows
            .to_string_lossy()
            .starts_with("/t/seekdeep-acp-snap-")
    );
    assert_eq!(
        posix.to_string_lossy().len(),
        windows.to_string_lossy().len() + 2
    );
    assert_eq!(posix, snapshot_spill_root(fixture, SnapshotPlatform::Other));
    assert_ne!(posix, snapshot_spill_root(other, SnapshotPlatform::Other));
}

#[tokio::test]
async fn scenario_runs_existing_plain_fixture_with_workspace_and_child_harvest() {
    let result = run_scenario(&input("plain-turn"), fixture_options("plain-turn"))
        .await
        .unwrap();
    assert!(result.raw_stdout.contains("workspace:seed.txt"));
    assert_eq!(result.session_logs.len(), 2);
    assert!(result.session_logs[0].parent_session.is_none());
    assert_eq!(
        result.session_logs[1].parent_session.as_deref(),
        result.session_id.as_ref().map(AcpSessionId::as_str)
    );
    assert!(
        result.session_logs[0]
            .content
            .contains(&result.cwd.to_string_lossy().to_string())
    );
    assert!(!result.cwd.exists());
    assert!(!result.cwd_aliases.is_empty());
}

#[tokio::test]
async fn no_model_and_expected_prompt_error_steps_settle_without_live_resources() {
    let no_model = run_scenario(&input("no-model"), fixture_options("no-model"))
        .await
        .unwrap();
    assert!(no_model.session_id.is_none());
    assert!(no_model.session_logs.is_empty());
    let failed = run_scenario(&input("authored-error"), fixture_options("authored-error"))
        .await
        .unwrap();
    assert_eq!(failed.session_logs.len(), 1);
    assert!(failed.raw_stdout.contains("model exploded"));
}

#[tokio::test]
async fn workspace_prepare_runs_after_committed_seed_under_an_explicit_parent() {
    let parent = tempfile::tempdir().unwrap();
    let mut options = fixture_options("plain-turn");
    options.workspace_parent = Some(parent.path().to_owned());
    options.prepare_workspace = Some(Arc::new(|cwd| {
        Box::pin(async move {
            assert!(cwd.join("seed.txt").is_file());
            tokio::fs::write(cwd.join("prepared.txt"), "ready").await?;
            Ok(())
        })
    }));
    let result = run_scenario(&input("plain-turn"), options).await.unwrap();
    assert!(result.cwd.starts_with(parent.path()));
    assert!(
        result
            .raw_stdout
            .contains("workspace:prepared.txt,seed.txt")
    );
    assert!(parent.path().is_dir());
    assert!(!result.cwd.exists());
}

#[tokio::test]
async fn permission_answers_are_fifo_fail_closed_and_reject_impossible_kinds() {
    let fixture = tempfile::tempdir().unwrap();
    let fixture_file = write_behavior(fixture.path(), &json!({"permissionProbe":true}));
    let script = InputScript {
        steps: vec![
            InputStep::Initialize,
            InputStep::NewSession,
            InputStep::Prompt {
                text: "permission".to_owned(),
            },
            InputStep::Prompt {
                text: "permission again".to_owned(),
            },
        ],
        permission_answers: vec![PermissionAnswer {
            kind: PermissionAnswerKind::RejectOnce,
        }],
    };
    let result = run_scenario(&script, options(fixture_file.clone()))
        .await
        .unwrap();
    assert!(result.raw_stdout.contains("opt-reject"));
    assert!(
        result
            .raw_stdout
            .contains("permission:{\\\"outcome\\\":\\\"cancelled\\\"}")
    );
    assert!(result.raw_stdout.contains("\"id\":1000"));
    assert!(result.raw_stdout.contains("\"id\":1001"));

    let impossible = InputScript {
        permission_answers: vec![PermissionAnswer {
            kind: PermissionAnswerKind::AllowAlways,
        }],
        ..script
    };
    let error = run_scenario(&impossible, options(fixture_file))
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("allow_always not among the offered options")
    );
}

#[tokio::test]
async fn prompt_waiter_and_scenario_environment_are_armed_before_the_matching_update() {
    let fixture = tempfile::tempdir().unwrap();
    let fixture_file = write_behavior(fixture.path(), &json!({"echoEnv":true}));
    let override_file = fixture.path().join("replay.override.json");
    let child_files = [
        fixture.path().join("session.1.jsonl"),
        fixture.path().join("session.2.jsonl"),
    ];
    let mut run_options = options(fixture_file.clone());
    run_options
        .environment
        .insert("SEEKDEEP_PERMISSION_MODE".into(), "layered".into());
    run_options.override_file = Some(override_file.clone());
    run_options.child_files = child_files.to_vec();
    let script = InputScript {
        steps: vec![
            InputStep::Initialize,
            InputStep::NewSession,
            InputStep::PromptAndWaitForAgentMessage {
                text: "go".to_owned(),
                wait_for_text: "thinking about it".to_owned(),
            },
        ],
        permission_answers: Vec::new(),
    };
    let result = run_scenario(&script, run_options).await.unwrap();
    let expected_children = std::env::join_paths(child_files)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let spill_root = snapshot_spill_root(&fixture_file, SnapshotPlatform::Other)
        .to_string_lossy()
        .into_owned();
    let environment = result
        .raw_stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|frame| {
            frame
                .pointer("/params/update/content/text")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .find(|text| text.starts_with("env:"))
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&environment["env:".len()..]).unwrap(),
        json!({
            "mode":"replay",
            "override":override_file.to_string_lossy(),
            "childFiles":expected_children,
            "spillRoot":spill_root,
            "permissionMode":"layered"
        })
    );
}

#[tokio::test]
async fn session_bound_steps_fail_before_any_session_is_created() {
    let fixture = tempfile::tempdir().unwrap();
    let fixture_file = write_behavior(fixture.path(), &json!({}));
    let cases = [
        (InputStep::Prompt { text: "x".into() }, "prompt"),
        (
            InputStep::PromptAndWaitForAgentMessage {
                text: "x".into(),
                wait_for_text: "later".into(),
            },
            "promptAndWaitForAgentMessage",
        ),
        (
            InputStep::PromptExpectError { text: "x".into() },
            "promptExpectError",
        ),
        (
            InputStep::PromptAndCancel {
                text: "x".into(),
                wait_for_file: None,
            },
            "promptAndCancel",
        ),
        (
            InputStep::WaitForTurnStart {
                minimum_turn: None,
                timeout_ms: None,
            },
            "waitForTurnStart",
        ),
        (
            InputStep::WaitForTurnEnd { timeout_ms: None },
            "waitForTurnEnd",
        ),
        (
            InputStep::WaitForGoalPhase {
                phase: GoalPhase::Active,
                timeout_ms: None,
            },
            "waitForGoalPhase",
        ),
        (
            InputStep::WaitForInboxMessage {
                text: "marker".into(),
                timeout_ms: None,
            },
            "waitForInboxMessage",
        ),
        (
            InputStep::WaitForTitleAfterTurnEnd { timeout_ms: None },
            "waitForTitleAfterTurnEnd",
        ),
        (
            InputStep::WaitForEventAfterTurnEnd {
                event_type: "user/message".into(),
                timeout_ms: None,
            },
            "waitForEventAfterTurnEnd",
        ),
        (
            InputStep::Cancel {
                wait_for_file: None,
            },
            "cancel",
        ),
    ];
    for (step, operation) in cases {
        let script = InputScript {
            steps: vec![InputStep::Initialize, step],
            permission_answers: Vec::new(),
        };
        let error = run_scenario(&script, options(fixture_file.clone()))
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains(&format!("{operation} before newSession")),
            "unexpected {operation} error: {error}"
        );
    }
}

#[tokio::test]
async fn expected_new_session_and_prompt_errors_fail_when_the_rpc_succeeds() {
    let fixture = tempfile::tempdir().unwrap();
    let fixture_file = write_behavior(fixture.path(), &json!({}));
    let new_session = InputScript {
        steps: vec![
            InputStep::Initialize,
            InputStep::NewSessionExpectError {
                additional_directories: None,
            },
        ],
        permission_answers: Vec::new(),
    };
    assert!(
        run_scenario(&new_session, options(fixture_file.clone()))
            .await
            .unwrap_err()
            .to_string()
            .contains("expected session/new to be rejected")
    );
    let prompt = InputScript {
        steps: vec![
            InputStep::Initialize,
            InputStep::NewSession,
            InputStep::PromptExpectError {
                text: "ok".to_owned(),
            },
        ],
        permission_answers: Vec::new(),
    };
    assert!(
        run_scenario(&prompt, options(fixture_file))
            .await
            .unwrap_err()
            .to_string()
            .contains("expected the prompt to fail")
    );
}

#[tokio::test]
async fn expected_new_session_errors_accept_rejected_base_and_extra_directory_calls() {
    for (behavior, additional_directories) in [
        (json!({"rejectNewSession":true}), None),
        (
            json!({"rejectExtraDirs":true}),
            Some(vec!["/outside".to_owned()]),
        ),
    ] {
        let fixture = tempfile::tempdir().unwrap();
        let fixture_file = write_behavior(fixture.path(), &behavior);
        let script = InputScript {
            steps: vec![
                InputStep::Initialize,
                InputStep::NewSessionExpectError {
                    additional_directories,
                },
            ],
            permission_answers: Vec::new(),
        };
        run_scenario(&script, options(fixture_file)).await.unwrap();
    }
}

#[tokio::test]
async fn prompt_and_cancel_waits_for_durable_start_then_harvests_final_logs() {
    let fixture = tempfile::tempdir().unwrap();
    let fixture_file = write_behavior(
        fixture.path(),
        &json!({
            "prompt":"hang-until-cancel",
            "persistLogsOnCancel":true,
            "logs":[{"file":"bucket/main/session.jsonl","lines":[
                {"type":"session","id":"{{SID}}","createdAt":1,"cwd":"{{CWD}}"},
                {"type":"turn/start","seq":0,"time":1,"data":{"turn":1}},
                {"type":"turn/end","seq":1,"time":2,"data":{"turn":1,"kind":"aborted"}}
            ]}]
        }),
    );
    let script = InputScript {
        steps: vec![
            InputStep::Initialize,
            InputStep::NewSession,
            InputStep::PromptAndCancel {
                text: "hang".to_owned(),
                wait_for_file: None,
            },
            InputStep::WaitForTurnEnd {
                timeout_ms: Some(1_000),
            },
        ],
        permission_answers: Vec::new(),
    };
    let result = run_scenario(&script, options(fixture_file)).await.unwrap();
    assert_eq!(result.session_logs.len(), 1);
    assert!(latest_turn_is_closed(&result.session_logs[0].content));
    assert!(result.raw_stdout.contains("cancelled"));
}

#[tokio::test]
async fn standalone_file_wait_holds_later_steps_until_prepare_marker_exists() {
    let fixture = tempfile::tempdir().unwrap();
    let fixture_file = write_behavior(fixture.path(), &json!({}));
    let mut options = options(fixture_file);
    options.prepare_workspace = Some(Arc::new(|cwd| {
        Box::pin(async move {
            let marker = cwd.join("ready");
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                tokio::fs::write(marker, "ready").await.unwrap();
            });
            Ok(())
        })
    }));
    let script = InputScript {
        steps: vec![
            InputStep::Initialize,
            InputStep::WaitForFile {
                path: "ready".into(),
                timeout_ms: Some(1_000),
            },
        ],
        permission_answers: Vec::new(),
    };
    run_scenario(&script, options).await.unwrap();
}

#[test]
fn complete_boundary_helpers_ignore_partial_tails_and_validate_turn_numbers() {
    let open = concat!(
        "{\"type\":\"session\"}\n",
        "{\"type\":\"turn/start\",\"data\":{\"turn\":2}}\n"
    );
    assert_eq!(latest_open_turn(open).unwrap(), Some(2));
    assert_eq!(
        latest_open_turn(
            "{\"type\":\"session\"}\n{\"type\":\"turn/start\",\"data\":{\"turn\":2.0}}\n"
        )
        .unwrap(),
        Some(2)
    );
    assert!(!latest_turn_is_closed(open));
    let closed = format!(
        "{open}{{\"type\":\"turn/end\",\"data\":{{\"turn\":2}}}}\n{{\"type\":\"session/title\",\"data\":{{}}}}\n{{\"type\":\"goal/change\","
    );
    assert!(latest_turn_is_closed(&closed));
    assert!(latest_title_follows_turn_end(&closed));
    assert!(!latest_event_follows_turn_end(&closed, "goal/change"));
    assert!(has_closed_turn(&closed, 2).is_err());
    assert!(
        latest_open_turn(
            "{\"type\":\"session\"}\n{\"type\":\"turn/start\",\"data\":{\"turn\":0}}\n"
        )
        .is_err()
    );
    assert!(
        latest_open_turn(
            "{\"type\":\"session\"}\n{\"type\":\"turn/start\",\"data\":{\"turn\":9007199254740992}}\n"
        )
        .is_err()
    );
}

#[test]
fn descriptor_and_closed_turn_helpers_require_child_model_work() {
    let content = concat!(
        "{\"type\":\"session\"}\n",
        "{\"type\":\"subagent/descriptor\"}\n",
        "{\"type\":\"request/header\"}\n",
        "{\"type\":\"turn/end\",\"data\":{\"turn\":3}}\n"
    );
    assert!(has_request_header_after_descriptor(content).unwrap());
    assert!(has_closed_turn(content, 3).unwrap());
    assert!(!has_closed_turn(content, 2).unwrap());
    assert!(has_closed_turn("{\"type\":\"turn/end\",\"data\":{\"turn\":0}}\n", 0).unwrap());
}

#[tokio::test]
async fn harvest_orders_primary_then_children_and_skips_filesystem_noise() {
    let root = tempfile::tempdir().unwrap();
    for (path, header) in [
        (
            "a/child-b/session.jsonl",
            json!({"id":"b","createdAt":3,"parentSession":"p"}),
        ),
        ("z/parent/session.jsonl", json!({"id":"p","createdAt":9})),
        (
            "a/child-a/session.jsonl",
            json!({"id":"a","createdAt":3,"parentSession":"p"}),
        ),
    ] {
        let target = root.path().join(path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, format!("{header}\n")).unwrap();
    }
    fs::write(root.path().join("stray.txt"), "noise").unwrap();
    let logs = harvest_session_logs(root.path()).await.unwrap();
    assert_eq!(
        logs.iter().map(|log| log.id.as_str()).collect::<Vec<_>>(),
        ["p", "a", "b"]
    );
    assert!(
        harvest_session_logs(&root.path().join("missing"))
            .await
            .unwrap()
            .is_empty()
    );

    let empty_root = tempfile::tempdir().unwrap();
    let empty = empty_root.path().join("bucket/empty/session.jsonl");
    fs::create_dir_all(empty.parent().unwrap()).unwrap();
    fs::write(empty, "").unwrap();
    let logs = harvest_session_logs(empty_root.path()).await.unwrap();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].id, "");
    assert!(logs[0].created_at.abs() < f64::EPSILON);
    assert!(logs[0].parent_session.is_none());
}

#[tokio::test]
async fn durable_waits_observe_goal_inbox_title_event_and_workspace_markers() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let session = AcpSessionId::new("s1");
    let log = root.path().join("bucket/s1/session.jsonl");
    fs::create_dir_all(log.parent().unwrap()).unwrap();
    fs::write(&log, "{\"type\":\"session\",\"id\":\"s1\"}\n").unwrap();
    let child = root.path().join("bucket/c1/session.jsonl");
    fs::create_dir_all(child.parent().unwrap()).unwrap();
    fs::write(
        &child,
        concat!(
            "{\"type\":\"session\",\"id\":\"c1\",\"createdAt\":2,\"parentSession\":\"s1\"}\n",
            "{\"type\":\"subagent/descriptor\"}\n",
            "{\"type\":\"request/header\"}\n",
            "{\"type\":\"turn/end\",\"data\":{\"turn\":2}}\n"
        ),
    )
    .unwrap();
    let writer = tokio::spawn({
        let log = log.clone();
        let marker = workspace.path().join("ready");
        async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let mut output = fs::OpenOptions::new().append(true).open(&log).unwrap();
            writeln!(output, "{}", json!({"type":"turn/start","data":{"turn":1}})).unwrap();
            output.flush().unwrap();
            drop(output);
            tokio::time::sleep(Duration::from_millis(30)).await;
            let mut records = String::new();
            for record in [
                json!({"type":"turn/end","data":{"turn":1}}),
                json!({"type":"goal/change","data":{"goal":{"phase":"paused"}}}),
                json!({"type":"agent/inbox/spliced","data":{"inserted":[{"content":[{"type":"text","text":"contains marker"}]}]}}),
                json!({"type":"session/title","data":{"title":"done"}}),
                json!({"type":"custom/end","data":{}}),
            ] {
                writeln!(&mut records, "{record}").unwrap();
            }
            fs::OpenOptions::new()
                .append(true)
                .open(log)
                .unwrap()
                .write_all(records.as_bytes())
                .unwrap();
            fs::write(marker, "ready").unwrap();
        }
    });
    let timeout = Duration::from_secs(1);
    wait_for_persisted_turn_start(root.path(), &session, timeout, Some(1))
        .await
        .unwrap();
    wait_for_persisted_turn_end(root.path(), &session, timeout)
        .await
        .unwrap();
    wait_for_persisted_goal_phase(root.path(), &session, GoalPhase::Paused, timeout)
        .await
        .unwrap();
    wait_for_persisted_inbox_message(root.path(), &session, "marker", timeout)
        .await
        .unwrap();
    wait_for_persisted_title_after_turn_end(root.path(), &session, timeout)
        .await
        .unwrap();
    wait_for_persisted_event_after_turn_end(root.path(), &session, "custom/end", timeout)
        .await
        .unwrap();
    wait_for_persisted_child_turn_end(root.path(), 1, timeout, 2)
        .await
        .unwrap();
    wait_for_workspace_file(workspace.path(), Path::new("ready"), timeout)
        .await
        .unwrap();
    writer.await.unwrap();
}

#[tokio::test]
async fn wait_timeouts_keep_the_source_diagnostics() {
    let root = tempfile::tempdir().unwrap();
    let session = AcpSessionId::new("missing");
    let error = wait_for_persisted_turn_end(root.path(), &session, Duration::from_millis(1))
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("did not persist turn/end within 1ms")
    );
    let error =
        wait_for_workspace_file(root.path(), Path::new("missing"), Duration::from_millis(1))
            .await
            .unwrap_err();
    assert!(error.to_string().contains("did not appear within 1ms"));
}

#[test]
fn wait_for_file_shape_round_trips_optional_timeout() {
    let marker = WaitForFile {
        path: "nested/ready".into(),
        timeout_ms: Some(25),
    };
    assert_eq!(
        serde_json::to_value(&marker).unwrap(),
        json!({"path":"nested/ready","timeoutMs":25})
    );
}
