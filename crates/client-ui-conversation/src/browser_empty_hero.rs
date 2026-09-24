//! Compiled blank-session hero chrome, workspace chip, and glow.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::browser_reasoning::inject_style;

const HERO_CSS: &str = include_str!(
    "../../../packages/client/ui-conversation/src/client/skeleton/HeroShell.module.css"
);

thread_local! {
    static COMPONENTS: RefCell<Option<HeroComponents>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    fish_logo: JsValue,
    chevron: JsValue,
    folder_closed: JsValue,
    folder_open: JsValue,
}

#[derive(Clone)]
struct HeroComponents {
    workspace_chip: JsValue,
    hero_glow: JsValue,
    hero_shell: JsValue,
}

/// Configures the compiled empty-session hero family.
///
/// # Errors
///
/// Returns on missing React/ui-primitives faces or stylesheet failure.
#[wasm_bindgen(js_name = configureClientUiConversationEmptyHero)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_conversation_empty_hero(
    react: JsValue,
    ui_primitives: JsValue,
) -> Result<(), JsValue> {
    for method in ["createElement", "useId"] {
        required_function(&react, method, "React")?;
    }
    let modules = BrowserModules {
        fish_logo: required_property(&ui_primitives, "FishLogo", "ui-primitives")?,
        chevron: required_property(&ui_primitives, "IconChevronDownOutline14", "ui-primitives")?,
        folder_closed: required_property(&ui_primitives, "IconFolderClose16", "ui-primitives")?,
        folder_open: required_property(&ui_primitives, "IconFolderOpen16", "ui-primitives")?,
        react,
    };
    inject_style(
        "HeroShell",
        HERO_CSS,
        &[
            ("body", "seekdeep-conversation-hero-body"),
            ("chevron", "seekdeep-conversation-hero-chevron"),
            ("fish", "seekdeep-conversation-hero-fish"),
            ("fishHitbox", "seekdeep-conversation-hero-fishHitbox"),
            ("folder", "seekdeep-conversation-hero-folder"),
            ("glow", "seekdeep-conversation-hero-glow"),
            ("headline", "seekdeep-conversation-hero-headline"),
            ("headlineText", "seekdeep-conversation-hero-headlineText"),
            ("heroGlow", "seekdeep-conversation-hero-heroGlow"),
            ("modalAction", "seekdeep-conversation-hero-modalAction"),
            ("modalError", "seekdeep-conversation-hero-modalError"),
            ("modalInput", "seekdeep-conversation-hero-modalInput"),
            ("previewBadge", "seekdeep-conversation-hero-previewBadge"),
            ("root", "seekdeep-conversation-hero-root"),
            ("stack", "seekdeep-conversation-hero-stack"),
            ("workspace", "seekdeep-conversation-hero-workspace"),
            (
                "workspaceLabel",
                "seekdeep-conversation-hero-workspaceLabel",
            ),
            ("workspaceRow", "seekdeep-conversation-hero-workspaceRow"),
        ],
    )?;
    let workspace_modules = modules.clone();
    let workspace_chip = Closure::wrap(Box::new(move |props: JsValue| {
        render_workspace_chip(&workspace_modules, &props)
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value();
    let glow_modules = modules.clone();
    let hero_glow =
        Closure::wrap(
            Box::new(move |props: JsValue| render_hero_glow(&glow_modules, &props))
                as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
        )
        .into_js_value();
    let shell_modules = modules;
    let hero_shell =
        Closure::wrap(
            Box::new(move |props: JsValue| render_hero_shell(&shell_modules, &props))
                as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
        )
        .into_js_value();
    COMPONENTS.with(|configured| {
        *configured.borrow_mut() = Some(HeroComponents {
            workspace_chip,
            hero_glow,
            hero_shell,
        });
    });
    Ok(())
}

/// Returns the compiled workspace chip.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = workspaceChipComponent)]
pub fn workspace_chip_component() -> Result<JsValue, JsValue> {
    Ok(configured_components()?.workspace_chip)
}

/// Returns the compiled hero glow.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = heroGlowComponent)]
pub fn hero_glow_component() -> Result<JsValue, JsValue> {
    Ok(configured_components()?.hero_glow)
}

/// Returns the compiled hero shell.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = heroShellComponent)]
pub fn hero_shell_component() -> Result<JsValue, JsValue> {
    Ok(configured_components()?.hero_shell)
}

/// Returns the workspace basename, falling back to the raw path for separator-only input.
#[must_use]
#[wasm_bindgen(js_name = workspaceLabel)]
pub fn workspace_label_browser(cwd: &str) -> String {
    let title = cwd
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default();
    if title.is_empty() { cwd } else { title }.to_owned()
}

fn render_workspace_chip(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let translate = required_function(props, "t", "WorkspaceChip props")?;
    let aria_label = translate.call1(
        &JsValue::UNDEFINED,
        &JsValue::from_str("hero.chooseWorkspace"),
    )?;
    let label = Reflect::get(props, &JsValue::from_str("label"))?;
    let menu_open = Reflect::get(props, &JsValue::from_str("menuOpen"))?
        .as_bool()
        .unwrap_or(false);
    let folder = create_element(
        &modules.react,
        if label.is_undefined() {
            &modules.folder_closed
        } else {
            &modules.folder_open
        },
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-conversation-hero-folder"),
            ),
            ("size", JsValue::from_f64(16.0)),
        ])?),
        &[],
    )?;
    let visible_label = if label.is_null() || label.is_undefined() {
        translate.call1(
            &JsValue::UNDEFINED,
            &JsValue::from_str("hero.chooseWorkspace"),
        )?
    } else {
        label
    };
    let label = create_element(
        &modules.react,
        &JsValue::from_str("span"),
        Some(&object(&[(
            "className",
            JsValue::from_str("seekdeep-conversation-hero-workspaceLabel"),
        )])?),
        &[visible_label],
    )?;
    let chevron = create_element(
        &modules.react,
        &modules.chevron,
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-conversation-hero-chevron"),
            ),
            ("size", JsValue::from_f64(12.0)),
        ])?),
        &[],
    )?;
    create_element(
        &modules.react,
        &JsValue::from_str("button"),
        Some(&object(&[
            ("ref", Reflect::get(props, &JsValue::from_str("buttonRef"))?),
            ("type", JsValue::from_str("button")),
            (
                "className",
                JsValue::from_str("seekdeep-conversation-hero-workspace"),
            ),
            ("aria-label", aria_label),
            ("aria-haspopup", JsValue::from_str("menu")),
            ("aria-expanded", JsValue::from_bool(menu_open)),
            (
                "onClick",
                Reflect::get(props, &JsValue::from_str("onClick"))?,
            ),
        ])?),
        &[folder, label, chevron],
    )
}

#[allow(clippy::too_many_lines)] // Closed SVG filter tree stays auditable against the source.
fn render_hero_glow(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let raw_id = required_function(&modules.react, "useId", "React")?
        .call0(&modules.react)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("React useId did not return a string"))?;
    let filter_id = format!("empty-glow-{}", raw_id.replace(':', ""));
    let flood = create_element(
        &modules.react,
        &JsValue::from_str("feFlood"),
        Some(&object(&[
            ("floodOpacity", JsValue::from_str("0")),
            ("result", JsValue::from_str("BackgroundImageFix")),
        ])?),
        &[],
    )?;
    let blend = create_element(
        &modules.react,
        &JsValue::from_str("feBlend"),
        Some(&object(&[
            ("mode", JsValue::from_str("normal")),
            ("in", JsValue::from_str("SourceGraphic")),
            ("in2", JsValue::from_str("BackgroundImageFix")),
            ("result", JsValue::from_str("shape")),
        ])?),
        &[],
    )?;
    let blur = create_element(
        &modules.react,
        &JsValue::from_str("feGaussianBlur"),
        Some(&object(&[
            ("stdDeviation", JsValue::from_str("50")),
            ("result", JsValue::from_str("effect1_foregroundBlur")),
        ])?),
        &[],
    )?;
    let filter = create_element(
        &modules.react,
        &JsValue::from_str("filter"),
        Some(&object(&[
            ("id", JsValue::from_str(&filter_id)),
            ("x", JsValue::from_str("0")),
            ("y", JsValue::from_str("0")),
            ("width", JsValue::from_str("1051")),
            ("height", JsValue::from_str("468")),
            ("filterUnits", JsValue::from_str("userSpaceOnUse")),
            ("colorInterpolationFilters", JsValue::from_str("sRGB")),
        ])?),
        &[flood, blend, blur],
    )?;
    let defs = create_element(&modules.react, &JsValue::from_str("defs"), None, &[filter])?;
    let ellipse = create_element(
        &modules.react,
        &JsValue::from_str("ellipse"),
        Some(&object(&[
            ("cx", JsValue::from_str("525.5")),
            ("cy", JsValue::from_str("234")),
            ("rx", JsValue::from_str("425.5")),
            ("ry", JsValue::from_str("134")),
            ("fill", JsValue::from_str("#6187D8")),
            ("fillOpacity", JsValue::from_str("0.08")),
        ])?),
        &[],
    )?;
    let group = create_element(
        &modules.react,
        &JsValue::from_str("g"),
        Some(&object(&[(
            "filter",
            JsValue::from_str(&format!("url(#{filter_id})")),
        )])?),
        &[ellipse],
    )?;
    create_element(
        &modules.react,
        &JsValue::from_str("svg"),
        Some(&object(&[
            (
                "className",
                Reflect::get(props, &JsValue::from_str("className"))?,
            ),
            ("viewBox", JsValue::from_str("0 0 1051 468")),
            ("fill", JsValue::from_str("none")),
            ("aria-hidden", JsValue::from_str("true")),
        ])?),
        &[defs, group],
    )
}

fn render_hero_shell(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let translate = required_function(props, "t", "HeroShell props")?;
    let fish = create_element(
        &modules.react,
        &modules.fish_logo,
        Some(&object(&[
            ("size", JsValue::from_f64(34.0)),
            (
                "className",
                JsValue::from_str("seekdeep-conversation-hero-fish"),
            ),
        ])?),
        &[],
    )?;
    let headline = create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&class_props("seekdeep-conversation-hero-headline")?),
        &[
            create_element(
                &modules.react,
                &JsValue::from_str("span"),
                Some(&class_props("seekdeep-conversation-hero-fishHitbox")?),
                &[fish],
            )?,
            create_element(
                &modules.react,
                &JsValue::from_str("span"),
                Some(&class_props("seekdeep-conversation-hero-headlineText")?),
                &[translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("hero.headline"))?],
            )?,
            create_element(
                &modules.react,
                &JsValue::from_str("span"),
                Some(&class_props("seekdeep-conversation-hero-previewBadge")?),
                &[translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("hero.preview"))?],
            )?,
        ],
    )?;
    let body = create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&class_props("seekdeep-conversation-hero-body")?),
        &[],
    )?;
    let stack = create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&class_props("seekdeep-conversation-hero-stack")?),
        &[headline, body],
    )?;
    create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&class_props("seekdeep-conversation-hero-root")?),
        &[stack, Reflect::get(props, &JsValue::from_str("children"))?],
    )
}

fn configured_components() -> Result<HeroComponents, JsValue> {
    COMPONENTS.with(|components| {
        components.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-conversation empty hero was not configured").into()
        })
    })
}

fn class_props(class_name: &str) -> Result<Object, JsValue> {
    object(&[("className", JsValue::from_str(class_name))])
}

fn required_function(value: &JsValue, key: &str, owner: &str) -> Result<Function, JsValue> {
    required_property(value, key, owner)?.dyn_into()
}

fn required_property(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_null() || property.is_undefined() {
        Err(js_sys::Error::new(&format!("{owner} omitted {key}")).into())
    } else {
        Ok(property)
    }
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let object = Object::new();
    for (key, value) in entries {
        Reflect::set(&object, &JsValue::from_str(key), value)?;
    }
    Ok(object)
}

fn create_element(
    react: &JsValue,
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
    required_function(react, "createElement", "React")?.apply(react, &arguments)
}
