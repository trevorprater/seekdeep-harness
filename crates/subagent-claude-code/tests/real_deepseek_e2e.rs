//! Credentialed mirror of the source's Claude Code real-`DeepSeek` e2e.
//!
//! The nonce round trip is ignored even when `DEEPSEEK_API_KEY` is present because it spends the
//! account's quota; run it deliberately with
//! `DEEPSEEK_API_KEY=... cargo test -p seekdeep-subagent-claude-code --test real_deepseek_e2e -- --ignored`.
//! Like the source, it drives the pinned `@anthropic-ai/claude-agent-sdk` platform CLI from the
//! package's own installation, never a host-installed `claude`; the pure helpers (the official
//! upstream guard and the platform package layout) run keylessly below.

#![cfg(unix)]

mod support;

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use seekdeep_agent::{Agent, AgentOptions, Inbox, NoopInboxNotifications};
use seekdeep_cordis::Context;
use seekdeep_core::session::{Session, SessionHeader, SessionId};
use seekdeep_llm::{AbortSignal, ContentBlock};
use seekdeep_scope::ScopeKey;
use seekdeep_subagent::{SubagentRuntime, SubagentStartRequest, SubagentStopReason};
use seekdeep_subagent_claude_code::{Config, apply};
use seekdeep_subprocess_local::LocalSubprocessRuntime;

use support::sdk_package::{PinnedCli, claude_bin, platform_package};

/// The only upstream the credential may be spent against.
const OFFICIAL_DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com";

fn live_key() -> Option<String> {
    std::env::var("DEEPSEEK_API_KEY")
        .ok()
        .filter(|key| !key.is_empty())
}

/// A per-run nonce without ambient randomness: the clock and the process identity.
fn nonce() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is past the epoch");
    format!(
        "SEEKDEEP_CLAUDE_DEEPSEEK_{:X}_{:X}",
        now.as_nanos(),
        std::process::id()
    )
}

/// Accepts only the official `DeepSeek` endpoint (trailing slashes ignored) as the upstream.
fn configured_base_url(configured: Option<&str>) -> anyhow::Result<String> {
    let configured = configured
        .unwrap_or(OFFICIAL_DEEPSEEK_BASE_URL)
        .trim_end_matches('/');
    anyhow::ensure!(
        configured == OFFICIAL_DEEPSEEK_BASE_URL,
        "Claude Code DeepSeek e2e requires the official DeepSeek base URL"
    );
    Ok(configured.to_owned())
}

fn deep_seek_base_url() -> anyhow::Result<String> {
    let configured = std::env::var("DEEPSEEK_BASE_URL").ok();
    configured_base_url(configured.as_deref())
}

fn parent(context: &Context, cwd: &str) -> Arc<Agent> {
    let id = SessionId::new("deepseek-e2e-parent");
    let mut header = SessionHeader::new(id.clone());
    header.cwd = Some(cwd.to_owned());
    let session = Session::create(&id, None, Some(header)).unwrap();
    let inbox = Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
    Arc::new(Agent::new(
        id,
        AgentOptions::default(),
        session,
        inbox,
        context.clone(),
        ScopeKey::new(),
    ))
}

fn result_text(result: &seekdeep_subagent::SubagentResult) -> String {
    result
        .output
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str().expect("the answer is scalar text")),
            _ => None,
        })
        .collect()
}

async fn quiescent(runtime: &LocalSubprocessRuntime) {
    for _ in 0..600 {
        if runtime.live_process_count() == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(runtime.live_process_count(), 0);
}

/// The isolated homes the source creates under one temporary root.
struct Homes {
    workspace: PathBuf,
    claude_config: PathBuf,
    xdg_config: PathBuf,
    xdg_cache: PathBuf,
    xdg_data: PathBuf,
    xdg_state: PathBuf,
}

fn homes(root: &Path) -> std::io::Result<Homes> {
    let homes = Homes {
        workspace: root.join("workspace"),
        claude_config: root.join("claude-config"),
        xdg_config: root.join("xdg-config"),
        xdg_cache: root.join("xdg-cache"),
        xdg_data: root.join("xdg-data"),
        xdg_state: root.join("xdg-state"),
    };
    for directory in [
        &homes.workspace,
        &homes.claude_config,
        &homes.xdg_config,
        &homes.xdg_cache,
        &homes.xdg_data,
        &homes.xdg_state,
    ] {
        std::fs::create_dir(directory)?;
    }
    Ok(homes)
}

/// The child environment the source passes: the pinned CLI first on `PATH`, the credential as
/// the Anthropic auth token against the official `DeepSeek` Anthropic surface, `DeepSeek` models
/// for every tier, isolated homes, no nonessential traffic, and no proxies.
fn child_env(
    root: &Path,
    homes: &Homes,
    bin_dir: &Path,
    api_key: String,
) -> anyhow::Result<BTreeMap<String, String>> {
    let path = std::env::join_paths(std::iter::once(bin_dir.to_path_buf()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))?
    .to_string_lossy()
    .into_owned();
    let directory = |directory: &Path| directory.to_string_lossy().into_owned();
    let text = |value: &str| value.to_owned();
    Ok(BTreeMap::from([
        ("PATH".to_owned(), path),
        ("ANTHROPIC_AUTH_TOKEN".to_owned(), api_key),
        (
            "ANTHROPIC_BASE_URL".to_owned(),
            format!("{}/anthropic", deep_seek_base_url()?),
        ),
        ("ANTHROPIC_MODEL".to_owned(), text("deepseek-v4-pro[1m]")),
        (
            "ANTHROPIC_DEFAULT_OPUS_MODEL".to_owned(),
            text("deepseek-v4-pro[1m]"),
        ),
        (
            "ANTHROPIC_DEFAULT_SONNET_MODEL".to_owned(),
            text("deepseek-v4-pro[1m]"),
        ),
        (
            "ANTHROPIC_DEFAULT_HAIKU_MODEL".to_owned(),
            text("deepseek-v4-flash"),
        ),
        (
            "CLAUDE_CODE_SUBAGENT_MODEL".to_owned(),
            text("deepseek-v4-flash"),
        ),
        ("CLAUDE_CODE_EFFORT_LEVEL".to_owned(), text("max")),
        (
            "CLAUDE_CONFIG_DIR".to_owned(),
            directory(&homes.claude_config),
        ),
        ("HOME".to_owned(), directory(root)),
        ("XDG_CONFIG_HOME".to_owned(), directory(&homes.xdg_config)),
        ("XDG_CACHE_HOME".to_owned(), directory(&homes.xdg_cache)),
        ("XDG_DATA_HOME".to_owned(), directory(&homes.xdg_data)),
        ("XDG_STATE_HOME".to_owned(), directory(&homes.xdg_state)),
        (
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".to_owned(),
            text("1"),
        ),
        (
            "CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL".to_owned(),
            text("1"),
        ),
        ("DISABLE_TELEMETRY".to_owned(), text("1")),
        ("DISABLE_ERROR_REPORTING".to_owned(), text("1")),
        ("HTTP_PROXY".to_owned(), String::new()),
        ("HTTPS_PROXY".to_owned(), String::new()),
        ("ALL_PROXY".to_owned(), String::new()),
        ("NO_PROXY".to_owned(), text("127.0.0.1,localhost")),
    ]))
}

#[tokio::test]
#[ignore = "contacts the real DeepSeek API through the real Claude Code CLI and consumes account quota"]
async fn returns_one_unique_nonce_through_the_production_provider_and_real_sdk_cli()
-> anyhow::Result<()> {
    let Some(api_key) = live_key() else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("seekdeep-claude-deepseek-e2e-")
        .tempdir()?;
    let homes = homes(root.path())?;
    let pinned = PinnedCli::resolve()?;
    let bin_dir = pinned
        .claude_bin
        .parent()
        .expect("the CLI sits inside its platform package");
    let env = child_env(root.path(), &homes, bin_dir, api_key)?;
    let context = Context::new();
    let subagents = SubagentRuntime::install(&context)?;
    let runtime = LocalSubprocessRuntime::install(&context)?;
    apply(
        &context,
        Config {
            env: env.clone(),
            dispose_grace_ms: 3_000.0,
        },
    )?;
    pinned.assert_versions(&pinned.claude_bin, &env)?;

    let nonce = nonce();
    let parent = parent(&context, &homes.workspace.to_string_lossy());
    let task = format!("Reply with exactly {nonce} and nothing else. Do not use tools.");
    let run = subagents
        .start(
            "claude-code",
            SubagentStartRequest {
                label: None,
                prompt: vec![ContentBlock::Text {
                    text: task.as_str().into(),
                }],
                parent,
                signal: AbortSignal::default(),
                agent_options: None,
                output_schema: None,
                max_depth: None,
                tool_filter: None,
                persona: None,
            },
        )
        .await?;
    let result = tokio::time::timeout(Duration::from_secs(180), run.result()).await??;
    run.dispose().await?;

    assert_eq!(result.stop_reason, SubagentStopReason::Completed);
    assert_eq!(result_text(&result).trim(), nonce);
    quiescent(&runtime).await;
    context.fiber().dispose().await?;
    Ok(())
}

#[test]
fn only_the_official_endpoint_may_receive_the_credential() {
    assert_eq!(
        configured_base_url(None).unwrap(),
        OFFICIAL_DEEPSEEK_BASE_URL
    );
    assert_eq!(
        configured_base_url(Some("https://api.deepseek.com///")).unwrap(),
        OFFICIAL_DEEPSEEK_BASE_URL
    );
    for configured in [
        "http://127.0.0.1:9/",
        "",
        "https://api.deepseek.com/anthropic",
    ] {
        assert_eq!(
            configured_base_url(Some(configured))
                .unwrap_err()
                .to_string(),
            "Claude Code DeepSeek e2e requires the official DeepSeek base URL"
        );
    }
}

#[test]
fn the_cli_lives_in_the_platform_package_beside_the_sdk() {
    assert_eq!(
        platform_package("darwin", "arm64"),
        "@anthropic-ai/claude-agent-sdk-darwin-arm64"
    );
    let sdk_root = Path::new("/store/@anthropic-ai/claude-agent-sdk");
    assert_eq!(
        claude_bin(sdk_root, &platform_package("linux", "x64"), "linux"),
        Path::new("/store/@anthropic-ai/claude-agent-sdk/../claude-agent-sdk-linux-x64/claude")
    );
    assert_eq!(
        claude_bin(sdk_root, &platform_package("win32", "x64"), "win32"),
        Path::new("/store/@anthropic-ai/claude-agent-sdk/../claude-agent-sdk-win32-x64/claude.exe")
    );
}
