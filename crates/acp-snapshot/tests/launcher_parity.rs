//! Real subprocess, ACP stdio, capture, permission, waiter, and shutdown parity.

use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};

use seekdeep_acp::{AcpPermissionHandler, AcpStopReason, PROTOCOL_VERSION};
use seekdeep_acp_snapshot::{
    AcpTestLaunchOptions, AcpTestSignal, AgentUnderTest, launch_acp_test_agent,
};
use serde_json::{Map, Value, json};
use tempfile::TempDir;

fn fixture_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_acp-snapshot-launcher-fixture"))
}

fn options(cwd: &TempDir) -> AcpTestLaunchOptions {
    AcpTestLaunchOptions {
        agent: AgentUnderTest {
            source_bin: fixture_binary(),
            library_bin: Some(fixture_binary()),
            config_path: cwd.path().join("cordis.yml"),
            tsconfig_path: cwd.path().join("tsconfig.json"),
        },
        cwd: cwd.path().to_owned(),
        config_path: None,
        mode: None,
        environment: BTreeMap::new(),
        request_permission: None,
    }
}

fn update_text(update: &Value) -> Option<&str> {
    update
        .get("content")
        .and_then(|content| content.get("text"))
        .and_then(Value::as_str)
}

#[tokio::test]
async fn fake_agent_issues_a_distinct_deterministic_identity_per_session() {
    let cwd = tempfile::tempdir().unwrap();
    let launched = launch_acp_test_agent(options(&cwd)).unwrap();
    launched.client().initialize().await.unwrap();
    let first = launched
        .client()
        .new_session(&cwd.path().to_string_lossy())
        .await
        .unwrap();
    let second = launched
        .client()
        .new_session(&cwd.path().to_string_lossy())
        .await
        .unwrap();
    assert_ne!(first, second);
    assert!(first.as_str().starts_with("11111111-2222-4333-8444-"));
    assert!(second.as_str().starts_with("11111111-2222-4333-8444-"));
    launched.close(None).await.unwrap();
}

#[tokio::test]
async fn launch_drives_real_acp_and_drains_late_inherited_stdio() {
    let cwd = tempfile::tempdir().unwrap();
    let mut options = options(&cwd);
    options
        .environment
        .insert("SEEKDEEP_ACP_FIXTURE_STDERR".into(), "boot note".into());
    options
        .environment
        .insert("SEEKDEEP_ACP_FIXTURE_LATE_OUTPUT".into(), "1".into());
    let launched = launch_acp_test_agent(options).unwrap();
    assert!(launched.pid().is_some());
    let initialized = launched.client().initialize().await.unwrap();
    assert_eq!(initialized["protocolVersion"], json!(PROTOCOL_VERSION));
    let session = launched
        .client()
        .new_session(&cwd.path().to_string_lossy())
        .await
        .unwrap();
    let waiter = tokio::spawn({
        let launched = launched.clone();
        async move {
            launched
                .wait_for_update(Box::new(|update| {
                    Ok(update_text(update) == Some("thinking about it"))
                }))
                .await
        }
    });
    assert_eq!(
        launched
            .client()
            .prompt(&session, vec![json!({"type":"text","text":"hello"})])
            .await
            .unwrap(),
        AcpStopReason::EndTurn
    );
    assert_eq!(
        update_text(&waiter.await.unwrap().unwrap()),
        Some("thinking about it")
    );
    tokio::time::timeout(Duration::from_secs(5), launched.close(None))
        .await
        .unwrap()
        .unwrap();
    assert!(launched.raw_stdout().contains("late inherited stdout"));
    assert!(launched.stderr().contains("boot note"));
    assert!(launched.stderr().contains("late inherited stderr"));
    assert!(
        launched
            .updates()
            .iter()
            .any(|update| update_text(update) == Some("late inherited stdout"))
    );
}

#[tokio::test]
async fn absent_permission_policy_fails_closed_and_custom_policy_selects_by_kind() {
    for (handler, expected) in [
        (None, "cancelled"),
        (Some(reject_once_handler()), "opt-reject"),
    ] {
        let cwd = tempfile::tempdir().unwrap();
        let mut options = options(&cwd);
        options
            .environment
            .insert("SEEKDEEP_ACP_FIXTURE_PERMISSION".into(), "1".into());
        options.request_permission = handler;
        let launched = launch_acp_test_agent(options).unwrap();
        launched.client().initialize().await.unwrap();
        let session = launched
            .client()
            .new_session(&cwd.path().to_string_lossy())
            .await
            .unwrap();
        launched
            .client()
            .prompt(&session, vec![json!({"type":"text","text":"permission"})])
            .await
            .unwrap();
        assert!(
            launched
                .updates()
                .iter()
                .any(|update| { update_text(update).is_some_and(|text| text.contains(expected)) })
        );
        launched.close(None).await.unwrap();
    }
}

#[tokio::test]
async fn waiter_predicate_failures_propagate_and_stream_close_rejects_pending_waiters() {
    let cwd = tempfile::tempdir().unwrap();
    let launched = launch_acp_test_agent(options(&cwd)).unwrap();
    launched.client().initialize().await.unwrap();
    let session = launched
        .client()
        .new_session(&cwd.path().to_string_lossy())
        .await
        .unwrap();
    let failed = tokio::spawn({
        let launched = launched.clone();
        async move {
            launched
                .wait_for_update(Box::new(|_update| anyhow::bail!("predicate failed")))
                .await
        }
    });
    launched
        .client()
        .prompt(&session, vec![json!({"type":"text","text":"hello"})])
        .await
        .unwrap();
    assert_eq!(
        failed.await.unwrap().unwrap_err().to_string(),
        "predicate failed"
    );

    let pending = tokio::spawn({
        let launched = launched.clone();
        async move {
            launched
                .wait_for_update(Box::new(|_update| Ok(false)))
                .await
        }
    });
    launched.close(None).await.unwrap();
    let expected = "ACP test agent update stream closed before a matching session update arrived";
    assert_eq!(pending.await.unwrap().unwrap_err().to_string(), expected);
    assert_eq!(
        launched
            .wait_for_update(Box::new(|_update| Ok(true)))
            .await
            .unwrap_err()
            .to_string(),
        expected
    );
}

#[tokio::test]
async fn update_waiters_run_newest_first_like_the_source_listener_array() {
    let cwd = tempfile::tempdir().unwrap();
    let launched = launch_acp_test_agent(options(&cwd)).unwrap();
    launched.client().initialize().await.unwrap();
    let session = launched
        .client()
        .new_session(&cwd.path().to_string_lossy())
        .await
        .unwrap();
    let order = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let first = tokio::spawn({
        let launched = launched.clone();
        let order = order.clone();
        async move {
            launched
                .wait_for_update(Box::new(move |_update| {
                    order.lock().push("first");
                    Ok(true)
                }))
                .await
        }
    });
    tokio::task::yield_now().await;
    let second = tokio::spawn({
        let launched = launched.clone();
        let order = order.clone();
        async move {
            launched
                .wait_for_update(Box::new(move |_update| {
                    order.lock().push("second");
                    Ok(true)
                }))
                .await
        }
    });
    tokio::task::yield_now().await;
    launched
        .client()
        .prompt(&session, vec![json!({"type":"text","text":"order"})])
        .await
        .unwrap();
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    assert_eq!(order.lock().as_slice(), ["second", "first"]);
    launched.close(None).await.unwrap();
}

#[tokio::test]
async fn explicit_signal_terminates_a_live_child_and_close_is_idempotent() {
    let cwd = tempfile::tempdir().unwrap();
    let launched = launch_acp_test_agent(options(&cwd)).unwrap();
    launched.client().initialize().await.unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        launched.close(Some(AcpTestSignal::Terminate)),
    )
    .await
    .unwrap()
    .unwrap();
    launched.close(Some(AcpTestSignal::Kill)).await.unwrap();
}

#[tokio::test]
async fn launch_layers_custom_environment_but_owns_both_isolated_homes() {
    let cwd = tempfile::tempdir().unwrap();
    let mut options = options(&cwd);
    options
        .environment
        .insert("SEEKDEEP_ACP_FIXTURE_ECHO_ENV".into(), "1".into());
    options
        .environment
        .insert("SEEKDEEP_ACP_FIXTURE_CUSTOM".into(), "layered".into());
    options
        .environment
        .insert("SEEKDEEP_HOME".into(), "wrong".into());
    options
        .environment
        .insert("SEEKDEEP_AGENTS_HOME".into(), "wrong".into());
    let launched = launch_acp_test_agent(options).unwrap();
    launched.client().initialize().await.unwrap();
    let session = launched
        .client()
        .new_session(&cwd.path().to_string_lossy())
        .await
        .unwrap();
    launched
        .client()
        .prompt(&session, vec![json!({"type":"text","text":"env"})])
        .await
        .unwrap();
    let environment = launched
        .updates()
        .into_iter()
        .filter_map(|update| update_text(&update).map(str::to_owned))
        .find(|text| text.starts_with("env:"))
        .unwrap();
    assert!(environment.contains("layered"));
    assert!(environment.contains(&cwd.path().join(".seekdeep").to_string_lossy().to_string()));
    assert!(environment.contains(&cwd.path().join(".agents").to_string_lossy().to_string()));
    assert!(!environment.contains("wrong"));
    launched.close(None).await.unwrap();
}

#[tokio::test]
async fn early_nonzero_exit_rejects_protocol_startup_and_close_drains_stderr() {
    let cwd = tempfile::tempdir().unwrap();
    let mut options = options(&cwd);
    options
        .environment
        .insert("SEEKDEEP_ACP_FIXTURE_FAIL_BOOT".into(), "1".into());
    options
        .environment
        .insert("SEEKDEEP_ACP_FIXTURE_STDERR".into(), "boot failed".into());
    let launched = launch_acp_test_agent(options).unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), launched.client().initialize())
            .await
            .unwrap()
            .is_err()
    );
    launched.close(None).await.unwrap();
    assert!(launched.stderr().contains("boot failed"));
}

#[tokio::test]
async fn missing_binary_and_library_mode_resolution_fail_before_a_launch_handle_exists() {
    let cwd = tempfile::tempdir().unwrap();
    let mut missing = options(&cwd);
    missing.agent.source_bin = cwd.path().join("missing-agent");
    assert!(launch_acp_test_agent(missing).is_err());

    let mut library = options(&cwd);
    library.mode = Some(seekdeep_loader_smoke::ExampleMode::Library);
    library.agent.library_bin = None;
    assert!(
        launch_acp_test_agent(library)
            .unwrap_err()
            .to_string()
            .contains("libraryBin")
    );
}

#[test]
fn launch_without_a_tokio_runtime_fails_before_spawning_the_child() {
    let cwd = tempfile::tempdir().unwrap();
    assert_eq!(
        launch_acp_test_agent(options(&cwd))
            .unwrap_err()
            .to_string(),
        "launchAcpTestAgent requires an active Tokio runtime"
    );
}

fn reject_once_handler() -> AcpPermissionHandler {
    Arc::new(|params: Map<String, Value>| {
        Box::pin(async move {
            let option = params
                .get("options")
                .and_then(Value::as_array)
                .and_then(|options| {
                    options.iter().find(|option| {
                        option.get("kind").and_then(Value::as_str) == Some("reject_once")
                    })
                })
                .and_then(|option| option.get("optionId"))
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("reject_once was not offered"))?;
            Ok(json!({"outcome":{"outcome":"selected","optionId":option}}))
        })
    })
}
