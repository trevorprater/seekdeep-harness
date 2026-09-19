//! Cordis adapter for subagent references, navigation actions, and presentation slots.

use js_sys::{Array, Function, Object, Promise};
use wasm_bindgen::{JsValue, closure::Closure};

use super::{
    BrowserModules, call_method, object, optional, read_only_component, required, required_bool,
    required_string, set,
};
use crate::{
    AddressedSubagentState, SUBAGENT_LOCALES, SUBAGENT_NS, SubagentMode, SubagentReadOnlyReason,
    picked_reference, select_read_only_subagent, serialized_reference,
};

pub(super) fn apply(modules: &BrowserModules, ctx: &JsValue) -> Result<(), JsValue> {
    let input_triggers = required(ctx, "inputTriggers", "Client Context")?;
    let sessions = required(ctx, "sessions", "Client Context")?;
    let slots = required(ctx, "slots", "Client Context")?;
    let locale = required(ctx, "locale", "Client Context")?;
    own_locale_dictionaries(ctx, &locale)?;
    own_reference_source(ctx, &input_triggers, &sessions)?;
    own_catalog_action(modules, &slots, &sessions)?;
    own_read_only_composer(modules, &slots)?;
    Ok(())
}

fn own_reference_source(
    ctx: &JsValue,
    input_triggers: &JsValue,
    sessions: &JsValue,
) -> Result<(), JsValue> {
    let source = source_object(sessions)?;
    let triggers = input_triggers.clone();
    let setup_source = source;
    let setup = Closure::wrap(Box::new(move || {
        call_method(
            &triggers,
            "registerSource",
            std::slice::from_ref(&setup_source),
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        ctx,
        "effect",
        &[
            setup.into_js_value(),
            JsValue::from_str("ui-subagent: @ source"),
        ],
    )?;
    Ok(())
}

fn source_object(sessions: &JsValue) -> Result<JsValue, JsValue> {
    let source = Object::new();
    set(&source, "trigger", &JsValue::from_str("@"))?;
    set(&source, "name", &JsValue::from_str("subagent"))?;

    let candidate_sessions = sessions.clone();
    let candidates = Closure::wrap(
        Box::new(move |session: JsValue, request: JsValue| -> Promise {
            match required_string(&request, "query", "subagent candidate request")
                .and_then(|query| child_labels(&candidate_sessions, &session, &query))
                .and_then(|labels| {
                    let values = Array::new();
                    for label in labels {
                        let candidate: JsValue =
                            object(&[("name", JsValue::from_str(&label))])?.into();
                        values.push(&candidate);
                    }
                    Ok::<JsValue, JsValue>(values.into())
                }) {
                Ok(values) => Promise::resolve(&values),
                Err(error) => Promise::reject(&error),
            }
        }) as Box<dyn FnMut(JsValue, JsValue) -> Promise>,
    );
    set(&source, "candidates", &candidates.into_js_value())?;

    let lexicon_sessions = sessions.clone();
    let lexicon = Closure::wrap(
        Box::new(move |session: JsValue| -> Result<JsValue, JsValue> {
            let labels = child_labels(&lexicon_sessions, &session, "")?;
            let values = Array::new();
            for label in labels {
                values.push(&JsValue::from_str(&label));
            }
            Ok(values.into())
        }) as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    );
    set(&source, "lexicon", &lexicon.into_js_value())?;

    let subscribe_sessions = sessions.clone();
    let subscribe = Closure::wrap(Box::new(
        move |_session: JsValue, listener: Function| -> Result<JsValue, JsValue> {
            let list = required(&subscribe_sessions, "list", "Sessions service")?;
            call_method(&list, "subscribe", &[listener.into()])
        },
    )
        as Box<dyn FnMut(JsValue, Function) -> Result<JsValue, JsValue>>);
    set(&source, "subscribeLexicon", &subscribe.into_js_value())?;

    let pick = Closure::wrap(Box::new(move |input: JsValue| -> Result<JsValue, JsValue> {
        let candidate = required(&input, "candidate", "subagent pick")?;
        let name = required_string(&candidate, "name", "subagent candidate")?;
        object(&[("text", JsValue::from_str(&picked_reference(&name)))]).map(Into::into)
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    set(&source, "onPick", &pick.into_js_value())?;

    let codec = Object::new();
    let clipboard = Closure::wrap(Box::new(move |label: String| serialized_reference(&label))
        as Box<dyn FnMut(String) -> String>);
    set(&codec, "clipboardText", &clipboard.into_js_value())?;
    let serialize = Closure::wrap(Box::new(move |label: String, _signal: JsValue| -> Promise {
        Promise::resolve(&JsValue::from_str(&serialized_reference(&label)))
    }) as Box<dyn FnMut(String, JsValue) -> Promise>);
    set(&codec, "serialize", &serialize.into_js_value())?;
    set(&source, "codec", &codec.into())?;
    Ok(source.into())
}

fn child_labels(
    sessions: &JsValue,
    session: &JsValue,
    query: &str,
) -> Result<Vec<String>, JsValue> {
    let parent = required_string(session, "sessionId", "Client Session Context")?;
    let list = required(sessions, "list", "Sessions service")?;
    let snapshot = call_method(&list, "getSnapshot", &[])?;
    let by_id = required(&snapshot, "byId", "Session list state")?;
    let values = Object::values(&Object::from(by_id));
    let mut labels = Vec::new();
    for summary in values.iter() {
        if super::optional_string(&summary, "parentId")?.as_deref() != Some(&parent)
            || !required_bool(&summary, "running", "Session summary")?
        {
            continue;
        }
        let label = required_string(&summary, "displayTitle", "Session summary")?;
        if label.contains(query) {
            labels.push(label);
        }
    }
    Ok(labels)
}

fn own_catalog_action(
    modules: &BrowserModules,
    slots: &JsValue,
    sessions: &JsValue,
) -> Result<(), JsValue> {
    let component = super::catalog_action::component(modules);
    let action_sessions = sessions.clone();
    let inject = Closure::wrap(Box::new(
        move |_parent_session_id: JsValue| -> Result<JsValue, JsValue> {
            catalog_actions(&action_sessions)
        },
    ) as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    let registration_slots = slots.clone();
    let installer = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let options = object(&[
            (
                "name",
                JsValue::from_str("conversation.session.header.actions"),
            ),
            ("id", JsValue::from_str("subagent-catalog")),
            ("order", JsValue::from_f64(10.0)),
            ("locale", JsValue::from_str(SUBAGENT_NS)),
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
            JsValue::from_str("conversation.session.header.actions"),
            installer.into_js_value(),
        ],
    )?;
    Ok(())
}

fn catalog_actions(sessions: &JsValue) -> Result<JsValue, JsValue> {
    let actions = Object::new();
    let open_sessions = sessions.clone();
    let open = Closure::wrap(Box::new(move |address: JsValue| -> Result<(), JsValue> {
        call_method(&open_sessions, "openSubagent", &[address]).map(|_| ())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    set(&actions, "openChild", &open.into_js_value())?;
    let refresh_sessions = sessions.clone();
    let refresh = Closure::wrap(Box::new(move |parent: JsValue| -> Result<(), JsValue> {
        call_method(&refresh_sessions, "refreshSubagents", &[parent]).map(|_| ())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    set(&actions, "refresh", &refresh.into_js_value())?;
    let catalog_sessions = sessions.clone();
    let set_open = Closure::wrap(Box::new(
        move |parent: JsValue, open: bool| -> Result<(), JsValue> {
            call_method(
                &catalog_sessions,
                "setSubagentCatalogOpen",
                &[parent, JsValue::from_bool(open)],
            )
            .map(|_| ())
        },
    )
        as Box<dyn FnMut(JsValue, bool) -> Result<(), JsValue>>);
    set(&actions, "setCatalogOpen", &set_open.into_js_value())?;
    Ok(actions.into())
}

fn own_read_only_composer(modules: &BrowserModules, slots: &JsValue) -> Result<(), JsValue> {
    let component = read_only_component(modules);
    let select = Closure::wrap(Box::new(move |owner: JsValue| select_read_only(&owner))
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    let registration_slots = slots.clone();
    let installer = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let options = object(&[
            ("name", JsValue::from_str("conversation.composer")),
            ("priority", JsValue::from_f64(-10.0)),
            ("locale", JsValue::from_str(SUBAGENT_NS)),
            ("select", select.as_ref().clone()),
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
            JsValue::from_str("conversation.composer"),
            installer.into_js_value(),
        ],
    )?;
    Ok(())
}

fn select_read_only(owner: &JsValue) -> Result<JsValue, JsValue> {
    let Some(session) = optional(owner, "session")? else {
        return Ok(JsValue::NULL);
    };
    let Some(subagent) = optional(&session, "subagent")? else {
        return Ok(JsValue::NULL);
    };
    let address = required(&subagent, "address", "subagent conversation")?;
    let mode = match required_string(&address, "mode", "subagent address")?.as_str() {
        "one-shot" => SubagentMode::OneShot,
        "continuable" => SubagentMode::Continuable,
        other => {
            return Err(js_sys::Error::new(&format!("unknown subagent mode {other:?}")).into());
        }
    };
    let state = AddressedSubagentState {
        mode,
        parent_available: required_bool(&subagent, "parentAvailable", "subagent conversation")?,
        running: optional(&session, "running")?
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
    };
    let Some(reason) = select_read_only_subagent(Some(state)) else {
        return Ok(JsValue::NULL);
    };
    object(&[(
        "reason",
        JsValue::from_str(match reason {
            SubagentReadOnlyReason::OneShot => "one-shot",
            SubagentReadOnlyReason::ParentUnavailable => "parent-unavailable",
        }),
    )])
    .map(Into::into)
}

fn own_locale_dictionaries(ctx: &JsValue, locale: &JsValue) -> Result<(), JsValue> {
    let zh = Object::new();
    let en = Object::new();
    for (key, zh_value, en_value) in SUBAGENT_LOCALES {
        set(&zh, key, &JsValue::from_str(zh_value))?;
        set(&en, key, &JsValue::from_str(en_value))?;
    }
    let dictionaries = object(&[("zh", zh.into()), ("en", en.into())])?;
    let locale = locale.clone();
    let installer = Closure::wrap(Box::new(move || {
        call_method(
            &locale,
            "register",
            &[JsValue::from_str(SUBAGENT_NS), dictionaries.clone().into()],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        ctx,
        "effect",
        &[
            installer.into_js_value(),
            JsValue::from_str("ui-subagent: dictionaries"),
        ],
    )?;
    Ok(())
}
