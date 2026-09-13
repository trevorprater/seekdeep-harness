//! Browser scheduler for Client observable publication.

use std::rc::Rc;

use js_sys::{Function, Promise, Reflect};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::{JsFuture, spawn_local};

use crate::NotifierScheduler;

#[derive(Clone, Copy, Debug, Default)]
struct BrowserNotifierScheduler;

impl NotifierScheduler for BrowserNotifierScheduler {
    fn has_animation_frame(&self) -> bool {
        Reflect::get(
            &js_sys::global(),
            &JsValue::from_str("requestAnimationFrame"),
        )
        .ok()
        .and_then(|value| value.dyn_into::<Function>().ok())
        .is_some()
    }

    fn queue_microtask(&self, callback: Box<dyn FnOnce()>) {
        spawn_local(async move {
            let _ = JsFuture::from(Promise::resolve(&JsValue::UNDEFINED)).await;
            callback();
        });
    }

    fn queue_animation_frame(&self, callback: Box<dyn FnOnce()>) {
        let global = js_sys::global();
        let frame = Reflect::get(&global, &JsValue::from_str("requestAnimationFrame"))
            .ok()
            .and_then(|value| value.dyn_into::<Function>().ok());
        let Some(frame) = frame else {
            self.queue_microtask(callback);
            return;
        };
        let callback = Closure::once_into_js(callback);
        if let Err(error) = frame.call1(&global, &callback) {
            wasm_bindgen::throw_val(error);
        }
    }
}

pub(crate) fn browser_notifier_scheduler() -> Rc<dyn NotifierScheduler> {
    Rc::new(BrowserNotifierScheduler)
}
