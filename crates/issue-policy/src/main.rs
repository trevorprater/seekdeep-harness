//! GitHub Actions entry point for Issue policy and lifecycle handling.

use std::{env, fs, path::PathBuf, process::ExitCode};

use anyhow::{Context as _, Result, bail};
use seekdeep_issue_policy::{
    IssuePolicyConfig, IssuePolicyRuntime, LifecycleEvent, ReqwestGitHubTransport,
    resolving_issue_status_command,
};
use serde_json::Value;

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    let command = env::args().nth(1);
    let config = IssuePolicyConfig::bundled()?;
    match command.as_deref() {
        Some("pr") => {
            let event = read_event()?;
            let runtime =
                IssuePolicyRuntime::new(config, ReqwestGitHubTransport::from_environment()?);
            let outcome = runtime.check_pull_request_event(&event).await?;
            if !outcome.errors.is_empty() {
                for error in &outcome.errors {
                    println!("::error::{error}");
                }
                bail!("Issue policy 未通过，共 {} 项", outcome.errors.len());
            }
            if outcome.enforced {
                println!("Issue policy 通过。");
            } else {
                println!("PR 尚未进入 Issue policy 强制范围。");
            }
            Ok(())
        }
        Some("lifecycle") => {
            let event = read_event()?;
            let event_name = env::var("GITHUB_EVENT_NAME").unwrap_or_default();
            if !lifecycle_requires_transport(&event_name, &event) {
                return Ok(());
            }
            let runtime =
                IssuePolicyRuntime::new(config, ReqwestGitHubTransport::from_environment()?);
            runtime.handle_lifecycle_event(&event_name, &event).await
        }
        _ => bail!("用法：seekdeep-issue-policy pr|lifecycle"),
    }
}

fn lifecycle_requires_transport(event_name: &str, event: &Value) -> bool {
    if event_name == "issues" {
        return true;
    }
    if !matches!(event_name, "pull_request" | "pull_request_review") {
        return false;
    }
    resolving_issue_status_command(
        event_name,
        &LifecycleEvent {
            action: event
                .get("action")
                .and_then(Value::as_str)
                .map(str::to_owned),
            review_state: event
                .pointer("/review/state")
                .and_then(Value::as_str)
                .map(str::to_owned),
        },
    )
    .is_some()
}

fn read_event() -> Result<Value> {
    let path = env::var_os("GITHUB_EVENT_PATH")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("GITHUB_EVENT_PATH 未设置"))?;
    let source =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(serde_json::from_str(&source)?)
}
