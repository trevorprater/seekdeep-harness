//! Browser `setTimeout`/`setInterval` driver for the shared Cordis Timer Service.

use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI32, Ordering},
    },
    time::Duration,
};

use seekdeep_cordis_timer::{PreparedTimer, TimerCallback, TimerDriver};
use wasm_bindgen::{JsCast, closure::Closure};

thread_local! {
    static TIMER_CLOSURES: RefCell<BTreeMap<u64, Closure<dyn FnMut()>>> = const { RefCell::new(BTreeMap::new()) };
    static NEXT_TIMER_ID: Cell<u64> = const { Cell::new(0) };
}

/// Browser timer driver with deterministic process-local handle identities.
#[derive(Debug, Default)]
pub struct WasmTimerDriver;

impl TimerDriver for WasmTimerDriver {
    fn now(&self) -> Duration {
        Duration::from_secs_f64(js_sys::Date::now() / 1_000.0)
    }

    fn prepare_timeout(&self, delay: Duration, callback: TimerCallback) -> Arc<dyn PreparedTimer> {
        WasmPreparedTimer::new(Self::next_id(), delay, callback, false)
    }

    fn prepare_interval(&self, delay: Duration, callback: TimerCallback) -> Arc<dyn PreparedTimer> {
        WasmPreparedTimer::new(Self::next_id(), delay, callback, true)
    }
}

impl WasmTimerDriver {
    fn next_id() -> u64 {
        NEXT_TIMER_ID.with(|next| {
            let id = next
                .get()
                .checked_add(1)
                .expect("browser timer id exhausted");
            next.set(id);
            id
        })
    }
}

struct WasmPreparedTimer {
    id: u64,
    delay_ms: i32,
    callback: TimerCallback,
    repeat: bool,
    started: AtomicBool,
    cancelled: Arc<AtomicBool>,
    browser_handle: Arc<AtomicI32>,
}

impl WasmPreparedTimer {
    fn new(id: u64, delay: Duration, callback: TimerCallback, repeat: bool) -> Arc<Self> {
        let delay_ms = i32::try_from(delay.as_millis()).unwrap_or(i32::MAX);
        Arc::new(Self {
            id,
            delay_ms,
            callback,
            repeat,
            started: AtomicBool::new(false),
            cancelled: Arc::new(AtomicBool::new(false)),
            browser_handle: Arc::new(AtomicI32::new(0)),
        })
    }
}

impl PreparedTimer for WasmPreparedTimer {
    fn start(&self) {
        if self.started.swap(true, Ordering::AcqRel) {
            return;
        }
        let id = self.id;
        let callback = self.callback.clone();
        let cancelled = self.cancelled.clone();
        let repeat = self.repeat;
        let closure = Closure::wrap(Box::new(move || {
            if cancelled.load(Ordering::Acquire) {
                return;
            }
            wasm_bindgen_futures::spawn_local(callback());
            if !repeat {
                TIMER_CLOSURES.with(|closures| {
                    closures.borrow_mut().remove(&id);
                });
            }
        }) as Box<dyn FnMut()>);
        let window = web_sys::window().expect("WasmTimerDriver requires Window");
        let handle = if self.repeat {
            window
                .set_interval_with_callback_and_timeout_and_arguments_0(
                    closure.as_ref().unchecked_ref(),
                    self.delay_ms,
                )
                .expect("browser rejected setInterval")
        } else {
            window
                .set_timeout_with_callback_and_timeout_and_arguments_0(
                    closure.as_ref().unchecked_ref(),
                    self.delay_ms,
                )
                .expect("browser rejected setTimeout")
        };
        self.browser_handle.store(handle, Ordering::Release);
        TIMER_CLOSURES.with(|closures| {
            closures.borrow_mut().insert(id, closure);
        });
    }

    fn cancel(&self) {
        if self.cancelled.swap(true, Ordering::AcqRel) {
            return;
        }
        let handle = self.browser_handle.swap(0, Ordering::AcqRel);
        if handle != 0
            && let Some(window) = web_sys::window()
        {
            if self.repeat {
                window.clear_interval_with_handle(handle);
            } else {
                window.clear_timeout_with_handle(handle);
            }
        }
        TIMER_CLOSURES.with(|closures| {
            closures.borrow_mut().remove(&self.id);
        });
    }
}
