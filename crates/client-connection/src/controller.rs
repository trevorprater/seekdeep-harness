//! Dual downstream stream controller with generation handshakes and reconnect backoff.

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use futures::{StreamExt, future::BoxFuture, stream::BoxStream};
use parking_lot::Mutex;
use seekdeep_llm::AbortSignal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::{RpcId, RpcResult};

/// Successful value returned by the Host handshake.
pub type HostDescription = Value;

/// One correlated downstream event envelope.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventFrame {
    /// Host-minted event id.
    #[serde(rename = "rpcId")]
    pub rpc_id: RpcId,
    /// Mux or Host frame payload.
    pub payload: Value,
}

impl EventFrame {
    fn is_stream_error(&self) -> bool {
        self.payload
            .as_object()
            .and_then(|payload| payload.get("type"))
            .and_then(Value::as_str)
            == Some("stream/error")
    }
}

/// Physical API operations required by the Connection controller.
pub trait StreamApi: Send + Sync + 'static {
    /// Proves unary reachability for one generation.
    fn describe(&self) -> BoxFuture<'static, anyhow::Result<RpcResult<HostDescription>>>;

    /// Opens the mux downstream. The carrier calls `on_open` at physical establishment.
    fn mux(
        &self,
        signal: AbortSignal,
        on_open: Arc<dyn Fn() + Send + Sync>,
    ) -> BoxStream<'static, anyhow::Result<EventFrame>>;

    /// Opens the Host downstream. The carrier calls `on_open` at physical establishment.
    fn host(
        &self,
        signal: AbortSignal,
        on_open: Arc<dyn Fn() + Send + Sync>,
    ) -> BoxStream<'static, anyhow::Result<EventFrame>>;
}

/// Reconnect and handshake tunables.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ConnectionConfig {
    /// First retry backoff cap in milliseconds.
    pub backoff_base_ms: f64,
    /// Exponential growth factor per consecutive failed attempt.
    pub backoff_factor: f64,
    /// Upper backoff cap in milliseconds.
    pub backoff_max_ms: f64,
    /// Maximum wait for both physical stream-open callbacks.
    pub stream_open_timeout_ms: f64,
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            backoff_base_ms: 500.0,
            backoff_factor: 2.0,
            backoff_max_ms: 10_000.0,
            stream_open_timeout_ms: 3_000.0,
        }
    }
}

/// Coarse connection state exposed to the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    /// Handshake complete for the current generation.
    Connected,
    /// Current generation failed; covers backoff and subsequent attempts.
    Reconnecting,
}

/// Isolated business-layer callbacks fed by the physical controller.
#[derive(Clone, Default)]
pub struct ConnectionSinks {
    /// Mux envelope sink.
    pub on_mux_envelope: Option<Arc<dyn Fn(EventFrame) + Send + Sync>>,
    /// Host envelope sink.
    pub on_host_envelope: Option<Arc<dyn Fn(EventFrame) + Send + Sync>>,
    /// Completed generation handshake sink.
    pub on_connected: Option<Arc<dyn Fn(HostDescription) + Send + Sync>>,
    /// Deduplicated coarse state sink.
    pub on_state_change: Option<Arc<dyn Fn(ConnectionState) + Send + Sync>>,
}

impl std::fmt::Debug for ConnectionSinks {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConnectionSinks")
            .field("on_mux_envelope", &self.on_mux_envelope.is_some())
            .field("on_host_envelope", &self.on_host_envelope.is_some())
            .field("on_connected", &self.on_connected.is_some())
            .field("on_state_change", &self.on_state_change.is_some())
            .finish()
    }
}

struct ControllerState {
    attempt: u32,
    current: Option<AbortSignal>,
    last_state: Option<ConnectionState>,
}

/// Idempotently started dual-stream generation controller.
pub struct ConnectionController {
    api: Arc<dyn StreamApi>,
    sinks: ConnectionSinks,
    config: ConnectionConfig,
    running: AtomicBool,
    generation: AtomicU64,
    state: Mutex<ControllerState>,
}

impl std::fmt::Debug for ConnectionController {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConnectionController")
            .field("running", &self.running.load(Ordering::Acquire))
            .field("generation", &self.generation.load(Ordering::Acquire))
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl ConnectionController {
    /// Creates a stopped controller.
    #[must_use]
    pub fn new(
        api: Arc<dyn StreamApi>,
        sinks: ConnectionSinks,
        config: ConnectionConfig,
    ) -> Arc<Self> {
        Arc::new(Self {
            api,
            sinks,
            config,
            running: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            state: Mutex::new(ControllerState {
                attempt: 0,
                current: None,
                last_state: None,
            }),
        })
    }

    /// Idempotently begins the connect/pump/reconnect loop.
    pub fn start(self: &Arc<Self>) {
        if self.running.swap(true, Ordering::AcqRel) {
            return;
        }
        let controller = self.clone();
        tokio::spawn(async move { controller.run().await });
    }

    /// Stops the loop and aborts the current generation's streams.
    pub fn stop(&self) {
        self.running.store(false, Ordering::Release);
        if let Some(signal) = self.state.lock().current.take() {
            signal.abort();
        }
    }

    /// Whether the controller currently owns its loop.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    async fn run(self: Arc<Self>) {
        while self.is_running() {
            let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
            let signal = AbortSignal::default();
            self.state.lock().current = Some(signal.clone());
            let (events_tx, mut events_rx) = mpsc::unbounded_channel();

            let mux_open_tx = events_tx.clone();
            let mux_open: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                let _ = mux_open_tx.send(PumpEvent::Opened(StreamKind::Mux));
            });
            let host_open_tx = events_tx.clone();
            let host_open: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                let _ = host_open_tx.send(PumpEvent::Opened(StreamKind::Host));
            });

            let mux = self.api.mux(signal.clone(), mux_open);
            let host = self.api.host(signal.clone(), host_open);
            self.spawn_pump(
                generation,
                signal.clone(),
                StreamKind::Mux,
                mux,
                events_tx.clone(),
            );
            self.spawn_pump(
                generation,
                signal.clone(),
                StreamKind::Host,
                host,
                events_tx,
            );

            let description = self.api.describe();
            let stream_readiness = wait_for_streams(
                &mut events_rx,
                duration_ms(self.config.stream_open_timeout_ms),
                &signal,
            );
            let (description, streams_ready) = tokio::join!(description, stream_readiness);
            let description = description.and_then(|result| match result {
                RpcResult::Success { value: Some(value) } => Ok(value),
                RpcResult::Success { value: None } => {
                    Err(anyhow::anyhow!("host.describe returned no value"))
                }
                RpcResult::Failure { error } => Err(anyhow::anyhow!(
                    "host.describe failed: {}: {}",
                    error.code,
                    error.message
                )),
            });

            if streams_ready && !signal.is_aborted() {
                if let Ok(description) = description {
                    self.state.lock().attempt = 0;
                    self.emit_state(ConnectionState::Connected);
                    if self.generation_active(generation, &signal) {
                        Self::call_sink(|| {
                            if let Some(sink) = &self.sinks.on_connected {
                                sink(description);
                            }
                        });
                    }
                } else {
                    signal.abort();
                }
            } else if !signal.is_aborted() {
                signal.abort();
            }

            wait_for_failure(&mut events_rx, &signal).await;
            if !self.is_running() {
                return;
            }
            self.emit_state(ConnectionState::Reconnecting);
            let attempt = {
                let mut state = self.state.lock();
                state.attempt = state.attempt.saturating_add(1);
                state.attempt
            };
            tracing::warn!(attempt, "[web-runtime] connection lost, retry");
            tokio::time::sleep(self.backoff_delay(attempt)).await;
        }
    }

    fn spawn_pump(
        &self,
        generation: u64,
        signal: AbortSignal,
        kind: StreamKind,
        mut stream: BoxStream<'static, anyhow::Result<EventFrame>>,
        events: mpsc::UnboundedSender<PumpEvent>,
    ) {
        let sink = match kind {
            StreamKind::Mux => self.sinks.on_mux_envelope.clone(),
            StreamKind::Host => self.sinks.on_host_envelope.clone(),
        };
        tokio::spawn(async move {
            loop {
                let item = tokio::select! {
                    () = signal.cancelled() => break,
                    item = stream.next() => item,
                };
                let Some(Ok(frame)) = item else {
                    break;
                };
                if frame.is_stream_error() {
                    break;
                }
                if let Some(sink) = &sink {
                    let sink = sink.clone();
                    let _ = catch_unwind(AssertUnwindSafe(|| sink(frame)));
                }
            }
            if !signal.is_aborted() {
                signal.abort();
            }
            let _ = events.send(PumpEvent::Ended(generation));
        });
    }

    fn generation_active(&self, generation: u64, signal: &AbortSignal) -> bool {
        self.is_running()
            && !signal.is_aborted()
            && self.generation.load(Ordering::Acquire) == generation
    }

    fn emit_state(&self, state: ConnectionState) {
        {
            let mut inner = self.state.lock();
            if inner.last_state == Some(state) {
                return;
            }
            inner.last_state = Some(state);
        }
        Self::call_sink(|| {
            if let Some(sink) = &self.sinks.on_state_change {
                sink(state);
            }
        });
    }

    fn call_sink(callback: impl FnOnce()) {
        let _ = catch_unwind(AssertUnwindSafe(callback));
    }

    fn backoff_delay(&self, attempt: u32) -> Duration {
        let exponent = i32::try_from(attempt.saturating_sub(1)).unwrap_or(i32::MAX);
        let cap = self
            .config
            .backoff_max_ms
            .min(self.config.backoff_base_ms * self.config.backoff_factor.powi(exponent));
        let bytes = *Uuid::new_v4().as_bytes();
        let random_bits = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let random = f64::from(random_bits) / f64::from(u32::MAX);
        duration_ms(cap / 2.0 + random * (cap / 2.0))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StreamKind {
    Mux,
    Host,
}

enum PumpEvent {
    Opened(StreamKind),
    Ended(u64),
}

async fn wait_for_streams(
    events: &mut mpsc::UnboundedReceiver<PumpEvent>,
    timeout: Duration,
    signal: &AbortSignal,
) -> bool {
    let mut mux = false;
    let mut host = false;
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    loop {
        if mux && host {
            return true;
        }
        tokio::select! {
            () = signal.cancelled() => return false,
            () = &mut deadline => return true,
            event = events.recv() => match event {
                Some(PumpEvent::Opened(StreamKind::Mux)) => mux = true,
                Some(PumpEvent::Opened(StreamKind::Host)) => host = true,
                Some(PumpEvent::Ended(_)) | None => return false,
            }
        }
    }
}

async fn wait_for_failure(events: &mut mpsc::UnboundedReceiver<PumpEvent>, signal: &AbortSignal) {
    if signal.is_aborted() {
        return;
    }
    loop {
        tokio::select! {
            () = signal.cancelled() => return,
            event = events.recv() => match event {
                Some(PumpEvent::Ended(generation)) => {
                    let _ = generation;
                    return;
                }
                Some(PumpEvent::Opened(_)) => {}
                None => return,
            }
        }
    }
}

fn duration_ms(milliseconds: f64) -> Duration {
    if !milliseconds.is_finite() || milliseconds <= 0.0 {
        Duration::ZERO
    } else {
        Duration::from_secs_f64(milliseconds / 1000.0)
    }
}
