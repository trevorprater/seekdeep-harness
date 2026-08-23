//! Browser runtime assembly for Slots, Sessions, Workspaces, and connection delivery.

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{
    WasmClientSlotRegistry, WasmConversationEventRegistry, WasmConversationViewRegistry,
    WasmSessionRuntime, WasmWorkspaceRuntime,
    wasm_conversation_adapter::{browser_event_definitions, browser_view_definitions},
};

/// Mounts the complete browser runtime into one Client Cordis context.
///
/// # Errors
///
/// Returns missing injected services, malformed faces, or synchronous connection-start failures.
#[wasm_bindgen(js_name = applyClientRuntime)]
#[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
pub fn apply_client_runtime(root: JsValue) -> Result<JsValue, JsValue> {
    let connection = context_service(&root, "connection")?;
    let api = required(&connection, "api", "Client connection")?;
    let remote = context_service(&root, "remote")?;

    let changed_root = root.clone();
    let on_changed = Closure::wrap(Box::new(move |key: String| {
        call_method(
            &changed_root,
            "emit",
            &[JsValue::from_str("slots/changed"), JsValue::from_str(&key)],
        )
        .unwrap_or_else(|error| wasm_bindgen::throw_val(error));
    }) as Box<dyn FnMut(String)>);
    let slots = WasmClientSlotRegistry::new(Some(
        on_changed.into_js_value().unchecked_into::<Function>(),
    ));
    let slots_face = slots.face_for(root.clone())?;
    provide(&root, "slots", &slots_face)?;

    let events = WasmConversationEventRegistry::new();
    let views = WasmConversationViewRegistry::new();
    let events_face = events.face_for(root.clone())?;
    let views_face = views.face_for(root.clone())?;
    provide(&root, "conversationEvents", &events_face)?;
    provide(&root, "conversationViews", &views_face)?;

    let event_definitions = browser_event_definitions(events.core_registry());
    let view_definitions = browser_view_definitions(views.core_registry());
    let sessions = WasmSessionRuntime::new_with_definitions(
        root.clone(),
        api.clone(),
        remote.clone(),
        event_definitions,
        view_definitions,
    )?;
    let workspaces = WasmWorkspaceRuntime::new(root.clone(), api, &sessions)?;
    let sessions: JsValue = sessions.into();
    let workspaces: JsValue = workspaces.into();
    provide(&root, "sessions", &sessions)?;
    provide(&root, "workspaces", &workspaces)?;

    let session_standard = Object::new();
    set(
        &session_standard,
        "list",
        &required(&sessions, "list", "Sessions service")?,
    )?;
    set(
        &session_standard,
        "provideInfo",
        &required(&sessions, "currentProvideInfo", "Sessions service")?,
    )?;
    own_cleanup(
        &root,
        slots.install_sessions(session_standard.into()),
        "runtime: Slot Sessions face",
    )?;
    let workspace_standard = Object::new();
    set(
        &workspace_standard,
        "list",
        &required(&workspaces, "list", "Workspaces service")?,
    )?;
    own_cleanup(
        &root,
        slots.install_workspaces(workspace_standard.into()),
        "runtime: Slot Workspaces face",
    )?;

    let rebuild_sessions = sessions.clone();
    let rebuild = Closure::wrap(Box::new(move || {
        call_method(&rebuild_sessions, "rebuildConversationRegistry", &[])
            .unwrap_or_else(|error| wasm_bindgen::throw_val(error));
    }) as Box<dyn FnMut()>);
    let rebuild: Function = rebuild.into_js_value().unchecked_into();
    let dispose_events = events.subscribe(rebuild.clone());
    let dispose_views = views.subscribe(rebuild);
    let cleanup = Closure::wrap(Box::new(move || {
        let _ = dispose_views.call0(&JsValue::UNDEFINED);
        let _ = dispose_events.call0(&JsValue::UNDEFINED);
    }) as Box<dyn FnMut()>);
    own_cleanup(
        &root,
        cleanup.into_js_value().unchecked_into(),
        "runtime: Conversation registry rebuild",
    )?;

    install_typert_agent_identity(&root, &sessions)?;
    let initial = required_function(&workspaces, "startInitialSelection", "Workspaces service")?
        .call0(&workspaces)?
        .dyn_into::<Function>()?;
    own_cleanup(&root, initial, "runtime: initial Workspace selection")?;

    let sinks = connection_sinks(&root, &sessions, &workspaces, &remote)?;
    let loop_handle =
        required_function(&connection, "start", "Client connection")?.call1(&connection, &sinks)?;
    let stop_loop = Closure::wrap(Box::new(move || {
        if let Ok(stop) = Reflect::get(&loop_handle, &JsValue::from_str("stop"))
            && let Ok(stop) = stop.dyn_into::<Function>()
        {
            let _ = stop.call0(&loop_handle);
        }
    }) as Box<dyn FnMut()>);
    own_cleanup(
        &root,
        stop_loop.into_js_value().unchecked_into(),
        "runtime: connection stream loop",
    )?;

    let result = Object::new();
    set(&result, "slots", &slots_face)?;
    set(&result, "conversationEvents", &events_face)?;
    set(&result, "conversationViews", &views_face)?;
    set(&result, "sessions", &sessions)?;
    set(&result, "workspaces", &workspaces)?;
    set(&result, "sinks", &sinks)?;
    Ok(result.into())
}

fn connection_sinks(
    root: &JsValue,
    sessions: &JsValue,
    workspaces: &JsValue,
    remote: &JsValue,
) -> Result<JsValue, JsValue> {
    let sinks = Object::new();
    let session_face = sessions.clone();
    let mux = Closure::wrap(Box::new(move |envelope: JsValue| {
        call_method(&session_face, "handleMuxEnvelope", &[envelope])
            .unwrap_or_else(|error| wasm_bindgen::throw_val(error));
    }) as Box<dyn FnMut(JsValue)>);
    set(&sinks, "onMuxEnvelope", &mux.into_js_value())?;

    let session_face = sessions.clone();
    let workspace_face = workspaces.clone();
    let remote_face = remote.clone();
    let host = Closure::wrap(Box::new(move |envelope: JsValue| {
        call_method(
            &session_face,
            "handleHostEnvelope",
            std::slice::from_ref(&envelope),
        )
        .unwrap_or_else(|error| wasm_bindgen::throw_val(error));
        call_method(
            &workspace_face,
            "handleHostEnvelope",
            std::slice::from_ref(&envelope),
        )
        .unwrap_or_else(|error| wasm_bindgen::throw_val(error));
        let payload =
            Reflect::get(&envelope, &JsValue::from_str("payload")).unwrap_or(JsValue::UNDEFINED);
        let frame_type = Reflect::get(&payload, &JsValue::from_str("type"))
            .ok()
            .and_then(|value| value.as_string());
        if frame_type.as_deref() == Some("host/remote-event") {
            let event =
                Reflect::get(&payload, &JsValue::from_str("event")).unwrap_or(JsValue::UNDEFINED);
            let args =
                Reflect::get(&payload, &JsValue::from_str("args")).unwrap_or(JsValue::UNDEFINED);
            call_method(&remote_face, "$dispatch", &[event, args])
                .unwrap_or_else(|error| wasm_bindgen::throw_val(error));
        }
    }) as Box<dyn FnMut(JsValue)>);
    set(&sinks, "onHostEnvelope", &host.into_js_value())?;

    let session_face = sessions.clone();
    let workspace_face = workspaces.clone();
    let reset_root = root.clone();
    let connected = Closure::wrap(Box::new(move |_description: JsValue| {
        call_method(&session_face, "handleConnected", &[])
            .unwrap_or_else(|error| wasm_bindgen::throw_val(error));
        call_method(&workspace_face, "handleConnected", &[])
            .unwrap_or_else(|error| wasm_bindgen::throw_val(error));
        call_method(
            &reset_root,
            "emit",
            &[JsValue::from_str("connection/reset")],
        )
        .unwrap_or_else(|error| wasm_bindgen::throw_val(error));
    }) as Box<dyn FnMut(JsValue)>);
    set(&sinks, "onConnected", &connected.into_js_value())?;

    let session_face = sessions.clone();
    let state_change = Closure::wrap(Box::new(move |state: String| {
        if state == "reconnecting" {
            call_method(&session_face, "handleDisconnected", &[])
                .unwrap_or_else(|error| wasm_bindgen::throw_val(error));
        }
    }) as Box<dyn FnMut(String)>);
    set(&sinks, "onStateChange", &state_change.into_js_value())?;
    Ok(sinks.into())
}

fn install_typert_agent_identity(root: &JsValue, sessions: &JsValue) -> Result<(), JsValue> {
    let typert = context_service(root, "typert")?;
    let contexts = required(&typert, "contexts", "Client Typert registry")?;
    let sessions = sessions.clone();
    let identity = Closure::wrap(Box::new(move |candidate: JsValue| {
        call_method(&sessions, "scopeOf", &[candidate]).unwrap_or(JsValue::UNDEFINED)
    }) as Box<dyn FnMut(JsValue) -> JsValue>);
    let descriptor = Object::new();
    set(&descriptor, "identity", &identity.into_js_value())?;
    required_function(&contexts, "registerClient", "Client Typert contexts")?.call2(
        &contexts,
        &JsValue::from_str("agent"),
        &descriptor,
    )?;
    Ok(())
}

fn context_service(root: &JsValue, name: &str) -> Result<JsValue, JsValue> {
    let get = required_function(root, "get", "Client Context")?;
    let service = get.call1(root, &JsValue::from_str(name))?;
    if service.is_undefined() || service.is_null() {
        Err(js_sys::Error::new(&format!("Client runtime requires {name:?}")).into())
    } else {
        Ok(service)
    }
}

fn provide(root: &JsValue, name: &str, service: &JsValue) -> Result<(), JsValue> {
    let reflect = required(root, "reflect", "Client Context")?;
    required_function(&reflect, "provide", "Client Context reflect")?.call3(
        &reflect,
        &JsValue::from_str(name),
        service,
        &JsValue::UNDEFINED,
    )?;
    Ok(())
}

fn own_cleanup(root: &JsValue, cleanup: Function, label: &str) -> Result<(), JsValue> {
    let installer =
        Closure::wrap(Box::new(move || cleanup.clone()) as Box<dyn FnMut() -> Function>);
    required_function(root, "effect", "Client Context")?.call2(
        root,
        &installer.into_js_value(),
        &JsValue::from_str(label),
    )?;
    Ok(())
}

fn call_method(value: &JsValue, method: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let function = required_function(value, method, "Client runtime face")?;
    let arguments_array = Array::new();
    for argument in arguments {
        arguments_array.push(argument);
    }
    function.apply(value, &arguments_array)
}

fn required(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let member = Reflect::get(value, &JsValue::from_str(key))?;
    if member.is_undefined() || member.is_null() {
        Err(js_sys::Error::new(&format!("{owner} requires {key:?}")).into())
    } else {
        Ok(member)
    }
}

fn required_function(value: &JsValue, key: &str, owner: &str) -> Result<Function, JsValue> {
    required(value, key, owner)?.dyn_into::<Function>()
}

fn set(object: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    if Reflect::set(object, &JsValue::from_str(key), value)? {
        Ok(())
    } else {
        Err(js_sys::Error::new(&format!("failed to set Client runtime member {key:?}")).into())
    }
}
