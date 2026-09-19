//! Browser WASM facade, Cordis assembly, and React sidebar shell.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{
    COLLAPSE_SETTLE_MS, SCROLLBAR_LINGER_MS, SIDEBAR_EN, SIDEBAR_LOCALE_NAMESPACE, SIDEBAR_STYLES,
    SIDEBAR_ZH,
};

const INJECT: &[&str] = &["slots", "layout", "sessions", "workspaces", "locale"];

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    primitives: JsValue,
}

/// Configures shell-owned JavaScript modules and installs the compiled stylesheet.
///
/// # Errors
///
/// Returns DOM style-injection failures.
#[wasm_bindgen(js_name = configureClientUiSidebar)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_sidebar(react: JsValue, primitives: JsValue) -> Result<(), JsValue> {
    MODULES.with(|modules| {
        *modules.borrow_mut() = Some(BrowserModules { react, primitives });
    });
    inject_styles()
}

/// Browser Client plugin apply function.
///
/// # Errors
///
/// Returns missing-service, locale, Slot, React, or DOM failures.
#[wasm_bindgen(js_name = applyClientUiSidebar)]
#[allow(clippy::needless_pass_by_value)]
pub fn apply_client_ui_sidebar(ctx: JsValue) -> Result<(), JsValue> {
    let modules = configured_modules()?;
    let slots = required_service(&ctx, "slots")?;
    let layout = required_service(&ctx, "layout")?;
    required_service(&ctx, "sessions")?;
    let workspaces = required_service(&ctx, "workspaces")?;
    let locale = required_service(&ctx, "locale")?;
    own_locale_dictionaries(&ctx, &locale)?;

    let inject_layout = layout;
    let inject_workspaces = workspaces;
    let inject = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let start_workspaces = inject_workspaces.clone();
        let start_session = Closure::wrap(Box::new(move |workspace_id: JsValue| {
            call_method(&start_workspaces, "startSession", &[workspace_id])
        })
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
        let toggle_layout = inject_layout.clone();
        let toggle =
            Closure::wrap(
                Box::new(move || call_method(&toggle_layout, "toggleSidebar", &[]))
                    as Box<dyn FnMut() -> Result<JsValue, JsValue>>,
            );
        object(&[
            ("startSession", start_session.into_js_value()),
            ("toggleSidebar", toggle.into_js_value()),
        ])
        .map(Into::into)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let children = Object::new();
    for (name, kind) in [
        ("sidebar.workspaces", "single"),
        ("sidebar.settings", "single"),
        ("sidebar.footer.action", "list"),
    ] {
        set(
            &children,
            name,
            &object(&[
                ("kind", JsValue::from_str(kind)),
                ("scope", JsValue::from_str("root")),
            ])?
            .into(),
        )?;
    }
    let options = object(&[
        ("name", JsValue::from_str("sidebar")),
        ("locale", JsValue::from_str(SIDEBAR_LOCALE_NAMESPACE)),
        ("children", children.into()),
        ("inject", inject.into_js_value()),
    ])?;
    let component = sidebar_root_component(&modules);
    let installer_slots = slots;
    let installer = Closure::wrap(Box::new(move || {
        call_method(
            &installer_slots,
            "register",
            &[options.clone().into(), component.clone()],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        &ctx,
        "effect",
        &[
            installer.into_js_value(),
            JsValue::from_str("ui-sidebar: slot registration"),
        ],
    )?;
    Ok(())
}

/// Exact Client plugin inject list.
#[wasm_bindgen(js_name = sidebarInject)]
pub fn sidebar_inject() -> Array {
    let values = Array::new();
    for dependency in INJECT {
        values.push(&JsValue::from_str(dependency));
    }
    values
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
        let args = Array::new();
        args.push(kind);
        args.push(props.map_or(&JsValue::NULL, AsRef::as_ref));
        for child in children {
            args.push(child);
        }
        function(&self.react, "createElement")?.apply(&self.react, &args)
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
}

fn sidebar_root_component(modules: &BrowserModules) -> JsValue {
    let ui = ReactUi {
        react: modules.react.clone(),
        primitives: modules.primitives.clone(),
    };
    let component = Closure::wrap(Box::new(move |props: JsValue| render_sidebar(&ui, &props))
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    component.into_js_value()
}

#[allow(clippy::too_many_lines)]
fn render_sidebar(ui: &ReactUi, props: &JsValue) -> Result<JsValue, JsValue> {
    let collapsed = required_property(props, "collapsed", "Sidebar props")?
        .as_bool()
        .unwrap_or(false);
    let width = required_property(props, "width", "Sidebar props")?
        .as_f64()
        .unwrap_or_default();
    let (settled, set_settled) = use_state(&ui.react, &JsValue::from_bool(collapsed))?;
    let effect_settled = set_settled.clone();
    let collapse_effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if !collapsed {
            effect_settled.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
            return Ok(JsValue::UNDEFINED);
        }
        let window = required_property(&js_sys::global(), "window", "global")?;
        let settle = effect_settled.clone();
        let callback = Closure::wrap(Box::new(move || {
            let _ = settle.call1(&JsValue::UNDEFINED, &JsValue::TRUE);
        }) as Box<dyn FnMut()>);
        let timer = call_method(
            &window,
            "setTimeout",
            &[callback.into_js_value(), js_number(COLLAPSE_SETTLE_MS)],
        )?;
        let cleanup_window = window;
        let cleanup = Closure::wrap(Box::new(move || {
            let _ = call_method(
                &cleanup_window,
                "clearTimeout",
                std::slice::from_ref(&timer),
            );
        }) as Box<dyn FnMut()>);
        Ok(cleanup.into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    use_effect(
        &ui.react,
        &collapse_effect.into_js_value(),
        &[JsValue::from_bool(collapsed)],
    )?;
    let settled = settled.as_bool().unwrap_or(collapsed);
    let wide = !collapsed || !settled;

    let last_width = use_ref(&ui.react, &JsValue::from_f64(width))?;
    if !collapsed {
        set_ref(&last_width, &JsValue::from_f64(width))?;
    }
    let ever_wide = use_ref(&ui.react, &JsValue::from_bool(!collapsed))?;
    if !collapsed {
        set_ref(&ever_wide, &JsValue::TRUE)?;
    }

    let column = use_ref(&ui.react, &JsValue::NULL)?;
    let (pointer_inside, set_pointer_inside) = use_state(&ui.react, &JsValue::FALSE)?;
    let pointer_inside = pointer_inside.as_bool().unwrap_or(false);
    let linger = use_ref(&ui.react, &JsValue::UNDEFINED)?;
    let window = required_property(&js_sys::global(), "window", "global")?;
    let arm = linger_arm(&window, &linger, &set_pointer_inside);
    let cancel = linger_cancel(&window, &linger);

    let move_document = required_property(&js_sys::global(), "document", "global")?;
    let move_column = column.clone();
    let move_arm = arm.clone();
    let move_cancel = cancel.clone();
    let pointer_effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if !pointer_inside {
            return Ok(JsValue::UNDEFINED);
        }
        let pointer_column = move_column.clone();
        let pointer_arm = move_arm.clone();
        let pointer_cancel = move_cancel.clone();
        let listener = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
            let current = Reflect::get(&pointer_column, &JsValue::from_str("current"))?;
            if current.is_null() || current.is_undefined() {
                return Ok(());
            }
            let rect = call_method(&current, "getBoundingClientRect", &[])?;
            let x = required_number(&event, "clientX")?;
            let y = required_number(&event, "clientY")?;
            let inside = x >= required_number(&rect, "left")?
                && x < required_number(&rect, "right")?
                && y >= required_number(&rect, "top")?
                && y < required_number(&rect, "bottom")?;
            if inside {
                pointer_cancel.call0(&JsValue::UNDEFINED)?;
            } else {
                pointer_arm.call0(&JsValue::UNDEFINED)?;
            }
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
        let listener: JsValue = listener.into_js_value();
        call_method(
            &move_document,
            "addEventListener",
            &[JsValue::from_str("pointermove"), listener.clone()],
        )?;
        let cleanup_document = move_document.clone();
        let cleanup_cancel = move_cancel.clone();
        let cleanup = Closure::wrap(Box::new(move || {
            let _ = call_method(
                &cleanup_document,
                "removeEventListener",
                &[JsValue::from_str("pointermove"), listener.clone()],
            );
            let _ = cleanup_cancel.call0(&JsValue::UNDEFINED);
        }) as Box<dyn FnMut()>);
        Ok(cleanup.into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    use_effect(
        &ui.react,
        &pointer_effect.into_js_value(),
        &[JsValue::from_bool(pointer_inside)],
    )?;

    let start_session = function(props, "startSession")?;
    let brand_start = start_session.clone();
    let brand_click =
        Closure::wrap(
            Box::new(move || brand_start.call1(&JsValue::UNDEFINED, &JsValue::UNDEFINED))
                as Box<dyn FnMut() -> Result<JsValue, JsValue>>,
        );
    let new_start = start_session;
    let new_click =
        Closure::wrap(
            Box::new(move || new_start.call1(&JsValue::UNDEFINED, &JsValue::UNDEFINED))
                as Box<dyn FnMut() -> Result<JsValue, JsValue>>,
        );
    let toggle = function(props, "toggleSidebar")?;
    let toggle_click = toggle.clone();
    let toggle_callback = Closure::wrap(Box::new(move || toggle_click.call0(&JsValue::UNDEFINED))
        as Box<dyn FnMut() -> Result<JsValue, JsValue>>);

    let mut logo_children = Vec::new();
    if wide {
        let wordmark = ui.primitive("BrandWordmark", None, &[])?;
        logo_children.push(ui.tag(
            "button",
            Some(&object(&[
                ("type", JsValue::from_str("button")),
                (
                    "className",
                    JsValue::from_str("seekdeep-sidebar-brand seekdeep-sidebar-wide"),
                ),
                ("aria-label", translated(props, "session.new.label")?),
                ("onClick", brand_click.into_js_value()),
            ])?),
            &[wordmark],
        )?);
    }
    let mut toggle_children = Vec::new();
    if !wide {
        toggle_children.push(ui.primitive(
            "FishLogo",
            Some(&object(&[
                ("className", JsValue::from_str("seekdeep-sidebar-rail-fish")),
                ("size", JsValue::from_f64(24.0)),
            ])?),
            &[],
        )?);
    }
    toggle_children.push(ui.primitive(
        "IconPanelLeftOutline16",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-sidebar-panel-icon"),
            ),
            ("size", JsValue::from_f64(if wide { 16.0 } else { 18.0 })),
        ])?),
        &[],
    )?);
    let toggle_label = translated(
        props,
        if collapsed {
            "toggle.open"
        } else {
            "toggle.collapse"
        },
    )?;
    let toggle_button = ui.tag(
        "button",
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            (
                "className",
                JsValue::from_str("seekdeep-sidebar-icon-button seekdeep-sidebar-toggle"),
            ),
            ("aria-label", toggle_label.clone()),
            ("onClick", toggle_callback.into_js_value()),
        ])?),
        &toggle_children,
    )?;
    logo_children.push(ui.primitive(
        "Tooltip",
        Some(&object(&[
            ("label", toggle_label),
            ("delayMs", JsValue::from_f64(500.0)),
        ])?),
        &[toggle_button],
    )?);
    let logo_row = ui.tag(
        "div",
        Some(&class_props("seekdeep-sidebar-logo-row")?),
        &logo_children,
    )?;

    let icon = ui.primitive(
        "IconNewChatOutline16",
        Some(&object(&[(
            "size",
            JsValue::from_f64(if wide { 14.0 } else { 18.0 }),
        )])?),
        &[],
    )?;
    let mut new_children = vec![icon];
    if wide {
        new_children.push(ui.tag(
            "span",
            Some(&class_props(
                "seekdeep-sidebar-new-session-label seekdeep-sidebar-wide",
            )?),
            &[translated(props, "session.new")?],
        )?);
    }
    let new_label = translated(props, "session.new.label")?;
    let new_button = ui.tag(
        "button",
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            (
                "className",
                JsValue::from_str("seekdeep-sidebar-new-session"),
            ),
            ("aria-label", new_label.clone()),
            ("onClick", new_click.into_js_value()),
        ])?),
        &new_children,
    )?;
    let new_session = ui.primitive(
        "Tooltip",
        Some(&object(&[
            ("label", new_label),
            ("delayMs", JsValue::from_f64(500.0)),
            ("disabled", JsValue::from_bool(wide)),
        ])?),
        &[new_button],
    )?;

    let expand_toggle = toggle;
    let expand = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if collapsed {
            expand_toggle.call0(&JsValue::UNDEFINED)?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let workspaces = call_prop(
        props,
        "renderSlot",
        &[
            JsValue::from_str("sidebar.workspaces"),
            object(&[
                ("wide", JsValue::from_bool(wide)),
                ("expandSidebar", expand.into_js_value()),
            ])?
            .into(),
        ],
    )?;
    let region = ui.tag(
        "div",
        Some(&class_props("seekdeep-sidebar-region-area")?),
        &[workspaces],
    )?;
    let footer = call_prop(
        props,
        "renderSlot",
        &[
            JsValue::from_str("sidebar.footer.action"),
            object(&[("wide", JsValue::from_bool(wide))])?.into(),
        ],
    )?;
    let footer = ui.tag(
        "div",
        Some(&class_props("seekdeep-sidebar-footer-actions")?),
        &[footer],
    )?;
    let settings = call_prop(
        props,
        "renderSlot",
        &[
            JsValue::from_str("sidebar.settings"),
            object(&[("wide", JsValue::from_bool(wide))])?.into(),
        ],
    )?;
    let settings = ui.tag(
        "div",
        Some(&class_props("seekdeep-sidebar-settings-area")?),
        &[settings],
    )?;
    let foot = ui.tag(
        "div",
        Some(&class_props("seekdeep-sidebar-foot-area")?),
        &[footer, settings],
    )?;

    let mut classes = vec!["seekdeep-sidebar-root"];
    if !wide {
        classes.push("seekdeep-sidebar-collapsed");
        if Reflect::get(&ever_wide, &JsValue::from_str("current"))?
            .as_bool()
            .unwrap_or(false)
        {
            classes.push("seekdeep-sidebar-rail-in");
        }
    }
    if collapsed && wide {
        classes.push("seekdeep-sidebar-fading");
    }
    if !pointer_inside {
        classes.push("seekdeep-sidebar-quiet-bars");
    }
    let style = if wide {
        object(&[(
            "width",
            if collapsed {
                Reflect::get(&last_width, &JsValue::from_str("current"))?
            } else {
                JsValue::from_f64(width)
            },
        )])?
        .into()
    } else {
        JsValue::UNDEFINED
    };
    let enter_cancel = cancel;
    let enter_setter = set_pointer_inside;
    let pointer_enter = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        enter_cancel.call0(&JsValue::UNDEFINED)?;
        enter_setter.call1(&JsValue::UNDEFINED, &JsValue::TRUE)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    ui.tag(
        "div",
        Some(&object(&[
            ("ref", column),
            ("className", JsValue::from_str(&classes.join(" "))),
            ("style", style),
            ("onPointerEnter", pointer_enter.into_js_value()),
            ("onPointerLeave", arm.into()),
        ])?),
        &[logo_row, new_session, region, foot],
    )
}

fn linger_arm(window: &JsValue, linger: &JsValue, setter: &Function) -> Function {
    let window = window.clone();
    let linger = linger.clone();
    let setter = setter.clone();
    Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let current = Reflect::get(&linger, &JsValue::from_str("current"))?;
        if !current.is_undefined() {
            return Ok(());
        }
        let callback_linger = linger.clone();
        let callback_setter = setter.clone();
        let callback = Closure::wrap(Box::new(move || {
            let _ = Reflect::set(
                &callback_linger,
                &JsValue::from_str("current"),
                &JsValue::UNDEFINED,
            );
            let _ = callback_setter.call1(&JsValue::UNDEFINED, &JsValue::FALSE);
        }) as Box<dyn FnMut()>);
        let timer = call_method(
            &window,
            "setTimeout",
            &[callback.into_js_value(), js_number(SCROLLBAR_LINGER_MS)],
        )?;
        Reflect::set(&linger, &JsValue::from_str("current"), &timer)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value()
    .unchecked_into()
}

fn linger_cancel(window: &JsValue, linger: &JsValue) -> Function {
    let window = window.clone();
    let linger = linger.clone();
    Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let timer = Reflect::get(&linger, &JsValue::from_str("current"))?;
        call_method(&window, "clearTimeout", &[timer])?;
        Reflect::set(&linger, &JsValue::from_str("current"), &JsValue::UNDEFINED)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value()
    .unchecked_into()
}

fn own_locale_dictionaries(ctx: &JsValue, locale: &JsValue) -> Result<(), JsValue> {
    let dictionaries = Object::new();
    set(&dictionaries, "zh", &dictionary(SIDEBAR_ZH)?)?;
    set(&dictionaries, "en", &dictionary(SIDEBAR_EN)?)?;
    let locale = locale.clone();
    let installer = Closure::wrap(Box::new(move || {
        call_method(
            &locale,
            "register",
            &[
                JsValue::from_str(SIDEBAR_LOCALE_NAMESPACE),
                dictionaries.clone().into(),
            ],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        ctx,
        "effect",
        &[
            installer.into_js_value(),
            JsValue::from_str("ui-sidebar: dictionaries"),
        ],
    )?;
    Ok(())
}

fn dictionary(entries: &[(&str, &str)]) -> Result<JsValue, JsValue> {
    let dictionary = Object::new();
    for (key, value) in entries {
        set(&dictionary, key, &JsValue::from_str(value))?;
    }
    Ok(dictionary.into())
}

fn inject_styles() -> Result<(), JsValue> {
    let document = required_property(&js_sys::global(), "document", "global")?;
    let style = call_method(&document, "createElement", &[JsValue::from_str("style")])?;
    call_method(
        &style,
        "setAttribute",
        &[
            JsValue::from_str("data-plugin"),
            JsValue::from_str("@seekdeep-ai/seekdeep-client-ui-sidebar"),
        ],
    )?;
    Reflect::set(
        &style,
        &JsValue::from_str("textContent"),
        &JsValue::from_str(SIDEBAR_STYLES),
    )?;
    let head = required_property(&document, "head", "document")?;
    call_method(&head, "appendChild", &[style])?;
    Ok(())
}

fn configured_modules() -> Result<BrowserModules, JsValue> {
    MODULES.with(|modules| {
        modules.borrow().clone().ok_or_else(|| {
            js_error("client-ui-sidebar module factory did not configure shell modules")
        })
    })
}

fn use_state(react: &JsValue, initial: &JsValue) -> Result<(JsValue, Function), JsValue> {
    let state = Array::from(&function(react, "useState")?.call1(react, initial)?);
    Ok((state.get(0), state.get(1).dyn_into::<Function>()?))
}

fn use_ref(react: &JsValue, initial: &JsValue) -> Result<JsValue, JsValue> {
    function(react, "useRef")?.call1(react, initial)
}

fn set_ref(reference: &JsValue, value: &JsValue) -> Result<(), JsValue> {
    Reflect::set(reference, &JsValue::from_str("current"), value).map(|_| ())
}

fn use_effect(react: &JsValue, effect: &JsValue, dependencies: &[JsValue]) -> Result<(), JsValue> {
    let deps = Array::new();
    for dependency in dependencies {
        deps.push(dependency);
    }
    function(react, "useEffect")?.call2(react, effect, &deps)?;
    Ok(())
}

fn translated(props: &JsValue, key: &str) -> Result<JsValue, JsValue> {
    function(props, "t")?.call1(props, &JsValue::from_str(key))
}

fn call_prop(value: &JsValue, name: &str, args: &[JsValue]) -> Result<JsValue, JsValue> {
    let function = function(value, name)?;
    let arguments = Array::new();
    for argument in args {
        arguments.push(argument);
    }
    function.apply(value, &arguments)
}

fn class_props(class_name: &str) -> Result<Object, JsValue> {
    object(&[("className", JsValue::from_str(class_name))])
}

fn function(value: &JsValue, name: &str) -> Result<Function, JsValue> {
    required_property(value, name, "JavaScript object")?.dyn_into::<Function>()
}

fn required_number(value: &JsValue, property: &str) -> Result<f64, JsValue> {
    Reflect::get(value, &JsValue::from_str(property))?
        .as_f64()
        .ok_or_else(|| js_error(&format!("ui-sidebar: missing number {property:?}")))
}

fn required_service(ctx: &JsValue, name: &str) -> Result<JsValue, JsValue> {
    let service = call_method(ctx, "get", &[JsValue::from_str(name)])?;
    if service.is_undefined() {
        Err(js_error(&format!(
            "client-ui-sidebar requires Client Service {name:?}"
        )))
    } else {
        Ok(service)
    }
}

fn required_property(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_undefined() || property.is_null() {
        Err(js_error(&format!(
            "ui-sidebar: {owner} omitted required property {key:?}"
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

fn call_method(value: &JsValue, name: &str, args: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = function(value, name)?;
    let arguments = Array::new();
    for argument in args {
        arguments.push(argument);
    }
    method.apply(value, &arguments)
}

fn js_error(message: &str) -> JsValue {
    js_sys::Error::new(message).into()
}

fn js_number(value: u64) -> JsValue {
    JsValue::from_f64(
        value
            .to_string()
            .parse()
            .expect("u64 decimal text is a finite JavaScript number"),
    )
}
