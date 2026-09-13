//! Cordis assembly for the resolver, `/model` command, and composer model seat.

use std::rc::Rc;

use js_sys::{Function, Object, Promise};
use seekdeep_identity::SessionId;
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};
use wasm_bindgen_futures::{future_to_promise, spawn_local};

use super::{
    BrowserModules, call_method, object, required, required_string,
    resolver::BrowserModelDirectoryResolver, set, translated, translated_values,
};
use crate::{MODEL_LOCALES, MODEL_NS, ModelSelection, selection_of, try_options_of};

pub(super) fn apply(modules: &BrowserModules, ctx: &JsValue) -> Result<(), JsValue> {
    let command_ui = required(ctx, "commandUi", "Client Context")?;
    let connection = required(ctx, "connection", "Client Context")?;
    let locale = required(ctx, "locale", "Client Context")?;
    let sessions = required(ctx, "sessions", "Client Context")?;
    let slots = required(ctx, "slots", "Client Context")?;
    let remote = required(ctx, "remote", "Client Context")?;
    own_locale(ctx, &locale)?;
    let translate =
        call_method(&locale, "bind", &[JsValue::from_str(MODEL_NS)])?.dyn_into::<Function>()?;
    let api = required(&connection, "api", "connection")?;
    let sessions_api = required(&api, "sessions", "generated API")?;
    let resolver = BrowserModelDirectoryResolver::new(
        ctx.clone(),
        sessions.clone(),
        sessions_api,
        translate.clone(),
    );
    own_resolver(ctx, &remote, &resolver)?;
    own_command(ctx, &command_ui, &sessions, &resolver, &translate)?;
    own_seat(modules, &slots, &sessions, &resolver)?;
    Ok(())
}

pub(crate) fn own_resolver(
    ctx: &JsValue,
    remote: &JsValue,
    resolver: &Rc<BrowserModelDirectoryResolver>,
) -> Result<(), JsValue> {
    let reflect = required(ctx, "reflect", "Client Context")?;
    let face = resolver.face()?;
    let setup_ctx = ctx.clone();
    let setup_remote = remote.clone();
    let setup_resolver = resolver.clone();
    let setup = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let provide = call_method(
            &reflect,
            "provide",
            &[JsValue::from_str("modelDirectories"), face.clone()],
        )?
        .dyn_into::<Function>()?;
        let reset_resolver = setup_resolver.clone();
        let reset = Closure::wrap(Box::new(move || reset_resolver.reset_all()) as Box<dyn FnMut()>);
        let reset_disposer = match call_method(
            &setup_ctx,
            "on",
            &[JsValue::from_str("connection/reset"), reset.into_js_value()],
        ) {
            Ok(value) => match value.dyn_into::<Function>() {
                Ok(value) => value,
                Err(error) => {
                    let _ = provide.call0(&JsValue::UNDEFINED);
                    return Err(error);
                }
            },
            Err(error) => {
                let _ = provide.call0(&JsValue::UNDEFINED);
                return Err(error);
            }
        };
        let mut remote_disposers = Vec::new();
        for event in ["llm/adapters-updated", "settings/document-updated"] {
            let refresh_resolver = setup_resolver.clone();
            let refresh = Closure::wrap(Box::new(move |_first: JsValue, _second: JsValue| {
                refresh_resolver.refresh_all();
            }) as Box<dyn FnMut(JsValue, JsValue)>);
            match call_method(
                &setup_remote,
                "$on",
                &[JsValue::from_str(event), refresh.into_js_value()],
            ) {
                Ok(value) => match value.dyn_into::<Function>() {
                    Ok(disposer) => remote_disposers.push(disposer),
                    Err(error) => {
                        let _ = reset_disposer.call0(&JsValue::UNDEFINED);
                        let _ = provide.call0(&JsValue::UNDEFINED);
                        for disposer in remote_disposers {
                            let _ = disposer.call0(&JsValue::UNDEFINED);
                        }
                        return Err(error);
                    }
                },
                Err(error) => {
                    let _ = reset_disposer.call0(&JsValue::UNDEFINED);
                    let _ = provide.call0(&JsValue::UNDEFINED);
                    for disposer in remote_disposers {
                        let _ = disposer.call0(&JsValue::UNDEFINED);
                    }
                    return Err(error);
                }
            }
        }
        let dispose_resolver = setup_resolver.clone();
        Ok(Closure::wrap(Box::new(move || {
            dispose_resolver.dispose_all();
            for disposer in &remote_disposers {
                let _ = disposer.call0(&JsValue::UNDEFINED);
            }
            let _ = reset_disposer.call0(&JsValue::UNDEFINED);
            let _ = provide.call0(&JsValue::UNDEFINED);
        }) as Box<dyn FnMut()>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        ctx,
        "effect",
        &[
            setup.into_js_value(),
            JsValue::from_str("ui-model-selection: resolver"),
        ],
    )?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn own_command(
    ctx: &JsValue,
    command_ui: &JsValue,
    sessions: &JsValue,
    resolver: &Rc<BrowserModelDirectoryResolver>,
    translate: &Function,
) -> Result<(), JsValue> {
    let contribution = Object::new();
    set(&contribution, "name", &JsValue::from_str("model"))?;
    set(
        &contribution,
        "description",
        &translated(translate, "command.description")?,
    )?;
    let available_sessions = sessions.clone();
    let available = Closure::wrap(Box::new(move |session: JsValue| -> Result<bool, JsValue> {
        is_available(&available_sessions, &session)
    }) as Box<dyn FnMut(JsValue) -> Result<bool, JsValue>>);
    set(&contribution, "available", &available.into_js_value())?;
    let ui = Object::new();
    set(&ui, "kind", &JsValue::from_str("popupSelect"))?;
    let option_sessions = sessions.clone();
    let option_resolver = resolver.clone();
    let option_translate = translate.clone();
    let options = Closure::wrap(
        Box::new(move |session: JsValue, _signal: JsValue| -> Promise {
            let sessions = option_sessions.clone();
            let resolver = option_resolver.clone();
            let translate = option_translate.clone();
            future_to_promise(async move {
                if !is_available(&sessions, &session)? {
                    return Err(js_sys::Error::new(
                        "model selection is unavailable for addressed subagent sessions",
                    )
                    .into());
                }
                let id = SessionId::new(required_string(
                    &session,
                    "sessionId",
                    "Client Session Context",
                )?);
                let directory = resolver.directory(&id)?;
                let models = directory
                    .load()
                    .await
                    .map_err(|message| js_sys::Error::new(&message))?;
                let rows = try_options_of(&models, |message| {
                    translated_values(
                        &translate,
                        "option.loadError",
                        &[("message", JsValue::from_str(message))],
                    )?
                    .as_string()
                    .ok_or_else(|| {
                        JsValue::from(js_sys::Error::new(
                            "model load error must translate to a string",
                        ))
                    })
                })?;
                serde_wasm_bindgen::to_value(&rows)
                    .map_err(|error| js_sys::Error::new(&error.to_string()).into())
            })
        }) as Box<dyn FnMut(JsValue, JsValue) -> Promise>,
    );
    set(&ui, "options", &options.into_js_value())?;

    let select_sessions = sessions.clone();
    let select_resolver = resolver.clone();
    let on_select = Closure::wrap(
        Box::new(move |option: JsValue, session: JsValue| -> Promise {
            let sessions = select_sessions.clone();
            let resolver = select_resolver.clone();
            future_to_promise(async move {
                if !is_available(&sessions, &session)? {
                    return Err(js_sys::Error::new(
                        "model selection is unavailable for addressed subagent sessions",
                    )
                    .into());
                }
                let id = SessionId::new(required_string(
                    &session,
                    "sessionId",
                    "Client Session Context",
                )?);
                let directory = resolver.directory(&id)?;
                let option_id = required_string(&option, "id", "model option")?;
                let Some(selection) = selection_of(&directory.snapshot(), &option_id) else {
                    return Err(js_sys::Error::new(
                        "this provider's catalog failed to load — pick a model from a loaded group",
                    )
                    .into());
                };
                directory
                    .select(selection)
                    .await
                    .map_err(|message| js_sys::Error::new(&message))?;
                Ok(JsValue::UNDEFINED)
            })
        }) as Box<dyn FnMut(JsValue, JsValue) -> Promise>,
    );
    set(&ui, "onSelect", &on_select.into_js_value())?;
    set(&contribution, "ui", &ui.into())?;
    let command = command_ui.clone();
    let contribution: JsValue = contribution.into();
    let setup = Closure::wrap(Box::new(move || {
        call_method(&command, "register", std::slice::from_ref(&contribution))
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        ctx,
        "effect",
        &[
            setup.into_js_value(),
            JsValue::from_str("ui-model-selection: /model contribution"),
        ],
    )?;
    Ok(())
}

fn own_seat(
    modules: &BrowserModules,
    slots: &JsValue,
    sessions: &JsValue,
    resolver: &Rc<BrowserModelDirectoryResolver>,
) -> Result<(), JsValue> {
    let component = super::model_select::component(modules);
    let inject_sessions = sessions.clone();
    let inject_resolver = resolver.clone();
    let inject = Closure::wrap(
        Box::new(move |session_id: String| -> Result<JsValue, JsValue> {
            let id = SessionId::new(session_id);
            let directory_face = inject_resolver.face_for(&id)?;
            let directory = inject_resolver.directory(&id)?;
            let available = call_method(
                &inject_sessions,
                "subagentAddress",
                &[JsValue::from_str(id.as_str())],
            )?
            .is_undefined();
            let load_directory = directory.clone();
            let load = Closure::wrap(Box::new(move || {
                let directory = load_directory.clone();
                spawn_local(async move {
                    let _ = directory.load().await;
                });
            }) as Box<dyn FnMut()>);
            let select_directory = directory;
            let select = Closure::wrap(Box::new(move |selection: JsValue| -> Promise {
                if !available {
                    return Promise::resolve(&JsValue::FALSE);
                }
                let selection = serde_wasm_bindgen::from_value::<ModelSelection>(selection);
                let directory = select_directory.clone();
                future_to_promise(async move {
                    let accepted = match selection {
                        Ok(selection) => directory.select(selection).await.is_ok(),
                        Err(_) => false,
                    };
                    Ok(JsValue::from_bool(accepted))
                })
            }) as Box<dyn FnMut(JsValue) -> Promise>);
            object(&[
                ("available", JsValue::from_bool(available)),
                (
                    "directory",
                    required(&directory_face, "store", "ModelDirectory")?,
                ),
                ("load", load.into_js_value()),
                ("select", select.into_js_value()),
            ])
            .map(Into::into)
        }) as Box<dyn FnMut(String) -> Result<JsValue, JsValue>>,
    );
    let registration_slots = slots.clone();
    let installer = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let options = object(&[
            ("name", JsValue::from_str("conversation.input.model")),
            ("locale", JsValue::from_str(MODEL_NS)),
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
            JsValue::from_str("conversation.input.model"),
            installer.into_js_value(),
        ],
    )?;
    Ok(())
}

fn is_available(sessions: &JsValue, session: &JsValue) -> Result<bool, JsValue> {
    let id = required_string(session, "sessionId", "Client Session Context")?;
    Ok(call_method(sessions, "subagentAddress", &[JsValue::from_str(&id)])?.is_undefined())
}

fn own_locale(ctx: &JsValue, locale: &JsValue) -> Result<(), JsValue> {
    let zh = Object::new();
    let en = Object::new();
    for (key, zh_value, en_value) in MODEL_LOCALES {
        set(&zh, key, &JsValue::from_str(zh_value))?;
        set(&en, key, &JsValue::from_str(en_value))?;
    }
    let dictionaries = object(&[("zh", zh.into()), ("en", en.into())])?;
    let locale = locale.clone();
    let setup = Closure::wrap(Box::new(move || {
        call_method(
            &locale,
            "register",
            &[JsValue::from_str(MODEL_NS), dictionaries.clone().into()],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        ctx,
        "effect",
        &[
            setup.into_js_value(),
            JsValue::from_str("ui-model-selection: dictionaries"),
        ],
    )?;
    Ok(())
}
