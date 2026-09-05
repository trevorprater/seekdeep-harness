//! Behavioral mirror of the browser Connection generation controller.

use std::{
    collections::VecDeque,
    sync::{
        Arc, Weak,
        atomic::{AtomicUsize, Ordering},
    },
};

use futures::{StreamExt, channel::mpsc, future::BoxFuture, stream::BoxStream};
use parking_lot::Mutex;
use seekdeep_client_connection::{
    ConnectionConfig, ConnectionController, ConnectionSinks, ConnectionState, EventFrame,
    HostDescription, RpcError, RpcId, RpcResult, StreamApi,
};
use seekdeep_llm::AbortSignal;
use serde_json::{Value, json};
use tokio::sync::oneshot;

struct StreamSpec {
    open: oneshot::Receiver<()>,
    frames: mpsc::UnboundedReceiver<anyhow::Result<EventFrame>>,
}

struct StreamControl {
    open: Option<oneshot::Sender<()>>,
    frames: mpsc::UnboundedSender<anyhow::Result<EventFrame>>,
}

fn stream_pair() -> (StreamSpec, StreamControl) {
    let (open_tx, open_rx) = oneshot::channel();
    let (frames_tx, frames_rx) = mpsc::unbounded();
    (
        StreamSpec {
            open: open_rx,
            frames: frames_rx,
        },
        StreamControl {
            open: Some(open_tx),
            frames: frames_tx,
        },
    )
}

impl StreamControl {
    fn open(&mut self) {
        if let Some(sender) = self.open.take() {
            let _ = sender.send(());
        }
    }

    fn frame(&mut self, kind: &str, value: impl serde::Serialize) {
        self.frames
            .start_send(Ok(EventFrame {
                rpc_id: RpcId::new(format!("{kind}-id")),
                payload: json!({ "type": kind, "value": value }),
            }))
            .unwrap();
    }

    fn stream_error(&mut self) {
        self.frames
            .start_send(Ok(EventFrame {
                rpc_id: RpcId::new("stream-error"),
                payload: json!({
                    "type": "stream/error",
                    "error": { "code": "internal", "message": "lost", "details": {} },
                }),
            }))
            .unwrap();
    }
}

type DescriptionSender = oneshot::Sender<anyhow::Result<RpcResult<HostDescription>>>;

#[derive(Default)]
struct FakeApi {
    descriptions: Mutex<VecDeque<oneshot::Receiver<anyhow::Result<RpcResult<HostDescription>>>>>,
    mux: Mutex<VecDeque<StreamSpec>>,
    host: Mutex<VecDeque<StreamSpec>>,
    mux_calls: AtomicUsize,
    host_calls: AtomicUsize,
    describe_calls: AtomicUsize,
}

impl FakeApi {
    fn generation(&self) -> (DescriptionSender, StreamControl, StreamControl) {
        let (description_tx, description_rx) = oneshot::channel();
        let (mux, mux_control) = stream_pair();
        let (host, host_control) = stream_pair();
        self.descriptions.lock().push_back(description_rx);
        self.mux.lock().push_back(mux);
        self.host.lock().push_back(host);
        (description_tx, mux_control, host_control)
    }
}

fn controlled_stream(
    spec: StreamSpec,
    on_open: Arc<dyn Fn() + Send + Sync>,
) -> BoxStream<'static, anyhow::Result<EventFrame>> {
    Box::pin(futures::stream::unfold(
        (false, Some(spec.open), spec.frames, on_open),
        |(mut opened, mut gate, mut frames, on_open)| async move {
            if !opened {
                let gate = gate.take()?;
                if gate.await.is_err() {
                    return None;
                }
                on_open();
                opened = true;
            }
            frames
                .next()
                .await
                .map(|item| (item, (opened, gate, frames, on_open)))
        },
    ))
}

impl StreamApi for FakeApi {
    fn describe(&self) -> BoxFuture<'static, anyhow::Result<RpcResult<HostDescription>>> {
        self.describe_calls.fetch_add(1, Ordering::AcqRel);
        let receiver = self.descriptions.lock().pop_front().unwrap();
        Box::pin(async move { receiver.await.unwrap() })
    }

    fn mux(
        &self,
        _signal: AbortSignal,
        on_open: Arc<dyn Fn() + Send + Sync>,
    ) -> BoxStream<'static, anyhow::Result<EventFrame>> {
        self.mux_calls.fetch_add(1, Ordering::AcqRel);
        controlled_stream(self.mux.lock().pop_front().unwrap(), on_open)
    }

    fn host(
        &self,
        _signal: AbortSignal,
        on_open: Arc<dyn Fn() + Send + Sync>,
    ) -> BoxStream<'static, anyhow::Result<EventFrame>> {
        self.host_calls.fetch_add(1, Ordering::AcqRel);
        controlled_stream(self.host.lock().pop_front().unwrap(), on_open)
    }
}

fn description(value: &str) -> RpcResult<HostDescription> {
    RpcResult::Success {
        value: Some(json!({ "host": value })),
    }
}

fn fast_config() -> ConnectionConfig {
    ConnectionConfig {
        backoff_base_ms: 1.0,
        backoff_factor: 1.0,
        backoff_max_ms: 1.0,
        stream_open_timeout_ms: 10.0,
    }
}

async fn settle() {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test(start_paused = true)]
async fn announces_only_after_describe_and_both_streams_then_pumps_frames() {
    let api = Arc::new(FakeApi::default());
    let (describe, mut mux, mut host) = api.generation();
    let mux_frames = Arc::new(Mutex::new(Vec::new()));
    let host_frames = Arc::new(Mutex::new(Vec::new()));
    let connected = Arc::new(Mutex::new(Vec::new()));
    let states = Arc::new(Mutex::new(Vec::new()));
    let controller = ConnectionController::new(
        api,
        ConnectionSinks {
            on_mux_envelope: Some({
                let values = mux_frames.clone();
                Arc::new(move |frame| values.lock().push(frame))
            }),
            on_host_envelope: Some({
                let values = host_frames.clone();
                Arc::new(move |frame| values.lock().push(frame))
            }),
            on_connected: Some({
                let values = connected.clone();
                Arc::new(move |value| values.lock().push(value))
            }),
            on_state_change: Some({
                let values = states.clone();
                Arc::new(move |state| values.lock().push(state))
            }),
        },
        fast_config(),
    );
    controller.start();
    describe.send(Ok(description("fixture"))).unwrap();
    mux.open();
    settle().await;
    assert!(connected.lock().is_empty());
    host.open();
    settle().await;
    assert_eq!(*connected.lock(), vec![json!({ "host": "fixture" })]);
    assert_eq!(*states.lock(), vec![ConnectionState::Connected]);
    mux.frame("mux/message", json!(1));
    host.frame("host/status", json!(2));
    settle().await;
    assert_eq!(mux_frames.lock().len(), 1);
    assert_eq!(host_frames.lock().len(), 1);
    controller.stop();
}

#[tokio::test(start_paused = true)]
async fn stream_open_timeout_runs_concurrently_with_describe() {
    let api = Arc::new(FakeApi::default());
    let (describe, _mux, _host) = api.generation();
    let connected = Arc::new(Mutex::new(Vec::new()));
    let controller = ConnectionController::new(
        api,
        ConnectionSinks {
            on_connected: Some({
                let values = connected.clone();
                Arc::new(move |value| values.lock().push(value))
            }),
            ..ConnectionSinks::default()
        },
        fast_config(),
    );
    controller.start();
    settle().await;
    tokio::time::advance(std::time::Duration::from_millis(20)).await;
    settle().await;
    assert!(
        connected.lock().is_empty(),
        "describe remains part of readiness"
    );
    describe.send(Ok(description("late"))).unwrap();
    settle().await;
    assert_eq!(*connected.lock(), vec![json!({ "host": "late" })]);
    controller.stop();
}

#[tokio::test(start_paused = true)]
async fn business_describe_failure_and_stream_error_reconnect_without_dispatch() {
    let api = Arc::new(FakeApi::default());
    let (first_describe, mut first_mux, mut first_host) = api.generation();
    let (second_describe, mut second_mux, mut second_host) = api.generation();
    let mux_frames = Arc::new(Mutex::new(Vec::new()));
    let states = Arc::new(Mutex::new(Vec::new()));
    let controller = ConnectionController::new(
        api.clone(),
        ConnectionSinks {
            on_mux_envelope: Some({
                let values = mux_frames.clone();
                Arc::new(move |frame| values.lock().push(frame))
            }),
            on_state_change: Some({
                let values = states.clone();
                Arc::new(move |state| values.lock().push(state))
            }),
            ..ConnectionSinks::default()
        },
        fast_config(),
    );
    controller.start();
    settle().await;
    first_mux.open();
    first_host.open();
    first_describe
        .send(Ok(RpcResult::Failure {
            error: RpcError {
                code: "internal".to_owned(),
                message: "not ready".to_owned(),
                details: serde_json::Map::new(),
            },
        }))
        .unwrap();
    settle().await;
    tokio::time::advance(std::time::Duration::from_millis(2)).await;
    settle().await;
    second_mux.open();
    second_host.open();
    second_describe.send(Ok(description("second"))).unwrap();
    settle().await;
    assert_eq!(api.describe_calls.load(Ordering::Acquire), 2);
    assert_eq!(
        *states.lock(),
        vec![ConnectionState::Reconnecting, ConnectionState::Connected]
    );
    second_mux.stream_error();
    settle().await;
    assert!(mux_frames.lock().is_empty());
    assert_eq!(states.lock().last(), Some(&ConnectionState::Reconnecting));
    controller.stop();
}

#[tokio::test(start_paused = true)]
async fn sink_panics_are_contained_and_start_is_idempotent() {
    let api = Arc::new(FakeApi::default());
    let (describe, mut mux, mut host) = api.generation();
    let host_frames = Arc::new(AtomicUsize::new(0));
    let controller = ConnectionController::new(
        api.clone(),
        ConnectionSinks {
            on_mux_envelope: Some(Arc::new(|_| panic!("business sink broke"))),
            on_host_envelope: Some({
                let count = host_frames.clone();
                Arc::new(move |_| {
                    count.fetch_add(1, Ordering::AcqRel);
                })
            }),
            ..ConnectionSinks::default()
        },
        fast_config(),
    );
    controller.start();
    controller.start();
    settle().await;
    assert_eq!(api.mux_calls.load(Ordering::Acquire), 1);
    assert_eq!(api.host_calls.load(Ordering::Acquire), 1);
    mux.open();
    host.open();
    describe.send(Ok(description("fixture"))).unwrap();
    settle().await;
    mux.frame("mux/message", Value::Null);
    host.frame("host/status", Value::Null);
    settle().await;
    assert_eq!(host_frames.load(Ordering::Acquire), 1);
    assert!(controller.is_running());
    controller.stop();
}

#[tokio::test(start_paused = true)]
async fn synchronous_connected_state_stop_suppresses_generation_description() {
    let api = Arc::new(FakeApi::default());
    let (describe, mut mux, mut host) = api.generation();
    let slot = Arc::new(Mutex::new(Weak::<ConnectionController>::new()));
    let connected = Arc::new(AtomicUsize::new(0));
    let controller = ConnectionController::new(
        api,
        ConnectionSinks {
            on_connected: Some({
                let count = connected.clone();
                Arc::new(move |_| {
                    count.fetch_add(1, Ordering::AcqRel);
                })
            }),
            on_state_change: Some({
                let slot = slot.clone();
                Arc::new(move |state| {
                    if state == ConnectionState::Connected {
                        slot.lock().upgrade().unwrap().stop();
                    }
                })
            }),
            ..ConnectionSinks::default()
        },
        fast_config(),
    );
    *slot.lock() = Arc::downgrade(&controller);
    controller.start();
    mux.open();
    host.open();
    describe.send(Ok(description("stopped"))).unwrap();
    settle().await;
    assert_eq!(connected.load(Ordering::Acquire), 0);
    assert!(!controller.is_running());
}

#[tokio::test(start_paused = true)]
async fn stream_failure_emits_reconnecting_once_across_consecutive_failures() {
    let api = Arc::new(FakeApi::default());
    let (first_description, mut first_mux, mut first_host) = api.generation();
    let (second_description, mut second_mux, mut second_host) = api.generation();
    let states = Arc::new(Mutex::new(Vec::new()));
    let controller = ConnectionController::new(
        api,
        ConnectionSinks {
            on_state_change: Some({
                let values = states.clone();
                Arc::new(move |state| values.lock().push(state))
            }),
            ..ConnectionSinks::default()
        },
        fast_config(),
    );
    controller.start();
    settle().await;
    first_mux.open();
    first_host.open();
    first_description
        .send(Err(anyhow::anyhow!("describe down")))
        .unwrap();
    settle().await;
    tokio::time::advance(std::time::Duration::from_millis(2)).await;
    settle().await;
    second_mux.open();
    second_host.open();
    second_description
        .send(Err(anyhow::anyhow!("still down")))
        .unwrap();
    settle().await;
    tokio::time::advance(std::time::Duration::from_millis(2)).await;
    settle().await;
    assert_eq!(*states.lock(), vec![ConnectionState::Reconnecting]);
    controller.stop();
}
