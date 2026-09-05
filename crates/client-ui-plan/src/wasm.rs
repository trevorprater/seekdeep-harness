//! Browser Plan chip and command-backed Client plugin.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Promise, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, future_to_promise, spawn_local};

use crate::{PLAN_CHIP_STYLES, PLAN_LOCALES, PLAN_NS, PlanProjection, effective_plan_target};

const INJECT: &[&str] = &["slots", "remote", "remote.commands", "locale"];

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    close_icon: JsValue,
}

/// Configures React, the close icon, and the compiled stylesheet.
///
/// # Errors
///
/// Returns missing primitive or DOM stylesheet-injection failures.
#[wasm_bindgen(js_name = configureClientUiPlan)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_plan(react: JsValue, primitives: JsValue) -> Result<(), JsValue> {
    MODULES.with(|modules| {
        *modules.borrow_mut() = Some(BrowserModules {
            react,
            close_icon: required(&primitives, "IconCloseFill14", "UI primitives")?,
        });
        Ok::<_, JsValue>(())
    })?;
    inject_styles()
}

/// Applies the Plan browser plugin.
///
/// # Errors
///
/// Returns missing Slot, Remote, locale, command, or component failures.
#[wasm_bindgen(js_name = applyClientUiPlan)]
#[allow(clippy::needless_pass_by_value)]
pub fn apply_client_ui_plan(ctx: JsValue) -> Result<(), JsValue> {
    let modules = configured_modules()?;
    let slots = required(&ctx, "slots", "Client Context")?;
    let remote = required(&ctx, "remote", "Client Context")?;
    required(&ctx, "remote.commands", "Client Context")?;
    let commands = required(&remote, "commands", "Remote")?;
    let locale = required(&ctx, "locale", "Client Context")?;
    own_locale_dictionaries(&ctx, &locale)?;

    let component = plan_chip_component(&modules);
    let registration_slots = slots.clone();
    let installer = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let inject_commands = commands.clone();
        let inject = Closure::wrap(
            Box::new(move |session_id: String| -> Result<JsValue, JsValue> {
                let execute_commands = inject_commands.clone();
                let execute_session = session_id;
                let exit = Closure::wrap(Box::new(move || -> Promise {
                    let commands = execute_commands.clone();
                    let session_id = execute_session.clone();
                    future_to_promise(async move {
                        let returned = call_method(
                            &commands,
                            "execute",
                            &[
                                JsValue::from_str(&session_id),
                                JsValue::from_str("/plan off"),
                            ],
                        )?;
                        let result = JsFuture::from(Promise::resolve(&returned)).await?;
                        fold_command_result(&result)
                    })
                }) as Box<dyn FnMut() -> Promise>);
                object(&[("exitPlanMode", exit.into_js_value())]).map(Into::into)
            }) as Box<dyn FnMut(String) -> Result<JsValue, JsValue>>,
        );
        let options = object(&[
            ("name", JsValue::from_str("conversation.input.plan")),
            ("locale", JsValue::from_str(PLAN_NS)),
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
            JsValue::from_str("conversation.input.plan"),
            installer.into_js_value(),
        ],
    )?;
    Ok(())
}

/// Returns the exact browser dependency order.
#[wasm_bindgen(js_name = planInject)]
pub fn plan_inject() -> Array {
    let values = Array::new();
    for dependency in INJECT {
        values.push(&JsValue::from_str(dependency));
    }
    values
}

/// Returns the compiled `PlanChip` component.
///
/// # Errors
///
/// Returns before browser modules are configured.
#[wasm_bindgen(js_name = planChipComponent)]
pub fn exported_plan_chip_component() -> Result<JsValue, JsValue> {
    Ok(plan_chip_component(&configured_modules()?))
}

fn plan_chip_component(modules: &BrowserModules) -> JsValue {
    let modules = modules.clone();
    Closure::wrap(
        Box::new(move |props: JsValue| render_plan_chip(&modules, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value()
}

#[allow(clippy::too_many_lines)]
fn render_plan_chip(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let use_projection = required_function(props, "useProjection", "PlanChip")?;
    let projection = use_projection.call1(&JsValue::UNDEFINED, &JsValue::from_str("plan"))?;
    let (leaving, set_leaving) = use_state(&modules.react, &JsValue::FALSE)?;
    let (error, set_error) = use_state(&modules.react, &JsValue::NULL)?;
    let alive_ref = use_ref(&modules.react, &JsValue::TRUE)?;
    install_alive_effect(&modules.react, &alive_ref)?;

    if projection.is_undefined() {
        return Ok(JsValue::NULL);
    }
    let plan = PlanProjection {
        active: required_bool(&projection, "active", "Plan projection")?,
        pending: required_bool(&projection, "pending", "Plan projection")?,
    };
    if !effective_plan_target(plan) {
        return Ok(JsValue::NULL);
    }

    let locked = required(props, "locked", "PlanChip")?
        .as_bool()
        .unwrap_or(false);
    let exit_plan_mode = required_function(props, "exitPlanMode", "PlanChip")?;
    let translate = required_function(props, "t", "PlanChip")?;
    let leaving = leaving.as_bool().unwrap_or(false);
    let start_leaving = set_leaving;
    let settle_error = set_error;
    let alive = alive_ref;
    let exit = exit_plan_mode;
    let off = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        start_leaving.call1(&JsValue::UNDEFINED, &JsValue::TRUE)?;
        settle_error.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
        let returned = exit.call0(&JsValue::UNDEFINED)?;
        let alive = alive.clone();
        let set_leaving = start_leaving.clone();
        let set_error = settle_error.clone();
        spawn_local(async move {
            let settled = JsFuture::from(Promise::resolve(&returned)).await;
            if !ref_current_bool(&alive) {
                return;
            }
            let _ = set_leaving.call1(&JsValue::UNDEFINED, &JsValue::FALSE);
            match settled {
                Ok(failure) => {
                    let _ = set_error.call1(&JsValue::UNDEFINED, &failure);
                }
                Err(reason) => {
                    let _ = set_error.call1(
                        &JsValue::UNDEFINED,
                        &JsValue::from_str(&rejection_text(&reason)),
                    );
                }
            }
        });
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);

    let icon = component(
        &modules.react,
        &modules.close_icon,
        Some(&object(&[("size", JsValue::from_f64(12.0))])?),
        &[],
    )?;
    let close = tag(
        &modules.react,
        "span",
        Some(&object(&[
            ("className", JsValue::from_str("seekdeep-plan-close")),
            ("aria-hidden", JsValue::TRUE),
        ])?),
        &[icon],
    )?;
    let button = tag(
        &modules.react,
        "button",
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            ("className", JsValue::from_str("seekdeep-plan-chip")),
            ("aria-label", translated(&translate, "chip.on.aria")?),
            ("title", translated(&translate, "chip.on.title")?),
            ("disabled", JsValue::from_bool(locked || leaving)),
            ("onClick", off.into_js_value()),
        ])?),
        &[JsValue::from_str("Plan"), close],
    )?;
    let mut children = vec![button];
    if !error.is_null() {
        let title = error
            .as_string()
            .ok_or_else(|| js_sys::Error::new("Plan exit failure must be a string"))?;
        children.push(tag(
            &modules.react,
            "span",
            Some(&object(&[
                ("className", JsValue::from_str("seekdeep-plan-error")),
                ("role", JsValue::from_str("status")),
                ("title", JsValue::from_str(&title)),
            ])?),
            &[JsValue::from_str("failed to exit plan mode")],
        )?);
    }
    tag(
        &modules.react,
        "span",
        Some(&class("seekdeep-plan-wrap")?),
        &children,
    )
}

fn install_alive_effect(react: &JsValue, alive_ref: &JsValue) -> Result<(), JsValue> {
    let alive = alive_ref.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        Reflect::set(&alive, &JsValue::from_str("current"), &JsValue::TRUE)?;
        let cleanup_alive = alive.clone();
        Ok(Closure::wrap(Box::new(move || {
            let _ = Reflect::set(
                &cleanup_alive,
                &JsValue::from_str("current"),
                &JsValue::FALSE,
            );
        }) as Box<dyn FnMut()>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    required_function(react, "useEffect", "React")?
        .call2(react, &effect.into_js_value(), &Array::new())
        .map(|_| ())
}

fn fold_command_result(result: &JsValue) -> Result<JsValue, JsValue> {
    if !required_bool(result, "ok", "Command result")? {
        let error = required(result, "error", "Command result")?;
        let message = required_string(&error, "message", "Command error")?;
        let code = required_string(&error, "code", "Command error")?;
        return Ok(JsValue::from_str(&format!("{message} ({code})")));
    }
    if Reflect::get(result, &JsValue::from_str("value"))?.is_undefined() {
        return Ok(JsValue::from_str("unknown command: /plan off"));
    }
    Ok(JsValue::NULL)
}

fn rejection_text(reason: &JsValue) -> String {
    if reason.is_instance_of::<js_sys::Error>() {
        return Reflect::get(reason, &JsValue::from_str("message"))
            .ok()
            .and_then(|message| message.as_string())
            .unwrap_or_default();
    }
    Reflect::get(&js_sys::global(), &JsValue::from_str("String"))
        .ok()
        .and_then(|value| value.dyn_into::<Function>().ok())
        .and_then(|string| string.call1(&JsValue::UNDEFINED, reason).ok())
        .and_then(|value| value.as_string())
        .unwrap_or_else(|| reason.as_string().unwrap_or_default())
}

fn ref_current_bool(reference: &JsValue) -> bool {
    Reflect::get(reference, &JsValue::from_str("current"))
        .ok()
        .and_then(|value| value.as_bool())
        == Some(true)
}

fn own_locale_dictionaries(ctx: &JsValue, locale: &JsValue) -> Result<(), JsValue> {
    let zh = Object::new();
    let en = Object::new();
    for (key, zh_value, en_value) in PLAN_LOCALES {
        set(&zh, key, &JsValue::from_str(zh_value))?;
        set(&en, key, &JsValue::from_str(en_value))?;
    }
    let dictionaries = object(&[("zh", zh.into()), ("en", en.into())])?;
    let locale = locale.clone();
    let installer = Closure::wrap(Box::new(move || {
        call_method(
            &locale,
            "register",
            &[JsValue::from_str(PLAN_NS), dictionaries.clone().into()],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        ctx,
        "effect",
        &[
            installer.into_js_value(),
            JsValue::from_str("ui-plan: dictionaries"),
        ],
    )?;
    Ok(())
}

fn inject_styles() -> Result<(), JsValue> {
    const PACKAGE: &str = "@seekdeep-ai/seekdeep-client-ui-plan";
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
        &JsValue::from_str(PLAN_CHIP_STYLES),
    )?;
    let head = required(&document, "head", "document")?;
    call_method(&head, "appendChild", &[style])?;
    Ok(())
}

fn configured_modules() -> Result<BrowserModules, JsValue> {
    MODULES.with(|modules| {
        modules
            .borrow()
            .clone()
            .ok_or_else(|| js_sys::Error::new("client-ui-plan is not configured").into())
    })
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

fn use_ref(react: &JsValue, initial: &JsValue) -> Result<JsValue, JsValue> {
    required_function(react, "useRef", "React")?.call1(react, initial)
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
