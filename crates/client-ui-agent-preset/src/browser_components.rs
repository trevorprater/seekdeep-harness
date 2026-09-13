//! Compiled label, menu, settings-row, and new-session-seat components.

use std::cell::RefCell;

use js_sys::{Array, Function, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::browser::{
    class, create_element, css, inject_prefixed_style, object, optional, required,
    required_function, tag, translated,
};

const LABEL_CSS: &str =
    include_str!("../../../packages/client/ui-agent-preset/src/client/AgentPresetLabel.module.css");
const ROW_CSS: &str =
    include_str!("../../../packages/client/ui-agent-preset/src/client/AgentPresetRow.module.css");
const SEAT_CSS: &str =
    include_str!("../../../packages/client/ui-agent-preset/src/client/AgentPresetSeat.module.css");

const INTRO_TEXT_DELAY_MS: f64 = 150.0;
const INTRO_CHAR_STAGGER_MS: f64 = 40.0;
const INTRO_TEXT_REVEAL_MS: f64 = 200.0;
const INTRO_CHAR_FADE_MS: f64 = 400.0;

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
    static COMPONENTS: RefCell<Option<Components>> = const { RefCell::new(None) };
}

#[derive(Clone)]
pub(crate) struct BrowserModules {
    pub(crate) react: JsValue,
    pub(crate) primitives: JsValue,
}

impl BrowserModules {
    pub(crate) fn primitive(&self, name: &str) -> Result<JsValue, JsValue> {
        required(&self.primitives, name, "UI primitives")
    }
}

#[derive(Clone)]
struct Components {
    label: JsValue,
    menu: JsValue,
    row: JsValue,
    seat: JsValue,
    section: JsValue,
}

/// Configures page-owned React, primitives, and compiled styles.
///
/// # Errors
///
/// Returns before mutation when a required browser dependency is unavailable.
#[wasm_bindgen(js_name = configureClientUiAgentPreset)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_agent_preset(
    react: JsValue,
    primitives: JsValue,
) -> Result<(), JsValue> {
    for method in [
        "createElement",
        "useEffect",
        "useLayoutEffect",
        "useRef",
        "useState",
    ] {
        required_function(&react, method, "React")?;
    }
    for primitive in [
        "Button",
        "IconAgentPresetOutline16",
        "IconBrowseOutline16",
        "IconChevronDownOutline14",
        "IconCopyOutline16",
        "IconFolderOpenOutline16",
        "IconPlusOutline16",
        "IconTrashOutline16",
        "Menu",
        "Modal",
        "Tooltip",
    ] {
        required(&primitives, primitive, "UI primitives")?;
    }
    required(&react, "Fragment", "React")?;
    inject_prefixed_style("AgentPresetLabel", LABEL_CSS)?;
    inject_prefixed_style("AgentPresetRow", ROW_CSS)?;
    inject_prefixed_style("AgentPresetSeat", SEAT_CSS)?;
    let modules = BrowserModules { react, primitives };
    let section = crate::browser_section::configure_section_component(&modules)?;
    MODULES.with(|configured| *configured.borrow_mut() = Some(modules.clone()));
    COMPONENTS.with(|configured| {
        *configured.borrow_mut() = Some(Components {
            label: component(&modules, render_label),
            menu: component(&modules, render_preset_menu),
            row: component(&modules, render_row),
            seat: component(&modules, render_seat),
            section,
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

fn configured_components() -> Result<Components, JsValue> {
    COMPONENTS.with(|configured| {
        configured.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-agent-preset components were not configured").into()
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
    agent_preset_label_component,
    "agentPresetLabelComponent",
    label,
    "AgentPresetLabel"
);
component_getter!(
    preset_menu_component,
    "presetMenuComponent",
    menu,
    "PresetMenu"
);
component_getter!(
    agent_preset_row_component,
    "agentPresetRowComponent",
    row,
    "AgentPresetRow"
);
component_getter!(
    agent_preset_seat_component,
    "agentPresetSeatComponent",
    seat,
    "AgentPresetSeat"
);
component_getter!(
    agent_preset_section_component,
    "agentPresetSectionComponent",
    section,
    "AgentPresetSection"
);

fn identity_selector() -> JsValue {
    Closure::wrap(Box::new(move |value: JsValue| value) as Box<dyn FnMut(JsValue) -> JsValue>)
        .into_js_value()
}

fn use_state(react: &JsValue, initial: &JsValue) -> Result<(JsValue, Function), JsValue> {
    let pair = Array::from(&required_function(react, "useState", "React")?.call1(react, initial)?);
    Ok((pair.get(0), pair.get(1).dyn_into()?))
}

fn use_effect(react: &JsValue, effect: JsValue, dependencies: &Array) -> Result<(), JsValue> {
    let result = required_function(react, "useEffect", "React")?
        .call2(react, &effect, dependencies)
        .map(|_| ());
    drop(effect);
    result
}

pub(crate) fn preset_text(
    option: &JsValue,
    translate: &Function,
) -> Result<(String, Option<String>), JsValue> {
    let id = required(option, "id", "Agent preset option")?
        .as_string()
        .unwrap_or_default();
    let trust = required(option, "trust", "Agent preset option")?
        .as_string()
        .unwrap_or_default();
    let built_in = (trust == "system")
        .then_some(match id.as_str() {
            "standard" => Some(("presetStandardName", "presetStandardDescription")),
            "code" => Some(("presetCodeName", "presetCodeDescription")),
            "minimal" => Some(("presetMinimalName", "presetMinimalDescription")),
            "cordis" => Some(("presetCordisName", "presetCordisDescription")),
            _ => None,
        })
        .flatten();
    if let Some((name, description)) = built_in {
        return Ok((
            translated(translate, name)?.as_string().unwrap_or_default(),
            translated(translate, description)?.as_string(),
        ));
    }
    Ok((
        optional(option, "name")?
            .and_then(|name| name.as_string())
            .unwrap_or(id),
        optional(option, "description")?.and_then(|description| description.as_string()),
    ))
}

fn render_label(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let session_id = required(props, "sessionId", "AgentPresetLabel props")?
        .as_string()
        .unwrap_or_default();
    let select_session = Closure::wrap(Box::new(move |state: JsValue| -> JsValue {
        Reflect::get(&state, &JsValue::from_str("byId"))
            .ok()
            .and_then(|by_id| Reflect::get(&by_id, &JsValue::from_str(&session_id)).ok())
            .and_then(|summary| Reflect::get(&summary, &JsValue::from_str("agentPreset")).ok())
            .unwrap_or(JsValue::UNDEFINED)
    }) as Box<dyn FnMut(JsValue) -> JsValue>);
    let preset = required_function(props, "useSessions", "AgentPresetLabel props")?
        .call1(&JsValue::UNDEFINED, &select_session.into_js_value())?;
    let select_options = Closure::wrap(Box::new(move |state: JsValue| -> JsValue {
        Reflect::get(&state, &JsValue::from_str("options")).unwrap_or(JsValue::UNDEFINED)
    }) as Box<dyn FnMut(JsValue) -> JsValue>);
    let options = required_function(props, "useAgentPresets", "AgentPresetLabel props")?
        .call1(&JsValue::UNDEFINED, &select_options.into_js_value())?;
    let load = required_function(props, "load", "AgentPresetLabel props")?;
    let should_load = !preset.is_undefined();
    let effect = Closure::wrap(Box::new(move || {
        if should_load {
            let _ = load.call0(&JsValue::UNDEFINED);
        }
    }) as Box<dyn FnMut()>);
    use_effect(
        &modules.react,
        effect.into_js_value(),
        &Array::of2(&preset, &required(props, "load", "AgentPresetLabel props")?),
    )?;
    let Some(preset_id) = preset.as_string() else {
        return Ok(JsValue::NULL);
    };
    let translate = required_function(props, "t", "AgentPresetLabel props")?;
    let option = Array::from(&options).iter().find(|option| {
        Reflect::get(option, &JsValue::from_str("id"))
            .ok()
            .and_then(|id| id.as_string())
            .as_deref()
            == Some(preset_id.as_str())
    });
    let text = option
        .as_ref()
        .map(|option| preset_text(option, &translate))
        .transpose()?;
    let title = text
        .as_ref()
        .and_then(|(_, description)| description.clone())
        .map_or_else(
            || translated(&translate, "headerHint"),
            |value| Ok(JsValue::from_str(&value)),
        )?;
    let name = text.map_or(preset_id, |(name, _)| name);
    create_element(
        &modules.react,
        &JsValue::from_str("span"),
        Some(&object(&[
            ("className", JsValue::from_str(&css("label"))),
            ("title", title),
        ])?),
        &[
            create_element(
                &modules.react,
                &modules.primitive("IconAgentPresetOutline16")?,
                Some(&object(&[
                    ("size", JsValue::from_f64(14.0)),
                    ("className", JsValue::from_str(&css("icon"))),
                ])?),
                &[],
            )?,
            JsValue::from_str(&name),
        ],
    )
}

fn render_preset_menu(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let translate = required_function(props, "t", "PresetMenu props")?;
    let items = Array::new();
    for option in Array::from(&required(props, "options", "PresetMenu props")?).iter() {
        let id = required(&option, "id", "Agent preset option")?;
        let trust = required(&option, "trust", "Agent preset option")?
            .as_string()
            .unwrap_or_default();
        let (name, _) = preset_text(&option, &translate)?;
        let label = if trust == "user" {
            format!(
                "{name} · {}",
                translated(&translate, "userTrust")?
                    .as_string()
                    .unwrap_or_default()
            )
        } else {
            name
        };
        let item: JsValue = object(&[("id", id), ("label", JsValue::from_str(&label))])?.into();
        items.push(&item);
    }
    let set_open = required_function(props, "onOpenChange", "PresetMenu props")?;
    let close = set_open.clone();
    let on_close = Closure::wrap(Box::new(move || {
        let _ = close.call1(&JsValue::UNDEFINED, &JsValue::FALSE);
    }) as Box<dyn FnMut()>);
    let select = required_function(props, "onSelect", "PresetMenu props")?;
    let selected_open = set_open;
    let on_select = Closure::wrap(Box::new(move |id: String| -> Result<(), JsValue> {
        selected_open.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
        select.call1(&JsValue::UNDEFINED, &JsValue::from_str(&id))?;
        Ok(())
    }) as Box<dyn FnMut(String) -> Result<(), JsValue>>);
    let open = required(props, "open", "PresetMenu props")?
        .as_bool()
        .unwrap_or(false);
    let toggle = required_function(props, "onOpenChange", "PresetMenu props")?;
    let on_click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        toggle.call1(&JsValue::UNDEFINED, &JsValue::from_bool(!open))?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let anchor = tag(
        &modules.react,
        "button",
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            (
                "className",
                optional(props, "buttonClassName")?.unwrap_or(JsValue::UNDEFINED),
            ),
            ("aria-haspopup", JsValue::from_str("menu")),
            ("aria-expanded", JsValue::from_bool(open)),
            ("disabled", required(props, "disabled", "PresetMenu props")?),
            ("onClick", on_click.into_js_value()),
        ])?),
        &[
            required(props, "label", "PresetMenu props")?,
            create_element(
                &modules.react,
                &modules.primitive("IconChevronDownOutline14")?,
                Some(&object(&[(
                    "className",
                    optional(props, "chevronClassName")?.unwrap_or(JsValue::UNDEFINED),
                )])?),
                &[],
            )?,
        ],
    )?;
    create_element(
        &modules.react,
        &modules.primitive("Menu")?,
        Some(&object(&[
            ("open", JsValue::from_bool(open)),
            ("onClose", on_close.into_js_value()),
            ("items", items.into()),
            (
                "selectedId",
                required(props, "selectedId", "PresetMenu props")?,
            ),
            ("onSelect", on_select.into_js_value()),
            (
                "align",
                optional(props, "align")?.unwrap_or(JsValue::from_str("end")),
            ),
            ("portal", JsValue::TRUE),
            ("anchor", anchor),
        ])?),
        &[],
    )
}

#[allow(clippy::too_many_lines)] // Row lifecycle, error copy, and menu assembly are one source component.
fn render_row(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let state = required_function(props, "useAgentPreset", "AgentPresetRow props")?
        .call1(&JsValue::UNDEFINED, &identity_selector())?;
    let (open, set_open) = use_state(&modules.react, &JsValue::FALSE)?;
    let open = open.as_bool().unwrap_or(false);
    let load = required_function(props, "load", "AgentPresetRow props")?;
    let load_effect = Closure::wrap(Box::new(move || {
        let _ = load.call0(&JsValue::UNDEFINED);
    }) as Box<dyn FnMut()>);
    use_effect(
        &modules.react,
        load_effect.into_js_value(),
        &Array::of1(&required(props, "load", "AgentPresetRow props")?),
    )?;
    let status = required(&state, "status", "Agent preset settings state")?
        .as_string()
        .unwrap_or_default();
    let writable = required(&state, "writable", "Agent preset settings state")?
        .as_bool()
        .unwrap_or(false);
    let close_setter = set_open.clone();
    let close = status == "unavailable" || !writable;
    let close_effect = Closure::wrap(Box::new(move || {
        if close {
            let _ = close_setter.call1(&JsValue::UNDEFINED, &JsValue::FALSE);
        }
    }) as Box<dyn FnMut()>);
    use_effect(
        &modules.react,
        close_effect.into_js_value(),
        &Array::of2(&JsValue::from_str(&status), &JsValue::from_bool(writable)),
    )?;
    if status == "unavailable" {
        return Ok(JsValue::NULL);
    }
    let translate = required_function(props, "t", "AgentPresetRow props")?;
    let options = Array::from(&required(&state, "options", "Agent preset settings state")?);
    let current = required(&state, "currentValue", "Agent preset settings state")?
        .as_string()
        .unwrap_or_default();
    let chosen = options.iter().find(|option| {
        Reflect::get(option, &JsValue::from_str("id"))
            .ok()
            .and_then(|id| id.as_string())
            .as_deref()
            == Some(current.as_str())
    });
    let chosen_name = chosen
        .as_ref()
        .map(|option| preset_text(option, &translate).map(|(name, _)| name))
        .transpose()?;
    let label = if current.is_empty() {
        translated(&translate, "loading")?
            .as_string()
            .unwrap_or_default()
    } else {
        chosen_name.unwrap_or_else(|| current.clone())
    };
    let error = optional(&state, "error")?.and_then(|error| error.as_string());
    let description = error.clone().map_or_else(
        || translated(&translate, "description"),
        |error| Ok(JsValue::from_str(&error)),
    )?;
    let select = required_function(props, "select", "AgentPresetRow props")?;
    let on_select = Closure::wrap(Box::new(move |id: String| {
        let _ = select.call1(&JsValue::UNDEFINED, &JsValue::from_str(&id));
    }) as Box<dyn FnMut(String)>);
    let menu = create_element(
        &modules.react,
        &configured_components()?.menu,
        Some(&object(&[
            ("options", options.into()),
            ("selectedId", JsValue::from_str(&current)),
            ("label", JsValue::from_str(&label)),
            ("t", translate.clone().into()),
            ("buttonClassName", JsValue::from_str(&css("selector"))),
            ("chevronClassName", JsValue::from_str(&css("chevron"))),
            (
                "disabled",
                JsValue::from_bool(
                    matches!(status.as_str(), "loading" | "saving")
                        || !writable
                        || Array::from(&required(
                            &state,
                            "options",
                            "Agent preset settings state",
                        )?)
                        .length()
                            == 0,
                ),
            ),
            ("open", JsValue::from_bool(open)),
            ("onOpenChange", set_open.into()),
            ("onSelect", on_select.into_js_value()),
            ("align", JsValue::from_str("end")),
        ])?),
        &[],
    )?;
    tag(
        &modules.react,
        "div",
        Some(&class("row")?),
        &[
            tag(
                &modules.react,
                "div",
                Some(&class("rowText")?),
                &[
                    tag(
                        &modules.react,
                        "div",
                        Some(&class("title")?),
                        &[translated(&translate, "title")?],
                    )?,
                    tag(
                        &modules.react,
                        "div",
                        Some(&object(&[
                            ("className", JsValue::from_str(&css("desc"))),
                            (
                                "role",
                                error.map_or(JsValue::UNDEFINED, |_| JsValue::from_str("alert")),
                            ),
                        ])?),
                        &[description],
                    )?,
                ],
            )?,
            menu,
        ],
    )
}

fn intro_stagger_ms(count: usize) -> f64 {
    if count <= 1 {
        0.0
    } else {
        #[allow(clippy::cast_precision_loss)]
        let gaps = (count - 1) as f64;
        INTRO_CHAR_STAGGER_MS.min(INTRO_TEXT_REVEAL_MS / gaps)
    }
}

#[allow(clippy::too_many_lines)] // Seat motion, menu, and staged-state gates are one source component.
fn render_seat(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let state = required_function(props, "useAgentPresetSeat", "AgentPresetSeat props")?
        .call1(&JsValue::UNDEFINED, &identity_selector())?;
    let (open, set_open) = use_state(&modules.react, &JsValue::FALSE)?;
    let open = open.as_bool().unwrap_or(false);
    let load = required_function(props, "load", "AgentPresetSeat props")?;
    let load_effect = Closure::wrap(Box::new(move || {
        let _ = load.call0(&JsValue::UNDEFINED);
    }) as Box<dyn FnMut()>);
    use_effect(
        &modules.react,
        load_effect.into_js_value(),
        &Array::of1(&required(props, "load", "AgentPresetSeat props")?),
    )?;
    let options = Array::from(&required(&state, "options", "Agent preset seat state")?);
    let current = required(&state, "current", "Agent preset seat state")?
        .as_string()
        .unwrap_or_default();
    let translate = required_function(props, "t", "AgentPresetSeat props")?;
    let chosen = options.iter().find(|option| {
        Reflect::get(option, &JsValue::from_str("id"))
            .ok()
            .and_then(|id| id.as_string())
            .as_deref()
            == Some(current.as_str())
    });
    let label = chosen
        .as_ref()
        .map(|option| preset_text(option, &translate).map(|(name, _)| name))
        .transpose()?
        .unwrap_or_else(|| current.clone());
    let ready = options.length() > 0 && !current.is_empty();
    let (introducing, set_introducing) = use_state(&modules.react, &JsValue::FALSE)?;
    let introducing = introducing.as_bool().unwrap_or(false);
    let introduce = required(&state, "introduce", "Agent preset seat state")?
        .as_bool()
        .unwrap_or(false);
    let introduced = required_function(props, "introduced", "AgentPresetSeat props")?;
    let effect_label = label.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if !introduce || !ready {
            return Ok(JsValue::UNDEFINED);
        }
        let characters = effect_label.chars().count();
        let window = required(&js_sys::global(), "window", "browser global")?;
        let media = crate::browser::call_method(
            &window,
            "matchMedia",
            &[JsValue::from_str("(prefers-reduced-motion: reduce)")],
        )?;
        let reduced = required(&media, "matches", "MediaQueryList")?
            .as_bool()
            .unwrap_or(false);
        if characters == 0 || reduced {
            introduced.call0(&JsValue::UNDEFINED)?;
            return Ok(JsValue::UNDEFINED);
        }
        set_introducing.call1(&JsValue::UNDEFINED, &JsValue::TRUE)?;
        let finish_setter = set_introducing.clone();
        let finish_introduced = introduced.clone();
        let finish = Closure::wrap(Box::new(move || {
            let _ = finish_setter.call1(&JsValue::UNDEFINED, &JsValue::FALSE);
            let _ = finish_introduced.call0(&JsValue::UNDEFINED);
        }) as Box<dyn FnMut()>);
        #[allow(clippy::cast_precision_loss)]
        let delay = INTRO_TEXT_DELAY_MS
            + (characters.saturating_sub(1) as f64) * intro_stagger_ms(characters)
            + INTRO_CHAR_FADE_MS;
        let timer = crate::browser::call_method(
            &window,
            "setTimeout",
            &[finish.into_js_value(), JsValue::from_f64(delay)],
        )?;
        let cleanup_window = window;
        Ok(Closure::wrap(Box::new(move || {
            let _ = crate::browser::call_method(
                &cleanup_window,
                "clearTimeout",
                std::slice::from_ref(&timer),
            );
        }) as Box<dyn FnMut()>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    use_effect(
        &modules.react,
        effect.into_js_value(),
        &Array::of4(
            &JsValue::from_bool(introduce),
            &JsValue::from_bool(ready),
            &JsValue::from_str(&label),
            &required(props, "introduced", "AgentPresetSeat props")?,
        ),
    )?;
    if !ready {
        return Ok(JsValue::NULL);
    }
    let characters = label.chars().collect::<Vec<_>>();
    let shown_label = if introducing {
        let stagger = intro_stagger_ms(characters.len());
        let children = characters
            .iter()
            .enumerate()
            .map(|(index, character)| {
                let key = u32::try_from(index)
                    .map_err(|_| js_sys::RangeError::new("character index exceeds u32"))?;
                #[allow(clippy::cast_precision_loss)]
                let delay = INTRO_TEXT_DELAY_MS + index as f64 * stagger;
                tag(
                    &modules.react,
                    "span",
                    Some(&object(&[
                        ("key", JsValue::from_f64(f64::from(key))),
                        ("className", JsValue::from_str(&css("introChar"))),
                        (
                            "style",
                            object(&[(
                                "animationDelay",
                                JsValue::from_str(&format!("{delay}ms")),
                            )])?
                            .into(),
                        ),
                    ])?),
                    &[JsValue::from_str(&character.to_string())],
                )
            })
            .collect::<Result<Vec<_>, JsValue>>()?;
        tag(
            &modules.react,
            "span",
            Some(&class("introText")?),
            &children,
        )?
    } else {
        JsValue::from_str(&label)
    };
    let items = Array::new();
    for option in options.iter() {
        let (name, description) = preset_text(&option, &translate)?;
        let item = tag(
            &modules.react,
            "span",
            Some(&class("item")?),
            &[
                tag(
                    &modules.react,
                    "span",
                    Some(&class("itemName")?),
                    &[JsValue::from_str(&name)],
                )?,
                tag(
                    &modules.react,
                    "span",
                    Some(&class("itemDesc")?),
                    &[description.map_or_else(
                        || translated(&translate, "noDescription"),
                        |description| Ok(JsValue::from_str(&description)),
                    )?],
                )?,
            ],
        )?;
        let item: JsValue = object(&[
            ("id", required(&option, "id", "Agent preset option")?),
            ("label", item),
        ])?
        .into();
        items.push(&item);
    }
    let close_setter = set_open.clone();
    let on_close = Closure::wrap(Box::new(move || {
        let _ = close_setter.call1(&JsValue::UNDEFINED, &JsValue::FALSE);
    }) as Box<dyn FnMut()>);
    let select = required_function(props, "select", "AgentPresetSeat props")?;
    let selected_setter = set_open.clone();
    let on_select = Closure::wrap(Box::new(move |id: String| -> Result<(), JsValue> {
        selected_setter.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
        select.call1(&JsValue::UNDEFINED, &JsValue::from_str(&id))?;
        Ok(())
    }) as Box<dyn FnMut(String) -> Result<(), JsValue>>);
    let toggle_setter = set_open;
    let toggle = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let updater = Closure::wrap(Box::new(move |value: JsValue| {
            JsValue::from_bool(!value.as_bool().unwrap_or(false))
        }) as Box<dyn FnMut(JsValue) -> JsValue>);
        toggle_setter.call1(&JsValue::UNDEFINED, &updater.into_js_value())?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let error = optional(&state, "error")?.and_then(|error| error.as_string());
    let icon_class = if introducing {
        format!("{} {}", css("seatIcon"), css("introIcon"))
    } else {
        css("seatIcon")
    };
    let anchor = tag(
        &modules.react,
        "button",
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            ("className", JsValue::from_str(&css("seat"))),
            ("aria-haspopup", JsValue::from_str("menu")),
            ("aria-expanded", JsValue::from_bool(open)),
            (
                "title",
                error.map_or_else(
                    || translated(&translate, "seatHint"),
                    |error| Ok(JsValue::from_str(&error)),
                )?,
            ),
            (
                "disabled",
                required(&state, "busy", "Agent preset seat state")?,
            ),
            ("onClick", toggle.into_js_value()),
        ])?),
        &[
            create_element(
                &modules.react,
                &modules.primitive("IconAgentPresetOutline16")?,
                Some(&object(&[("className", JsValue::from_str(&icon_class))])?),
                &[],
            )?,
            shown_label,
            create_element(
                &modules.react,
                &modules.primitive("IconChevronDownOutline14")?,
                Some(&class("chevron")?),
                &[],
            )?,
        ],
    )?;
    create_element(
        &modules.react,
        &modules.primitive("Menu")?,
        Some(&object(&[
            ("open", JsValue::from_bool(open)),
            ("onClose", on_close.into_js_value()),
            ("items", items.into()),
            ("selectedId", JsValue::from_str(&current)),
            ("onSelect", on_select.into_js_value()),
            ("align", JsValue::from_str("start")),
            ("portal", JsValue::TRUE),
            ("anchor", anchor),
        ])?),
        &[],
    )
}
