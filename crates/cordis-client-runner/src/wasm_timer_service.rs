//! JavaScript-facing Client Timer Service backed by Rust scheduling state.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use futures::{FutureExt, channel::oneshot};
use js_sys::{Array, Function, Object, Promise, Reflect};
use parking_lot::Mutex;
use seekdeep_cordis_timer::{PreparedTimer, TimerCallback, TimerDriver};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::future_to_promise;

use crate::{WasmTimerDriver, call_method, construct_function, set};

const CONTEXT_DISPOSED: &str = "Context has been disposed";

/// Installs the `timer` Service and its mixed-in Context helpers.
///
/// # Errors
///
/// Returns JavaScript reflection or Cordis registration failures.
pub fn install_wasm_client_timer(ctx: &JsValue) -> Result<JsValue, JsValue> {
    let service = Arc::new(WasmClientTimerService {
        ctx: ctx.clone(),
        driver: Arc::new(WasmTimerDriver),
    });
    let timeout = variadic({
        let service = service.clone();
        move |args| service.timeout(&args)
    })?;
    let interval = variadic({
        let service = service.clone();
        move |args| service.interval(&args)
    })?;
    let throttle = variadic({
        let service = service.clone();
        move |args| service.throttle(&args)
    })?;
    let debounce = variadic({
        let service = service.clone();
        move |args| service.debounce(&args)
    })?;
    let object = Object::new();
    set(&object, "timeout", &timeout)?;
    set(&object, "interval", &interval)?;
    set(&object, "throttle", &throttle)?;
    set(&object, "debounce", &debounce)?;
    set(&object, "setTimeout", &timeout)?;
    set(&object, "setInterval", &interval)?;
    let object: JsValue = object.into();
    call_method(
        ctx,
        "provide",
        &[JsValue::from_str("timer"), object.clone()],
    )?;
    let mixins = Array::new();
    for name in [
        "timeout",
        "interval",
        "throttle",
        "debounce",
        "setTimeout",
        "setInterval",
    ] {
        mixins.push(&JsValue::from_str(name));
    }
    call_method(ctx, "mixin", &[JsValue::from_str("timer"), mixins.into()])?;
    Ok(object)
}

struct WasmClientTimerService {
    ctx: JsValue,
    driver: Arc<WasmTimerDriver>,
}

impl WasmClientTimerService {
    fn timeout(&self, arguments: &Array) -> Result<JsValue, JsValue> {
        if let Ok(callback) = arguments.get(0).dyn_into::<Function>() {
            let delay = timer_duration(&arguments.get(1));
            let holder = Arc::new(Mutex::new(None::<Function>));
            let callback_holder = holder.clone();
            let timer_callback: TimerCallback = Arc::new(move || {
                let callback = callback.clone();
                let disposer = callback_holder.lock().take();
                async move {
                    if let Some(disposer) = disposer {
                        let _ = disposer.call0(&JsValue::UNDEFINED);
                    }
                    if let Err(error) = callback.call0(&JsValue::UNDEFINED) {
                        wasm_bindgen::throw_val(error);
                    }
                }
                .boxed()
            });
            let timer = self.driver.prepare_timeout(delay, timer_callback);
            let disposer = own_timer(&self.ctx, "ctx.timeout()", timer, Arc::new(|| {}))?;
            *holder.lock() = Some(disposer.clone());
            return Ok(disposer.into());
        }

        let delay = timer_duration(&arguments.get(0));
        let (sender, receiver) = oneshot::channel::<Result<(), JsValue>>();
        let sender = Arc::new(Mutex::new(Some(sender)));
        let firing = Arc::new(AtomicBool::new(false));
        let holder = Arc::new(Mutex::new(None::<Function>));
        let timer_callback: TimerCallback = {
            let sender = sender.clone();
            let firing = firing.clone();
            let holder = holder.clone();
            Arc::new(move || {
                firing.store(true, Ordering::Release);
                let disposer = holder.lock().take();
                let sender = sender.clone();
                async move {
                    if let Some(disposer) = disposer {
                        let _ = disposer.call0(&JsValue::UNDEFINED);
                    }
                    if let Some(sender) = sender.lock().take() {
                        let _ = sender.send(Ok(()));
                    }
                }
                .boxed()
            })
        };
        let timer = self.driver.prepare_timeout(delay, timer_callback);
        let cancel = {
            let sender = sender.clone();
            Arc::new(move || {
                if !firing.load(Ordering::Acquire)
                    && let Some(sender) = sender.lock().take()
                {
                    let _ = sender.send(Err(js_sys::Error::new(CONTEXT_DISPOSED).into()));
                }
            }) as Arc<dyn Fn() + Send + Sync>
        };
        let disposer = own_timer(&self.ctx, "ctx.timeout()", timer, cancel)?;
        *holder.lock() = Some(disposer);
        Ok(future_to_promise(async move {
            match receiver.await {
                Ok(Ok(())) => Ok(JsValue::UNDEFINED),
                Ok(Err(error)) => Err(error),
                Err(_) => Err(js_sys::Error::new(CONTEXT_DISPOSED).into()),
            }
        })
        .into())
    }

    fn interval(&self, arguments: &Array) -> Result<JsValue, JsValue> {
        if let Ok(callback) = arguments.get(0).dyn_into::<Function>() {
            let delay = timer_duration(&arguments.get(1));
            let timer_callback: TimerCallback = Arc::new(move || {
                let callback = callback.clone();
                async move {
                    if let Err(error) = callback.call0(&JsValue::UNDEFINED) {
                        wasm_bindgen::throw_val(error);
                    }
                }
                .boxed()
            });
            let timer = self.driver.prepare_interval(delay, timer_callback);
            return own_timer(&self.ctx, "ctx.interval()", timer, Arc::new(|| {})).map(Into::into);
        }
        self.interval_iterator(timer_duration(&arguments.get(0)))
    }

    fn interval_iterator(&self, delay: Duration) -> Result<JsValue, JsValue> {
        let state = Arc::new(IteratorState::default());
        let tick_state = state.clone();
        let callback: TimerCallback = Arc::new(move || {
            let state = tick_state.clone();
            async move {
                if let Some(sender) = state.pending.lock().take() {
                    let _ = sender.send(Ok(IteratorSignal::Tick));
                }
            }
            .boxed()
        });
        let timer = self.driver.prepare_interval(delay, callback);
        let cancel_state = state.clone();
        let cancel = Arc::new(move || {
            let reason: JsValue = js_sys::Error::new(CONTEXT_DISPOSED).into();
            let mut done = cancel_state.done.lock();
            if done.is_none() {
                *done = Some(Err(reason.clone()));
                if let Some(sender) = cancel_state.pending.lock().take() {
                    let _ = sender.send(Err(reason));
                }
            }
        });
        let disposer = own_timer(&self.ctx, "ctx.interval()", timer, cancel)?;
        *state.disposer.lock() = Some(disposer);
        iterator_object(state)
    }

    fn throttle(&self, arguments: &Array) -> Result<JsValue, JsValue> {
        let callback = required_callback(arguments, "throttle")?;
        let delay = numeric_delay(&arguments.get(1));
        let no_trailing = arguments.get(2).as_bool().unwrap_or(false);
        self.scheduled_wrapper(callback, delay, no_trailing, true)
    }

    fn debounce(&self, arguments: &Array) -> Result<JsValue, JsValue> {
        let callback = required_callback(arguments, "debounce")?;
        let delay = numeric_delay(&arguments.get(1));
        self.scheduled_wrapper(callback, delay, false, false)
    }

    fn scheduled_wrapper(
        &self,
        callback: Function,
        delay_ms: f64,
        initially_disposed: bool,
        throttle: bool,
    ) -> Result<JsValue, JsValue> {
        let pending = Arc::new(Mutex::new(None::<Arc<dyn PreparedTimer>>));
        let is_disposed = Arc::new(AtomicBool::new(initially_disposed));
        let last_call = Arc::new(Mutex::new(f64::NEG_INFINITY));
        let cleanup_pending = pending.clone();
        let cleanup_disposed = is_disposed.clone();
        let wrapper_disposer = own_cleanup(
            &self.ctx,
            if throttle {
                "ctx.throttle()"
            } else {
                "ctx.debounce()"
            },
            Arc::new(move || {
                cleanup_disposed.store(true, Ordering::Release);
                if let Some(timer) = cleanup_pending.lock().take() {
                    timer.cancel();
                }
            }),
        )?;
        let driver = self.driver.clone();
        let invoke = Closure::wrap(Box::new(move |args: Array| -> Result<JsValue, JsValue> {
            if let Some(timer) = pending.lock().take() {
                timer.cancel();
            }
            if throttle {
                let now = js_sys::Date::now();
                let remaining = delay_ms - now + *last_call.lock();
                if remaining <= 0.0 {
                    *last_call.lock() = now;
                    Reflect::apply(&callback, &JsValue::UNDEFINED, &args)?;
                } else if !is_disposed.load(Ordering::Acquire) {
                    let callback = callback.clone();
                    let args = args.clone();
                    let last_call = last_call.clone();
                    let pending_after = pending.clone();
                    let timer_callback: TimerCallback = Arc::new(move || {
                        let callback = callback.clone();
                        let args = args.clone();
                        let last_call = last_call.clone();
                        let pending = pending_after.clone();
                        async move {
                            pending.lock().take();
                            *last_call.lock() = js_sys::Date::now();
                            if let Err(error) =
                                Reflect::apply(&callback, &JsValue::UNDEFINED, &args)
                            {
                                wasm_bindgen::throw_val(error);
                            }
                        }
                        .boxed()
                    });
                    let timer =
                        driver.prepare_timeout(duration_from_millis(remaining), timer_callback);
                    *pending.lock() = Some(timer.clone());
                    timer.start();
                }
            } else if !is_disposed.load(Ordering::Acquire) {
                let callback = callback.clone();
                let args = args.clone();
                let pending_after = pending.clone();
                let timer_callback: TimerCallback = Arc::new(move || {
                    let callback = callback.clone();
                    let args = args.clone();
                    let pending = pending_after.clone();
                    async move {
                        pending.lock().take();
                        if let Err(error) = Reflect::apply(&callback, &JsValue::UNDEFINED, &args) {
                            wasm_bindgen::throw_val(error);
                        }
                    }
                    .boxed()
                });
                let timer = driver.prepare_timeout(duration_from_millis(delay_ms), timer_callback);
                *pending.lock() = Some(timer.clone());
                timer.start();
            }
            Ok(JsValue::UNDEFINED)
        })
            as Box<dyn FnMut(Array) -> Result<JsValue, JsValue>>);
        let wrapper = construct_function(&["invoke"], "return (...args) => invoke(args);")?
            .call1(&JsValue::UNDEFINED, &invoke.into_js_value())?
            .dyn_into::<Function>()?;
        Reflect::set(&wrapper, &JsValue::from_str("dispose"), &wrapper_disposer)?;
        Ok(wrapper.into())
    }
}

type IteratorSender = oneshot::Sender<Result<IteratorSignal, JsValue>>;

#[derive(Debug)]
enum IteratorSignal {
    Tick,
    Returned(JsValue),
}

#[derive(Default)]
struct IteratorState {
    done: Mutex<Option<Result<JsValue, JsValue>>>,
    pending: Mutex<Option<IteratorSender>>,
    disposer: Mutex<Option<Function>>,
}

fn iterator_object(state: Arc<IteratorState>) -> Result<JsValue, JsValue> {
    let next_state = state.clone();
    let next = Closure::wrap(Box::new(move || -> Promise {
        if let Some(done) = next_state.done.lock().clone() {
            return match done {
                Ok(value) => Promise::resolve(&iterator_result(true, &value)),
                Err(reason) => Promise::reject(&reason),
            };
        }
        let (sender, receiver) = oneshot::channel();
        *next_state.pending.lock() = Some(sender);
        future_to_promise(async move {
            match receiver.await {
                Ok(Ok(IteratorSignal::Tick)) => Ok(iterator_result(false, &JsValue::UNDEFINED)),
                Ok(Ok(IteratorSignal::Returned(value))) => Ok(iterator_result(true, &value)),
                Ok(Err(reason)) => Err(reason),
                Err(_) => Err(js_sys::Error::new(CONTEXT_DISPOSED).into()),
            }
        })
    }) as Box<dyn FnMut() -> Promise>);
    let return_state = state.clone();
    let return_value = Closure::wrap(Box::new(move |value: JsValue| -> Promise {
        {
            let mut done = return_state.done.lock();
            if done.is_none() {
                *done = Some(Ok(value.clone()));
            }
        }
        if let Some(sender) = return_state.pending.lock().take() {
            let _ = sender.send(Ok(IteratorSignal::Returned(value.clone())));
        }
        dispose_iterator(&return_state);
        Promise::resolve(&iterator_result(true, &value))
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    let throw_state = state;
    let throw_value = Closure::wrap(Box::new(move |reason: JsValue| -> Promise {
        {
            let mut done = throw_state.done.lock();
            if done.is_none() {
                *done = Some(Err(reason.clone()));
            }
        }
        if let Some(sender) = throw_state.pending.lock().take() {
            let _ = sender.send(Err(reason));
        }
        dispose_iterator(&throw_state);
        Promise::resolve(&iterator_result(true, &JsValue::UNDEFINED))
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    let factory = construct_function(
        &["next", "returnValue", "throwValue"],
        r"
return {
  next,
  return: returnValue,
  throw: throwValue,
  [Symbol.asyncIterator]() { return this; },
};
",
    )?;
    factory.call3(
        &JsValue::UNDEFINED,
        &next.into_js_value(),
        &return_value.into_js_value(),
        &throw_value.into_js_value(),
    )
}

fn dispose_iterator(state: &IteratorState) {
    if let Some(disposer) = state.disposer.lock().take() {
        let _ = disposer.call0(&JsValue::UNDEFINED);
    }
}

fn iterator_result(done: bool, value: &JsValue) -> JsValue {
    let result = Object::new();
    let _ = set(&result, "done", &JsValue::from_bool(done));
    let _ = set(&result, "value", value);
    result.into()
}

fn own_timer(
    ctx: &JsValue,
    label: &str,
    timer: Arc<dyn PreparedTimer>,
    on_cancel: Arc<dyn Fn() + Send + Sync>,
) -> Result<Function, JsValue> {
    let start = timer.clone();
    let installer = Closure::once_into_js(move || -> JsValue {
        start.start();
        let cancel = timer;
        let disposer = Closure::wrap(Box::new(move || {
            cancel.cancel();
            on_cancel();
        }) as Box<dyn FnMut()>);
        disposer.into_js_value()
    });
    call_method(ctx, "effect", &[installer, JsValue::from_str(label)])?.dyn_into::<Function>()
}

fn own_cleanup(
    ctx: &JsValue,
    label: &str,
    cleanup: Arc<dyn Fn() + Send + Sync>,
) -> Result<Function, JsValue> {
    let installer = Closure::once_into_js(move || -> JsValue {
        let disposer = Closure::wrap(Box::new(move || cleanup()) as Box<dyn FnMut()>);
        disposer.into_js_value()
    });
    call_method(ctx, "effect", &[installer, JsValue::from_str(label)])?.dyn_into::<Function>()
}

fn variadic(
    callback: impl Fn(Array) -> Result<JsValue, JsValue> + 'static,
) -> Result<Function, JsValue> {
    let callback =
        Closure::wrap(Box::new(callback) as Box<dyn FnMut(Array) -> Result<JsValue, JsValue>>);
    construct_function(&["invoke"], "return (...args) => invoke(args);")?
        .call1(&JsValue::UNDEFINED, &callback.into_js_value())?
        .dyn_into::<Function>()
}

fn required_callback(arguments: &Array, method: &str) -> Result<Function, JsValue> {
    arguments.get(0).dyn_into::<Function>().map_err(|_| {
        js_sys::TypeError::new(&format!("ctx.{method}() needs a callback function")).into()
    })
}

fn numeric_delay(value: &JsValue) -> f64 {
    value.as_f64().unwrap_or(0.0)
}

fn timer_duration(value: &JsValue) -> Duration {
    duration_from_millis(numeric_delay(value))
}

fn duration_from_millis(milliseconds: f64) -> Duration {
    if !milliseconds.is_finite() || milliseconds <= 0.0 {
        Duration::ZERO
    } else {
        Duration::from_secs_f64((milliseconds.min(f64::from(i32::MAX))) / 1_000.0)
    }
}
