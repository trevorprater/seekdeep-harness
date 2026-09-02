//! Complete ACP example snapshot scenario registration and fixture guards.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::{Duration, SystemTime},
};

use seekdeep_acp_snapshot::{
    AcpSnapshotSuite, AgentUnderTest, SnapshotScenario, SnapshotSuiteMode, SnapshotSuiteOptions,
    define_acp_snapshot_suite,
};

macro_rules! snapshot {
    ($name:literal, $model:literal, $recorded:literal) => {
        SnapshotScenario {
            name: $name.to_owned(),
            has_model_turn: $model,
            recorded: $recorded,
            ..SnapshotScenario::default()
        }
    };
    ($name:literal, $model:literal, $recorded:literal, $($field:ident = $value:expr),+ $(,)?) => {{
        let mut scenario = SnapshotScenario {
            name: $name.to_owned(),
            has_model_turn: $model,
            recorded: $recorded,
            ..SnapshotScenario::default()
        };
        $(scenario.$field = $value;)+
        scenario
    }};
}

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn config(root: &Path, file: &str) -> Option<PathBuf> {
    Some(root.join("examples/acp-agent").join(file))
}

fn class(name: &str) -> Option<String> {
    Some(name.to_owned())
}

fn source(name: &str) -> Option<String> {
    Some(name.to_owned())
}

fn environment(values: &[(&str, &str)]) -> BTreeMap<OsString, OsString> {
    values
        .iter()
        .map(|(name, value)| (OsString::from(name), OsString::from(value)))
        .collect()
}

fn prepare_delimiter_workspace() -> seekdeep_acp_snapshot::PrepareWorkspace {
    Arc::new(|cwd| {
        Box::pin(async move {
            let directory = cwd.join("scope</system-reminder>");
            tokio::fs::create_dir_all(&directory).await?;
            tokio::fs::write(
                directory.join("AGENTS.md"),
                "Delimiter path snapshot instruction.\n",
            )
            .await?;
            tokio::fs::write(directory.join("task.txt"), "delimiter path snapshot task\n").await?;
            Ok(())
        })
    })
}

fn prepare_fs_search_workspace() -> seekdeep_acp_snapshot::PrepareWorkspace {
    Arc::new(|cwd| {
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let files = [
                    ("archive/a.ts", 1),
                    ("archive/b.ts", 2),
                    ("archive/c.ts", 3),
                    ("docs/guide.md", 4),
                    ("src/index.ts", 5),
                    ("test/spec.ts", 6),
                    ("top.txt", 7),
                    ("notes.md", 8),
                ];
                for (relative, milliseconds) in files {
                    let target = cwd.join("tree").join(relative);
                    fs::create_dir_all(target.parent().expect("fixture file has a parent"))?;
                    fs::write(&target, "fixture\n")?;
                    let timestamp = SystemTime::UNIX_EPOCH
                        + Duration::from_secs(946_684_800)
                        + Duration::from_millis(milliseconds);
                    fs::File::options().write(true).open(target)?.set_times(
                        fs::FileTimes::new()
                            .set_accessed(timestamp)
                            .set_modified(timestamp),
                    )?;
                }
                Ok::<(), anyhow::Error>(())
            })
            .await??;
            Ok(())
        })
    })
}

fn scenarios(root: &Path) -> Vec<SnapshotScenario> {
    vec![
        snapshot!("handshake", false, false),
        snapshot!("reject-extra-dirs", false, false),
        snapshot!("text-turn", true, true, pins_header = true),
        snapshot!(
            "product-subagent-codex",
            true,
            false,
            pins_header = true,
            header_class = class("product-subagent-codex"),
            config_path = config(root, "product-subagent-codex.cordis.yml")
        ),
        snapshot!(
            "product-subagent-both",
            true,
            false,
            pins_header = true,
            header_class = class("product-subagent-both"),
            system_prompt_source = source("product-subagent-codex"),
            config_path = config(root, "product-subagent-both.cordis.yml")
        ),
        snapshot!(
            "session-title-after-turn",
            true,
            false,
            overridden = true,
            config_path = config(root, "session-title.cordis.yml")
        ),
        snapshot!("tool-call-turn", true, true),
        snapshot!("packed-chunks", true, false),
        snapshot!(
            "parallel-tool-calls",
            true,
            false,
            config_path = config(root, "fs.cordis.yml")
        ),
        snapshot!(
            "bash-spill",
            true,
            false,
            config_path = config(root, "fs.cordis.yml")
        ),
        snapshot!(
            "session-query-spill",
            true,
            false,
            overridden = true,
            pins_header = true,
            header_class = class("session-query"),
            config_path = config(root, "session-query.cordis.yml"),
            posix_only = true
        ),
        snapshot!(
            "read-image",
            true,
            false,
            pins_header = true,
            header_class = class("image"),
            system_prompt_source = source("text-turn"),
            config_path = config(root, "image.cordis.yml")
        ),
        snapshot!(
            "read-image-text-route",
            true,
            false,
            header_class = class("image"),
            config_path = config(root, "image-text-route.cordis.yml")
        ),
        snapshot!(
            "pty-tools",
            true,
            false,
            pins_header = true,
            header_class = class("pty"),
            config_path = config(root, "pty.cordis.yml")
        ),
        snapshot!("bash-tool-turn", true, true),
        snapshot!(
            "background-job-admission",
            true,
            false,
            overridden = true,
            config_path = config(root, "background-job-admission.cordis.yml"),
            posix_only = true
        ),
        snapshot!(
            "pwsh-tool-turn",
            true,
            true,
            pins_header = true,
            header_class = class("pwsh"),
            config_path = config(root, "tests/pwsh.cordis.yml"),
            pwsh_only = true
        ),
        snapshot!(
            "partial-landlock-child-failure",
            true,
            false,
            header_class = class("sandbox"),
            config_path = config(root, "partial-landlock.cordis.yml"),
            environment = environment(&[("SEEKDEEP_PERMISSION_MODE", "read-only")]),
            posix_only = true
        ),
        snapshot!(
            "missing-sandbox-runner",
            true,
            false,
            header_class = class("sandbox"),
            config_path = config(root, "partial-landlock.cordis.yml"),
            environment = environment(&[
                ("SEEKDEEP_PERMISSION_MODE", "read-only"),
                ("SEEKDEEP_SNAPSHOT_MISSING_SANDBOX_RUNNER", "1")
            ]),
            posix_only = true
        ),
        snapshot!("todo-write", true, true),
        snapshot!(
            "skill-load",
            true,
            false,
            pins_header = true,
            header_class = class("skill"),
            system_prompt_source = source("text-turn"),
            tool_schemas_source = source("text-turn")
        ),
        snapshot!(
            "lsp-definition",
            true,
            false,
            pins_header = true,
            header_class = class("lsp"),
            config_path = config(root, "tests/lsp.cordis.yml")
        ),
        snapshot!(
            "web-fetch",
            true,
            true,
            pins_header = true,
            header_class = class("web"),
            config_path = config(root, "web.cordis.yml")
        ),
        snapshot!("workspace-edit", true, true),
        snapshot!(
            "fs-glob-sampling",
            true,
            true,
            posix_only = true,
            pins_header = true,
            header_class = class("fs-search"),
            config_path = config(root, "tests/fs-search.cordis.yml"),
            prepare_workspace = Some(prepare_fs_search_workspace())
        ),
        snapshot!("fs-read", true, true),
        snapshot!("fs-write", true, true),
        snapshot!("fs-edit", true, true),
        snapshot!("fs-write-overwrite", true, true),
        snapshot!(
            "fs-write-overwrite-bounded",
            true,
            true,
            pins_header = true,
            header_class = class("fs-diff-bound"),
            system_prompt_source = source("text-turn"),
            tool_schemas_source = source("text-turn"),
            config_path = config(root, "tests/fs-diff-bound.cordis.yml")
        ),
        snapshot!("fs-read-window", true, true),
        snapshot!("fs-policy-reject", true, true),
        snapshot!("fs-delete-recreate", true, true),
        snapshot!("multi-turn", true, true),
        snapshot!("error-finish", true, false, overridden = true),
        snapshot!(
            "empty-response-retry",
            true,
            false,
            config_path = config(root, "retry.cordis.yml")
        ),
        snapshot!("repeat-tool-reminder", true, false),
        snapshot!(
            "agent-instructions",
            true,
            false,
            overridden = true,
            pins_header = true,
            header_class = class("agent-instructions"),
            tool_schemas_source = source("text-turn"),
            config_path = config(root, "agent-instructions.cordis.yml"),
            prepare_workspace = Some(prepare_delimiter_workspace()),
            posix_only = true
        ),
        snapshot!("cancel", true, false, overridden = true),
        snapshot!(
            "cancel-tool-calls",
            true,
            false,
            overridden = true,
            posix_only = true
        ),
        snapshot!("subagent-spawn-in-process", true, true),
        snapshot!("subagent-max-tokens-partial", true, false),
        snapshot!("subagent-multi", true, true),
        snapshot!("subagent-parallel", true, false),
        snapshot!("subagent-fork-in-process", true, true),
        snapshot!("subagent-mixed", true, true),
        snapshot!(
            "subagent-continuable",
            true,
            false,
            pins_child_tool_schemas = vec![1],
            pins_child_system_prompts = vec![1],
            config_path = config(root, "subagent-durability-failure.cordis.yml")
        ),
        snapshot!(
            "subagent-continuable-inheritance",
            true,
            false,
            pins_child_tool_schemas = vec![1],
            pins_child_system_prompts = vec![1],
            config_path = config(root, "subagent-continuable-inheritance.cordis.yml")
        ),
        snapshot!(
            "subagent-published-run-failure",
            true,
            false,
            environment = environment(&[("SEEKDEEP_SUBAGENT_PUBLISHED_FAILURE", "1")]),
            overridden = true,
            config_path = config(root, "subagent-durability-failure.cordis.yml")
        ),
        snapshot!(
            "subagent-report",
            true,
            false,
            config_path = config(root, "subagent-report-quiet.cordis.yml"),
            pins_child_tool_schemas = vec![1],
            pins_child_system_prompts = vec![1]
        ),
        snapshot!(
            "subagent-list-agents",
            true,
            false,
            pins_child_tool_schemas = vec![1],
            pins_child_system_prompts = vec![1]
        ),
        snapshot!(
            "subagent-depth-two-rejection",
            true,
            false,
            overridden = true,
            config_path = config(root, "depth-two.cordis.yml")
        ),
        snapshot!(
            "subagent-child-question-rejection",
            true,
            false,
            pins_header = true,
            header_class = class("child-question"),
            system_prompt_source = source("text-turn"),
            config_path = config(root, "child-question.cordis.yml")
        ),
        snapshot!("workflow-run", true, true),
        snapshot!(
            "advanced-toolchain",
            true,
            false,
            pins_header = true,
            header_class = class("advanced"),
            config_path = config(root, "advanced.cordis.yml")
        ),
        snapshot!(
            "cordis-inspect-jsdoc",
            true,
            false,
            header_class = class("advanced"),
            config_path = config(root, "advanced.cordis.yml")
        ),
        snapshot!("hook-cc-promptsubmit-block", false, false),
        snapshot!("hook-codex-promptsubmit-block", false, false),
        snapshot!("hook-cc-invalid-matcher", true, false),
        snapshot!("hook-codex-invalid-matcher", true, false),
        snapshot!("hook-cc-promptsubmit-context", true, true),
        snapshot!("hook-cc-pretool-deny", true, true),
        snapshot!("hook-cc-pretool-ask", true, true),
        snapshot!("hook-cc-posttool-block", true, true),
        snapshot!("hook-cc-posttool-context", true, true),
        snapshot!("hook-cc-stop-continue", true, true),
        snapshot!("hook-codex-promptsubmit-context", true, true),
        snapshot!("hook-codex-pretool-block", true, true),
        snapshot!("hook-codex-posttool-block", true, true),
        snapshot!("hook-codex-posttool-context", true, true),
        snapshot!("hook-codex-stop-continue", true, true),
        snapshot!(
            "code-mode-turn",
            true,
            true,
            pins_header = true,
            header_class = class("code"),
            config_path = config(root, "code-mode.cordis.yml")
        ),
        snapshot!(
            "code-mode-workspace-context",
            true,
            false,
            overridden = true,
            pins_header = true,
            header_class = class("code-workspace-context"),
            system_prompt_source = source("code-mode-turn"),
            tool_schemas_source = source("code-mode-turn"),
            config_path = config(root, "code-mode-workspace-context.cordis.yml")
        ),
        snapshot!(
            "both-mode-turn",
            true,
            true,
            pins_header = true,
            header_class = class("both"),
            config_path = config(root, "both-mode.cordis.yml")
        ),
        snapshot!(
            "escalation-approved",
            true,
            true,
            pins_header = true,
            header_class = class("sandbox"),
            system_prompt_source = source("text-turn"),
            tool_schemas_source = source("text-turn"),
            environment = environment(&[("SEEKDEEP_PERMISSION_MODE", "workspace-write")])
        ),
        snapshot!(
            "escalation-rejected",
            true,
            true,
            header_class = class("sandbox"),
            environment = environment(&[("SEEKDEEP_PERMISSION_MODE", "workspace-write")])
        ),
        snapshot!(
            "fs-escalation-approved",
            true,
            true,
            header_class = class("sandbox"),
            environment = environment(&[("SEEKDEEP_PERMISSION_MODE", "workspace-write")])
        ),
        snapshot!(
            "session-sandbox-root",
            true,
            false,
            overridden = true,
            header_class = class("sandbox"),
            config_path = config(root, "session-sandbox-root.cordis.yml"),
            environment = environment(&[("SEEKDEEP_PERMISSION_MODE", "workspace-write")]),
            workspace_parent = std::env::var_os("HOME").map(PathBuf::from)
        ),
    ]
}

fn suite(mode: SnapshotSuiteMode) -> anyhow::Result<AcpSnapshotSuite> {
    let root = repository_root();
    define_acp_snapshot_suite(SnapshotSuiteOptions {
        agent: AgentUnderTest {
            source_bin: PathBuf::from(env!("CARGO_BIN_EXE_seekdeep-acp-demo")),
            library_bin: Some(PathBuf::from(env!("CARGO_BIN_EXE_seekdeep-acp-demo"))),
            config_path: root.join("examples/acp-agent/cordis.yml"),
            tsconfig_path: root.join("tsconfig.json"),
        },
        snapshots_dir: root.join("examples/acp-agent/tests/snapshots"),
        scenarios: scenarios(&root),
        mode,
        has_pwsh: Some(has_pwsh()),
    })
}

fn has_pwsh() -> bool {
    let environment = std::env::vars().collect::<BTreeMap<_, _>>();
    let platform = if cfg!(windows) {
        seekdeep_pwsh_local::PwshPlatform::Windows
    } else {
        seekdeep_pwsh_local::PwshPlatform::Other
    };
    Command::new(seekdeep_pwsh_local::resolve_pwsh_path(
        None,
        &environment,
        platform,
    ))
    .args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        "$true",
    ])
    .status()
    .is_ok_and(|status| status.success())
}

fn mode_from_environment() -> anyhow::Result<SnapshotSuiteMode> {
    match std::env::var("SEEKDEEP_SNAPSHOT") {
        Err(std::env::VarError::NotPresent) => Ok(SnapshotSuiteMode::Replay),
        Err(error) => Err(error.into()),
        Ok(value) => match value.as_str() {
            "" | "replay" => Ok(SnapshotSuiteMode::Replay),
            "record" => Ok(SnapshotSuiteMode::Record),
            "refresh" => Ok(SnapshotSuiteMode::Refresh),
            _ => anyhow::bail!("unknown SEEKDEEP_SNAPSHOT mode: {value}"),
        },
    }
}

#[test]
fn complete_scenario_table_matches_every_committed_directory_and_fixture_guard() {
    let suite = suite(SnapshotSuiteMode::Replay).unwrap();
    assert_eq!(suite.options().scenarios.len(), 78);
    suite.validate_fixtures().unwrap();
}

#[tokio::test]
async fn compiled_demo_runs_the_complete_snapshot_suite() {
    suite(mode_from_environment().unwrap())
        .unwrap()
        .run()
        .await
        .unwrap();
}
