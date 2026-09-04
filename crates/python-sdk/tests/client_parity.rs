//! Synchronous lifecycle and routing through real keyless subprocess peers.

use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use seekdeep_identity::{MessageId, SessionId};
use seekdeep_llm::{ModelId, ProviderId};
use seekdeep_python_sdk::{
    Client, Error, ErrorKind, ExceptionId, Harness, HarnessConfig, HarnessOptions, Host,
    NotificationObserver, RequestId, RequestOptions, SeededIds,
};
use serde_json::{Value, json};

const PEER: &str = r#"
import json, os, signal, sys, time
turn = 0
for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    response = None
    if method == "initialize":
        if os.environ.get("REJECT_INIT"):
            print(json.dumps({"id":message["id"],"error":{"code":-32000,"message":"bad initialize"}}), flush=True)
            continue
        response = {"serverInfo":{"name":"keyless-python-peer"}}
        if os.environ.get("CAPTURE"):
            with open(os.environ["CAPTURE"],"w") as output:
                json.dump({"cwd":os.getcwd(),"workspace":os.environ.get("SEEKDEEP_CWD"),"config":os.environ.get("SEEKDEEP_CORDIS_CONFIG"),"params":message.get("params")},output)
        print("ignored non-JSON warning", flush=True)
    elif method == "session/prompt":
        turn += 1
        root = message["params"]["sessionId"]
        mid = "message-" + str(turn)
        frames = [
            {"method":"session.status","params":{"sessionId":root,"status":"idle"}},
            {"method":"session.event","params":{"sessionId":root,"event":{"type":"agent/inbox/spliced","data":{"inserted":[{"id":mid}]}}}},
            {"method":"session.status","params":{"sessionId":root,"status":"running"}},
            {"id":message["id"],"result":{"messageId":mid}},
            {"method":"subagent.started","params":{"parentSessionId":root,"childSessionId":"child"}},
            {"method":"subagent.started","params":{"parentSessionId":"child","childSessionId":"grandchild"}},
            {"method":"session.event","params":{"sessionId":"grandchild","event":{"type":"assistant/message","data":{"content":[{"type":"text","text":"descendant"}]}}}},
            {"method":"subagent.finished","params":{"parentSessionId":"child","childSessionId":"grandchild"}},
            {"method":"session.event","params":{"sessionId":root,"event":{"type":"assistant/message","data":{"content":[{"type":"text","text":"turn " + str(turn)}]}}}},
            {"method":"session.event","params":{"sessionId":root,"event":{"type":"turn/end","data":{"reason":{"kind":"completed"}}}}},
            {"method":"session.status","params":{"sessionId":root,"status":"idle"}},
        ]
        for frame in frames:
            print(json.dumps(frame), flush=True)
        continue
    elif method == "emit":
        print(json.dumps({"method":"tick","params":{"source":"emit"}}), flush=True)
        continue
    elif method == "with-notification":
        print(json.dumps({"method":"tick","params":{"source":"request"}}), flush=True)
        response = {"ok":True}
    elif method == "error":
        print(json.dumps({"id":message["id"],"error":{"code":True,"message":None,"data":{"retained":True}}}), flush=True)
        continue
    elif method == "peer-request":
        print(json.dumps({"id":True,"method":"peer.call","params":["not-an-object"]}), flush=True)
        continue
    elif method == "hang":
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        print("peer is deliberately not responding", file=sys.stderr, flush=True)
        time.sleep(60)
    elif method == "shutdown":
        print(json.dumps({"id":message["id"],"result":{}}), flush=True)
        break
    elif "id" in message and method is None:
        print(json.dumps({"method":"response/seen","params":message}), flush=True)
        continue
    if response is not None:
        print(json.dumps({"id":message["id"],"result":response}), flush=True)
"#;

fn host() -> Host {
    Host::native(
        Arc::new(|| {
            let mut error = Error::new(ErrorKind::Foreign, "bundled module absent");
            error.import_error = true;
            Err(error)
        }),
        Arc::new(|| Err(Error::new(ErrorKind::FileNotFound, "default config absent"))),
    )
}

fn client(config: HarnessConfig) -> Arc<Client> {
    Client::new(config, host(), Arc::new(SeededIds::new([7; 16])))
}

fn peer() -> (tempfile::TempDir, HarnessConfig) {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("peer.py");
    std::fs::write(&script, PEER).unwrap();
    let config = HarnessConfig {
        launch_args_override: Some(vec![
            "python3".to_owned(),
            script.to_string_lossy().into_owned(),
        ]),
        request_timeout_seconds: Some(3.0),
        shutdown_timeout_seconds: Some(0.05),
        ..HarnessConfig::default()
    };
    (root, config)
}

fn initialize(client: &Arc<Client>) {
    let result = client
        .initialize(
            ".",
            &ProviderId::new("deepseek-official"),
            &ModelId::new("model"),
            None,
            Ok,
        )
        .unwrap();
    assert_eq!(result["serverInfo"]["name"], "keyless-python-peer");
}

#[test]
fn request_ids_preserve_arbitrary_precision_python_integers() {
    let wide = "10000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000";
    let value: Value = serde_json::from_str(wide).unwrap();
    let request = RequestId::from_value(&value).expect("wide integer id");
    assert_eq!(request.value().to_string(), wide);
    for value in ["1.0", "1e3", "-2E4"] {
        let value: Value = serde_json::from_str(value).unwrap();
        assert!(RequestId::from_value(&value).is_none());
    }
}

#[test]
fn notification_routing_preserves_global_fallback_and_contains_filter_errors() {
    let client = client(HarnessConfig::default());
    let broken = client.subscribe_notifications(Some(Arc::new(|_| {
        let mut error = Error::new(ErrorKind::Foreign, "bad notification filter");
        error.exception = Some(ExceptionId(42));
        Err(error)
    })));
    let healthy =
        client.subscribe_notifications(Some(Arc::new(|n| Ok(n.read().unwrap().method == "tick"))));
    client
        .handle_message(&json!({"method":"tick","params":{"source":"first"}}))
        .unwrap();
    assert_eq!(broken.next().unwrap_err().exception, Some(ExceptionId(42)));
    assert_eq!(
        healthy.next().unwrap().read().unwrap().payload["source"],
        "first"
    );
    assert_eq!(client.notification_count(), 0);
    client
        .handle_message(&json!({"method":"unmatched","params":false}))
        .unwrap();
    assert_eq!(
        client.next_notification().unwrap().read().unwrap().payload,
        serde_json::Map::new()
    );
    client
        .handle_message(&json!({"method":"tick","params":{"source":"second"}}))
        .unwrap();
    assert_eq!(
        healthy.next().unwrap().read().unwrap().payload["source"],
        "second"
    );
    assert_eq!(broken.queued(), 0);
    broken.close();
    healthy.close();
}

#[test]
fn ancestry_survives_subscriptions_and_late_finishes_do_not_reparent_reused_children() {
    let client = client(HarnessConfig::default());
    let old = client.subscribe_session(SessionId::new("old"));
    let new = client.subscribe_session(SessionId::new("new"));
    client.handle_message(&json!({"method":"subagent.started","params":{"parentSessionId":"old","childSessionId":"child"}})).unwrap();
    assert_eq!(
        old.next().unwrap().read().unwrap().method,
        "subagent.started"
    );
    client.handle_message(&json!({"method":"subagent.started","params":{"parentSessionId":"new","childSessionId":"child"}})).unwrap();
    assert_eq!(
        new.next().unwrap().read().unwrap().method,
        "subagent.started"
    );
    client.handle_message(&json!({"method":"subagent.finished","params":{"parentSessionId":"old","childSessionId":"child"}})).unwrap();
    assert_eq!(
        old.next().unwrap().read().unwrap().method,
        "subagent.finished"
    );
    client
        .handle_message(
            &json!({"method":"session.event","params":{"sessionId":"child","event":{}}}),
        )
        .unwrap();
    assert_eq!(new.next().unwrap().read().unwrap().method, "session.event");
    assert_eq!(old.queued(), 0);
    new.close();
    let next = client.subscribe_session(SessionId::new("new"));
    client.handle_message(&json!({"method":"subagent.started","params":{"parentSessionId":"child","childSessionId":"grandchild"}})).unwrap();
    client
        .handle_message(&json!({"method":"session.event","params":{"sessionId":"grandchild"}}))
        .unwrap();
    assert_eq!(
        next.next().unwrap().read().unwrap().payload["childSessionId"],
        "grandchild"
    );
    assert_eq!(
        next.next().unwrap().read().unwrap().payload["sessionId"],
        "grandchild"
    );
    assert_eq!(client.notification_count(), 0);
    old.close();
    next.close();
}

#[test]
fn closing_a_subscription_retains_its_queued_items_and_error_is_single_consumption() {
    let client = client(HarnessConfig::default());
    let subscription = client.subscribe_notifications(None);
    client.handle_message(&json!({"method":"queued"})).unwrap();
    subscription.close();
    subscription.close();
    assert!(subscription.is_closed());
    assert_eq!(
        subscription.next().unwrap().read().unwrap().method,
        "queued"
    );
    assert_eq!(subscription.try_next().unwrap_err().kind, ErrorKind::Empty);
    let subscription = client.subscribe_notifications(None);
    client.fail_waiters(Error::new(ErrorKind::TransportClosed, "stopped"));
    assert_eq!(subscription.next().unwrap_err().message, "stopped");
    assert_eq!(subscription.try_next().unwrap_err().kind, ErrorKind::Empty);
    assert_eq!(client.next_notification().unwrap_err().message, "stopped");
    assert_eq!(client.next_request().unwrap_err().message, "stopped");
}

#[test]
fn real_process_initialization_prompt_and_restart_preserve_source_lifecycle() {
    let (_root, config) = peer();
    let client = client(config);
    client.close().unwrap();
    client.start().unwrap();
    let process = client.process().unwrap();
    client.start().unwrap();
    assert!(Arc::ptr_eq(&process, &client.process().unwrap()));
    initialize(&client);
    let message = client
        .session_prompt(
            &SessionId::new("main"),
            json!([]),
            RequestOptions::default(),
        )
        .unwrap();
    assert_eq!(message.as_str(), "message-1");
    client.close().unwrap();
    client.close().unwrap();
    assert!(process.poll().unwrap().is_some());
    assert!(client.process().is_none());
    client.start().unwrap();
    initialize(&client);
    client.close().unwrap();
}

#[test]
fn observer_delivery_is_synchronous_and_does_not_dispose_caller_owned_subscriptions() {
    let (_root, config) = peer();
    let client = client(config);
    client.start().unwrap();
    initialize(&client);
    let subscription = client.subscribe_notifications(None);
    let received = Arc::new(Mutex::new(Vec::new()));
    let values = Arc::clone(&received);
    let observer: NotificationObserver = Arc::new(move |notification| {
        values
            .lock()
            .push(notification.read().unwrap().method.clone());
        Ok(())
    });
    let result = client
        .request_object(
            "with-notification",
            None,
            RequestOptions {
                on_notification: Some(observer),
                notification_subscription: Some(Arc::clone(&subscription)),
                ..RequestOptions::default()
            },
        )
        .unwrap();
    assert_eq!(result["ok"], true);
    assert_eq!(*received.lock(), ["tick"]);
    assert!(!subscription.is_closed());
    client.notify("emit", None).unwrap();
    assert_eq!(
        subscription.next().unwrap().read().unwrap().payload["source"],
        "emit"
    );
    subscription.close();
    client.close().unwrap();
}

#[test]
fn rpc_errors_and_incoming_boolean_ids_keep_python_integer_semantics() {
    let (_root, config) = peer();
    let client = client(config);
    client.start().unwrap();
    initialize(&client);
    let error = client
        .request_raw("error", None, RequestOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::JsonRpc);
    assert_eq!(error.message, "None");
    assert_eq!(error.code, Some(Value::Bool(true)));
    assert_eq!(error.data, Some(json!({"retained":true})));
    client.notify("peer-request", None).unwrap();
    let request = client.next_request().unwrap();
    assert_eq!(request.id.value(), &Value::Bool(true));
    assert!(request.payload.is_empty());
    client.respond(&request.id, json!({"answer":1})).unwrap();
    let seen = client.next_notification().unwrap();
    assert_eq!(seen.read().unwrap().payload["id"], true);
    assert_eq!(seen.read().unwrap().payload["result"], json!({"answer":1}));
    client.close().unwrap();
}

#[test]
fn failed_initialization_reaps_and_uncooperative_shutdown_is_bounded() {
    let (_root, mut config) = peer();
    config.env = Some(BTreeMap::from([("REJECT_INIT".to_owned(), "1".to_owned())]));
    let rejected = client(config);
    rejected.start().unwrap();
    let process = rejected.process().unwrap();
    let result = rejected.initialize(
        ".",
        &ProviderId::new("deepseek-official"),
        &ModelId::new("model"),
        None,
        Ok,
    );
    assert_eq!(result.unwrap_err().message, "bad initialize");
    assert!(rejected.process().is_none());
    assert!(process.poll().unwrap().is_some());

    let (_root, config) = peer();
    let client = client(config);
    client.start().unwrap();
    initialize(&client);
    let process = client.process().unwrap();
    let started = Instant::now();
    let failure = client
        .request_raw(
            "hang",
            None,
            RequestOptions {
                timeout_seconds: Some(0.15),
                ..RequestOptions::default()
            },
        )
        .unwrap_err();
    assert_eq!(failure.kind, ErrorKind::Timeout);
    assert!(
        failure
            .message
            .contains("peer is deliberately not responding")
    );
    client.close().unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(process.poll().unwrap().is_some());
    assert!(client.process().is_none());
}

#[test]
fn harness_owns_only_matching_receipt_to_idle_and_keeps_root_events_separate() {
    let (root, config) = peer();
    let root_path = root.path().canonicalize().unwrap();
    let capture = root_path.join("capture.json");
    let harness = Harness::new(
        HarnessOptions {
            cwd: Some(root_path.to_string_lossy().into_owned()),
            runtime_cwd: Some(root_path.to_string_lossy().into_owned()),
            launch_args_override: config.launch_args_override,
            session_root: Some(root_path.join("sessions").to_string_lossy().into_owned()),
            cordis: Some("./explicit.yml".to_owned()),
            env: BTreeMap::from([("CAPTURE".to_owned(), capture.to_string_lossy().into_owned())]),
            max_tokens: Some(json!(4096)),
            shutdown_timeout_seconds: Some(0.05),
            ..HarnessOptions::default()
        },
        host(),
        Arc::new(SeededIds::new([9; 16])),
        Arc::new(|_| Ok(())),
        Arc::new(|response| Ok(MessageId::new(response["messageId"].as_str().unwrap()))),
    )
    .unwrap();
    let received = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&received);
    let observer: NotificationObserver = Arc::new(move |n| {
        seen.lock().push(n.read().unwrap().method.clone());
        Ok(())
    });
    let result = harness
        .run(
            json!("first"),
            Some(SessionId::new("main")),
            Some(&observer),
        )
        .unwrap();
    assert_eq!(result.final_response, "turn 1");
    assert_eq!(result.finish_reason.as_deref(), Some("completed"));
    assert_eq!(
        result
            .notifications
            .iter()
            .map(|notification| notification.read().unwrap().method)
            .collect::<Vec<_>>(),
        [
            "session.event",
            "session.status",
            "subagent.started",
            "subagent.started",
            "session.event",
            "subagent.finished",
            "session.event",
            "session.event",
            "session.status",
        ]
    );
    assert_eq!(result.events.len(), 3);
    assert_eq!(
        result.notifications[0].read().unwrap().payload["event"]["type"],
        "agent/inbox/spliced"
    );
    assert_eq!(
        *received.lock(),
        result
            .notifications
            .iter()
            .map(|n| n.read().unwrap().method.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(harness.client().notification_count(), 0);
    let second = harness
        .run(json!("second"), Some(SessionId::new("main")), None)
        .unwrap();
    assert_eq!(second.final_response, "turn 2");
    let captured: Value = serde_json::from_slice(&std::fs::read(capture).unwrap()).unwrap();
    assert_eq!(captured["cwd"], root_path.to_string_lossy().as_ref());
    assert_eq!(captured["workspace"], root_path.to_string_lossy().as_ref());
    assert_eq!(captured["config"], "./explicit.yml");
    assert_eq!(captured["params"]["maxTokens"], 4096);
    harness.close().unwrap();
}
