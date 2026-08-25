//! In-process composition and real binary/Loader ACP acceptance.

use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream;
use parking_lot::Mutex;
use seekdeep_acp::{AcpClient, AcpRuntime, PermissionPolicy};
use seekdeep_acp_demo::{Config, apply_with_runtime};
use seekdeep_cordis::Context;
use seekdeep_llm::{AdapterStream, FinishReason, GenerateOptions, LlmAdapter, StreamChunk};
use serde_json::json;

#[derive(Debug)]
struct MockAdapter;

#[async_trait]
impl LlmAdapter for MockAdapter {
    fn stream(&self, _options: GenerateOptions) -> AdapterStream {
        AdapterStream::new(stream::iter([
            Ok(StreamChunk::TextDelta {
                index: 0,
                text: "ACP RUST OK".to_owned(),
            }),
            Ok(StreamChunk::Finish {
                reason: FinishReason::Stop,
                replay_state: None,
            }),
        ]))
    }
}

fn config(root: &std::path::Path) -> Config {
    serde_json::from_value(json!({
        "provider":"mock",
        "model":"mock",
        "persistenceRoot":root,
        "persistenceCompression":"none",
        "workspaceContext":false,
        "skills":{"enabled":false},
        "toolBash":false,
        "toolJobs":false,
        "goals":false
    }))
    .unwrap()
}

#[tokio::test]
async fn in_process_app_mounts_spine_persistence_query_checkpoint_and_acp() {
    let root = tempfile::tempdir().unwrap();
    let context = Context::new();
    let (server_io, client_io) = tokio::io::duplex(256 * 1024);
    let (server_read, server_write) = tokio::io::split(server_io);
    let (client_read, client_write) = tokio::io::split(client_io);
    let runtime = apply_with_runtime(
        &context,
        {
            let mut config = config(root.path());
            config.goals = None;
            config
        },
        Some(AcpRuntime {
            input: Box::pin(server_read),
            output: Box::pin(server_write),
        }),
    )
    .await
    .unwrap();
    assert!(
        context
            .get(seekdeep_session_persistence::SESSION_PERSISTENCE)
            .is_some()
    );
    assert!(context.get(seekdeep_session_query::SESSION_QUERY).is_some());
    assert!(context.get(seekdeep_acp::ACP_BRIDGE).is_some());
    assert!(runtime.spine.agents.list().is_empty());
    assert!(runtime.spine.tools.get("get_goal", None).is_some());
    runtime
        .spine
        .llm
        .register_adapter(&["mock".to_owned()], Arc::new(MockAdapter))
        .unwrap();
    let transport = seekdeep_sdk_protocol::JsonRpcLineTransport::new(client_read, client_write);
    let client = AcpClient::new(&transport, PermissionPolicy::Reject);
    let updates = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&updates);
    client.on_update(Arc::new(move |update| observed.lock().push(update.clone())));
    client.start();
    client.initialize().await.unwrap();
    let session = client
        .new_session(&root.path().to_string_lossy())
        .await
        .unwrap();
    assert!(
        runtime
            .spine
            .agents
            .get(&seekdeep_core::session::SessionId::new(session.as_str()))
            .is_some()
    );
    assert_eq!(
        client
            .prompt(&session, vec![json!({"type":"text","text":"reply"})])
            .await
            .unwrap(),
        seekdeep_acp::AcpStopReason::EndTurn
    );
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while updates.lock().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        updates.lock()[0].update.pointer("/content/text"),
        Some(&json!("ACP RUST OK"))
    );
    client.shutdown_output().await.unwrap();
    runtime.bridge.connection_closed_signal().cancelled().await;
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn compiled_binary_boots_loader_negotiates_fresh_session_and_exits_on_eof() {
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("cordis.yml");
    let yaml = format!(
        concat!(
            "- id: acp\n",
            "  name: seekdeep-acp-demo\n",
            "  config:\n",
            "    provider: mock\n",
            "    model: mock\n",
            "    persistenceRoot: {}\n",
            "    persistenceCompression: none\n",
            "    workspaceContext: false\n",
            "    skills: {{ enabled: false }}\n",
            "    toolBash: false\n",
            "    toolJobs: false\n",
            "    goals: false\n",
        ),
        serde_json::to_string(&root.path().join("sessions").to_string_lossy()).unwrap()
    );
    std::fs::write(&config_path, yaml).unwrap();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_seekdeep-acp-demo"))
        .arg("--config")
        .arg(&config_path)
        .current_dir(root.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let input = child.stdout.take().unwrap();
    let output = child.stdin.take().unwrap();
    let client = AcpClient::from_boxed(Box::pin(input), Box::pin(output), PermissionPolicy::Reject);
    client.start();
    if let Err(error) = client.initialize().await {
        let status = child.wait().await.unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let mut bytes = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut stderr, &mut bytes)
            .await
            .unwrap();
        panic!(
            "ACP demo initialization failed ({status}): {error:#}\n{}",
            String::from_utf8_lossy(&bytes)
        );
    }
    let session = client
        .new_session(&root.path().to_string_lossy())
        .await
        .unwrap();
    assert!(!session.as_str().is_empty());
    client.shutdown_output().await.unwrap();
    let status = tokio::time::timeout(std::time::Duration::from_secs(30), child.wait())
        .await
        .unwrap()
        .unwrap();
    if !status.success() {
        let mut stderr = child.stderr.take().unwrap();
        let mut bytes = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut stderr, &mut bytes)
            .await
            .unwrap();
        panic!("ACP demo failed: {}", String::from_utf8_lossy(&bytes));
    }
}

#[test]
fn config_requires_provider_model_and_workspace_policy() {
    assert!(serde_json::from_value::<Config>(json!({})).is_err());
    assert!(
        serde_json::from_value::<Config>(json!({
            "provider":"p","model":"m"
        }))
        .is_err()
    );
}
