//! Browser WASM facade, module-table configuration, locale service, and Language row.

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{
    COMMON_NAMESPACE, FALLBACK_LOCALE, LOCALE_PREFERENCE_FIELD, LOCALE_SETTINGS_NAMESPACE,
    LocaleDictionary, LocaleDisposer, LocaleHostScope, LocaleId, LocaleRuntime, LocaleSettings,
    LocaleSnapshot, SETTINGS_NAMESPACE, TranslateParameters, chinese_common_dictionary,
    detect_browser_locale, english_common_dictionary, language_settings_dictionaries,
};

const SLOT_NAME: &str = "settings.general.item";
const STYLES: &str = include_str!("../data/styles.css");
const INJECT: &[&str] = &["slots", "connection", "remote", "settingsScope"];

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    primitives: JsValue,
    runtime: JsValue,
}

/// Configures shell-owned modules at Client-module factory materialization.
///
/// # Errors
///
/// Returns DOM style-injection failures.
#[wasm_bindgen(js_name = configureClientLocale)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_locale(
    react: JsValue,
    primitives: JsValue,
    runtime: JsValue,
) -> Result<(), JsValue> {
    MODULES.with(|modules| {
        *modules.borrow_mut() = Some(BrowserModules {
            react,
            primitives,
            runtime,
        });
    });
    inject_styles()
}

/// Browser Client plugin apply function.
///
/// # Errors
///
/// Returns missing-service, settings-scope, locale, Store, Slot, React, or DOM failures.
#[wasm_bindgen(js_name = applyClientLocale)]
#[allow(clippy::needless_pass_by_value)]
#[allow(clippy::too_many_lines)]
pub fn apply_client_locale(ctx: JsValue) -> Result<(), JsValue> {
    let modules = configured_modules()?;
    let slots = required_service(&ctx, "slots")?;
    let settings_scope = required_service(&ctx, "settingsScope")?;
    let bind_options = object(&[("namespace", JsValue::from_str(LOCALE_SETTINGS_NAMESPACE))])?;
    let scope = call_method(&settings_scope, "bind", &[bind_options.into()])?;
    let host = js_host_scope(scope);
    let provisional = browser_locale().unwrap_or(FALLBACK_LOCALE);
    let event_context = ctx.clone();
    let locales = locale_definitions_js()?;
    let event_locales = locales.clone();
    let runtime = LocaleRuntime::new(
        provisional,
        Some(host),
        move |snapshot| {
            if let Ok(value) = snapshot_to_js(&snapshot, &event_locales) {
                let _ = call_method(
                    &event_context,
                    "emit",
                    &[JsValue::from_str("locale/change"), value],
                );
            }
        },
        |message| web_sys::console::error_1(&JsValue::from_str(&message)),
    );
    own_runtime_disposal(&ctx, &runtime)?;
    runtime
        .register_many(
            COMMON_NAMESPACE,
            [
                ("zh".into(), chinese_common_dictionary()),
                ("en".into(), english_common_dictionary()),
            ],
        )
        .map_err(|error| js_error(&error))?;
    runtime
        .register_many(SETTINGS_NAMESPACE, language_settings_dictionaries())
        .map_err(|error| js_error(&error))?;
    let service = locale_service(runtime.clone(), &locales)?;
    call_method(
        &ctx,
        "provide",
        &[JsValue::from_str("locale"), service.clone()],
    )?;
    call_method(&slots, "installLocale", std::slice::from_ref(&service))?;

    let store = create_language_row_store_with(&modules.runtime)?;
    let bound_actions = Rc::new(RefCell::new(None::<JsValue>));
    let event_actions = bound_actions.clone();
    let event_listener = Closure::wrap(Box::new(move |snapshot: JsValue| {
        if let Some(actions) = event_actions.borrow().as_ref() {
            let _ = sync_actions(actions, &snapshot);
        }
    }) as Box<dyn FnMut(JsValue)>);
    call_method(
        &ctx,
        "on",
        &[
            JsValue::from_str("locale/change"),
            event_listener.into_js_value(),
        ],
    )?;

    let component = language_row_component(&modules);
    let service_for_inject = service.clone();
    let actions_for_inject = bound_actions;
    let inject = Closure::wrap(
        Box::new(move |actions: JsValue| -> Result<JsValue, JsValue> {
            *actions_for_inject.borrow_mut() = Some(actions.clone());
            sync_actions(
                &actions,
                &call_method(&service_for_inject, "getLocale", &[])?,
            )?;
            let set_service = service_for_inject.clone();
            let set_locale = Closure::wrap(Box::new(move |id: String| -> Result<(), JsValue> {
                call_method(&set_service, "setLocale", &[JsValue::from_str(&id)])?;
                Ok(())
            })
                as Box<dyn FnMut(String) -> Result<(), JsValue>>);
            object(&[("setLocale", set_locale.into_js_value())]).map(Into::into)
        }) as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    );
    let options = object(&[
        ("name", JsValue::from_str(SLOT_NAME)),
        ("id", JsValue::from_str("language")),
        ("order", JsValue::from_f64(0.0)),
        ("store", store),
        ("locale", JsValue::from_str(SETTINGS_NAMESPACE)),
        ("inject", inject.into_js_value()),
    ])?;
    let registrar = slots.clone();
    let register = Closure::wrap(Box::new(move || {
        call_method(
            &registrar,
            "register",
            &[options.clone().into(), component.clone()],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        &slots,
        "inject",
        &[JsValue::from_str(SLOT_NAME), register.into_js_value()],
    )?;

    Ok(())
}

fn own_runtime_disposal(ctx: &JsValue, runtime: &LocaleRuntime) -> Result<(), JsValue> {
    let dispose_runtime = runtime.clone();
    let disposer = Closure::wrap(Box::new(move || dispose_runtime.dispose()) as Box<dyn FnMut()>);
    let disposer: JsValue = disposer.into_js_value();
    let installer =
        Closure::wrap(Box::new(move || disposer.clone()) as Box<dyn FnMut() -> JsValue>);
    call_method(
        ctx,
        "effect",
        &[
            installer.into_js_value(),
            JsValue::from_str("locale: settings scope adoption"),
        ],
    )?;
    Ok(())
}

/// Builds the source-compatible Store handle through Client Runtime's Store engine.
///
/// # Errors
///
/// Returns missing module configuration or JavaScript Store construction failures.
#[wasm_bindgen(js_name = createLanguageRowStore)]
pub fn create_language_row_store() -> Result<JsValue, JsValue> {
    create_language_row_store_with(&configured_modules()?.runtime)
}

fn locale_service(runtime: LocaleRuntime, locales: &JsValue) -> Result<JsValue, JsValue> {
    type SnapshotCache = Rc<RefCell<Option<(Rc<LocaleSnapshot>, JsValue)>>>;
    let cache: SnapshotCache = Rc::new(RefCell::new(None));
    let snapshot_value = {
        let runtime = runtime.clone();
        let locales = locales.clone();
        let cache = cache.clone();
        move || -> Result<JsValue, JsValue> {
            let snapshot = runtime.snapshot();
            if let Some((current, value)) = cache.borrow().as_ref()
                && Rc::ptr_eq(current, &snapshot)
            {
                return Ok(value.clone());
            }
            let value = snapshot_to_js(&snapshot, &locales)?;
            *cache.borrow_mut() = Some((snapshot, value.clone()));
            Ok(value)
        }
    };
    let getter: Rc<dyn Fn() -> Result<JsValue, JsValue>> = Rc::new(snapshot_value);
    let service = Object::new();
    let locale_getter = getter.clone();
    let get_locale = Closure::wrap(
        Box::new(move || locale_getter()) as Box<dyn FnMut() -> Result<JsValue, JsValue>>
    );
    set(&service, "getLocale", &get_locale.into_js_value())?;
    let get_snapshot =
        Closure::wrap(Box::new(move || getter()) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    set(&service, "getSnapshot", &get_snapshot.into_js_value())?;

    let subscribe_runtime = runtime.clone();
    let subscribe = Closure::wrap(Box::new(move |listener: Function| -> Function {
        let subscription = subscribe_runtime.subscribe(Rc::new(move || {
            if let Err(error) = listener.call0(&JsValue::UNDEFINED) {
                web_sys::console::error_2(&JsValue::from_str("locale subscriber crashed:"), &error);
            }
        }));
        Closure::wrap(Box::new(move || subscription.dispose()) as Box<dyn FnMut()>)
            .into_js_value()
            .unchecked_into()
    }) as Box<dyn FnMut(Function) -> Function>);
    set(&service, "subscribe", &subscribe.into_js_value())?;

    let setter_runtime = runtime.clone();
    let set_locale = Closure::wrap(Box::new(move |id: String| -> Result<(), JsValue> {
        setter_runtime
            .set_locale(&id)
            .map_err(|error| js_error(&error))
    }) as Box<dyn FnMut(String) -> Result<(), JsValue>>);
    set(&service, "setLocale", &set_locale.into_js_value())?;

    let bound = Rc::new(RefCell::new(HashMap::<String, Function>::new()));
    let bind_runtime = runtime.clone();
    let bind = Closure::wrap(Box::new(move |namespace: String| -> Function {
        if let Some(function) = bound.borrow().get(&namespace) {
            return function.clone();
        }
        let translate = bind_runtime.bind(namespace.clone());
        let function: Function =
            Closure::wrap(Box::new(move |key: String, parameters: JsValue| -> String {
                let decoded = if parameters.is_undefined() || parameters.is_null() {
                    None
                } else {
                    serde_wasm_bindgen::from_value::<TranslateParameters>(parameters).ok()
                };
                translate(&key, decoded.as_ref())
            }) as Box<dyn FnMut(String, JsValue) -> String>)
            .into_js_value()
            .unchecked_into();
        bound.borrow_mut().insert(namespace, function.clone());
        function
    }) as Box<dyn FnMut(String) -> Function>);
    set(&service, "bind", &bind.into_js_value())?;

    let register_runtime = runtime;
    let register = Closure::wrap(Box::new(
        move |namespace: String, locale_or_dicts: JsValue, dictionary: JsValue| {
            let registration = if let Some(locale) = locale_or_dicts.as_string() {
                register_runtime
                    .register(namespace, locale, decode_dictionary(dictionary)?)
                    .map_err(|error| js_error(&error))?
            } else {
                let object = Object::from(locale_or_dicts);
                let mut dictionaries = Vec::new();
                for locale in Object::keys(&object)
                    .iter()
                    .filter_map(|value| value.as_string())
                {
                    dictionaries.push((
                        locale.clone(),
                        decode_dictionary(Reflect::get(&object, &JsValue::from_str(&locale))?)?,
                    ));
                }
                register_runtime
                    .register_many(namespace, dictionaries)
                    .map_err(|error| js_error(&error))?
            };
            let dispose =
                Closure::wrap(Box::new(move || registration.dispose()) as Box<dyn FnMut()>);
            Ok::<JsValue, JsValue>(dispose.into_js_value())
        },
    )
        as Box<dyn FnMut(String, JsValue, JsValue) -> Result<JsValue, JsValue>>);
    set(&service, "register", &register.into_js_value())?;
    Ok(service.into())
}

fn create_language_row_store_with(runtime: &JsValue) -> Result<JsValue, JsValue> {
    let init = Closure::wrap(Box::new(move || {
        object(&[
            ("active", JsValue::from_str("")),
            ("options", Array::new().into()),
            ("revision", JsValue::from_f64(-1.0)),
        ])
        .map(Into::into)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let sync = Closure::wrap(Box::new(
        move |draft: JsValue, active: String, options: JsValue, revision: f64| {
            let current = required(&draft, "revision")?.as_f64().unwrap_or(-1.0);
            if revision <= current {
                return Ok(());
            }
            Reflect::set(
                &draft,
                &JsValue::from_str("active"),
                &JsValue::from_str(&active),
            )?;
            Reflect::set(&draft, &JsValue::from_str("options"), &options)?;
            Reflect::set(
                &draft,
                &JsValue::from_str("revision"),
                &JsValue::from_f64(revision),
            )?;
            Ok::<(), JsValue>(())
        },
    )
        as Box<dyn FnMut(JsValue, String, JsValue, f64) -> Result<(), JsValue>>);
    let actions = object(&[("sync", sync.into_js_value())])?;
    let declaration = object(&[("init", init.into_js_value()), ("actions", actions.into())])?;
    call_method(runtime, "defineStore", &[declaration.into()])
}

fn language_row_component(modules: &BrowserModules) -> JsValue {
    let ui = ReactUi {
        react: modules.react.clone(),
        primitives: modules.primitives.clone(),
    };
    let component = Closure::wrap(
        Box::new(move |props: JsValue| render_language_row(&ui, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    );
    component.into_js_value()
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
        self.element(&required(&self.primitives, name)?, props, children)
    }
}

#[allow(clippy::too_many_lines)]
fn render_language_row(ui: &ReactUi, props: &JsValue) -> Result<JsValue, JsValue> {
    let use_store = function(props, "useStore")?;
    let active_selector = Closure::wrap(Box::new(move |state: JsValue| required(&state, "active"))
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    let active = use_store
        .call1(props, &active_selector.into_js_value())?
        .as_string()
        .unwrap_or_default();
    let options_selector =
        Closure::wrap(Box::new(move |state: JsValue| required(&state, "options"))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    let options = Array::from(&use_store.call1(props, &options_selector.into_js_value())?);
    let state = Array::from(&function(&ui.react, "useState")?.call1(&ui.react, &JsValue::FALSE)?);
    let open = state.get(0).as_bool().unwrap_or(false);
    let set_open = state.get(1).dyn_into::<Function>()?;
    let mut active_label = active.clone();
    let items = Array::new();
    for option in options.iter() {
        let id = required_string(&option, "id")?;
        let label = required_string(&option, "label")?;
        if id == active {
            active_label.clone_from(&label);
        }
        items.push(
            &object(&[
                ("id", JsValue::from_str(&id)),
                ("label", JsValue::from_str(&label)),
            ])?
            .into(),
        );
    }
    let title = function(props, "t")?.call1(props, &JsValue::from_str("language.title"))?;
    let title = ui.tag(
        "div",
        Some(&class_props("seekdeep-locale-title")?),
        &[title],
    )?;
    let row_text = ui.tag(
        "div",
        Some(&class_props("seekdeep-locale-row-text")?),
        &[title],
    )?;
    let icon = ui.primitive(
        "IconChevronDownOutline14",
        Some(&class_props("seekdeep-locale-chevron")?),
        &[],
    )?;
    let toggle_setter = set_open.clone();
    let toggle = Closure::wrap(Box::new(move || {
        let setter = toggle_setter.clone();
        let invert =
            Closure::wrap(Box::new(move |value: bool| !value) as Box<dyn FnMut(bool) -> bool>);
        let _ = setter.call1(&JsValue::UNDEFINED, &invert.into_js_value());
    }) as Box<dyn FnMut()>);
    let button = ui.tag(
        "button",
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            ("className", JsValue::from_str("seekdeep-locale-selector")),
            ("aria-haspopup", JsValue::from_str("menu")),
            ("aria-expanded", JsValue::from_bool(open)),
            ("onClick", toggle.into_js_value()),
        ])?),
        &[JsValue::from_str(&active_label), icon],
    )?;
    let close_setter = set_open.clone();
    let close = Closure::wrap(Box::new(move || {
        let _ = close_setter.call1(&JsValue::UNDEFINED, &JsValue::FALSE);
    }) as Box<dyn FnMut()>);
    let selection = function(props, "setLocale")?;
    let select = Closure::wrap(Box::new(move |id: String| {
        let _ = selection.call1(&JsValue::UNDEFINED, &JsValue::from_str(&id));
        let _ = set_open.call1(&JsValue::UNDEFINED, &JsValue::FALSE);
    }) as Box<dyn FnMut(String)>);
    let menu = ui.primitive(
        "Menu",
        Some(&object(&[
            ("open", JsValue::from_bool(open)),
            ("onClose", close.into_js_value()),
            ("items", items.into()),
            ("selectedId", JsValue::from_str(&active)),
            ("onSelect", select.into_js_value()),
            ("align", JsValue::from_str("end")),
            ("portal", JsValue::TRUE),
            ("anchor", button),
        ])?),
        &[],
    )?;
    ui.tag(
        "div",
        Some(&class_props("seekdeep-locale-row")?),
        &[row_text, menu],
    )
}

fn js_host_scope(scope: JsValue) -> LocaleHostScope {
    let snapshot_scope = scope.clone();
    let setter_scope = scope.clone();
    let subscriber_scope = scope;
    LocaleHostScope {
        snapshot: Rc::new(move || {
            let snapshot = call_method(&snapshot_scope, "getSnapshot", &[]).ok()?;
            let value = required(&snapshot, "value").ok()?;
            if value.is_undefined() {
                return None;
            }
            let preference = Reflect::get(&value, &JsValue::from_str(LOCALE_PREFERENCE_FIELD))
                .ok()
                .and_then(|value| value.as_string())
                .and_then(|value| LocaleId::parse(&value));
            Some(LocaleSettings { preference })
        }),
        set_preference: Rc::new(move |locale| {
            let _ = call_method(
                &setter_scope,
                "set",
                &[
                    JsValue::from_str(LOCALE_PREFERENCE_FIELD),
                    JsValue::from_str(locale.as_str()),
                ],
            );
        }),
        subscribe: Rc::new(move |listener| {
            let callback = Closure::wrap(Box::new(move || listener()) as Box<dyn FnMut()>);
            let disposer = call_method(&subscriber_scope, "subscribe", &[callback.into_js_value()])
                .ok()
                .and_then(|value| value.dyn_into::<Function>().ok());
            LocaleDisposer::new(move || {
                if let Some(disposer) = disposer {
                    let _ = disposer.call0(&JsValue::UNDEFINED);
                }
            })
        }),
    }
}

fn browser_locale() -> Option<LocaleId> {
    let global = js_sys::global();
    if Reflect::get(&global, &JsValue::from_str("window"))
        .ok()
        .is_none_or(|window| window.is_undefined())
    {
        return None;
    }
    let navigator = Reflect::get(&global, &JsValue::from_str("navigator")).ok()?;
    let languages = Reflect::get(&navigator, &JsValue::from_str("languages"))
        .ok()
        .filter(|value| !value.is_undefined())
        .map(|value| {
            Array::from(&value)
                .iter()
                .filter_map(|value| value.as_string())
                .collect::<Vec<_>>()
        });
    let language = Reflect::get(&navigator, &JsValue::from_str("language"))
        .ok()
        .and_then(|value| value.as_string())
        .unwrap_or_default();
    detect_browser_locale(true, languages.as_deref(), &language)
}

fn sync_actions(actions: &JsValue, snapshot: &JsValue) -> Result<(), JsValue> {
    let source = Array::from(&required(snapshot, "locales")?);
    let options = Array::new();
    for locale in source.iter() {
        options.push(
            &object(&[
                ("id", required(&locale, "id")?),
                ("label", required(&locale, "label")?),
            ])?
            .into(),
        );
    }
    call_method(
        actions,
        "sync",
        &[
            required(snapshot, "active")?,
            options.into(),
            required(snapshot, "revision")?,
        ],
    )?;
    Ok(())
}

fn snapshot_to_js(snapshot: &LocaleSnapshot, locales: &JsValue) -> Result<JsValue, JsValue> {
    let snapshot = object(&[
        ("active", JsValue::from_str(snapshot.active.as_str())),
        ("locales", locales.clone()),
        ("revision", js_number(snapshot.revision)),
    ])?;
    Object::freeze(&snapshot);
    Ok(snapshot.into())
}

fn locale_definitions_js() -> Result<JsValue, JsValue> {
    let locales = Array::new();
    for (id, label) in [("zh", "中文"), ("en", "English")] {
        locales.push(
            &object(&[
                ("id", JsValue::from_str(id)),
                ("label", JsValue::from_str(label)),
            ])?
            .into(),
        );
    }
    Object::freeze(&locales);
    Ok(locales.into())
}

fn decode_dictionary(value: JsValue) -> Result<LocaleDictionary, JsValue> {
    serde_wasm_bindgen::from_value(value)
        .map_err(|error| js_sys::Error::new(&error.to_string()).into())
}

fn configured_modules() -> Result<BrowserModules, JsValue> {
    MODULES.with(|modules| {
        modules.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-locale module factory did not configure shell modules")
                .into()
        })
    })
}

fn inject_styles() -> Result<(), JsValue> {
    let document = required(&js_sys::global(), "document")?;
    let style = call_method(&document, "createElement", &[JsValue::from_str("style")])?;
    call_method(
        &style,
        "setAttribute",
        &[
            JsValue::from_str("data-plugin"),
            JsValue::from_str("@seekdeep-ai/seekdeep-client-locale"),
        ],
    )?;
    Reflect::set(
        &style,
        &JsValue::from_str("textContent"),
        &JsValue::from_str(STYLES),
    )?;
    call_method(&required(&document, "head")?, "appendChild", &[style])?;
    Ok(())
}

fn required_service(ctx: &JsValue, name: &str) -> Result<JsValue, JsValue> {
    let value = call_method(ctx, "get", &[JsValue::from_str(name)])?;
    if value.is_undefined() {
        Err(js_sys::Error::new(&format!("client-locale requires Client Service {name:?}")).into())
    } else {
        Ok(value)
    }
}

fn required(value: &JsValue, key: &str) -> Result<JsValue, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_undefined() {
        Err(js_sys::Error::new(&format!("missing property {key:?}")).into())
    } else {
        Ok(property)
    }
}

fn required_string(value: &JsValue, key: &str) -> Result<String, JsValue> {
    required(value, key)?
        .as_string()
        .ok_or_else(|| js_sys::Error::new(&format!("property {key:?} must be a string")).into())
}

fn function(value: &JsValue, key: &str) -> Result<Function, JsValue> {
    required(value, key)?.dyn_into::<Function>()
}

fn class_props(class_name: &str) -> Result<Object, JsValue> {
    object(&[("className", JsValue::from_str(class_name))])
}

fn object(properties: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let object = Object::new();
    for (key, value) in properties {
        set(&object, key, value)?;
    }
    Ok(object)
}

fn set(object: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    Reflect::set(object, &JsValue::from_str(key), value)?;
    Ok(())
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = function(value, name)?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    method.apply(value, &args)
}

fn js_number(value: u64) -> JsValue {
    JsValue::from_f64(
        value
            .to_string()
            .parse()
            .expect("u64 decimal text is a finite JavaScript number"),
    )
}

fn js_error(error: &anyhow::Error) -> JsValue {
    js_sys::Error::new(&error.to_string()).into()
}

/// Exact Client plugin inject list exposed for built-module verification.
#[wasm_bindgen(js_name = localeInject)]
pub fn locale_inject() -> Array {
    let values = Array::new();
    for dependency in INJECT {
        values.push(&JsValue::from_str(dependency));
    }
    values
}
