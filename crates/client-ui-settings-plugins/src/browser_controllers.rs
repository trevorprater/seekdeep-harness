//! Browser Settings-scope, credential, and observable adapters.

use std::{cell::RefCell, rc::Rc};

use futures::{FutureExt as _, future::LocalBoxFuture};
use js_sys::{Function, JSON, Object, Reflect};
use seekdeep_client_runtime::{SnapshotStore, SnapshotStoreSubscription};
use seekdeep_client_settings_contract::{
    ClientSettingsDisposer, ClientSettingsMode, ClientSettingsScope, ClientSettingsScopeSnapshot,
    ClientSettingsStatus,
};
use serde::Serialize;
use serde_json::Value;
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::spawn_local;

use crate::{
    AgentLoopCardController, BashCardController, CardCredentialsTransport, CardForm,
    CardTaskSpawner, CredentialView, WebSearchCardController,
    browser::{call_async, call_method, object, optional, rejection_text, required, set, to_js},
};

type ScopeCache = Rc<RefCell<Option<(JsValue, Rc<ClientSettingsScopeSnapshot<Value>>)>>>;

pub(crate) struct BrowserSettingsScope {
    scope: JsValue,
    cache: ScopeCache,
}

impl BrowserSettingsScope {
    pub(crate) fn new(scope: JsValue) -> Rc<Self> {
        Rc::new(Self {
            scope,
            cache: Rc::new(RefCell::new(None)),
        })
    }

    fn parse_snapshot(&self) -> Result<Rc<ClientSettingsScopeSnapshot<Value>>, String> {
        let current = call_method(&self.scope, "getSnapshot", &[]).map_err(|error| {
            format!(
                "plugin card settings snapshot failed: {}",
                rejection_text(&error)
            )
        })?;
        if let Some((cached, snapshot)) = self.cache.borrow().as_ref()
            && Object::is(cached, &current)
        {
            return Ok(snapshot.clone());
        }
        let status = match required(&current, "status", "settings scope snapshot")
            .map_err(|error| rejection_text(&error))?
            .as_string()
            .as_deref()
        {
            Some("loading") => ClientSettingsStatus::Loading,
            Some("ready") => ClientSettingsStatus::Ready,
            Some("unavailable") => ClientSettingsStatus::Unavailable,
            Some(value) => return Err(format!("unknown settings scope status {value:?}")),
            None => return Err("settings scope status must be a string".to_owned()),
        };
        let snapshot = Rc::new(ClientSettingsScopeSnapshot {
            status,
            value: json_field(&current, "value")?.map(Rc::new),
            base: json_field(&current, "base")?,
            user: json_field(&current, "user")?,
            revision: optional(&current, "revision")
                .map_err(|error| rejection_text(&error))?
                .and_then(|value| value.as_f64()),
            writable: required(&current, "writable", "settings scope snapshot")
                .map_err(|error| rejection_text(&error))?
                .as_bool()
                .unwrap_or(false),
            mode: match optional(&current, "mode")
                .map_err(|error| rejection_text(&error))?
                .and_then(|value| value.as_string())
                .as_deref()
            {
                Some("memory") => ClientSettingsMode::Memory,
                _ => ClientSettingsMode::Host,
            },
        });
        *self.cache.borrow_mut() = Some((current, snapshot.clone()));
        Ok(snapshot)
    }
}

impl ClientSettingsScope<Value> for BrowserSettingsScope {
    fn snapshot(&self) -> Rc<ClientSettingsScopeSnapshot<Value>> {
        self.parse_snapshot().unwrap_or_else(|_| {
            Rc::new(ClientSettingsScopeSnapshot {
                status: ClientSettingsStatus::Unavailable,
                value: None,
                base: None,
                user: None,
                revision: None,
                writable: false,
                mode: ClientSettingsMode::Host,
            })
        })
    }

    fn subscribe(&self, listener: Rc<dyn Fn()>) -> ClientSettingsDisposer {
        let listener = Closure::wrap(Box::new(move || listener()) as Box<dyn FnMut()>);
        let disposer = call_method(&self.scope, "subscribe", &[listener.into_js_value()])
            .ok()
            .and_then(|value| value.dyn_into::<Function>().ok());
        ClientSettingsDisposer::new(move || {
            if let Some(disposer) = disposer {
                let _ = disposer.call0(&JsValue::UNDEFINED);
            }
        })
    }

    fn set(&self, field: String, value: Value) -> LocalBoxFuture<'static, Result<(), String>> {
        let scope = self.scope.clone();
        async move {
            let value = to_js(&value).map_err(|error| rejection_text(&error))?;
            call_async(&scope, "set", &[JsValue::from_str(&field), value])
                .await
                .map(|_| ())
                .map_err(|error| rejection_text(&error))
        }
        .boxed_local()
    }

    fn unset(&self, field: String) -> LocalBoxFuture<'static, Result<(), String>> {
        let scope = self.scope.clone();
        async move {
            call_async(&scope, "unset", &[JsValue::from_str(&field)])
                .await
                .map(|_| ())
                .map_err(|error| rejection_text(&error))
        }
        .boxed_local()
    }
}

fn json_field(value: &JsValue, key: &str) -> Result<Option<Value>, String> {
    let field =
        Reflect::get(value, &JsValue::from_str(key)).map_err(|error| rejection_text(&error))?;
    if field.is_undefined() {
        return Ok(None);
    }
    let encoded = JSON::stringify(&field)
        .map_err(|error| rejection_text(&error))?
        .as_string()
        .ok_or_else(|| format!("settings scope {key} is not JSON-compatible"))?;
    serde_json::from_str(&encoded)
        .map(Some)
        .map_err(|error| error.to_string())
}

pub(crate) struct BrowserCredentialsTransport {
    credentials: JsValue,
}

impl BrowserCredentialsTransport {
    pub(crate) fn new(api: &JsValue) -> Result<Rc<Self>, JsValue> {
        Ok(Rc::new(Self {
            credentials: required(api, "credentials", "generated API")?,
        }))
    }
}

impl CardCredentialsTransport for BrowserCredentialsTransport {
    fn describe(
        &self,
        reference: String,
    ) -> LocalBoxFuture<'static, Result<Option<CredentialView>, String>> {
        let credentials = self.credentials.clone();
        async move {
            let refs = js_sys::Array::of1(&JsValue::from_str(&reference));
            let response = call_async(
                &credentials,
                "describe",
                &[object(&[("refs", refs.into())])
                    .map_err(|error| rejection_text(&error))?
                    .into()],
            )
            .await
            .map_err(|error| rejection_text(&error))?;
            let value = rpc_value(&response)?;
            let views = required(&value, "credentials", "credentials.describe value")
                .map_err(|error| rejection_text(&error))?;
            let view = Reflect::get(&views, &JsValue::from_str(&reference))
                .map_err(|error| rejection_text(&error))?;
            if view.is_null() || view.is_undefined() {
                return Ok(None);
            }
            Ok(Some(CredentialView {
                configured: required(&view, "configured", "credential view")
                    .map_err(|error| rejection_text(&error))?
                    .as_bool()
                    .unwrap_or(false),
                writable: required(&view, "writable", "credential view")
                    .map_err(|error| rejection_text(&error))?
                    .as_bool()
                    .unwrap_or(true),
            }))
        }
        .boxed_local()
    }

    fn set(&self, reference: String, value: String) -> LocalBoxFuture<'static, Result<(), String>> {
        let credentials = self.credentials.clone();
        async move {
            call_async(
                &credentials,
                "set",
                &[object(&[
                    ("ref", JsValue::from_str(&reference)),
                    ("value", JsValue::from_str(&value)),
                ])
                .map_err(|error| rejection_text(&error))?
                .into()],
            )
            .await
            .map(|_| ())
            .map_err(|error| rejection_text(&error))
        }
        .boxed_local()
    }
}

struct BrowserTaskSpawner;

impl CardTaskSpawner for BrowserTaskSpawner {
    fn spawn(&self, task: LocalBoxFuture<'static, ()>) {
        spawn_local(task);
    }
}

pub(crate) struct BrowserCardControllers {
    pub(crate) bash: Rc<BashCardController>,
    pub(crate) agent_loop: Rc<AgentLoopCardController>,
    pub(crate) web_search: Rc<WebSearchCardController>,
}

impl BrowserCardControllers {
    pub(crate) fn new(settings_scope: &JsValue, api: &JsValue) -> Result<Self, JsValue> {
        let bash_scope = bind_scope(settings_scope, crate::SHELL_NS)?;
        let agent_scope = bind_scope(settings_scope, crate::AGENT_LOOP_NS)?;
        let web_scope = bind_scope(settings_scope, crate::WEB_SEARCH_NS)?;
        let credentials = BrowserCredentialsTransport::new(api)?;
        Ok(Self {
            bash: BashCardController::new(bash_scope),
            agent_loop: AgentLoopCardController::new(agent_scope),
            web_search: WebSearchCardController::new(
                web_scope,
                credentials,
                Rc::new(BrowserTaskSpawner),
            ),
        })
    }

    pub(crate) fn bash_face(&self) -> Result<JsValue, JsValue> {
        controller_face(
            self.bash.clone(),
            self.bash.form(),
            self.bash.store(),
            "bashCard",
        )
    }

    pub(crate) fn agent_loop_face(&self) -> Result<JsValue, JsValue> {
        controller_face(
            self.agent_loop.clone(),
            self.agent_loop.form(),
            self.agent_loop.store(),
            "agentLoopCard",
        )
    }

    pub(crate) fn web_search_face(&self) -> Result<JsValue, JsValue> {
        controller_face(
            self.web_search.clone(),
            self.web_search.form(),
            self.web_search.store(),
            "webSearchCard",
        )
    }
}

fn bind_scope(
    settings_scope: &JsValue,
    namespace: &str,
) -> Result<Rc<dyn ClientSettingsScope<Value>>, JsValue> {
    let scope = call_method(
        settings_scope,
        "bind",
        &[object(&[("namespace", JsValue::from_str(namespace))])?.into()],
    )?;
    Ok(BrowserSettingsScope::new(scope))
}

fn controller_face<T: Clone + Serialize + 'static, O: 'static>(
    owner: Rc<O>,
    form: Rc<CardForm>,
    store: Rc<SnapshotStore<T>>,
    hook_name: &str,
) -> Result<JsValue, JsValue> {
    let output = Object::new();
    let hooks: JsValue = object(&[(hook_name, snapshot_face(store)?)])?.into();
    set(&output, "hooks", &hooks)?;
    let edit_form = form.clone();
    let edit_owner = owner;
    let edit = Closure::wrap(Box::new(move |field: String, text: String| {
        let _ = &edit_owner;
        edit_form.edit(field, text);
    }) as Box<dyn FnMut(String, String)>);
    set(&output, "edit", &edit.into_js_value())?;
    let reset_form = form.clone();
    let reset = Closure::wrap(Box::new(move |field: String| -> Result<(), JsValue> {
        reset_form
            .try_reset_field(&field)
            .map_err(|error| js_sys::Error::new(&error).into())
    }) as Box<dyn FnMut(String) -> Result<(), JsValue>>);
    set(&output, "resetField", &reset.into_js_value())?;
    let save_form = form.clone();
    let save = Closure::wrap(Box::new(move || {
        let form = save_form.clone();
        spawn_local(async move {
            let _ = form.save().await;
        });
    }) as Box<dyn FnMut()>);
    set(&output, "save", &save.into_js_value())?;
    let discard = Closure::wrap(Box::new(move || form.discard()) as Box<dyn FnMut()>);
    set(&output, "discard", &discard.into_js_value())?;
    Ok(output.into())
}

fn snapshot_face<T: Clone + Serialize + 'static>(
    store: Rc<SnapshotStore<T>>,
) -> Result<JsValue, JsValue> {
    let output = Object::new();
    let cache = Rc::new(RefCell::new(None::<(Rc<T>, JsValue)>));
    let getter_store = store.clone();
    let getter_cache = cache;
    let getter = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let snapshot = getter_store.snapshot();
        if let Some((cached, value)) = getter_cache.borrow().as_ref()
            && Rc::ptr_eq(cached, &snapshot)
        {
            return Ok(value.clone());
        }
        let value = to_js(snapshot.as_ref())?;
        *getter_cache.borrow_mut() = Some((snapshot, value.clone()));
        Ok(value)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    set(&output, "getSnapshot", &getter.into_js_value())?;
    let subscriber_store = store;
    let subscribe = Closure::wrap(Box::new(move |listener: Function| -> Function {
        let callback = listener.clone();
        let subscription = subscriber_store.subscribe(Rc::new(move || {
            let _ = callback.call0(&JsValue::UNDEFINED);
        }));
        subscription_disposer(subscription)
    }) as Box<dyn FnMut(Function) -> Function>);
    set(&output, "subscribe", &subscribe.into_js_value())?;
    Ok(output.into())
}

fn subscription_disposer<T: 'static>(subscription: SnapshotStoreSubscription<T>) -> Function {
    Closure::wrap(Box::new(move || subscription.dispose()) as Box<dyn FnMut()>)
        .into_js_value()
        .unchecked_into()
}

fn rpc_value(response: &JsValue) -> Result<JsValue, String> {
    let result =
        required(response, "result", "RPC response").map_err(|error| rejection_text(&error))?;
    if required(&result, "ok", "RPC result")
        .map_err(|error| rejection_text(&error))?
        .as_bool()
        == Some(true)
    {
        required(&result, "value", "RPC result").map_err(|error| rejection_text(&error))
    } else {
        let error =
            required(&result, "error", "RPC result").map_err(|error| rejection_text(&error))?;
        Err(required(&error, "message", "RPC error")
            .map_err(|error| rejection_text(&error))?
            .as_string()
            .unwrap_or_default())
    }
}

/// Creates a compiled Bash controller face for live WASM tests and compatibility adapters.
///
/// # Errors
///
/// Returns malformed Settings-scope or JavaScript face failures.
#[wasm_bindgen(js_name = createPluginsBashController)]
#[allow(clippy::needless_pass_by_value)]
pub fn create_plugins_bash_controller(scope: JsValue) -> Result<JsValue, JsValue> {
    let controller = BashCardController::new(BrowserSettingsScope::new(scope));
    controller_face(
        controller.clone(),
        controller.form(),
        controller.store(),
        "bashCard",
    )
}

/// Creates a compiled Agent-loop controller face.
///
/// # Errors
///
/// Returns malformed Settings-scope or JavaScript face failures.
#[wasm_bindgen(js_name = createPluginsAgentLoopController)]
#[allow(clippy::needless_pass_by_value)]
pub fn create_plugins_agent_loop_controller(scope: JsValue) -> Result<JsValue, JsValue> {
    let controller = AgentLoopCardController::new(BrowserSettingsScope::new(scope));
    controller_face(
        controller.clone(),
        controller.form(),
        controller.store(),
        "agentLoopCard",
    )
}

/// Creates a compiled Web-search controller face.
///
/// # Errors
///
/// Returns malformed Settings-scope, credential API, or JavaScript face failures.
#[wasm_bindgen(js_name = createPluginsWebSearchController)]
#[allow(clippy::needless_pass_by_value)]
pub fn create_plugins_web_search_controller(
    scope: JsValue,
    api: JsValue,
) -> Result<JsValue, JsValue> {
    let controller = WebSearchCardController::new(
        BrowserSettingsScope::new(scope),
        BrowserCredentialsTransport::new(&api)?,
        Rc::new(BrowserTaskSpawner),
    );
    controller_face(
        controller.clone(),
        controller.form(),
        controller.store(),
        "webSearchCard",
    )
}
