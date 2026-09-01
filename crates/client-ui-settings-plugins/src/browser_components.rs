//! Compiled React components for plugin Settings.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Reflect, Set};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::browser::{
    call_method, class, create_element, css, event_value, inject_prefixed_style, object, optional,
    required, required_function, tag, translated,
};

const FIELDS_CSS: &str =
    include_str!("../../../packages/client/ui-settings-plugins/src/client/fields.module.css");
const CARD_CSS: &str =
    include_str!("../../../packages/client/ui-settings-plugins/src/client/PluginCard.module.css");
const SECTION_CSS: &str = include_str!(
    "../../../packages/client/ui-settings-plugins/src/client/PluginsSettingsSection.module.css"
);

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
    static COMPONENTS: RefCell<Option<Components>> = const { RefCell::new(None) };
}

#[derive(Clone)]
pub(crate) struct BrowserModules {
    pub(crate) react: JsValue,
    clsx: Function,
    primitives: JsValue,
    pub(crate) resolve_slot_label: Function,
}

impl BrowserModules {
    fn primitive(&self, name: &str) -> Result<JsValue, JsValue> {
        required(&self.primitives, name, "UI primitives")
    }
}

#[derive(Clone)]
pub(crate) struct Components {
    pub(crate) section: JsValue,
    pub(crate) configurable_tab: JsValue,
    pub(crate) bash_card: JsValue,
    pub(crate) agent_loop_card: JsValue,
    pub(crate) web_search_card: JsValue,
    plugin_card: JsValue,
    value_field: JsValue,
    secret_field: JsValue,
}

/// Configures page-owned React, `clsx`, primitives, Slot label resolution, and styles.
///
/// # Errors
///
/// Returns before mutation when a required browser dependency is unavailable.
#[wasm_bindgen(js_name = configureClientUiSettingsPlugins)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_settings_plugins(
    react: JsValue,
    clsx: Function,
    primitives: JsValue,
    resolve_slot_label: Function,
) -> Result<(), JsValue> {
    for method in ["createElement", "useEffect", "useId", "useRef", "useState"] {
        required_function(&react, method, "React")?;
    }
    required(&react, "Fragment", "React")?;
    required(&primitives, "IconChevronDownOutline14", "UI primitives")?;
    inject_prefixed_style("fields", FIELDS_CSS)?;
    inject_prefixed_style("PluginCard", CARD_CSS)?;
    inject_prefixed_style("PluginsSettingsSection", SECTION_CSS)?;
    let modules = BrowserModules {
        react,
        clsx,
        primitives,
        resolve_slot_label,
    };
    MODULES.with(|configured| *configured.borrow_mut() = Some(modules.clone()));
    let value_field = component(&modules, render_value_field);
    let secret_field = component(&modules, render_secret_field);
    let plugin_card = component(&modules, render_plugin_card);
    let section = component(&modules, render_section);
    let configurable_tab = component(&modules, render_configurable_tab);
    let bash_card = component(&modules, render_bash_card);
    let agent_loop_card = component(&modules, render_agent_loop_card);
    let web_search_card = component(&modules, render_web_search_card);
    COMPONENTS.with(|configured| {
        *configured.borrow_mut() = Some(Components {
            section,
            configurable_tab,
            bash_card,
            agent_loop_card,
            web_search_card,
            plugin_card,
            value_field,
            secret_field,
        });
    });
    Ok(())
}

type Renderer = fn(&BrowserModules, &JsValue) -> Result<JsValue, JsValue>;

fn component(modules: &BrowserModules, renderer: Renderer) -> JsValue {
    let modules = modules.clone();
    Closure::wrap(Box::new(move |props: JsValue| renderer(&modules, &props))
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value()
}

pub(crate) fn configured_modules() -> Result<BrowserModules, JsValue> {
    MODULES.with(|configured| {
        configured.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-settings-plugins modules were not configured").into()
        })
    })
}

pub(crate) fn configured_components() -> Result<Components, JsValue> {
    COMPONENTS.with(|configured| {
        configured.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-settings-plugins components were not configured").into()
        })
    })
}

macro_rules! component_getter {
    ($rust:ident, $js:literal, $field:ident, $label:literal) => {
        #[doc = concat!("Returns the compiled `", $label, "` component.")]
        ///
        /// # Errors
        ///
        /// Returns before browser configuration.
        #[wasm_bindgen(js_name = $js)]
        pub fn $rust() -> Result<JsValue, JsValue> {
            Ok(configured_components()?.$field)
        }
    };
}

component_getter!(
    plugins_settings_section_component,
    "pluginsSettingsSectionComponent",
    section,
    "PluginsSettingsSection"
);
component_getter!(
    configurable_plugins_tab_component,
    "configurablePluginsTabComponent",
    configurable_tab,
    "ConfigurablePluginsTab"
);
component_getter!(
    plugins_bash_card_component,
    "pluginsBashCardComponent",
    bash_card,
    "BashCard"
);
component_getter!(
    plugins_agent_loop_card_component,
    "pluginsAgentLoopCardComponent",
    agent_loop_card,
    "AgentLoopCard"
);
component_getter!(
    plugins_web_search_card_component,
    "pluginsWebSearchCardComponent",
    web_search_card,
    "WebSearchCard"
);
component_getter!(
    plugins_value_field_component,
    "pluginsValueFieldComponent",
    value_field,
    "ValueField"
);
component_getter!(
    plugins_secret_field_component,
    "pluginsSecretFieldComponent",
    secret_field,
    "SecretField"
);
component_getter!(
    plugins_plugin_card_component,
    "pluginsPluginCardComponent",
    plugin_card,
    "PluginCard"
);

fn classes(modules: &BrowserModules, values: &[(&str, bool)]) -> Result<JsValue, JsValue> {
    let arguments = Array::new();
    for (name, enabled) in values {
        let value = if *enabled {
            JsValue::from_str(&css(name))
        } else {
            JsValue::FALSE
        };
        arguments.push(&value);
    }
    modules.clsx.apply(&JsValue::UNDEFINED, &arguments)
}

fn state_bool(state: &JsValue, key: &str) -> Result<bool, JsValue> {
    Ok(required(state, key, "plugin card state")?
        .as_bool()
        .unwrap_or(false))
}

fn render_value_field(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let invalid = required(props, "invalid", "ValueField props")?
        .as_bool()
        .unwrap_or(false);
    let overridden = required(props, "overridden", "ValueField props")?
        .as_bool()
        .unwrap_or(false);
    let disabled = required(props, "disabled", "ValueField props")?
        .as_bool()
        .unwrap_or(false);
    let mut head = vec![tag(
        &modules.react,
        "label",
        Some(&object(&[
            ("className", JsValue::from_str(&css("label"))),
            ("htmlFor", required(props, "id", "ValueField props")?),
        ])?),
        &[required(props, "label", "ValueField props")?],
    )?];
    if overridden {
        head.push(tag(
            &modules.react,
            "span",
            Some(&class("badges")?),
            &[
                tag(
                    &modules.react,
                    "span",
                    Some(&class("badge")?),
                    &[required(props, "overriddenLabel", "ValueField props")?],
                )?,
                tag(
                    &modules.react,
                    "button",
                    Some(&object(&[
                        ("type", JsValue::from_str("button")),
                        ("className", JsValue::from_str(&css("reset"))),
                        ("disabled", JsValue::from_bool(disabled)),
                        ("onClick", required(props, "onReset", "ValueField props")?),
                    ])?),
                    &[required(props, "resetLabel", "ValueField props")?],
                )?,
            ],
        )?);
    }
    let edit = required_function(props, "onEdit", "ValueField props")?;
    let on_change = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        edit.call1(
            &JsValue::UNDEFINED,
            &JsValue::from_str(&event_value(&event)?),
        )?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let mut input_props = vec![
        ("id", required(props, "id", "ValueField props")?),
        (
            "className",
            classes(modules, &[("input", true), ("inputInvalid", invalid)])?,
        ),
        ("type", JsValue::from_str("text")),
        ("value", required(props, "text", "ValueField props")?),
        (
            "placeholder",
            optional(props, "placeholder")?.unwrap_or(JsValue::from_str("")),
        ),
        ("disabled", JsValue::from_bool(disabled)),
        ("onChange", on_change.into_js_value()),
    ];
    if optional(props, "numeric")?.and_then(|value| value.as_bool()) == Some(true) {
        input_props.push(("inputMode", JsValue::from_str("numeric")));
    }
    if invalid {
        input_props.push(("aria-invalid", JsValue::TRUE));
    }
    tag(
        &modules.react,
        "div",
        Some(&class("field")?),
        &[
            tag(&modules.react, "div", Some(&class("head")?), &head)?,
            tag(&modules.react, "input", Some(&object(&input_props)?), &[])?,
            tag(
                &modules.react,
                "p",
                Some(&class(if invalid { "invalid" } else { "hint" })?),
                &[required(
                    props,
                    if invalid { "invalidLabel" } else { "hint" },
                    "ValueField props",
                )?],
            )?,
        ],
    )
}

fn render_secret_field(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let configured = required(props, "configured", "SecretField props")?
        .as_bool()
        .unwrap_or(false);
    let disabled = required(props, "disabled", "SecretField props")?
        .as_bool()
        .unwrap_or(false);
    let edit = required_function(props, "onEdit", "SecretField props")?;
    let on_change = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        edit.call1(
            &JsValue::UNDEFINED,
            &JsValue::from_str(&event_value(&event)?),
        )?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let head = tag(
        &modules.react,
        "div",
        Some(&class("head")?),
        &[
            tag(
                &modules.react,
                "label",
                Some(&object(&[
                    ("className", JsValue::from_str(&css("label"))),
                    ("htmlFor", required(props, "id", "SecretField props")?),
                ])?),
                &[required(props, "label", "SecretField props")?],
            )?,
            tag(
                &modules.react,
                "span",
                Some(&class("badges")?),
                &[tag(
                    &modules.react,
                    "span",
                    Some(&class(if configured { "badge" } else { "badgeMuted" })?),
                    &[required(props, "stateLabel", "SecretField props")?],
                )?],
            )?,
        ],
    )?;
    tag(
        &modules.react,
        "div",
        Some(&class("field")?),
        &[
            head,
            tag(
                &modules.react,
                "input",
                Some(&object(&[
                    ("id", required(props, "id", "SecretField props")?),
                    ("className", JsValue::from_str(&css("input"))),
                    ("type", JsValue::from_str("password")),
                    ("autoComplete", JsValue::from_str("off")),
                    ("value", required(props, "text", "SecretField props")?),
                    ("disabled", JsValue::from_bool(disabled)),
                    ("onChange", on_change.into_js_value()),
                ])?),
                &[],
            )?,
            tag(
                &modules.react,
                "p",
                Some(&class("hint")?),
                &[required(props, "hint", "SecretField props")?],
            )?,
        ],
    )
}

fn child_values(props: &JsValue) -> Result<Vec<JsValue>, JsValue> {
    let Some(children) = optional(props, "children")? else {
        return Ok(Vec::new());
    };
    if Array::is_array(&children) {
        Ok(children.dyn_into::<Array>()?.iter().collect())
    } else {
        Ok(vec![children])
    }
}

#[allow(clippy::too_many_lines)] // Card disclosure and settlement chrome are one source component.
fn render_plugin_card(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let (open, set_open) = use_state(&modules.react, &JsValue::FALSE)?;
    let open = open.as_bool().unwrap_or(false);
    let state = required(props, "state", "PluginCard props")?;
    if !state_bool(&state, "available")? {
        return Ok(JsValue::NULL);
    }
    let translate = required_function(props, "t", "PluginCard props")?;
    let title = translate.call1(
        &JsValue::UNDEFINED,
        &required(props, "titleKey", "PluginCard props")?,
    )?;
    let title_text = title.as_string().unwrap_or_default();
    let dirty = state_bool(&state, "dirty")?;
    let invalid = state_bool(&state, "invalid")?;
    let saving = state_bool(&state, "saving")?;
    let failed = state_bool(&state, "failed")?;
    let writable = state_bool(&state, "writable")?;
    let toggle = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        set_open.call1(&JsValue::UNDEFINED, &JsValue::from_bool(!open))?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let mut header = vec![tag(
        &modules.react,
        "span",
        Some(&class("headText")?),
        &[
            tag(&modules.react, "span", Some(&class("name")?), &[title])?,
            tag(
                &modules.react,
                "span",
                Some(&class("description")?),
                &[translate.call1(
                    &JsValue::UNDEFINED,
                    &required(props, "descriptionKey", "PluginCard props")?,
                )?],
            )?,
        ],
    )?];
    if dirty {
        header.push(tag(
            &modules.react,
            "span",
            Some(&class("pending")?),
            &[translated(&translate, "unsaved")?],
        )?);
    }
    header.push(create_element(
        &modules.react,
        &modules.primitive("IconChevronDownOutline14")?,
        Some(&object(&[(
            "className",
            classes(modules, &[("chevron", true), ("chevronOpen", open)])?,
        )])?),
        &[],
    )?);
    let mut card = vec![tag(
        &modules.react,
        "button",
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            ("className", JsValue::from_str(&css("header"))),
            ("aria-expanded", JsValue::from_bool(open)),
            (
                "aria-label",
                JsValue::from_str(&format!(
                    "{}: {title_text}",
                    translated(&translate, if open { "collapse" } else { "expand" })?
                        .as_string()
                        .unwrap_or_default()
                )),
            ),
            ("onClick", toggle.into_js_value()),
        ])?),
        &header,
    )?];
    if open {
        let mut body = Vec::new();
        if !writable {
            body.push(tag(
                &modules.react,
                "p",
                Some(&object(&[
                    ("className", JsValue::from_str(&css("readOnly"))),
                    ("role", JsValue::from_str("status")),
                ])?),
                &[translated(&translate, "readOnly")?],
            )?);
        }
        body.extend(child_values(props)?);
        let mut footer = Vec::new();
        if failed {
            footer.push(tag(
                &modules.react,
                "p",
                Some(&object(&[
                    ("className", JsValue::from_str(&css("failed"))),
                    ("role", JsValue::from_str("status")),
                ])?),
                &[translated(&translate, "saveFailed")?],
            )?);
        }
        footer.push(tag(
            &modules.react,
            "button",
            Some(&object(&[
                ("type", JsValue::from_str("button")),
                ("className", JsValue::from_str(&css("discard"))),
                ("disabled", JsValue::from_bool(!dirty || saving)),
                ("onClick", required(props, "onDiscard", "PluginCard props")?),
            ])?),
            &[translated(&translate, "discard")?],
        )?);
        footer.push(tag(
            &modules.react,
            "button",
            Some(&object(&[
                ("type", JsValue::from_str("button")),
                ("className", JsValue::from_str(&css("save"))),
                ("disabled", JsValue::from_bool(!dirty || invalid || saving)),
                ("onClick", required(props, "onSave", "PluginCard props")?),
            ])?),
            &[translated(
                &translate,
                if saving { "saving" } else { "save" },
            )?],
        )?);
        body.push(tag(
            &modules.react,
            "div",
            Some(&class("footer")?),
            &footer,
        )?);
        card.push(tag(&modules.react, "div", Some(&class("body")?), &body)?);
    }
    tag(
        &modules.react,
        "li",
        Some(&object(&[(
            "className",
            classes(modules, &[("card", true), ("cardOpen", open)])?,
        )])?),
        &card,
    )
}

fn use_state(react: &JsValue, initial: &JsValue) -> Result<(JsValue, Function), JsValue> {
    let pair = Array::from(&required_function(react, "useState", "React")?.call1(react, initial)?);
    Ok((pair.get(0), pair.get(1).dyn_into()?))
}

fn identity_selector() -> JsValue {
    Closure::wrap(Box::new(move |value: JsValue| value) as Box<dyn FnMut(JsValue) -> JsValue>)
        .into_js_value()
}

#[allow(clippy::too_many_arguments)] // One field's exact source prop grammar is assembled at this boundary.
fn field_props(
    translate: &Function,
    state: &JsValue,
    field: &str,
    id: &str,
    label: &str,
    hint: &str,
    disabled: bool,
    numeric: bool,
    edit: &Function,
    reset: &Function,
) -> Result<Object, JsValue> {
    let field_state = required(state, field, "plugin card state")?;
    let edit_field = field.to_owned();
    let edit = edit.clone();
    let on_edit = Closure::wrap(Box::new(move |text: String| -> Result<(), JsValue> {
        edit.call2(
            &JsValue::UNDEFINED,
            &JsValue::from_str(&edit_field),
            &JsValue::from_str(&text),
        )?;
        Ok(())
    }) as Box<dyn FnMut(String) -> Result<(), JsValue>>);
    let reset_field = field.to_owned();
    let reset = reset.clone();
    let on_reset = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        reset.call1(&JsValue::UNDEFINED, &JsValue::from_str(&reset_field))?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    object(&[
        ("id", JsValue::from_str(id)),
        ("label", translated(translate, label)?),
        ("hint", translated(translate, hint)?),
        ("overriddenLabel", translated(translate, "overridden")?),
        ("resetLabel", translated(translate, "reset")?),
        ("invalidLabel", translated(translate, "invalidNumber")?),
        ("numeric", JsValue::from_bool(numeric)),
        ("disabled", JsValue::from_bool(disabled)),
        ("text", required(&field_state, "text", "card field state")?),
        (
            "overridden",
            required(&field_state, "overridden", "card field state")?,
        ),
        (
            "invalid",
            required(&field_state, "invalid", "card field state")?,
        ),
        ("onEdit", on_edit.into_js_value()),
        ("onReset", on_reset.into_js_value()),
    ])
}

fn card_props(
    props: &JsValue,
    state: &JsValue,
    title_key: &str,
    description_key: &str,
) -> Result<Object, JsValue> {
    object(&[
        ("t", required(props, "t", "card props")?),
        ("titleKey", JsValue::from_str(title_key)),
        ("descriptionKey", JsValue::from_str(description_key)),
        ("state", state.clone()),
        ("onSave", required(props, "save", "card props")?),
        ("onDiscard", required(props, "discard", "card props")?),
    ])
}

fn render_bash_card(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let hook = required_function(props, "useBashCard", "BashCard props")?;
    let state = hook.call1(&JsValue::UNDEFINED, &identity_selector())?;
    let translate = required_function(props, "t", "BashCard props")?;
    let disabled = !state_bool(&state, "writable")?;
    let edit = required_function(props, "edit", "BashCard props")?;
    let reset = required_function(props, "resetField", "BashCard props")?;
    let fields = [
        (
            "timeoutMs",
            "plugin-config-bash-timeout",
            "bashTimeoutMs",
            "bashTimeoutMsHint",
        ),
        (
            "maxOutputBytes",
            "plugin-config-bash-output",
            "bashMaxOutputBytes",
            "bashMaxOutputBytesHint",
        ),
    ]
    .into_iter()
    .map(|(field, id, label, hint)| {
        create_element(
            &modules.react,
            &configured_components()?.value_field,
            Some(&field_props(
                &translate, &state, field, id, label, hint, disabled, true, &edit, &reset,
            )?),
            &[],
        )
    })
    .collect::<Result<Vec<_>, JsValue>>()?;
    create_element(
        &modules.react,
        &configured_components()?.plugin_card,
        Some(&card_props(props, &state, "bashTitle", "bashDescription")?),
        &fields,
    )
}

fn render_agent_loop_card(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let hook = required_function(props, "useAgentLoopCard", "AgentLoopCard props")?;
    let state = hook.call1(&JsValue::UNDEFINED, &identity_selector())?;
    let translate = required_function(props, "t", "AgentLoopCard props")?;
    let field = create_element(
        &modules.react,
        &configured_components()?.value_field,
        Some(&field_props(
            &translate,
            &state,
            "maxParallelToolCalls",
            "plugin-config-agent-loop-parallel",
            "agentLoopMaxParallel",
            "agentLoopMaxParallelHint",
            !state_bool(&state, "writable")?,
            true,
            &required_function(props, "edit", "AgentLoopCard props")?,
            &required_function(props, "resetField", "AgentLoopCard props")?,
        )?),
        &[],
    )?;
    create_element(
        &modules.react,
        &configured_components()?.plugin_card,
        Some(&card_props(
            props,
            &state,
            "agentLoopTitle",
            "agentLoopDescription",
        )?),
        &[field],
    )
}

fn render_web_search_card(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let hook = required_function(props, "useWebSearchCard", "WebSearchCard props")?;
    let state = hook.call1(&JsValue::UNDEFINED, &identity_selector())?;
    let translate = required_function(props, "t", "WebSearchCard props")?;
    let edit = required_function(props, "edit", "WebSearchCard props")?;
    let reset = required_function(props, "resetField", "WebSearchCard props")?;
    let key_state = required(&state, "apiKey", "WebSearch card state")?;
    let edit_key = edit.clone();
    let on_key = Closure::wrap(Box::new(move |text: String| -> Result<(), JsValue> {
        edit_key.call2(
            &JsValue::UNDEFINED,
            &JsValue::from_str("apiKey"),
            &JsValue::from_str(&text),
        )?;
        Ok(())
    }) as Box<dyn FnMut(String) -> Result<(), JsValue>>);
    let secret = create_element(
        &modules.react,
        &configured_components()?.secret_field,
        Some(&object(&[
            ("id", JsValue::from_str("plugin-config-web-search-key")),
            ("label", translated(&translate, "webSearchApiKey")?),
            ("hint", translated(&translate, "webSearchApiKeyHint")?),
            (
                "disabled",
                JsValue::from_bool(!state_bool(&state, "apiKeyWritable")?),
            ),
            ("text", required(&key_state, "text", "card field state")?),
            (
                "configured",
                JsValue::from_bool(state_bool(&state, "apiKeyConfigured")?),
            ),
            (
                "stateLabel",
                translated(
                    &translate,
                    if state_bool(&state, "apiKeyConfigured")? {
                        "webSearchApiKeySet"
                    } else {
                        "webSearchApiKeyUnset"
                    },
                )?,
            ),
            ("onEdit", on_key.into_js_value()),
        ])?),
        &[],
    )?;
    let disabled = !state_bool(&state, "writable")?;
    let base = create_element(
        &modules.react,
        &configured_components()?.value_field,
        Some(&field_props(
            &translate,
            &state,
            "baseURL",
            "plugin-config-web-search-endpoint",
            "webSearchBaseUrl",
            "webSearchBaseUrlHint",
            disabled,
            false,
            &edit,
            &reset,
        )?),
        &[],
    )?;
    let uses = create_element(
        &modules.react,
        &configured_components()?.value_field,
        Some(&field_props(
            &translate,
            &state,
            "maxUses",
            "plugin-config-web-search-max-uses",
            "webSearchMaxUses",
            "webSearchMaxUsesHint",
            disabled,
            true,
            &edit,
            &reset,
        )?),
        &[],
    )?;
    create_element(
        &modules.react,
        &configured_components()?.plugin_card,
        Some(&card_props(
            props,
            &state,
            "webSearchTitle",
            "webSearchDescription",
        )?),
        &[secret, base, uses],
    )
}

fn render_configurable_tab(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let translate = required_function(props, "t", "ConfigurablePluginsTab props")?;
    let count = required(props, "cardCount", "ConfigurablePluginsTab props")?
        .as_f64()
        .unwrap_or(0.0);
    if count == 0.0 {
        return tag(
            &modules.react,
            "p",
            Some(&class("empty")?),
            &[translated(&translate, "empty")?],
        );
    }
    let render = required_function(props, "renderSlot", "ConfigurablePluginsTab props")?;
    let cards = render.call2(
        &JsValue::UNDEFINED,
        &JsValue::from_str("settings.plugin.item"),
        &Object::new(),
    )?;
    tag(&modules.react, "ul", Some(&class("cards")?), &[cards])
}

#[allow(clippy::too_many_lines)] // Tabs own keyboard focus, visited mounts, and panel dispatch together.
fn render_section(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let tabs_id = required_function(&modules.react, "useId", "React")?
        .call0(&modules.react)?
        .as_string()
        .unwrap_or_default();
    let refs = required_function(&modules.react, "useRef", "React")?
        .call1(&modules.react, &Array::new())?;
    let use_tabs = required_function(props, "useTabs", "PluginsSettingsSection props")?;
    let rows = use_tabs
        .call1(&JsValue::UNDEFINED, &identity_selector())?
        .dyn_into::<Array>()?;
    let (active_id, set_active_id) = use_state(&modules.react, &JsValue::UNDEFINED)?;
    let (visited, set_visited) = use_state(&modules.react, &Set::new(&JsValue::UNDEFINED).into())?;
    let visited = visited.dyn_into::<Set>()?;
    let requested = active_id.as_string();
    let active = rows
        .iter()
        .find(|row| {
            requested.as_deref()
                == optional(row, "id")
                    .ok()
                    .flatten()
                    .and_then(|value| value.as_string())
                    .as_deref()
        })
        .or_else(|| rows.iter().next())
        .and_then(|row| optional(&row, "id").ok().flatten())
        .and_then(|value| value.as_string());
    let effect_active = active.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let Some(active) = effect_active.as_deref() else {
            return Ok(());
        };
        let active = active.to_owned();
        let update = Closure::wrap(Box::new(move |current: JsValue| -> JsValue {
            let current = current.dyn_into::<Set>().unwrap_or_default();
            if current.has(&JsValue::from_str(&active)) {
                return current.into();
            }
            let next = Set::new(&current);
            next.add(&JsValue::from_str(&active));
            next.into()
        }) as Box<dyn FnMut(JsValue) -> JsValue>);
        set_visited.call1(&JsValue::UNDEFINED, &update.into_js_value())?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let active_dependency = active
        .as_deref()
        .map_or(JsValue::UNDEFINED, JsValue::from_str);
    required_function(&modules.react, "useEffect", "React")?.call2(
        &modules.react,
        &effect.into_js_value(),
        &Array::of1(&active_dependency),
    )?;
    let translate = required_function(props, "t", "PluginsSettingsSection props")?;
    let mut body = vec![
        tag(
            &modules.react,
            "h2",
            Some(&class("heading")?),
            &[translated(&translate, "title")?],
        )?,
        tag(
            &modules.react,
            "p",
            Some(&class("intro")?),
            &[translated(&translate, "intro")?],
        )?,
    ];
    if rows.length() == 0 {
        body.push(tag(
            &modules.react,
            "p",
            Some(&class("empty")?),
            &[translated(&translate, "empty")?],
        )?);
    } else {
        let mut tabs = Vec::new();
        for (index, row) in rows.iter().enumerate() {
            let id = required(&row, "id", "Plugins tab entry")?
                .as_string()
                .unwrap_or_default();
            let selected = active.as_deref() == Some(id.as_str());
            let remember = refs.clone();
            let at = u32::try_from(index)
                .map_err(|_| js_sys::RangeError::new("tab index exceeds u32"))?;
            let reference = Closure::wrap(Box::new(move |element: JsValue| {
                let _ = Reflect::set(&remember, &JsValue::from_f64(f64::from(at)), &element);
            }) as Box<dyn FnMut(JsValue)>);
            let click_id = id.clone();
            let click_setter = set_active_id.clone();
            let click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
                click_setter.call1(&JsValue::UNDEFINED, &JsValue::from_str(&click_id))?;
                Ok(())
            }) as Box<dyn FnMut() -> Result<(), JsValue>>);
            let key_rows = rows.clone();
            let key_refs = refs.clone();
            let key_setter = set_active_id.clone();
            let keydown = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
                let key = required(&event, "key", "tab key event")?
                    .as_string()
                    .unwrap_or_default();
                let length = key_rows.length();
                let Some(next) = (match key.as_str() {
                    "ArrowRight" => Some((at + 1) % length),
                    "ArrowLeft" => Some((at + length - 1) % length),
                    "Home" => Some(0),
                    "End" => Some(length - 1),
                    _ => None,
                }) else {
                    return Ok(());
                };
                call_method(&event, "preventDefault", &[])?;
                let row = key_rows.get(next);
                key_setter.call1(
                    &JsValue::UNDEFINED,
                    &required(&row, "id", "Plugins tab entry")?,
                )?;
                let tab = Reflect::get(&key_refs, &JsValue::from_f64(f64::from(next)))?;
                call_method(&tab, "focus", &[])?;
                Ok(())
            })
                as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
            tabs.push(tag(
                &modules.react,
                "button",
                Some(&object(&[
                    ("key", JsValue::from_str(&id)),
                    ("ref", reference.into_js_value()),
                    ("id", JsValue::from_str(&format!("{tabs_id}-tab-{id}"))),
                    ("type", JsValue::from_str("button")),
                    ("role", JsValue::from_str("tab")),
                    ("className", JsValue::from_str(&css("tab"))),
                    ("aria-selected", JsValue::from_bool(selected)),
                    (
                        "aria-controls",
                        JsValue::from_str(&format!("{tabs_id}-panel-{id}")),
                    ),
                    (
                        "data-active",
                        if selected {
                            JsValue::from_str("true")
                        } else {
                            JsValue::UNDEFINED
                        },
                    ),
                    (
                        "tabIndex",
                        JsValue::from_f64(if selected { 0.0 } else { -1.0 }),
                    ),
                    ("onClick", click.into_js_value()),
                    ("onKeyDown", keydown.into_js_value()),
                ])?),
                &[required(&row, "label", "Plugins tab entry")?],
            )?);
        }
        body.push(tag(
            &modules.react,
            "div",
            Some(&object(&[
                ("className", JsValue::from_str(&css("tabs"))),
                ("role", JsValue::from_str("tablist")),
                ("aria-label", translated(&translate, "tabs")?),
            ])?),
            &tabs,
        )?);
        let render = required_function(props, "renderSlot", "PluginsSettingsSection props")?;
        for row in rows.iter() {
            let id = required(&row, "id", "Plugins tab entry")?
                .as_string()
                .unwrap_or_default();
            let selected = active.as_deref() == Some(id.as_str());
            if !selected && !visited.has(&JsValue::from_str(&id)) {
                continue;
            }
            let options: JsValue = object(&[("only", JsValue::from_str(&id))])?.into();
            let content = render.call3(
                &JsValue::UNDEFINED,
                &JsValue::from_str("settings.plugins.tab"),
                &Object::new(),
                &options,
            )?;
            body.push(tag(
                &modules.react,
                "div",
                Some(&object(&[
                    ("key", JsValue::from_str(&id)),
                    ("id", JsValue::from_str(&format!("{tabs_id}-panel-{id}"))),
                    ("className", JsValue::from_str(&css("panel"))),
                    ("role", JsValue::from_str("tabpanel")),
                    (
                        "aria-labelledby",
                        JsValue::from_str(&format!("{tabs_id}-tab-{id}")),
                    ),
                    ("hidden", JsValue::from_bool(!selected)),
                ])?),
                &[content],
            )?);
        }
    }
    tag(&modules.react, "div", Some(&class("section")?), &body)
}
