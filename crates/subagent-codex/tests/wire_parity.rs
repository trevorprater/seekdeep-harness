//! Codex app-server request, notification, association, and output-selection parity.

use std::sync::Arc;

use seekdeep_llm::AbortSignal;
use seekdeep_sdk_protocol::JsonRpcResponseError;
use seekdeep_subagent::SubagentStopReason;
use seekdeep_subagent_codex::CodexAppServerWire;
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader, ReadHalf, WriteHalf};

struct Peer {
    reader: BufReader<ReadHalf<tokio::io::DuplexStream>>,
    writer: WriteHalf<tokio::io::DuplexStream>,
}

impl Peer {
    async fn next(&mut self) -> Value {
        let mut line = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            self.reader.read_line(&mut line),
        )
        .await
        .expect("frame timeout")
        .expect("frame read");
        serde_json::from_str(line.trim()).expect("frame JSON")
    }

    async fn next_method(&mut self, method: &str) -> Value {
        loop {
            let frame = self.next().await;
            if frame["method"] == method {
                return frame;
            }
        }
    }

    async fn send(&mut self, frames: &[Value]) {
        let body = frames
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        self.writer
            .write_all(format!("{body}\n").as_bytes())
            .await
            .unwrap();
    }

    async fn respond(&mut self, request: &Value, result: Value) {
        self.send(&[json!({"id":request["id"], "result":result})])
            .await;
    }
}

fn wire_pair() -> (Arc<CodexAppServerWire>, Peer) {
    let (host, peer) = tokio::io::duplex(128 * 1024);
    let (host_read, host_write) = tokio::io::split(host);
    let (peer_read, peer_write) = tokio::io::split(peer);
    (
        CodexAppServerWire::new(Box::pin(host_read), Box::pin(host_write)),
        Peer {
            reader: BufReader::new(peer_read),
            writer: peer_write,
        },
    )
}

async fn initialized_wire() -> (Arc<CodexAppServerWire>, Peer) {
    let (wire, mut peer) = wire_pair();
    wire.start();
    let initializing = {
        let wire = Arc::clone(&wire);
        tokio::spawn(async move { wire.initialize(AbortSignal::default()).await })
    };
    let initialize = peer.next_method("initialize").await;
    peer.respond(&initialize, json!({"userAgent":"codex-cli 0.147.0"}))
        .await;
    initializing.await.unwrap().unwrap();
    assert_eq!(
        peer.next_method("initialized").await,
        json!({"jsonrpc":"2.0", "method":"initialized"})
    );
    let starting = {
        let wire = Arc::clone(&wire);
        tokio::spawn(async move {
            wire.start_thread("/workspace", AbortSignal::default())
                .await
        })
    };
    let thread = peer.next_method("thread/start").await;
    assert_eq!(
        thread["params"],
        json!({"cwd":"/workspace", "ephemeral":true})
    );
    peer.respond(
        &thread,
        json!({"thread":{"id":"thread-1", "ephemeral":true}}),
    )
    .await;
    starting.await.unwrap().unwrap();
    (wire, peer)
}

fn agent_message(text: Value, phase: Value, turn: &str, thread: &str) -> Value {
    let frame = json!({
        "method":"item/completed",
        "params":{
            "threadId":thread,
            "turnId":turn,
            "item":{"type":"agentMessage", "text":text, "phase":phase}
        }
    });
    drop(text);
    drop(phase);
    frame
}

fn turn_completed(status: &str, error: Value) -> Value {
    let frame = json!({
        "method":"turn/completed",
        "params":{
            "threadId":"thread-1",
            "turn":{"id":"turn-1", "status":status, "error":error}
        }
    });
    drop(error);
    frame
}

#[tokio::test]
async fn sends_fixed_product_payloads_and_selects_the_last_final_answer() {
    let (wire, mut peer) = wire_pair();
    assert!(wire.collect_output().is_empty());
    wire.start();
    let initializing = {
        let wire = Arc::clone(&wire);
        tokio::spawn(async move { wire.initialize(AbortSignal::default()).await })
    };
    let initialize = peer.next_method("initialize").await;
    assert_eq!(
        initialize["params"],
        json!({
            "clientInfo":{
                "name":"seekdeep-harness",
                "title":"SeekDeep Harness",
                "version":"0.0.1"
            },
            "capabilities":{"experimentalApi":false,"requestAttestation":false}
        })
    );
    peer.respond(&initialize, json!({})).await;
    initializing.await.unwrap().unwrap();
    let _ = peer.next_method("initialized").await;
    let starting = {
        let wire = Arc::clone(&wire);
        tokio::spawn(async move {
            wire.start_thread("/workspace", AbortSignal::default())
                .await
        })
    };
    let thread = peer.next_method("thread/start").await;
    peer.respond(
        &thread,
        json!({"thread":{"id":"thread-1","ephemeral":true}}),
    )
    .await;
    starting.await.unwrap().unwrap();

    let run = {
        let wire = Arc::clone(&wire);
        tokio::spawn(async move {
            wire.run_turn(&["first".into(), "second".into()], AbortSignal::default())
                .await
        })
    };
    let turn = peer.next_method("turn/start").await;
    assert_eq!(
        turn["params"],
        json!({
            "threadId":"thread-1",
            "input":[
                {"type":"text","text":"first","text_elements":[]},
                {"type":"text","text":"second","text_elements":[]}
            ]
        })
    );
    peer.respond(&turn, json!({"turn":{"id":"turn-1"}})).await;
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    peer.send(&[
        agent_message(json!("other thread"), json!("final_answer"), "turn-1", "thread-2"),
        agent_message(json!("other turn"), json!("final_answer"), "turn-2", "thread-1"),
        json!({"method":"item/completed","params":{"threadId":"thread-1","turnId":"turn-1","item":{"type":"reasoning","text":"ignored"}}}),
        agent_message(json!("commentary"), json!("commentary"), "turn-1", "thread-1"),
        agent_message(json!("fallback"), Value::Null, "turn-1", "thread-1"),
        agent_message(json!("first final"), json!("final_answer"), "turn-1", "thread-1"),
        agent_message(json!("last final"), json!("final_answer"), "turn-1", "thread-1"),
        turn_completed("completed", Value::Null),
    ]).await;
    let result = run.await.unwrap().unwrap();
    assert_eq!(result.stop_reason, SubagentStopReason::Completed);
    assert_eq!(
        result.output,
        [seekdeep_llm::ContentBlock::Text {
            text: "last final".to_owned()
        }]
    );
    wire.close();
}

#[tokio::test]
async fn uses_nullable_fallback_and_maps_only_explicit_context_exhaustion() {
    let (wire, mut peer) = initialized_wire().await;
    let run = {
        let wire = Arc::clone(&wire);
        tokio::spawn(async move {
            wire.run_turn(&["task".into()], AbortSignal::default())
                .await
        })
    };
    let turn = peer.next_method("turn/start").await;
    peer.respond(&turn, json!({"turn":{"id":"turn-1"}})).await;
    peer.send(&[
        agent_message(json!("fallback"), Value::Null, "turn-1", "thread-1"),
        turn_completed("failed", json!({"codexErrorInfo":"contextWindowExceeded"})),
    ])
    .await;
    let result = run.await.unwrap().unwrap();
    assert_eq!(result.stop_reason, SubagentStopReason::MaxTokens);
    assert_eq!(
        result.output,
        [seekdeep_llm::ContentBlock::Text {
            text: "fallback".to_owned()
        }]
    );
    wire.close();

    let (wire, mut peer) = initialized_wire().await;
    let run = {
        let wire = Arc::clone(&wire);
        tokio::spawn(async move {
            wire.run_turn(&["task".into()], AbortSignal::default())
                .await
        })
    };
    let turn = peer.next_method("turn/start").await;
    peer.respond(&turn, json!({"turn":{"id":"turn-1"}})).await;
    peer.send(&[turn_completed("completed", Value::Null)]).await;
    assert!(
        run.await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("without a final answer")
    );
    wire.close();
}

#[tokio::test]
async fn denies_every_unattended_request_and_fails_unknown_requests_authoritatively() {
    let (wire, mut peer) = initialized_wire().await;
    let run = {
        let wire = Arc::clone(&wire);
        tokio::spawn(async move {
            wire.run_turn(&["task".into()], AbortSignal::default())
                .await
        })
    };
    let turn = peer.next_method("turn/start").await;
    peer.send(&[json!({
        "id":"command",
        "method":"item/commandExecution/requestApproval",
        "params":{"threadId":"thread-1","turnId":"turn-1","availableDecisions":["decline","cancel"]}
    })])
    .await;
    assert_eq!(peer.next().await["result"], json!({"decision":"cancel"}));
    peer.respond(&turn, json!({"turn":{"id":"turn-1"}})).await;
    for (id, method, params, expected) in [
        (
            "file",
            "item/fileChange/requestApproval",
            json!({"threadId":"thread-1","turnId":"turn-1","availableDecisions":["decline"]}),
            json!({"decision":"decline"}),
        ),
        (
            "permissions",
            "item/permissions/requestApproval",
            json!({"threadId":"thread-1","turnId":"turn-1"}),
            json!({"permissions":{},"scope":"turn"}),
        ),
        (
            "input",
            "item/tool/requestUserInput",
            json!({"threadId":"thread-1","turnId":"turn-1"}),
            json!({"answers":{}}),
        ),
        (
            "mcp",
            "mcpServer/elicitation/request",
            json!({"threadId":"thread-1","turnId":null}),
            json!({"action":"decline","content":null,"_meta":null}),
        ),
    ] {
        peer.send(&[json!({"id":id,"method":method,"params":params})])
            .await;
        assert_eq!(peer.next().await["result"], expected);
    }
    peer.send(&[
        agent_message(json!("answer"), json!("final_answer"), "turn-1", "thread-1"),
        turn_completed("completed", Value::Null),
    ])
    .await;
    assert_eq!(
        run.await.unwrap().unwrap().stop_reason,
        SubagentStopReason::Completed
    );
    wire.close();

    let (wire, mut peer) = initialized_wire().await;
    let run = {
        let wire = Arc::clone(&wire);
        tokio::spawn(async move {
            wire.run_turn(&["task".into()], AbortSignal::default())
                .await
        })
    };
    let turn = peer.next_method("turn/start").await;
    peer.send(&[
        json!({"id":turn["id"],"result":{"turn":{"id":"turn-1"}}}),
        json!({"id":"future","method":"future/request","params":{}}),
        agent_message(
            json!("early answer"),
            json!("final_answer"),
            "turn-1",
            "thread-1",
        ),
        turn_completed("completed", Value::Null),
    ])
    .await;
    assert!(
        run.await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("unsupported app-server request")
    );
    let error = peer.next().await;
    assert_eq!(error["error"]["code"], -32603);
    wire.close();
}

#[tokio::test]
async fn enforces_turn_association_and_contains_best_effort_interrupt_failure() {
    let (wire, mut peer) = initialized_wire().await;
    let signal = seekdeep_llm::AbortSignal::default();
    let run = {
        let wire = Arc::clone(&wire);
        let signal = signal.clone();
        tokio::spawn(async move { wire.run_turn(&["task".into()], signal).await })
    };
    let turn = peer.next_method("turn/start").await;
    peer.send(&[json!({
        "method":"turn/started",
        "params":{"threadId":"thread-1","turn":{"id":"conflict"}}
    })])
    .await;
    peer.respond(&turn, json!({"turn":{"id":"turn-1"}})).await;
    assert!(
        run.await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("did not match the active turn")
    );
    wire.close();

    let (wire, mut peer) = initialized_wire().await;
    let signal = seekdeep_llm::AbortSignal::default();
    let run = {
        let wire = Arc::clone(&wire);
        let signal = signal.clone();
        tokio::spawn(async move { wire.run_turn(&["task".into()], signal).await })
    };
    let turn = peer.next_method("turn/start").await;
    peer.respond(&turn, json!({"turn":{"id":"turn-1"}})).await;
    tokio::task::yield_now().await;
    wire.interrupt();
    let interrupt = peer.next_method("turn/interrupt").await;
    assert_eq!(
        interrupt["params"],
        json!({"threadId":"thread-1","turnId":"turn-1"})
    );
    peer.send(&[json!({
        "id":interrupt["id"],
        "error":{"code":-1,"message":"already closed"}
    })])
    .await;
    signal.abort_with_reason(json!("local stop"));
    assert!(
        run.await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("local stop")
    );
    wire.close();
}

#[test]
fn response_error_type_remains_available_to_product_protocol_consumers() {
    let error = JsonRpcResponseError {
        code: Some(1),
        message: "x".into(),
        data: None,
    };
    assert_eq!(error.to_string(), "x");
    assert_eq!(Map::<String, Value>::new().len(), 0);
}
