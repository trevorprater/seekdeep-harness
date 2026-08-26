//! Pinned-source in-process fixture Host, carrier, stream, and timing parity.

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures::StreamExt;
use parking_lot::Mutex;
use seekdeep_client_connection::{
    CLIENT_CONNECTION, ClientConnection, FixtureApi, FixtureCreateFrameOrder, FixtureOptions,
    RpcId, RpcResult, StreamApi, fixture_connection, install_fixture_client,
};
use seekdeep_cordis::Context;
use seekdeep_llm::AbortSignal;
use serde_json::{Value, json};

fn fixture() -> Arc<FixtureApi> {
    FixtureApi::new(FixtureOptions::default())
}

async fn call(api: &FixtureApi, method: &str, payload: Value) -> RpcResult<Value> {
    api.unary(
        method,
        RpcId::new(format!("test-{method}")),
        payload,
        AbortSignal::default(),
    )
    .await
    .unwrap()
    .result
}

fn value(result: RpcResult<Value>) -> Value {
    match result {
        RpcResult::Success { value: Some(value) } => value,
        RpcResult::Success { value: None } => Value::Null,
        RpcResult::Failure { error } => {
            panic!("fixture RPC failed: {}: {}", error.code, error.message)
        }
    }
}

fn failure_code(result: RpcResult<Value>) -> String {
    match result {
        RpcResult::Failure { error } => error.code,
        RpcResult::Success { .. } => panic!("expected fixture failure"),
    }
}

#[tokio::test]
async fn serves_seeded_sessions_history_models_settings_and_credentials() {
    let api = fixture();
    let response = api
        .unary(
            "session.list",
            RpcId::new("echo-me"),
            json!({}),
            AbortSignal::default(),
        )
        .await
        .unwrap();
    assert_eq!(response.rpc_id.as_str(), "echo-me");
    let sessions = value(response.result);
    assert_eq!(
        sessions["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|row| row["sessionId"].as_str())
            .collect::<Vec<_>>(),
        ["fx-alpha", "fx-beta", "fx-gamma"]
    );
    assert_eq!(sessions["items"][1]["parentSessionId"], "fx-alpha");

    let models = value(call(&api, "session.models", json!({"sessionId":"fx-alpha"})).await);
    assert_eq!(models["groups"][0]["name"], "DeepSeek");
    assert_eq!(models["groups"][1]["name"], "OpenAI");
    let settings = value(call(&api, "settings.describe", json!({})).await);
    assert_eq!(settings["namespaces"][0]["ns"], "llm-deepseek");
    assert_eq!(
        settings["namespaces"][0]["value"]["apiKeyEnv"],
        "DEEPSEEK_API_KEY"
    );

    let credentials = value(
        call(
            &api,
            "credentials.describe",
            json!({"refs":["DEEPSEEK_API_KEY","TEST_API_KEY"]}),
        )
        .await,
    );
    assert_eq!(
        credentials["credentials"]["DEEPSEEK_API_KEY"]["configured"],
        true
    );
    assert_eq!(
        credentials["credentials"]["TEST_API_KEY"],
        json!({"configured":false,"writable":true})
    );
    value(
        call(
            &api,
            "credentials.set",
            json!({"ref":"TEST_API_KEY","value":"write-only"}),
        )
        .await,
    );
    let configured = value(
        call(
            &api,
            "credentials.describe",
            json!({"refs":["TEST_API_KEY"]}),
        )
        .await,
    );
    assert_eq!(configured["credentials"]["TEST_API_KEY"]["source"], "file");
    value(call(&api, "credentials.unset", json!({"ref":"TEST_API_KEY"})).await);
}

#[tokio::test]
async fn searches_literal_unicode_token_phrases_and_respects_abort_and_current_text() {
    let api = fixture();
    let phrase = value(call(&api, "session.search", json!({"query":"FIXTURE 历史消息"})).await);
    assert_eq!(phrase["items"][0]["sessionId"], "fx-alpha");
    assert!(
        phrase["items"][0]["snippet"]
            .as_str()
            .unwrap()
            .contains("fixture 历史消息")
    );

    api.append_user(
        "fx-alpha",
        &format!(
            "{}late café token{}",
            "leading context ".repeat(20),
            " trailing context".repeat(20)
        ),
    );
    let late = value(call(&api, "session.search", json!({"query":"LATE CAFE TOKEN"})).await);
    let snippet = late["items"][0]["snippet"].as_str().unwrap();
    assert!(snippet.contains("late café token"));
    assert!(snippet.starts_with('…') && snippet.ends_with('…'));
    assert!(snippet.chars().count() <= 122);
    api.append_user("fx-alpha", "Greek final sigma: ος");
    let sigma = value(call(&api, "session.search", json!({"query":"ΟΣ"})).await);
    assert!(
        sigma["items"][0]["snippet"]
            .as_str()
            .unwrap()
            .contains("ος")
    );
    assert_eq!(
        value(call(&api, "session.search", json!({"query":"ixtur"})).await)["items"],
        json!([])
    );
    assert_eq!(
        value(call(&api, "session.search", json!({"query":"*"})).await)["items"],
        json!([])
    );
    assert_eq!(
        value(call(&api, "session.search", json!({"query":"思考过程"})).await)["items"],
        json!([])
    );

    let signal = AbortSignal::default();
    signal.abort();
    assert_eq!(
        failure_code(
            api.call("/api", "session.search", json!({"query":"fixture"}), signal)
                .await
                .unwrap()
        ),
        "cancelled"
    );
}

#[tokio::test]
async fn pages_backwards_on_turn_boundaries_and_serves_empty_tail_projections() {
    let api = fixture();
    let tail = value(
        call(
            &api,
            "session.history",
            json!({"sessionId":"fx-alpha","maxMessages":10}),
        )
        .await,
    );
    assert_eq!(tail["hasMore"], true);
    assert_eq!(tail["events"][0]["event"]["type"], "turn/start");
    let boundary = tail["events"][0]["event"]["seq"].as_i64().unwrap();
    let older = value(
        call(
            &api,
            "session.history",
            json!({"sessionId":"fx-alpha","beforeSeq":boundary,"maxMessages":10}),
        )
        .await,
    );
    assert_eq!(
        older["events"].as_array().unwrap().last().unwrap()["event"]["seq"]
            .as_i64()
            .unwrap()
            + 1,
        boundary
    );
    let clamped = value(
        call(
            &api,
            "session.history",
            json!({"sessionId":"fx-alpha","beforeSeq":-5,"maxMessages":10}),
        )
        .await,
    );
    assert_eq!(clamped["events"], json!([]));
    let empty = value(
        call(
            &api,
            "session.history",
            json!({"sessionId":"missing","maxMessages":10}),
        )
        .await,
    );
    assert_eq!(empty["events"], json!([]));
    assert_eq!(empty["projections"]["asOfSeq"], -1);
    assert_eq!(empty["projections"]["values"]["todos"], Value::Null);
    assert_eq!(
        empty["projections"]["values"]["permissions"]["currentValue"],
        "workspace-write"
    );
    assert_eq!(
        empty["projections"]["values"]["imageLimits"]["maxImagesPerMessage"],
        20
    );
}

#[tokio::test(start_paused = true)]
async fn model_selection_prompt_stream_cancel_and_steering_share_one_log() {
    let api = fixture();
    let selected = value(
        call(
            &api,
            "session.selectModel",
            json!({"sessionId":"fx-alpha","provider":"openai","model":"gpt-5"}),
        )
        .await,
    );
    assert_eq!(
        selected["selected"],
        json!({"provider":"openai","model":"gpt-5"})
    );
    value(call(
        &api,
        "session.prompt",
        json!({"sessionId":"fx-alpha","mode":"queue","content":[{"type":"text","text":"report model"}]}),
    ).await);
    tokio::task::yield_now().await;
    advance_replay(20).await;
    let history = value(call(&api, "session.history", json!({"sessionId":"fx-alpha"})).await);
    assert!(history.to_string().contains("openai/gpt-5"));

    value(call(
        &api,
        "session.prompt",
        json!({"sessionId":"fx-alpha","mode":"queue","content":[{"type":"text","text":"cancel me"}]}),
    ).await);
    tokio::task::yield_now().await;
    advance_replay(1).await;
    value(call(&api, "session.cancel", json!({"sessionId":"fx-alpha"})).await);
    advance_replay(1).await;
    let history = value(call(&api, "session.history", json!({"sessionId":"fx-alpha"})).await);
    assert!(history.to_string().contains("已中断"));

    value(call(
        &api,
        "session.prompt",
        json!({"sessionId":"fx-alpha","mode":"queue","content":[{"type":"text","text":"base"}]}),
    ).await);
    tokio::task::yield_now().await;
    value(call(
        &api,
        "session.prompt",
        json!({"sessionId":"fx-alpha","mode":"steer","content":[{"type":"text","text":"steered"}]}),
    ).await);
    advance_replay(20).await;
    let history = value(call(&api, "session.history", json!({"sessionId":"fx-alpha"})).await);
    assert!(history.to_string().contains("steered"));
}

async fn advance_replay(steps: usize) {
    for _ in 0..steps {
        tokio::time::advance(Duration::from_millis(100)).await;
        tokio::task::yield_now().await;
    }
}

async fn host_frame_after(
    api: Arc<FixtureApi>,
    operation: impl std::future::Future<Output = ()>,
) -> Value {
    let signal = AbortSignal::default();
    let stream = api.host(signal.clone(), Arc::new(|| {}));
    tokio::pin!(stream);
    operation.await;
    let frame = tokio::time::timeout(Duration::from_secs(1), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    signal.abort();
    frame.payload
}

#[tokio::test]
async fn creates_reconciles_and_accounts_sessions_with_failure_and_frame_order_branches() {
    let api = FixtureApi::new(FixtureOptions {
        empty: true,
        ..FixtureOptions::default()
    });
    let frame = host_frame_after(api.clone(), async {
        let created = value(
            call(
                &api,
                "session.create",
                json!({"sessionId":"fx-created","cwd":"/tmp/new"}),
            )
            .await,
        );
        assert_eq!(created["sessionId"], "fx-created");
    })
    .await;
    assert_eq!(frame["type"], "host/session-added");
    assert_eq!(frame["blank"], true);
    let retry = value(
        call(
            &api,
            "session.create",
            json!({"sessionId":"fx-created","cwd":"/tmp/new"}),
        )
        .await,
    );
    assert_eq!(retry["sessionId"], "fx-created");
    let conflict = call(
        &api,
        "session.create",
        json!({"sessionId":"fx-created","cwd":"/tmp/other"}),
    )
    .await;
    assert_eq!(failure_code(conflict), "session-conflict");

    let attach = FixtureApi::new(FixtureOptions {
        fail_workspace_attach: true,
        ..FixtureOptions::default()
    });
    let rejected = call(
        &attach,
        "session.create",
        json!({"workspaceId":"fx-ws-fixture","sessionId":"fx-partial"}),
    )
    .await;
    assert_eq!(failure_code(rejected), "workspace-attach-failed");
    let rows = value(call(&attach, "session.list", json!({})).await);
    assert!(
        rows["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["sessionId"] == "fx-partial")
    );

    let dropped = FixtureApi::new(FixtureOptions {
        drop_session_create_response: true,
        ..FixtureOptions::default()
    });
    assert!(
        dropped
            .unary(
                "session.create",
                RpcId::new("drop"),
                json!({"sessionId":"fx-dropped"}),
                AbortSignal::default()
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("dropped session.create response")
    );
    let rows = value(call(&dropped, "session.list", json!({})).await);
    assert!(rows.to_string().contains("fx-dropped"));
}

#[tokio::test]
async fn directory_and_workspace_mutations_cover_reuse_conflict_order_move_and_delete() {
    let api = fixture();
    let listing = value(
        call(
            &api,
            "host.listDirectory",
            json!({"path":"/home/fixture/Documents"}),
        )
        .await,
    );
    assert_eq!(listing["path"], "/home/fixture/Documents");
    let made = value(
        call(
            &api,
            "host.createDirectory",
            json!({"path":"/home/fixture/Documents","name":"new-folder"}),
        )
        .await,
    );
    assert_eq!(made["path"], "/home/fixture/Documents/new-folder");
    let listing = value(
        call(
            &api,
            "host.listDirectory",
            json!({"path":"/home/fixture/Documents"}),
        )
        .await,
    );
    assert!(listing.to_string().contains("new-folder"));
    assert_eq!(
        failure_code(
            call(
                &api,
                "host.createDirectory",
                json!({"path":"/home/fixture/Documents","name":"new-folder"}),
            )
            .await
        ),
        "directory-exists"
    );

    let reused = value(call(&api, "workspace.create", json!({"path":"/tmp/fixture"})).await);
    assert_eq!(reused["created"], false);
    let fresh = value(call(&api, "workspace.create", json!({"path":"/tmp/new-space"})).await);
    assert_eq!(fresh["created"], true);
    let id = fresh["workspace"]["workspaceId"].as_str().unwrap();
    let renamed = value(
        call(
            &api,
            "workspace.rename",
            json!({"workspaceId":id,"title":"renamed"}),
        )
        .await,
    );
    assert_eq!(renamed["workspace"]["title"], "renamed");
    assert_eq!(
        failure_code(
            call(
                &api,
                "workspace.rename",
                json!({"workspaceId":id,"title":"fixture"})
            )
            .await
        ),
        "workspace-name-conflict"
    );
    let created = value(
        call(
            &api,
            "session.create",
            json!({"workspaceId":id,"sessionId":"fx-move"}),
        )
        .await,
    );
    let sid = created["sessionId"].as_str().unwrap();
    let moved = value(
        call(
            &api,
            "workspace.insertSessionBefore",
            json!({"workspaceId":id,"sessionId":sid}),
        )
        .await,
    );
    assert_eq!(moved["workspace"]["sessionIds"], json!(["fx-move"]));
    assert_eq!(
        value(call(&api, "workspace.delete", json!({"workspaceId":id})).await)["deleted"],
        true
    );
    assert_eq!(
        failure_code(call(&api, "workspace.delete", json!({"workspaceId":id})).await),
        "workspace-not-found"
    );
}

#[tokio::test]
async fn session_rename_fork_pending_responses_and_stream_replay_match_host_rules() {
    let api = fixture();
    assert_eq!(
        failure_code(
            call(
                &api,
                "session.rename",
                json!({"sessionId":"missing","title":"x"})
            )
            .await
        ),
        "session-not-found"
    );
    assert_eq!(
        failure_code(
            call(
                &api,
                "session.rename",
                json!({"sessionId":"fx-alpha","title":"   "})
            )
            .await
        ),
        "title-invalid"
    );
    let renamed = value(
        call(
            &api,
            "session.rename",
            json!({"sessionId":"fx-alpha","title":"  New   title "}),
        )
        .await,
    );
    assert_eq!(renamed["title"], "New title");
    let forked = value(call(&api, "session.fork", json!({"sessionId":"fx-alpha"})).await);
    assert!(forked["sessionId"].as_str().unwrap().starts_with("fx-"));

    let first = collect_initial_mux(api.clone()).await;
    let approval = first
        .iter()
        .find(|frame| frame.payload["type"] == "approval/requested")
        .unwrap();
    let question = first
        .iter()
        .find(|frame| frame.payload["type"] == "question/requested")
        .unwrap();
    let second = collect_initial_mux(api.clone()).await;
    assert_eq!(
        second
            .iter()
            .find(|frame| frame.payload["type"] == "approval/requested")
            .unwrap()
            .rpc_id,
        approval.rpc_id
    );
    assert_eq!(
        second
            .iter()
            .find(|frame| frame.payload["type"] == "question/requested")
            .unwrap()
            .rpc_id,
        question.rpc_id
    );
    let bad = api.respond(&json!({"type":"client-response","rpcId":approval.rpc_id,"result":{"ok":true,"value":{"approvalId":"wrong","outcome":"allowed-once"}}}));
    assert_eq!(bad, json!({"accepted":false,"reason":"bad-response"}));
    let approval_id = approval.payload["approvalId"].clone();
    assert_eq!(
        api.respond(&json!({"type":"client-response","rpcId":approval.rpc_id,"result":{"ok":true,"value":{"approvalId":approval_id,"outcome":"allowed-once"}}})),
        json!({"accepted":true})
    );
    assert_eq!(
        api.respond(&json!({"type":"client-response","rpcId":approval.rpc_id,"result":{"ok":true,"value":{"approvalId":approval_id,"outcome":"allowed-once"}}})),
        json!({"accepted":false,"reason":"not-pending"})
    );
    assert_eq!(
        api.respond(&json!({"type":"client-response","rpcId":question.rpc_id,"result":{"ok":false,"error":{}}})),
        json!({"accepted":true})
    );
}

async fn collect_initial_mux(api: Arc<FixtureApi>) -> Vec<seekdeep_client_connection::EventFrame> {
    let signal = AbortSignal::default();
    let stream = api.mux(signal.clone(), Arc::new(|| {}));
    tokio::pin!(stream);
    let mut frames = Vec::new();
    while let Some(frame) = stream.next().await {
        let frame = frame.unwrap();
        let done = frame.payload["type"] == "question/requested";
        frames.push(frame);
        if done {
            signal.abort();
            break;
        }
    }
    frames
}

#[tokio::test]
async fn commands_skills_presets_llm_and_generic_remote_share_state() {
    let api = fixture();
    let commands = value(
        api.remote_call(
            "/api",
            "commands/list",
            json!({"args":{"agentId":"fx-alpha"}}),
        )
        .await
        .unwrap(),
    );
    assert_eq!(
        commands
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|row| row["name"].as_str())
            .collect::<Vec<_>>(),
        ["compact", "echo", "goal", "permission", "plan"]
    );
    let execution = value(
        api.remote_call(
            "/api",
            "commands/execute",
            json!({"args":{"agentId":"fx-alpha","line":"/echo hello world"}}),
        )
        .await
        .unwrap(),
    );
    assert!(execution["commandId"].as_str().is_some());
    assert_eq!(execution["result"]["text"], "hello world");
    assert_eq!(
        failure_code(
            api.remote_call(
                "/api",
                "commands/list",
                json!({"args":{"agentId":"missing"}})
            )
            .await
            .unwrap()
        ),
        "session-not-found"
    );

    let skills = value(call(&api, "skill.list", json!({"sessionId":"fx-alpha"})).await);
    assert_eq!(skills["skills"][0]["name"], "fixture-demo");
    let presets = value(call(&api, "agentPreset.list", json!({})).await);
    assert!(presets["presets"].as_array().unwrap().len() >= 3);
    let copied = value(
        call(
            &api,
            "agentPreset.copy",
            json!({"from":"standard","agentPreset":"copy"}),
        )
        .await,
    );
    assert_eq!(copied["agentPreset"], "copy");
    assert_eq!(
        value(
            call(
                &api,
                "agentPreset.openDocument",
                json!({"agentPreset":"copy"})
            )
            .await
        )["opened"],
        true
    );
    assert_eq!(
        failure_code(
            call(
                &api,
                "agentPreset.remove",
                json!({"agentPreset":"standard"})
            )
            .await
        ),
        "agent-preset-read-only"
    );
    let providers = value(call(&api, "llm.providers", json!({})).await);
    assert!(providers.to_string().contains("deepseek-official"));
    let discovered = value(call(&api, "llm.discoverModels", json!({})).await);
    assert!(discovered["models"].as_array().unwrap().len() >= 3);
}

#[tokio::test]
async fn goal_lifecycle_uses_one_cas_state_graph_and_durable_event_sequence() {
    let api = fixture();
    let created = value(
        call(
            &api,
            "goal.create",
            json!({"sessionId":"fx-alpha","objective":"ship it"}),
        )
        .await,
    );
    let mut reference = created["ref"].clone();
    for method in ["goal.edit", "goal.pause", "goal.resume", "goal.complete"] {
        let mut payload = json!({"sessionId":"fx-alpha","ref":reference});
        if method == "goal.edit" {
            payload["objective"] = json!("ship it v2");
        }
        reference = value(call(&api, method, payload).await)["ref"].clone();
    }
    assert_eq!(
        failure_code(
            call(
                &api,
                "goal.complete",
                json!({"sessionId":"fx-alpha","ref":reference})
            )
            .await
        ),
        "internal"
    );
    assert_eq!(
        value(
            call(
                &api,
                "goal.clear",
                json!({"sessionId":"fx-alpha","ref":reference})
            )
            .await
        )["cleared"],
        true
    );
    let history = value(call(&api, "session.history", json!({"sessionId":"fx-alpha"})).await);
    for operation in ["create", "edit", "pause", "resume", "complete", "clear"] {
        assert!(history.to_string().contains(operation));
    }
}

#[tokio::test(start_paused = true)]
async fn timing_hooks_delay_fail_silent_append_break_streams_and_reasoning_storm() {
    let api = fixture();
    api.set_history_delay(5);
    api.fail_next_history();
    let history = api.unary(
        "session.history",
        RpcId::new("doomed"),
        json!({"sessionId":"fx-alpha","maxMessages":5}),
        AbortSignal::default(),
    );
    tokio::pin!(history);
    tokio::time::advance(Duration::from_millis(5)).await;
    assert!(
        history
            .await
            .unwrap_err()
            .to_string()
            .contains("simulated history transport failure")
    );
    api.set_history_delay(0);
    value(
        call(
            &api,
            "session.history",
            json!({"sessionId":"fx-alpha","maxMessages":5}),
        )
        .await,
    );

    let signal = AbortSignal::default();
    let stream = api.mux(signal.clone(), Arc::new(|| {}));
    tokio::pin!(stream);
    while let Some(frame) = stream.next().await {
        if frame.unwrap().payload["type"] == "question/requested" {
            break;
        }
    }
    api.append_silent("fx-alpha", "静默丢帧");
    api.append_user("fx-alpha", "正常直播");
    let live = stream.next().await.unwrap().unwrap();
    assert!(live.payload.to_string().contains("正常直播"));
    assert!(!live.payload.to_string().contains("静默丢帧"));
    let history = value(
        call(
            &api,
            "session.history",
            json!({"sessionId":"fx-alpha","maxMessages":5}),
        )
        .await,
    );
    assert!(history.to_string().contains("静默丢帧"));
    api.break_streams();
    assert!(stream.next().await.is_none());
    assert!(!signal.is_aborted());

    assert!(
        api.start_reasoning_chunk_storm("fx-alpha", 0, 1, 16)
            .unwrap_err()
            .to_string()
            .contains("chunk count")
    );
    let marker = api
        .start_reasoning_chunk_storm("fx-alpha", 3, 2, 16)
        .unwrap();
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(16)).await;
    tokio::task::yield_now().await;
    let state = api.reasoning_chunk_storm_state().unwrap();
    assert_eq!(state.emitted, 3);
    assert!(!state.emitting);
    assert_eq!(state.marker, marker);
}

#[tokio::test]
async fn full_envelope_tap_and_stream_open_cover_all_carrier_forms() {
    let api = fixture();
    let envelopes = Arc::new(Mutex::new(Vec::new()));
    let _subscription = api.subscribe_envelopes({
        let envelopes = envelopes.clone();
        Arc::new(move |batch| envelopes.lock().extend(batch))
    });
    value(call(&api, "session.list", json!({})).await);
    let opened = Arc::new(AtomicUsize::new(0));
    let signal = AbortSignal::default();
    let stream = api.mux(signal.clone(), {
        let opened = opened.clone();
        Arc::new(move || {
            opened.fetch_add(1, Ordering::Relaxed);
        })
    });
    tokio::pin!(stream);
    let _ = stream.next().await.unwrap().unwrap();
    assert_eq!(opened.load(Ordering::Relaxed), 1);
    signal.abort();
    let kinds = envelopes
        .lock()
        .iter()
        .filter_map(|row| row["type"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    assert!(kinds.iter().any(|kind| kind == "client-request"));
    assert!(kinds.iter().any(|kind| kind == "server-response"));
    assert!(kinds.iter().any(|kind| kind == "server-request"));
    let question = collect_initial_mux(api.clone())
        .await
        .into_iter()
        .find(|frame| frame.payload["type"] == "question/requested")
        .unwrap();
    api.respond(
        &json!({"type":"client-response","rpcId":question.rpc_id,"result":{"ok":false,"error":{}}}),
    );
    assert!(
        envelopes
            .lock()
            .iter()
            .any(|row| row["type"] == "client-response")
    );
}

#[test]
fn query_options_and_connection_handle_preserve_fixture_selection_and_authority() {
    assert_eq!(
        FixtureOptions::from_query(
            "?fixture=empty&fixturePrompt=reject&fixtureAttach=fail&fixtureSessionCreate=drop-response&fixtureFrames=workspace-first"
        ),
        FixtureOptions {
            empty: true,
            reject_prompt: true,
            fail_workspace_attach: true,
            drop_session_create_response: true,
            create_frame_order: FixtureCreateFrameOrder::WorkspaceFirst,
        }
    );
    assert!(fixture_connection(FixtureOptions::default(), true).is_loopback());
    assert!(!fixture_connection(FixtureOptions::default(), false).is_loopback());
    let context = Context::new();
    install_fixture_client(&context, "?fixture=empty", Some("localhost")).unwrap();
    let handle = context.get(CLIENT_CONNECTION).unwrap();
    assert!(handle.is_loopback());
    let listed = futures::executor::block_on(handle.call(
        "/api",
        "session.list",
        json!({}),
        AbortSignal::default(),
    ))
    .unwrap();
    assert_eq!(value(listed)["items"], json!([]));
}
