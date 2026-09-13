//! Browser command-list transport and abort-signal adapter.

use std::{cell::RefCell, rc::Rc};

use futures::{FutureExt as _, future::LocalBoxFuture};
use js_sys::{Function, Promise, Reflect};
use seekdeep_commands_contract::CommandDescriptor;
use seekdeep_identity::SessionId;
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, spawn_local};

use super::{call_method, js_error_text, object, required, required_string};
use crate::{
    CommandDirectory, CommandDirectoryAbort, CommandDirectorySpawner, CommandDirectoryStatus,
    CommandDirectoryTransport,
};

pub(crate) struct BrowserDirectoryTransport {
    commands: JsValue,
    sessions: JsValue,
}

struct BrowserFetchTransport {
    fetch_commands: Function,
}

impl CommandDirectoryTransport for BrowserFetchTransport {
    fn fetch(
        &self,
        session_id: SessionId,
    ) -> LocalBoxFuture<'static, Result<Vec<CommandDescriptor>, String>> {
        let fetch_commands = self.fetch_commands.clone();
        async move {
            let returned = fetch_commands
                .call1(&JsValue::UNDEFINED, &JsValue::from_str(session_id.as_str()))
                .map_err(|error| js_error_text(&error))?;
            let commands = JsFuture::from(Promise::resolve(&returned))
                .await
                .map_err(|error| js_error_text(&error))?;
            serde_wasm_bindgen::from_value(commands).map_err(|error| error.to_string())
        }
        .boxed_local()
    }
}

impl BrowserDirectoryTransport {
    pub(crate) fn for_runtime(commands: JsValue, sessions: JsValue) -> Rc<Self> {
        Rc::new(Self { commands, sessions })
    }
}

impl CommandDirectoryTransport for BrowserDirectoryTransport {
    fn fetch(
        &self,
        session_id: SessionId,
    ) -> LocalBoxFuture<'static, Result<Vec<CommandDescriptor>, String>> {
        let commands = self.commands.clone();
        let sessions = self.sessions.clone();
        async move {
            let address = call_method(
                &sessions,
                "subagentAddress",
                &[JsValue::from_str(session_id.as_str())],
            )
            .map_err(|error| js_error_text(&error))?;
            if !address.is_undefined() {
                return Ok(Vec::new());
            }
            let returned =
                call_method(&commands, "list", &[JsValue::from_str(session_id.as_str())])
                    .map_err(|error| js_error_text(&error))?;
            let result = JsFuture::from(Promise::resolve(&returned))
                .await
                .map_err(|error| js_error_text(&error))?;
            let ok = required(&result, "ok", "command.list result")
                .map_err(|error| js_error_text(&error))?
                .as_bool()
                .unwrap_or(false);
            if !ok {
                let error = required(&result, "error", "command.list result")
                    .map_err(|error| js_error_text(&error))?;
                let code = required_string(&error, "code", "command.list error")
                    .map_err(|error| js_error_text(&error))?;
                let message = required_string(&error, "message", "command.list error")
                    .map_err(|error| js_error_text(&error))?;
                return Err(format!("command.list failed: {code}: {message}"));
            }
            serde_wasm_bindgen::from_value(
                required(&result, "value", "command.list result")
                    .map_err(|error| js_error_text(&error))?,
            )
            .map_err(|error| error.to_string())
        }
        .boxed_local()
    }
}

pub(crate) struct BrowserSpawner;

impl CommandDirectorySpawner for BrowserSpawner {
    fn spawn(&self, task: LocalBoxFuture<'static, ()>) {
        spawn_local(task);
    }
}

pub(crate) struct BrowserDirectoryAbort {
    signal: JsValue,
}

impl BrowserDirectoryAbort {
    pub(crate) fn new(signal: JsValue) -> Rc<Self> {
        Rc::new(Self { signal })
    }
}

impl CommandDirectoryAbort for BrowserDirectoryAbort {
    fn aborted(&self) -> bool {
        Reflect::get(&self.signal, &JsValue::from_str("aborted"))
            .ok()
            .and_then(|value| value.as_bool())
            == Some(true)
    }

    fn reason(&self) -> String {
        let reason =
            Reflect::get(&self.signal, &JsValue::from_str("reason")).unwrap_or(JsValue::UNDEFINED);
        if reason.is_instance_of::<js_sys::Error>() {
            Reflect::get(&reason, &JsValue::from_str("message"))
                .ok()
                .and_then(|value| value.as_string())
                .unwrap_or_else(|| "command directory wait aborted".to_owned())
        } else {
            "command directory wait aborted".to_owned()
        }
    }

    fn cancelled(&self) -> LocalBoxFuture<'static, ()> {
        let signal = self.signal.clone();
        async move {
            if Reflect::get(&signal, &JsValue::from_str("aborted"))
                .ok()
                .and_then(|value| value.as_bool())
                == Some(true)
            {
                return;
            }
            let (sender, receiver) = futures::channel::oneshot::channel();
            let sender = Rc::new(RefCell::new(Some(sender)));
            let listener_sender = sender;
            let listener = Closure::wrap(Box::new(move || {
                if let Some(sender) = listener_sender.borrow_mut().take() {
                    let _ = sender.send(());
                }
            }) as Box<dyn FnMut()>)
            .into_js_value();
            let _ = call_method(
                &signal,
                "addEventListener",
                &[
                    JsValue::from_str("abort"),
                    listener.clone(),
                    object(&[("once", JsValue::TRUE)])
                        .map(Into::into)
                        .unwrap_or(JsValue::UNDEFINED),
                ],
            );
            let _ = receiver.await;
            let _ = call_method(
                &signal,
                "removeEventListener",
                &[JsValue::from_str("abort"), listener],
            );
        }
        .boxed_local()
    }
}

/// Compiled command directory class.
#[wasm_bindgen(js_name = __CommandDirectory)]
pub struct WasmCommandDirectory {
    pub(crate) inner: Rc<CommandDirectory>,
}

#[wasm_bindgen(js_class = __CommandDirectory)]
impl WasmCommandDirectory {
    /// Creates a directory over an injected source-compatible fetch callback.
    #[wasm_bindgen(constructor)]
    pub fn new(fetch_commands: Function) -> Self {
        Self {
            inner: CommandDirectory::new(
                Rc::new(BrowserFetchTransport { fetch_commands }),
                Rc::new(BrowserSpawner),
            ),
        }
    }

    /// Returns one Session status.
    pub fn status(&self, session_id: String) -> String {
        status_name(self.inner.status(&SessionId::new(session_id))).to_owned()
    }

    /// Resolves one exact hot descriptor.
    ///
    /// # Errors
    ///
    /// Returns JavaScript serialization failures.
    #[allow(clippy::needless_pass_by_value)]
    pub fn resolve(&self, session_id: String, name: String) -> Result<JsValue, JsValue> {
        self.inner
            .resolve(&SessionId::new(session_id), &name)
            .map(|value| {
                serde_wasm_bindgen::to_value(&value)
                    .map_err(|error| js_sys::Error::new(&error.to_string()).into())
            })
            .transpose()
            .map(|value| value.unwrap_or(JsValue::UNDEFINED))
    }

    /// Soft-refreshes touched keys.
    #[wasm_bindgen(js_name = invalidateAll)]
    pub fn invalidate_all(&self) {
        self.inner.invalidate_all();
    }

    /// Hard-resets touched keys.
    #[wasm_bindgen(js_name = resetConnected)]
    pub fn reset_connected(&self) {
        self.inner.reset_connected();
    }

    /// Prewarms one Session.
    pub fn warm(&self, session_id: String) {
        self.inner.warm(SessionId::new(session_id));
    }

    /// Refreshes one Session.
    pub fn refresh(&self, session_id: String) -> Promise {
        let future = self.inner.refresh(SessionId::new(session_id));
        wasm_bindgen_futures::future_to_promise(async move {
            future.await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Strong-waits for one ready Session catalog.
    ///
    /// # Errors
    ///
    /// Returns serialization failures.
    #[wasm_bindgen(js_name = ensureReady)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn ensure_ready(&self, session_id: String, signal: JsValue) -> Promise {
        let future = self.inner.ensure_ready(
            SessionId::new(session_id),
            BrowserDirectoryAbort::new(signal),
        );
        wasm_bindgen_futures::future_to_promise(async move {
            let commands = future
                .await
                .map_err(|message| js_sys::Error::new(&message))?;
            serde_wasm_bindgen::to_value(&commands)
                .map_err(|error| js_sys::Error::new(&error.to_string()).into())
        })
    }
}

fn status_name(status: CommandDirectoryStatus) -> &'static str {
    match status {
        CommandDirectoryStatus::Cold => "cold",
        CommandDirectoryStatus::Pending => "pending",
        CommandDirectoryStatus::Ready => "ready",
        CommandDirectoryStatus::Failed => "failed",
    }
}
