//! One-context, per-Session workspace confinement through assembled tools.

use std::{path::Path, sync::Arc, time::Duration};

use seekdeep_agent::{Agent, CreateAgentOptions};
use seekdeep_agent_spine_demo::{Config, apply};
use seekdeep_cordis::Context;
use seekdeep_core::session::SessionId;
use seekdeep_llm::{AbortSignal, CallId, ContentBlock};
use seekdeep_sandbox::SandboxMode;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use seekdeep_sandbox::{ConfinedSandboxMode, SandboxPolicy};
use seekdeep_sandbox_policy::{SandboxPolicyConfig, SandboxPolicyService};
use seekdeep_tools::{ToolExecutionInput, ToolExecutionResult};
use serde_json::{Value, json};

fn config(root: &Path) -> Config {
    serde_json::from_value(json!({
        "seekdeepHome":root.join(".seekdeep"),
        "workspaceContext":false,
        "skills":{"enabled":false},
        "toolBash":{"enableRunInBackground":false},
        "toolJobs":false
    }))
    .unwrap()
}

fn text(result: &ToolExecutionResult) -> String {
    result
        .content()
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(unix)]
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

fn input(id: &str, name: &str, arguments: Value, agent: &Arc<Agent>) -> ToolExecutionInput {
    ToolExecutionInput::new(CallId::new(id), name, arguments, AbortSignal::default())
        .with_agent(agent.clone())
}

async fn wait_for_tool(runtime: &seekdeep_agent_spine_demo::SpineRuntime, name: &str) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while runtime.tools.get(name, None).is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

fn process_sandbox_usable(workspace: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        let Ok(args) = seekdeep_sandbox_local::seatbelt_profile_args(&SandboxPolicy {
            mode: ConfinedSandboxMode::WorkspaceWrite,
            workspace_root: workspace.to_owned(),
            session_id: None,
        }) else {
            return false;
        };
        std::process::Command::new("sandbox-exec")
            .args(args)
            .args(["--", "true"])
            .status()
            .is_ok_and(|status| status.success())
    }
    #[cfg(target_os = "linux")]
    {
        let Ok(args) = seekdeep_sandbox_local::bwrap_profile_args(&SandboxPolicy {
            mode: ConfinedSandboxMode::WorkspaceWrite,
            workspace_root: workspace.to_owned(),
            session_id: None,
        }) else {
            return false;
        };
        if std::process::Command::new("bwrap")
            .args(args)
            .args(["--", "true"])
            .status()
            .is_ok_and(|status| status.success())
        {
            true
        } else {
            seekdeep_landlock_run::launcher_path().is_ok_and(|launcher| {
                seekdeep_landlock_run::probe(&launcher, Duration::from_secs(5))
                    != seekdeep_landlock_run::LandlockEnforcement::Unusable
            })
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = workspace;
        false
    }
}

struct Harness {
    context: Context,
    runtime: Arc<seekdeep_agent_spine_demo::SpineRuntime>,
    base: tempfile::TempDir,
}

impl Harness {
    async fn new() -> Self {
        #[cfg(unix)]
        let base = tempfile::Builder::new()
            .prefix("seekdeep-multi-project-")
            .tempdir_in(std::env::var_os("HOME").expect("home"))
            .unwrap();
        #[cfg(not(unix))]
        let base = tempfile::tempdir().unwrap();
        let fallback = base.path().join("fallback");
        let spill = base.path().join("spill");
        std::fs::create_dir(&fallback).unwrap();
        std::fs::create_dir(&spill).unwrap();

        let context = Context::new();
        seekdeep_sandbox_local::install(
            &context,
            &seekdeep_sandbox_local::LocalSandboxConfig::default(),
        )
        .unwrap();
        SandboxPolicyService::new(SandboxPolicyConfig {
            mode: SandboxMode::WorkspaceWrite,
            workspace_root: Some(fallback.clone()),
        })
        .unwrap()
        .provide(&context)
        .unwrap();
        seekdeep_subprocess_local::LocalSubprocessRuntime::install_runtime(
            &context,
            Arc::new(seekdeep_subprocess_local::LocalSubprocessRuntime::with_spill_dir(&spill)),
        )
        .unwrap();
        seekdeep_bash_sandbox::apply(
            &context,
            seekdeep_bash_sandbox::Config {
                cwd: Some(fallback.to_string_lossy().into_owned()),
                timeout_ms: 30_000.0,
                ..seekdeep_bash_sandbox::Config::default()
            },
        )
        .await
        .unwrap();
        seekdeep_fs_sandbox::apply(
            &context,
            seekdeep_fs_local::Config {
                cwd: Some(fallback.to_string_lossy().into_owned()),
                ..seekdeep_fs_local::Config::default()
            },
        )
        .unwrap();
        let runtime = apply(&context, config(base.path())).await.unwrap();
        seekdeep_tool_fs::apply(&context, &seekdeep_tool_fs::Config::default()).unwrap();
        wait_for_tool(&runtime, "bash").await;
        wait_for_tool(&runtime, "write").await;
        Self {
            context,
            runtime,
            base,
        }
    }

    fn project(&self, name: &str) -> std::path::PathBuf {
        let path = self.base.path().join(name);
        std::fs::create_dir(&path).unwrap();
        path
    }

    async fn agent(&self, id: &str, cwd: &Path) -> seekdeep_agent::AgentHandle {
        let mut request = CreateAgentOptions::new(SessionId::new(id));
        request.meta.cwd = Some(cwd.to_string_lossy().into_owned());
        self.runtime.agents.create(request).await.unwrap()
    }
}

#[tokio::test]
async fn concurrent_filesystem_writes_stay_inside_each_calling_session_workspace() {
    let harness = Harness::new().await;
    let project_a = harness.project("project-a");
    let project_b = harness.project("project-b");
    let agent_a = harness.agent("project-a-session", &project_a).await;
    let agent_b = harness.agent("project-b-session", &project_b).await;

    let (a_own, b_own, a_cross, b_cross) = tokio::join!(
        harness.runtime.tools.execute(input(
            "fs-a-own",
            "write",
            json!({"file_path":"a-owned.txt","content":"a"}),
            &agent_a.agent,
        )),
        harness.runtime.tools.execute(input(
            "fs-b-own",
            "write",
            json!({"file_path":"b-owned.txt","content":"b"}),
            &agent_b.agent,
        )),
        harness.runtime.tools.execute(input(
            "fs-a-cross",
            "write",
            json!({"file_path":project_b.join("from-a.txt"),"content":"cross"}),
            &agent_a.agent,
        )),
        harness.runtime.tools.execute(input(
            "fs-b-cross",
            "write",
            json!({"file_path":project_a.join("from-b.txt"),"content":"cross"}),
            &agent_b.agent,
        )),
    );

    assert!(!a_own.is_error());
    assert!(!b_own.is_error());
    assert!(a_cross.is_error());
    assert!(b_cross.is_error());
    assert!(text(&a_cross).contains("[sandbox: file access denied under workspace-write mode]"));
    assert!(text(&b_cross).contains("[sandbox: file access denied under workspace-write mode]"));
    assert_eq!(
        std::fs::read_to_string(project_a.join("a-owned.txt")).unwrap(),
        "a"
    );
    assert_eq!(
        std::fs::read_to_string(project_b.join("b-owned.txt")).unwrap(),
        "b"
    );
    assert!(!project_b.join("from-a.txt").exists());
    assert!(!project_a.join("from-b.txt").exists());
    harness.context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn concurrent_bash_calls_stay_inside_each_calling_session_workspace() {
    let harness = Harness::new().await;
    if !process_sandbox_usable(harness.base.path()) {
        harness.context.fiber().dispose().await.unwrap();
        return;
    }
    let project_a = harness.project("project-a");
    let project_b = harness.project("project-b");
    let agent_a = harness.agent("project-a-session", &project_a).await;
    let agent_b = harness.agent("project-b-session", &project_b).await;
    let sibling_a = project_a.file_name().unwrap().to_string_lossy();
    let sibling_b = project_b.file_name().unwrap().to_string_lossy();

    let (a_own, b_own, a_cross, b_cross) = tokio::join!(
        harness.runtime.tools.execute(input(
            "bash-a-own",
            "bash",
            json!({"command":"printf a > a-owned.txt","description":"Write project A marker"}),
            &agent_a.agent,
        )),
        harness.runtime.tools.execute(input(
            "bash-b-own",
            "bash",
            json!({"command":"printf b > b-owned.txt","description":"Write project B marker"}),
            &agent_b.agent,
        )),
        harness.runtime.tools.execute(input(
            "bash-a-cross",
            "bash",
            json!({"command":format!("printf cross > ../{sibling_b}/from-a.txt"),"description":"Attempt project B write"}),
            &agent_a.agent,
        )),
        harness.runtime.tools.execute(input(
            "bash-b-cross",
            "bash",
            json!({"command":format!("printf cross > ../{sibling_a}/from-b.txt"),"description":"Attempt project A write"}),
            &agent_b.agent,
        )),
    );

    for result in [&a_own, &b_own, &a_cross, &b_cross] {
        assert!(!result.is_error(), "{}", text(result));
    }
    assert!(text(&a_cross).contains("[sandbox: file access denied under workspace-write mode]"));
    assert!(text(&b_cross).contains("[sandbox: file access denied under workspace-write mode]"));
    assert_eq!(
        std::fs::read_to_string(project_a.join("a-owned.txt")).unwrap(),
        "a"
    );
    assert_eq!(
        std::fs::read_to_string(project_b.join("b-owned.txt")).unwrap(),
        "b"
    );
    assert!(!project_b.join("from-a.txt").exists());
    assert!(!project_a.join("from-b.txt").exists());
    harness.context.fiber().dispose().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_sensitive_session_cwd_matches_between_bash_fs_and_policy() {
    let harness = Harness::new().await;
    if !process_sandbox_usable(harness.base.path()) {
        harness.context.fiber().dispose().await.unwrap();
        return;
    }
    let lexical_root = harness.project("lexical-workspace");
    let physical_root = harness.project("physical-workspace");
    let physical_child = physical_root.join("child");
    std::fs::create_dir(&physical_child).unwrap();
    let link = lexical_root.join("link");
    std::os::unix::fs::symlink(&physical_child, &link).unwrap();
    let session_cwd = link.join("..");
    let agent = harness.agent("symlink-parent-session", &session_cwd).await;

    let (bash_own, bash_lexical, fs_own, fs_lexical) = tokio::join!(
        harness.runtime.tools.execute(input(
            "bash-symlink-own",
            "bash",
            json!({"command":"printf bash > bash-owned.txt","description":"Write physical workspace marker"}),
            &agent.agent,
        )),
        harness.runtime.tools.execute(input(
            "bash-symlink-lexical",
            "bash",
            json!({"command":format!("printf escaped > {}", shell_quote(&lexical_root.join("bash-escaped.txt"))),"description":"Attempt lexical workspace write"}),
            &agent.agent,
        )),
        harness.runtime.tools.execute(input(
            "fs-symlink-own",
            "write",
            json!({"file_path":"fs-owned.txt","content":"fs"}),
            &agent.agent,
        )),
        harness.runtime.tools.execute(input(
            "fs-symlink-lexical",
            "write",
            json!({"file_path":lexical_root.join("fs-escaped.txt"),"content":"escaped"}),
            &agent.agent,
        )),
    );

    assert!(!bash_own.is_error());
    assert!(!bash_lexical.is_error());
    assert!(!fs_own.is_error());
    assert!(fs_lexical.is_error());
    assert!(
        text(&bash_lexical).contains("[sandbox: file access denied under workspace-write mode]")
    );
    assert!(text(&fs_lexical).contains("[sandbox: file access denied under workspace-write mode]"));
    assert_eq!(
        std::fs::read_to_string(physical_root.join("bash-owned.txt")).unwrap(),
        "bash"
    );
    assert_eq!(
        std::fs::read_to_string(physical_root.join("fs-owned.txt")).unwrap(),
        "fs"
    );
    assert!(!lexical_root.join("bash-escaped.txt").exists());
    assert!(!lexical_root.join("fs-escaped.txt").exists());
    harness.context.fiber().dispose().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn parent_traversal_from_a_symlinked_session_root_is_physical() {
    let harness = Harness::new().await;
    if !process_sandbox_usable(harness.base.path()) {
        harness.context.fiber().dispose().await.unwrap();
        return;
    }
    let lexical_root = harness.project("lexical-parent");
    let physical_root = harness.project("physical-parent");
    let physical_child = physical_root.join("child");
    std::fs::create_dir(&physical_child).unwrap();
    let link = lexical_root.join("link");
    std::os::unix::fs::symlink(&physical_child, &link).unwrap();
    std::fs::write(lexical_root.join("shared.txt"), "from-lexical-parent").unwrap();
    std::fs::write(physical_root.join("shared.txt"), "from-physical-parent").unwrap();
    let agent = harness
        .agent("symlink-root-parent-path-session", &link)
        .await;

    let (bash_read, fs_read) = tokio::join!(
        harness.runtime.tools.execute(input(
            "bash-symlink-parent-read",
            "bash",
            json!({"command":"cat ../shared.txt","description":"Read through the physical parent"}),
            &agent.agent,
        )),
        harness.runtime.tools.execute(input(
            "fs-symlink-parent-read",
            "read",
            json!({"file_path":"../shared.txt"}),
            &agent.agent,
        )),
    );

    assert!(!bash_read.is_error(), "{}", text(&bash_read));
    assert!(!fs_read.is_error(), "{}", text(&fs_read));
    assert!(text(&bash_read).contains("from-physical-parent"));
    assert!(text(&fs_read).contains("from-physical-parent"));
    assert!(!text(&bash_read).contains("from-lexical-parent"));
    assert!(!text(&fs_read).contains("from-lexical-parent"));
    harness.context.fiber().dispose().await.unwrap();
}
