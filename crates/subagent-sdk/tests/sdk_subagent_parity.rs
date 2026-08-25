//! Real-process SDK subagent lifecycle, configuration, and Loader parity.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use seekdeep_agent::{Agent, AgentOptions, Inbox, NoopInboxNotifications};
use seekdeep_cordis::Context;
use seekdeep_core::session::{Session, SessionHeader, SessionId};
use seekdeep_llm::{AbortSignal, ContentBlock};
use seekdeep_loader::PluginCatalog;
use seekdeep_scope::ScopeKey;
use seekdeep_subagent::{SubagentRuntime, SubagentStartRequest, SubagentStopReason};
use seekdeep_subagent_sdk::{
    Config, DEFAULT_DISPOSE_EOF_GRACE_MS, DEFAULT_DISPOSE_GRACE_MS, DEFAULT_SHUTDOWN_TIMEOUT_MS,
    INJECT, NAME, SdkRunSpec, apply, plugin, sdk_stop_reason, start_sdk_run,
};
use serde_json::{Value, json};

const FIXTURE: &str = env!("CARGO_BIN_EXE_seekdeep-subagent-sdk-fixture");

fn agent(context: &Context, cwd: Option<&str>, id: &str) -> Arc<Agent> {
    let id = SessionId::new(id);
    let mut header = SessionHeader::new(id.clone());
    header.cwd = cwd.map(str::to_owned);
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

fn request(parent: Arc<Agent>, signal: AbortSignal) -> SubagentStartRequest {
    SubagentStartRequest {
        label: Some("fixture".to_owned()),
        prompt: vec![ContentBlock::Text {
            text: "do the task".to_owned(),
        }],
        parent,
        signal,
        agent_options: None,
        output_schema: None,
        max_depth: None,
        tool_filter: None,
        persona: None,
    }
}

fn spec(cwd: &str, env: impl IntoIterator<Item = (&'static str, String)>) -> SdkRunSpec {
    SdkRunSpec {
        command: FIXTURE.to_owned(),
        args: Vec::new(),
        cwd: cwd.to_owned(),
        provider: "fixture-provider".to_owned(),
        model: "fixture-model".to_owned(),
        max_tokens: Some(4_096),
        env: env
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
        shutdown_timeout_ms: 100.0,
        dispose_eof_grace_ms: 200.0,
        dispose_grace_ms: 500.0,
        on_error: None,
    }
}

fn text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

async fn result(
    run: &Arc<dyn seekdeep_subagent::SubagentRun>,
) -> seekdeep_subagent::SubagentResult {
    tokio::time::timeout(Duration::from_secs(3), run.result())
        .await
        .expect("run result timeout")
        .unwrap()
}

#[test]
fn names_defaults_and_stop_reason_vocabulary_are_exact() {
    assert_eq!(NAME, "subagent-seekdeep-sdk");
    assert_eq!(INJECT, ["subagents"]);
    let config = Config::default();
    assert_eq!(config.provider_name, "seekdeep-sdk");
    assert_eq!(config.provider, "deepseek-official");
    assert_eq!(config.model, "deepseek-v4-flash");
    assert_eq!(config.shutdown_timeout_ms, DEFAULT_SHUTDOWN_TIMEOUT_MS);
    assert_eq!(config.dispose_eof_grace_ms, DEFAULT_DISPOSE_EOF_GRACE_MS);
    assert_eq!(config.dispose_grace_ms, DEFAULT_DISPOSE_GRACE_MS);
    for (reason, expected) in [
        (
            Some(json!({"kind":"completed"})),
            SubagentStopReason::Completed,
        ),
        (
            Some(json!({"kind":"max-tokens"})),
            SubagentStopReason::MaxTokens,
        ),
        (
            Some(json!({"kind":"aborted","reason":{"kind":"user"}})),
            SubagentStopReason::Aborted,
        ),
        (Some(json!({"kind":"error"})), SubagentStopReason::Error),
        (
            Some(json!({"kind":"interrupted"})),
            SubagentStopReason::Error,
        ),
        (Some(json!({"kind":"disposed"})), SubagentStopReason::Error),
        (
            Some(json!({"kind":"something-new"})),
            SubagentStopReason::Error,
        ),
        (None, SubagentStopReason::Error),
    ] {
        assert_eq!(sdk_stop_reason(reason.as_ref()), expected);
    }
}

#[tokio::test]
async fn real_child_initializes_runs_preserves_environment_and_disposes_idempotently() {
    let workspace = tempfile::tempdir().unwrap();
    let record = workspace.path().join("initialize.jsonl");
    let context = Context::new();
    let parent = agent(
        &context,
        Some(&workspace.path().to_string_lossy()),
        "parent",
    );
    let environment = [
        (
            "SEEKDEEP_SDK_FIXTURE_TEXT",
            "hello from sdk child".to_owned(),
        ),
        (
            "SEEKDEEP_SDK_FIXTURE_RECORD_INIT",
            record.to_string_lossy().into_owned(),
        ),
        (
            "SEEKDEEP_SDK_FIXTURE_ECHO_ENV",
            "DEEPSEEK_API_KEY".to_owned(),
        ),
        ("DEEPSEEK_API_KEY", "explicit-child-key".to_owned()),
    ];
    let first = start_sdk_run(
        request(Arc::clone(&parent), AbortSignal::default()),
        spec(&workspace.path().to_string_lossy(), environment.clone()),
    )
    .await
    .unwrap();
    assert!(first.local_agent().is_none());
    let first_result = result(&first).await;
    assert_eq!(first_result.stop_reason, SubagentStopReason::Completed);
    assert_eq!(
        text(&first_result.output),
        "DEEPSEEK_API_KEY=explicit-child-key\nhello from sdk child"
    );
    let (left, right) = tokio::join!(first.dispose(), first.dispose());
    left.unwrap();
    right.unwrap();

    let second = start_sdk_run(
        request(parent, AbortSignal::default()),
        spec(&workspace.path().to_string_lossy(), environment),
    )
    .await
    .unwrap();
    assert_ne!(first.id(), second.id());
    let _ = result(&second).await;
    second.dispose().await.unwrap();

    let records = std::fs::read_to_string(record)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 2);
    assert!(records.iter().all(|record| {
        record["cwd"] == json!(workspace.path().to_string_lossy())
            && record["provider"] == json!("fixture-provider")
            && record["model"] == json!("fixture-model")
            && record["maxTokens"] == json!(4_096)
    }));
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn maps_terminal_reasons_and_retains_the_canonical_partial_output() {
    for (variables, expected_reason, expected_text) in [
        (
            vec![("SEEKDEEP_SDK_FIXTURE_REASON", "max-tokens".to_owned())],
            SubagentStopReason::MaxTokens,
            "hello from fake runtime",
        ),
        (
            vec![("SEEKDEEP_SDK_FIXTURE_REASON", "error".to_owned())],
            SubagentStopReason::Error,
            "hello from fake runtime",
        ),
        (
            vec![("SEEKDEEP_SDK_FIXTURE_REASON", "none".to_owned())],
            SubagentStopReason::Error,
            "hello from fake runtime",
        ),
        (
            vec![
                ("SEEKDEEP_SDK_FIXTURE_REASON", "max-tokens".to_owned()),
                ("SEEKDEEP_SDK_FIXTURE_EMPTY_MESSAGE", "1".to_owned()),
            ],
            SubagentStopReason::MaxTokens,
            "hello from fake runtime",
        ),
        (
            vec![
                ("SEEKDEEP_SDK_FIXTURE_MALFORMED_MESSAGE", "1".to_owned()),
                ("SEEKDEEP_SDK_FIXTURE_TEXT", "stream-only answer".to_owned()),
            ],
            SubagentStopReason::Error,
            "stream-only answer",
        ),
    ] {
        let workspace = tempfile::tempdir().unwrap();
        let context = Context::new();
        let run = start_sdk_run(
            request(
                agent(
                    &context,
                    Some(&workspace.path().to_string_lossy()),
                    "parent",
                ),
                AbortSignal::default(),
            ),
            spec(&workspace.path().to_string_lossy(), variables),
        )
        .await
        .unwrap();
        let outcome = result(&run).await;
        assert_eq!(outcome.stop_reason, expected_reason);
        assert_eq!(text(&outcome.output), expected_text);
        run.dispose().await.unwrap();
        context.fiber().dispose().await.unwrap();
    }
}

#[tokio::test]
async fn cancellation_and_protocol_failures_obey_publication_and_output_ownership() {
    let workspace = tempfile::tempdir().unwrap();
    let context = Context::new();
    let parent = agent(
        &context,
        Some(&workspace.path().to_string_lossy()),
        "parent",
    );
    let signal = AbortSignal::default();
    let run = start_sdk_run(
        request(Arc::clone(&parent), signal.clone()),
        spec(
            &workspace.path().to_string_lossy(),
            [("SEEKDEEP_SDK_FIXTURE_HANG_PROMPT", "1".to_owned())],
        ),
    )
    .await
    .unwrap();
    signal.abort_with_reason(json!("cancel"));
    let outcome = result(&run).await;
    assert_eq!(outcome.stop_reason, SubagentStopReason::Aborted);
    assert!(outcome.output.is_empty());
    run.dispose().await.unwrap();

    let run = start_sdk_run(
        request(Arc::clone(&parent), AbortSignal::default()),
        spec(
            &workspace.path().to_string_lossy(),
            [("SEEKDEEP_SDK_FIXTURE_STREAM_THEN_MALFORMED", "1".to_owned())],
        ),
    )
    .await
    .unwrap();
    let outcome = result(&run).await;
    assert_eq!(outcome.stop_reason, SubagentStopReason::Error);
    assert!(outcome.output.is_empty());
    run.dispose().await.unwrap();

    let seen = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&seen);
    let mut failing = spec(
        &workspace.path().to_string_lossy(),
        [("SEEKDEEP_SDK_FIXTURE_BAD_PROMPT", "1".to_owned())],
    );
    failing.on_error = Some(Arc::new(move |error, reason| {
        observed.lock().unwrap().push((error.to_string(), reason));
        panic!("observer failure is contained");
    }));
    let run = start_sdk_run(request(parent, AbortSignal::default()), failing)
        .await
        .unwrap();
    assert_eq!(result(&run).await.stop_reason, SubagentStopReason::Error);
    assert_eq!(seen.lock().unwrap().len(), 1);
    run.dispose().await.unwrap();
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn prepublication_abort_and_child_death_reject_after_rollback() {
    let workspace = tempfile::tempdir().unwrap();
    let context = Context::new();
    let parent = agent(
        &context,
        Some(&workspace.path().to_string_lossy()),
        "parent",
    );
    let sentinel = workspace.path().join("spawned");
    let signal = AbortSignal::default();
    signal.abort_with_reason(json!("already cancelled"));
    let error = start_sdk_run(
        request(Arc::clone(&parent), signal),
        spec(
            &workspace.path().to_string_lossy(),
            [(
                "SEEKDEEP_SDK_FIXTURE_SPAWNED",
                sentinel.to_string_lossy().into_owned(),
            )],
        ),
    )
    .await
    .err()
    .expect("pre-abort must reject");
    assert!(error.to_string().contains("aborted before"));
    assert!(!sentinel.exists());

    let error = start_sdk_run(
        request(Arc::clone(&parent), AbortSignal::default()),
        spec(
            &workspace.path().to_string_lossy(),
            [("SEEKDEEP_SDK_FIXTURE_EXIT_BEFORE_INIT", "1".to_owned())],
        ),
    )
    .await
    .err()
    .expect("dead child must reject");
    let message = error.to_string();
    assert!(message.contains("exit code: 3"), "{message}");
    assert!(message.contains("scripted boot failure"), "{message}");

    let ready = workspace.path().join("ready");
    let go = workspace.path().join("go");
    let signal = AbortSignal::default();
    let pending_signal = signal.clone();
    let pending = tokio::spawn(start_sdk_run(
        request(parent, pending_signal),
        spec(
            &workspace.path().to_string_lossy(),
            [
                (
                    "SEEKDEEP_SDK_FIXTURE_INIT_READY",
                    ready.to_string_lossy().into_owned(),
                ),
                (
                    "SEEKDEEP_SDK_FIXTURE_INIT_GO",
                    go.to_string_lossy().into_owned(),
                ),
            ],
        ),
    ));
    for _ in 0..500 {
        if ready.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(ready.exists(), "fixture never entered initialize");
    signal.abort_with_reason(json!("mid-handshake"));
    std::fs::write(go, b"go\n").unwrap();
    let error = pending
        .await
        .unwrap()
        .err()
        .expect("mid-handshake cancel must reject");
    assert!(error.to_string().contains("aborted before"));
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn plugin_validates_registers_unwinds_and_honors_cwd_override() {
    assert_eq!(plugin().name(), NAME);
    assert_eq!(plugin().inject(), INJECT);
    for (field, value) in [
        ("shutdownTimeoutMs", json!(0)),
        ("disposeEofGraceMs", json!(-1)),
        ("disposeGraceMs", json!(f64::INFINITY)),
    ] {
        let context = Context::new();
        SubagentRuntime::install(&context).unwrap();
        let mut config = serde_json::to_value(Config {
            command: FIXTURE.to_owned(),
            ..Config::default()
        })
        .unwrap();
        config[field] = value;
        assert!(
            context
                .plugin(plugin(), config)
                .unwrap()
                .await_settled()
                .await
                .is_err()
        );
        context.fiber().dispose().await.unwrap();
    }
    for invalid in [0, 9_007_199_254_740_992_u64] {
        let context = Context::new();
        SubagentRuntime::install(&context).unwrap();
        let error = apply(
            &context,
            Config {
                command: FIXTURE.to_owned(),
                max_tokens: Some(invalid),
                ..Config::default()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("maxTokens"));
    }

    let context = Context::new();
    let subagents = SubagentRuntime::install(&context).unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let parent_workspace = tempfile::tempdir().unwrap();
    let fiber = context
        .plugin(
            plugin(),
            serde_json::to_value(Config {
                provider_name: "sdk-hmr".to_owned(),
                command: FIXTURE.to_owned(),
                cwd: Some(workspace.path().to_string_lossy().into_owned()),
                provider: "p".to_owned(),
                model: "m".to_owned(),
                env: BTreeMap::from([("SEEKDEEP_SDK_FIXTURE_ECHO_CWD".to_owned(), "1".to_owned())]),
                ..Config::default()
            })
            .unwrap(),
        )
        .unwrap();
    fiber.await_settled().await.unwrap();
    let provider = subagents.get_provider("sdk-hmr").unwrap();
    assert!(!provider.inherits_parent_context());
    assert_eq!(
        provider.capabilities(),
        &seekdeep_subagent::no_start_capabilities()
    );
    let run = subagents
        .start(
            "sdk-hmr",
            request(
                agent(
                    &context,
                    Some(&parent_workspace.path().to_string_lossy()),
                    "parent",
                ),
                AbortSignal::default(),
            ),
        )
        .await
        .unwrap();
    let expected = std::fs::canonicalize(workspace.path()).unwrap();
    assert!(text(&result(&run).await.output).contains(&format!("cwd={}", expected.display())));
    run.dispose().await.unwrap();
    fiber.dispose().await.unwrap();
    assert!(subagents.get_provider("sdk-hmr").is_none());
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn missing_cwd_fails_before_spawn_and_loader_composition_inherits_parent_workspace() {
    let context = Context::new();
    let subagents = SubagentRuntime::install(&context).unwrap();
    apply(
        &context,
        Config {
            command: FIXTURE.to_owned(),
            ..Config::default()
        },
    )
    .unwrap();
    let error = subagents
        .start(
            "seekdeep-sdk",
            request(agent(&context, None, "no-cwd"), AbortSignal::default()),
        )
        .await
        .err()
        .expect("missing cwd must reject");
    assert!(error.to_string().contains("no working directory"));
    context.fiber().dispose().await.unwrap();

    let catalog = PluginCatalog::new();
    catalog
        .register_named("subagents", seekdeep_subagent::plugin())
        .unwrap();
    catalog.register_named("sdk", plugin()).unwrap();
    let context = Context::new();
    let fixture = serde_json::to_string(FIXTURE).unwrap();
    let composition = catalog
        .load_yaml(
            &context,
            &format!(
                concat!(
                    "- id: subagents\n",
                    "  name: subagents\n",
                    "- id: sdk\n",
                    "  name: sdk\n",
                    "  config:\n",
                    "    command: {}\n",
                    "    env:\n",
                    "      SEEKDEEP_SDK_FIXTURE_ECHO_CWD: '1'\n",
                ),
                fixture
            ),
        )
        .await
        .unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let subagents = context.get(seekdeep_subagent::SUBAGENTS).unwrap();
    let run = subagents
        .start(
            "seekdeep-sdk",
            request(
                agent(
                    &context,
                    Some(&workspace.path().to_string_lossy()),
                    "loader-parent",
                ),
                AbortSignal::default(),
            ),
        )
        .await
        .unwrap();
    let expected = std::fs::canonicalize(workspace.path()).unwrap();
    assert!(text(&result(&run).await.output).contains(&format!("cwd={}", expected.display())));
    run.dispose().await.unwrap();
    composition.dispose().await.unwrap();
    context.fiber().dispose().await.unwrap();
}
