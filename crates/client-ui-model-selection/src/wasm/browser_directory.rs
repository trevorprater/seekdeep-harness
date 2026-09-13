//! JavaScript Session API and observable Store adapter for `ModelDirectory`.

use std::{cell::RefCell, rc::Rc};

use futures::{FutureExt as _, future::LocalBoxFuture};
use js_sys::{Function, Object, Promise};
use seekdeep_identity::SessionId;
use wasm_bindgen::{JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, future_to_promise, spawn_local};

use super::{call_method, js_error_string, object, required, required_bool, required_string, set};
use crate::{
    ModelDirectory, ModelDirectoryFailure, ModelDirectoryState, ModelDirectoryStatus,
    ModelDirectoryTransport, ModelSelection, SessionModels,
};

pub(crate) struct BrowserTransport {
    sessions: JsValue,
}

impl BrowserTransport {
    pub(crate) fn new(sessions: JsValue) -> Rc<Self> {
        Rc::new(Self { sessions })
    }
}

impl ModelDirectoryTransport for BrowserTransport {
    fn models(
        &self,
        session_id: SessionId,
    ) -> LocalBoxFuture<'static, Result<SessionModels, ModelDirectoryFailure>> {
        let sessions = self.sessions.clone();
        async move {
            let payload = object(&[("sessionId", JsValue::from_str(session_id.as_str()))])
                .map_err(js_failure)?;
            let response = await_method(&sessions, "models", &[payload.into()])
                .await
                .map_err(|message| ModelDirectoryFailure {
                    code: "internal".to_owned(),
                    message,
                })?;
            let value = result_value(&response)?;
            serde_wasm_bindgen::from_value(value).map_err(|error| ModelDirectoryFailure {
                code: "internal".to_owned(),
                message: error.to_string(),
            })
        }
        .boxed_local()
    }

    fn select_model(
        &self,
        session_id: SessionId,
        selection: ModelSelection,
    ) -> LocalBoxFuture<'static, Result<ModelSelection, ModelDirectoryFailure>> {
        let sessions = self.sessions.clone();
        async move {
            let payload = object(&[
                ("sessionId", JsValue::from_str(session_id.as_str())),
                ("provider", JsValue::from_str(selection.provider.as_str())),
                ("model", JsValue::from_str(selection.model.as_str())),
                (
                    "reasoningEffort",
                    selection
                        .reasoning_effort
                        .as_ref()
                        .map_or(JsValue::UNDEFINED, |effort| {
                            JsValue::from_str(effort.as_str())
                        }),
                ),
            ])
            .map_err(js_failure)?;
            let response = await_method(&sessions, "selectModel", &[payload.into()])
                .await
                .map_err(|message| ModelDirectoryFailure {
                    code: "internal".to_owned(),
                    message,
                })?;
            let value = result_value(&response)?;
            serde_wasm_bindgen::from_value(
                required(&value, "selected", "session.selectModel response").map_err(js_failure)?,
            )
            .map_err(|error| ModelDirectoryFailure {
                code: "internal".to_owned(),
                message: error.to_string(),
            })
        }
        .boxed_local()
    }
}

type SnapshotCache = Rc<RefCell<Option<(Rc<ModelDirectoryState>, JsValue)>>>;

/// Compiled per-session model directory.
#[wasm_bindgen(js_name = __ModelDirectory)]
pub struct WasmModelDirectory {
    pub(crate) directory: Rc<ModelDirectory>,
    pub(crate) store_face: JsValue,
}

#[wasm_bindgen(js_class = __ModelDirectory)]
impl WasmModelDirectory {
    /// Creates an idle directory over the generated Session API.
    ///
    /// # Errors
    ///
    /// Returns a malformed availability callback.
    #[wasm_bindgen(constructor)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn new(
        sessions: JsValue,
        session_id: String,
        available: Function,
    ) -> Result<WasmModelDirectory, JsValue> {
        let available = Rc::new(move || {
            available
                .call0(&JsValue::UNDEFINED)
                .ok()
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
        });
        let directory = ModelDirectory::new(
            BrowserTransport::new(sessions),
            SessionId::new(session_id),
            available,
        );
        Self::from_directory(directory)
    }

    /// uSES-safe observable Store face.
    #[wasm_bindgen(getter)]
    pub fn store(&self) -> JsValue {
        self.store_face.clone()
    }

    /// Loads and returns the advisory directory.
    pub fn load(&self) -> Promise {
        let future = self.directory.load();
        future_to_promise(async move {
            let models = future
                .await
                .map_err(|message| js_sys::Error::new(&message))?;
            serde_wasm_bindgen::to_value(&models)
                .map_err(|error| js_sys::Error::new(&error.to_string()).into())
        })
    }

    /// Selects one complete route.
    ///
    /// # Errors
    ///
    /// Returns malformed selection input.
    #[allow(clippy::needless_pass_by_value)]
    pub fn select(&self, selection: JsValue) -> Result<Promise, JsValue> {
        let selection = serde_wasm_bindgen::from_value(selection)
            .map_err(|error| js_sys::Error::new(&error.to_string()))?;
        Ok(select_promise(self.directory.select(selection)))
    }

    /// Clears process-local state and asynchronously restores the Host selection.
    #[wasm_bindgen(js_name = resetConnected)]
    pub fn reset_connected(&self) {
        if let Some(future) = self.directory.reset_connected() {
            spawn_local(async move {
                let _ = future.await;
            });
        }
    }

    /// Suppresses every later settlement.
    pub fn dispose(&self) {
        self.directory.dispose();
    }
}

impl WasmModelDirectory {
    pub(crate) fn from_directory(directory: Rc<ModelDirectory>) -> Result<Self, JsValue> {
        let store_face = store_face(&directory)?;
        Ok(Self {
            directory,
            store_face,
        })
    }
}

pub(crate) fn store_face(directory: &Rc<ModelDirectory>) -> Result<JsValue, JsValue> {
    let face = Object::new();
    let cache: SnapshotCache = Rc::new(RefCell::new(None));
    let get_directory = directory.clone();
    let get_cache = cache;
    let get_snapshot = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let snapshot = get_directory.snapshot();
        if let Some((cached, value)) = get_cache.borrow().as_ref()
            && Rc::ptr_eq(cached, &snapshot)
        {
            return Ok(value.clone());
        }
        let value = state_to_js(&snapshot)?;
        *get_cache.borrow_mut() = Some((snapshot, value.clone()));
        Ok(value)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    set(&face, "getSnapshot", &get_snapshot.into_js_value())?;
    let subscribe_directory = directory.clone();
    let subscribe = Closure::wrap(Box::new(move |listener: Function| -> JsValue {
        let subscription = subscribe_directory.subscribe(Rc::new(move || {
            let _ = listener.call0(&JsValue::UNDEFINED);
        }));
        let subscription = Rc::new(RefCell::new(Some(subscription)));
        Closure::wrap(Box::new(move || {
            if let Some(mut subscription) = subscription.borrow_mut().take() {
                subscription.dispose();
            }
        }) as Box<dyn FnMut()>)
        .into_js_value()
    }) as Box<dyn FnMut(Function) -> JsValue>);
    set(&face, "subscribe", &subscribe.into_js_value())?;
    Ok(face.into())
}

fn state_to_js(state: &ModelDirectoryState) -> Result<JsValue, JsValue> {
    let current = state
        .current
        .as_ref()
        .map(serde_wasm_bindgen::to_value)
        .transpose()
        .map_err(|error| js_sys::Error::new(&error.to_string()))?
        .unwrap_or(JsValue::NULL);
    let groups = serde_wasm_bindgen::to_value(&state.groups)
        .map_err(|error| js_sys::Error::new(&error.to_string()))?;
    let failures = serde_wasm_bindgen::to_value(&state.failures)
        .map_err(|error| js_sys::Error::new(&error.to_string()))?;
    object(&[
        ("current", current),
        (
            "routable",
            state.routable.map_or(JsValue::NULL, JsValue::from_bool),
        ),
        ("groups", groups),
        ("failures", failures),
        (
            "status",
            JsValue::from_str(match state.status {
                ModelDirectoryStatus::Idle => "idle",
                ModelDirectoryStatus::Loading => "loading",
                ModelDirectoryStatus::Ready => "ready",
                ModelDirectoryStatus::Selecting => "selecting",
                ModelDirectoryStatus::Error => "error",
            }),
        ),
        (
            "error",
            state
                .error
                .as_ref()
                .map_or(JsValue::NULL, |error| JsValue::from_str(error)),
        ),
    ])
    .map(Into::into)
}

fn select_promise(future: LocalBoxFuture<'static, Result<(), String>>) -> Promise {
    future_to_promise(async move {
        future
            .await
            .map_err(|message| js_sys::Error::new(&message))?;
        Ok(JsValue::UNDEFINED)
    })
}

async fn await_method(
    value: &JsValue,
    name: &str,
    arguments: &[JsValue],
) -> Result<JsValue, String> {
    let returned = call_method(value, name, arguments).map_err(|error| js_error_string(&error))?;
    JsFuture::from(Promise::resolve(&returned))
        .await
        .map_err(|error| js_error_string(&error))
}

fn result_value(response: &JsValue) -> Result<JsValue, ModelDirectoryFailure> {
    let result = required(response, "result", "Session model response").map_err(js_failure)?;
    if required_bool(&result, "ok", "Session model result").map_err(js_failure)? {
        return required(&result, "value", "Session model result").map_err(js_failure);
    }
    let error = required(&result, "error", "Session model result").map_err(js_failure)?;
    Err(ModelDirectoryFailure {
        code: required_string(&error, "code", "Session model error").map_err(js_failure)?,
        message: required_string(&error, "message", "Session model error").map_err(js_failure)?,
    })
}

#[allow(clippy::needless_pass_by_value)]
fn js_failure(error: JsValue) -> ModelDirectoryFailure {
    ModelDirectoryFailure {
        code: "internal".to_owned(),
        message: js_error_string(&error),
    }
}
