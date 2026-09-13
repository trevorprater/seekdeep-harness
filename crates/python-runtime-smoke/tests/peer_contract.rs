//! Process transport checks run separately from listener tests to isolate forked descriptors.

#[path = "../src/json.rs"]
mod json;
#[path = "../src/peer.rs"]
mod peer;

use peer::Peer;
use serde_json::json;
use tokio::process::Command;

fn python(script: &str) -> Command {
    let mut command = Command::new(if cfg!(windows) { "python" } else { "python3" });
    command.args(["-c", script]);
    command
}

#[tokio::test]
async fn malformed_lines_are_ignored_but_valid_json_must_be_an_object() {
    let mut child = Peer::spawn(&mut python("import json,sys; print('diagnostic',flush=True); print(json.dumps({'id':'ready'}),flush=True); value=json.loads(sys.stdin.readline()); print(json.dumps(value),flush=True); sys.stdin.read()" )).unwrap();
    assert_eq!(
        child
            .read_until(|value| value["id"] == "ready")
            .await
            .unwrap(),
        [json!({"id":"ready"})]
    );
    let value = json!({"id":"work","text":"é🦀"});
    child.send(&value).await.unwrap();
    let messages = child
        .read_until(|value| value["id"] == "work")
        .await
        .unwrap();
    assert_eq!(messages.len(), 1);
    // JSON object equality ignores field order; snapshots retain it.
    assert_eq!(json::compact(&messages[0]), json::compact(&value));
    child.close().await.unwrap();
    let mut child = Peer::spawn(&mut python(
        "import sys; print('false',flush=True); sys.stdin.read()",
    ))
    .unwrap();
    assert!(
        child
            .read_until(|_| false)
            .await
            .unwrap_err()
            .to_string()
            .contains("non-object JSON")
    );
    child.close().await.unwrap();
}

#[tokio::test]
async fn early_exit_retains_the_complete_stderr_diagnostic() {
    let mut child = Peer::spawn(&mut python(
        "import sys; print('intentional failure',file=sys.stderr,flush=True); sys.exit(7)",
    ))
    .unwrap();
    assert!(
        child
            .read_until(|_| false)
            .await
            .unwrap_err()
            .to_string()
            .contains("exited before expected message")
    );
    let error = child.close().await.unwrap_err().to_string();
    assert!(error.contains('7'));
    assert!(error.contains("intentional failure"));
}

#[tokio::test(start_paused = true)]
async fn uncooperative_process_is_killed_after_the_source_close_grace() {
    let mut child = Peer::spawn(&mut python("import sys,threading; print('{\"results\":[]}',flush=True); sys.stdin.read(); threading.Event().wait()" )).unwrap();
    child.read_adapter().await.unwrap();
    let start = tokio::time::Instant::now();
    let error = child.close().await.unwrap_err().to_string();
    assert!(start.elapsed() >= std::time::Duration::from_secs(10));
    assert!(error.contains("runtime exited"));
}

#[tokio::test]
async fn cancellation_wakes_reads_refuses_writes_and_keeps_close_owned() {
    let mut child = Peer::spawn(&mut python(
        "import json,sys; print('{\"results\":[]}',flush=True); assert json.loads(sys.stdin.read()) == {'cleanup': True}",
    ))
    .unwrap();
    child.read_adapter().await.unwrap();
    let (cancel, cancellation) = tokio::sync::watch::channel(false);
    child.cancel_on(cancellation);
    let signal = tokio::spawn(async move {
        cancel.send(true).unwrap();
    });
    assert_eq!(
        child.read_adapter().await.unwrap_err().to_string(),
        "KeyboardInterrupt"
    );
    signal.await.unwrap();
    assert_eq!(
        child
            .send(&json!({"not":"written"}))
            .await
            .unwrap_err()
            .to_string(),
        "KeyboardInterrupt"
    );
    child.begin_interrupt_cleanup();
    child.send(&json!({"cleanup":true})).await.unwrap();
    child.close().await.unwrap();
}
