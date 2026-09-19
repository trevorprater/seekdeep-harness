//! Browser service provision, locale registration, and popup overlay assembly.

use js_sys::{Array, Object};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};

use super::{
    BrowserModules, call_method, object, popup_view, required, service::BrowserCommandUiRuntime,
    set,
};
use crate::{COMMAND_LOCALES, COMMAND_NS};

pub(crate) fn apply(modules: &BrowserModules, ctx: &JsValue) -> Result<(), JsValue> {
    let locale = required(ctx, "locale", "Client Context")?;
    own_locale(ctx, &locale)?;
    let runtime = BrowserCommandUiRuntime::new(ctx)?;
    own_service(ctx, &runtime)?;
    defer_overlay(modules, ctx)?;
    Ok(())
}

fn own_locale(ctx: &JsValue, locale: &JsValue) -> Result<(), JsValue> {
    let zh = Object::new();
    let en = Object::new();
    for (key, zh_value, en_value) in COMMAND_LOCALES {
        set(&zh, key, &JsValue::from_str(zh_value))?;
        set(&en, key, &JsValue::from_str(en_value))?;
    }
    let dictionaries = object(&[("zh", zh.into()), ("en", en.into())])?;
    let locale = locale.clone();
    let setup = Closure::wrap(Box::new(move || {
        call_method(
            &locale,
            "register",
            &[JsValue::from_str(COMMAND_NS), dictionaries.clone().into()],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        ctx,
        "effect",
        &[
            setup.into_js_value(),
            JsValue::from_str("ui-commands: dictionaries"),
        ],
    )?;
    Ok(())
}

pub(super) fn own_service(
    ctx: &JsValue,
    runtime: &std::rc::Rc<BrowserCommandUiRuntime>,
) -> Result<(), JsValue> {
    let reflect = required(ctx, "reflect", "Client Context")?;
    let face = BrowserCommandUiRuntime::face(runtime);
    let dispose_runtime = runtime.clone();
    let setup = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let retract = call_method(
            &reflect,
            "provide",
            &[JsValue::from_str("commandUi"), face.clone()],
        )?
        .dyn_into::<js_sys::Function>()?;
        let runtime = dispose_runtime.clone();
        Ok(Closure::wrap(Box::new(move || {
            runtime.dispose_all();
            let _ = retract.call0(&JsValue::UNDEFINED);
        }) as Box<dyn FnMut()>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        ctx,
        "effect",
        &[
            setup.into_js_value(),
            JsValue::from_str("ui-commands: service"),
        ],
    )?;
    Ok(())
}

fn defer_overlay(modules: &BrowserModules, ctx: &JsValue) -> Result<(), JsValue> {
    let component = popup_view::component(modules);
    let callback = Closure::wrap(Box::new(move |scope: JsValue| -> Result<(), JsValue> {
        let slots = required(&scope, "slots", "Client Context")?;
        let sessions = required(&scope, "sessions", "Client Context")?;
        let command = required(&scope, "commandUi", "Client Context")?;
        let inject_command = command;
        let inject_sessions = sessions;
        let inject = Closure::wrap(
            Box::new(move |session_id: String| -> Result<JsValue, JsValue> {
                let actx =
                    call_method(&inject_sessions, "scope", &[JsValue::from_str(&session_id)])?;
                if actx.is_null() || actx.is_undefined() {
                    return Err(js_sys::Error::new(&format!(
                        "ui-commands: session \"{session_id}\" resolved no scope"
                    ))
                    .into());
                }
                object(&[(
                    "popup",
                    call_method(&inject_command, "popupFor", std::slice::from_ref(&actx))?,
                )])
                .map(Into::into)
            }) as Box<dyn FnMut(String) -> Result<JsValue, JsValue>>,
        );
        let registration_slots = slots.clone();
        let registration_component = component.clone();
        let installer = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
            let options = object(&[
                ("name", JsValue::from_str("conversation.input.overlay")),
                ("id", JsValue::from_str("command-popup")),
                ("order", JsValue::from_f64(1.0)),
                ("locale", JsValue::from_str(COMMAND_NS)),
                ("inject", inject.as_ref().clone()),
            ])?;
            call_method(
                &registration_slots,
                "register",
                &[options.into(), registration_component.clone()],
            )
        }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
        call_method(
            &slots,
            "inject",
            &[
                JsValue::from_str("conversation.input.overlay"),
                installer.into_js_value(),
            ],
        )?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let dependencies = Array::new();
    for dependency in ["slots", "commandUi", "sessions"] {
        dependencies.push(&JsValue::from_str(dependency));
    }
    call_method(
        ctx,
        "inject",
        &[dependencies.into(), callback.into_js_value()],
    )?;
    Ok(())
}
