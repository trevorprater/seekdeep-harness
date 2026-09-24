//! Real compiled-runtime replay of the committed SDK example scenarios.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use seekdeep_core::session::SessionId;
use seekdeep_sdk_client::{
    DeepSeekHarness, DeepSeekHarnessOptions, HarnessClientOptions, RunOptions,
};

const EXAMPLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/jsonrpc-agent");

struct Scenario {
    name: &'static str,
    prompt: &'static str,
    session_id: &'static str,
    final_response: &'static str,
    child_count: usize,
    minimal: bool,
}

fn hydrate(source: &Path, destination: &Path, cwd: &Path) {
    let content = std::fs::read_to_string(source)
        .unwrap()
        .replace("{{cwd}}", &cwd.to_string_lossy());
    std::fs::write(destination, content).unwrap();
}

fn jsonl_files(root: &Path) -> Vec<PathBuf> {
    fn visit(path: &Path, files: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                visit(&path, files);
            } else if path.extension().and_then(|value| value.to_str()) == Some("jsonl") {
                files.push(path);
            }
        }
    }
    let mut files = Vec::new();
    visit(root, &mut files);
    files.sort();
    files
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn compiled_runtime_replays_every_sdk_snapshot_scenario() {
    let scenarios = [
        Scenario {
            name: "text-turn",
            prompt: "Reply with exactly: SDK snapshot OK",
            session_id: "sdk-snapshot-text",
            final_response: "SDK snapshot OK",
            child_count: 0,
            minimal: false,
        },
        Scenario {
            name: "bash-tool",
            prompt: "Run this exact command with your bash tool, then reply with its stdout only: echo seekdeep-sdk-proof-7391",
            session_id: "sdk-snapshot-bash",
            final_response: "seekdeep-sdk-proof-7391",
            child_count: 0,
            minimal: false,
        },
        Scenario {
            name: "subagent-spawn-in-process",
            prompt: "Use the subagent tool exactly once with description 'echo probe' and prompt: Reply with exactly: child answer 42. Then reply with the subagent's final answer verbatim.",
            session_id: "sdk-snapshot-subagent",
            final_response: "child answer 42.",
            child_count: 1,
            minimal: false,
        },
        Scenario {
            name: "persistent-tools",
            prompt: "Prove that bash state persists. Then create {{cwd}}/note.txt with a tab-indented line, view it, replace that literal tab-indented line, and make the persistent shell exit with code 9.",
            session_id: "persistent-tools-snapshot",
            final_response: "PERSISTENT_TOOLS_OK",
            child_count: 0,
            minimal: true,
        },
    ];

    for scenario in scenarios {
        let temporary = tempfile::tempdir().unwrap();
        let cwd = std::fs::canonicalize(temporary.path()).unwrap();
        let replay_root = cwd.join(".replay");
        std::fs::create_dir(&replay_root).unwrap();
        let fixture_root = Path::new(EXAMPLE)
            .join("tests/snapshots")
            .join(scenario.name);
        let parent = replay_root.join("session.jsonl");
        hydrate(&fixture_root.join("session.jsonl"), &parent, &cwd);
        let mut children = Vec::new();
        for index in 1..=scenario.child_count {
            let target = replay_root.join(format!("session.{index}.jsonl"));
            hydrate(
                &fixture_root.join(format!("session.{index}.jsonl")),
                &target,
                &cwd,
            );
            children.push(target);
        }
        let mut environment = std::env::vars().collect::<BTreeMap<_, _>>();
        environment.insert(
            "SEEKDEEP_CORDIS_CONFIG".to_owned(),
            Path::new(EXAMPLE)
                .join(if scenario.minimal {
                    "minimal.snapshot.cordis.yml"
                } else {
                    "cordis.snapshot.yml"
                })
                .to_string_lossy()
                .into_owned(),
        );
        environment.insert(
            "SEEKDEEP_SESSION_ROOT".to_owned(),
            cwd.join(".sessions").to_string_lossy().into_owned(),
        );
        environment.insert(
            "SEEKDEEP_CWD".to_owned(),
            cwd.to_string_lossy().into_owned(),
        );
        environment.insert("SEEKDEEP_SNAPSHOT".to_owned(), "replay".to_owned());
        environment.insert(
            "SEEKDEEP_SNAPSHOT_FILE".to_owned(),
            parent.to_string_lossy().into_owned(),
        );
        environment.insert(
            "SEEKDEEP_HOME".to_owned(),
            cwd.join(".seekdeep").to_string_lossy().into_owned(),
        );
        environment.insert(
            "SEEKDEEP_AGENTS_HOME".to_owned(),
            cwd.join(".agents").to_string_lossy().into_owned(),
        );
        if !children.is_empty() {
            environment.insert(
                "SEEKDEEP_SNAPSHOT_CHILD_FILES".to_owned(),
                std::env::join_paths(children.iter())
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        if scenario.minimal {
            environment.insert(
                "SEEKDEEP_SYSTEM_PROMPT".to_owned(),
                "You are the environment-selected minimal software engineer.".to_owned(),
            );
        }
        let mut launch = HarnessClientOptions::new(env!("CARGO_BIN_EXE_seekdeep-jsonrpc-agent"));
        launch.cwd = Some(cwd.to_string_lossy().into_owned());
        launch.env = Some(environment);
        launch.request_timeout_ms = Some(110_000.0);
        let harness = DeepSeekHarness::new(DeepSeekHarnessOptions {
            launch,
            cwd: Some(cwd.to_string_lossy().into_owned()),
            provider: Some("deepseek-official".to_owned()),
            model: Some("deepseek-v4-flash".to_owned()),
            max_tokens: None,
        })
        .unwrap();
        let result = harness
            .run(
                scenario.prompt.replace("{{cwd}}", &cwd.to_string_lossy()),
                RunOptions {
                    session_id: Some(SessionId::new(scenario.session_id)),
                    on_notification: None,
                },
            )
            .await
            .unwrap_or_else(|error| panic!("{} failed: {error:#}", scenario.name));
        assert_eq!(
            result.final_response, scenario.final_response,
            "{} events={:#?}",
            scenario.name, result.events,
        );
        assert_eq!(
            result
                .notifications
                .last()
                .map(|value| value.method.as_str()),
            Some("session.status"),
            "{}",
            scenario.name
        );
        assert_eq!(
            jsonl_files(&cwd.join(".sessions")).len(),
            scenario.child_count + 1,
            "{}",
            scenario.name
        );
        if scenario.minimal {
            assert_eq!(
                std::fs::read_to_string(cwd.join("note.txt")).unwrap(),
                "target:\n\tnew\n"
            );
        }
        harness.close().await.unwrap();
    }
}
