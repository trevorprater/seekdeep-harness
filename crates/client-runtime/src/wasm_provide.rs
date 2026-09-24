//! JavaScript-compatible facade for the portable Session provide channel.

use std::{cell::RefCell, rc::Rc};

use indexmap::IndexMap;
use js_sys::{Array, Function, Object, Reflect};
use seekdeep_identity::SessionId;
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{
    SessionBinding, SessionProvideChannel, SessionProvideChannelHost, SessionProvideContribution,
    SessionProvideDescriptor, SessionProvideError, SessionProvideInfo,
};

type JsInfo = SessionProvideInfo<JsValue, JsValue, JsValue>;
type JsChannel = SessionProvideChannel<JsValue, JsValue, JsValue, JsValue>;

#[derive(Default)]
struct JsInfoCache {
    entries: RefCell<Vec<(JsValue, Rc<JsInfo>)>>,
}

impl JsInfoCache {
    fn resolve(&self, value: JsValue) -> Result<Rc<JsInfo>, JsValue> {
        if let Some((_, info)) = self
            .entries
            .borrow()
            .iter()
            .find(|(candidate, _)| Object::is(candidate, &value))
        {
            return Ok(info.clone());
        }
        let info = Rc::new(parse_info(&value)?);
        self.entries.borrow_mut().push((value, info.clone()));
        Ok(info)
    }

    fn materialize(&self, info: Rc<JsInfo>) -> Result<JsValue, JsValue> {
        if let Some((value, _)) = self
            .entries
            .borrow()
            .iter()
            .find(|(_, candidate)| Rc::ptr_eq(candidate, &info))
        {
            return Ok(value.clone());
        }
        let value = info_to_js(&info)?;
        self.entries.borrow_mut().push((value.clone(), info));
        Ok(value)
    }
}

struct JsProvideHost {
    host: JsValue,
    cache: Rc<JsInfoCache>,
}

impl SessionProvideChannelHost<JsValue, JsValue, JsValue, JsValue> for JsProvideHost {
    fn rebuild_bundles(&self, _channel: &JsChannel) -> Result<(), SessionProvideError> {
        call_method(&self.host, "rebuildBundles", &[])
            .map(|_| ())
            .map_err(provide_error)
    }

    fn resolve_current(&self) -> Result<Rc<JsInfo>, SessionProvideError> {
        let value = call_method(&self.host, "resolveCurrent", &[]).map_err(provide_error)?;
        self.cache.resolve(value).map_err(provide_error)
    }

    fn report_subscriber_failure(&self, message: &str) {
        console_error(message);
    }
}

/// Browser `SessionProvideChannel` backed by the portable Rust roster ledger.
#[wasm_bindgen(js_name = SessionProvideChannel)]
pub struct WasmSessionProvideChannel {
    channel: Rc<JsChannel>,
    cache: Rc<JsInfoCache>,
    current_face: JsValue,
}

#[wasm_bindgen(js_class = SessionProvideChannel)]
impl WasmSessionProvideChannel {
    /// Creates a provider channel over an owner-side host object.
    ///
    /// # Errors
    ///
    /// Returns when the current observable face cannot be constructed.
    #[wasm_bindgen(constructor)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn new(host: JsValue) -> Result<Self, JsValue> {
        let cache = Rc::new(JsInfoCache::default());
        let channel = SessionProvideChannel::new(Rc::new(JsProvideHost {
            host,
            cache: cache.clone(),
        }));
        let current_face = current_face(channel.clone(), cache.clone())?;
        Ok(Self {
            channel,
            cache,
            current_face,
        })
    }

    /// Atomic current-Session provide observable.
    #[wasm_bindgen(getter, js_name = currentProvideInfo)]
    pub fn current_provide_info(&self) -> JsValue {
        self.current_face.clone()
    }

    /// Static no-Session projection under the current roster.
    #[wasm_bindgen(getter, js_name = maybeInfo)]
    pub fn maybe_info(&self) -> JsValue {
        self.cache
            .materialize(self.channel.maybe_info())
            .unwrap_or_else(|error| wasm_bindgen::throw_val(error))
    }

    /// Registers one provider and returns its repeatable disposer.
    ///
    /// # Errors
    ///
    /// Returns malformed descriptors or fail-loud roster materialization errors.
    #[allow(clippy::needless_pass_by_value)]
    pub fn provide(&self, descriptor: JsValue) -> Result<Function, JsValue> {
        let descriptor = parse_descriptor(&descriptor)?;
        let registration = self
            .channel
            .provide(descriptor)
            .map_err(js_error_from_provide)?;
        Ok(Closure::wrap(Box::new(move || {
            if let Err(error) = registration.dispose() {
                wasm_bindgen::throw_val(js_error_from_provide(error));
            }
        }) as Box<dyn FnMut()>)
        .into_js_value()
        .unchecked_into())
    }

    /// Publishes the current bundle when its identity changed.
    ///
    /// # Errors
    ///
    /// Returns owner-side current-resolution failures.
    #[wasm_bindgen(js_name = publishCurrent)]
    pub fn publish_current(&self) -> Result<(), JsValue> {
        self.channel
            .publish_current()
            .map_err(js_error_from_provide)
    }

    /// Materializes one definite Session bundle.
    ///
    /// # Errors
    ///
    /// Returns malformed bindings or provider materialization failures.
    #[wasm_bindgen(js_name = materializeInfo)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn materialize_info(&self, binding: JsValue) -> Result<JsValue, JsValue> {
        let session_id = required_string(&binding, "sessionId", "Session binding")?;
        let session = required(&binding, "session", "Session binding")?;
        let projections = required(&session, "projections", "Session face")?;
        let info = self
            .channel
            .materialize_info(&SessionBinding {
                session_id: SessionId::new(session_id),
                session,
                projections,
                payload: binding,
            })
            .map_err(js_error_from_provide)?;
        self.cache.materialize(info)
    }
}

fn current_face(channel: Rc<JsChannel>, cache: Rc<JsInfoCache>) -> Result<JsValue, JsValue> {
    let face = Object::new();
    let snapshot_channel = channel.clone();
    let snapshot_cache = cache;
    let snapshot = Closure::wrap(Box::new(move || {
        snapshot_cache
            .materialize(snapshot_channel.current_snapshot())
            .unwrap_or_else(|error| wasm_bindgen::throw_val(error))
    }) as Box<dyn FnMut() -> JsValue>);
    set(&face, "getSnapshot", &snapshot.into_js_value())?;
    let subscribe = Closure::wrap(Box::new(move |listener: Function| {
        let subscription = channel.subscribe_current(Rc::new(move || {
            listener
                .call0(&JsValue::UNDEFINED)
                .map(|_| ())
                .map_err(|error| render_js(&error))
        }));
        Closure::wrap(Box::new(move || subscription.dispose()) as Box<dyn FnMut()>)
            .into_js_value()
            .unchecked_into::<Function>()
    }) as Box<dyn FnMut(Function) -> Function>);
    set(&face, "subscribe", &subscribe.into_js_value())?;
    Ok(face.into())
}

fn parse_descriptor(
    descriptor: &JsValue,
) -> Result<SessionProvideDescriptor<JsValue, JsValue, JsValue, JsValue>, JsValue> {
    let hooks = optional_string_array(descriptor, "hooks", "sessions.provide descriptor")?;
    let props = optional_string_array(descriptor, "props", "sessions.provide descriptor")?;
    let resolve =
        required(descriptor, "resolve", "sessions.provide descriptor")?.dyn_into::<Function>()?;
    Ok(SessionProvideDescriptor {
        hooks,
        props,
        resolve: Rc::new(move |binding| {
            let contribution = resolve
                .call1(&JsValue::UNDEFINED, &binding.payload)
                .map_err(provide_error)?;
            parse_contribution(&contribution).map_err(provide_error)
        }),
    })
}

fn parse_contribution(
    contribution: &JsValue,
) -> Result<SessionProvideContribution<JsValue, JsValue>, JsValue> {
    if !contribution.is_object() || contribution.is_null() {
        return Err(js_sys::Error::new("sessions.provide resolver must return an object").into());
    }
    Ok(SessionProvideContribution {
        hooks: optional_members(contribution, "hooks")?,
        props: optional_members(contribution, "props")?,
    })
}

fn parse_info(value: &JsValue) -> Result<JsInfo, JsValue> {
    if !value.is_object() || value.is_null() {
        return Err(js_sys::Error::new("sessions current provide info must be an object").into());
    }
    let session_id = Reflect::get(value, &JsValue::from_str("sessionId"))?;
    let session_id = if session_id.is_undefined() {
        None
    } else {
        Some(SessionId::new(session_id.as_string().ok_or_else(|| {
            js_sys::Error::new("sessions current provide info sessionId must be a string")
        })?))
    };
    let hooks = required(value, "hooks", "sessions current provide info")?;
    let props = required(value, "props", "sessions current provide info")?;
    let projections = Reflect::get(value, &JsValue::from_str("projections"))?;
    Ok(SessionProvideInfo {
        session_id,
        hooks: optionalized_members(&hooks)?,
        props: optionalized_members(&props)?,
        projections: (!projections.is_undefined()).then_some(projections),
    })
}

fn info_to_js(info: &JsInfo) -> Result<JsValue, JsValue> {
    let value = Object::new();
    set(
        &value,
        "sessionId",
        &info
            .session_id
            .as_ref()
            .map_or(JsValue::UNDEFINED, |id| JsValue::from_str(id.as_str())),
    )?;
    let hooks = Object::new();
    for (name, source) in &info.hooks {
        set(&hooks, name, source.as_ref().unwrap_or(&JsValue::UNDEFINED))?;
    }
    set(&value, "hooks", &hooks)?;
    let props = Object::new();
    for (name, prop) in &info.props {
        set(&props, name, prop.as_ref().unwrap_or(&JsValue::UNDEFINED))?;
    }
    set(&value, "props", &props)?;
    if let Some(projections) = &info.projections {
        set(&value, "projections", projections)?;
    }
    Ok(value.into())
}

fn optional_members(
    value: &JsValue,
    key: &str,
) -> Result<IndexMap<String, Option<JsValue>>, JsValue> {
    let members = Reflect::get(value, &JsValue::from_str(key))?;
    if members.is_undefined() {
        return Ok(IndexMap::new());
    }
    optionalized_members(&members)
}

fn optionalized_members(members: &JsValue) -> Result<IndexMap<String, Option<JsValue>>, JsValue> {
    if !members.is_object() || members.is_null() {
        return Err(js_sys::Error::new("sessions.provide members must be an object").into());
    }
    let members = Object::from(members.clone());
    let mut result = IndexMap::new();
    for key in Object::keys(&members).iter() {
        let key = key
            .as_string()
            .ok_or_else(|| js_sys::Error::new("sessions.provide member name must be a string"))?;
        let value = Reflect::get(&members, &JsValue::from_str(&key))?;
        result.insert(key, (!value.is_undefined()).then_some(value));
    }
    Ok(result)
}

fn optional_string_array(value: &JsValue, key: &str, owner: &str) -> Result<Vec<String>, JsValue> {
    let member = Reflect::get(value, &JsValue::from_str(key))?;
    if member.is_undefined() {
        return Ok(Vec::new());
    }
    if !Array::is_array(&member) {
        return Err(js_sys::Error::new(&format!("{owner} {key} must be an array")).into());
    }
    Array::from(&member)
        .iter()
        .map(|value| {
            value.as_string().ok_or_else(|| {
                js_sys::Error::new(&format!("{owner} {key} names must be strings")).into()
            })
        })
        .collect()
}

fn required(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let member = Reflect::get(value, &JsValue::from_str(key))?;
    if member.is_undefined() || member.is_null() {
        Err(js_sys::Error::new(&format!("{owner} requires {key:?}")).into())
    } else {
        Ok(member)
    }
}

fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    required(value, key, owner)?
        .as_string()
        .ok_or_else(|| js_sys::Error::new(&format!("{owner} {key} must be a string")).into())
}

fn call_method(value: &JsValue, method: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let function = required(value, method, "SessionProvideChannel host")?.dyn_into::<Function>()?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    function.apply(value, &args)
}

fn set(object: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    if Reflect::set(object, &JsValue::from_str(key), value)? {
        Ok(())
    } else {
        Err(js_sys::Error::new(&format!("failed to set Session provide member {key:?}")).into())
    }
}

#[allow(clippy::needless_pass_by_value)]
fn provide_error(error: JsValue) -> SessionProvideError {
    SessionProvideError::new(render_js(&error))
}

#[allow(clippy::needless_pass_by_value)]
fn js_error_from_provide(error: SessionProvideError) -> JsValue {
    js_sys::Error::new(&error.to_string()).into()
}

fn render_js(value: &JsValue) -> String {
    value
        .as_string()
        .or_else(|| {
            Reflect::get(value, &JsValue::from_str("message"))
                .ok()
                .and_then(|message| message.as_string())
        })
        .or_else(|| {
            js_sys::JSON::stringify(value)
                .ok()
                .and_then(|value| value.as_string())
        })
        .unwrap_or_else(|| format!("{value:?}"))
}

fn console_error(message: &str) {
    let global = js_sys::global();
    let Some((console, error)) = Reflect::get(&global, &JsValue::from_str("console"))
        .ok()
        .and_then(|console| {
            Reflect::get(&console, &JsValue::from_str("error"))
                .ok()
                .and_then(|error| error.dyn_into::<Function>().ok())
                .map(|error| (console, error))
        })
    else {
        return;
    };
    let _ = error.call2(
        &console,
        &JsValue::from_str("sessions.currentProvideInfo subscriber failed:"),
        &JsValue::from_str(message),
    );
}
