//! JavaScript `Promise`, `AbortController`, listener, and Cordis adapter for the portable cache.

use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_identity::SessionId;
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};
use wasm_bindgen_futures::{JsFuture, future_to_promise};

use super::{
    BrowserModules, call_method, js_error_from_display, object, required, required_string, set,
    skill_row_component, translated,
};
use crate::{SKILL_LOCALES, SKILL_NS, SkillCatalogCache, SkillCatalogDecision, SkillCatalogEntry};

#[derive(Clone)]
struct FetchBoundary {
    promise: Promise,
    abort: JsValue,
}

#[derive(Default)]
struct PluginState {
    cache: SkillCatalogCache,
    boundaries: BTreeMap<(SessionId, u64), FetchBoundary>,
    listeners: BTreeMap<SessionId, BTreeMap<u64, Function>>,
    next_listener: u64,
}

pub(super) fn apply(modules: &BrowserModules, ctx: &JsValue) -> Result<(), JsValue> {
    let input_triggers = required(ctx, "inputTriggers", "Client Context")?;
    let connection = required(ctx, "connection", "Client Context")?;
    let sessions = required(ctx, "sessions", "Client Context")?;
    let slots = required(ctx, "slots", "Client Context")?;
    let locale = required(ctx, "locale", "Client Context")?;
    let remote = required(ctx, "remote", "Client Context")?;
    let api = required(&connection, "api", "connection")?;
    let skills = required(&api, "skills", "connection API")?;
    own_locale_dictionaries(ctx, &locale)?;
    let translate =
        call_method(&locale, "bind", &[JsValue::from_str(SKILL_NS)])?.dyn_into::<Function>()?;
    own_toolview(modules, &slots)?;

    let state = Rc::new(RefCell::new(PluginState::default()));
    let source = source_object(&state, &sessions, &skills, &translate)?;

    let preset_state = state.clone();
    let preset = Closure::wrap(Box::new(move |session_id: String, _preset: JsValue| {
        invalidate(&preset_state, &SessionId::new(session_id));
    }) as Box<dyn FnMut(String, JsValue)>);
    call_method(
        &remote,
        "$on",
        &[
            JsValue::from_str("agent-preset/selected"),
            preset.into_js_value(),
        ],
    )?;
    let reset_state = state.clone();
    let reset = Closure::wrap(Box::new(move || clear_all(&reset_state)) as Box<dyn FnMut()>);
    call_method(
        ctx,
        "on",
        &[JsValue::from_str("connection/reset"), reset.into_js_value()],
    )?;

    let setup_triggers = input_triggers;
    let setup_source = source;
    let cleanup_state = state;
    let setup = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let unregister = call_method(
            &setup_triggers,
            "registerSource",
            std::slice::from_ref(&setup_source),
        )?
        .dyn_into::<Function>()?;
        let state = cleanup_state.clone();
        Ok(Closure::wrap(Box::new(move || {
            let _ = unregister.call0(&JsValue::UNDEFINED);
            clear_all(&state);
        }) as Box<dyn FnMut()>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        ctx,
        "effect",
        &[setup.into_js_value(), JsValue::from_str("ui-skill: source")],
    )?;
    Ok(())
}

fn own_toolview(modules: &BrowserModules, slots: &JsValue) -> Result<(), JsValue> {
    let component = skill_row_component(modules);
    let registration_slots = slots.clone();
    let installer = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let options = object(&[
            ("name", JsValue::from_str("tool.call.toolview")),
            ("key", JsValue::from_str("skill")),
            ("locale", JsValue::from_str(SKILL_NS)),
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
            JsValue::from_str("tool.call.toolview"),
            installer.into_js_value(),
        ],
    )?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn source_object(
    state: &Rc<RefCell<PluginState>>,
    sessions: &JsValue,
    skills: &JsValue,
    translate: &Function,
) -> Result<JsValue, JsValue> {
    let source = Object::new();
    set(&source, "trigger", &JsValue::from_str("/"))?;
    set(&source, "name", &JsValue::from_str("skill"))?;
    set(&source, "order", &JsValue::from_f64(2.0))?;

    let candidate_state = state.clone();
    let candidate_sessions = sessions.clone();
    let candidate_skills = skills.clone();
    let candidate_translate = translate.clone();
    let candidates = Closure::wrap(
        Box::new(move |session: JsValue, request: JsValue| -> Promise {
            let state = candidate_state.clone();
            let sessions = candidate_sessions.clone();
            let skills = candidate_skills.clone();
            let translate = candidate_translate.clone();
            future_to_promise(async move {
                let session_id = session_id(&session)?;
                let query = required_string(&request, "query", "skill candidate request")?;
                let signal = required(&request, "signal", "skill candidate request")?;
                let promise = fetch_catalog(&state, &sessions, &skills, &session_id)?;
                let value = JsFuture::from(promise).await?;
                if Reflect::get(&signal, &JsValue::from_str("aborted"))?.as_bool() == Some(true) {
                    return Ok(Array::new().into());
                }
                let catalog = serde_wasm_bindgen::from_value::<Vec<SkillCatalogEntry>>(value)
                    .map_err(js_error_from_display)?;
                let marker = translated(&translate, "menu.userOnly")?
                    .as_string()
                    .ok_or_else(|| {
                        js_sys::Error::new("menu.userOnly must translate to a string")
                    })?;
                serde_wasm_bindgen::to_value(&SkillCatalogCache::candidates(
                    &catalog, &query, &marker,
                ))
                .map_err(js_error_from_display)
            })
        }) as Box<dyn FnMut(JsValue, JsValue) -> Promise>,
    );
    set(&source, "candidates", &candidates.into_js_value())?;

    let warm_state = state.clone();
    let warm_sessions = sessions.clone();
    let warm_skills = skills.clone();
    let warm = Closure::wrap(Box::new(move |session: JsValue| -> Result<(), JsValue> {
        let session_id = session_id(&session)?;
        let promise = fetch_catalog(&warm_state, &warm_sessions, &warm_skills, &session_id)?;
        let ignore = Closure::wrap(Box::new(|_error: JsValue| {}) as Box<dyn FnMut(JsValue)>);
        let _ = promise.catch(&ignore);
        drop(ignore.into_js_value());
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    set(&source, "warm", &warm.into_js_value())?;

    let lexicon_state = state.clone();
    let lexicon = Closure::wrap(
        Box::new(move |session: JsValue| -> Result<JsValue, JsValue> {
            let session_id = session_id(&session)?;
            Ok(lexicon_state.borrow().cache.lexicon(&session_id).map_or(
                JsValue::UNDEFINED,
                |names| {
                    let values = Array::new();
                    for name in names {
                        values.push(&JsValue::from_str(&name));
                    }
                    values.into()
                },
            ))
        }) as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    );
    set(&source, "lexicon", &lexicon.into_js_value())?;

    let subscribe_state = state.clone();
    let subscribe = Closure::wrap(Box::new(
        move |session: JsValue, listener: Function| -> Result<JsValue, JsValue> {
            let session_id = session_id(&session)?;
            let id = {
                let mut state = subscribe_state.borrow_mut();
                state.next_listener = state
                    .next_listener
                    .checked_add(1)
                    .ok_or_else(|| js_sys::Error::new("skill lexicon listener id exhausted"))?;
                let id = state.next_listener;
                state
                    .listeners
                    .entry(session_id.clone())
                    .or_default()
                    .insert(id, listener);
                id
            };
            let state = subscribe_state.clone();
            Ok(Closure::wrap(Box::new(move || {
                let mut state = state.borrow_mut();
                if let Some(listeners) = state.listeners.get_mut(&session_id) {
                    listeners.remove(&id);
                    if listeners.is_empty() {
                        state.listeners.remove(&session_id);
                    }
                }
            }) as Box<dyn FnMut()>)
            .into_js_value())
        },
    )
        as Box<dyn FnMut(JsValue, Function) -> Result<JsValue, JsValue>>);
    set(&source, "subscribeLexicon", &subscribe.into_js_value())?;

    let pick = Closure::wrap(Box::new(move |input: JsValue| -> Result<JsValue, JsValue> {
        let candidate = required(&input, "candidate", "skill pick")?;
        let name = required_string(&candidate, "name", "skill candidate")?;
        object(&[(
            "text",
            JsValue::from_str(&SkillCatalogCache::picked_text(&name)),
        )])
        .map(Into::into)
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    set(&source, "onPick", &pick.into_js_value())?;
    Ok(source.into())
}

fn fetch_catalog(
    state: &Rc<RefCell<PluginState>>,
    sessions: &JsValue,
    skills: &JsValue,
    session_id: &SessionId,
) -> Result<Promise, JsValue> {
    let addressed = !call_method(
        sessions,
        "subagentAddress",
        &[JsValue::from_str(session_id.as_str())],
    )?
    .is_undefined();
    let decision = state.borrow_mut().cache.begin(session_id, addressed);
    match decision {
        SkillCatalogDecision::Addressed => {
            let empty: JsValue = Array::new().into();
            Ok(Promise::resolve(&empty))
        }
        SkillCatalogDecision::Settled(skills) => Ok(Promise::resolve(
            &serde_wasm_bindgen::to_value(&skills).map_err(js_error_from_display)?,
        )),
        SkillCatalogDecision::Join(generation) => state
            .borrow()
            .boundaries
            .get(&(session_id.clone(), generation.value()))
            .map(|boundary| boundary.promise.clone())
            .ok_or_else(|| js_sys::Error::new("skill cache omitted joined generation").into()),
        SkillCatalogDecision::Start(generation) => {
            let controller = construct("AbortController", &[])?;
            let signal = required(&controller, "signal", "AbortController")?;
            let request_state = state.clone();
            let request_skills = skills.clone();
            let request_session = session_id.clone();
            let promise = future_to_promise(async move {
                let result = async {
                    let returned = call_method(
                        &request_skills,
                        "list",
                        &[
                            object(&[("sessionId", JsValue::from_str(request_session.as_str()))])?
                                .into(),
                            signal,
                        ],
                    )?;
                    let envelope = JsFuture::from(Promise::resolve(&returned)).await?;
                    let result = required(&envelope, "result", "skill.list response")?;
                    if !required(&result, "ok", "skill.list result")?
                        .as_bool()
                        .unwrap_or(false)
                    {
                        let error = required(&result, "error", "skill.list result")?;
                        let code = required_string(&error, "code", "skill.list error")?;
                        let message = required_string(&error, "message", "skill.list error")?;
                        return Err(js_sys::Error::new(&format!(
                            "skill.list failed: {code}: {message}"
                        ))
                        .into());
                    }
                    let value = required(&result, "value", "skill.list result")?;
                    let catalog = serde_wasm_bindgen::from_value::<Vec<SkillCatalogEntry>>(
                        required(&value, "skills", "skill.list value")?,
                    )
                    .map_err(js_error_from_display)?;
                    Ok::<_, JsValue>(catalog)
                }
                .await;
                match result {
                    Ok(catalog) => {
                        let current = request_state.borrow_mut().cache.settle_success(
                            &request_session,
                            generation,
                            catalog.clone(),
                        );
                        if current {
                            notify(&request_state, &request_session);
                        }
                        serde_wasm_bindgen::to_value(&catalog).map_err(js_error_from_display)
                    }
                    Err(error) => {
                        let mut state = request_state.borrow_mut();
                        state.cache.settle_failure(&request_session, generation);
                        state
                            .boundaries
                            .remove(&(request_session.clone(), generation.value()));
                        Err(error)
                    }
                }
            });
            state.borrow_mut().boundaries.insert(
                (session_id.clone(), generation.value()),
                FetchBoundary {
                    promise: promise.clone(),
                    abort: controller,
                },
            );
            Ok(promise)
        }
    }
}

fn invalidate(state: &Rc<RefCell<PluginState>>, session_id: &SessionId) {
    let boundary = {
        let mut state = state.borrow_mut();
        state.cache.invalidate(session_id).and_then(|generation| {
            state
                .boundaries
                .remove(&(session_id.clone(), generation.value()))
        })
    };
    if let Some(boundary) = boundary {
        let _ = call_method(&boundary.abort, "abort", &[]);
        notify(state, session_id);
    }
}

fn clear_all(state: &Rc<RefCell<PluginState>>) {
    let entries = {
        let mut state = state.borrow_mut();
        state
            .cache
            .clear()
            .into_iter()
            .map(|(session_id, generation)| {
                let boundary = state
                    .boundaries
                    .remove(&(session_id.clone(), generation.value()));
                (session_id, boundary)
            })
            .collect::<Vec<_>>()
    };
    for (session_id, boundary) in entries {
        if let Some(boundary) = boundary {
            let _ = call_method(&boundary.abort, "abort", &[]);
        }
        notify(state, &session_id);
    }
}

fn notify(state: &Rc<RefCell<PluginState>>, session_id: &SessionId) {
    let listeners = state
        .borrow()
        .listeners
        .get(session_id)
        .map(|listeners| listeners.values().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    for listener in listeners {
        if let Err(error) = listener.call0(&JsValue::UNDEFINED)
            && let Ok(console) = Reflect::get(&js_sys::global(), &JsValue::from_str("console"))
        {
            let _ = call_method(
                &console,
                "error",
                &[
                    JsValue::from_str("[ui-skill] lexicon listener failed:"),
                    error,
                ],
            );
        }
    }
}

fn session_id(session: &JsValue) -> Result<SessionId, JsValue> {
    required_string(session, "sessionId", "Client Session Context").map(SessionId::new)
}

fn construct(name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let constructor =
        Reflect::get(&js_sys::global(), &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    Reflect::construct(&constructor, &args)
}

fn own_locale_dictionaries(ctx: &JsValue, locale: &JsValue) -> Result<(), JsValue> {
    let zh = Object::new();
    let en = Object::new();
    for (key, zh_value, en_value) in SKILL_LOCALES {
        set(&zh, key, &JsValue::from_str(zh_value))?;
        set(&en, key, &JsValue::from_str(en_value))?;
    }
    let dictionaries = object(&[("zh", zh.into()), ("en", en.into())])?;
    let locale = locale.clone();
    let installer = Closure::wrap(Box::new(move || {
        call_method(
            &locale,
            "register",
            &[JsValue::from_str(SKILL_NS), dictionaries.clone().into()],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        ctx,
        "effect",
        &[
            installer.into_js_value(),
            JsValue::from_str("ui-skill: dictionaries"),
        ],
    )?;
    Ok(())
}
