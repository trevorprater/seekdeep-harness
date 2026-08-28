//! Browser Client plugin registration backed entirely by compiled Rust/WASM.

use js_sys::{Array, Function, Object, Promise, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, future_to_promise};

use crate::{
    DURATION_PERSISTENCE_KEY, INJECT, LOCALE_NAMESPACE, TRAJECTORY_EN, TRAJECTORY_ZH,
    trajectory_event_definitions, trajectory_runtime_module, trajectory_view_component,
    trajectory_view_definition,
};

/// Applies the browser trajectory plugin to a caller-bound Client Context.
///
/// # Errors
///
/// Returns missing services, Definition/Slot registration, locale, store, paging, or component
/// construction failures.
#[wasm_bindgen(js_name = applyClientUiTrajectory)]
#[allow(clippy::needless_pass_by_value)]
pub fn apply_client_ui_trajectory(ctx: JsValue) -> Result<(), JsValue> {
    let runtime = trajectory_runtime_module()?;
    let slots = required_service(&ctx, "slots")?;
    let events = required_service(&ctx, "conversationEvents")?;
    let views = required_service(&ctx, "conversationViews")?;
    let sessions = required_service(&ctx, "sessions")?;
    let locale = required_service(&ctx, "locale")?;
    own_locale_dictionaries(&ctx, &locale)?;
    let translate = call_method(&locale, "bind", &[JsValue::from_str(LOCALE_NAMESPACE)])?
        .dyn_into::<Function>()?;
    let duration = create_duration_store(&runtime)?;

    for definition in trajectory_event_definitions() {
        call_method(
            &events,
            "register",
            &[seekdeep_client_runtime::native_conversation_node_definition_to_js(definition)?],
        )?;
    }
    call_method(
        &views,
        "register",
        &[
            seekdeep_client_runtime::native_conversation_view_definition_to_js(
                trajectory_view_definition(),
            )?,
        ],
    )?;

    let component = trajectory_view_component()?;
    let installer_slots = slots.clone();
    let installer_sessions = sessions;
    let installer_duration = duration;
    let installer_translate = translate;
    let installer = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let label_translate = installer_translate.clone();
        let label = Closure::wrap(Box::new(move || {
            label_translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("view.trajectory"))
        }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
        let inject_sessions = installer_sessions.clone();
        let inject_duration = installer_duration.clone();
        let inject = Closure::wrap(Box::new(
            move |session_id: JsValue| -> Result<JsValue, JsValue> {
                injected_view_face(&inject_sessions, &inject_duration, &session_id)
            },
        )
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
        let options = object(&[
            ("name", JsValue::from_str("conversation.view")),
            ("id", JsValue::from_str("trajectory")),
            ("order", JsValue::from_f64(10.0)),
            ("locale", JsValue::from_str(LOCALE_NAMESPACE)),
            ("label", label.into_js_value()),
            ("inject", inject.into_js_value()),
        ])?;
        call_method(
            &installer_slots,
            "register",
            &[options.into(), component.clone()],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        &slots,
        "inject",
        &[
            JsValue::from_str("conversation.view"),
            installer.into_js_value(),
        ],
    )?;
    Ok(())
}

/// Returns the exact browser Client dependency list.
#[wasm_bindgen(js_name = trajectoryInject)]
pub fn trajectory_inject() -> Array {
    let values = Array::new();
    for dependency in INJECT {
        values.push(&JsValue::from_str(dependency));
    }
    values
}

fn create_duration_store(runtime: &JsValue) -> Result<JsValue, JsValue> {
    let persist = object(&[("name", JsValue::from_str(DURATION_PERSISTENCE_KEY))])?;
    let options = object(&[("persist", persist.into())])?;
    call_method(
        runtime,
        "createSnapshotStore",
        &[JsValue::FALSE, options.into()],
    )
}

fn injected_view_face(
    sessions: &JsValue,
    duration: &JsValue,
    session_id: &JsValue,
) -> Result<JsValue, JsValue> {
    let id = session_id
        .as_string()
        .ok_or_else(|| js_sys::Error::new("ui-trajectory: injected Session id must be a string"))?;
    let binding = call_method(sessions, "binding", std::slice::from_ref(session_id))?;
    let session = if binding.is_null() || binding.is_undefined() {
        JsValue::UNDEFINED
    } else {
        Reflect::get(&binding, &JsValue::from_str("session"))?
    };
    if session.is_undefined() {
        return Err(
            js_sys::Error::new(&format!("ui-trajectory: session {id:?} is unavailable")).into(),
        );
    }
    let hooks = object(&[("duration", duration.clone())])?;
    let paging_session = session.clone();
    let load_older = Closure::wrap(Box::new(move || -> Promise {
        let session = paging_session.clone();
        future_to_promise(async move {
            let before = trajectory_view_snapshot(&session)?;
            let returned = call_method(&session, "loadOlder", &[])?;
            JsFuture::from(Promise::resolve(&returned)).await?;
            let after = trajectory_view_snapshot(&session)?;
            Ok(JsValue::from_bool(!Object::is(&before, &after)))
        })
    }) as Box<dyn FnMut() -> Promise>);
    let duration_store = duration.clone();
    let set_actual_duration = Closure::wrap(Box::new(move |value: bool| {
        call_method(&duration_store, "set", &[JsValue::from_bool(value)])
    })
        as Box<dyn FnMut(bool) -> Result<JsValue, JsValue>>);
    object(&[
        ("hooks", hooks.into()),
        ("loadOlder", load_older.into_js_value()),
        ("setActualDuration", set_actual_duration.into_js_value()),
    ])
    .map(Into::into)
}

fn trajectory_view_snapshot(session: &JsValue) -> Result<JsValue, JsValue> {
    let snapshot = call_method(session, "getSnapshot", &[])?;
    let views = required(&snapshot, "views", "Session snapshot")?;
    call_method(&views, "get", &[JsValue::from_str("trajectory")])
}

fn own_locale_dictionaries(ctx: &JsValue, locale: &JsValue) -> Result<(), JsValue> {
    let dictionaries = object(&[
        ("zh", dictionary(TRAJECTORY_ZH)?.into()),
        ("en", dictionary(TRAJECTORY_EN)?.into()),
    ])?;
    let locale = locale.clone();
    let installer = Closure::wrap(Box::new(move || {
        call_method(
            &locale,
            "register",
            &[
                JsValue::from_str(LOCALE_NAMESPACE),
                dictionaries.clone().into(),
            ],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        ctx,
        "effect",
        &[
            installer.into_js_value(),
            JsValue::from_str("ui-trajectory: dictionaries"),
        ],
    )?;
    Ok(())
}

fn dictionary(entries: &[(&str, &str)]) -> Result<Object, JsValue> {
    let dictionary = Object::new();
    for (key, value) in entries {
        set(&dictionary, key, &JsValue::from_str(value))?;
    }
    Ok(dictionary)
}

fn required_service(ctx: &JsValue, key: &str) -> Result<JsValue, JsValue> {
    required(ctx, key, "Client Context")
}

fn required(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let entry = Reflect::get(value, &JsValue::from_str(key))?;
    if entry.is_null() || entry.is_undefined() {
        Err(js_sys::Error::new(&format!("{owner} omitted required property {key:?}")).into())
    } else {
        Ok(entry)
    }
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    method.apply(value, &args)
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
