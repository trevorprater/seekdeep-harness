//! Executor and timer seam for the transport paths that build on both targets.
//!
//! Natively the fixture transport and the generation controller run on tokio;
//! in the browser the same code runs on the page event loop, where a task is
//! `spawn_local` and a timer is `setTimeout`. Only these three primitives differ.

#[cfg(not(target_arch = "wasm32"))]
use std::{future::Future, time::Duration};

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn spawn(future: impl Future<Output = ()> + Send + 'static) {
    drop(tokio::spawn(future));
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn sleep(duration: Duration) -> impl Future<Output = ()> + Send {
    tokio::time::sleep(duration)
}

/// Uniform jitter in `[0, 1)` for reconnect backoff (source: `Math.random()`).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn random_unit() -> f64 {
    let bytes = *uuid::Uuid::new_v4().as_bytes();
    let random_bits = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    // Divide by 2^32, not `u32::MAX`, so the sample never reaches 1.0.
    f64::from(random_bits) / 4_294_967_296.0
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    #[test]
    fn the_jitter_sample_stays_below_one() {
        for _ in 0..10_000 {
            let sample = super::random_unit();
            assert!((0.0..1.0).contains(&sample), "{sample}");
        }
        assert!(f64::from(u32::MAX) / 4_294_967_296.0 < 1.0);
    }
}

#[cfg(target_arch = "wasm32")]
pub(crate) use browser::{random_unit, sleep, spawn};

#[cfg(target_arch = "wasm32")]
mod browser {
    use std::{
        future::Future,
        pin::Pin,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        task::{Context, Poll, Waker},
        time::Duration,
    };

    use js_sys::{Function, Reflect};
    use parking_lot::Mutex;
    use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};

    pub(crate) fn spawn(future: impl Future<Output = ()> + Send + 'static) {
        wasm_bindgen_futures::spawn_local(future);
    }

    pub(crate) fn sleep(duration: Duration) -> Sleep {
        Sleep {
            millis: duration.as_secs_f64() * 1000.0,
            shared: Arc::new(Shared {
                fired: AtomicBool::new(false),
                waker: Mutex::new(None),
            }),
            armed: false,
        }
    }

    pub(crate) fn random_unit() -> f64 {
        js_sys::Math::random()
    }

    struct Shared {
        fired: AtomicBool,
        waker: Mutex<Option<Waker>>,
    }

    /// A `setTimeout`-backed timer whose handle stays `Send`: the JavaScript callback lives
    /// only inside the browser's timer table and wakes the task through shared atomics.
    pub(crate) struct Sleep {
        millis: f64,
        shared: Arc<Shared>,
        armed: bool,
    }

    impl Future for Sleep {
        type Output = ();

        fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
            if self.shared.fired.load(Ordering::Acquire) {
                return Poll::Ready(());
            }
            *self.shared.waker.lock() = Some(context.waker().clone());
            if !self.armed {
                self.armed = true;
                let shared = self.shared.clone();
                let callback = Closure::once_into_js(move || {
                    shared.fired.store(true, Ordering::Release);
                    if let Some(waker) = shared.waker.lock().take() {
                        waker.wake();
                    }
                });
                let global = js_sys::global();
                let set_timeout = Reflect::get(&global, &JsValue::from_str("setTimeout"))
                    .ok()
                    .and_then(|value| value.dyn_into::<Function>().ok());
                if let Some(set_timeout) = set_timeout {
                    let _ = set_timeout.call2(&global, &callback, &JsValue::from_f64(self.millis));
                } else {
                    // Without a timer table the timer fires immediately (a test bench
                    // without `setTimeout` still makes progress).
                    self.shared.fired.store(true, Ordering::Release);
                    return Poll::Ready(());
                }
            }
            Poll::Pending
        }
    }
}
