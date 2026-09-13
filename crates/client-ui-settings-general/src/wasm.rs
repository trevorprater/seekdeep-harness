//! Browser WASM facade, Cordis assembly, and React settings-shell components.

use std::{cell::RefCell, rc::Rc};

use futures::{FutureExt, future::LocalBoxFuture};
use js_sys::{Array, Function, Object, Promise, Reflect};
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, future_to_promise, spawn_local};

use crate::{
    SETTINGS_EN, SETTINGS_LOCALE_NAMESPACE, SETTINGS_ZH, SettingsDocumentCall,
    SettingsDocumentDescription, SettingsDocumentState, SettingsDocumentStatus,
    SettingsDocumentStore, SettingsDocumentSubscription, SettingsDocumentTaskSpawner,
    SettingsDocumentTransport, SettingsLedgerProjection, SettingsOnboardingEntry,
    SettingsOnboardingStep, SettingsSectionEntry, SettingsSectionRow, refresh_document_if_loaded,
};

const INJECT: &[&str] = &["slots", "locale", "connection"];
const STYLES: &str = include_str!("../data/styles.css");

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    primitives: JsValue,
    web_react: JsValue,
}

struct BrowserSpawner;

impl SettingsDocumentTaskSpawner for BrowserSpawner {
    fn spawn(&self, task: LocalBoxFuture<'static, ()>) {
        spawn_local(task);
    }
}

struct BrowserDocumentTransport {
    settings: JsValue,
}

impl SettingsDocumentTransport for BrowserDocumentTransport {
    fn describe(
        &self,
    ) -> LocalBoxFuture<'static, SettingsDocumentCall<SettingsDocumentDescription>> {
        let settings = self.settings.clone();
        async move {
            let response = match await_method(&settings, "describe", &[Object::new().into()]).await
            {
                Ok(response) => response,
                Err(error) => return SettingsDocumentCall::Failed(error),
            };
            parse_describe_response(&response)
        }
        .boxed_local()
    }

    fn open_document(&self) -> LocalBoxFuture<'static, SettingsDocumentCall<()>> {
        let settings = self.settings.clone();
        async move {
            let response =
                match await_method(&settings, "openDocument", &[Object::new().into()]).await {
                    Ok(response) => response,
                    Err(error) => return SettingsDocumentCall::Failed(error),
                };
            parse_empty_response(&response)
        }
        .boxed_local()
    }
}

type DocumentSnapshotCache = Rc<RefCell<Option<(Rc<SettingsDocumentState>, JsValue)>>>;

/// Compiled implementation of the source `SettingsDocumentStore` class.
#[wasm_bindgen(js_name = __SettingsDocumentStore)]
pub struct WasmSettingsDocumentStore {
    controller: Rc<SettingsDocumentStore>,
    store_face: JsValue,
}

#[wasm_bindgen(js_class = __SettingsDocumentStore)]
impl WasmSettingsDocumentStore {
    /// Creates an idle controller over a generated settings API face.
    ///
    /// # Errors
    ///
    /// Returns a malformed generated API face.
    #[wasm_bindgen(constructor)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn new(api: JsValue) -> Result<WasmSettingsDocumentStore, JsValue> {
        Self::from_api(&api)
    }

    /// uSES-safe bare observable Store face.
    #[wasm_bindgen(getter)]
    pub fn store(&self) -> JsValue {
        self.store_face.clone()
    }

    /// Loads local-document availability.
    #[wasm_bindgen]
    pub fn load(&self) -> Promise {
        document_operation_promise(self.controller.load())
    }

    /// Requests the Host-owned native document handoff.
    #[wasm_bindgen]
    pub fn open(&self) -> Promise {
        document_operation_promise(self.controller.open())
    }
}

impl WasmSettingsDocumentStore {
    fn from_api(api: &JsValue) -> Result<Self, JsValue> {
        let settings = required_property(api, "settings", "generated API")?;
        let controller = SettingsDocumentStore::new(Rc::new(BrowserDocumentTransport { settings }));
        let store_face = document_store_face(&controller)?;
        Ok(Self {
            controller,
            store_face,
        })
    }
}

/// Configures shell-owned JavaScript modules at Client factory materialization.
///
/// # Errors
///
/// Returns DOM style-injection failures.
#[wasm_bindgen(js_name = configureClientUiSettingsGeneral)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_settings_general(
    react: JsValue,
    primitives: JsValue,
    web_react: JsValue,
) -> Result<(), JsValue> {
    MODULES.with(|modules| {
        *modules.borrow_mut() = Some(BrowserModules {
            react,
            primitives,
            web_react,
        });
    });
    inject_styles()
}

/// Browser Client plugin apply function.
///
/// # Errors
///
/// Returns missing-service, locale, Slot, document-state, React, or DOM failures.
#[wasm_bindgen(js_name = applyClientUiSettingsGeneral)]
#[allow(clippy::needless_pass_by_value)]
#[allow(clippy::too_many_lines)]
pub fn apply_client_ui_settings_general(ctx: JsValue) -> Result<(), JsValue> {
    let modules = configured_modules()?;
    let slots = required_service(&ctx, "slots")?;
    let locale = required_service(&ctx, "locale")?;
    let connection = required_service(&ctx, "connection")?;
    own_locale_dictionaries(&ctx, &locale)?;
    let translator = call_method(
        &locale,
        "bind",
        &[JsValue::from_str(SETTINGS_LOCALE_NAMESPACE)],
    )?
    .dyn_into::<Function>()?;

    let api = required_property(&connection, "api", "connection")?;
    let loopback = Reflect::get(&connection, &JsValue::from_str("isLoopback"))?
        .as_bool()
        .unwrap_or(false);
    let (document_controller, document_injected) = if loopback {
        let document = WasmSettingsDocumentStore::from_api(&api)?;
        let controller = document.controller.clone();
        let store = document.store_face.clone();
        let use_snapshot = call_method(
            &modules.web_react,
            "bindSnapshotSelector",
            std::slice::from_ref(&store),
        )?;
        let value: JsValue = document.into();
        let injected_controller = value.clone();
        let injected = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
            object(&[
                ("controller", injected_controller.clone()),
                ("useSnapshot", use_snapshot.clone()),
            ])
            .map(Into::into)
        }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
        (Some(controller), Some(injected.into_js_value()))
    } else {
        (None, None)
    };
    own_connection_reset(&ctx, document_controller.as_ref())?;

    let projection = Rc::new(SettingsLedgerProjection::default());
    let sections = section_source(&slots, &locale, projection.clone())?;
    let onboarding = onboarding_source(&slots, projection)?;
    let shell_injected = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let hooks = object(&[
            ("sections", sections.clone()),
            ("onboardingSteps", onboarding.clone()),
        ])?;
        object(&[("hooks", hooks.into())]).map(Into::into)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);

    let root_children = Object::new();
    for (name, kind) in [
        ("settings.trigger", "single"),
        ("settings.header", "single"),
        ("settings.action", "list"),
        ("settings.close", "single"),
        ("settings.section", "list"),
        ("settings.onboarding", "list"),
    ] {
        set(
            &root_children,
            name,
            &object(&[
                ("kind", JsValue::from_str(kind)),
                ("scope", JsValue::from_str("root")),
            ])?
            .into(),
        )?;
    }
    register_injected(
        &slots,
        "sidebar.settings",
        object(&[
            ("name", JsValue::from_str("sidebar.settings")),
            ("children", root_children.into()),
            ("inject", shell_injected.into_js_value()),
        ])?
        .into(),
        settings_root_component(&modules),
    )?;

    register_injected(
        &slots,
        "settings.trigger",
        locale_options("settings.trigger")?,
        trigger_content_component(&modules),
    )?;
    register_injected(
        &slots,
        "settings.header",
        locale_options("settings.header")?,
        translated_text_component("title"),
    )?;
    if let Some(injected) = document_injected {
        register_injected(
            &slots,
            "settings.action",
            object(&[
                ("name", JsValue::from_str("settings.action")),
                ("id", JsValue::from_str("open-document")),
                ("order", JsValue::from_f64(0.0)),
                ("locale", JsValue::from_str(SETTINGS_LOCALE_NAMESPACE)),
                ("inject", injected),
            ])?
            .into(),
            settings_document_action_component(&modules),
        )?;
    }
    register_injected(
        &slots,
        "settings.close",
        locale_options("settings.close")?,
        translated_text_component("close"),
    )?;
    let item_children = object(&[(
        "settings.general.item",
        object(&[
            ("kind", JsValue::from_str("list")),
            ("scope", JsValue::from_str("root")),
        ])?
        .into(),
    )])?;
    let label_translator = translator;
    let label = Closure::wrap(Box::new(move || {
        label_translator
            .call1(&JsValue::UNDEFINED, &JsValue::from_str("general.nav"))
            .unwrap_or_else(|_| JsValue::from_str(""))
    }) as Box<dyn FnMut() -> JsValue>);
    register_injected(
        &slots,
        "settings.section",
        object(&[
            ("name", JsValue::from_str("settings.section")),
            ("id", JsValue::from_str("general")),
            ("order", JsValue::from_f64(0.0)),
            ("label", label.into_js_value()),
            ("locale", JsValue::from_str(SETTINGS_LOCALE_NAMESPACE)),
            ("children", item_children.into()),
        ])?
        .into(),
        general_section_component(&modules),
    )?;
    Ok(())
}

/// Exact Client plugin inject list.
#[wasm_bindgen(js_name = settingsGeneralInject)]
pub fn settings_general_inject() -> Array {
    let values = Array::new();
    for dependency in INJECT {
        values.push(&JsValue::from_str(dependency));
    }
    values
}

fn document_store_face(controller: &Rc<SettingsDocumentStore>) -> Result<JsValue, JsValue> {
    let cache: DocumentSnapshotCache = Rc::new(RefCell::new(None));
    let face = Object::new();
    let snapshot_controller = controller.clone();
    let snapshot_cache = cache;
    let get_snapshot = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let snapshot = snapshot_controller.snapshot();
        if let Some((current, value)) = snapshot_cache.borrow().as_ref()
            && Rc::ptr_eq(current, &snapshot)
        {
            return Ok(value.clone());
        }
        let value = document_snapshot_to_js(&snapshot)?;
        *snapshot_cache.borrow_mut() = Some((snapshot, value.clone()));
        Ok(value)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    set(&face, "getSnapshot", &get_snapshot.into_js_value())?;
    let subscribe_controller = controller.clone();
    let subscribe = Closure::wrap(Box::new(move |listener: Function| -> Function {
        let subscription = subscribe_controller.subscribe(Rc::new(move || {
            listener
                .call0(&JsValue::UNDEFINED)
                .map(|_| ())
                .map_err(|error| error_text(&error))
        }));
        document_disposer(subscription)
    }) as Box<dyn FnMut(Function) -> Function>);
    set(&face, "subscribe", &subscribe.into_js_value())?;
    Ok(face.into())
}

fn document_snapshot_to_js(snapshot: &SettingsDocumentState) -> Result<JsValue, JsValue> {
    let status = match snapshot.status {
        SettingsDocumentStatus::Idle => "idle",
        SettingsDocumentStatus::Loading => "loading",
        SettingsDocumentStatus::Ready => "ready",
        SettingsDocumentStatus::Unavailable => "unavailable",
    };
    let snapshot = object(&[
        ("status", JsValue::from_str(status)),
        ("opening", JsValue::from_bool(snapshot.opening)),
        (
            "error",
            snapshot
                .error
                .as_deref()
                .map_or(JsValue::NULL, JsValue::from_str),
        ),
    ])?;
    Object::freeze(&snapshot);
    Ok(snapshot.into())
}

fn section_source(
    slots: &JsValue,
    locale: &JsValue,
    projection: Rc<SettingsLedgerProjection>,
) -> Result<JsValue, JsValue> {
    let source = Object::new();
    let source_slots = slots.clone();
    let source_locale = locale.clone();
    let cache = Rc::new(RefCell::new(None::<(Rc<Vec<SettingsSectionRow>>, JsValue)>));
    let get_snapshot = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let version = method_u64(&source_slots, "getVersion", "settings.section")?;
        let locale_snapshot = call_method(&source_locale, "getSnapshot", &[])?;
        let revision = property_u64(&locale_snapshot, "revision")?;
        let entries = Array::from(&call_method(
            &source_slots,
            "entries",
            &[JsValue::from_str("settings.section")],
        )?);
        let mut parsed = Vec::new();
        for entry in entries.iter() {
            let options = required_property(&entry, "options", "Slot entry")?;
            parsed.push(SettingsSectionEntry {
                id: optional_string(&options, "id")?,
                order: optional_number(&options, "order")?,
                label: resolve_label(Reflect::get(&options, &JsValue::from_str("label"))?)?,
            });
        }
        let rows = projection.sections(version, revision, parsed);
        if let Some((current, value)) = cache.borrow().as_ref()
            && Rc::ptr_eq(current, &rows)
        {
            return Ok(value.clone());
        }
        let value = section_rows_to_js(&rows)?;
        *cache.borrow_mut() = Some((rows, value.clone()));
        Ok(value)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    set(&source, "getSnapshot", &get_snapshot.into_js_value())?;
    let subscribe_slots = slots.clone();
    let subscribe_locale = locale.clone();
    let subscribe = Closure::wrap(
        Box::new(move |listener: Function| -> Result<Function, JsValue> {
            let off_ledger = call_method(
                &subscribe_slots,
                "subscribe",
                &[
                    JsValue::from_str("settings.section"),
                    listener.clone().into(),
                ],
            )?
            .dyn_into::<Function>()?;
            let off_locale =
                call_method(&subscribe_locale, "subscribe", &[listener.clone().into()])?
                    .dyn_into::<Function>()?;
            Ok(composed_disposer([off_ledger, off_locale]))
        }) as Box<dyn FnMut(Function) -> Result<Function, JsValue>>,
    );
    set(&source, "subscribe", &subscribe.into_js_value())?;
    Ok(source.into())
}

fn onboarding_source(
    slots: &JsValue,
    projection: Rc<SettingsLedgerProjection>,
) -> Result<JsValue, JsValue> {
    let source = Object::new();
    let source_slots = slots.clone();
    let cache = Rc::new(RefCell::new(
        None::<(Rc<Vec<SettingsOnboardingStep>>, JsValue)>,
    ));
    let get_snapshot = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let version = method_u64(&source_slots, "getVersion", "settings.onboarding")?;
        let entries = Array::from(&call_method(
            &source_slots,
            "entries",
            &[JsValue::from_str("settings.onboarding")],
        )?);
        let mut parsed = Vec::new();
        for entry in entries.iter() {
            let options = required_property(&entry, "options", "Slot entry")?;
            parsed.push(SettingsOnboardingEntry {
                id: optional_string(&options, "id")?,
                order: optional_number(&options, "order")?,
            });
        }
        let steps = projection.onboarding(version, parsed);
        if let Some((current, value)) = cache.borrow().as_ref()
            && Rc::ptr_eq(current, &steps)
        {
            return Ok(value.clone());
        }
        let value = onboarding_steps_to_js(&steps)?;
        *cache.borrow_mut() = Some((steps, value.clone()));
        Ok(value)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    set(&source, "getSnapshot", &get_snapshot.into_js_value())?;
    let subscribe_slots = slots.clone();
    let subscribe = Closure::wrap(
        Box::new(move |listener: Function| -> Result<Function, JsValue> {
            call_method(
                &subscribe_slots,
                "subscribe",
                &[JsValue::from_str("settings.onboarding"), listener.into()],
            )?
            .dyn_into::<Function>()
        }) as Box<dyn FnMut(Function) -> Result<Function, JsValue>>,
    );
    set(&source, "subscribe", &subscribe.into_js_value())?;
    Ok(source.into())
}

fn section_rows_to_js(rows: &[SettingsSectionRow]) -> Result<JsValue, JsValue> {
    let values = Array::new();
    for row in rows {
        let value = object(&[
            ("id", JsValue::from_str(&row.id)),
            ("order", JsValue::from_f64(row.order)),
            ("label", JsValue::from_str(&row.label)),
        ])?;
        Object::freeze(&value);
        values.push(&value);
    }
    Object::freeze(&values);
    Ok(values.into())
}

fn onboarding_steps_to_js(steps: &[SettingsOnboardingStep]) -> Result<JsValue, JsValue> {
    let values = Array::new();
    for step in steps {
        let value = object(&[
            ("id", JsValue::from_str(&step.id)),
            ("order", JsValue::from_f64(step.order)),
        ])?;
        Object::freeze(&value);
        values.push(&value);
    }
    Object::freeze(&values);
    Ok(values.into())
}

fn own_locale_dictionaries(ctx: &JsValue, locale: &JsValue) -> Result<(), JsValue> {
    let dictionaries = Object::new();
    set(&dictionaries, "zh", &dictionary(SETTINGS_ZH)?)?;
    set(&dictionaries, "en", &dictionary(SETTINGS_EN)?)?;
    let locale = locale.clone();
    let installer = Closure::wrap(Box::new(move || {
        call_method(
            &locale,
            "register",
            &[
                JsValue::from_str(SETTINGS_LOCALE_NAMESPACE),
                dictionaries.clone().into(),
            ],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        ctx,
        "effect",
        &[
            installer.into_js_value(),
            JsValue::from_str("ui-settings-general: dictionaries"),
        ],
    )?;
    Ok(())
}

fn own_connection_reset(
    ctx: &JsValue,
    document: Option<&Rc<SettingsDocumentStore>>,
) -> Result<(), JsValue> {
    let document = document.cloned();
    let event_ctx = ctx.clone();
    let installer = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let document = document.clone();
        let listener = Closure::wrap(Box::new(move || {
            refresh_document_if_loaded(document.as_ref(), &BrowserSpawner);
        }) as Box<dyn FnMut()>);
        call_method(
            &event_ctx,
            "on",
            &[
                JsValue::from_str("connection/reset"),
                listener.into_js_value(),
            ],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        ctx,
        "effect",
        &[
            installer.into_js_value(),
            JsValue::from_str("ui-settings-general: metadata invalidations"),
        ],
    )?;
    Ok(())
}

fn register_injected(
    slots: &JsValue,
    name: &str,
    options: JsValue,
    component: JsValue,
) -> Result<(), JsValue> {
    let registrar = slots.clone();
    let register = Closure::wrap(Box::new(move || {
        call_method(
            &registrar,
            "register",
            &[options.clone(), component.clone()],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        slots,
        "inject",
        &[JsValue::from_str(name), register.into_js_value()],
    )?;
    Ok(())
}

fn locale_options(name: &str) -> Result<JsValue, JsValue> {
    object(&[
        ("name", JsValue::from_str(name)),
        ("locale", JsValue::from_str(SETTINGS_LOCALE_NAMESPACE)),
    ])
    .map(Into::into)
}

fn dictionary(entries: &[(&str, &str)]) -> Result<JsValue, JsValue> {
    let dictionary = Object::new();
    for (key, value) in entries {
        set(&dictionary, key, &JsValue::from_str(value))?;
    }
    Ok(dictionary.into())
}

fn parse_describe_response(
    response: &JsValue,
) -> SettingsDocumentCall<SettingsDocumentDescription> {
    match response_result(response) {
        Ok(Ok(value)) => {
            let has_document = Reflect::get(&value, &JsValue::from_str("hasDocument"))
                .ok()
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            SettingsDocumentCall::Success(SettingsDocumentDescription { has_document })
        }
        Ok(Err(message)) => SettingsDocumentCall::Rejected(message),
        Err(message) => SettingsDocumentCall::Failed(message),
    }
}

fn parse_empty_response(response: &JsValue) -> SettingsDocumentCall<()> {
    match response_result(response) {
        Ok(Ok(_)) => SettingsDocumentCall::Success(()),
        Ok(Err(message)) => SettingsDocumentCall::Rejected(message),
        Err(message) => SettingsDocumentCall::Failed(message),
    }
}

fn response_result(response: &JsValue) -> Result<Result<JsValue, String>, String> {
    let result =
        Reflect::get(response, &JsValue::from_str("result")).map_err(|error| error_text(&error))?;
    let ok = Reflect::get(&result, &JsValue::from_str("ok"))
        .map_err(|error| error_text(&error))?
        .as_bool()
        .ok_or_else(|| "ui-settings-general: RPC result omitted boolean ok".to_owned())?;
    if ok {
        return Reflect::get(&result, &JsValue::from_str("value"))
            .map(Ok)
            .map_err(|error| error_text(&error));
    }
    let error =
        Reflect::get(&result, &JsValue::from_str("error")).map_err(|error| error_text(&error))?;
    let message = Reflect::get(&error, &JsValue::from_str("message"))
        .ok()
        .and_then(|value| value.as_string())
        .unwrap_or_default();
    Ok(Err(message))
}

fn document_operation_promise(future: LocalBoxFuture<'static, Result<(), String>>) -> Promise {
    future_to_promise(async move {
        future
            .await
            .map(|()| JsValue::UNDEFINED)
            .map_err(|error| js_error(&error))
    })
}

fn document_disposer(disposer: SettingsDocumentSubscription) -> Function {
    Closure::wrap(Box::new(move || disposer.dispose()) as Box<dyn FnMut()>)
        .into_js_value()
        .unchecked_into()
}

fn composed_disposer<const N: usize>(disposers: [Function; N]) -> Function {
    Closure::wrap(Box::new(move || {
        for disposer in &disposers {
            let _ = disposer.call0(&JsValue::UNDEFINED);
        }
    }) as Box<dyn FnMut()>)
    .into_js_value()
    .unchecked_into()
}

async fn await_method(
    value: &JsValue,
    name: &str,
    arguments: &[JsValue],
) -> Result<JsValue, String> {
    let result = call_method(value, name, arguments).map_err(|error| error_text(&error))?;
    JsFuture::from(Promise::resolve(&result))
        .await
        .map_err(|error| error_text(&error))
}

fn resolve_label(value: JsValue) -> Result<Option<String>, JsValue> {
    if value.is_undefined() || value.is_null() {
        return Ok(None);
    }
    if let Some(value) = value.as_string() {
        return Ok(Some(value));
    }
    let function = value.dyn_into::<Function>()?;
    Ok(function.call0(&JsValue::UNDEFINED)?.as_string())
}

fn method_u64(value: &JsValue, method: &str, argument: &str) -> Result<u64, JsValue> {
    js_u64(&call_method(value, method, &[JsValue::from_str(argument)])?)
}

fn property_u64(value: &JsValue, property: &str) -> Result<u64, JsValue> {
    js_u64(&Reflect::get(value, &JsValue::from_str(property))?)
}

fn js_u64(value: &JsValue) -> Result<u64, JsValue> {
    value
        .as_f64()
        .filter(|value| value.is_finite() && value.fract() == 0.0 && *value >= 0.0)
        .and_then(|value| format!("{value:.0}").parse().ok())
        .ok_or_else(|| js_error("ui-settings-general: expected an unsigned integer revision"))
}

fn optional_string(value: &JsValue, property: &str) -> Result<Option<String>, JsValue> {
    let value = Reflect::get(value, &JsValue::from_str(property))?;
    if value.is_undefined() || value.is_null() {
        Ok(None)
    } else {
        value
            .as_string()
            .map(Some)
            .ok_or_else(|| js_error("ui-settings-general: expected an optional string"))
    }
}

fn optional_number(value: &JsValue, property: &str) -> Result<Option<f64>, JsValue> {
    let value = Reflect::get(value, &JsValue::from_str(property))?;
    if value.is_undefined() || value.is_null() {
        Ok(None)
    } else {
        value
            .as_f64()
            .map(Some)
            .ok_or_else(|| js_error("ui-settings-general: expected an optional number"))
    }
}

fn configured_modules() -> Result<BrowserModules, JsValue> {
    MODULES.with(|modules| {
        modules.borrow().clone().ok_or_else(|| {
            js_error("client-ui-settings-general module factory did not configure shell modules")
        })
    })
}

fn inject_styles() -> Result<(), JsValue> {
    let document = required_property(&js_sys::global(), "document", "global")?;
    let style = call_method(&document, "createElement", &[JsValue::from_str("style")])?;
    call_method(
        &style,
        "setAttribute",
        &[
            JsValue::from_str("data-plugin"),
            JsValue::from_str("@seekdeep-ai/seekdeep-client-ui-settings-general"),
        ],
    )?;
    Reflect::set(
        &style,
        &JsValue::from_str("textContent"),
        &JsValue::from_str(STYLES),
    )?;
    let head = required_property(&document, "head", "document")?;
    call_method(&head, "appendChild", &[style])?;
    Ok(())
}

fn required_service(ctx: &JsValue, name: &str) -> Result<JsValue, JsValue> {
    let service = call_method(ctx, "get", &[JsValue::from_str(name)])?;
    if service.is_undefined() {
        Err(js_error(&format!(
            "client-ui-settings-general requires Client Service {name:?}"
        )))
    } else {
        Ok(service)
    }
}

fn required_property(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_undefined() || property.is_null() {
        Err(js_error(&format!(
            "ui-settings-general: {owner} omitted required property {key:?}"
        )))
    } else {
        Ok(property)
    }
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let object = Object::new();
    for (key, value) in entries {
        set(&object, key, value)?;
    }
    Ok(object)
}

fn set(object: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    Reflect::set(object, &JsValue::from_str(key), value).map(|_| ())
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    method.apply(value, &args)
}

fn js_error(message: &str) -> JsValue {
    js_sys::Error::new(message).into()
}

fn error_text(error: &JsValue) -> String {
    Reflect::get(error, &JsValue::from_str("message"))
        .ok()
        .and_then(|message| message.as_string())
        .or_else(|| error.as_string())
        .unwrap_or_else(|| format!("{error:?}"))
}

#[derive(Clone)]
struct ReactUi {
    react: JsValue,
    primitives: JsValue,
}

impl ReactUi {
    fn element(
        &self,
        kind: &JsValue,
        props: Option<&Object>,
        children: &[JsValue],
    ) -> Result<JsValue, JsValue> {
        let arguments = Array::new();
        arguments.push(kind);
        arguments.push(props.map_or(&JsValue::NULL, AsRef::as_ref));
        for child in children {
            arguments.push(child);
        }
        function(&self.react, "createElement")?.apply(&self.react, &arguments)
    }

    fn tag(
        &self,
        name: &str,
        props: Option<&Object>,
        children: &[JsValue],
    ) -> Result<JsValue, JsValue> {
        self.element(&JsValue::from_str(name), props, children)
    }

    fn primitive(
        &self,
        name: &str,
        props: Option<&Object>,
        children: &[JsValue],
    ) -> Result<JsValue, JsValue> {
        self.element(
            &required_property(&self.primitives, name, "UI primitives")?,
            props,
            children,
        )
    }

    fn fragment(&self, children: &[JsValue]) -> Result<JsValue, JsValue> {
        self.element(
            &required_property(&self.react, "Fragment", "React")?,
            None,
            children,
        )
    }
}

fn browser_ui(modules: &BrowserModules) -> ReactUi {
    ReactUi {
        react: modules.react.clone(),
        primitives: modules.primitives.clone(),
    }
}

fn trigger_content_component(modules: &BrowserModules) -> JsValue {
    let ui = browser_ui(modules);
    let component = Closure::wrap(Box::new(move |props: JsValue| -> Result<JsValue, JsValue> {
        let wide = required_property(&props, "wide", "TriggerContent props")?
            .as_bool()
            .unwrap_or(false);
        let icon = ui.primitive(
            if wide {
                "IconSettingsOutline16"
            } else {
                "IconSettingsOutline14"
            },
            Some(&object(&[(
                "size",
                JsValue::from_f64(if wide { 16.0 } else { 18.0 }),
            )])?),
            &[],
        )?;
        let mut children = vec![icon];
        if wide {
            let label = translated(&props, "trigger")?;
            children.push(ui.tag(
                "span",
                Some(&class_props("seekdeep-settings-trigger-label")?),
                &[label],
            )?);
        }
        ui.fragment(&children)
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    component.into_js_value()
}

fn translated_text_component(key: &'static str) -> JsValue {
    let component = Closure::wrap(Box::new(move |props: JsValue| translated(&props, key))
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    component.into_js_value()
}

fn settings_document_action_component(modules: &BrowserModules) -> JsValue {
    let ui = browser_ui(modules);
    let component =
        Closure::wrap(
            Box::new(move |props: JsValue| render_settings_document_action(&ui, &props))
                as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
        );
    component.into_js_value()
}

fn render_settings_document_action(ui: &ReactUi, props: &JsValue) -> Result<JsValue, JsValue> {
    let controller = required_property(props, "controller", "document action props")?;
    let use_snapshot = function(props, "useSnapshot")?;
    let selector =
        Closure::wrap(Box::new(|snapshot: JsValue| snapshot) as Box<dyn FnMut(JsValue) -> JsValue>);
    let state = use_snapshot.call1(props, &selector.into_js_value())?;

    let load_controller = controller.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        call_method(&load_controller, "load", &[])?;
        Ok(JsValue::UNDEFINED)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let dependencies = Array::of1(&controller);
    function(&ui.react, "useEffect")?.call2(&ui.react, &effect.into_js_value(), &dependencies)?;

    if required_property(&state, "status", "document state")?
        .as_string()
        .as_deref()
        != Some("ready")
    {
        return Ok(JsValue::NULL);
    }
    let error = Reflect::get(&state, &JsValue::from_str("error"))?;
    let mut children = Vec::new();
    if !error.is_null() && !error.is_undefined() {
        children.push(ui.tag(
            "span",
            Some(&object(&[
                (
                    "className",
                    JsValue::from_str("seekdeep-settings-document-error"),
                ),
                ("role", JsValue::from_str("alert")),
            ])?),
            &[translated(props, "openDocument.error")?],
        )?);
    }
    let open_controller = controller;
    let open = Closure::wrap(Box::new(move || {
        let _ = call_method(&open_controller, "open", &[]);
    }) as Box<dyn FnMut()>);
    children.push(
        ui.primitive(
            "Button",
            Some(&object(&[
                ("variant", JsValue::from_str("outline")),
                ("size", JsValue::from_str("sm")),
                (
                    "disabled",
                    JsValue::from_bool(
                        required_property(&state, "opening", "document state")?
                            .as_bool()
                            .unwrap_or(false),
                    ),
                ),
                ("onClick", open.into_js_value()),
            ])?),
            &[translated(props, "openDocument")?],
        )?,
    );
    ui.tag(
        "div",
        Some(&class_props("seekdeep-settings-document-action")?),
        &children,
    )
}

fn general_section_component(modules: &BrowserModules) -> JsValue {
    let ui = browser_ui(modules);
    let component = Closure::wrap(Box::new(move |props: JsValue| -> Result<JsValue, JsValue> {
        let body = call_prop(
            &props,
            "renderSlot",
            &[
                JsValue::from_str("settings.general.item"),
                Object::new().into(),
            ],
        )?;
        ui.tag(
            "div",
            Some(&class_props("seekdeep-settings-general-section")?),
            &[body],
        )
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    component.into_js_value()
}

fn settings_root_component(modules: &BrowserModules) -> JsValue {
    let ui = browser_ui(modules);
    let panel = settings_panel_component(ui.clone());
    let component =
        Closure::wrap(
            Box::new(move |props: JsValue| render_settings_root(&ui, &panel, &props))
                as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
        );
    component.into_js_value()
}

#[allow(clippy::too_many_lines)]
fn render_settings_root(
    ui: &ReactUi,
    panel: &JsValue,
    props: &JsValue,
) -> Result<JsValue, JsValue> {
    let (open, set_open) = use_state(&ui.react, &JsValue::FALSE)?;
    let (active_id, set_active_id) = use_state(&ui.react, &JsValue::UNDEFINED)?;
    let completed_initial: JsValue = js_sys::Set::new(&JsValue::UNDEFINED).into();
    let (completed, set_completed) = use_state(&ui.react, &completed_initial)?;
    let close_set_open = set_open.clone();
    let close_set_active = set_active_id.clone();
    let close = use_callback(
        &ui.react,
        &Closure::wrap(Box::new(move || {
            let _ = close_set_open.call1(&JsValue::UNDEFINED, &JsValue::FALSE);
            let _ = close_set_active.call1(&JsValue::UNDEFINED, &JsValue::UNDEFINED);
        }) as Box<dyn FnMut()>)
        .into_js_value(),
        &[],
    )?;
    let section_set_open = set_open.clone();
    let section_set_active = set_active_id.clone();
    let open_section = use_callback(
        &ui.react,
        &Closure::wrap(Box::new(move |id: String| {
            let _ = section_set_active.call1(&JsValue::UNDEFINED, &JsValue::from_str(&id));
            let _ = section_set_open.call1(&JsValue::UNDEFINED, &JsValue::TRUE);
        }) as Box<dyn FnMut(String)>)
        .into_js_value(),
        &[],
    )?;

    let identity =
        Closure::wrap(Box::new(|value: JsValue| value) as Box<dyn FnMut(JsValue) -> JsValue>);
    let rows =
        Array::from(&function(props, "useSections")?.call1(props, &identity.into_js_value())?);
    let identity =
        Closure::wrap(Box::new(|value: JsValue| value) as Box<dyn FnMut(JsValue) -> JsValue>);
    let onboarding = Array::from(
        &function(props, "useOnboardingSteps")?.call1(props, &identity.into_js_value())?,
    );
    let sessions_selector = Closure::wrap(Box::new(|state: JsValue| -> Result<bool, JsValue> {
        if required_property(&state, "phase", "Sessions state")?
            .as_string()
            .as_deref()
            != Some("ready")
        {
            return Ok(false);
        }
        let current = Reflect::get(&state, &JsValue::from_str("current"))?;
        if current.is_undefined() {
            return Ok(true);
        }
        let Some(current) = current.as_string() else {
            return Ok(false);
        };
        let by_id = required_property(&state, "byId", "Sessions state")?;
        let session = Reflect::get(&by_id, &JsValue::from_str(&current))?;
        Ok(!session.is_undefined()
            && Reflect::get(&session, &JsValue::from_str("blank"))?
                .as_bool()
                .unwrap_or(false))
    })
        as Box<dyn FnMut(JsValue) -> Result<bool, JsValue>>);
    let onboarding_active = function(props, "useSessions")?
        .call1(props, &sessions_selector.into_js_value())?
        .as_bool()
        .unwrap_or(false);

    let reset_setter = set_completed.clone();
    let reset = Closure::wrap(Box::new(move || {
        if !onboarding_active {
            let _ = reset_setter.call1(&JsValue::UNDEFINED, &js_sys::Set::new(&JsValue::UNDEFINED));
        }
        JsValue::UNDEFINED
    }) as Box<dyn FnMut() -> JsValue>);
    let reset_deps = Array::of1(&JsValue::from_bool(onboarding_active));
    function(&ui.react, "useEffect")?.call2(&ui.react, &reset.into_js_value(), &reset_deps)?;

    let completed = js_sys::Set::from(completed);
    let onboarding_step = if onboarding_active {
        onboarding.iter().find(|step| {
            optional_string(step, "id")
                .ok()
                .flatten()
                .is_some_and(|id| !completed.has(&JsValue::from_str(&id)))
        })
    } else {
        None
    };

    let wide = required_property(props, "wide", "SettingsRoot props")?
        .as_bool()
        .unwrap_or(false);
    let trigger_content = call_prop(
        props,
        "renderSlot",
        &[
            JsValue::from_str("settings.trigger"),
            object(&[("wide", JsValue::from_bool(wide))])?.into(),
        ],
    )?;
    let trigger_setter = set_open;
    let trigger = Closure::wrap(Box::new(move || {
        let _ = trigger_setter.call1(&JsValue::UNDEFINED, &JsValue::TRUE);
    }) as Box<dyn FnMut()>);
    let trigger_class = if wide {
        "seekdeep-settings-trigger"
    } else {
        "seekdeep-settings-trigger seekdeep-settings-rail"
    };
    let trigger = ui.tag(
        "button",
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            ("className", JsValue::from_str(trigger_class)),
            ("aria-haspopup", JsValue::from_str("dialog")),
            (
                "aria-expanded",
                JsValue::from_bool(open.as_bool().unwrap_or(false)),
            ),
            ("onClick", trigger.into_js_value()),
        ])?),
        &[trigger_content],
    )?;

    let mut children = vec![trigger];
    if open.as_bool().unwrap_or(false) {
        children.push(ui.element(
            panel,
            Some(&object(&[
                ("rows", rows.clone().into()),
                ("renderSlot", function(props, "renderSlot")?.into()),
                ("activeId", active_id),
                ("onSelect", set_active_id.into()),
                ("onClose", close.clone().into()),
            ])?),
            &[],
        )?);
    }
    if let Some(step) = onboarding_step {
        let id = required_string(&step, "id", "onboarding step")?;
        let complete_setter = set_completed;
        let complete_id = id.clone();
        let complete = Closure::wrap(Box::new(move || {
            let id = complete_id.clone();
            let update = Closure::wrap(Box::new(move |previous: js_sys::Set| -> js_sys::Set {
                if previous.has(&JsValue::from_str(&id)) {
                    return previous;
                }
                let next = js_sys::Set::new(previous.as_ref());
                next.add(&JsValue::from_str(&id));
                next
            })
                as Box<dyn FnMut(js_sys::Set) -> js_sys::Set>);
            let _ = complete_setter.call1(&JsValue::UNDEFINED, &update.into_js_value());
        }) as Box<dyn FnMut()>);
        let owner = object(&[
            ("stepId", JsValue::from_str(&id)),
            ("complete", complete.into_js_value()),
            ("openSection", open_section.into()),
        ])?;
        let only = object(&[("only", JsValue::from_str(&id))])?;
        children.push(call_prop(
            props,
            "renderSlot",
            &[
                JsValue::from_str("settings.onboarding"),
                owner.into(),
                only.into(),
            ],
        )?);
    }
    ui.fragment(&children)
}

fn settings_panel_component(ui: ReactUi) -> JsValue {
    let component = Closure::wrap(
        Box::new(move |props: JsValue| render_settings_panel(&ui, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    );
    component.into_js_value()
}

#[allow(clippy::too_many_lines)]
fn render_settings_panel(ui: &ReactUi, props: &JsValue) -> Result<JsValue, JsValue> {
    let rows = Array::from(&required_property(props, "rows", "SettingsPanel props")?);
    let active_id = Reflect::get(props, &JsValue::from_str("activeId"))?.as_string();
    let first = rows.get(0);
    let first_id = if first.is_undefined() {
        None
    } else {
        optional_string(&first, "id")?
    };
    let active = active_id
        .filter(|active| {
            rows.iter().any(|row| {
                optional_string(&row, "id").ok().flatten().as_deref() == Some(active.as_str())
            })
        })
        .or(first_id);
    let title_id = function(&ui.react, "useId")?
        .call0(&ui.react)?
        .as_string()
        .unwrap_or_default();
    let on_close = function(props, "onClose")?;

    let key_document = required_property(&js_sys::global(), "document", "global")?;
    let key_close = on_close.clone();
    let key_effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let listener_close = key_close.clone();
        let listener = Closure::wrap(Box::new(move |event: JsValue| {
            if Reflect::get(&event, &JsValue::from_str("key"))
                .ok()
                .and_then(|value| value.as_string())
                .as_deref()
                == Some("Escape")
            {
                let _ = listener_close.call0(&JsValue::UNDEFINED);
            }
        }) as Box<dyn FnMut(JsValue)>);
        let listener: JsValue = listener.into_js_value();
        call_method(
            &key_document,
            "addEventListener",
            &[JsValue::from_str("keydown"), listener.clone()],
        )?;
        let cleanup_document = key_document.clone();
        let cleanup = Closure::wrap(Box::new(move || {
            let _ = call_method(
                &cleanup_document,
                "removeEventListener",
                &[JsValue::from_str("keydown"), listener.clone()],
            );
        }) as Box<dyn FnMut()>);
        Ok(cleanup.into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let key_deps = Array::of1(&on_close);
    function(&ui.react, "useEffect")?.call2(&ui.react, &key_effect.into_js_value(), &key_deps)?;

    let close_ref = function(&ui.react, "useRef")?.call1(&ui.react, &JsValue::NULL)?;
    let focus_ref = close_ref.clone();
    let focus_effect = Closure::wrap(Box::new(move || {
        let current =
            Reflect::get(&focus_ref, &JsValue::from_str("current")).unwrap_or(JsValue::UNDEFINED);
        if !current.is_null() && !current.is_undefined() {
            let _ = call_method(&current, "focus", &[]);
        }
        JsValue::UNDEFINED
    }) as Box<dyn FnMut() -> JsValue>);
    function(&ui.react, "useEffect")?.call2(
        &ui.react,
        &focus_effect.into_js_value(),
        &Array::new(),
    )?;

    let title = call_prop(
        props,
        "renderSlot",
        &[JsValue::from_str("settings.header"), Object::new().into()],
    )?;
    let title = ui.tag(
        "div",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-settings-nav-title"),
            ),
            ("id", JsValue::from_str(&title_id)),
        ])?),
        &[title],
    )?;
    let mut nav_cells = Vec::new();
    for row in rows.iter() {
        let id = required_string(&row, "id", "settings row")?;
        let label = required_string(&row, "label", "settings row")?;
        let selected = active.as_deref() == Some(id.as_str());
        let select = function(props, "onSelect")?;
        let select_id = id.clone();
        let on_click = Closure::wrap(Box::new(move || {
            let _ = select.call1(&JsValue::UNDEFINED, &JsValue::from_str(&select_id));
        }) as Box<dyn FnMut()>);
        let icon = settings_nav_icon(ui, &id)?;
        let label = ui.tag(
            "span",
            Some(&class_props("seekdeep-settings-nav-label")?),
            &[JsValue::from_str(&label)],
        )?;
        nav_cells.push(ui.tag(
            "button",
            Some(&object(&[
                ("key", JsValue::from_str(&id)),
                ("type", JsValue::from_str("button")),
                (
                    "className",
                    JsValue::from_str(if selected {
                        "seekdeep-settings-nav-cell seekdeep-settings-active"
                    } else {
                        "seekdeep-settings-nav-cell"
                    }),
                ),
                (
                    "aria-current",
                    if selected {
                        JsValue::from_str("true")
                    } else {
                        JsValue::UNDEFINED
                    },
                ),
                ("onClick", on_click.into_js_value()),
            ])?),
            &[icon, label],
        )?);
    }
    let nav_list = ui.tag(
        "div",
        Some(&class_props("seekdeep-settings-nav-list")?),
        &nav_cells,
    )?;
    let nav = ui.tag(
        "nav",
        Some(&class_props("seekdeep-settings-nav")?),
        &[title, nav_list],
    )?;

    let actions = call_prop(
        props,
        "renderSlot",
        &[JsValue::from_str("settings.action"), Object::new().into()],
    )?;
    let actions = ui.tag(
        "div",
        Some(&class_props("seekdeep-settings-actions")?),
        &[actions],
    )?;
    let close_label = call_prop(
        props,
        "renderSlot",
        &[JsValue::from_str("settings.close"), Object::new().into()],
    )?;
    let close_label = ui.tag(
        "span",
        Some(&class_props("seekdeep-settings-hidden-label")?),
        &[close_label],
    )?;
    let close_icon = ui.primitive(
        "IconCloseOutline16",
        Some(&object(&[("size", JsValue::from_f64(14.0))])?),
        &[],
    )?;
    let close_button = ui.tag(
        "button",
        Some(&object(&[
            ("ref", close_ref),
            ("type", JsValue::from_str("button")),
            ("className", JsValue::from_str("seekdeep-settings-close")),
            ("onClick", on_close.clone().into()),
        ])?),
        &[close_icon, close_label],
    )?;
    let header = ui.tag(
        "div",
        Some(&class_props("seekdeep-settings-header")?),
        &[actions, close_button],
    )?;
    let section = if let Some(active) = active {
        call_prop(
            props,
            "renderSlot",
            &[
                JsValue::from_str("settings.section"),
                object(&[("close", on_close.clone().into())])?.into(),
                object(&[("only", JsValue::from_str(&active))])?.into(),
            ],
        )?
    } else {
        JsValue::UNDEFINED
    };
    let options = ui.tag(
        "div",
        Some(&class_props("seekdeep-settings-options")?),
        if section.is_undefined() {
            &[]
        } else {
            std::slice::from_ref(&section)
        },
    )?;
    let content = ui.tag(
        "div",
        Some(&class_props("seekdeep-settings-content")?),
        &[header, options],
    )?;
    let panel = ui.tag(
        "div",
        Some(&object(&[
            ("className", JsValue::from_str("seekdeep-settings-panel")),
            ("role", JsValue::from_str("dialog")),
            ("aria-modal", JsValue::TRUE),
            ("aria-labelledby", JsValue::from_str(&title_id)),
        ])?),
        &[nav, content],
    )?;
    let mask = ui.tag(
        "div",
        Some(&object(&[
            ("className", JsValue::from_str("seekdeep-settings-mask")),
            ("aria-hidden", JsValue::from_str("true")),
            ("onClick", on_close.into()),
        ])?),
        &[],
    )?;
    ui.tag(
        "div",
        Some(&object(&[
            ("className", JsValue::from_str("seekdeep-settings-overlay")),
            ("role", JsValue::from_str("presentation")),
        ])?),
        &[mask, panel],
    )
}

fn settings_nav_icon(ui: &ReactUi, id: &str) -> Result<JsValue, JsValue> {
    let primitive = match id {
        "models" => "IconDataOutline16",
        "agent-presets" => "IconAgentPresetOutline16",
        "plugins" => "IconPersonalizationOutline16",
        _ => "IconSettingsOutline16",
    };
    ui.primitive(
        primitive,
        Some(&object(&[
            ("className", JsValue::from_str("seekdeep-settings-nav-icon")),
            ("size", JsValue::from_f64(16.0)),
        ])?),
        &[],
    )
}

fn translated(props: &JsValue, key: &str) -> Result<JsValue, JsValue> {
    function(props, "t")?.call1(props, &JsValue::from_str(key))
}

fn use_state(react: &JsValue, initial: &JsValue) -> Result<(JsValue, Function), JsValue> {
    let state = Array::from(&function(react, "useState")?.call1(react, initial)?);
    Ok((state.get(0), state.get(1).dyn_into::<Function>()?))
}

fn use_callback(
    react: &JsValue,
    callback: &JsValue,
    dependencies: &[JsValue],
) -> Result<Function, JsValue> {
    let deps = Array::new();
    for dependency in dependencies {
        deps.push(dependency);
    }
    function(react, "useCallback")?
        .call2(react, callback, &deps)?
        .dyn_into::<Function>()
}

fn call_prop(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = function(value, name)?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    method.apply(value, &args)
}

fn class_props(class_name: &str) -> Result<Object, JsValue> {
    object(&[("className", JsValue::from_str(class_name))])
}

fn function(value: &JsValue, name: &str) -> Result<Function, JsValue> {
    required_property(value, name, "JavaScript object")?.dyn_into::<Function>()
}

fn required_string(value: &JsValue, property: &str, owner: &str) -> Result<String, JsValue> {
    Reflect::get(value, &JsValue::from_str(property))?
        .as_string()
        .ok_or_else(|| {
            js_error(&format!(
                "ui-settings-general: {owner} omitted string property {property:?}"
            ))
        })
}
