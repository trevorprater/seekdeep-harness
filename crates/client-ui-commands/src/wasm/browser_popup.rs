//! JavaScript popup business-spec, `AbortController`, deps, and Store adapter.

use std::{any::Any, cell::RefCell, rc::Rc};

use futures::{FutureExt as _, future::LocalBoxFuture};
use js_sys::{Array, Function, Object, Promise, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, future_to_promise, spawn_local};

use super::{call_method, js_error_text, object, required, required_function, set, to_js};
use crate::{
    PopupAbortFactory, PopupAbortHandle, PopupBusinessSpec, PopupContext, PopupSelectController,
    PopupSelectDeps, PopupState, PopupStatus, PopupTaskSpawner, PopupTokenSegment, SelectOption,
};

struct BrowserPopupAbort {
    controller: JsValue,
    signal: JsValue,
}

impl PopupAbortHandle for BrowserPopupAbort {
    fn abort(&self) {
        let _ = call_method(&self.controller, "abort", &[]);
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub(crate) struct BrowserAbortFactory;

impl PopupAbortFactory for BrowserAbortFactory {
    fn create(&self) -> Rc<dyn PopupAbortHandle> {
        let controller = construct("AbortController").expect("AbortController must exist");
        let signal = required(&controller, "signal", "AbortController")
            .expect("AbortController must expose signal");
        Rc::new(BrowserPopupAbort { controller, signal })
    }
}

struct BrowserPopupSpec {
    value: JsValue,
}

impl PopupBusinessSpec for BrowserPopupSpec {
    fn options(
        &self,
        context: Rc<dyn PopupContext>,
        signal: Rc<dyn PopupAbortHandle>,
    ) -> LocalBoxFuture<'static, Result<Vec<SelectOption>, String>> {
        let value = self.value.clone();
        async move {
            let context = context
                .as_ref()
                .as_any()
                .downcast_ref::<JsValue>()
                .ok_or_else(|| "popup context is not a JavaScript value".to_owned())?
                .clone();
            let signal = signal
                .as_ref()
                .as_any()
                .downcast_ref::<BrowserPopupAbort>()
                .ok_or_else(|| "popup signal is not a browser AbortController".to_owned())?
                .signal
                .clone();
            let returned = required_function(&value, "options", "popupSelect spec")
                .map_err(|error| js_error_text(&error))?
                .call2(&value, &context, &signal)
                .map_err(|error| js_error_text(&error))?;
            let rows = JsFuture::from(Promise::resolve(&returned))
                .await
                .map_err(|error| js_error_text(&error))?;
            serde_wasm_bindgen::from_value(rows).map_err(|error| error.to_string())
        }
        .boxed_local()
    }

    fn on_select(
        &self,
        option: SelectOption,
        context: Rc<dyn PopupContext>,
    ) -> LocalBoxFuture<'static, Result<(), String>> {
        let value = self.value.clone();
        async move {
            let option =
                serde_wasm_bindgen::to_value(&option).map_err(|error| error.to_string())?;
            let context = context
                .as_ref()
                .as_any()
                .downcast_ref::<JsValue>()
                .ok_or_else(|| "popup context is not a JavaScript value".to_owned())?
                .clone();
            let returned = required_function(&value, "onSelect", "popupSelect spec")
                .map_err(|error| js_error_text(&error))?
                .call2(&value, &option, &context)
                .map_err(|error| js_error_text(&error))?;
            JsFuture::from(Promise::resolve(&returned))
                .await
                .map_err(|error| js_error_text(&error))?;
            Ok(())
        }
        .boxed_local()
    }
}

struct BrowserPopupDeps {
    consume: Function,
    focus: Function,
}

impl PopupSelectDeps for BrowserPopupDeps {
    fn consume(&self, segment: &PopupTokenSegment) -> bool {
        let Ok(segment) = to_js(segment) else {
            return false;
        };
        self.consume
            .call1(&JsValue::UNDEFINED, &segment)
            .ok()
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
    }

    fn focus_composer(&self) {
        let _ = self.focus.call0(&JsValue::UNDEFINED);
    }
}

pub(crate) struct BrowserPopupSpawner;

impl PopupTaskSpawner for BrowserPopupSpawner {
    fn spawn(&self, task: LocalBoxFuture<'static, ()>) {
        spawn_local(task);
    }
}

type SnapshotCache = Rc<RefCell<Option<(Rc<PopupState>, JsValue)>>>;

/// Compiled popup-select controller.
#[wasm_bindgen(js_name = __PopupSelectController)]
pub struct WasmPopupSelectController {
    pub(crate) inner: Rc<PopupSelectController>,
    pub(crate) state_face: JsValue,
}

#[wasm_bindgen(js_class = __PopupSelectController)]
impl WasmPopupSelectController {
    /// Creates one closed controller from consume/focus callbacks.
    ///
    /// # Errors
    ///
    /// Returns malformed callback diagnostics.
    #[wasm_bindgen(constructor)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn new(deps: JsValue) -> Result<Self, JsValue> {
        let inner = PopupSelectController::new(
            Rc::new(BrowserPopupDeps {
                consume: required_function(&deps, "consume", "popup deps")?,
                focus: required_function(&deps, "focusComposer", "popup deps")?,
            }),
            Rc::new(BrowserAbortFactory),
            Rc::new(BrowserPopupSpawner),
        );
        Self::from_inner(inner)
    }

    /// Observable popup state Store.
    #[wasm_bindgen(getter)]
    pub fn state(&self) -> JsValue {
        self.state_face.clone()
    }

    /// Opens one command binding.
    ///
    /// # Errors
    ///
    /// Returns malformed context or token-segment diagnostics.
    #[allow(clippy::needless_pass_by_value)]
    pub fn open(
        &self,
        command: String,
        spec: JsValue,
        context: JsValue,
        segment: JsValue,
    ) -> Result<(), JsValue> {
        let segment = serde_wasm_bindgen::from_value(segment)
            .map_err(|error| js_sys::Error::new(&error.to_string()))?;
        self.inner.open(
            command,
            Rc::new(BrowserPopupSpec { value: spec }),
            context,
            segment,
        );
        Ok(())
    }

    /// Retries a failed load.
    pub fn retry(&self) {
        self.inner.retry();
    }

    /// Replaces local search.
    #[wasm_bindgen(js_name = setSearch)]
    pub fn set_search(&self, search: String) {
        self.inner.set_search(search);
    }

    /// Moves filtered highlight.
    #[wasm_bindgen(js_name = move)]
    pub fn move_highlight(&self, direction: i8) {
        self.inner.move_highlight(direction);
    }

    /// Sets direct highlight.
    pub fn highlight(&self, index: usize) {
        self.inner.highlight(index);
    }

    /// Selects one filtered row.
    pub fn select(&self, index: usize) -> Promise {
        operation_promise(self.inner.select(index))
    }

    /// Updates risk acknowledgement.
    pub fn acknowledge(&self, acknowledged: bool) {
        self.inner.acknowledge(acknowledged);
    }

    /// Cancels the risk gate.
    #[wasm_bindgen(js_name = cancelConfirmation)]
    pub fn cancel_confirmation(&self) {
        self.inner.cancel_confirmation();
    }

    /// Confirms one acknowledged gated row.
    pub fn confirm(&self) -> Promise {
        operation_promise(self.inner.confirm())
    }

    /// Dismisses and optionally restores focus.
    #[allow(clippy::needless_pass_by_value)]
    pub fn dismiss(&self, options: Option<JsValue>) {
        let focus = options
            .as_ref()
            .and_then(|options| Reflect::get(options, &JsValue::from_str("focusComposer")).ok())
            .and_then(|value| value.as_bool())
            == Some(true);
        self.inner.dismiss(focus);
    }

    /// Tears down without focus.
    pub fn dispose(&self) {
        self.inner.dispose();
    }
}

impl WasmPopupSelectController {
    pub(crate) fn from_inner(inner: Rc<PopupSelectController>) -> Result<Self, JsValue> {
        let state_face = state_face(&inner)?;
        Ok(Self { inner, state_face })
    }
}

fn state_face(controller: &Rc<PopupSelectController>) -> Result<JsValue, JsValue> {
    let face = Object::new();
    let cache: SnapshotCache = Rc::new(RefCell::new(None));
    let get_controller = controller.clone();
    let get_cache = cache;
    let get = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let snapshot = get_controller.snapshot();
        if let Some((cached, value)) = get_cache.borrow().as_ref()
            && Rc::ptr_eq(cached, &snapshot)
        {
            return Ok(value.clone());
        }
        let value = popup_state_to_js(&snapshot)?;
        *get_cache.borrow_mut() = Some((snapshot, value.clone()));
        Ok(value)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    set(&face, "getSnapshot", &get.into_js_value())?;
    let subscribe_controller = controller.clone();
    let subscribe = Closure::wrap(Box::new(move |listener: Function| -> JsValue {
        let subscription = subscribe_controller.subscribe(Rc::new(move || {
            let _ = listener.call0(&JsValue::UNDEFINED);
        }));
        let subscription = Rc::new(RefCell::new(Some(subscription)));
        Closure::wrap(Box::new(move || {
            subscription.borrow_mut().take();
        }) as Box<dyn FnMut()>)
        .into_js_value()
    }) as Box<dyn FnMut(Function) -> JsValue>);
    set(&face, "subscribe", &subscribe.into_js_value())?;
    Ok(face.into())
}

fn popup_state_to_js(state: &PopupState) -> Result<JsValue, JsValue> {
    object(&[
        ("open", JsValue::from_bool(state.open)),
        (
            "command",
            state
                .command
                .as_ref()
                .map_or(JsValue::NULL, |value| JsValue::from_str(value)),
        ),
        (
            "status",
            JsValue::from_str(match state.status {
                PopupStatus::Pending => "pending",
                PopupStatus::Ready => "ready",
                PopupStatus::Failed => "failed",
            }),
        ),
        (
            "options",
            serde_wasm_bindgen::to_value(&state.options)
                .map_err(|error| js_sys::Error::new(&error.to_string()))?,
        ),
        ("search", JsValue::from_str(&state.search)),
        ("active", JsValue::from_f64(usize_as_f64(state.active))),
        ("submitting", JsValue::from_bool(state.submitting)),
        (
            "confirming",
            state
                .confirming
                .as_ref()
                .map(serde_wasm_bindgen::to_value)
                .transpose()
                .map_err(|error| js_sys::Error::new(&error.to_string()))?
                .unwrap_or(JsValue::NULL),
        ),
        ("acknowledged", JsValue::from_bool(state.acknowledged)),
        (
            "error",
            state
                .error
                .as_ref()
                .map_or(JsValue::NULL, |value| JsValue::from_str(value)),
        ),
    ])
    .map(Into::into)
}

fn operation_promise(future: LocalBoxFuture<'static, ()>) -> Promise {
    future_to_promise(async move {
        future.await;
        Ok(JsValue::UNDEFINED)
    })
}

fn construct(name: &str) -> Result<JsValue, JsValue> {
    let constructor =
        Reflect::get(&js_sys::global(), &JsValue::from_str(name))?.dyn_into::<Function>()?;
    Reflect::construct(&constructor, &Array::new())
}

fn usize_as_f64(value: usize) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
        value as f64
    }
}
