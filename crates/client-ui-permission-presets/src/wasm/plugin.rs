//! Cordis adapter for Permission Settings, command UI, locale, and Slot registration.

use js_sys::{Function, Object, Promise};
use seekdeep_client_ui_commands::SelectConfirmation;
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};
use wasm_bindgen_futures::{JsFuture, future_to_promise, spawn_local};

use super::{
    BrowserModules, WasmPermissionPresetSettingsController, call_method, object, required,
    required_bool, required_string, set,
};
use crate::{
    ACCESS_LOCALES, ACCESS_NS, PermissionSelect, SETTINGS_LOCALES, SETTINGS_NS, try_popup_options,
};

pub(super) fn apply(modules: &BrowserModules, ctx: &JsValue) -> Result<(), JsValue> {
    let command_ui = required(ctx, "commandUi", "Client Context")?;
    let sessions = required(ctx, "sessions", "Client Context")?;
    let slots = required(ctx, "slots", "Client Context")?;
    let locale = required(ctx, "locale", "Client Context")?;
    let connection = required(ctx, "connection", "Client Context")?;
    let remote = required(ctx, "remote", "Client Context")?;
    own_access_locales(ctx, &locale)?;
    own_settings_locales(ctx, &locale)?;
    let translate =
        call_method(&locale, "bind", &[JsValue::from_str(ACCESS_NS)])?.dyn_into::<Function>()?;
    let api = required(&connection, "api", "connection")?;
    let browser = WasmPermissionPresetSettingsController::from_api(&api)?;
    let controller = browser.controller.clone();
    let store = browser.store_face.clone();
    own_invalidations(ctx, &remote, &controller)?;
    own_settings_row(modules, &slots, &controller, &store)?;
    own_command_decoration(ctx, &command_ui, &sessions, &translate)?;
    Ok(())
}

fn own_settings_row(
    modules: &BrowserModules,
    slots: &JsValue,
    controller: &std::rc::Rc<crate::PermissionPresetSettingsController>,
    store: &JsValue,
) -> Result<(), JsValue> {
    let load_controller = controller.clone();
    let load =
        Closure::wrap(
            Box::new(move || -> Promise { operation_promise(load_controller.load()) })
                as Box<dyn FnMut() -> Promise>,
        )
        .into_js_value();
    let select_controller = controller.clone();
    let select = Closure::wrap(Box::new(move |preset: String| -> Promise {
        operation_promise(select_controller.select(preset))
    }) as Box<dyn FnMut(String) -> Promise>)
    .into_js_value();
    let inject_store = store.clone();
    let inject_load = load;
    let inject_select = select;
    let inject = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let hooks = object(&[("permission", inject_store.clone())])?;
        object(&[
            ("hooks", hooks.into()),
            ("load", inject_load.clone()),
            ("select", inject_select.clone()),
        ])
        .map(Into::into)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let component = super::row::component(modules);
    let registration_slots = slots.clone();
    let installer = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let options = object(&[
            ("name", JsValue::from_str("settings.general.item")),
            ("id", JsValue::from_str("permission")),
            ("order", JsValue::from_f64(-20.0)),
            ("locale", JsValue::from_str(SETTINGS_NS)),
            ("inject", inject.as_ref().clone()),
        ])?;
        call_method(
            &registration_slots,
            "register",
            &[options.into(), component.clone()],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        slots,
        "inject",
        &[
            JsValue::from_str("settings.general.item"),
            installer.into_js_value(),
        ],
    )?;
    Ok(())
}

fn own_invalidations(
    ctx: &JsValue,
    remote: &JsValue,
    controller: &std::rc::Rc<crate::PermissionPresetSettingsController>,
) -> Result<(), JsValue> {
    let setup_ctx = ctx.clone();
    let setup_remote = remote.clone();
    let setup_controller = controller.clone();
    let setup = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let remote_controller = setup_controller.clone();
        let remote_listener =
            Closure::wrap(Box::new(move |namespace: String, _revision: JsValue| {
                if namespace == crate::PERMISSION_SETTINGS_NS
                    && let Some(future) = remote_controller.refresh_if_loaded()
                {
                    spawn_local(future);
                }
            }) as Box<dyn FnMut(String, JsValue)>);
        let remote_disposer = call_method(
            &setup_remote,
            "$on",
            &[
                JsValue::from_str("settings/document-updated"),
                remote_listener.into_js_value(),
            ],
        )?
        .dyn_into::<Function>()?;

        let reset_controller = setup_controller.clone();
        let reset_listener = Closure::wrap(Box::new(move || {
            if let Some(future) = reset_controller.refresh_if_loaded() {
                spawn_local(future);
            }
        }) as Box<dyn FnMut()>);
        let reset_disposer = match call_method(
            &setup_ctx,
            "on",
            &[
                JsValue::from_str("connection/reset"),
                reset_listener.into_js_value(),
            ],
        ) {
            Ok(value) => match value.dyn_into::<Function>() {
                Ok(disposer) => disposer,
                Err(error) => {
                    let _ = remote_disposer.call0(&JsValue::UNDEFINED);
                    setup_controller.dispose();
                    return Err(error);
                }
            },
            Err(error) => {
                let _ = remote_disposer.call0(&JsValue::UNDEFINED);
                setup_controller.dispose();
                return Err(error);
            }
        };
        let dispose_controller = setup_controller.clone();
        Ok(Closure::wrap(Box::new(move || {
            dispose_controller.dispose();
            let _ = remote_disposer.call0(&JsValue::UNDEFINED);
            let _ = reset_disposer.call0(&JsValue::UNDEFINED);
        }) as Box<dyn FnMut()>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        ctx,
        "effect",
        &[
            setup.into_js_value(),
            JsValue::from_str("ui-permission: settings invalidations"),
        ],
    )?;
    Ok(())
}

fn own_command_decoration(
    ctx: &JsValue,
    command_ui: &JsValue,
    sessions: &JsValue,
    translate: &Function,
) -> Result<(), JsValue> {
    let decoration = Object::new();
    set(&decoration, "name", &JsValue::from_str("permission"))?;
    let available_sessions = sessions.clone();
    let available = Closure::wrap(Box::new(move |session: JsValue| -> Result<bool, JsValue> {
        Ok(select_of(&session_for(&available_sessions, &session)?)?.is_some())
    }) as Box<dyn FnMut(JsValue) -> Result<bool, JsValue>>);
    set(&decoration, "available", &available.into_js_value())?;

    let ui = Object::new();
    set(&ui, "kind", &JsValue::from_str("popupSelect"))?;
    let option_sessions = sessions.clone();
    let option_translate = translate.clone();
    let options = Closure::wrap(Box::new(
        move |session: JsValue, _signal: JsValue| -> Result<Promise, JsValue> {
            let live = session_for(&option_sessions, &session)?;
            let Some(value) = select_of(&live)? else {
                return Err(js_sys::Error::new(
                    "permission presets are not available on this host",
                )
                .into());
            };
            let value = serde_wasm_bindgen::from_value::<PermissionSelect>(value)
                .map_err(|error| js_sys::Error::new(&error.to_string()))?;
            let rows = try_popup_options(&value, || confirmation(&option_translate))?;
            let rows = serde_wasm_bindgen::to_value(&rows)
                .map_err(|error| js_sys::Error::new(&error.to_string()))?;
            Ok(Promise::resolve(&rows))
        },
    )
        as Box<dyn FnMut(JsValue, JsValue) -> Result<Promise, JsValue>>);
    set(&ui, "options", &options.into_js_value())?;

    let select_sessions = sessions.clone();
    let on_select = Closure::wrap(
        Box::new(move |option: JsValue, session: JsValue| -> Promise {
            let sessions = select_sessions.clone();
            future_to_promise(async move {
                let live = session_for(&sessions, &session)?;
                if live.is_undefined() {
                    return Err(js_sys::Error::new("this session is not materialized yet").into());
                }
                let id = required_string(&option, "id", "permission option")?;
                let returned = call_method(
                    &live,
                    "command",
                    &[JsValue::from_str(&format!("/permission {id}"))],
                )?;
                let result = JsFuture::from(Promise::resolve(&returned)).await?;
                if !required_bool(&result, "ok", "Session command result")? {
                    let error = required(&result, "error", "Session command result")?;
                    let code = required_string(&error, "code", "Session command error")?;
                    let message = required_string(&error, "message", "Session command error")?;
                    return Err(js_sys::Error::new(&format!(
                        "permission switch failed: {code}: {message}"
                    ))
                    .into());
                }
                let value = required(&result, "value", "Session command result")?;
                if !required_bool(&value, "matched", "Session command result")? {
                    return Err(js_sys::Error::new("the host offers no /permission command").into());
                }
                Ok(JsValue::UNDEFINED)
            })
        }) as Box<dyn FnMut(JsValue, JsValue) -> Promise>,
    );
    set(&ui, "onSelect", &on_select.into_js_value())?;
    set(&decoration, "ui", &ui.into())?;

    let decorate = command_ui.clone();
    let setup_decoration: JsValue = decoration.into();
    let setup = Closure::wrap(Box::new(move || {
        call_method(
            &decorate,
            "decorate",
            std::slice::from_ref(&setup_decoration),
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        ctx,
        "effect",
        &[
            setup.into_js_value(),
            JsValue::from_str("ui-permission: /permission decoration"),
        ],
    )?;
    Ok(())
}

fn session_for(sessions: &JsValue, session: &JsValue) -> Result<JsValue, JsValue> {
    let id = required_string(session, "sessionId", "Client Session Context")?;
    let binding = call_method(sessions, "binding", &[JsValue::from_str(&id)])?;
    if binding.is_null() || binding.is_undefined() {
        Ok(JsValue::UNDEFINED)
    } else {
        required(&binding, "session", "Session binding")
    }
}

fn select_of(session: &JsValue) -> Result<Option<JsValue>, JsValue> {
    if session.is_undefined() {
        return Ok(None);
    }
    let projections = required(session, "projections", "Session face")?;
    let face = call_method(&projections, "faceOf", &[JsValue::from_str("permissions")])?;
    let value = call_method(&face, "getSnapshot", &[])?;
    Ok((!value.is_undefined()).then_some(value))
}

fn confirmation(translate: &Function) -> Result<SelectConfirmation, JsValue> {
    Ok(SelectConfirmation {
        title: translation_string(translate, "confirm.title")?,
        description: translation_string(translate, "confirm.description")?,
        acknowledge_label: translation_string(translate, "confirm.acknowledge")?,
        cancel_label: translation_string(translate, "confirm.cancel")?,
        confirm_label: translation_string(translate, "confirm.enable")?,
    })
}

fn translation_string(translate: &Function, key: &str) -> Result<String, JsValue> {
    translate
        .call1(&JsValue::UNDEFINED, &JsValue::from_str(key))?
        .as_string()
        .ok_or_else(|| js_sys::Error::new(&format!("{key} must translate to a string")).into())
}

fn own_access_locales(ctx: &JsValue, locale: &JsValue) -> Result<(), JsValue> {
    let zh = Object::new();
    let en = Object::new();
    for (key, zh_value, en_value) in ACCESS_LOCALES {
        set(&zh, key, &JsValue::from_str(zh_value))?;
        set(&en, key, &JsValue::from_str(en_value))?;
    }
    let locale = locale.clone();
    let setup = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let zh_disposer = call_method(
            &locale,
            "register",
            &[
                JsValue::from_str(ACCESS_NS),
                JsValue::from_str("zh"),
                zh.clone().into(),
            ],
        )?
        .dyn_into::<Function>()?;
        let en_disposer = match call_method(
            &locale,
            "register",
            &[
                JsValue::from_str(ACCESS_NS),
                JsValue::from_str("en"),
                en.clone().into(),
            ],
        ) {
            Ok(value) => match value.dyn_into::<Function>() {
                Ok(disposer) => disposer,
                Err(error) => {
                    let _ = zh_disposer.call0(&JsValue::UNDEFINED);
                    return Err(error);
                }
            },
            Err(error) => {
                let _ = zh_disposer.call0(&JsValue::UNDEFINED);
                return Err(error);
            }
        };
        Ok(Closure::wrap(Box::new(move || {
            let _ = zh_disposer.call0(&JsValue::UNDEFINED);
            let _ = en_disposer.call0(&JsValue::UNDEFINED);
        }) as Box<dyn FnMut()>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        ctx,
        "effect",
        &[
            setup.into_js_value(),
            JsValue::from_str("ui-permission: Full access confirmation dictionaries"),
        ],
    )?;
    Ok(())
}

fn own_settings_locales(ctx: &JsValue, locale: &JsValue) -> Result<(), JsValue> {
    let zh = Object::new();
    let en = Object::new();
    for (key, zh_value, en_value) in SETTINGS_LOCALES {
        set(&zh, key, &JsValue::from_str(zh_value))?;
        set(&en, key, &JsValue::from_str(en_value))?;
    }
    let dictionaries = object(&[("zh", zh.into()), ("en", en.into())])?;
    let locale = locale.clone();
    let setup = Closure::wrap(Box::new(move || {
        call_method(
            &locale,
            "register",
            &[JsValue::from_str(SETTINGS_NS), dictionaries.clone().into()],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        ctx,
        "effect",
        &[
            setup.into_js_value(),
            JsValue::from_str("ui-permission: settings row dictionaries"),
        ],
    )?;
    Ok(())
}

fn operation_promise(future: futures::future::LocalBoxFuture<'static, ()>) -> Promise {
    future_to_promise(async move {
        future.await;
        Ok(JsValue::UNDEFINED)
    })
}
