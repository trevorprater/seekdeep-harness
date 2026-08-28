//! Browser plugin-inventory settings tab and Client registration.

use std::{cell::Cell, cell::RefCell, collections::BTreeSet, rc::Rc};

use js_sys::{Array, Function, Object, Promise, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, future_to_promise, spawn_local};

use crate::{
    EN, LOCALE_NAMESPACE, PLUGIN_INVENTORY_STYLES, PluginInventoryEntry, PluginInventorySnapshot,
    ZH, module_short_name, phase_locale_key, remote_list_error,
};

const INJECT: &[&str] = &["slots", "locale", "remote", "remote.pluginInventory"];

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    chevron_down: JsValue,
    search_icon: JsValue,
}

/// Configures React, UI primitives, and the compiled stylesheet.
///
/// # Errors
///
/// Returns missing primitive or DOM stylesheet-injection failures.
#[wasm_bindgen(js_name = configureClientUiSettingsPluginInventory)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_settings_plugin_inventory(
    react: JsValue,
    primitives: JsValue,
) -> Result<(), JsValue> {
    MODULES.with(|modules| {
        *modules.borrow_mut() = Some(BrowserModules {
            react,
            chevron_down: required(&primitives, "IconChevronDownOutline14", "UI primitives")?,
            search_icon: required(&primitives, "IconSearchOutline16", "UI primitives")?,
        });
        Ok::<_, JsValue>(())
    })?;
    inject_styles()
}

/// Applies the plugin-inventory Settings browser plugin.
///
/// # Errors
///
/// Returns missing Slot, locale, Remote, registration, or component failures.
#[wasm_bindgen(js_name = applyClientUiSettingsPluginInventory)]
#[allow(clippy::needless_pass_by_value)]
pub fn apply_client_ui_settings_plugin_inventory(ctx: JsValue) -> Result<(), JsValue> {
    let modules = configured_modules()?;
    let slots = required(&ctx, "slots", "Client Context")?;
    let locale = required(&ctx, "locale", "Client Context")?;
    let remote = required(&ctx, "remote", "Client Context")?;
    required(&ctx, "remote.pluginInventory", "Client Context")?;
    let inventory = required(&remote, "pluginInventory", "Remote")?;
    own_locale_dictionaries(&ctx, &locale)?;
    let translate = call_method(&locale, "bind", &[JsValue::from_str(LOCALE_NAMESPACE)])?
        .dyn_into::<Function>()?;

    let list_inventory = inventory;
    let list = Closure::wrap(Box::new(move || -> Promise {
        let inventory = list_inventory.clone();
        future_to_promise(async move {
            let returned = call_method(&inventory, "list", &[])?;
            let result = JsFuture::from(Promise::resolve(&returned)).await?;
            if !required_bool(&result, "ok", "pluginInventory.list result")? {
                let error = required(&result, "error", "pluginInventory.list result")?;
                let code = required_string(&error, "code", "pluginInventory.list error")?;
                let message = required_string(&error, "message", "pluginInventory.list error")?;
                return Err(js_sys::Error::new(&remote_list_error(&code, &message)).into());
            }
            required(&result, "value", "pluginInventory.list result")
        })
    }) as Box<dyn FnMut() -> Promise>)
    .into_js_value();

    let component = plugin_inventory_component(&modules);
    let registration_slots = slots.clone();
    let installer_translate = translate;
    let installer = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let label_translate = installer_translate.clone();
        let label = Closure::wrap(Box::new(move || {
            label_translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("tab"))
        }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
        let injected_list = list.clone();
        let inject = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
            object(&[("list", injected_list.clone())]).map(Into::into)
        }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
        let options = object(&[
            ("name", JsValue::from_str("settings.plugins.tab")),
            ("id", JsValue::from_str("all")),
            ("order", JsValue::from_f64(10.0)),
            ("label", label.into_js_value()),
            ("locale", JsValue::from_str(LOCALE_NAMESPACE)),
            ("inject", inject.into_js_value()),
        ])?;
        call_method(
            &registration_slots,
            "register",
            &[options.into(), component.clone()],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        &slots,
        "inject",
        &[
            JsValue::from_str("settings.plugins.tab"),
            installer.into_js_value(),
        ],
    )?;
    Ok(())
}

/// Returns the exact browser dependency order.
#[wasm_bindgen(js_name = settingsPluginInventoryInject)]
pub fn settings_plugin_inventory_inject() -> Array {
    let values = Array::new();
    for dependency in INJECT {
        values.push(&JsValue::from_str(dependency));
    }
    values
}

/// Returns the compiled inventory Settings component.
///
/// # Errors
///
/// Returns before browser modules are configured.
#[wasm_bindgen(js_name = pluginInventorySettingsTabComponent)]
pub fn exported_plugin_inventory_settings_tab_component() -> Result<JsValue, JsValue> {
    Ok(plugin_inventory_component(&configured_modules()?))
}

fn plugin_inventory_component(modules: &BrowserModules) -> JsValue {
    let modules = modules.clone();
    Closure::wrap(
        Box::new(move |props: JsValue| render_plugin_inventory(&modules, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value()
}

#[allow(clippy::too_many_lines)]
fn render_plugin_inventory(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let catalog_id = required_function(&modules.react, "useId", "React")?
        .call0(&modules.react)?
        .as_string()
        .ok_or_else(|| js_sys::Error::new("React useId must return a string"))?;
    let (request, set_request) = use_state(&modules.react, &JsValue::from_f64(0.0))?;
    let (query, set_query) = use_state(&modules.react, &JsValue::from_str(""))?;
    let (expanded, set_expanded) = use_state(&modules.react, &JsValue::NULL)?;
    let (state, set_state) = use_state(&modules.react, &loading_state()?.into())?;
    let list = required_function(props, "list", "PluginInventorySettingsTab")?;
    let translate = required_function(props, "t", "PluginInventorySettingsTab")?;
    install_load_effect(&modules.react, &list, &request, &set_state)?;

    let status = required_string(&state, "status", "inventory view state")?;
    let query = query
        .as_string()
        .ok_or_else(|| js_sys::Error::new("plugin inventory query must be a string"))?;
    let normalized_query = locale_lower(query.trim())?;
    let snapshot = if status == "ready" {
        Some(
            serde_wasm_bindgen::from_value::<PluginInventorySnapshot>(required(
                &state,
                "snapshot",
                "inventory ready state",
            )?)
            .map_err(js_error_from_display)?,
        )
    } else {
        None
    };
    let filtered = snapshot.as_ref().map_or_else(Vec::new, |snapshot| {
        snapshot
            .entries
            .iter()
            .filter(|entry| matches_entry(entry, &normalized_query))
            .cloned()
            .collect::<Vec<_>>()
    });
    install_expansion_effect(&modules.react, &expanded, &filtered, &set_expanded)?;

    let mut children = Vec::new();
    match status.as_str() {
        "loading" => children.push(status_paragraph(modules, &translate, "loading")?),
        "error" => children.push(render_failure(
            modules,
            &translate,
            &set_state,
            &set_request,
        )?),
        "ready" => children.push(render_catalog(
            modules,
            snapshot.as_ref().expect("ready snapshot"),
            &filtered,
            &query,
            &set_query,
            &expanded,
            &set_expanded,
            &catalog_id,
            &translate,
        )?),
        _ => {
            return Err(js_sys::Error::new(&format!(
                "inventory view status {status:?} is invalid"
            ))
            .into());
        }
    }
    tag(
        &modules.react,
        "div",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-plugin-inventory-section"),
            ),
            ("aria-busy", JsValue::from_bool(status == "loading")),
        ])?),
        &children,
    )
}

fn install_load_effect(
    react: &JsValue,
    list: &Function,
    request: &JsValue,
    set_state: &Function,
) -> Result<(), JsValue> {
    let list = list.clone();
    let dependency_list = list.clone();
    let setter = set_state.clone();
    let current = Rc::new(Cell::new(true));
    let effect_current = current.clone();
    let effect = Closure::wrap(Box::new(move || -> JsValue {
        effect_current.set(true);
        let list = list.clone();
        let setter = setter.clone();
        let task_current = effect_current.clone();
        spawn_local(async move {
            let _ = JsFuture::from(Promise::resolve(&JsValue::UNDEFINED)).await;
            let settled = match list.call0(&JsValue::UNDEFINED) {
                Ok(returned) => JsFuture::from(Promise::resolve(&returned)).await,
                Err(error) => Err(error),
            };
            if !task_current.get() {
                return;
            }
            let next = match settled {
                Ok(snapshot) => ready_state(&snapshot).map(Into::into),
                Err(_) => error_state().map(Into::into),
            };
            if let Ok(next) = next {
                let _ = setter.call1(&JsValue::UNDEFINED, &next);
            }
        });
        let cleanup_current = effect_current.clone();
        Closure::wrap(Box::new(move || cleanup_current.set(false)) as Box<dyn FnMut()>)
            .into_js_value()
    }) as Box<dyn FnMut() -> JsValue>);
    use_effect(
        react,
        &effect.into_js_value(),
        &Array::of2(dependency_list.as_ref(), request),
    )
}

fn install_expansion_effect(
    react: &JsValue,
    expanded: &JsValue,
    filtered: &[PluginInventoryEntry],
    set_expanded: &Function,
) -> Result<(), JsValue> {
    let expanded = expanded.clone();
    let dependency_expanded = expanded.clone();
    let ids = filtered
        .iter()
        .map(|entry| entry.entry_id.clone())
        .collect::<BTreeSet<_>>();
    let setter = set_expanded.clone();
    let dependency = JsValue::from_str(
        &filtered
            .iter()
            .map(|entry| entry.entry_id.as_str())
            .collect::<Vec<_>>()
            .join("\0"),
    );
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if let Some(id) = expanded.as_string()
            && !ids.contains(&id)
        {
            setter.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
        }
        Ok(JsValue::UNDEFINED)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    use_effect(
        react,
        &effect.into_js_value(),
        &Array::of2(dependency_expanded.as_ref(), &dependency),
    )
}

fn render_failure(
    modules: &BrowserModules,
    translate: &Function,
    set_state: &Function,
    set_request: &Function,
) -> Result<JsValue, JsValue> {
    let loading_setter = set_state.clone();
    let request_setter = set_request.clone();
    let retry = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        loading_setter.call1(&JsValue::UNDEFINED, &loading_state()?.into())?;
        let increment =
            Closure::wrap(Box::new(|value: f64| value + 1.0) as Box<dyn FnMut(f64) -> f64>);
        request_setter.call1(&JsValue::UNDEFINED, &increment.into_js_value())?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let alert = tag(
        &modules.react,
        "p",
        Some(&object(&[("role", JsValue::from_str("alert"))])?),
        &[translated(translate, "error")?],
    )?;
    let button = tag(
        &modules.react,
        "button",
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            ("onClick", retry.into_js_value()),
        ])?),
        &[translated(translate, "retry")?],
    )?;
    tag(
        &modules.react,
        "div",
        Some(&class("seekdeep-plugin-inventory-failure")?),
        &[alert, button],
    )
}

#[allow(clippy::too_many_arguments)]
fn render_catalog(
    modules: &BrowserModules,
    snapshot: &PluginInventorySnapshot,
    filtered: &[PluginInventoryEntry],
    query: &str,
    set_query: &Function,
    expanded: &JsValue,
    set_expanded: &Function,
    catalog_id: &str,
    translate: &Function,
) -> Result<JsValue, JsValue> {
    let search = render_search(modules, query, set_query, translate)?;
    let heading = tag(
        &modules.react,
        "div",
        Some(&class("seekdeep-plugin-inventory-catalogHeading")?),
        &[
            tag(
                &modules.react,
                "h3",
                None,
                &[translated(translate, "catalog")?],
            )?,
            tag(
                &modules.react,
                "span",
                Some(&object(&[(
                    "data-plugin-count",
                    JsValue::from_f64(usize_as_f64(filtered.len())),
                )])?),
                &[JsValue::from_str(&filtered.len().to_string())],
            )?,
        ],
    )?;
    let mut children = vec![search, heading];
    if snapshot.entries.is_empty() {
        children.push(status_paragraph(modules, translate, "empty")?);
    } else if filtered.is_empty() {
        children.push(status_paragraph(modules, translate, "emptySearch")?);
    } else {
        let mut cards = Vec::new();
        for entry in filtered {
            cards.push(render_card(
                modules,
                entry,
                expanded,
                set_expanded,
                catalog_id,
                translate,
            )?);
        }
        children.push(tag(
            &modules.react,
            "ul",
            Some(&class("seekdeep-plugin-inventory-cards")?),
            &cards,
        )?);
    }
    tag(
        &modules.react,
        "div",
        Some(&class("seekdeep-plugin-inventory-catalog")?),
        &children,
    )
}

fn render_search(
    modules: &BrowserModules,
    query: &str,
    set_query: &Function,
    translate: &Function,
) -> Result<JsValue, JsValue> {
    let setter = set_query.clone();
    let change = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        let current = required(&event, "currentTarget", "search change event")?;
        let value = required_string(&current, "value", "search input")?;
        setter.call1(&JsValue::UNDEFINED, &JsValue::from_str(&value))?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let label = translated(translate, "search")?;
    let icon = component(
        &modules.react,
        &modules.search_icon,
        Some(&object(&[("aria-hidden", JsValue::from_str("true"))])?),
        &[],
    )?;
    let hidden = tag(
        &modules.react,
        "span",
        Some(&class("seekdeep-plugin-inventory-visuallyHidden")?),
        std::slice::from_ref(&label),
    )?;
    let input = tag(
        &modules.react,
        "input",
        Some(&object(&[
            ("type", JsValue::from_str("search")),
            ("value", JsValue::from_str(query)),
            ("placeholder", label.clone()),
            ("aria-label", label),
            ("onChange", change.into_js_value()),
        ])?),
        &[],
    )?;
    tag(
        &modules.react,
        "label",
        Some(&class("seekdeep-plugin-inventory-search")?),
        &[icon, hidden, input],
    )
}

fn render_card(
    modules: &BrowserModules,
    entry: &PluginInventoryEntry,
    expanded: &JsValue,
    set_expanded: &Function,
    catalog_id: &str,
    translate: &Function,
) -> Result<JsValue, JsValue> {
    let status = translated(translate, phase_locale_key(entry.fiber_phase))?
        .as_string()
        .ok_or_else(|| js_sys::Error::new("plugin phase must translate to a string"))?;
    let title = module_short_name(&entry.module_name);
    let configuration = translated(
        translate,
        if entry.enabled {
            "enabledTag"
        } else {
            "disabledTag"
        },
    )?
    .as_string()
    .ok_or_else(|| js_sys::Error::new("plugin configuration must translate to a string"))?;
    let open = expanded.as_string().as_deref() == Some(entry.entry_id.as_str());
    let detail_id = format!(
        "{catalog_id}-details-{}",
        encode_component(&entry.entry_id)?
    );
    let toggle_id = entry.entry_id.clone();
    let toggle_setter = set_expanded.clone();
    let toggle = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let id = toggle_id.clone();
        let update = Closure::wrap(Box::new(move |current: JsValue| -> JsValue {
            if current.as_string().as_deref() == Some(id.as_str()) {
                JsValue::NULL
            } else {
                JsValue::from_str(&id)
            }
        }) as Box<dyn FnMut(JsValue) -> JsValue>);
        toggle_setter.call1(&JsValue::UNDEFINED, &update.into_js_value())?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let aria = if entry.enabled {
        format!("{title}, {status}, {configuration}")
    } else {
        format!("{title}, {configuration}")
    };
    let button = render_card_button(
        modules,
        entry,
        &title,
        &status,
        &configuration,
        open,
        &detail_id,
        &aria,
        toggle.into_js_value(),
    )?;
    let mut children = vec![button];
    if open {
        children.push(render_details(
            modules,
            entry,
            &configuration,
            &status,
            &detail_id,
            translate,
        )?);
    }
    tag(
        &modules.react,
        "li",
        Some(&object(&[
            ("key", JsValue::from_str(&entry.entry_id)),
            (
                "className",
                JsValue::from_str("seekdeep-plugin-inventory-card"),
            ),
            ("data-plugin-entry", JsValue::from_str(&entry.entry_id)),
            (
                "data-open",
                if open {
                    JsValue::from_str("true")
                } else {
                    JsValue::UNDEFINED
                },
            ),
        ])?),
        &children,
    )
}

#[allow(clippy::too_many_arguments)]
fn render_card_button(
    modules: &BrowserModules,
    entry: &PluginInventoryEntry,
    title: &str,
    status: &str,
    configuration: &str,
    open: bool,
    detail_id: &str,
    aria: &str,
    toggle: JsValue,
) -> Result<JsValue, JsValue> {
    let title_node = tag(
        &modules.react,
        "strong",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-plugin-inventory-cardTitle"),
            ),
            ("title", JsValue::from_str(&entry.module_name)),
        ])?),
        &[JsValue::from_str(title)],
    )?;
    let mut trailing = Vec::new();
    if entry.enabled {
        trailing.push(tag(
            &modules.react,
            "span",
            Some(&object(&[
                (
                    "className",
                    JsValue::from_str("seekdeep-plugin-inventory-statusDot"),
                ),
                (
                    "data-phase",
                    JsValue::from_str(entry.fiber_phase.map_or("unobserved", phase_name)),
                ),
                ("role", JsValue::from_str("img")),
                ("aria-label", JsValue::from_str(status)),
                ("title", JsValue::from_str(status)),
            ])?),
            &[],
        )?);
    }
    trailing.push(tag(
        &modules.react,
        "span",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-plugin-inventory-configTag"),
            ),
            (
                "data-enabled",
                JsValue::from_str(if entry.enabled { "true" } else { "false" }),
            ),
        ])?),
        &[JsValue::from_str(configuration)],
    )?);
    trailing.push(component(
        &modules.react,
        &modules.chevron_down,
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-plugin-inventory-chevron"),
            ),
            ("size", JsValue::from_f64(12.0)),
            ("aria-hidden", JsValue::from_str("true")),
        ])?),
        &[],
    )?);
    let trailing = tag(
        &modules.react,
        "span",
        Some(&class("seekdeep-plugin-inventory-cardTrailing")?),
        &trailing,
    )?;
    tag(
        &modules.react,
        "button",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-plugin-inventory-cardContent"),
            ),
            ("type", JsValue::from_str("button")),
            ("aria-expanded", JsValue::from_bool(open)),
            ("aria-controls", JsValue::from_str(detail_id)),
            ("aria-label", JsValue::from_str(aria)),
            ("onClick", toggle),
        ])?),
        &[title_node, trailing],
    )
}

fn render_details(
    modules: &BrowserModules,
    entry: &PluginInventoryEntry,
    configuration: &str,
    status: &str,
    detail_id: &str,
    translate: &Function,
) -> Result<JsValue, JsValue> {
    let code = tag(
        &modules.react,
        "code",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-plugin-inventory-entryValue"),
            ),
            ("data-loader-entry", JsValue::from_str("")),
        ])?),
        &[JsValue::from_str(&entry.entry_id)],
    )?;
    let mut rows = vec![detail_row(
        modules,
        &translated(translate, "configuration")?,
        configuration,
    )?];
    if entry.enabled {
        rows.push(detail_row(
            modules,
            &translated(translate, "cordis")?,
            status,
        )?);
    }
    let details = tag(
        &modules.react,
        "dl",
        Some(&class("seekdeep-plugin-inventory-details")?),
        &rows,
    )?;
    tag(
        &modules.react,
        "div",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-plugin-inventory-cardDetails"),
            ),
            ("id", JsValue::from_str(detail_id)),
        ])?),
        &[code, details],
    )
}

fn detail_row(modules: &BrowserModules, term: &JsValue, detail: &str) -> Result<JsValue, JsValue> {
    tag(
        &modules.react,
        "div",
        None,
        &[
            tag(&modules.react, "dt", None, std::slice::from_ref(term))?,
            tag(&modules.react, "dd", None, &[JsValue::from_str(detail)])?,
        ],
    )
}

fn status_paragraph(
    modules: &BrowserModules,
    translate: &Function,
    key: &str,
) -> Result<JsValue, JsValue> {
    tag(
        &modules.react,
        "p",
        Some(&class("seekdeep-plugin-inventory-status")?),
        &[translated(translate, key)?],
    )
}

fn loading_state() -> Result<Object, JsValue> {
    object(&[("status", JsValue::from_str("loading"))])
}

fn error_state() -> Result<Object, JsValue> {
    object(&[("status", JsValue::from_str("error"))])
}

fn ready_state(snapshot: &JsValue) -> Result<Object, JsValue> {
    object(&[
        ("status", JsValue::from_str("ready")),
        ("snapshot", snapshot.clone()),
    ])
}

fn matches_entry(entry: &PluginInventoryEntry, query: &str) -> bool {
    query.is_empty()
        || locale_lower(&entry.module_name).is_ok_and(|value| value.contains(query))
        || locale_lower(&entry.entry_id).is_ok_and(|value| value.contains(query))
}

fn locale_lower(value: &str) -> Result<String, JsValue> {
    js_sys::JsString::from(value)
        .to_locale_lower_case(None)
        .as_string()
        .ok_or_else(|| js_sys::Error::new("String.toLocaleLowerCase must return a string").into())
}

fn encode_component(value: &str) -> Result<String, JsValue> {
    Reflect::get(&js_sys::global(), &JsValue::from_str("encodeURIComponent"))?
        .dyn_into::<Function>()?
        .call1(&JsValue::UNDEFINED, &JsValue::from_str(value))?
        .as_string()
        .ok_or_else(|| js_sys::Error::new("encodeURIComponent must return a string").into())
}

const fn phase_name(phase: crate::PluginFiberPhase) -> &'static str {
    match phase {
        crate::PluginFiberPhase::Pending => "pending",
        crate::PluginFiberPhase::Loading => "loading",
        crate::PluginFiberPhase::Active => "active",
        crate::PluginFiberPhase::Failed => "failed",
        crate::PluginFiberPhase::Unloading => "unloading",
    }
}

fn own_locale_dictionaries(ctx: &JsValue, locale: &JsValue) -> Result<(), JsValue> {
    let zh = dictionary(ZH)?;
    let en = dictionary(EN)?;
    let dictionaries = object(&[("zh", zh.into()), ("en", en.into())])?;
    let locale = locale.clone();
    let installer = Closure::wrap(Box::new(move || {
        call_method(
            &locale,
            "register",
            &[
                JsValue::from_str(LOCALE_NAMESPACE),
                dictionaries.clone().into(),
            ],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        ctx,
        "effect",
        &[
            installer.into_js_value(),
            JsValue::from_str("ui-settings-plugin-inventory: dictionaries"),
        ],
    )?;
    Ok(())
}

fn inject_styles() -> Result<(), JsValue> {
    const PACKAGE: &str = "@seekdeep-ai/seekdeep-client-ui-settings-plugin-inventory";
    let document = Reflect::get(&js_sys::global(), &JsValue::from_str("document"))?;
    if document.is_null() || document.is_undefined() {
        return Ok(());
    }
    let selector = format!(
        "style[data-plugin={}]",
        serde_json::to_string(PACKAGE).unwrap()
    );
    if !call_method(&document, "querySelector", &[JsValue::from_str(&selector)])?.is_null() {
        return Ok(());
    }
    let style = call_method(&document, "createElement", &[JsValue::from_str("style")])?;
    call_method(
        &style,
        "setAttribute",
        &[JsValue::from_str("data-plugin"), JsValue::from_str(PACKAGE)],
    )?;
    Reflect::set(
        &style,
        &JsValue::from_str("textContent"),
        &JsValue::from_str(PLUGIN_INVENTORY_STYLES),
    )?;
    let head = required(&document, "head", "document")?;
    call_method(&head, "appendChild", &[style])?;
    Ok(())
}

fn configured_modules() -> Result<BrowserModules, JsValue> {
    MODULES.with(|modules| {
        modules.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-settings-plugin-inventory is not configured").into()
        })
    })
}

fn dictionary(entries: &[(&str, &str)]) -> Result<Object, JsValue> {
    let value = Object::new();
    for (key, entry) in entries {
        set(&value, key, &JsValue::from_str(entry))?;
    }
    Ok(value)
}

fn translated(translate: &Function, key: &str) -> Result<JsValue, JsValue> {
    translate.call1(&JsValue::UNDEFINED, &JsValue::from_str(key))
}

fn tag(
    react: &JsValue,
    name: &str,
    props: Option<&Object>,
    children: &[JsValue],
) -> Result<JsValue, JsValue> {
    element(react, &JsValue::from_str(name), props, children)
}

fn component(
    react: &JsValue,
    component: &JsValue,
    props: Option<&Object>,
    children: &[JsValue],
) -> Result<JsValue, JsValue> {
    element(react, component, props, children)
}

fn element(
    react: &JsValue,
    kind: &JsValue,
    props: Option<&Object>,
    children: &[JsValue],
) -> Result<JsValue, JsValue> {
    let args = Array::new();
    args.push(kind);
    args.push(props.map_or(&JsValue::NULL, AsRef::as_ref));
    for child in children {
        args.push(child);
    }
    required_function(react, "createElement", "React")?.apply(react, &args)
}

fn use_state(react: &JsValue, initial: &JsValue) -> Result<(JsValue, Function), JsValue> {
    let pair = Array::from(&required_function(react, "useState", "React")?.call1(react, initial)?);
    Ok((pair.get(0), pair.get(1).dyn_into()?))
}

fn use_effect(react: &JsValue, effect: &JsValue, dependencies: &Array) -> Result<(), JsValue> {
    required_function(react, "useEffect", "React")?
        .call2(react, effect, dependencies)
        .map(|_| ())
}

fn class(value: &str) -> Result<Object, JsValue> {
    object(&[("className", JsValue::from_str(value))])
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let value = Object::new();
    for (key, entry) in entries {
        set(&value, key, entry)?;
    }
    Ok(value)
}

fn set(value: &Object, key: &str, entry: &JsValue) -> Result<(), JsValue> {
    Reflect::set(value, &JsValue::from_str(key), entry).map(|_| ())
}

fn required(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let entry = Reflect::get(value, &JsValue::from_str(key))?;
    if entry.is_null() || entry.is_undefined() {
        Err(js_sys::Error::new(&format!("{owner} omitted required property {key:?}")).into())
    } else {
        Ok(entry)
    }
}

fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    required(value, key, owner)?
        .as_string()
        .ok_or_else(|| js_sys::Error::new(&format!("{owner} {key:?} must be a string")).into())
}

fn required_bool(value: &JsValue, key: &str, owner: &str) -> Result<bool, JsValue> {
    required(value, key, owner)?
        .as_bool()
        .ok_or_else(|| js_sys::Error::new(&format!("{owner} {key:?} must be a boolean")).into())
}

fn required_function(value: &JsValue, key: &str, owner: &str) -> Result<Function, JsValue> {
    required(value, key, owner)?.dyn_into()
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    method.apply(value, &args)
}

fn usize_as_f64(value: usize) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
        value as f64
    }
}

fn js_error_from_display(error: impl std::fmt::Display) -> JsValue {
    js_sys::Error::new(&error.to_string()).into()
}
