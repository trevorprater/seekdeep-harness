//! Low-level process, timeout, subscription, high-level run, and teardown parity.

use std::{collections::BTreeMap, sync::Arc};

use seekdeep_core::session::SessionId;
use seekdeep_llm::ContentBlock;
use seekdeep_sdk_client::{
    DeepSeekHarness, DeepSeekHarnessOptions, HarnessClient, HarnessClientOptions,
    JsonRpcResponseError, RequestTimeoutError, RunOptions, SdkProtocolError, TransportClosedError,
};
use seekdeep_sdk_protocol::InitializeParams;
use serde_json::{Map, Value, json};

fn options(mode: &str) -> HarnessClientOptions {
    let mut options = HarnessClientOptions::new(env!("CARGO_BIN_EXE_seekdeep-sdk-fake-runtime"));
    options.args = vec![mode.to_owned()];
    options.cwd = Some(
        std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
    );
    options.env = Some(
        std::env::vars()
            .filter(|(key, _)| !key.to_ascii_uppercase().contains("KEY"))
            .collect::<BTreeMap<_, _>>(),
    );
    options.request_timeout_ms = Some(2_000.0);
    options.dispose_eof_grace_ms = 100.0;
    options.dispose_grace_ms = 500.0;
    options
}

#[tokio::test]
async fn low_level_client_initializes_prompts_fans_out_and_closes_idempotently() {
    let client = HarnessClient::new(options("normal"));
    let subscription = client.subscribe(None);
    let initialized = client
        .initialize(InitializeParams {
            cwd: std::env::current_dir()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            provider: seekdeep_llm::ProviderId::new("mock"),
            model: seekdeep_llm::ModelId::new("model"),
            max_tokens: Some(100),
        })
        .await
        .unwrap();
    assert_eq!(initialized.server_info.name, "seekdeep-harness-sdk-runtime");
    assert_eq!(initialized.server_info.version, "0.0.1");
    assert_eq!(
        client
            .prompt(
                SessionId::new("root"),
                vec![ContentBlock::Text { text: "hi".into() }]
            )
            .await
            .unwrap()
            .as_str(),
        "message-1"
    );
    let methods = futures::future::join_all((0..13).map(|_| subscription.next()))
        .await
        .into_iter()
        .map(|result| result.unwrap().method)
        .collect::<Vec<_>>();
    assert_eq!(
        methods,
        [
            "session.event",
            "session.event",
            "session.status",
            "session.event",
            "session.event",
            "subagent.started",
            "session.event",
            "subagent.started",
            "session.event",
            "session.event",
            "subagent.finished",
            "subagent.finished",
            "session.status"
        ]
    );
    client.close().await.unwrap();
    client.close().await.unwrap();
    let failure = subscription.next().await.unwrap_err().to_string();
    assert!(failure.contains("runtime"), "{failure}");
}

#[tokio::test]
async fn high_level_run_owns_the_receipt_to_idle_interval_and_descendant_lineage() {
    let harness = DeepSeekHarness::new(DeepSeekHarnessOptions {
        launch: options("normal"),
        cwd: Some(".".to_owned()),
        provider: Some("mock".to_owned()),
        model: Some("model".to_owned()),
        max_tokens: Some(100),
    })
    .unwrap();
    let result = harness
        .run(
            "task",
            RunOptions {
                session_id: Some(SessionId::new("root")),
                on_notification: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(result.session_id.as_str(), "root");
    assert_eq!(result.final_response, "fixture final");
    assert_eq!(
        result
            .events
            .iter()
            .map(|event| event.event_type.as_str())
            .collect::<Vec<_>>(),
        ["agent/inbox/spliced", "assistant/message", "turn/end"]
    );
    assert_eq!(
        result
            .notifications
            .last()
            .map(|notification| notification.method.as_str()),
        Some("session.status")
    );
    harness.close().await.unwrap();
}

#[tokio::test]
async fn request_timeout_abandons_correlation_and_runtime_remains_shutdown_capable() {
    let mut configured = options("normal");
    configured.request_timeout_ms = None;
    let client = HarnessClient::new(configured);
    let error = client
        .request("hold", Map::new(), Some(10.0))
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<RequestTimeoutError>().is_some());
    assert!(error.to_string().contains("hold timed out after 10ms"));
    client.close().await.unwrap();
}

#[tokio::test]
async fn malformed_handshake_and_early_exit_surface_typed_protocol_and_process_context() {
    let bad = HarnessClient::new(options("bad-init"));
    let error = bad
        .initialize(InitializeParams {
            cwd: ".".to_owned(),
            provider: seekdeep_llm::ProviderId::new("p"),
            model: seekdeep_llm::ModelId::new("m"),
            max_tokens: None,
        })
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<SdkProtocolError>().is_some());
    bad.close().await.unwrap();

    let dead = HarnessClient::new(options("exit-error"));
    let error = dead
        .request("initialize", Map::new(), None)
        .await
        .unwrap_err();
    assert!(
        error.downcast_ref::<TransportClosedError>().is_some(),
        "{error:?}"
    );
    let message = error.to_string();
    assert!(message.contains("exit code: 7"), "{message}");
    assert!(
        message.contains("fixture runtime stderr marker"),
        "{message}"
    );
    dead.close().await.unwrap();
}

#[tokio::test]
async fn subscriptions_filter_isolate_failures_and_session_tree_follows_descendants() {
    let client = HarnessClient::new(options("normal"));
    let tree = client.subscribe_session_tree(&SessionId::new("root"));
    let broken = client.subscribe(Some(Arc::new(|_| panic!("filter failure"))));
    client
        .initialize(InitializeParams {
            cwd: ".".to_owned(),
            provider: seekdeep_llm::ProviderId::new("p"),
            model: seekdeep_llm::ModelId::new("m"),
            max_tokens: None,
        })
        .await
        .unwrap();
    client
        .prompt(
            SessionId::new("root"),
            vec![ContentBlock::Text { text: "x".into() }],
        )
        .await
        .unwrap();
    assert!(
        broken
            .next()
            .await
            .unwrap_err()
            .to_string()
            .contains("filter failure")
    );
    let tree_items = futures::future::join_all((0..12).map(|_| tree.next()))
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect::<Vec<_>>();
    assert!(tree_items.iter().any(|notification| {
        notification.method == "session.event"
            && notification.params.get("sessionId") == Some(&json!("child-1"))
    }));
    assert!(tree_items.iter().any(|notification| {
        notification.method == "subagent.finished"
            && notification.params.get("childSessionId") == Some(&json!("grandchild-1"))
    }));
    assert!(
        !tree_items.iter().any(|notification| {
            notification.params.get("sessionId") == Some(&json!("stranger"))
        })
    );
    tree.close();
    client.close().await.unwrap();
}

#[tokio::test]
async fn handshake_payload_is_exact_once_and_relative_launch_cwd_is_absolute_on_the_wire() {
    let workspace = tempfile::tempdir().unwrap();
    let record = workspace.path().join("initialize.jsonl");
    let mode = format!("record-init={}", record.display());
    let harness = DeepSeekHarness::new(DeepSeekHarnessOptions {
        launch: options(&mode),
        cwd: Some(workspace.path().to_string_lossy().into_owned()),
        provider: Some("configured-provider".to_owned()),
        model: Some("configured-model".to_owned()),
        max_tokens: Some(4_096),
    })
    .unwrap();
    for session in ["one", "two"] {
        harness
            .run(
                "task",
                RunOptions {
                    session_id: Some(SessionId::new(session)),
                    on_notification: None,
                },
            )
            .await
            .unwrap();
    }
    harness.close().await.unwrap();
    let records = std::fs::read_to_string(record)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        records,
        [json!({
            "cwd":workspace.path().to_string_lossy(),
            "provider":"configured-provider",
            "model":"configured-model",
            "maxTokens":4_096
        })]
    );

    let current = std::env::current_dir().unwrap();
    let root = tempfile::Builder::new()
        .prefix("sdk-client-relative-")
        .tempdir_in(&current)
        .unwrap();
    let worker = root.path().join("worker");
    std::fs::create_dir(&worker).unwrap();
    let relative = worker.strip_prefix(&current).unwrap();
    let record = root.path().join("cwd.jsonl");
    let mode = format!("record-cwd={}", record.display());
    let mut launch = options(&mode);
    launch.cwd = Some(relative.to_string_lossy().into_owned());
    let harness = DeepSeekHarness::new(DeepSeekHarnessOptions {
        launch,
        cwd: None,
        provider: Some("p".to_owned()),
        model: Some("m".to_owned()),
        max_tokens: None,
    })
    .unwrap();
    harness.start().await.unwrap();
    let identity = harness
        .client()
        .initialize(InitializeParams {
            cwd: worker.to_string_lossy().into_owned(),
            provider: seekdeep_llm::ProviderId::new("p"),
            model: seekdeep_llm::ModelId::new("m"),
            max_tokens: None,
        })
        .await
        .unwrap();
    assert_eq!(
        std::path::Path::new(&identity.server_info.version),
        std::fs::canonicalize(&worker).unwrap()
    );
    harness.close().await.unwrap();
    let wire_cwds = std::fs::read_to_string(record)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap()["cwd"].clone())
        .collect::<Vec<_>>();
    assert_eq!(wire_cwds, [json!(worker), json!(worker)]);
}

#[tokio::test]
async fn response_errors_retry_with_a_fresh_process_and_close_is_typed_and_terminal() {
    let client = HarnessClient::new(options("init-error"));
    let error = client
        .initialize(InitializeParams {
            cwd: ".".to_owned(),
            provider: seekdeep_llm::ProviderId::new("p"),
            model: seekdeep_llm::ModelId::new("m"),
            max_tokens: None,
        })
        .await
        .unwrap_err();
    let response = error.downcast_ref::<JsonRpcResponseError>().unwrap();
    assert_eq!(response.code, Some(7));
    assert_eq!(response.message, "scripted init failure");
    assert_eq!(response.data, Some(json!({"hint":"fake"})));
    client.close().await.unwrap();

    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("failed-once");
    let mode = format!("init-error-once={}", marker.display());
    let harness = DeepSeekHarness::new(DeepSeekHarnessOptions {
        launch: options(&mode),
        cwd: Some(directory.path().to_string_lossy().into_owned()),
        provider: Some("p".to_owned()),
        model: Some("m".to_owned()),
        max_tokens: None,
    })
    .unwrap();
    let first_client = harness.client();
    let first_error = harness.start().await.unwrap_err();
    assert!(first_error.downcast_ref::<JsonRpcResponseError>().is_some());
    let run = harness.run("retry", RunOptions::default()).await.unwrap();
    assert_eq!(run.final_response, "fixture final");
    assert!(!Arc::ptr_eq(&first_client, &harness.client()));
    harness.close().await.unwrap();
    let error = harness
        .run("after close", RunOptions::default())
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<TransportClosedError>().is_some());
}

#[tokio::test]
async fn malformed_prompt_and_event_payloads_are_typed_protocol_failures() {
    let client = HarnessClient::new(options("bad-prompt"));
    let error = client
        .prompt(
            SessionId::new("bad-prompt"),
            vec![ContentBlock::Text { text: "x".into() }],
        )
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<SdkProtocolError>().is_some());
    client.close().await.unwrap();

    for (mode, expected) in [
        ("malformed-event", "no event envelope"),
        ("malformed-message", "malformed content"),
        ("message-no-data", "malformed content"),
    ] {
        let harness = DeepSeekHarness::new(DeepSeekHarnessOptions {
            launch: options(mode),
            cwd: Some(".".to_owned()),
            provider: Some("p".to_owned()),
            model: Some("m".to_owned()),
            max_tokens: None,
        })
        .unwrap();
        let error = harness
            .run("bad event", RunOptions::default())
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<SdkProtocolError>().is_some());
        assert!(error.to_string().contains(expected), "{error}");
        harness.close().await.unwrap();
    }
}

#[tokio::test]
async fn client_wide_timeouts_abandon_every_request_and_allow_shutdown() {
    let mut configured = options("hang-prompt");
    configured.request_timeout_ms = Some(20.0);
    let client = HarnessClient::new(configured);
    for _ in 0..3 {
        let error = client
            .prompt(
                SessionId::new("hung"),
                vec![ContentBlock::Text { text: "x".into() }],
            )
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<RequestTimeoutError>().is_some());
    }
    client.close().await.unwrap();
}

#[tokio::test]
async fn subscription_queue_and_waiter_failure_rules_match_runtime_lifecycle() {
    let client = HarnessClient::new(options("normal"));
    client
        .initialize(InitializeParams {
            cwd: ".".to_owned(),
            provider: seekdeep_llm::ProviderId::new("p"),
            model: seekdeep_llm::ModelId::new("m"),
            max_tokens: None,
        })
        .await
        .unwrap();
    let manually_closed = client.subscribe(None);
    let drainable = client.subscribe(None);
    client
        .prompt(
            SessionId::new("queues"),
            vec![ContentBlock::Text { text: "x".into() }],
        )
        .await
        .unwrap();
    assert!(manually_closed.try_next().is_some());
    manually_closed.close();
    assert!(manually_closed.try_next().is_none());
    assert!(
        manually_closed
            .next()
            .await
            .unwrap_err()
            .to_string()
            .contains("subscription closed")
    );
    client.close().await.unwrap();
    assert!(drainable.try_next().is_some());
    let born_failed = client.subscribe(None);
    assert!(
        born_failed
            .next()
            .await
            .unwrap_err()
            .downcast_ref::<TransportClosedError>()
            .is_some()
    );

    let untouched = HarnessClient::new(options("normal"));
    let parked = untouched.subscribe(None);
    untouched.close().await.unwrap();
    assert!(
        parked
            .next()
            .await
            .unwrap_err()
            .to_string()
            .contains("runtime closed")
    );
}

#[tokio::test]
async fn launch_and_exit_diagnostics_are_typed_complete_and_tail_bounded() {
    for (mode, expected) in [
        ("exit-no-newline", "unterminated stderr marker"),
        ("exit-many-lines", "line-449"),
    ] {
        let client = HarnessClient::new(options(mode));
        let error = client
            .request("initialize", Map::new(), None)
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<TransportClosedError>().is_some());
        assert!(error.to_string().contains(expected), "{error}");
        if mode == "exit-many-lines" {
            assert!(!error.to_string().contains("line-0\n"));
        }
        client.close().await.unwrap();
    }

    let missing = HarnessClient::new(HarnessClientOptions::new(
        tempfile::tempdir()
            .unwrap()
            .path()
            .join("no-such-sdk-runtime")
            .to_string_lossy()
            .into_owned(),
    ));
    let error = missing
        .request("initialize", Map::new(), Some(1_000.0))
        .await
        .unwrap_err();
    assert!(
        error.downcast_ref::<TransportClosedError>().is_some(),
        "{error:?}"
    );
    assert!(error.to_string().contains("failed to start"), "{error}");
    missing.close().await.unwrap();
}
