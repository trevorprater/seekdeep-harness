//! Real macOS Seatbelt confinement through the assembled shell executor.

#![cfg(target_os = "macos")]

use std::{fs, path::Path, process::Command, sync::Arc};

use seekdeep_cordis::Context;
use seekdeep_llm::{AbortSignal, CallId, ContentBlock};
use seekdeep_sandbox::{ConfinedSandboxMode, SandboxExecutionPolicy, SandboxMode, SandboxPolicy};
use seekdeep_sandbox_local::{
    LocalSandboxConfig, LocalSandboxProvider, SandboxInternals, seatbelt_profile_args,
};
use seekdeep_sandbox_policy::{SandboxPolicyConfig, SandboxPolicyService};
use seekdeep_shell::{ShellExecRequest, ShellExecutor};
use seekdeep_subprocess_local::LocalSubprocessRuntime;
use seekdeep_tools::{ToolExecutionInput, ToolPresentationMode, ToolRuntimeConfig};
use serde_json::json;

fn usable() -> bool {
    let profile = seatbelt_profile_args(&SandboxPolicy {
        mode: ConfinedSandboxMode::ReadOnly,
        workspace_root: "/".into(),
        session_id: None,
    })
    .expect("profile");
    Command::new("sandbox-exec")
        .args(profile)
        .args(["--", "true"])
        .status()
        .is_ok_and(|status| status.success())
}

async fn executor(
    context: &Context,
    workspace: &Path,
) -> Arc<seekdeep_bash_sandbox::SandboxBashExecutor> {
    let spill = workspace.join("spill");
    fs::create_dir_all(&spill).expect("spill");
    LocalSubprocessRuntime::install_runtime(
        context,
        Arc::new(LocalSubprocessRuntime::with_spill_dir(&spill)),
    )
    .expect("subprocess");
    let sandbox = LocalSandboxProvider::new(&LocalSandboxConfig::default()).expect("sandbox");
    sandbox.set_internals(SandboxInternals {
        platform: Some("darwin".into()),
        ..SandboxInternals::default()
    });
    seekdeep_sandbox::SandboxService::new(sandbox)
        .provide(context)
        .expect("provide sandbox");
    SandboxPolicyService::new(SandboxPolicyConfig {
        mode: SandboxMode::ReadOnly,
        workspace_root: Some(workspace.to_owned()),
    })
    .expect("policy")
    .provide(context)
    .expect("provide policy");
    seekdeep_bash_sandbox::apply(context, seekdeep_bash_sandbox::Config::default())
        .await
        .expect("bash sandbox")
}

fn request(command: impl Into<String>, policy: SandboxExecutionPolicy) -> ShellExecRequest {
    let mut request = ShellExecRequest::new(command);
    request.sandbox_policy = Some(policy);
    request
}

fn policy(mode: SandboxMode, workspace: &Path) -> SandboxExecutionPolicy {
    SandboxExecutionPolicy {
        mode,
        workspace_root: workspace.to_owned(),
        session_id: None,
    }
}

#[tokio::test]
async fn assembled_seatbelt_denies_read_only_and_escape_writes_but_grants_workspace_write() {
    if !usable() {
        eprintln!("seatbelt e2e skipped: sandbox-exec cannot enforce the profile");
        return;
    }
    let home = std::env::var_os("HOME").expect("home");
    let workspace = tempfile::Builder::new()
        .prefix("seekdeep-bash-seatbelt-")
        .tempdir_in(&home)
        .expect("workspace");
    let outside = tempfile::Builder::new()
        .prefix("seekdeep-bash-seatbelt-outside-")
        .tempdir_in(home)
        .expect("outside");
    let context = Context::new();
    let executor = executor(&context, workspace.path()).await;

    let denied_path = workspace.path().join("read-only-denied.txt");
    let denied = executor
        .run(
            executor
                .resolve(request(
                    format!("printf denied > {}", denied_path.display()),
                    policy(SandboxMode::ReadOnly, workspace.path()),
                ))
                .expect("read-only spec"),
        )
        .await
        .expect("read-only result");
    assert_ne!(denied.exit_code, Some(0));
    assert!(!denied_path.exists());
    let facts = denied.sandbox.expect("read-only facts");
    assert!(facts.denied);
    assert_eq!(facts.mode, SandboxMode::ReadOnly);

    let readable = workspace.path().join("readable.txt");
    fs::write(&readable, "read-ok").expect("readable fixture");
    let read = executor
        .run(
            executor
                .resolve(request(
                    format!("cat {}", readable.display()),
                    policy(SandboxMode::ReadOnly, workspace.path()),
                ))
                .expect("read spec"),
        )
        .await
        .expect("read result");
    assert_eq!(read.stdout.text, "read-ok");

    let allowed = workspace.path().join("allowed.txt");
    let allowed_result = executor
        .run(
            executor
                .resolve(request(
                    format!("printf allowed > {}", allowed.display()),
                    policy(SandboxMode::WorkspaceWrite, workspace.path()),
                ))
                .expect("workspace spec"),
        )
        .await
        .expect("workspace result");
    assert_eq!(allowed_result.exit_code, Some(0));
    assert_eq!(
        fs::read_to_string(allowed).expect("allowed file"),
        "allowed"
    );

    let escaped = outside.path().join("escaped.txt");
    let escaped_result = executor
        .run(
            executor
                .resolve(request(
                    format!("printf escaped > {}", escaped.display()),
                    policy(SandboxMode::WorkspaceWrite, workspace.path()),
                ))
                .expect("escape spec"),
        )
        .await
        .expect("escape result");
    assert_ne!(escaped_result.exit_code, Some(0));
    assert!(escaped_result.sandbox.expect("escape facts").denied);
    assert!(!escaped.exists());
}

#[tokio::test]
async fn assembled_seatbelt_background_denial_is_stamped_before_done_returns() {
    if !usable() {
        eprintln!("seatbelt e2e skipped: sandbox-exec cannot enforce the profile");
        return;
    }
    let home = std::env::var_os("HOME").expect("home");
    let workspace = tempfile::Builder::new()
        .prefix("seekdeep-bash-seatbelt-background-")
        .tempdir_in(home)
        .expect("workspace");
    let context = Context::new();
    let executor = executor(&context, workspace.path()).await;
    let denied_path = workspace.path().join("background-denied.txt");
    let process = executor
        .start(
            executor
                .resolve(request(
                    format!("printf denied > {}", denied_path.display()),
                    policy(SandboxMode::ReadOnly, workspace.path()),
                ))
                .expect("spec"),
        )
        .expect("start");
    process.done().await;
    assert!(process.sandbox().expect("settled facts").denied);
    assert!(!denied_path.exists());
}

#[tokio::test]
async fn model_facing_bash_tool_renders_a_real_seatbelt_denial() {
    if !usable() {
        eprintln!("seatbelt e2e skipped: sandbox-exec cannot enforce the profile");
        return;
    }
    let home = std::env::var_os("HOME").expect("home");
    let workspace = tempfile::Builder::new()
        .prefix("seekdeep-bash-seatbelt-tool-")
        .tempdir_in(home)
        .expect("workspace");
    let context = Context::new();
    executor(&context, workspace.path()).await;
    let prompt = seekdeep_system_prompt::install(
        &context,
        seekdeep_system_prompt::SystemPromptConfig::default(),
    )
    .expect("prompt");
    let tools = seekdeep_tools::install(
        &context,
        &prompt,
        ToolRuntimeConfig {
            mode: ToolPresentationMode::Native,
            ..ToolRuntimeConfig::default()
        },
    )
    .expect("tools");
    seekdeep_shell_env::apply(&context, &seekdeep_shell_env::ShellEnvConfig::default())
        .expect("shell env");
    seekdeep_tool_bash::apply(&context, seekdeep_tool_bash::Config::default()).expect("tool bash");

    let denied_path = workspace.path().join("tool-denied.txt");
    let result = tools
        .execute(ToolExecutionInput::new(
            CallId::new("seatbelt-tool-call"),
            "bash",
            json!({
                "command": format!("printf denied > {}", denied_path.display()),
                "description": "Attempt a denied write"
            }),
            AbortSignal::default(),
        ))
        .await;
    assert!(!result.is_error());
    let text = result
        .content()
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    assert!(
        text.contains("[sandbox: file access denied under read-only mode]"),
        "{text}"
    );
    assert!(text.contains("[exit code:"), "{text}");
    assert!(!denied_path.exists());
}
