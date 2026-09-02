//! Compiled ACP demo replay through the committed snapshot harness.

use std::{collections::BTreeMap, path::PathBuf};

use seekdeep_acp_snapshot::{
    AgentUnderTest, InputScript, NormalizeContext, NormalizeOptions, RunOptions, SnapshotRunMode,
    fixture_context, normalize_session_log, normalize_stdout, run_scenario, scrub_request_headers,
};

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn compiled_agent(root: &std::path::Path) -> AgentUnderTest {
    AgentUnderTest {
        source_bin: PathBuf::from(env!("CARGO_BIN_EXE_seekdeep-acp-demo")),
        library_bin: Some(PathBuf::from(env!("CARGO_BIN_EXE_seekdeep-acp-demo"))),
        config_path: root.join("examples/acp-agent/cordis.yml"),
        tsconfig_path: root.join("tsconfig.json"),
    }
}

fn normalize_context(result: &seekdeep_acp_snapshot::RunResult) -> NormalizeContext {
    NormalizeContext {
        session_ids: result
            .session_id
            .iter()
            .map(|id| id.as_str().to_owned())
            .chain(result.session_logs.iter().map(|log| log.id.clone()))
            .collect(),
        cwd: result.cwd.to_string_lossy().into_owned(),
        cwd_aliases: result
            .cwd_aliases
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect(),
    }
}

#[tokio::test]
async fn compiled_demo_replays_the_committed_handshake_contract() {
    let root = repository_root();
    let scenario = root.join("examples/acp-agent/tests/snapshots/hanseekdeepake");
    let input: InputScript =
        serde_json::from_slice(&std::fs::read(scenario.join("input.json")).unwrap()).unwrap();
    let result = run_scenario(
        &input,
        RunOptions {
            agent: compiled_agent(&root),
            mode: SnapshotRunMode::Replay,
            environment: BTreeMap::new(),
            fixture_file: scenario.join("session.jsonl"),
            override_file: None,
            child_files: Vec::new(),
            workspace_dir: None,
            prepare_workspace: None,
            workspace_parent: None,
            config_path: None,
            artifact_mode: None,
        },
    )
    .await
    .unwrap();
    let context = normalize_context(&result);
    assert_eq!(
        normalize_stdout(&result.raw_stdout, &context, NormalizeOptions::default()).unwrap(),
        std::fs::read_to_string(scenario.join("stdout.expected.jsonl")).unwrap()
    );
}

#[tokio::test]
async fn compiled_demo_replays_a_model_turn_and_persists_the_expected_log() {
    let root = repository_root();
    let scenario = root.join("examples/acp-agent/tests/snapshots/text-turn");
    let fixture_file = scenario.join("session.jsonl");
    let input: InputScript =
        serde_json::from_slice(&std::fs::read(scenario.join("input.json")).unwrap()).unwrap();
    let result = run_scenario(
        &input,
        RunOptions {
            agent: compiled_agent(&root),
            mode: SnapshotRunMode::Replay,
            environment: BTreeMap::new(),
            fixture_file: fixture_file.clone(),
            override_file: scenario
                .join("replay.override.json")
                .is_file()
                .then(|| scenario.join("replay.override.json")),
            child_files: Vec::new(),
            workspace_dir: scenario
                .join("workspace")
                .is_dir()
                .then(|| scenario.join("workspace")),
            prepare_workspace: None,
            workspace_parent: None,
            config_path: None,
            artifact_mode: None,
        },
    )
    .await
    .unwrap();
    let context = normalize_context(&result);
    assert_eq!(
        normalize_stdout(&result.raw_stdout, &context, NormalizeOptions::default()).unwrap(),
        std::fs::read_to_string(scenario.join("stdout.expected.jsonl")).unwrap()
    );
    assert_eq!(result.session_logs.len(), 1);
    let harvested = scrub_request_headers(&result.session_logs[0].content).unwrap();
    let fixture = std::fs::read_to_string(&fixture_file)
        .unwrap()
        .replace("SEEKDEEP file policy", "SeekDeep file policy")
        .replace("SEEKDEEP file sandbox", "SeekDeep file sandbox");
    let fixture = scrub_request_headers(&fixture).unwrap();
    assert_eq!(
        normalize_session_log(&harvested, &context, NormalizeOptions::default()).unwrap(),
        normalize_session_log(
            &fixture,
            &fixture_context(&fixture).unwrap(),
            NormalizeOptions::default()
        )
        .unwrap()
    );
}
