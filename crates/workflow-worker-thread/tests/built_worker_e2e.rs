//! Plain-process smoke for the compiled workflow worker artifact.

use std::process::Stdio;

use seekdeep_workflow::WorkflowStopReason;
use seekdeep_workflow_worker_thread::{
    ChildResult, HostToWorkerMessage, WorkerInit, WorkerLimits, WorkerToHostMessage,
};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

fn init() -> WorkerInit {
    WorkerInit {
        meta: seekdeep_workflow::WorkflowMeta {
            name: "built-smoke".to_owned(),
            description: "compiled worker smoke".to_owned(),
            when_to_use: None,
            phases: None,
        },
        body: "const child = await agent('answer'); return { child, process: typeof process }"
            .to_owned(),
        args: None,
        limits: WorkerLimits {
            max_concurrent_agents: 2,
            max_total_agents: 10,
            max_items_per_call: 10,
            sync_timeout_ms: 5000,
        },
    }
}

async fn send(
    stdin: &mut tokio::process::ChildStdin,
    message: &HostToWorkerMessage,
) -> anyhow::Result<()> {
    stdin.write_all(&serde_json::to_vec(message)?).await?;
    stdin.write_all(b"\n").await?;
    stdin.flush().await?;
    Ok(())
}

#[tokio::test]
async fn compiled_worker_runs_under_a_scrubbed_plain_process_and_exits_cleanly() {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_seekdeep-workflow-worker"));
    command
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn().expect("spawn compiled worker");
    let mut stdin = child.stdin.take().expect("worker stdin");
    let stdout = child.stdout.take().expect("worker stdout");
    stdin
        .write_all(&serde_json::to_vec(&init()).unwrap())
        .await
        .unwrap();
    stdin.write_all(b"\n").await.unwrap();
    stdin.flush().await.unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let mut child_starts = 0;
    let result = loop {
        let line = tokio::time::timeout(std::time::Duration::from_secs(10), lines.next_line())
            .await
            .expect("worker message timeout")
            .unwrap()
            .expect("worker message");
        match serde_json::from_str::<WorkerToHostMessage>(&line).unwrap() {
            WorkerToHostMessage::Ready => {
                send(&mut stdin, &HostToWorkerMessage::Go).await.unwrap();
            }
            WorkerToHostMessage::ChildStart { call_id, request } => {
                child_starts += 1;
                assert_eq!(request.prompt, "answer");
                send(
                    &mut stdin,
                    &HostToWorkerMessage::ChildStarted {
                        call_id,
                        child_id: "built-child".to_owned(),
                    },
                )
                .await
                .unwrap();
                send(
                    &mut stdin,
                    &HostToWorkerMessage::ChildSettled {
                        call_id,
                        result: ChildResult {
                            output: vec![seekdeep_llm::ContentBlock::Text {
                                text: "forty-two".to_owned(),
                            }],
                            structured: None,
                            stop_reason: "completed".to_owned(),
                        },
                    },
                )
                .await
                .unwrap();
            }
            WorkerToHostMessage::ChildDispose { call_id } => {
                send(&mut stdin, &HostToWorkerMessage::ChildDisposed { call_id })
                    .await
                    .unwrap();
            }
            WorkerToHostMessage::Result { result } => break result,
            WorkerToHostMessage::Phase { .. }
            | WorkerToHostMessage::Log { .. }
            | WorkerToHostMessage::AgentStart { .. }
            | WorkerToHostMessage::AgentEnd { .. } => {}
        }
    };
    assert_eq!(child_starts, 1);
    assert_eq!(result.stop_reason, WorkflowStopReason::Completed);
    assert_eq!(
        result.value,
        serde_json::json!({"child": "forty-two", "process": "undefined"})
    );
    drop(stdin);
    let status = tokio::time::timeout(std::time::Duration::from_secs(10), child.wait())
        .await
        .expect("worker exit timeout")
        .unwrap();
    assert!(status.success());
}
