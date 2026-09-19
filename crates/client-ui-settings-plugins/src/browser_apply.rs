//! Browser Cordis and Slot assembly for plugin Settings.

use std::{cell::RefCell, cmp::Ordering, rc::Rc};

use js_sys::{Array, Function, JSON, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{
    browser::{call_method, object, optional, required},
    browser_components::{configured_components, configured_modules},
    browser_controllers::BrowserCardControllers,
};

const NS: &str = "settings.plugins";
const INJECT: &[&str] = &["slots", "locale", "connection", "remote", "settingsScope"];
const LOCALES: &str = include_str!("../data/locales.json");

#[derive(Clone)]
struct TabRow {
    id: String,
    order: f64,
    label: String,
}

#[derive(Default)]
struct TabCache {
    version: f64,
    revision: f64,
    initialized: bool,
    value: JsValue,
}

/// Registers the Plugins section, configurable tab, and three Host-plane cards.
///
/// # Errors
///
/// Returns for missing services, malformed locale/Slot faces, Settings binding failures, or
/// registration failures.
#[wasm_bindgen(js_name = applyClientUiSettingsPlugins)]
#[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
pub fn apply_client_ui_settings_plugins(ctx: JsValue) -> Result<(), JsValue> {
    let modules = configured_modules()?;
    let components = configured_components()?;
    let slots = required(&ctx, "slots", "Client Context")?;
    let locale = required(&ctx, "locale", "Client Context")?;
    let remote = required(&ctx, "remote", "Client Context")?;
    let settings_scope = required(&ctx, "settingsScope", "Client Context")?;
    let connection = call_method(&ctx, "get", &[JsValue::from_str("connection")])?;
    let api = required(&connection, "api", "Connection handle")?;
    let translate =
        call_method(&locale, "bind", &[JsValue::from_str(NS)])?.dyn_into::<Function>()?;
    let encoded_dictionaries = JSON::parse(LOCALES)?;
    let dictionaries: JsValue = object(&[
        (
            "zh",
            required(&encoded_dictionaries, "zh", "locale dictionaries")?,
        ),
        (
            "en",
            required(&encoded_dictionaries, "en", "locale dictionaries")?,
        ),
    ])?
    .into();
    let locale_owner = locale.clone();
    let install_locale = Closure::wrap(Box::new(move || {
        call_method(
            &locale_owner,
            "register",
            &[JsValue::from_str(NS), dictionaries.clone()],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        &ctx,
        "effect",
        &[
            install_locale.into_js_value(),
            JsValue::from_str("ui-settings-plugins: section dictionaries"),
        ],
    )?;

    let controllers = Rc::new(BrowserCardControllers::new(&settings_scope, &api)?);
    own_credential_invalidations(&ctx, &remote, &controllers)?;

    let tab_cache = Rc::new(RefCell::new(TabCache::default()));
    let section_slots = slots.clone();
    let section_locale = locale.clone();
    let section_label = modules.resolve_slot_label;
    let section_cache = tab_cache;
    let section_inject = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let tabs = tabs_source(
            &section_slots,
            &section_locale,
            &section_label,
            section_cache.clone(),
        )?;
        Ok(object(&[("hooks", object(&[("tabs", tabs)])?.into())])?.into())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let section_translate = translate.clone();
    let section_label = Closure::wrap(Box::new(move || {
        section_translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("nav"))
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    inject_registration(
        &slots,
        "settings.section",
        object(&[
            ("name", JsValue::from_str("settings.section")),
            ("id", JsValue::from_str("plugins")),
            ("order", JsValue::from_f64(15.0)),
            ("label", section_label.into_js_value()),
            ("locale", JsValue::from_str(NS)),
            ("inject", section_inject.into_js_value()),
            (
                "children",
                object(&[("settings.plugins.tab", slot_spec("list", "root")?.into())])?.into(),
            ),
        ])?,
        components.section.clone(),
    )?;

    let tab_slots = slots.clone();
    let tab_inject = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let entries = call_method(
            &tab_slots,
            "entries",
            &[JsValue::from_str("settings.plugin.item")],
        )?;
        Ok(object(&[(
            "cardCount",
            JsValue::from_f64(f64::from(Array::from(&entries).length())),
        )])?
        .into())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let tab_translate = translate.clone();
    let tab_label = Closure::wrap(Box::new(move || {
        tab_translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("configurableTab"))
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    inject_registration(
        &slots,
        "settings.plugins.tab",
        object(&[
            ("name", JsValue::from_str("settings.plugins.tab")),
            ("id", JsValue::from_str("configurable")),
            ("order", JsValue::from_f64(0.0)),
            ("label", tab_label.into_js_value()),
            ("locale", JsValue::from_str(NS)),
            ("inject", tab_inject.into_js_value()),
            (
                "children",
                object(&[("settings.plugin.item", slot_spec("list", "root")?.into())])?.into(),
            ),
        ])?,
        components.configurable_tab.clone(),
    )?;

    inject_cards(&slots, controllers, &components)
}

/// Exact Client service dependencies.
#[wasm_bindgen(js_name = settingsPluginsInject)]
pub fn settings_plugins_inject() -> Array {
    let output = Array::new();
    for value in INJECT {
        output.push(&JsValue::from_str(value));
    }
    output
}

fn slot_spec(kind: &str, scope: &str) -> Result<Object, JsValue> {
    object(&[
        ("kind", JsValue::from_str(kind)),
        ("scope", JsValue::from_str(scope)),
    ])
}

fn own_credential_invalidations(
    ctx: &JsValue,
    remote: &JsValue,
    controllers: &Rc<BrowserCardControllers>,
) -> Result<(), JsValue> {
    let remote = remote.clone();
    let web_search = controllers.web_search.clone();
    let installer = Closure::wrap(Box::new(move || {
        let controller = web_search.clone();
        let listener = Closure::wrap(Box::new(move |reference: String| {
            controller.refresh_credential(&reference);
        }) as Box<dyn FnMut(String)>);
        call_method(
            &remote,
            "$on",
            &[
                JsValue::from_str("credentials/updated"),
                listener.into_js_value(),
            ],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        ctx,
        "effect",
        &[
            installer.into_js_value(),
            JsValue::from_str("ui-settings-plugins: credential invalidations"),
        ],
    )?;
    Ok(())
}

fn inject_registration(
    slots: &JsValue,
    declaration: &str,
    options: Object,
    component: JsValue,
) -> Result<(), JsValue> {
    let registration_slots = slots.clone();
    let install = Closure::wrap(Box::new(move || {
        call_method(
            &registration_slots,
            "register",
            &[options.clone().into(), component.clone()],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        slots,
        "inject",
        &[JsValue::from_str(declaration), install.into_js_value()],
    )?;
    Ok(())
}

fn inject_cards(
    slots: &JsValue,
    controllers: Rc<BrowserCardControllers>,
    components: &crate::browser_components::Components,
) -> Result<(), JsValue> {
    let registration_slots = slots.clone();
    let components = components.clone();
    let install = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let mut owned = Vec::<Function>::new();
        for (id, order, inject, component) in [
            (
                "bash",
                0.0,
                controllers.bash_face()?,
                components.bash_card.clone(),
            ),
            (
                "agent-loop",
                10.0,
                controllers.agent_loop_face()?,
                components.agent_loop_card.clone(),
            ),
            (
                "web-search",
                20.0,
                controllers.web_search_face()?,
                components.web_search_card.clone(),
            ),
        ] {
            let face = inject.clone();
            let inject =
                Closure::wrap(Box::new(move || face.clone()) as Box<dyn FnMut() -> JsValue>);
            let registered = call_method(
                &registration_slots,
                "register",
                &[
                    object(&[
                        ("name", JsValue::from_str("settings.plugin.item")),
                        ("id", JsValue::from_str(id)),
                        ("order", JsValue::from_f64(order)),
                        ("locale", JsValue::from_str(NS)),
                        ("inject", inject.into_js_value()),
                    ])?
                    .into(),
                    component,
                ],
            )
            .and_then(wasm_bindgen::JsCast::dyn_into::<Function>);
            match registered {
                Ok(disposer) => owned.push(disposer),
                Err(error) => {
                    for disposer in owned.iter().rev() {
                        let _ = disposer.call0(&JsValue::UNDEFINED);
                    }
                    return Err(error);
                }
            }
        }
        Ok(Closure::wrap(Box::new(move || {
            for disposer in owned.iter().rev() {
                let _ = disposer.call0(&JsValue::UNDEFINED);
            }
        }) as Box<dyn FnMut()>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        slots,
        "inject",
        &[
            JsValue::from_str("settings.plugin.item"),
            install.into_js_value(),
        ],
    )?;
    Ok(())
}

#[allow(clippy::float_cmp, clippy::too_many_lines)] // Slot versions and locale revisions are exact integer-valued JS counters.
fn tabs_source(
    slots: &JsValue,
    locale: &JsValue,
    resolve_slot_label: &Function,
    cache: Rc<RefCell<TabCache>>,
) -> Result<JsValue, JsValue> {
    let source = Object::new();
    let snapshot_slots = slots.clone();
    let snapshot_locale = locale.clone();
    let resolve = resolve_slot_label.clone();
    let snapshot_cache = cache;
    let get_snapshot = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let version = call_method(
            &snapshot_slots,
            "getVersion",
            &[JsValue::from_str("settings.plugins.tab")],
        )?
        .as_f64()
        .unwrap_or(0.0);
        let locale_snapshot = call_method(&snapshot_locale, "getSnapshot", &[])?;
        let revision = required(&locale_snapshot, "revision", "locale snapshot")?
            .as_f64()
            .unwrap_or(0.0);
        let current = snapshot_cache.borrow();
        if current.initialized && current.version == version && current.revision == revision {
            return Ok(current.value.clone());
        }
        drop(current);
        let entries = Array::from(&call_method(
            &snapshot_slots,
            "entries",
            &[JsValue::from_str("settings.plugins.tab")],
        )?);
        let mut rows = Vec::new();
        for entry in entries.iter() {
            let options = required(&entry, "options", "Slot entry")?;
            let label = resolve.call1(
                &JsValue::UNDEFINED,
                &optional(&options, "label")?.unwrap_or(JsValue::UNDEFINED),
            )?;
            rows.push(TabRow {
                id: optional(&options, "id")?
                    .and_then(|value| value.as_string())
                    .unwrap_or_default(),
                order: optional(&options, "order")?
                    .and_then(|value| value.as_f64())
                    .unwrap_or(0.0),
                label: label.as_string().unwrap_or_default(),
            });
        }
        rows.sort_by(|left, right| {
            left.order
                .partial_cmp(&right.order)
                .unwrap_or(Ordering::Equal)
        });
        let value = Array::new();
        for row in rows {
            value.push(
                &object(&[
                    ("id", JsValue::from_str(&row.id)),
                    ("order", JsValue::from_f64(row.order)),
                    ("label", JsValue::from_str(&row.label)),
                ])?
                .into(),
            );
        }
        let value: JsValue = value.into();
        *snapshot_cache.borrow_mut() = TabCache {
            version,
            revision,
            initialized: true,
            value: value.clone(),
        };
        Ok(value)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    Reflect::set(
        &source,
        &JsValue::from_str("getSnapshot"),
        &get_snapshot.into_js_value(),
    )?;
    let subscribe_slots = slots.clone();
    let subscribe_locale = locale.clone();
    let subscribe = Closure::wrap(
        Box::new(move |listener: Function| -> Result<Function, JsValue> {
            let off_ledger = call_method(
                &subscribe_slots,
                "subscribe",
                &[
                    JsValue::from_str("settings.plugins.tab"),
                    listener.clone().into(),
                ],
            )?
            .dyn_into::<Function>()?;
            let off_locale = call_method(&subscribe_locale, "subscribe", &[listener.into()])?
                .dyn_into::<Function>()?;
            Ok(Closure::wrap(Box::new(move || -> Result<(), JsValue> {
                off_ledger.call0(&JsValue::UNDEFINED)?;
                off_locale.call0(&JsValue::UNDEFINED)?;
                Ok(())
            }) as Box<dyn FnMut() -> Result<(), JsValue>>)
            .into_js_value()
            .unchecked_into())
        }) as Box<dyn FnMut(Function) -> Result<Function, JsValue>>,
    );
    Reflect::set(
        &source,
        &JsValue::from_str("subscribe"),
        &subscribe.into_js_value(),
    )?;
    Ok(source.into())
}
