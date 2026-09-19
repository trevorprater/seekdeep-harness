//! Credentialed mirror of the source's Codex real-`DeepSeek` e2e.
//!
//! The nonce round trip is ignored even when `DEEPSEEK_API_KEY` is present because it spends the
//! account's quota; run it deliberately with
//! `DEEPSEEK_API_KEY=... cargo test -p seekdeep-subagent-codex --test real_deepseek_e2e -- --ignored`.
//! The bridge's own guards (route, single task, bearer credential, nonce, official upstream) run
//! keylessly below.

#![cfg(unix)]

use super::support;

use std::{collections::BTreeMap, path::Path, sync::Arc, time::Duration};

use seekdeep_agent::{Agent, AgentOptions, Inbox, NoopInboxNotifications};
use seekdeep_cordis::Context;
use seekdeep_core::session::{Session, SessionHeader, SessionId};
use seekdeep_llm::{AbortSignal, ContentBlock};
use seekdeep_scope::ScopeKey;
use seekdeep_subagent::{SubagentRuntime, SubagentStartRequest, SubagentStopReason};
use seekdeep_subagent_codex::{Config, apply};
use seekdeep_subprocess_local::LocalSubprocessRuntime;
use serde_json::{Map, Value, json};

use support::{
    codex_package::{
        PINNED_CODEX_VERSION, PINNED_CODEX_VERSION_LINE, codex_launcher, codex_path,
        installed_codex_version,
    },
    deepseek_responses_bridge::{
        DeepSeekResponsesBridge, OFFICIAL_DEEPSEEK_BASE_URL, configured_base_url, task_text,
    },
};

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
        "SEEKDEEP_CODEX_DEEPSEEK_{:X}_{:X}",
        now.as_nanos(),
        std::process::id()
    )
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

async fn quiescent(subprocess: &LocalSubprocessRuntime) {
    for _ in 0..200 {
        if subprocess.live_process_count() == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(subprocess.live_process_count(), 0);
}

/// The Codex home configuration pointing the `deepseek-e2e` provider at the bridge.
fn write_codex_config(codex_home: &Path, base_url: &str) -> std::io::Result<()> {
    std::fs::write(
        codex_home.join("config.toml"),
        format!(
            concat!(
                "model = \"deepseek-v4-flash\"\n",
                "model_provider = \"deepseek-e2e\"\n",
                "approval_policy = \"never\"\n",
                "sandbox_mode = \"read-only\"\n",
                "disable_response_storage = true\n",
                "check_for_update_on_startup = false\n\n",
                "[model_providers.deepseek-e2e]\n",
                "name = \"DeepSeek E2E bridge\"\n",
                "base_url = \"{base_url}\"\n",
                "env_key = \"DEEPSEEK_API_KEY\"\n",
                "wire_api = \"responses\"\n",
                "requires_openai_auth = false\n\n",
                "[analytics]\n",
                "enabled = false\n",
            ),
            base_url = base_url
        ),
    )
}

/// The child environment the source passes: the credential, isolated homes, the pinned launcher
/// first on `PATH`, and no proxies.
fn child_env(root: &Path, codex_home: &Path, api_key: String) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("DEEPSEEK_API_KEY".to_owned(), api_key),
        (
            "CODEX_HOME".to_owned(),
            codex_home.to_string_lossy().into_owned(),
        ),
        ("HOME".to_owned(), root.to_string_lossy().into_owned()),
        (
            "XDG_CONFIG_HOME".to_owned(),
            root.join("xdg-config").to_string_lossy().into_owned(),
        ),
        ("PATH".to_owned(), codex_path()),
        ("HTTP_PROXY".to_owned(), String::new()),
        ("HTTPS_PROXY".to_owned(), String::new()),
        ("ALL_PROXY".to_owned(), String::new()),
        ("NO_PROXY".to_owned(), "127.0.0.1,localhost".to_owned()),
    ])
}

fn assert_pinned_codex(env: &BTreeMap<String, String>) -> anyhow::Result<()> {
    let version = std::process::Command::new(codex_launcher())
        .arg("--version")
        .envs(env)
        .output()?;
    anyhow::ensure!(
        version.status.success(),
        "codex --version failed: {}",
        String::from_utf8_lossy(&version.stderr)
    );
    assert_eq!(installed_codex_version()?, PINNED_CODEX_VERSION);
    assert_eq!(
        String::from_utf8(version.stdout)?.trim(),
        PINNED_CODEX_VERSION_LINE
    );
    Ok(())
}

#[tokio::test]
#[ignore = "contacts the real DeepSeek API through real Codex and consumes account quota"]
async fn returns_one_unique_nonce_through_the_production_provider_and_real_codex()
-> anyhow::Result<()> {
    let Some(api_key) = live_key() else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("seekdeep-codex-deepseek-e2e-")
        .tempdir()?;
    let workspace = root.path().join("workspace");
    let codex_home = root.path().join("codex-home");
    std::fs::create_dir(&workspace)?;
    std::fs::create_dir(&codex_home)?;
    let nonce = nonce();
    let bridge = DeepSeekResponsesBridge::start(nonce.clone()).await?;
    write_codex_config(&codex_home, &bridge.base_url)?;
    let env = child_env(root.path(), &codex_home, api_key);
    let context = Context::new();
    let subagents = SubagentRuntime::install(&context)?;
    let subprocess = LocalSubprocessRuntime::install(&context)?;
    apply(
        &context,
        Config {
            env: env.clone(),
            dispose_grace_ms: 2_000.0,
        },
    )?;
    assert_pinned_codex(&env)?;

    let parent = parent(&context, &workspace.to_string_lossy());
    let task = format!("Reply with exactly {nonce} and nothing else. Do not use tools.");
    let run = subagents
        .start(
            "codex",
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
    assert_eq!(bridge.completed_requests(), 1);
    quiescent(&subprocess).await;
    context.fiber().dispose().await?;
    bridge.close();
    Ok(())
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

fn task_body(text: &str) -> Value {
    json!({
        "model": "deepseek-v4-flash",
        "instructions": "ignored while the input carries text",
        "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": text}]}],
    })
}

async fn post_task(
    bridge: &DeepSeekResponsesBridge,
    authorization: Option<&str>,
    body: &Value,
) -> reqwest::Response {
    let mut request = client()
        .post(format!("{}/responses", bridge.base_url))
        .header("content-type", "application/json")
        .body(body.to_string());
    if let Some(authorization) = authorization {
        request = request.header("authorization", authorization);
    }
    request.send().await.unwrap()
}

async fn bridge_failure(response: reqwest::Response) -> Value {
    assert_eq!(response.status().as_u16(), 502);
    assert_eq!(
        response.headers()["content-type"].to_str().unwrap(),
        "application/json"
    );
    serde_json::from_slice(&response.bytes().await.unwrap()).unwrap()
}

#[tokio::test]
async fn only_a_posted_responses_task_is_a_route() {
    let bridge = DeepSeekResponsesBridge::start("NONCE".to_owned())
        .await
        .unwrap();
    let models = client()
        .get(format!("{}/models", bridge.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(models.status().as_u16(), 404);
    let listed = client()
        .get(format!("{}/responses", bridge.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status().as_u16(), 404);
    assert_eq!(bridge.completed_requests(), 0);
    bridge.close();
}

#[tokio::test]
async fn a_task_without_a_bearer_credential_fails_and_a_second_task_conflicts() {
    let bridge = DeepSeekResponsesBridge::start("NONCE".to_owned())
        .await
        .unwrap();
    let body = task_body("Reply with exactly NONCE and nothing else.");
    let failure = bridge_failure(post_task(&bridge, None, &body).await).await;
    assert_eq!(
        failure,
        json!({"error": {"message": "DeepSeek bridge request failed"}})
    );
    let conflict = post_task(&bridge, Some("Bearer key"), &body).await;
    assert_eq!(conflict.status().as_u16(), 409);
    assert_eq!(bridge.completed_requests(), 0);
    bridge.close();
}

#[tokio::test]
async fn a_bare_bearer_prefix_is_no_credential() {
    let bridge = DeepSeekResponsesBridge::start("NONCE".to_owned())
        .await
        .unwrap();
    let body = task_body("Reply with exactly NONCE and nothing else.");
    bridge_failure(post_task(&bridge, Some("Bearer "), &body).await).await;
    assert_eq!(bridge.completed_requests(), 0);
    bridge.close();
}

#[tokio::test]
async fn a_task_that_omits_the_nonce_never_reaches_deepseek() {
    let bridge = DeepSeekResponsesBridge::start("NONCE".to_owned())
        .await
        .unwrap();
    let body = task_body("Reply with exactly PONG and nothing else.");
    bridge_failure(post_task(&bridge, Some("Bearer key"), &body).await).await;
    assert_eq!(bridge.completed_requests(), 0);
    bridge.close();
}

#[test]
fn the_task_is_the_input_text_or_else_the_instructions() {
    let body = |value: Value| -> Map<String, Value> { value.as_object().cloned().unwrap() };
    assert_eq!(
        task_text(&body(json!({
            "instructions": "fallback",
            "input": [
                {"content": [{"type": "input_text", "text": "first"}, "not a part"]},
                "not an item",
                {"content": [{"type": "input_text", "text": "second"}]},
            ],
        }))),
        "first\nsecond"
    );
    assert_eq!(
        task_text(&body(json!({
            "instructions": "fallback",
            "input": [{"content": [{"type": "input_text", "text": "  \n"}]}],
        }))),
        "fallback"
    );
    assert_eq!(task_text(&body(json!({"input": "text"}))), "");
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
    for configured in ["http://127.0.0.1:9/", "", "https://api.deepseek.com/v1"] {
        assert_eq!(
            configured_base_url(Some(configured))
                .unwrap_err()
                .to_string(),
            "Codex DeepSeek e2e requires the official DeepSeek base URL"
        );
    }
}
