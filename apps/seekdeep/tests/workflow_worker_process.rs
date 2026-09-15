//! Single-binary distribution smoke for the internal workflow worker mode.

use std::process::Stdio;

use seekdeep_workflow_worker_thread::{
    HostToWorkerMessage, WorkerInit, WorkerLimits, WorkerToHostMessage,
};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

#[tokio::test]
async fn installed_seekdeep_binary_can_host_the_scrubbed_worker_process() {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_seekdeep"));
    command
        .env_clear()
        .env("SEEKDEEP_INTERNAL_WORKFLOW_WORKER", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let init = WorkerInit {
        meta: seekdeep_workflow::WorkflowMeta {
            name: "integrated-worker".to_owned(),
            description: "single binary worker smoke".to_owned(),
            when_to_use: None,
            phases: None,
        },
        body: "return 6 * 7".to_owned(),
        args: None,
        limits: WorkerLimits {
            max_concurrent_agents: 1,
            max_total_agents: 1,
            max_items_per_call: 1,
            sync_timeout_ms: 5000,
        },
    };
    stdin
        .write_all(&serde_json::to_vec(&init).unwrap())
        .await
        .unwrap();
    stdin.write_all(b"\n").await.unwrap();
    stdin.flush().await.unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let ready = lines.next_line().await.unwrap().unwrap();
    assert_eq!(
        serde_json::from_str::<WorkerToHostMessage>(&ready).unwrap(),
        WorkerToHostMessage::Ready
    );
    stdin
        .write_all(&serde_json::to_vec(&HostToWorkerMessage::Go).unwrap())
        .await
        .unwrap();
    stdin.write_all(b"\n").await.unwrap();
    stdin.flush().await.unwrap();
    let result = lines.next_line().await.unwrap().unwrap();
    let WorkerToHostMessage::Result { result } =
        serde_json::from_str::<WorkerToHostMessage>(&result).unwrap()
    else {
        panic!("expected workflow result");
    };
    assert_eq!(result.value, serde_json::json!(42));
    drop(stdin);
    assert!(child.wait().await.unwrap().success());
}
