//! Browser RPC, observable, and action faces for Agent preset stores.

use std::{cell::RefCell, rc::Rc};

use futures::{FutureExt as _, future::LocalBoxFuture};
use js_sys::{Function, Object, Reflect};
use seekdeep_client_runtime::{SnapshotStore, SnapshotStoreSubscription};
use seekdeep_identity::SessionId;
use serde::Serialize;
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::spawn_local;

use crate::{
    AgentPresetSeatController, AgentPresetSeatTransport, AgentPresetSectionController,
    AgentPresetSectionTransport, AgentPresetSettingsController, AgentPresetSettingsTransport,
    PresetOpenResult, PresetReadValue, RosterValue, SeatSessionSummary,
    browser::{call_async, from_js, object, optional, rejection_text, required, rpc_value, to_js},
};

#[derive(Clone)]
struct BrowserAgentPresetTransport {
    agent_presets: JsValue,
    settings: JsValue,
}

impl BrowserAgentPresetTransport {
    fn new(api: &JsValue) -> Result<Rc<Self>, JsValue> {
        Ok(Rc::new(Self {
            agent_presets: required(api, "agentPresets", "generated API")?,
            settings: required(api, "settings", "generated API")?,
        }))
    }
}

impl AgentPresetSettingsTransport for BrowserAgentPresetTransport {
    fn list(&self) -> LocalBoxFuture<'static, Result<RosterValue, String>> {
        list_roster(self.agent_presets.clone())
    }

    fn describe_settings(&self) -> LocalBoxFuture<'static, Result<bool, String>> {
        let settings = self.settings.clone();
        async move {
            let response = call_async(&settings, "describe", &[Object::new().into()])
                .await
                .map_err(|error| rejection_text(&error))?;
            let result = required(&response, "result", "RPC response")
                .map_err(|error| rejection_text(&error))?;
            if required(&result, "ok", "RPC result")
                .map_err(|error| rejection_text(&error))?
                .as_bool()
                != Some(true)
            {
                return Ok(false);
            }
            let value =
                required(&result, "value", "RPC result").map_err(|error| rejection_text(&error))?;
            Ok(required(&value, "writable", "settings.describe value")
                .map_err(|error| rejection_text(&error))?
                .as_bool()
                .unwrap_or(false))
        }
        .boxed_local()
    }

    fn update_default(&self, id: String) -> LocalBoxFuture<'static, Result<(), String>> {
        update_default(self.settings.clone(), id)
    }
}

impl AgentPresetSeatTransport for BrowserAgentPresetTransport {
    fn list(&self) -> LocalBoxFuture<'static, Result<RosterValue, String>> {
        list_roster(self.agent_presets.clone())
    }

    fn select_session(
        &self,
        session_id: SessionId,
        agent_preset: String,
    ) -> LocalBoxFuture<'static, Result<String, String>> {
        let service = self.agent_presets.clone();
        async move {
            let request = object(&[
                ("sessionId", JsValue::from_str(session_id.as_str())),
                ("agentPreset", JsValue::from_str(&agent_preset)),
            ])
            .map_err(|error| rejection_text(&error))?;
            let response = call_async(&service, "select", &[request.into()])
                .await
                .map_err(|error| rejection_text(&error))?;
            let value = rpc_value(&response)?;
            required(&value, "agentPreset", "agentPresets.select value")
                .map_err(|error| rejection_text(&error))?
                .as_string()
                .ok_or_else(|| "agentPresets.select omitted agentPreset".to_owned())
        }
        .boxed_local()
    }
}

impl AgentPresetSectionTransport for BrowserAgentPresetTransport {
    fn list(&self) -> LocalBoxFuture<'static, Result<RosterValue, String>> {
        list_roster(self.agent_presets.clone())
    }

    fn read(&self, id: String) -> LocalBoxFuture<'static, Result<PresetReadValue, String>> {
        let service = self.agent_presets.clone();
        async move {
            let request = object(&[("agentPreset", JsValue::from_str(&id))])
                .map_err(|error| rejection_text(&error))?;
            let response = call_async(&service, "read", &[request.into()])
                .await
                .map_err(|error| rejection_text(&error))?;
            let value = rpc_value(&response)?;
            Ok(PresetReadValue {
                name: optional(&value, "name")
                    .map_err(|error| rejection_text(&error))?
                    .and_then(|name| name.as_string()),
                content: required(&value, "content", "agentPresets.read value")
                    .map_err(|error| rejection_text(&error))?
                    .as_string()
                    .ok_or_else(|| "agentPresets.read omitted content".to_owned())?,
            })
        }
        .boxed_local()
    }

    fn copy(
        &self,
        from: String,
        id: String,
        name: Option<String>,
    ) -> LocalBoxFuture<'static, Result<(), String>> {
        let service = self.agent_presets.clone();
        async move {
            let request = Object::new();
            for (key, value) in [
                ("from", JsValue::from_str(&from)),
                ("agentPreset", JsValue::from_str(&id)),
            ] {
                Reflect::set(&request, &JsValue::from_str(key), &value)
                    .map_err(|error| rejection_text(&error))?;
            }
            if let Some(name) = name {
                Reflect::set(
                    &request,
                    &JsValue::from_str("name"),
                    &JsValue::from_str(&name),
                )
                .map_err(|error| rejection_text(&error))?;
            }
            let response = call_async(&service, "copy", &[request.into()])
                .await
                .map_err(|error| rejection_text(&error))?;
            rpc_value(&response).map(|_| ())
        }
        .boxed_local()
    }

    fn open_document(
        &self,
        id: String,
    ) -> LocalBoxFuture<'static, Result<PresetOpenResult, String>> {
        let service = self.agent_presets.clone();
        async move {
            let request = object(&[("agentPreset", JsValue::from_str(&id))])
                .map_err(|error| rejection_text(&error))?;
            let response = call_async(&service, "openDocument", &[request.into()])
                .await
                .map_err(|error| rejection_text(&error))?;
            let value = rpc_value(&response)?;
            if required(&value, "opened", "agentPresets.openDocument value")
                .map_err(|error| rejection_text(&error))?
                .as_bool()
                == Some(true)
            {
                Ok(PresetOpenResult::Opened)
            } else {
                required(&value, "path", "agentPresets.openDocument value")
                    .map_err(|error| rejection_text(&error))?
                    .as_string()
                    .map(PresetOpenResult::Path)
                    .ok_or_else(|| "agentPresets.openDocument omitted path".to_owned())
            }
        }
        .boxed_local()
    }

    fn remove(&self, id: String) -> LocalBoxFuture<'static, Result<(), String>> {
        let service = self.agent_presets.clone();
        async move {
            let request = object(&[("agentPreset", JsValue::from_str(&id))])
                .map_err(|error| rejection_text(&error))?;
            let response = call_async(&service, "remove", &[request.into()])
                .await
                .map_err(|error| rejection_text(&error))?;
            rpc_value(&response).map(|_| ())
        }
        .boxed_local()
    }

    fn update_default(&self, id: String) -> LocalBoxFuture<'static, Result<(), String>> {
        update_default(self.settings.clone(), id)
    }
}

fn list_roster(service: JsValue) -> LocalBoxFuture<'static, Result<RosterValue, String>> {
    async move {
        let response = call_async(&service, "list", &[Object::new().into()])
            .await
            .map_err(|error| rejection_text(&error))?;
        from_js(rpc_value(&response)?)
    }
    .boxed_local()
}

fn update_default(settings: JsValue, id: String) -> LocalBoxFuture<'static, Result<(), String>> {
    async move {
        let patch = object(&[("default", JsValue::from_str(&id))])
            .map_err(|error| rejection_text(&error))?;
        let request = object(&[
            ("ns", JsValue::from_str(crate::AGENT_PRESET_SETTINGS_NS)),
            ("patch", patch.into()),
        ])
        .map_err(|error| rejection_text(&error))?;
        let response = call_async(&settings, "update", &[request.into()])
            .await
            .map_err(|error| rejection_text(&error))?;
        rpc_value(&response).map(|_| ())
    }
    .boxed_local()
}

fn snapshot_face<T: Clone + Serialize + 'static>(
    store: Rc<SnapshotStore<T>>,
) -> Result<JsValue, JsValue> {
    let output = Object::new();
    let cache = Rc::new(RefCell::new(None::<(Rc<T>, JsValue)>));
    let getter_store = store.clone();
    let getter_cache = cache;
    let getter = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let snapshot = getter_store.snapshot();
        if let Some((cached, value)) = getter_cache.borrow().as_ref()
            && Rc::ptr_eq(cached, &snapshot)
        {
            return Ok(value.clone());
        }
        let value = to_js(snapshot.as_ref())?;
        *getter_cache.borrow_mut() = Some((snapshot, value.clone()));
        Ok(value)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    Reflect::set(
        &output,
        &JsValue::from_str("getSnapshot"),
        &getter.into_js_value(),
    )?;
    let subscriber_store = store;
    let subscribe = Closure::wrap(Box::new(move |listener: Function| -> Function {
        let callback = listener.clone();
        let subscription = subscriber_store.subscribe(Rc::new(move || {
            let _ = callback.call0(&JsValue::UNDEFINED);
        }));
        subscription_disposer(subscription)
    }) as Box<dyn FnMut(Function) -> Function>);
    Reflect::set(
        &output,
        &JsValue::from_str("subscribe"),
        &subscribe.into_js_value(),
    )?;
    Ok(output.into())
}

fn subscription_disposer<T: 'static>(subscription: SnapshotStoreSubscription<T>) -> Function {
    Closure::wrap(Box::new(move || subscription.dispose()) as Box<dyn FnMut()>)
        .into_js_value()
        .unchecked_into()
}

fn set_method(output: &Object, name: &str, function: JsValue) -> Result<(), JsValue> {
    let result = Reflect::set(output, &JsValue::from_str(name), &function).map(|_| ());
    drop(function);
    result
}

/// Creates the compiled default-settings controller face.
///
/// # Errors
///
/// Returns malformed generated API or JavaScript face failures.
#[wasm_bindgen(js_name = createAgentPresetSettingsController)]
#[allow(clippy::needless_pass_by_value)]
pub fn create_agent_preset_settings_controller(api: JsValue) -> Result<JsValue, JsValue> {
    let transport = BrowserAgentPresetTransport::new(&api)?;
    let controller = AgentPresetSettingsController::new(transport);
    let output = Object::new();
    let hooks = object(&[("agentPresetSettings", snapshot_face(controller.store())?)])?;
    Reflect::set(&output, &JsValue::from_str("hooks"), &hooks)?;
    let load_controller = controller.clone();
    let load = Closure::wrap(Box::new(move || {
        let controller = load_controller.clone();
        spawn_local(async move { controller.load().await });
    }) as Box<dyn FnMut()>);
    set_method(&output, "load", load.into_js_value())?;
    let select_controller = controller;
    let select = Closure::wrap(Box::new(move |id: String| {
        let controller = select_controller.clone();
        spawn_local(async move { controller.select(&id).await });
    }) as Box<dyn FnMut(String)>);
    set_method(&output, "select", select.into_js_value())?;
    Ok(output.into())
}

/// Creates the compiled staged new-session seat face.
///
/// # Errors
///
/// Returns malformed API, Session reader, or callback faces.
#[wasm_bindgen(js_name = createAgentPresetSeatController)]
#[allow(clippy::needless_pass_by_value)]
pub fn create_agent_preset_seat_controller(
    api: JsValue,
    current_session: Function,
    on_applied: Option<Function>,
) -> Result<JsValue, JsValue> {
    let transport = BrowserAgentPresetTransport::new(&api)?;
    let session_reader = Rc::new(move || {
        let value = current_session.call0(&JsValue::UNDEFINED).ok()?;
        if value.is_null() || value.is_undefined() {
            return None;
        }
        Some(SeatSessionSummary {
            id: SessionId::new(
                required(&value, "id", "Session summary")
                    .ok()?
                    .as_string()?,
            ),
            blank: required(&value, "blank", "Session summary")
                .ok()?
                .as_bool()
                .unwrap_or(false),
            agent_preset: optional(&value, "agentPreset")
                .ok()?
                .and_then(|preset| preset.as_string()),
        })
    });
    let applied = on_applied.map(|callback| {
        Rc::new(move |session: SessionId, preset: String| {
            let _ = callback.call2(
                &JsValue::UNDEFINED,
                &JsValue::from_str(session.as_str()),
                &JsValue::from_str(&preset),
            );
        }) as Rc<dyn Fn(SessionId, String)>
    });
    let controller = AgentPresetSeatController::new(transport, session_reader, applied);
    seat_face(controller)
}

fn seat_face(controller: Rc<AgentPresetSeatController>) -> Result<JsValue, JsValue> {
    let output = Object::new();
    let hooks = object(&[("agentPresetSeat", snapshot_face(controller.store())?)])?;
    Reflect::set(&output, &JsValue::from_str("hooks"), &hooks)?;
    let load_controller = controller.clone();
    let load = Closure::wrap(Box::new(move || {
        let controller = load_controller.clone();
        spawn_local(async move { controller.load().await });
    }) as Box<dyn FnMut()>);
    set_method(&output, "load", load.into_js_value())?;
    let select_controller = controller.clone();
    let select = Closure::wrap(Box::new(move |id: String| {
        let controller = select_controller.clone();
        spawn_local(async move { controller.select(&id).await });
    }) as Box<dyn FnMut(String)>);
    set_method(&output, "select", select.into_js_value())?;
    let stage_controller = controller.clone();
    let stage = Closure::wrap(Box::new(move |id: String, introduce: bool| {
        stage_controller.stage(&id, introduce);
    }) as Box<dyn FnMut(String, bool)>);
    set_method(&output, "stage", stage.into_js_value())?;
    let introduced_controller = controller.clone();
    let introduced =
        Closure::wrap(Box::new(move || introduced_controller.introduced()) as Box<dyn FnMut()>);
    set_method(&output, "introduced", introduced.into_js_value())?;
    let apply_controller = controller;
    let apply = Closure::wrap(Box::new(move || {
        let controller = apply_controller.clone();
        spawn_local(async move { controller.apply().await });
    }) as Box<dyn FnMut()>);
    set_method(&output, "apply", apply.into_js_value())?;
    Ok(output.into())
}

/// Creates the compiled management-section controller face.
///
/// # Errors
///
/// Returns malformed generated API or callback faces.
#[wasm_bindgen(js_name = createAgentPresetSectionController)]
#[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
pub fn create_agent_preset_section_controller(
    api: JsValue,
    roster_changed: Option<Function>,
) -> Result<JsValue, JsValue> {
    let transport = BrowserAgentPresetTransport::new(&api)?;
    let changed = roster_changed.map(|callback| {
        Rc::new(move || {
            let _ = callback.call0(&JsValue::UNDEFINED);
        }) as Rc<dyn Fn()>
    });
    let controller = AgentPresetSectionController::new(transport, changed);
    let output = Object::new();
    let hooks = object(&[("agentPresetSection", snapshot_face(controller.store())?)])?;
    Reflect::set(&output, &JsValue::from_str("hooks"), &hooks)?;

    let load_controller = controller.clone();
    let load = Closure::wrap(Box::new(move || {
        let controller = load_controller.clone();
        spawn_local(async move { controller.load().await });
    }) as Box<dyn FnMut()>);
    set_method(&output, "load", load.into_js_value())?;
    let view_controller = controller.clone();
    let view = Closure::wrap(Box::new(move |id: String| {
        let controller = view_controller.clone();
        spawn_local(async move { controller.view(&id).await });
    }) as Box<dyn FnMut(String)>);
    set_method(&output, "view", view.into_js_value())?;
    let close_controller = controller.clone();
    let close = Closure::wrap(Box::new(move || close_controller.close_view()) as Box<dyn FnMut()>);
    set_method(&output, "closeView", close.into_js_value())?;
    let begin_controller = controller.clone();
    let begin = Closure::wrap(
        Box::new(move |id: String| begin_controller.begin_copy(&id)) as Box<dyn FnMut(String)>
    );
    set_method(&output, "beginCopy", begin.into_js_value())?;
    let cancel_controller = controller.clone();
    let cancel =
        Closure::wrap(Box::new(move || cancel_controller.cancel_copy()) as Box<dyn FnMut()>);
    set_method(&output, "cancelCopy", cancel.into_js_value())?;
    let id_controller = controller.clone();
    let set_id = Closure::wrap(
        Box::new(move |id: String| id_controller.set_copy_id(&id)) as Box<dyn FnMut(String)>
    );
    set_method(&output, "setCopyId", set_id.into_js_value())?;
    let name_controller = controller.clone();
    let set_name = Closure::wrap(
        Box::new(move |name: String| name_controller.set_copy_name(&name))
            as Box<dyn FnMut(String)>,
    );
    set_method(&output, "setCopyName", set_name.into_js_value())?;
    let copy_controller = controller.clone();
    let confirm_copy = Closure::wrap(Box::new(move || {
        let controller = copy_controller.clone();
        spawn_local(async move { controller.confirm_copy().await });
    }) as Box<dyn FnMut()>);
    set_method(&output, "confirmCopy", confirm_copy.into_js_value())?;
    let location_controller = controller.clone();
    let open_location = Closure::wrap(Box::new(move |id: String| {
        let controller = location_controller.clone();
        spawn_local(async move { controller.open_location(&id).await });
    }) as Box<dyn FnMut(String)>);
    set_method(&output, "openLocation", open_location.into_js_value())?;
    let delete_controller = controller.clone();
    let confirm_delete = Closure::wrap(Box::new(move |id: JsValue| {
        delete_controller.confirm_delete(id.as_string().as_deref());
    }) as Box<dyn FnMut(JsValue)>);
    set_method(&output, "confirmDelete", confirm_delete.into_js_value())?;
    let remove_controller = controller.clone();
    let remove = Closure::wrap(Box::new(move || {
        let controller = remove_controller.clone();
        spawn_local(async move { controller.remove().await });
    }) as Box<dyn FnMut()>);
    set_method(&output, "remove", remove.into_js_value())?;
    let default_controller = controller;
    let make_default = Closure::wrap(Box::new(move |id: String| {
        let controller = default_controller.clone();
        spawn_local(async move { controller.make_default(&id).await });
    }) as Box<dyn FnMut(String)>);
    set_method(&output, "makeDefault", make_default.into_js_value())?;
    Ok(output.into())
}
