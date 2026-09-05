//! Behavioral mirror of the Client plugin's complete `ctx.connection` handle.

use std::{
    collections::VecDeque,
    sync::{Arc, Weak},
};

use futures::{StreamExt, channel::mpsc, future::BoxFuture, stream::BoxStream};
use parking_lot::Mutex;
use seekdeep_client_connection::{
    ClientConnection, ClientConnectionFuture, ClientConnectionHandle, ConnectionConfig,
    ConnectionSinks, ConnectionState, ConnectionStopHandle, EventFrame, HostDescription, RpcId,
    RpcResult, StreamApi,
};
use seekdeep_llm::AbortSignal;
use serde_json::{Value, json};

struct NoopCaller;

impl ClientConnection for NoopCaller {
    fn call(
        &self,
        _channel: &str,
        _endpoint: &str,
        _payload: Value,
        _signal: AbortSignal,
    ) -> ClientConnectionFuture {
        Box::pin(async { Ok(RpcResult::Success { value: None }) })
    }
}

struct StreamControl {
    frames: mpsc::UnboundedSender<anyhow::Result<EventFrame>>,
}

impl StreamControl {
    fn end(&mut self) {
        self.frames.close_channel();
    }
}

#[derive(Default)]
struct HandleApi {
    descriptions: Mutex<VecDeque<RpcResult<HostDescription>>>,
    mux: Mutex<VecDeque<mpsc::UnboundedReceiver<anyhow::Result<EventFrame>>>>,
    host: Mutex<VecDeque<mpsc::UnboundedReceiver<anyhow::Result<EventFrame>>>>,
}

impl HandleApi {
    fn generation(&self, host: &str) -> (StreamControl, StreamControl) {
        let (mux_tx, mux_rx) = mpsc::unbounded();
        let (host_tx, host_rx) = mpsc::unbounded();
        self.descriptions.lock().push_back(RpcResult::Success {
            value: Some(json!({ "host": host })),
        });
        self.mux.lock().push_back(mux_rx);
        self.host.lock().push_back(host_rx);
        (
            StreamControl { frames: mux_tx },
            StreamControl { frames: host_tx },
        )
    }
}

fn open_stream(
    frames: mpsc::UnboundedReceiver<anyhow::Result<EventFrame>>,
    signal: AbortSignal,
    on_open: Arc<dyn Fn() + Send + Sync>,
) -> BoxStream<'static, anyhow::Result<EventFrame>> {
    Box::pin(futures::stream::unfold(
        (false, frames, signal, on_open),
        |(mut opened, mut frames, signal, on_open)| async move {
            if !opened {
                on_open();
                opened = true;
            }
            tokio::select! {
                () = signal.cancelled() => None,
                item = frames.next() => item.map(|item| {
                    (item, (opened, frames, signal, on_open))
                }),
            }
        },
    ))
}

impl StreamApi for HandleApi {
    fn describe(&self) -> BoxFuture<'static, anyhow::Result<RpcResult<HostDescription>>> {
        let result = self.descriptions.lock().pop_front().unwrap();
        Box::pin(async move { Ok(result) })
    }

    fn mux(
        &self,
        signal: AbortSignal,
        on_open: Arc<dyn Fn() + Send + Sync>,
    ) -> BoxStream<'static, anyhow::Result<EventFrame>> {
        open_stream(self.mux.lock().pop_front().unwrap(), signal, on_open)
    }

    fn host(
        &self,
        signal: AbortSignal,
        on_open: Arc<dyn Fn() + Send + Sync>,
    ) -> BoxStream<'static, anyhow::Result<EventFrame>> {
        open_stream(self.host.lock().pop_front().unwrap(), signal, on_open)
    }
}

fn config() -> ConnectionConfig {
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
async fn publishes_description_owns_one_loop_and_stop_retracts_snapshot() {
    let api = Arc::new(HandleApi::default());
    let (_mux, _host) = api.generation("first");
    let handle = ClientConnectionHandle::with_streams(Arc::new(NoopCaller), api, false);
    assert!(!handle.is_loopback());
    assert_eq!(handle.host_description(), None);
    let notices = Arc::new(Mutex::new(Vec::new()));
    let subscription = handle.subscribe_host_description({
        let notices = notices.clone();
        let handle = handle.clone();
        Arc::new(move || notices.lock().push(handle.host_description()))
    });
    let connected = Arc::new(Mutex::new(Vec::new()));
    let stop = handle
        .start(
            ConnectionSinks {
                on_connected: Some({
                    let connected = connected.clone();
                    Arc::new(move |value| connected.lock().push(value))
                }),
                ..ConnectionSinks::default()
            },
            config(),
        )
        .unwrap();
    settle().await;
    assert_eq!(handle.host_description(), Some(json!({ "host": "first" })));
    assert_eq!(*connected.lock(), vec![json!({ "host": "first" })]);
    assert_eq!(notices.lock().len(), 1);
    assert!(
        handle
            .start(ConnectionSinks::default(), config())
            .unwrap_err()
            .to_string()
            .contains("already owned")
    );
    stop.stop();
    assert_eq!(handle.host_description(), None);
    assert_eq!(notices.lock().len(), 2);
    subscription.dispose();
    stop.stop();
    assert_eq!(notices.lock().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn reconnect_retracts_then_republishes_even_for_equal_json_descriptions() {
    let api = Arc::new(HandleApi::default());
    let (mut first_mux, _first_host) = api.generation("same");
    let (_second_mux, _second_host) = api.generation("same");
    let handle = ClientConnectionHandle::with_streams(Arc::new(NoopCaller), api, true);
    let snapshots = Arc::new(Mutex::new(Vec::new()));
    let _subscription = handle.subscribe_host_description({
        let snapshots = snapshots.clone();
        let handle = handle.clone();
        Arc::new(move || snapshots.lock().push(handle.host_description()))
    });
    let states = Arc::new(Mutex::new(Vec::new()));
    let stop = handle
        .start(
            ConnectionSinks {
                on_state_change: Some({
                    let states = states.clone();
                    Arc::new(move |state| states.lock().push(state))
                }),
                ..ConnectionSinks::default()
            },
            config(),
        )
        .unwrap();
    settle().await;
    first_mux.end();
    settle().await;
    tokio::time::advance(std::time::Duration::from_millis(2)).await;
    settle().await;
    assert_eq!(
        *states.lock(),
        vec![
            ConnectionState::Connected,
            ConnectionState::Reconnecting,
            ConnectionState::Connected,
        ]
    );
    assert_eq!(
        *snapshots.lock(),
        vec![
            Some(json!({ "host": "same" })),
            None,
            Some(json!({ "host": "same" })),
        ]
    );
    stop.stop();
}

#[tokio::test(start_paused = true)]
async fn description_subscriber_can_stop_synchronously_before_consumer_notification() {
    let api = Arc::new(HandleApi::default());
    let (_mux, _host) = api.generation("stopped");
    let handle = ClientConnectionHandle::with_streams(Arc::new(NoopCaller), api, true);
    let stop_slot = Arc::new(Mutex::new(None::<Arc<ConnectionStopHandle>>));
    let _subscription = handle.subscribe_host_description({
        let stop_slot = stop_slot.clone();
        Arc::new(move || {
            let stop = stop_slot.lock().clone();
            if let Some(stop) = stop {
                stop.stop();
            }
        })
    });
    let connected = Arc::new(Mutex::new(Vec::new()));
    let stop = handle
        .start(
            ConnectionSinks {
                on_connected: Some({
                    let connected = connected.clone();
                    Arc::new(move |value| connected.lock().push(value))
                }),
                ..ConnectionSinks::default()
            },
            config(),
        )
        .unwrap();
    *stop_slot.lock() = Some(Arc::new(stop));
    settle().await;
    assert_eq!(handle.host_description(), None);
    assert!(connected.lock().is_empty());
}

#[tokio::test(start_paused = true)]
async fn description_listener_failure_is_contained_and_snapshot_delivery_continues() {
    let api = Arc::new(HandleApi::default());
    let (_mux, _host) = api.generation("live");
    let handle = ClientConnectionHandle::with_streams(Arc::new(NoopCaller), api, true);
    let _panicking = handle.subscribe_host_description(Arc::new(|| panic!("listener broke")));
    let delivered = Arc::new(Mutex::new(Vec::new()));
    let _delivering = handle.subscribe_host_description({
        let delivered = delivered.clone();
        let weak: Weak<ClientConnectionHandle> = Arc::downgrade(&handle);
        Arc::new(move || {
            delivered
                .lock()
                .push(weak.upgrade().unwrap().host_description());
        })
    });
    let stop = handle.start(ConnectionSinks::default(), config()).unwrap();
    settle().await;
    assert_eq!(*delivered.lock(), vec![Some(json!({ "host": "live" }))]);
    stop.stop();
    assert_eq!(delivered.lock().last(), Some(&None));
}

#[test]
fn transport_only_handle_stays_loopback_and_refuses_stream_ownership() {
    let handle = ClientConnectionHandle::new(Arc::new(NoopCaller));
    assert!(handle.is_loopback());
    assert!(
        handle
            .start(ConnectionSinks::default(), config())
            .unwrap_err()
            .to_string()
            .contains("no downstream stream API")
    );
    assert!(
        handle
            .start(ConnectionSinks::default(), config())
            .unwrap_err()
            .to_string()
            .contains("no downstream stream API")
    );
}

#[test]
fn event_frame_contract_keeps_rpc_id_and_payload() {
    assert_eq!(
        serde_json::to_value(EventFrame {
            rpc_id: RpcId::new("frame"),
            payload: json!({ "type": "host/status" }),
        })
        .unwrap(),
        json!({ "rpcId": "frame", "payload": { "type": "host/status" } })
    );
}
