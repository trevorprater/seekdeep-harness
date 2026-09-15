//! Browser service provision, locale registration, and overlay Slot assembly.

use js_sys::{Array, Object};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};

use super::{
    BrowserModules, call_method, menu_view, object, required, service::BrowserInputTriggerService,
    set,
};
use crate::{MENU_LOCALES, MENU_NS};

pub(crate) fn apply(modules: &BrowserModules, ctx: &JsValue) -> Result<(), JsValue> {
    let sessions = required(ctx, "sessions", "Client Context")?;
    let locale = required(ctx, "locale", "Client Context")?;
    let service = BrowserInputTriggerService::new(sessions);
    own_service(ctx, &service)?;
    own_locale(ctx, &locale)?;
    defer_overlay(modules, ctx, &service)?;
    Ok(())
}

fn own_service(
    ctx: &JsValue,
    service: &std::rc::Rc<BrowserInputTriggerService>,
) -> Result<(), JsValue> {
    let reflect = required(ctx, "reflect", "Client Context")?;
    let face = BrowserInputTriggerService::face(service);
    let dispose_service = service.clone();
    let setup = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let retract = call_method(
            &reflect,
            "provide",
            &[JsValue::from_str("inputTriggers"), face.clone()],
        )?
        .dyn_into::<js_sys::Function>()?;
        let service = dispose_service.clone();
        Ok(Closure::wrap(Box::new(move || {
            service.dispose_all();
            let _ = retract.call0(&JsValue::UNDEFINED);
        }) as Box<dyn FnMut()>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        ctx,
        "effect",
        &[
            setup.into_js_value(),
            JsValue::from_str("ui-input-trigger: service"),
        ],
    )?;
    Ok(())
}

fn own_locale(ctx: &JsValue, locale: &JsValue) -> Result<(), JsValue> {
    let zh = Object::new();
    let en = Object::new();
    for (key, zh_value, en_value) in MENU_LOCALES {
        set(&zh, key, &JsValue::from_str(zh_value))?;
        set(&en, key, &JsValue::from_str(en_value))?;
    }
    let dictionaries = object(&[("zh", zh.into()), ("en", en.into())])?;
    let locale = locale.clone();
    let setup = Closure::wrap(Box::new(move || {
        call_method(
            &locale,
            "register",
            &[JsValue::from_str(MENU_NS), dictionaries.clone().into()],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        ctx,
        "effect",
        &[
            setup.into_js_value(),
            JsValue::from_str("ui-input-trigger: menu dictionaries"),
        ],
    )?;
    Ok(())
}

fn defer_overlay(
    modules: &BrowserModules,
    ctx: &JsValue,
    service: &std::rc::Rc<BrowserInputTriggerService>,
) -> Result<(), JsValue> {
    let component = menu_view::component(modules);
    let service = service.clone();
    let callback = Closure::wrap(Box::new(move |scope: JsValue| -> Result<(), JsValue> {
        let slots = required(&scope, "slots", "Client Context")?;
        let sessions = required(&scope, "sessions", "Client Context")?;
        let inject_service = service.clone();
        let inject_sessions = sessions;
        let inject = Closure::wrap(
            Box::new(move |session_id: String| -> Result<JsValue, JsValue> {
                let actx =
                    call_method(&inject_sessions, "scope", &[JsValue::from_str(&session_id)])?;
                if actx.is_null() || actx.is_undefined() {
                    return Err(js_sys::Error::new(&format!(
                        "ui-input-trigger: session {session_id:?} resolved no scope"
                    ))
                    .into());
                }
                let controller = inject_service.session_of(&actx)?;
                let face = controller.face()?;
                let pick_controller = controller.clone();
                let on_pick = Closure::wrap(Box::new(
                    move |source: String, index: usize| -> Result<(), JsValue> {
                        pick_controller.pick(&source, index)
                    },
                )
                    as Box<dyn FnMut(String, usize) -> Result<(), JsValue>>);
                let dismiss_controller = controller;
                let on_dismiss = Closure::wrap(
                    Box::new(move || dismiss_controller.dismiss()) as Box<dyn FnMut()>
                );
                object(&[
                    ("menu", required(&face, "menu", "InputTriggerController")?),
                    ("onPick", on_pick.into_js_value()),
                    ("onDismiss", on_dismiss.into_js_value()),
                ])
                .map(Into::into)
            }) as Box<dyn FnMut(String) -> Result<JsValue, JsValue>>,
        );
        let registration_slots = slots.clone();
        let registration_component = component.clone();
        let installer = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
            let options = object(&[
                ("name", JsValue::from_str("conversation.input.overlay")),
                ("id", JsValue::from_str("slash-menu")),
                ("order", JsValue::from_f64(0.0)),
                ("locale", JsValue::from_str(MENU_NS)),
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
    for dependency in ["slots", "inputTriggers", "sessions"] {
        dependencies.push(&JsValue::from_str(dependency));
    }
    call_method(
        ctx,
        "inject",
        &[dependencies.into(), callback.into_js_value()],
    )?;
    Ok(())
}
