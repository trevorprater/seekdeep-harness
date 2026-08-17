//! Timeout arithmetic, first-cause signal fusion, and idle watchdogs.

use std::{error::Error, fmt, sync::Arc, time::Duration};

use futures::{Stream, StreamExt};
use parking_lot::Mutex;
use serde_json::json;
use thiserror::Error;

use crate::abort::AbortSignal;

/// Largest delay Node schedules without clamping it to one millisecond.
pub const MAX_TIMER_DELAY_MS: f64 = 2_147_483_647.0;

/// Identifiable capability-owned timeout reason.
#[derive(Clone, Debug, PartialEq)]
pub struct TimeoutReason {
    /// Capability-owned timeout code.
    pub code: String,
    /// Elapsed deadline in milliseconds.
    pub timeout_ms: f64,
}

impl fmt::Display for TimeoutReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} after {}ms", self.code, self.timeout_ms)
    }
}

impl Error for TimeoutReason {}

/// Invalid caller or timer delay.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{message}")]
pub struct TimeoutValidationError {
    message: String,
}

fn timer_delay(timeout_ms: f64, name: &str) -> Result<Duration, TimeoutValidationError> {
    if !timeout_ms.is_finite() || timeout_ms <= 0.0 || timeout_ms > MAX_TIMER_DELAY_MS {
        return Err(TimeoutValidationError {
            message: format!(
                "{name} must be a positive finite number no greater than {MAX_TIMER_DELAY_MS}"
            ),
        });
    }
    Ok(Duration::from_secs_f64(timeout_ms / 1_000.0))
}

/// Validates an optional timeout hint, applies the backend default, then caps
/// the result with JavaScript `Math.min` semantics.
///
/// # Errors
///
/// Rejects a supplied non-positive or non-finite hint. Defaults and caps are
/// backend-owned and intentionally are not validated here.
pub fn clamp_timeout(
    requested: Option<f64>,
    default: f64,
    maximum: f64,
    name: &str,
) -> Result<f64, TimeoutValidationError> {
    if requested.is_some_and(|value| !value.is_finite() || value <= 0.0) {
        return Err(TimeoutValidationError {
            message: format!("{name} must be a positive finite number"),
        });
    }
    let selected = requested.unwrap_or(default);
    Ok(if selected.is_nan() || maximum.is_nan() {
        f64::NAN
    } else if selected < maximum {
        selected
    } else {
        maximum
    })
}

/// Deadline signal plus its dispose-once timer cancellation.
#[derive(Debug)]
pub struct Deadline {
    /// Signal cancelled by the upstream source or the deadline.
    pub signal: AbortSignal,
    timer: Option<tokio::task::AbortHandle>,
}

impl Deadline {
    /// Clears the armed timer. Calling this more than once is a no-op.
    pub fn dispose(&mut self) {
        if let Some(timer) = self.timer.take() {
            timer.abort();
        }
    }
}

impl Drop for Deadline {
    fn drop(&mut self) {
        self.dispose();
    }
}

/// Fuses optional upstream cancellation with an identifiable deadline.
///
/// A non-positive value is the internal no-timer sentinel and simply forwards
/// the upstream signal, or creates a never-aborting signal when absent.
///
/// # Errors
///
/// Positive delays must be finite and no larger than
/// [`MAX_TIMER_DELAY_MS`].
pub fn deadline(
    upstream: Option<&AbortSignal>,
    timeout_ms: f64,
    code: impl Into<String>,
) -> Result<Deadline, TimeoutValidationError> {
    if timeout_ms <= 0.0 {
        return Ok(Deadline {
            signal: upstream.cloned().unwrap_or_default(),
            timer: None,
        });
    }
    let delay = timer_delay(timeout_ms, "deadline timeoutMs")?;
    let timeout_signal = AbortSignal::default();
    let signal = upstream.map_or_else(
        || timeout_signal.clone(),
        |upstream| AbortSignal::fuse(upstream, &timeout_signal),
    );
    let code = code.into();
    let task_signal = timeout_signal;
    let expires_at = tokio::time::Instant::now() + delay;
    let task = tokio::spawn(async move {
        tokio::time::sleep_until(expires_at).await;
        abort_for_timeout(&task_signal, code, timeout_ms);
    });
    let timer = task.abort_handle();
    drop(task);
    Ok(Deadline {
        signal,
        timer: Some(timer),
    })
}

fn abort_for_timeout(signal: &AbortSignal, code: String, timeout_ms: f64) {
    let reason = Arc::new(TimeoutReason { code, timeout_ms });
    let json = json!({
        "name": "TimeoutReason",
        "message": reason.to_string(),
        "code": reason.code,
        "timeoutMs": reason.timeout_ms,
    });
    signal.abort_with_typed_reason(reason, json);
}

/// Something that may carry a typed timeout reason.
pub trait TimeoutReasonCarrier {
    /// Returns the typed reason, if this carrier has one.
    fn timeout_reason(&self) -> Option<Arc<TimeoutReason>>;
}

impl TimeoutReasonCarrier for AbortSignal {
    fn timeout_reason(&self) -> Option<Arc<TimeoutReason>> {
        self.typed_reason::<TimeoutReason>()
    }
}

impl TimeoutReasonCarrier for Arc<TimeoutReason> {
    fn timeout_reason(&self) -> Option<Arc<TimeoutReason>> {
        Some(self.clone())
    }
}

/// Recovers only a genuinely typed timeout reason, optionally scoped by code.
#[must_use]
pub fn timeout_of(
    carrier: &impl TimeoutReasonCarrier,
    code: Option<&str>,
) -> Option<Arc<TimeoutReason>> {
    let reason = carrier.timeout_reason()?;
    if code.is_none_or(|code| reason.code == code) {
        Some(reason)
    } else {
        None
    }
}

#[derive(Debug, Default)]
struct WatchdogState {
    timer: Option<tokio::task::AbortHandle>,
    outstanding: bool,
    disposed: bool,
}

/// Invalid idle-watchdog operation.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum IdleWatchdogError {
    /// The watchdog was already disposed.
    #[error("idleWatchdog is disposed")]
    Disposed,
    /// Another iterator demand remains outstanding.
    #[error("idleWatchdog next is already outstanding")]
    AlreadyOutstanding,
}

/// Rearmable timeout around one outstanding asynchronous stream demand.
#[derive(Debug)]
pub struct IdleWatchdog {
    /// Stable signal cancelled by upstream or idle timeout.
    pub signal: AbortSignal,
    timeout_signal: AbortSignal,
    delay: Duration,
    timeout_ms: f64,
    code: String,
    state: Arc<Mutex<WatchdogState>>,
}

impl IdleWatchdog {
    /// Creates a stable idle watchdog.
    ///
    /// # Errors
    ///
    /// The interval must be positive, finite, and no larger than
    /// [`MAX_TIMER_DELAY_MS`].
    pub fn new(
        upstream: Option<&AbortSignal>,
        timeout_ms: f64,
        code: impl Into<String>,
    ) -> Result<Self, TimeoutValidationError> {
        let delay = timer_delay(timeout_ms, "idleWatchdog timeoutMs")?;
        let timeout_signal = AbortSignal::default();
        let signal = upstream.map_or_else(
            || timeout_signal.clone(),
            |upstream| AbortSignal::fuse(upstream, &timeout_signal),
        );
        Ok(Self {
            signal,
            timeout_signal,
            delay,
            timeout_ms,
            code: code.into(),
            state: Arc::new(Mutex::new(WatchdogState::default())),
        })
    }

    /// Awaits one stream item while the idle timer is armed.
    ///
    /// The timer is cleared as soon as the stream yields or terminates. It does
    /// not run during consumer think time.
    ///
    /// # Errors
    ///
    /// Rejects demand after disposal or while another demand is outstanding.
    pub async fn next<S>(&self, stream: &mut S) -> Result<Option<S::Item>, IdleWatchdogError>
    where
        S: Stream + Unpin,
    {
        {
            let mut state = self.state.lock();
            if state.disposed {
                return Err(IdleWatchdogError::Disposed);
            }
            if state.outstanding {
                return Err(IdleWatchdogError::AlreadyOutstanding);
            }
            state.outstanding = true;
            self.arm_locked(&mut state);
        }
        let _guard = OutstandingGuard {
            state: self.state.clone(),
        };
        Ok(stream.next().await)
    }

    /// Rearms an outstanding demand after out-of-band transport activity.
    pub fn pulse(&self) {
        let mut state = self.state.lock();
        if state.disposed || !state.outstanding {
            return;
        }
        self.arm_locked(&mut state);
    }

    /// Clears an armed timer and permanently rejects new demand.
    pub fn dispose(&self) {
        let mut state = self.state.lock();
        if state.disposed {
            return;
        }
        state.disposed = true;
        if let Some(timer) = state.timer.take() {
            timer.abort();
        }
    }

    fn arm_locked(&self, state: &mut WatchdogState) {
        if let Some(timer) = state.timer.take() {
            timer.abort();
        }
        let signal = self.timeout_signal.clone();
        let code = self.code.clone();
        let timeout_ms = self.timeout_ms;
        let delay = self.delay;
        let expires_at = tokio::time::Instant::now() + delay;
        let task = tokio::spawn(async move {
            tokio::time::sleep_until(expires_at).await;
            abort_for_timeout(&signal, code, timeout_ms);
        });
        state.timer = Some(task.abort_handle());
        drop(task);
    }
}

struct OutstandingGuard {
    state: Arc<Mutex<WatchdogState>>,
}

impl Drop for OutstandingGuard {
    fn drop(&mut self) {
        let mut state = self.state.lock();
        if let Some(timer) = state.timer.take() {
            timer.abort();
        }
        state.outstanding = false;
    }
}

/// A stream that remains pending until its sender provides an item.
#[cfg(test)]
struct ReceiverStream<T>(tokio::sync::mpsc::Receiver<T>);

#[cfg(test)]
impl<T> Stream for ReceiverStream<T> {
    type Item = T;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.0.poll_recv(context)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp)]

    use serde_json::Value;

    use super::*;

    #[test]
    fn timeout_reason_and_clamping_match_source() {
        let reason = TimeoutReason {
            code: "BASH_TIMEOUT".into(),
            timeout_ms: 100.0,
        };
        assert_eq!(reason.to_string(), "BASH_TIMEOUT after 100ms");
        assert_eq!(
            clamp_timeout(None, 120_000.0, 600_000.0, "timeoutMs").unwrap(),
            120_000.0
        );
        assert_eq!(
            clamp_timeout(Some(999_999.0), 120_000.0, 600_000.0, "timeoutMs").unwrap(),
            600_000.0
        );
        assert_eq!(
            clamp_timeout(None, 900_000.0, 600_000.0, "timeoutMs").unwrap(),
            600_000.0
        );
        assert_eq!(
            clamp_timeout(Some(0.0), 100.0, 200.0, "timeoutMs")
                .unwrap_err()
                .to_string(),
            "timeoutMs must be a positive finite number"
        );
        assert!(clamp_timeout(Some(f64::NAN), 100.0, 200.0, "named").is_err());
        assert!(
            clamp_timeout(None, f64::NAN, 200.0, "timeoutMs")
                .unwrap()
                .is_nan()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_times_out_with_typed_reason_and_disposes() {
        let mut timed = deadline(None, 100.0, "BASH_TIMEOUT").unwrap();
        assert!(!timed.signal.is_aborted());
        tokio::time::advance(Duration::from_millis(100)).await;
        tokio::task::yield_now().await;
        let reason = timeout_of(&timed.signal, None).unwrap();
        assert_eq!(reason.code, "BASH_TIMEOUT");
        assert_eq!(reason.timeout_ms, 100.0);

        let mut disposed = deadline(None, 100.0, "DISPOSED_TIMEOUT").unwrap();
        disposed.dispose();
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(!disposed.signal.is_aborted());
        timed.dispose();
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_preserves_first_cause_and_nested_code_classification() {
        let upstream = AbortSignal::default();
        let mut upstream_first = deadline(Some(&upstream), 100.0, "BASH_TIMEOUT").unwrap();
        upstream.abort_with_reason(json!("user cancelled"));
        tokio::time::advance(Duration::from_millis(200)).await;
        assert!(upstream_first.signal.is_aborted());
        assert!(timeout_of(&upstream_first.signal, None).is_none());
        upstream_first.dispose();

        let later_upstream = AbortSignal::default();
        let mut timeout_first =
            deadline(Some(&later_upstream), 100.0, "WEB_FETCH_TIMEOUT").unwrap();
        tokio::time::advance(Duration::from_millis(100)).await;
        tokio::task::yield_now().await;
        later_upstream.abort_with_reason(json!("too late"));
        assert_eq!(
            timeout_of(&timeout_first.signal, None).unwrap().code,
            "WEB_FETCH_TIMEOUT"
        );
        timeout_first.dispose();

        let outer = AbortSignal::default();
        abort_for_timeout(&outer, "OUTER_TIMEOUT".into(), 30.0);
        let mut inner = deadline(Some(&outer), 60_000.0, "BASH_TIMEOUT").unwrap();
        assert!(timeout_of(&inner.signal, Some("BASH_TIMEOUT")).is_none());
        assert_eq!(
            timeout_of(&inner.signal, None).unwrap().code,
            "OUTER_TIMEOUT"
        );
        inner.dispose();
    }

    #[tokio::test(start_paused = true)]
    async fn nonpositive_deadline_arms_no_timer_and_validates_large_values() {
        let upstream = AbortSignal::default();
        let mut no_timer = deadline(Some(&upstream), -5.0, "NONE").unwrap();
        tokio::time::advance(Duration::from_secs(1_000)).await;
        assert!(!no_timer.signal.is_aborted());
        upstream.abort();
        assert!(no_timer.signal.is_aborted());
        assert!(timeout_of(&no_timer.signal, None).is_none());
        no_timer.dispose();
        assert!(deadline(None, MAX_TIMER_DELAY_MS + 1.0, "TOO_LARGE").is_err());
        assert!(deadline(None, f64::INFINITY, "INFINITE").is_err());
    }

    #[test]
    fn typed_classification_rejects_plain_json_and_filters_code() {
        let signal = AbortSignal::default();
        signal.abort_with_reason(json!({ "name": "TimeoutReason", "code": "SPOOF" }));
        assert!(timeout_of(&signal, None).is_none());
        let reason = Arc::new(TimeoutReason {
            code: "BASH_TIMEOUT".into(),
            timeout_ms: 100.0,
        });
        assert!(Arc::ptr_eq(
            &timeout_of(&reason, Some("BASH_TIMEOUT")).unwrap(),
            &reason
        ));
        assert!(timeout_of(&reason, Some("WEB_FETCH_TIMEOUT")).is_none());
        let _: Value = signal.reason().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn idle_watchdog_arms_only_during_demand_and_reuses_signal() {
        let watchdog = IdleWatchdog::new(None, 100.0, "LLM_STREAM_IDLE_TIMEOUT").unwrap();
        let stable = watchdog.signal.clone();
        tokio::time::advance(Duration::from_secs(10)).await;
        assert!(!stable.is_aborted());

        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        let mut stream = ReceiverStream(receiver);
        let next = watchdog.next(&mut stream);
        tokio::pin!(next);
        assert!(futures::poll!(&mut next).is_pending());
        tokio::time::advance(Duration::from_millis(99)).await;
        assert!(!stable.is_aborted());
        sender.send(1).await.unwrap();
        assert_eq!(next.await.unwrap(), Some(1));
        tokio::time::advance(Duration::from_secs(10)).await;
        assert!(!stable.is_aborted());

        let (_sender, receiver) = tokio::sync::mpsc::channel::<i32>(1);
        let mut stream = ReceiverStream(receiver);
        let next = watchdog.next(&mut stream);
        tokio::pin!(next);
        assert!(futures::poll!(&mut next).is_pending());
        tokio::time::advance(Duration::from_millis(100)).await;
        tokio::task::yield_now().await;
        assert_eq!(
            timeout_of(&stable, Some("LLM_STREAM_IDLE_TIMEOUT"))
                .unwrap()
                .timeout_ms,
            100.0
        );
    }

    #[tokio::test(start_paused = true)]
    async fn pulse_rearms_outstanding_demand_and_disposal_clears_it() {
        let watchdog = IdleWatchdog::new(None, 100.0, "IDLE").unwrap();
        watchdog.pulse();
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(!watchdog.signal.is_aborted());
        let (_sender, receiver) = tokio::sync::mpsc::channel::<i32>(1);
        let mut stream = ReceiverStream(receiver);
        let next = watchdog.next(&mut stream);
        tokio::pin!(next);
        assert!(futures::poll!(&mut next).is_pending());
        tokio::time::advance(Duration::from_millis(99)).await;
        watchdog.pulse();
        tokio::time::advance(Duration::from_millis(99)).await;
        assert!(!watchdog.signal.is_aborted());
        watchdog.dispose();
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(!watchdog.signal.is_aborted());
        watchdog.pulse();
        let (_sender, receiver) = tokio::sync::mpsc::channel::<i32>(1);
        assert_eq!(
            watchdog
                .next(&mut ReceiverStream(receiver))
                .await
                .unwrap_err(),
            IdleWatchdogError::Disposed
        );
    }

    #[tokio::test]
    async fn watchdog_rejects_invalid_and_concurrent_demand() {
        assert!(IdleWatchdog::new(None, 0.0, "IDLE").is_err());
        assert!(IdleWatchdog::new(None, f64::NAN, "IDLE").is_err());
        assert!(IdleWatchdog::new(None, MAX_TIMER_DELAY_MS + 1.0, "IDLE").is_err());
        let watchdog = IdleWatchdog::new(None, 100.0, "IDLE").unwrap();
        let (_first_sender, first_receiver) = tokio::sync::mpsc::channel::<i32>(1);
        let mut first_stream = ReceiverStream(first_receiver);
        let first = watchdog.next(&mut first_stream);
        tokio::pin!(first);
        assert!(futures::poll!(&mut first).is_pending());
        let (_second_sender, second_receiver) = tokio::sync::mpsc::channel::<i32>(1);
        let mut second_stream = ReceiverStream(second_receiver);
        assert_eq!(
            watchdog.next(&mut second_stream).await.unwrap_err(),
            IdleWatchdogError::AlreadyOutstanding
        );
    }
}
