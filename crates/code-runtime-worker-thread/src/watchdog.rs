//! Joined watchdog for worker active-time, wall-time, and cancellation limits.

use std::{
    sync::Arc,
    thread::JoinHandle,
    time::{Duration, Instant},
};

use parking_lot::{Condvar, Mutex};
use seekdeep_llm::AbortSignal;
use serde_json::Value;

use crate::engine::{EngineCompletion, EngineLimits};

const POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Default)]
struct State {
    finished: bool,
    terminal: Option<EngineCompletion>,
    active_since: Option<Instant>,
    active_time: Duration,
}

/// Shared interruption state; the first terminal event wins.
#[derive(Default)]
pub(crate) struct Control {
    state: Mutex<State>,
    changed: Condvar,
    pub(crate) stopped: tokio::sync::Notify,
}

impl Control {
    pub(crate) fn stop(&self, completion: EngineCompletion) {
        let mut state = self.state.lock();
        if state.terminal.is_none() && !state.finished {
            state.terminal = Some(completion);
            self.stopped.notify_one();
            self.changed.notify_one();
        }
    }

    pub(crate) fn take_completion(&self) -> Option<EngineCompletion> {
        let mut state = self.state.lock();
        let completion = state.terminal.take()?;
        state.finished = true;
        self.changed.notify_one();
        Some(completion)
    }

    pub(crate) fn enter(&self) {
        let mut state = self.state.lock();
        assert!(
            state.active_since.is_none(),
            "worker activity is not nested"
        );
        state.active_since = Some(Instant::now());
    }

    pub(crate) fn leave(&self) {
        let mut state = self.state.lock();
        if let Some(start) = state.active_since.take() {
            state.active_time += start.elapsed();
        }
    }

    fn finish(&self) {
        self.state.lock().finished = true;
        self.changed.notify_one();
    }
}

/// Owns the interrupting thread until the isolate has stopped executing.
pub(crate) struct Watchdog {
    pub(crate) control: Arc<Control>,
    thread: Option<JoinHandle<()>>,
}

impl Watchdog {
    pub(crate) fn new(
        control: Arc<Control>,
        isolate: v8::IsolateHandle,
        limits: &EngineLimits,
        started: Instant,
    ) -> anyhow::Result<Self> {
        let monitoring = control.clone();
        let signal = limits.signal.clone();
        let compute_ms = limits.compute_ms;
        let wall = Duration::from_secs_f64(limits.max_wall_ms / 1_000.0);
        let thread = std::thread::Builder::new()
            .name("seekdeep-code-runtime-watchdog".to_owned())
            .spawn(move || monitor(&monitoring, &isolate, &signal, compute_ms, started, wall))?;
        Ok(Self {
            control,
            thread: Some(thread),
        })
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.control.finish();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn monitor(
    control: &Control,
    isolate: &v8::IsolateHandle,
    signal: &AbortSignal,
    compute_ms: f64,
    started: Instant,
    wall: Duration,
) {
    let mut state = control.state.lock();
    loop {
        if state.finished {
            return;
        }
        if state.terminal.is_none() {
            let active = state.active_time
                + state
                    .active_since
                    .map_or(Duration::ZERO, |start| start.elapsed());
            state.terminal = if signal.is_aborted() {
                Some(EngineCompletion::Abort(
                    signal.reason().unwrap_or(Value::Null),
                ))
            } else if started.elapsed() >= wall {
                Some(EngineCompletion::WallTimeout)
            } else if active.as_secs_f64() * 1_000.0 > compute_ms {
                Some(EngineCompletion::ComputeTimeout)
            } else {
                None
            };
        }
        if state.terminal.is_some() {
            isolate.terminate_execution();
            control.stopped.notify_one();
            return;
        }
        let remaining = wall.saturating_sub(started.elapsed());
        control
            .changed
            .wait_for(&mut state, POLL_INTERVAL.min(remaining));
    }
}
