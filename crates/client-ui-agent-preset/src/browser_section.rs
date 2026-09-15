//! Compiled Agent preset management section.

use std::cell::RefCell;

use js_sys::{Array, Function, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};

use crate::{
    browser::{
        class, create_element, css, inject_prefixed_style, object, optional, required,
        required_function, tag, translated,
    },
    browser_components::{BrowserModules, preset_text},
};

const SECTION_CSS: &str = include_str!(
    "../../../packages/client/ui-agent-preset/src/client/AgentPresetSection.module.css"
);

thread_local! {
    static DESCRIPTION: RefCell<Option<JsValue>> = const { RefCell::new(None) };
}

pub(crate) fn configure_section_component(modules: &BrowserModules) -> Result<JsValue, JsValue> {
    inject_prefixed_style("AgentPresetSection", SECTION_CSS)?;
    let description = component(modules, render_card_description);
    DESCRIPTION.with(|configured| *configured.borrow_mut() = Some(description));
    Ok(component(modules, render_section))
}

type Renderer = fn(&BrowserModules, &JsValue) -> Result<JsValue, JsValue>;

fn component(modules: &BrowserModules, renderer: Renderer) -> JsValue {
    let modules = modules.clone();
    Closure::wrap(Box::new(move |props: JsValue| renderer(&modules, &props))
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value()
}

fn description_component() -> Result<JsValue, JsValue> {
    DESCRIPTION.with(|configured| {
        configured
            .borrow()
            .clone()
            .ok_or_else(|| js_sys::Error::new("Agent preset section was not configured").into())
    })
}

fn use_state(react: &JsValue, initial: &JsValue) -> Result<(JsValue, Function), JsValue> {
    let pair = Array::from(&required_function(react, "useState", "React")?.call1(react, initial)?);
    Ok((pair.get(0), pair.get(1).dyn_into()?))
}

fn use_effect(
    react: &JsValue,
    method: &str,
    effect: JsValue,
    dependencies: &Array,
) -> Result<(), JsValue> {
    let result = required_function(react, method, "React")?
        .call2(react, &effect, dependencies)
        .map(|_| ());
    drop(effect);
    result
}

fn identity_selector() -> JsValue {
    Closure::wrap(Box::new(move |value: JsValue| value) as Box<dyn FnMut(JsValue) -> JsValue>)
        .into_js_value()
}

fn render_card_description(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let text = required(props, "text", "CardDescription props")?
        .as_string()
        .unwrap_or_default();
    let reference = required_function(&modules.react, "useRef", "React")?
        .call1(&modules.react, &JsValue::NULL)?;
    let (truncated, set_truncated) = use_state(&modules.react, &JsValue::FALSE)?;
    let effect_ref = reference.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let element = Reflect::get(&effect_ref, &JsValue::from_str("current"))?;
        if element.is_null() || element.is_undefined() {
            return Ok(JsValue::UNDEFINED);
        }
        let measure_element = element.clone();
        let measure_setter = set_truncated.clone();
        let measure = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            let scroll = required(&measure_element, "scrollHeight", "description element")?
                .as_f64()
                .unwrap_or(0.0);
            let client = required(&measure_element, "clientHeight", "description element")?
                .as_f64()
                .unwrap_or(0.0);
            measure_setter.call1(&JsValue::UNDEFINED, &JsValue::from_bool(scroll > client))?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>);
        let measure = measure.into_js_value();
        measure
            .clone()
            .dyn_into::<Function>()?
            .call0(&JsValue::UNDEFINED)?;
        let constructor = Reflect::get(&js_sys::global(), &JsValue::from_str("ResizeObserver"))?;
        if constructor.is_null() || constructor.is_undefined() {
            return Ok(JsValue::UNDEFINED);
        }
        let observer =
            Reflect::construct(&constructor.dyn_into::<Function>()?, &Array::of1(&measure))?;
        crate::browser::call_method(&observer, "observe", std::slice::from_ref(&element))?;
        Ok(Closure::wrap(Box::new(move || {
            let _ = crate::browser::call_method(&observer, "disconnect", &[]);
        }) as Box<dyn FnMut()>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    use_effect(
        &modules.react,
        "useLayoutEffect",
        effect.into_js_value(),
        &Array::of1(&JsValue::from_str(&text)),
    )?;
    let span = tag(
        &modules.react,
        "span",
        Some(&object(&[
            ("ref", reference),
            ("className", JsValue::from_str(&css("cardDesc"))),
            ("title", JsValue::from_str("")),
        ])?),
        &[JsValue::from_str(&text)],
    )?;
    create_element(
        &modules.react,
        &modules.primitive("Tooltip")?,
        Some(&object(&[
            ("label", JsValue::from_str(&text)),
            ("side", JsValue::from_str("bottom")),
            ("delayMs", JsValue::from_f64(400.0)),
            (
                "disabled",
                JsValue::from_bool(!truncated.as_bool().unwrap_or(false)),
            ),
            ("maxWidth", JsValue::from_f64(360.0)),
        ])?),
        &[span],
    )
}

fn action0(props: &JsValue, name: &str, owner: &str) -> Result<JsValue, JsValue> {
    let action = required_function(props, name, owner)?;
    Ok(Closure::wrap(Box::new(move || {
        let _ = action.call0(&JsValue::UNDEFINED);
    }) as Box<dyn FnMut()>)
    .into_js_value())
}

fn action1(props: &JsValue, name: &str, value: JsValue, owner: &str) -> Result<JsValue, JsValue> {
    let action = required_function(props, name, owner)?;
    Ok(Closure::wrap(Box::new(move || {
        let _ = action.call1(&JsValue::UNDEFINED, &value);
    }) as Box<dyn FnMut()>)
    .into_js_value())
}

fn event_value(event: &JsValue) -> Result<String, JsValue> {
    required(
        &required(event, "target", "input event")?,
        "value",
        "input target",
    )?
    .as_string()
    .ok_or_else(|| js_sys::TypeError::new("input target value must be a string").into())
}

fn copy_blocker(copy: &JsValue, rows: &Array) -> Result<Option<&'static str>, JsValue> {
    let id = required(copy, "id", "copy draft")?
        .as_string()
        .unwrap_or_default();
    if id.is_empty() {
        return Ok(Some("idRequired"));
    }
    let valid = id.bytes().enumerate().all(|(index, byte)| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || (index > 0 && byte == b'-')
    });
    if !valid {
        return Ok(Some("idInvalid"));
    }
    if rows.iter().any(|row| {
        Reflect::get(&row, &JsValue::from_str("id"))
            .ok()
            .and_then(|value| value.as_string())
            .as_deref()
            == Some(id.as_str())
    }) {
        return Ok(Some("idTaken"));
    }
    Ok(None)
}

#[allow(clippy::too_many_lines)] // Copy dialog owns its exact Modal and field contract.
fn copy_dialog(
    modules: &BrowserModules,
    props: &JsValue,
    state: &JsValue,
    translate: &Function,
    rows: &Array,
) -> Result<JsValue, JsValue> {
    let copy = optional(state, "copy")?;
    let blocker = copy
        .as_ref()
        .map(|copy| copy_blocker(copy, rows))
        .transpose()?
        .flatten();
    let source_title = if let Some(copy) = &copy {
        let from = required(copy, "from", "copy draft")?
            .as_string()
            .unwrap_or_default();
        if let Some(row) = rows.iter().find(|row| {
            Reflect::get(row, &JsValue::from_str("id"))
                .ok()
                .and_then(|value| value.as_string())
                .as_deref()
                == Some(from.as_str())
        }) {
            preset_text(&row, translate)?.0
        } else {
            required(copy, "fromTitle", "copy draft")?
                .as_string()
                .unwrap_or_default()
        }
    } else {
        String::new()
    };
    let title = if copy.is_some() {
        format!(
            "{} · {} {source_title}",
            translated(translate, "copyTitle")?
                .as_string()
                .unwrap_or_default(),
            translated(translate, "copyOf")?
                .as_string()
                .unwrap_or_default()
        )
    } else {
        translated(translate, "copyTitle")?
            .as_string()
            .unwrap_or_default()
    };
    let saving = copy
        .as_ref()
        .and_then(|copy| optional(copy, "saving").ok().flatten())
        .and_then(|value| value.as_bool())
        == Some(true);
    let cancel_button = create_element(
        &modules.react,
        &modules.primitive("Button")?,
        Some(&object(&[
            ("variant", JsValue::from_str("outline")),
            ("disabled", JsValue::from_bool(saving)),
            (
                "onClick",
                action0(props, "cancelCopy", "AgentPresetSection props")?,
            ),
        ])?),
        &[translated(translate, "cancel")?],
    )?;
    let create_button = create_element(
        &modules.react,
        &modules.primitive("Button")?,
        Some(&object(&[
            (
                "disabled",
                JsValue::from_bool(copy.is_none() || saving || blocker.is_some()),
            ),
            (
                "onClick",
                action0(props, "confirmCopy", "AgentPresetSection props")?,
            ),
        ])?),
        &[translated(
            translate,
            if saving { "creating" } else { "create" },
        )?],
    )?;
    let footer = create_element(
        &modules.react,
        &required(&modules.react, "Fragment", "React")?,
        None,
        &[cancel_button, create_button],
    )?;
    let children = if let Some(copy) = &copy {
        let id_edit = required_function(props, "setCopyId", "AgentPresetSection props")?;
        let id_change = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
            id_edit.call1(
                &JsValue::UNDEFINED,
                &JsValue::from_str(&event_value(&event)?),
            )?;
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
        let name_edit = required_function(props, "setCopyName", "AgentPresetSection props")?;
        let name_change = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
            name_edit.call1(
                &JsValue::UNDEFINED,
                &JsValue::from_str(&event_value(&event)?),
            )?;
            Ok(())
        })
            as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
        let message =
            if let Some(error) = optional(copy, "error")?.and_then(|error| error.as_string()) {
                Some(error)
            } else if let Some(blocker) = blocker {
                translated(translate, blocker)?.as_string()
            } else {
                None
            };
        let mut fields = vec![
            input_field(
                modules,
                translate,
                "presetId",
                "presetIdPlaceholder",
                required(copy, "id", "copy draft")?,
                true,
                id_change.into_js_value(),
            )?,
            input_field(
                modules,
                translate,
                "displayName",
                "displayNamePlaceholder",
                required(copy, "name", "copy draft")?,
                false,
                name_change.into_js_value(),
            )?,
        ];
        if let Some(message) = message {
            fields.push(tag(
                &modules.react,
                "p",
                Some(&object(&[
                    ("className", JsValue::from_str(&css("error"))),
                    ("role", JsValue::from_str("alert")),
                ])?),
                &[JsValue::from_str(&message)],
            )?);
        }
        tag(
            &modules.react,
            "div",
            Some(&class("dialogFields")?),
            &fields,
        )?
    } else {
        JsValue::NULL
    };
    create_element(
        &modules.react,
        &modules.primitive("Modal")?,
        Some(&object(&[
            ("open", JsValue::from_bool(copy.is_some())),
            (
                "onClose",
                action0(props, "cancelCopy", "AgentPresetSection props")?,
            ),
            ("title", JsValue::from_str(&title)),
            ("closeLabel", translated(translate, "close")?),
            ("description", translated(translate, "copyIntro")?),
            ("className", JsValue::from_str(&css("dialog"))),
            ("footer", footer),
        ])?),
        &[children],
    )
}

fn input_field(
    modules: &BrowserModules,
    translate: &Function,
    label: &str,
    placeholder: &str,
    value: JsValue,
    autofocus: bool,
    on_change: JsValue,
) -> Result<JsValue, JsValue> {
    tag(
        &modules.react,
        "label",
        Some(&class("field")?),
        &[
            tag(
                &modules.react,
                "span",
                Some(&class("fieldLabel")?),
                &[translated(translate, label)?],
            )?,
            tag(
                &modules.react,
                "input",
                Some(&object(&[
                    ("className", JsValue::from_str(&css("input"))),
                    ("value", value),
                    ("autoFocus", JsValue::from_bool(autofocus)),
                    ("spellCheck", JsValue::FALSE),
                    ("placeholder", translated(translate, placeholder)?),
                    ("onChange", on_change),
                ])?),
                &[],
            )?,
        ],
    )
}

#[allow(clippy::too_many_lines)] // One roster card owns its complete trust/action grammar.
fn render_card(
    modules: &BrowserModules,
    props: &JsValue,
    state: &JsValue,
    row: &JsValue,
    translate: &Function,
) -> Result<JsValue, JsValue> {
    let id = required(row, "id", "preset row")?
        .as_string()
        .unwrap_or_default();
    let trust = required(row, "trust", "preset row")?
        .as_string()
        .unwrap_or_default();
    let is_default = required(row, "isDefault", "preset row")?
        .as_bool()
        .unwrap_or(false);
    let broken = optional(row, "broken")?.and_then(|value| value.as_string());
    let (name, description) = preset_text(row, translate)?;
    let authorable = required(state, "authorable", "section state")?
        .as_bool()
        .unwrap_or(false);
    let has_document = required(state, "hasDocument", "section state")?
        .as_bool()
        .unwrap_or(false);
    let choose_key = if broken.is_some() {
        "brokenBadge"
    } else if is_default {
        "inUse"
    } else {
        "setDefault"
    };
    let choose = action1(
        props,
        "makeDefault",
        JsValue::from_str(&id),
        "AgentPresetSection props",
    )?;
    let mut head = vec![tag(
        &modules.react,
        "span",
        Some(&class("cardName")?),
        &[JsValue::from_str(&name)],
    )?];
    if broken.is_some() {
        head.push(tag(
            &modules.react,
            "span",
            Some(&class("brokenBadge")?),
            &[translated(translate, "brokenBadge")?],
        )?);
    }
    head.push(tag(
        &modules.react,
        "span",
        Some(&class("badge")?),
        &[translated(
            translate,
            if trust == "user" {
                "userTrust"
            } else {
                "builtIn"
            },
        )?],
    )?);
    if is_default {
        head.push(tag(
            &modules.react,
            "span",
            Some(&class("inUse")?),
            &[translated(translate, "inUse")?],
        )?);
    }
    let mut main = vec![
        tag(&modules.react, "span", Some(&class("cardHead")?), &head)?,
        create_element(
            &modules.react,
            &description_component()?,
            Some(&object(&[(
                "text",
                description.map_or_else(
                    || translated(translate, "noDescription"),
                    |description| Ok(JsValue::from_str(&description)),
                )?,
            )])?),
            &[],
        )?,
    ];
    if let Some(broken) = &broken {
        main.push(tag(
            &modules.react,
            "span",
            Some(&object(&[
                ("className", JsValue::from_str(&css("cardBrokenReason"))),
                ("role", JsValue::from_str("alert")),
            ])?),
            &[JsValue::from_str(broken)],
        )?);
    }
    main.push(tag(
        &modules.react,
        "code",
        Some(&class("cardId")?),
        &[JsValue::from_str(&id)],
    )?);
    let main_button = tag(
        &modules.react,
        "button",
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            ("className", JsValue::from_str(&css("cardMain"))),
            ("aria-pressed", JsValue::from_bool(is_default)),
            (
                "disabled",
                JsValue::from_bool(is_default || broken.is_some()),
            ),
            (
                "aria-label",
                JsValue::from_str(&format!(
                    "{}: {name}",
                    translated(translate, choose_key)?
                        .as_string()
                        .unwrap_or_default()
                )),
            ),
            (
                "title",
                broken.clone().map_or_else(
                    || translated(translate, if is_default { "inUse" } else { "setDefault" }),
                    |broken| Ok(JsValue::from_str(&broken)),
                )?,
            ),
            ("onClick", choose),
        ])?),
        &main,
    )?;
    let mut actions = Vec::new();
    if trust == "system" && broken.is_none() {
        actions.push(icon_action(
            modules,
            props,
            translate,
            "view",
            &name,
            "view",
            &id,
            "IconBrowseOutline16",
            false,
            None,
        )?);
    } else if trust == "user" {
        let location_key = if has_document {
            "openLocation"
        } else {
            "showLocation"
        };
        actions.push(icon_action(
            modules,
            props,
            translate,
            location_key,
            &name,
            "openLocation",
            &id,
            "IconFolderOpenOutline16",
            false,
            None,
        )?);
    }
    actions.push(icon_action(
        modules,
        props,
        translate,
        "duplicate",
        &name,
        "beginCopy",
        &id,
        "IconCopyOutline16",
        !authorable || broken.is_some(),
        Some(if broken.is_some() {
            "brokenNoCopy"
        } else if authorable {
            "duplicate"
        } else {
            "duplicateUnavailable"
        }),
    )?);
    if trust == "user" {
        actions.push(icon_action(
            modules,
            props,
            translate,
            "delete",
            &name,
            "confirmDelete",
            &id,
            "IconTrashOutline16",
            false,
            None,
        )?);
    }
    let mut card = vec![
        main_button,
        tag(&modules.react, "div", Some(&class("cardFoot")?), &actions)?,
    ];
    let revealed = required(state, "revealedPaths", "section state")?;
    if let Some(path) = optional(&revealed, &id)?.and_then(|value| value.as_string()) {
        card.push(tag(
            &modules.react,
            "p",
            Some(&class("revealedPath")?),
            &[
                tag(
                    &modules.react,
                    "span",
                    Some(&class("revealedPathLabel")?),
                    &[translated(translate, "revealedPathLabel")?],
                )?,
                tag(&modules.react, "code", None, &[JsValue::from_str(&path)])?,
            ],
        )?);
    }
    let classes = if broken.is_some() {
        format!("{} {}", css("card"), css("cardBroken"))
    } else if is_default {
        format!("{} {}", css("card"), css("cardActive"))
    } else {
        css("card")
    };
    tag(
        &modules.react,
        "li",
        Some(&object(&[
            ("key", JsValue::from_str(&id)),
            ("className", JsValue::from_str(&classes)),
        ])?),
        &card,
    )
}

#[allow(clippy::too_many_arguments)]
fn icon_action(
    modules: &BrowserModules,
    props: &JsValue,
    translate: &Function,
    label_key: &str,
    name: &str,
    action: &str,
    id: &str,
    icon: &str,
    disabled: bool,
    tip_key: Option<&str>,
) -> Result<JsValue, JsValue> {
    let class_name = if label_key == "delete" {
        format!("{} {}", css("iconButton"), css("iconDanger"))
    } else {
        css("iconButton")
    };
    tag(
        &modules.react,
        "button",
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            ("className", JsValue::from_str(&class_name)),
            ("disabled", JsValue::from_bool(disabled)),
            (
                "data-tip",
                translated(translate, tip_key.unwrap_or(label_key))?,
            ),
            (
                "aria-label",
                JsValue::from_str(&format!(
                    "{}: {name}",
                    translated(translate, label_key)?
                        .as_string()
                        .unwrap_or_default()
                )),
            ),
            (
                "onClick",
                action1(
                    props,
                    action,
                    JsValue::from_str(id),
                    "AgentPresetSection props",
                )?,
            ),
        ])?),
        &[create_element(
            &modules.react,
            &modules.primitive(icon)?,
            None,
            &[],
        )?],
    )
}

#[allow(clippy::too_many_lines)] // Whole management surface follows the source's grouped card and Modal grammar.
fn render_section(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let state = required_function(props, "useAgentPresetSection", "AgentPresetSection props")?
        .call1(&JsValue::UNDEFINED, &identity_selector())?;
    let load = required_function(props, "load", "AgentPresetSection props")?;
    let load_effect = Closure::wrap(Box::new(move || {
        let _ = load.call0(&JsValue::UNDEFINED);
    }) as Box<dyn FnMut()>);
    use_effect(
        &modules.react,
        "useEffect",
        load_effect.into_js_value(),
        &Array::of1(&required(props, "load", "AgentPresetSection props")?),
    )?;
    let status = required(&state, "status", "section state")?
        .as_string()
        .unwrap_or_default();
    if status == "unavailable" {
        return Ok(JsValue::NULL);
    }
    let translate = required_function(props, "t", "AgentPresetSection props")?;
    if status == "error" {
        let detail = optional(&state, "error")?
            .and_then(|error| error.as_string())
            .unwrap_or_default();
        return tag(
            &modules.react,
            "div",
            Some(&class("section")?),
            &[
                tag(
                    &modules.react,
                    "p",
                    Some(&object(&[
                        ("className", JsValue::from_str(&css("error"))),
                        ("role", JsValue::from_str("alert")),
                    ])?),
                    &[JsValue::from_str(&format!(
                        "{} {detail}",
                        translated(&translate, "error")?
                            .as_string()
                            .unwrap_or_default()
                    ))],
                )?,
                tag(
                    &modules.react,
                    "button",
                    Some(&object(&[
                        ("type", JsValue::from_str("button")),
                        ("className", JsValue::from_str(&css("secondaryButton"))),
                        (
                            "onClick",
                            action0(props, "load", "AgentPresetSection props")?,
                        ),
                    ])?),
                    &[translated(&translate, "retry")?],
                )?,
            ],
        );
    }
    let rows = Array::from(&required(&state, "rows", "section state")?);
    let authorable = required(&state, "authorable", "section state")?
        .as_bool()
        .unwrap_or(false);
    let has_cordis = rows.iter().any(|row| {
        Reflect::get(&row, &JsValue::from_str("id"))
            .ok()
            .and_then(|value| value.as_string())
            .as_deref()
            == Some("cordis")
    });
    let creator = if has_cordis && optional(props, "startCreatorDraft")?.is_some() {
        let start = required_function(props, "startCreatorDraft", "AgentPresetSection props")?;
        let close = required_function(props, "close", "AgentPresetSection props")?;
        let click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            start.call0(&JsValue::UNDEFINED)?;
            close.call0(&JsValue::UNDEFINED)?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>);
        Some(tag(
            &modules.react,
            "button",
            Some(&object(&[
                ("type", JsValue::from_str("button")),
                ("className", JsValue::from_str(&css("creatorButton"))),
                ("disabled", JsValue::from_bool(!authorable)),
                (
                    "title",
                    if authorable {
                        JsValue::UNDEFINED
                    } else {
                        translated(&translate, "duplicateUnavailable")?
                    },
                ),
                ("onClick", click.into_js_value()),
            ])?),
            &[
                create_element(
                    &modules.react,
                    &modules.primitive("IconPlusOutline16")?,
                    Some(&object(&[("size", JsValue::from_f64(14.0))])?),
                    &[],
                )?,
                translated(&translate, "creatorDraft")?,
            ],
        )?)
    } else {
        None
    };
    let mut body = vec![
        tag(
            &modules.react,
            "h2",
            Some(&class("title")?),
            &[translated(&translate, "nav")?],
        )?,
        tag(
            &modules.react,
            "p",
            Some(&class("intro")?),
            &[translated(&translate, "sectionIntro")?],
        )?,
    ];
    if let Some(error) = optional(&state, "error")?.and_then(|error| error.as_string()) {
        body.push(tag(
            &modules.react,
            "p",
            Some(&object(&[
                ("className", JsValue::from_str(&css("error"))),
                ("role", JsValue::from_str("alert")),
            ])?),
            &[JsValue::from_str(&error)],
        )?);
    }
    for (trust, heading) in [("system", "builtInGroup"), ("user", "customGroup")] {
        let group = rows
            .iter()
            .filter(|row| {
                Reflect::get(row, &JsValue::from_str("trust"))
                    .ok()
                    .and_then(|value| value.as_string())
                    .as_deref()
                    == Some(trust)
            })
            .collect::<Vec<_>>();
        let tail = (trust == "user").then(|| creator.clone()).flatten();
        if group.is_empty() && tail.is_none() {
            continue;
        }
        let mut children = vec![tag(
            &modules.react,
            "h3",
            Some(&class("groupHead")?),
            &[translated(&translate, heading)?],
        )?];
        if !group.is_empty() {
            let cards = group
                .iter()
                .map(|row| render_card(modules, props, &state, row, &translate))
                .collect::<Result<Vec<_>, JsValue>>()?;
            children.push(tag(&modules.react, "ul", Some(&class("cards")?), &cards)?);
        }
        if let Some(tail) = tail {
            children.push(tail);
        }
        body.push(tag(
            &modules.react,
            "section",
            Some(&object(&[
                ("key", JsValue::from_str(trust)),
                ("className", JsValue::from_str(&css("group"))),
            ])?),
            &children,
        )?);
    }
    body.push(copy_dialog(modules, props, &state, &translate, &rows)?);
    body.push(viewer_modal(modules, props, &state, &translate, &rows)?);
    body.push(delete_modal(modules, props, &state, &translate)?);
    tag(&modules.react, "div", Some(&class("section")?), &body)
}

fn viewer_modal(
    modules: &BrowserModules,
    props: &JsValue,
    state: &JsValue,
    translate: &Function,
    rows: &Array,
) -> Result<JsValue, JsValue> {
    let view = optional(state, "view")?;
    let title = if let Some(view) = &view {
        let id = required(view, "id", "preset view")?
            .as_string()
            .unwrap_or_default();
        let row = rows.iter().find(|row| {
            Reflect::get(row, &JsValue::from_str("id"))
                .ok()
                .and_then(|value| value.as_string())
                .as_deref()
                == Some(id.as_str())
        });
        row.as_ref()
            .map(|row| preset_text(row, translate).map(|(name, _)| name))
            .transpose()?
            .unwrap_or(
                required(view, "title", "preset view")?
                    .as_string()
                    .unwrap_or_default(),
            )
    } else {
        String::new()
    };
    let close = action0(props, "closeView", "AgentPresetSection props")?;
    let button = create_element(
        &modules.react,
        &modules.primitive("Button")?,
        Some(&object(&[
            ("variant", JsValue::from_str("outline")),
            ("autoFocus", JsValue::TRUE),
            ("onClick", close.clone()),
        ])?),
        &[translated(translate, "close")?],
    )?;
    let content = view.as_ref().map_or(Ok(JsValue::NULL), |view| {
        tag(
            &modules.react,
            "pre",
            Some(&class("viewerCode")?),
            &[required(view, "content", "preset view")?],
        )
    })?;
    create_element(
        &modules.react,
        &modules.primitive("Modal")?,
        Some(&object(&[
            ("open", JsValue::from_bool(view.is_some())),
            ("onClose", close),
            (
                "title",
                if view.is_some() {
                    JsValue::from_str(&format!(
                        "{} · {title}",
                        translated(translate, "view")?
                            .as_string()
                            .unwrap_or_default()
                    ))
                } else {
                    JsValue::from_str("")
                },
            ),
            ("closeLabel", translated(translate, "close")?),
            ("description", translated(translate, "composition")?),
            ("className", JsValue::from_str(&css("dialog"))),
            ("footer", button),
        ])?),
        &[content],
    )
}

fn delete_modal(
    modules: &BrowserModules,
    props: &JsValue,
    state: &JsValue,
    translate: &Function,
) -> Result<JsValue, JsValue> {
    let pending = optional(state, "pendingDelete")?;
    let deleting = required(state, "deleting", "section state")?
        .as_bool()
        .unwrap_or(false);
    let cancel = action1(
        props,
        "confirmDelete",
        JsValue::NULL,
        "AgentPresetSection props",
    )?;
    let cancel_button = create_element(
        &modules.react,
        &modules.primitive("Button")?,
        Some(&object(&[
            ("variant", JsValue::from_str("outline")),
            ("autoFocus", JsValue::TRUE),
            ("disabled", JsValue::from_bool(deleting)),
            ("onClick", cancel.clone()),
        ])?),
        &[translated(translate, "cancel")?],
    )?;
    let remove = create_element(
        &modules.react,
        &modules.primitive("Button")?,
        Some(&object(&[
            ("variant", JsValue::from_str("outline")),
            ("className", JsValue::from_str(&css("deleteConfirm"))),
            ("disabled", JsValue::from_bool(deleting)),
            (
                "onClick",
                action0(props, "remove", "AgentPresetSection props")?,
            ),
        ])?),
        &[translated(
            translate,
            if deleting {
                "deleting"
            } else {
                "deleteConfirm"
            },
        )?],
    )?;
    let footer = create_element(
        &modules.react,
        &required(&modules.react, "Fragment", "React")?,
        None,
        &[cancel_button, remove],
    )?;
    create_element(
        &modules.react,
        &modules.primitive("Modal")?,
        Some(&object(&[
            ("open", JsValue::from_bool(pending.is_some())),
            ("onClose", cancel),
            ("title", translated(translate, "deleteTitle")?),
            ("closeLabel", translated(translate, "close")?),
            ("description", translated(translate, "deleteDescription")?),
            ("className", JsValue::from_str(&css("deleteDialog"))),
            ("footer", footer),
        ])?),
        &[],
    )
}
