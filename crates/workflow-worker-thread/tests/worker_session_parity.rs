//! Typed channel coverage for the killable worker-process session.

use seekdeep_workflow::{WorkflowMeta, WorkflowStopReason};
use seekdeep_workflow_worker_thread::{
    ChildResult, HostToWorkerMessage, WorkerInit, WorkerLimits, WorkerToHostMessage,
    worker::run_worker_session,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

fn init(body: &str) -> WorkerInit {
    WorkerInit {
        meta: WorkflowMeta {
            name: "worker-session".to_owned(),
            description: "typed channel worker session".to_owned(),
            when_to_use: None,
            phases: None,
        },
        body: body.to_owned(),
        args: None,
        limits: WorkerLimits {
            max_concurrent_agents: 2,
            max_total_agents: 10,
            max_items_per_call: 10,
            sync_timeout_ms: 5000,
        },
    }
}

#[tokio::test]
async fn ready_go_child_rpc_and_terminal_result_round_trip() {
    let (worker_send, mut worker_receive) = mpsc::unbounded_channel();
    let (host_send, host_receive) = mpsc::unbounded_channel();
    let session = tokio::spawn(run_worker_session(
        init("phase('Scan'); return await agent('read it', { provider: 'openai' })"),
        worker_send,
        host_receive,
    ));
    assert_eq!(
        worker_receive.recv().await,
        Some(WorkerToHostMessage::Ready)
    );
    host_send.send(HostToWorkerMessage::Go).unwrap();
    assert_eq!(
        worker_receive.recv().await,
        Some(WorkerToHostMessage::Phase {
            title: "Scan".to_owned()
        })
    );
    let Some(WorkerToHostMessage::ChildStart { call_id, request }) = worker_receive.recv().await
    else {
        panic!("expected child start");
    };
    assert_eq!(request.prompt, "read it");
    assert_eq!(request.provider.as_deref(), Some("openai"));
    assert_eq!(request.model, None);
    host_send
        .send(HostToWorkerMessage::ChildStarted {
            call_id,
            child_id: "child-1".to_owned(),
        })
        .unwrap();
    let Some(WorkerToHostMessage::AgentStart { info }) = worker_receive.recv().await else {
        panic!("expected agent start");
    };
    assert_eq!(info.child_id.as_str(), "child-1");
    host_send
        .send(HostToWorkerMessage::ChildSettled {
            call_id,
            result: ChildResult {
                output: vec![seekdeep_llm::ContentBlock::Text {
                    text: "answer".to_owned(),
                }],
                structured: None,
                stop_reason: "completed".to_owned(),
            },
        })
        .unwrap();
    let mut saw_end = false;
    let mut saw_dispose = false;
    let result = loop {
        match worker_receive.recv().await.expect("worker message") {
            WorkerToHostMessage::AgentEnd { info } => {
                saw_end = true;
                assert_eq!(
                    info.outcome,
                    seekdeep_workflow::WorkflowAgentOutcome::Completed
                );
            }
            WorkerToHostMessage::ChildDispose { call_id: disposed } => {
                saw_dispose = true;
                assert_eq!(disposed, call_id);
                host_send
                    .send(HostToWorkerMessage::ChildDisposed { call_id })
                    .unwrap();
            }
            WorkerToHostMessage::Result { result } => break result,
            other => panic!("unexpected worker message: {other:?}"),
        }
    };
    assert!(saw_end);
    assert!(saw_dispose);
    assert_eq!(result.stop_reason, WorkflowStopReason::Completed);
    assert_eq!(result.value, Value::String("answer".to_owned()));
    session.await.unwrap();
}

#[tokio::test]
async fn cancel_before_go_suppresses_body_and_unknown_child_replies_are_noops() {
    let (worker_send, mut worker_receive) = mpsc::unbounded_channel();
    let (host_send, host_receive) = mpsc::unbounded_channel();
    let session = tokio::spawn(run_worker_session(
        init("log('must not run'); return 123"),
        worker_send,
        host_receive,
    ));
    assert_eq!(
        worker_receive.recv().await,
        Some(WorkerToHostMessage::Ready)
    );
    host_send
        .send(HostToWorkerMessage::ChildStarted {
            call_id: 999,
            child_id: "ghost".to_owned(),
        })
        .unwrap();
    host_send
        .send(HostToWorkerMessage::ChildStartError {
            call_id: 999,
            rendered: "ghost".to_owned(),
        })
        .unwrap();
    host_send
        .send(HostToWorkerMessage::ChildSettled {
            call_id: 999,
            result: ChildResult {
                output: Vec::new(),
                structured: None,
                stop_reason: "completed".to_owned(),
            },
        })
        .unwrap();
    host_send
        .send(HostToWorkerMessage::ChildFailed {
            call_id: 999,
            rendered: "ghost".to_owned(),
        })
        .unwrap();
    host_send
        .send(HostToWorkerMessage::ChildDisposed { call_id: 999 })
        .unwrap();
    host_send
        .send(HostToWorkerMessage::Cancel {
            reason: "before body".to_owned(),
        })
        .unwrap();
    let Some(WorkerToHostMessage::Result { result }) = worker_receive.recv().await else {
        panic!("expected result without narration");
    };
    assert_eq!(result.stop_reason, WorkflowStopReason::Cancelled);
    assert_eq!(result.value, json!(null));
    assert!(result.error.unwrap().contains("before body"));
    session.await.unwrap();
}
