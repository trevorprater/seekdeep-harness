//! Standalone argument parser and process wrapper parity.

use std::{process::Stdio, str::FromStr as _};

use seekdeep_llm_mock_server::{
    ConcreteMockLlmBehavior as Concrete, MockLlmBehavior,
    cli::{MOCK_LLM_CLI_USAGE, MockLlmCliParseResult, parse_mock_llm_cli_args},
};
use tokio::io::{AsyncBufReadExt as _, BufReader};

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn help_and_defaults_do_not_require_any_other_option() {
    assert!(matches!(
        parse_mock_llm_cli_args(&args(&["--help", "--unknown"])).unwrap(),
        MockLlmCliParseResult::Help
    ));
    assert!(MOCK_LLM_CLI_USAGE.contains("--sequence"));
    let MockLlmCliParseResult::Run(config) =
        parse_mock_llm_cli_args(&args(&["--sequence", "success"])).unwrap()
    else {
        panic!("expected run config");
    };
    assert!(!config.starts_unavailable);
    assert_eq!(config.listen_delay_ms, 0);
    assert_eq!(config.server.port, Some(8_000.0));
    assert!(!config.server.repeat_last);
    assert_eq!(config.server.sequence, [MockLlmBehavior::Success]);
}

#[test]
fn parses_every_listener_request_response_and_random_option() {
    let MockLlmCliParseResult::Run(config) = parse_mock_llm_cli_args(&args(&[
        "--sequence",
        "connection_refused,partial_disconnect,random",
        "--host",
        "localhost",
        "--port",
        "9010",
        "--api-key",
        "mock-key",
        "--listen-delay-ms",
        "100",
        "--repeat-last",
        "--seed",
        "42",
        "--random-weights",
        "success=8,partial_disconnect=2",
        "--success-text",
        "done",
        "--partial-text",
        "half",
        "--reasoning-text",
        "think",
        "--chunk-size",
        "2",
        "--chunk-delay-ms",
        "3",
        "--disconnect-delay-ms",
        "4",
        "--retry-after-ms",
        "5000",
        "--request-id",
        "request-1",
        "--tool-name",
        "lookup",
        "--tool-arguments",
        r#"{"id":1}"#,
    ]))
    .unwrap() else {
        panic!("expected run config");
    };
    assert!(config.starts_unavailable);
    assert_eq!(config.listen_delay_ms, 100);
    assert_eq!(config.server.host.as_deref(), Some("localhost"));
    assert_eq!(config.server.port, Some(9_010.0));
    assert_eq!(config.server.api_key.as_deref(), Some("mock-key"));
    assert!(config.server.repeat_last);
    assert_eq!(config.server.random_seed, Some(42.0));
    assert_eq!(
        config.server.random_weights.as_ref().unwrap()[&Concrete::Success].to_bits(),
        8.0_f64.to_bits()
    );
    assert_eq!(
        config.server.random_weights.as_ref().unwrap()[&Concrete::PartialDisconnect].to_bits(),
        2.0_f64.to_bits()
    );
    assert_eq!(config.server.success_text.as_deref(), Some("done"));
    assert_eq!(config.server.partial_text.as_deref(), Some("half"));
    assert_eq!(config.server.reasoning_text.as_deref(), Some("think"));
    assert_eq!(config.server.chunk_size, Some(2.0));
    assert_eq!(config.server.chunk_delay_ms, Some(3.0));
    assert_eq!(config.server.disconnect_delay_ms, Some(4.0));
    assert_eq!(config.server.retry_after_ms, Some(5_000.0));
    assert_eq!(config.server.request_id.as_deref(), Some("request-1"));
    assert_eq!(config.server.tool_name.as_deref(), Some("lookup"));
    assert_eq!(config.server.tool_arguments.as_deref(), Some(r#"{"id":1}"#));
}

#[test]
fn unavailable_default_and_invalid_argv_diagnostics_are_stable() {
    let MockLlmCliParseResult::Run(config) = parse_mock_llm_cli_args(&args(&[
        "--sequence",
        "connection_refused,success",
        "--port",
        "8001",
    ]))
    .unwrap() else {
        panic!("expected run config");
    };
    assert_eq!(config.listen_delay_ms, 750);
    for (argv, marker) in [
        (vec![], "--sequence is required"),
        (vec!["--wat"], "Unknown option '--wat'"),
        (vec!["--wat", "x"], "Unknown option '--wat'"),
        (vec!["--port"], "argument missing"),
        (
            vec!["--sequence", "success", "stray"],
            "Unexpected argument",
        ),
        (
            vec!["--port", "NaN", "--sequence", "success"],
            "finite number",
        ),
        (vec!["--sequence", "success,"], "non-empty"),
        (
            vec!["--sequence", "success,connection_refused"],
            "only as the first",
        ),
        (vec!["--sequence", "connection_refused"], "must be followed"),
        (vec!["--sequence", "unknown"], "unknown behavior"),
        (
            vec!["--sequence", "connection_refused,success", "--port", "0"],
            "nonzero",
        ),
        (
            vec!["--sequence", "success", "--listen-delay-ms", "5"],
            "requires connection_refused",
        ),
        (
            vec![
                "--sequence",
                "connection_refused,success",
                "--listen-delay-ms=-1",
            ],
            "integer between 0 and 2147483647",
        ),
        (
            vec![
                "--sequence",
                "connection_refused,success",
                "--listen-delay-ms",
                "1.5",
            ],
            "integer between 0 and 2147483647",
        ),
        (
            vec![
                "--sequence",
                "connection_refused,success",
                "--listen-delay-ms",
                "2147483648",
            ],
            "integer between 0 and 2147483647",
        ),
        (
            vec!["--sequence", "success", "--seed", "1"],
            "require random",
        ),
        (
            vec!["--sequence", "random", "--random-weights", "success"],
            "expects behavior=weight",
        ),
        (
            vec!["--sequence", "random", "--random-weights", "random=1"],
            "concrete behavior",
        ),
        (
            vec![
                "--sequence",
                "random",
                "--random-weights",
                "success=1,success=2",
            ],
            "duplicate",
        ),
        (
            vec!["--sequence", "random", "--random-weights", "success=nope"],
            "finite number",
        ),
    ] {
        let error = parse_mock_llm_cli_args(&args(&argv)).unwrap_err();
        assert!(error.to_string().contains(marker), "{argv:?}: {error:#}");
    }
    assert_eq!(
        MockLlmBehavior::from_str("max_tokens").unwrap(),
        MockLlmBehavior::MaxTokens
    );
}

#[tokio::test]
async fn standalone_binary_reports_help_unavailable_ready_and_jsonl_events() {
    let help = tokio::process::Command::new(env!("CARGO_BIN_EXE_seekdeep-llm-mock-server"))
        .arg("--help")
        .output()
        .await
        .unwrap();
    assert!(help.status.success());
    assert!(
        String::from_utf8(help.stdout)
            .unwrap()
            .contains("--sequence")
    );

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_seekdeep-llm-mock-server"))
        .args([
            "--sequence",
            "connection_refused,success",
            "--port",
            &port.to_string(),
            "--listen-delay-ms",
            "1",
            "--success-text",
            "binary ok",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let unavailable: serde_json::Value = serde_json::from_str(
        &tokio::time::timeout(std::time::Duration::from_secs(5), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(unavailable["type"], "unavailable");
    let ready: serde_json::Value = serde_json::from_str(
        &tokio::time::timeout(std::time::Duration::from_secs(5), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(ready["type"], "ready");
    let response = reqwest::Client::new()
        .post(ready["baseURL"].as_str().unwrap().to_owned() + "/chat/completions")
        .body("{}")
        .send()
        .await
        .unwrap();
    let body = response.text().await.unwrap();
    assert!(
        body.contains(r#""content":"binary o""#) && body.contains(r#""content":"k""#),
        "{body}"
    );
    let request: serde_json::Value = serde_json::from_str(
        &tokio::time::timeout(std::time::Duration::from_secs(5), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    let result: serde_json::Value = serde_json::from_str(
        &tokio::time::timeout(std::time::Duration::from_secs(5), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(request["type"], "request");
    assert_eq!(result["type"], "result");
    #[cfg(unix)]
    {
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(i32::try_from(child.id().unwrap()).unwrap()),
            nix::sys::signal::Signal::SIGTERM,
        )
        .unwrap();
        let status = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status.code(), Some(143));
    }
    #[cfg(not(unix))]
    {
        child.kill().await.unwrap();
        let _ = child.wait().await;
    }
}
