//! Browser fixture Session records and Sessions service assembly.

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_client_runtime::{
    SESSION_SEARCH_RESULT_LIMIT, WasmSessionProvideChannel, create_client_scope, scope_of,
};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, future_to_promise};

use crate::conversation_snapshot_js;

thread_local! {
    static FIXTURE_SESSION_PROTOTYPE: RefCell<Option<JsValue>> = const { RefCell::new(None) };
}

struct ProjectionState {
    values: RefCell<HashMap<String, JsValue>>,
    listeners: RefCell<HashMap<String, Vec<Function>>>,
    faces: RefCell<HashMap<String, JsValue>>,
}

struct FixtureSessionState {
    snapshot: RefCell<JsValue>,
    listeners: RefCell<Vec<Function>>,
}

struct BrowserSessionRecord {
    summary: JsValue,
    fixture: Rc<FixtureSessionState>,
    session: JsValue,
    scope: Option<JsValue>,
    scope_fiber: Option<JsValue>,
    provide_info: Option<JsValue>,
}

struct BrowserSessionsState {
    root: JsValue,
    stabilize: Function,
    produce: Function,
    list_snapshot: RefCell<JsValue>,
    list_listeners: RefCell<Vec<Function>>,
    records: RefCell<HashMap<String, Rc<RefCell<BrowserSessionRecord>>>>,
    record_order: RefCell<Vec<String>>,
    calls: Array,
    search_stub: RefCell<Option<Function>>,
    channel: RefCell<Option<Rc<WasmSessionProvideChannel>>>,
}

/// Configures the compatibility prototype assigned to every browser fixture Session.
#[wasm_bindgen(js_name = configureFixtureSessionPrototype)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_fixture_session_prototype(prototype: JsValue) {
    FIXTURE_SESSION_PROTOTYPE.with(|configured| *configured.borrow_mut() = Some(prototype));
}

/// Constructs and publishes the fixture-backed browser Sessions service as `ctx.sessions`.
///
/// # Errors
///
/// Returns malformed Context, stabilization, draft, provide-channel, fixture, or action failures.
#[wasm_bindgen(js_name = installTestSessions)]
#[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
pub fn install_test_sessions(
    context: JsValue,
    stabilizer: JsValue,
    produce: JsValue,
) -> Result<JsValue, JsValue> {
    let face = sessions_face(&context, stabilizer, produce)?;
    call_method(
        &context,
        "provide",
        &[JsValue::from_str("sessions"), face.clone()],
    )?;
    Ok(face)
}

/// Constructs the public fixture-backed Sessions double without publishing it.
///
/// # Errors
///
/// Returns malformed Context, stabilization, draft, provide-channel, fixture, or action failures.
#[wasm_bindgen(js_name = createTestSessions)]
#[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
pub fn create_test_sessions(
    stabilizer: JsValue,
    context: JsValue,
    produce: JsValue,
) -> Result<JsValue, JsValue> {
    sessions_face(&context, stabilizer, produce)
}

#[allow(clippy::too_many_lines)]
fn sessions_face(
    context: &JsValue,
    stabilizer: JsValue,
    produce: JsValue,
) -> Result<JsValue, JsValue> {
    let stabilize = stabilizer
        .dyn_into::<Function>()
        .map_err(|_| js_sys::TypeError::new("TestSessions stabilize must be a function"))?;
    let produce = produce
        .dyn_into::<Function>()
        .map_err(|_| js_sys::TypeError::new("TestSessions produce must be a function"))?;
    let state = Rc::new(BrowserSessionsState {
        root: context.clone(),
        stabilize,
        produce,
        list_snapshot: RefCell::new(initial_list_snapshot()?.into()),
        list_listeners: RefCell::new(Vec::new()),
        records: RefCell::new(HashMap::new()),
        record_order: RefCell::new(Vec::new()),
        calls: Array::new(),
        search_stub: RefCell::new(None),
        channel: RefCell::new(None),
    });
    let host = provide_host(&state)?;
    let channel = Rc::new(WasmSessionProvideChannel::new(host)?);
    *state.channel.borrow_mut() = Some(channel.clone());

    let face = Object::new();
    set(&face, "list", &list_face(&state)?)?;
    set(&face, "currentProvideInfo", &channel.current_provide_info())?;
    set(&face, "calls", state.calls.as_ref())?;
    set(
        &face,
        "searchResultLimit",
        &JsValue::from_f64(
            u32::try_from(SESSION_SEARCH_RESULT_LIMIT)
                .map(f64::from)
                .unwrap_or(f64::from(u32::MAX)),
        ),
    )?;
    install_session_methods(&face, &state)?;
    Ok(face.into())
}

/// Creates one browser fixture Session over a source-shaped snapshot and behavior overrides.
///
/// # Errors
///
/// Returns malformed snapshot, override, projection-face, or object-construction failures.
#[wasm_bindgen(js_name = createFixtureSession)]
#[allow(clippy::needless_pass_by_value)]
pub fn create_fixture_session(
    session_id: String,
    snapshot: JsValue,
    overrides: JsValue,
) -> Result<JsValue, JsValue> {
    let (session, _) = fixture_session(&session_id, snapshot, overrides)?;
    Ok(session)
}

/// Creates a public fixture Session over an externally owned snapshot Store.
///
/// # Errors
///
/// Returns malformed Store, snapshot, override, projection-face, or subscription failures.
#[wasm_bindgen(js_name = createFixtureSessionFromStore)]
#[allow(clippy::needless_pass_by_value)]
pub fn create_fixture_session_from_store(
    session_id: String,
    store: JsValue,
    overrides: JsValue,
) -> Result<JsValue, JsValue> {
    if !store.is_object() || store.is_null() {
        return Err(js_sys::TypeError::new("FixtureSession store must be an object").into());
    }
    let snapshot = call_method(&store, "getSnapshot", &[])?;
    let override_object = Object::from(overrides.clone());
    let overrides_snapshot = Reflect::has(&override_object, &JsValue::from_str("getSnapshot"))?;
    let overrides_subscribe = Reflect::has(&override_object, &JsValue::from_str("subscribe"))?;
    let (session, _) = fixture_session(&session_id, snapshot, overrides)?;
    let face = Object::from(session.clone());
    if !overrides_snapshot {
        let snapshot_store = store.clone();
        let get_snapshot = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
            call_method(&snapshot_store, "getSnapshot", &[])
        })
            as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
        set(&face, "getSnapshot", &get_snapshot.into_js_value())?;
    }
    if !overrides_subscribe {
        let subscribe_store = store;
        let subscribe = Closure::wrap(Box::new(
            move |listener: Function| -> Result<Function, JsValue> {
                call_method(&subscribe_store, "subscribe", &[listener.into()])?
                    .dyn_into::<Function>()
            },
        )
            as Box<dyn FnMut(Function) -> Result<Function, JsValue>>);
        set(&face, "subscribe", &subscribe.into_js_value())?;
    }
    Ok(session)
}

fn fixture_session(
    session_id: &str,
    snapshot: JsValue,
    overrides: JsValue,
) -> Result<(JsValue, Rc<FixtureSessionState>), JsValue> {
    if !snapshot.is_object() || snapshot.is_null() {
        return Err(js_sys::TypeError::new("FixtureSession snapshot must be an object").into());
    }
    if !overrides.is_object() || overrides.is_null() {
        return Err(js_sys::TypeError::new("FixtureSession overrides must be an object").into());
    }
    let state = Rc::new(FixtureSessionState {
        snapshot: RefCell::new(snapshot),
        listeners: RefCell::new(Vec::new()),
    });
    let projections = projection_face()?;
    let face = Object::new();
    set(&face, "sessionId", &JsValue::from_str(session_id))?;
    set(&face, "projections", &projections)?;

    let snapshot_state = state.clone();
    let get_snapshot =
        Closure::wrap(Box::new(move || snapshot_state.snapshot.borrow().clone())
            as Box<dyn FnMut() -> JsValue>);
    set(&face, "getSnapshot", &get_snapshot.into_js_value())?;
    let subscribe_state = state.clone();
    let subscribe = Closure::wrap(Box::new(move |listener: Function| -> Function {
        let mut listeners = subscribe_state.listeners.borrow_mut();
        if !listeners
            .iter()
            .any(|registered| Object::is(registered, &listener))
        {
            listeners.push(listener.clone());
        }
        drop(listeners);
        let cleanup_state = subscribe_state.clone();
        Closure::wrap(Box::new(move || {
            cleanup_state
                .listeners
                .borrow_mut()
                .retain(|registered| !Object::is(registered, &listener));
        }) as Box<dyn FnMut()>)
        .into_js_value()
        .unchecked_into()
    }) as Box<dyn FnMut(Function) -> Function>);
    set(&face, "subscribe", &subscribe.into_js_value())?;

    for method in [
        "prompt",
        "readAttachment",
        "updateQueue",
        "cancel",
        "command",
        "loadOlder",
        "rename",
    ] {
        let id = session_id.to_owned();
        let name = method;
        let missing = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            Err(js_sys::Error::new(&format!(
                "test session {id:?}: {name} is not stubbed — supply it on the fixture's session face"
            ))
            .into())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>);
        set(&face, method, &missing.into_js_value())?;
    }
    Object::assign(&face, &Object::from(overrides));
    FIXTURE_SESSION_PROTOTYPE.with(|configured| {
        if let Some(prototype) = configured.borrow().as_ref() {
            let constructor = Reflect::get(&js_sys::global(), &JsValue::from_str("Object"))?
                .dyn_into::<Function>()?;
            let set_prototype =
                Reflect::get(constructor.as_ref(), &JsValue::from_str("setPrototypeOf"))?
                    .dyn_into::<Function>()?;
            set_prototype.call2(&constructor, face.as_ref(), prototype)?;
        }
        Ok::<(), JsValue>(())
    })?;
    Ok((face.into(), state))
}

fn provide_host(state: &Rc<BrowserSessionsState>) -> Result<JsValue, JsValue> {
    let host = Object::new();
    let rebuild_state = Rc::downgrade(state);
    let rebuild = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let state = rebuild_state
            .upgrade()
            .ok_or_else(|| js_sys::Error::new("TestSessions is disposed"))?;
        for record in ordered_records(&state) {
            if record.borrow().provide_info.is_some() {
                let id = session_id_of(&record.borrow().session)?;
                let info = materialize_info(&state, &id, &record)?;
                record.borrow_mut().provide_info = Some(info);
            }
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    set(&host, "rebuildBundles", &rebuild.into_js_value())?;
    let current_state = Rc::downgrade(state);
    let resolve_current = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let state = current_state
            .upgrade()
            .ok_or_else(|| js_sys::Error::new("TestSessions is disposed"))?;
        let current = Reflect::get(&state.list_snapshot.borrow(), &JsValue::from_str("current"))?;
        maybe_provide_info(&state, current.as_string().as_deref())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    set(&host, "resolveCurrent", &resolve_current.into_js_value())?;
    Ok(host.into())
}

fn list_face(state: &Rc<BrowserSessionsState>) -> Result<JsValue, JsValue> {
    let face = Object::new();
    let snapshot_state = state.clone();
    let get_snapshot = Closure::wrap(
        Box::new(move || snapshot_state.list_snapshot.borrow().clone())
            as Box<dyn FnMut() -> JsValue>,
    );
    set(&face, "getSnapshot", &get_snapshot.into_js_value())?;
    let subscribe_state = state.clone();
    let subscribe = Closure::wrap(Box::new(move |listener: Function| -> Function {
        let mut listeners = subscribe_state.list_listeners.borrow_mut();
        if !listeners
            .iter()
            .any(|registered| Object::is(registered, &listener))
        {
            listeners.push(listener.clone());
        }
        drop(listeners);
        let cleanup_state = subscribe_state.clone();
        Closure::wrap(Box::new(move || {
            cleanup_state
                .list_listeners
                .borrow_mut()
                .retain(|registered| !Object::is(registered, &listener));
        }) as Box<dyn FnMut()>)
        .into_js_value()
        .unchecked_into()
    }) as Box<dyn FnMut(Function) -> Function>);
    set(&face, "subscribe", &subscribe.into_js_value())?;
    Ok(face.into())
}

fn channel(state: &BrowserSessionsState) -> Result<Rc<WasmSessionProvideChannel>, JsValue> {
    state
        .channel
        .borrow()
        .clone()
        .ok_or_else(|| js_sys::Error::new("TestSessions provide channel is unavailable").into())
}

fn provide_info(state: &Rc<BrowserSessionsState>, id: &str) -> Result<JsValue, JsValue> {
    let Some(record) = state.records.borrow().get(id).cloned() else {
        return Ok(JsValue::UNDEFINED);
    };
    if let Some(info) = record.borrow().provide_info.clone() {
        return Ok(info);
    }
    let info = materialize_info(state, id, &record)?;
    record.borrow_mut().provide_info = Some(info.clone());
    Ok(info)
}

fn maybe_provide_info(
    state: &Rc<BrowserSessionsState>,
    id: Option<&str>,
) -> Result<JsValue, JsValue> {
    if let Some(id) = id {
        let info = provide_info(state, id)?;
        if !info.is_undefined() {
            return Ok(info);
        }
    }
    Ok(channel(state)?.maybe_info())
}

fn ordered_records(state: &BrowserSessionsState) -> Vec<Rc<RefCell<BrowserSessionRecord>>> {
    let records = state.records.borrow();
    state
        .record_order
        .borrow()
        .iter()
        .filter_map(|id| records.get(id).cloned())
        .collect()
}

fn materialize_info(
    state: &Rc<BrowserSessionsState>,
    id: &str,
    record: &Rc<RefCell<BrowserSessionRecord>>,
) -> Result<JsValue, JsValue> {
    let binding = binding_of(state, id, record)?;
    channel(state)?.materialize_info(binding)
}

fn binding_of(
    state: &Rc<BrowserSessionsState>,
    id: &str,
    record: &Rc<RefCell<BrowserSessionRecord>>,
) -> Result<JsValue, JsValue> {
    let context = ensure_scope(state, id, record)?;
    object(&[
        ("sessionId", JsValue::from_str(id)),
        ("session", record.borrow().session.clone()),
        ("ctx", context),
    ])
    .map(Into::into)
}

fn ensure_scope(
    state: &BrowserSessionsState,
    id: &str,
    record: &Rc<RefCell<BrowserSessionRecord>>,
) -> Result<JsValue, JsValue> {
    if let Some(context) = record.borrow().scope.clone() {
        return Ok(context);
    }
    let handle = create_client_scope(state.root.clone(), id.to_owned())?;
    let context = Reflect::get(&handle, &JsValue::from_str("ctx"))?;
    let fiber = Reflect::get(&handle, &JsValue::from_str("fiber"))?;
    let mut record = record.borrow_mut();
    record.scope = Some(context.clone());
    record.scope_fiber = Some(fiber);
    Ok(context)
}

fn session_id_of(session: &JsValue) -> Result<String, JsValue> {
    Reflect::get(session, &JsValue::from_str("sessionId"))?
        .as_string()
        .ok_or_else(|| js_sys::Error::new("FixtureSession omitted sessionId").into())
}

fn require_record(
    state: &BrowserSessionsState,
    id: &str,
) -> Result<Rc<RefCell<BrowserSessionRecord>>, JsValue> {
    state
        .records
        .borrow()
        .get(id)
        .cloned()
        .ok_or_else(|| js_sys::Error::new(&format!("test session {id:?} is not added")).into())
}

fn publish_list(state: &Rc<BrowserSessionsState>, next: JsValue) -> Result<(), JsValue> {
    *state.list_snapshot.borrow_mut() = next;
    channel(state)?.publish_current()?;
    let listeners = state.list_listeners.borrow().clone();
    for listener in listeners {
        listener.call0(&JsValue::UNDEFINED)?;
    }
    Ok(())
}

fn update_list(state: &Rc<BrowserSessionsState>, mutator: Function) -> Promise {
    let mutation_state = state.clone();
    let produce = state.produce.clone();
    let mutation = Closure::once_into_js(move || -> Result<(), JsValue> {
        let next = produce.call2(
            &JsValue::UNDEFINED,
            &mutation_state.list_snapshot.borrow(),
            &mutator,
        )?;
        publish_list(&mutation_state, next)
    });
    stabilize(state, &mutation)
}

fn mutate_list_now(state: &Rc<BrowserSessionsState>, mutator: &Function) -> Result<(), JsValue> {
    let next = state
        .produce
        .call2(&JsValue::UNDEFINED, &state.list_snapshot.borrow(), mutator)?;
    publish_list(state, next)
}

fn update_fixture_snapshot(
    owner: &Rc<BrowserSessionsState>,
    record: Rc<RefCell<BrowserSessionRecord>>,
    mutator: Function,
) -> Promise {
    let mutation_record = record;
    let produce = owner.produce.clone();
    let mutation = Closure::once_into_js(move || -> Result<(), JsValue> {
        let fixture = mutation_record.borrow().fixture.clone();
        let next = produce.call2(&JsValue::UNDEFINED, &fixture.snapshot.borrow(), &mutator)?;
        *fixture.snapshot.borrow_mut() = next;
        let listeners = fixture.listeners.borrow().clone();
        for listener in listeners {
            listener.call0(&JsValue::UNDEFINED)?;
        }
        Ok(())
    });
    stabilize(owner, &mutation)
}

fn stabilize(state: &BrowserSessionsState, mutation: &JsValue) -> Promise {
    match state.stabilize.call1(&JsValue::UNDEFINED, mutation) {
        Ok(result) => Promise::resolve(&result),
        Err(error) => Promise::reject(&error),
    }
}

fn initial_list_snapshot() -> Result<Object, JsValue> {
    object(&[
        ("ids", Array::new().into()),
        ("byId", Object::new().into()),
        ("current", JsValue::UNDEFINED),
        ("phase", JsValue::from_str("ready")),
        ("subagentsByParent", Object::new().into()),
        ("jobsBySession", Object::new().into()),
        ("currentAddress", JsValue::UNDEFINED),
    ])
}

#[allow(clippy::too_many_lines)]
fn install_session_methods(face: &Object, state: &Rc<BrowserSessionsState>) -> Result<(), JsValue> {
    let add_state = state.clone();
    let add = Closure::wrap(
        Box::new(move |fixture: JsValue, options: JsValue| -> Promise {
            let operation = (|| -> Result<Promise, JsValue> {
                let id = required_string(&fixture, "id", "Session fixture")?;
                if add_state.records.borrow().contains_key(&id) {
                    return Err(
                        js_sys::Error::new(&format!("test session {id:?} already added")).into(),
                    );
                }
                let summary = object(&[
                    ("id", JsValue::from_str(&id)),
                    ("displayTitle", JsValue::from_str(&id)),
                    ("running", JsValue::FALSE),
                    ("blank", JsValue::FALSE),
                    (
                        "updatedAt",
                        JsValue::from_f64(
                            u32::try_from(add_state.records.borrow().len())
                                .map(f64::from)
                                .unwrap_or(f64::from(u32::MAX))
                                + 1.0,
                        ),
                    ),
                ])?;
                let summary_override = Reflect::get(&fixture, &JsValue::from_str("summary"))?;
                if !summary_override.is_undefined() && !summary_override.is_null() {
                    Object::assign(&summary, &Object::from(summary_override));
                }
                let snapshot = conversation_snapshot_js(id.clone())?;
                let snapshot_override = Reflect::get(&fixture, &JsValue::from_str("snapshot"))?;
                let snapshot = if snapshot_override.is_undefined() || snapshot_override.is_null() {
                    snapshot
                } else {
                    let merged =
                        Object::assign(&Object::from(snapshot), &Object::from(snapshot_override));
                    merged.into()
                };
                let overrides = Reflect::get(&fixture, &JsValue::from_str("session"))?;
                let overrides = if overrides.is_undefined() || overrides.is_null() {
                    Object::new().into()
                } else {
                    overrides
                };
                let (session, fixture_state) = fixture_session(&id, snapshot, overrides)?;
                let record = Rc::new(RefCell::new(BrowserSessionRecord {
                    summary: summary.clone().into(),
                    fixture: fixture_state,
                    session,
                    scope: None,
                    scope_fiber: None,
                    provide_info: None,
                }));
                add_state.records.borrow_mut().insert(id.clone(), record);
                add_state.record_order.borrow_mut().push(id.clone());
                let current = if options.is_undefined() || options.is_null() {
                    true
                } else {
                    Reflect::get(&options, &JsValue::from_str("current"))?
                        .as_bool()
                        .unwrap_or(true)
                };
                let mutation_id = id.clone();
                let mutation_summary: JsValue = summary.into();
                let mutator = Closure::once_into_js(move |draft: JsValue| -> Result<(), JsValue> {
                    let ids = Array::from(&Reflect::get(&draft, &JsValue::from_str("ids"))?);
                    ids.push(&JsValue::from_str(&mutation_id));
                    Reflect::set(&draft, &JsValue::from_str("ids"), &ids)?;
                    let by_id = Reflect::get(&draft, &JsValue::from_str("byId"))?;
                    Reflect::set(&by_id, &JsValue::from_str(&mutation_id), &mutation_summary)?;
                    if current {
                        Reflect::set(
                            &draft,
                            &JsValue::from_str("current"),
                            &JsValue::from_str(&mutation_id),
                        )?;
                    }
                    Ok(())
                });
                let promise = update_list(&add_state, mutator.unchecked_into::<Function>());
                Ok(future_to_promise(async move {
                    JsFuture::from(promise).await?;
                    Ok(JsValue::from_str(&id))
                }))
            })();
            match operation {
                Ok(promise) => promise,
                Err(error) => Promise::reject(&error),
            }
        }) as Box<dyn FnMut(JsValue, JsValue) -> Promise>,
    );
    set(face, "add", &add.into_js_value())?;

    let snapshot_state = state.clone();
    let update_snapshot = Closure::wrap(Box::new(move |id: String, mutator: Function| -> Promise {
        match require_record(&snapshot_state, &id) {
            Ok(record) => update_fixture_snapshot(&snapshot_state, record, mutator),
            Err(error) => Promise::reject(&error),
        }
    }) as Box<dyn FnMut(String, Function) -> Promise>);
    set(face, "updateSnapshot", &update_snapshot.into_js_value())?;

    let summary_state = state.clone();
    let update_summary = Closure::wrap(Box::new(move |id: String, patch: JsValue| -> Promise {
        let operation = (|| -> Result<Promise, JsValue> {
            let record = require_record(&summary_state, &id)?;
            let summary = Object::assign(
                &Object::new(),
                &Object::from(record.borrow().summary.clone()),
            );
            Object::assign(&summary, &Object::from(patch));
            record.borrow_mut().summary = summary.clone().into();
            let mutation_id = id;
            let mutation_summary: JsValue = summary.into();
            let mutator = Closure::once_into_js(move |draft: JsValue| -> Result<(), JsValue> {
                let by_id = Reflect::get(&draft, &JsValue::from_str("byId"))?;
                Reflect::set(&by_id, &JsValue::from_str(&mutation_id), &mutation_summary)?;
                Ok(())
            });
            Ok(update_list(
                &summary_state,
                mutator.unchecked_into::<Function>(),
            ))
        })();
        operation.unwrap_or_else(|error| Promise::reject(&error))
    }) as Box<dyn FnMut(String, JsValue) -> Promise>);
    set(face, "updateSummary", &update_summary.into_js_value())?;

    let current_state = state.clone();
    let set_current = Closure::wrap(Box::new(move |id: JsValue| -> Promise {
        let operation = (|| -> Result<Promise, JsValue> {
            if let Some(id) = id.as_string() {
                require_record(&current_state, &id)?;
            }
            let current = id;
            let mutator = Closure::once_into_js(move |draft: JsValue| -> Result<(), JsValue> {
                Reflect::set(&draft, &JsValue::from_str("current"), &current)?;
                Ok(())
            });
            Ok(update_list(
                &current_state,
                mutator.unchecked_into::<Function>(),
            ))
        })();
        operation.unwrap_or_else(|error| Promise::reject(&error))
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    set(face, "setCurrent", &set_current.into_js_value())?;

    let remove_state = state.clone();
    let remove = Closure::wrap(Box::new(move |id: String| -> Promise {
        let record = match require_record(&remove_state, &id) {
            Ok(record) => record,
            Err(error) => return Promise::reject(&error),
        };
        remove_state.records.borrow_mut().remove(&id);
        remove_state
            .record_order
            .borrow_mut()
            .retain(|record_id| record_id != &id);
        let mutation_id = id.clone();
        let mutator = Closure::once_into_js(move |draft: JsValue| -> Result<(), JsValue> {
            let ids = Array::from(&Reflect::get(&draft, &JsValue::from_str("ids"))?);
            let retained = Array::new();
            for existing in ids.iter() {
                if existing.as_string().as_deref() != Some(mutation_id.as_str()) {
                    retained.push(&existing);
                }
            }
            Reflect::set(&draft, &JsValue::from_str("ids"), &retained)?;
            let by_id = Object::from(Reflect::get(&draft, &JsValue::from_str("byId"))?);
            Reflect::delete_property(&by_id, &JsValue::from_str(&mutation_id))?;
            if Reflect::get(&draft, &JsValue::from_str("current"))?
                .as_string()
                .as_deref()
                == Some(mutation_id.as_str())
            {
                Reflect::set(&draft, &JsValue::from_str("current"), &JsValue::UNDEFINED)?;
            }
            Ok(())
        });
        let cleanup_state = remove_state.clone();
        let mutator = mutator.unchecked_into::<Function>();
        let cleanup = Closure::once_into_js(move || -> Promise {
            let state = cleanup_state.clone();
            let record = record.clone();
            let id = id.clone();
            let mutator = mutator.clone();
            future_to_promise(async move {
                mutate_list_now(&state, &mutator)?;
                let fiber = { record.borrow_mut().scope_fiber.take() };
                if let Some(fiber) = fiber {
                    let disposal = call_method(&fiber, "dispose", &[])?;
                    JsFuture::from(Promise::resolve(&disposal)).await?;
                    record.borrow_mut().scope = None;
                }
                let slots = call_method(&state.root, "get", &[JsValue::from_str("slots")])?;
                if !slots.is_undefined() && !slots.is_null() {
                    call_method(&slots, "pruneStoreScope", &[JsValue::from_str(&id)])?;
                }
                Ok(JsValue::UNDEFINED)
            })
        });
        stabilize(&remove_state, &cleanup)
    }) as Box<dyn FnMut(String) -> Promise>);
    set(face, "remove", &remove.into_js_value())?;

    install_provide_scope_methods(face, state)?;
    install_service_action_methods(face, state)
}

fn install_provide_scope_methods(
    face: &Object,
    state: &Rc<BrowserSessionsState>,
) -> Result<(), JsValue> {
    let provide_state = state.clone();
    let provide = Closure::wrap(
        Box::new(move |descriptor: JsValue| -> Result<Function, JsValue> {
            channel(&provide_state)?.provide(descriptor)
        }) as Box<dyn FnMut(JsValue) -> Result<Function, JsValue>>,
    );
    set(face, "provide", &provide.into_js_value())?;

    let info_state = state.clone();
    let info = Closure::wrap(Box::new(move |id: String| -> Result<JsValue, JsValue> {
        provide_info(&info_state, &id)
    }) as Box<dyn FnMut(String) -> Result<JsValue, JsValue>>);
    set(face, "provideInfo", &info.into_js_value())?;

    let maybe_state = state.clone();
    let maybe = Closure::wrap(Box::new(move |id: JsValue| -> Result<JsValue, JsValue> {
        maybe_provide_info(&maybe_state, id.as_string().as_deref())
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    set(face, "maybeProvideInfo", &maybe.into_js_value())?;

    let scope_state = state.clone();
    let scope = Closure::wrap(Box::new(move |id: String| -> Result<JsValue, JsValue> {
        let Some(record) = scope_state.records.borrow().get(&id).cloned() else {
            return Ok(JsValue::UNDEFINED);
        };
        ensure_scope(&scope_state, &id, &record)
    }) as Box<dyn FnMut(String) -> Result<JsValue, JsValue>>);
    set(face, "scope", &scope.into_js_value())?;

    let binding_state = state.clone();
    let binding = Closure::wrap(Box::new(move |id: String| -> Result<JsValue, JsValue> {
        let Some(record) = binding_state.records.borrow().get(&id).cloned() else {
            return Ok(JsValue::UNDEFINED);
        };
        binding_of(&binding_state, &id, &record)
    }) as Box<dyn FnMut(String) -> Result<JsValue, JsValue>>);
    set(face, "binding", &binding.into_js_value())?;

    let scope_of_context = Closure::wrap(Box::new(move |context: JsValue| {
        scope_of(context).map_or(JsValue::UNDEFINED, |id| JsValue::from_str(&id))
    }) as Box<dyn FnMut(JsValue) -> JsValue>);
    set(face, "scopeOf", &scope_of_context.into_js_value())?;

    let session_of_state = state.clone();
    let session_of = Closure::wrap(Box::new(move |context: JsValue| -> JsValue {
        scope_of(context)
            .and_then(|id| session_of_state.records.borrow().get(&id).cloned())
            .map_or(JsValue::UNDEFINED, |record| record.borrow().session.clone())
    }) as Box<dyn FnMut(JsValue) -> JsValue>);
    set(face, "sessionOf", &session_of.into_js_value())?;

    let behavior_state = state.clone();
    let behavior = Closure::wrap(Box::new(move |id: String| -> Result<JsValue, JsValue> {
        Ok(require_record(&behavior_state, &id)?
            .borrow()
            .session
            .clone())
    }) as Box<dyn FnMut(String) -> Result<JsValue, JsValue>>);
    set(face, "behavior", &behavior.into_js_value())?;

    let dispose_state = state.clone();
    let dispose_scopes = Closure::wrap(Box::new(move || -> Promise {
        let state = dispose_state.clone();
        future_to_promise(async move {
            for record in ordered_records(&state) {
                let fiber = { record.borrow_mut().scope_fiber.take() };
                if let Some(fiber) = fiber {
                    let result = call_method(&fiber, "dispose", &[])?;
                    JsFuture::from(Promise::resolve(&result)).await?;
                    record.borrow_mut().scope = None;
                }
            }
            Ok(JsValue::UNDEFINED)
        })
    }) as Box<dyn FnMut() -> Promise>);
    set(face, "disposeScopes", &dispose_scopes.into_js_value())?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn install_service_action_methods(
    face: &Object,
    state: &Rc<BrowserSessionsState>,
) -> Result<(), JsValue> {
    let open_state = state.clone();
    let open = Closure::wrap(Box::new(move |id: String| -> Result<(), JsValue> {
        record_call(&open_state, "open", Array::of1(&JsValue::from_str(&id)))?;
        require_record(&open_state, &id)?;
        let mutator = Closure::once_into_js(move |draft: JsValue| -> Result<(), JsValue> {
            Reflect::set(
                &draft,
                &JsValue::from_str("current"),
                &JsValue::from_str(&id),
            )?;
            Reflect::set(
                &draft,
                &JsValue::from_str("currentAddress"),
                &JsValue::UNDEFINED,
            )?;
            Ok(())
        });
        mutate_list_now(&open_state, &mutator.unchecked_into())
    }) as Box<dyn FnMut(String) -> Result<(), JsValue>>);
    set(face, "open", &open.into_js_value())?;

    let subagent_state = state.clone();
    let open_subagent = Closure::wrap(Box::new(move |address: JsValue| -> Result<(), JsValue> {
        record_call(&subagent_state, "openSubagent", Array::of1(&address))?;
        let id = required_string(&address, "childSessionId", "Subagent address")?;
        require_record(&subagent_state, &id)?;
        let mutator_address = address;
        let mutator = Closure::once_into_js(move |draft: JsValue| -> Result<(), JsValue> {
            Reflect::set(
                &draft,
                &JsValue::from_str("current"),
                &JsValue::from_str(&id),
            )?;
            Reflect::set(
                &draft,
                &JsValue::from_str("currentAddress"),
                &mutator_address,
            )?;
            Ok(())
        });
        mutate_list_now(&subagent_state, &mutator.unchecked_into())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    set(face, "openSubagent", &open_subagent.into_js_value())?;

    let address_state = state.clone();
    let subagent_address = Closure::wrap(Box::new(move |id: String| -> Result<JsValue, JsValue> {
        let address = Reflect::get(
            &address_state.list_snapshot.borrow(),
            &JsValue::from_str("currentAddress"),
        )?;
        if address.is_undefined()
            || required_string(&address, "childSessionId", "Subagent address")? != id
        {
            Ok(JsValue::UNDEFINED)
        } else {
            Ok(address)
        }
    })
        as Box<dyn FnMut(String) -> Result<JsValue, JsValue>>);
    set(face, "subagentAddress", &subagent_address.into_js_value())?;

    let catalog_state = state.clone();
    let catalog = Closure::wrap(Box::new(
        move |parent: JsValue, open: bool| -> Result<(), JsValue> {
            record_call(
                &catalog_state,
                "setSubagentCatalogOpen",
                Array::of2(&parent, &JsValue::from_bool(open)),
            )
        },
    )
        as Box<dyn FnMut(JsValue, bool) -> Result<(), JsValue>>);
    set(face, "setSubagentCatalogOpen", &catalog.into_js_value())?;

    let refresh_state = state.clone();
    let refresh = Closure::wrap(Box::new(move |parent: JsValue| -> Promise {
        match record_call(&refresh_state, "refreshSubagents", Array::of1(&parent)) {
            Ok(()) => Promise::resolve(&JsValue::UNDEFINED),
            Err(error) => Promise::reject(&error),
        }
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    set(face, "refreshSubagents", &refresh.into_js_value())?;

    let preset_state = state.clone();
    let preset = Closure::wrap(Box::new(
        move |id: String, agent_preset: JsValue| -> Result<(), JsValue> {
            let mutation_id = id;
            let mutator = Closure::once_into_js(move |draft: JsValue| -> Result<(), JsValue> {
                let by_id = Reflect::get(&draft, &JsValue::from_str("byId"))?;
                let summary = Reflect::get(&by_id, &JsValue::from_str(&mutation_id))?;
                if !summary.is_undefined() {
                    let next = Object::assign(&Object::new(), &Object::from(summary));
                    Reflect::set(&next, &JsValue::from_str("agentPreset"), &agent_preset)?;
                    Reflect::set(&by_id, &JsValue::from_str(&mutation_id), &next)?;
                }
                Ok(())
            });
            mutate_list_now(&preset_state, &mutator.unchecked_into())
        },
    )
        as Box<dyn FnMut(String, JsValue) -> Result<(), JsValue>>);
    set(face, "noteAgentPreset", &preset.into_js_value())?;

    let clear_state = state.clone();
    let clear = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        record_call(&clear_state, "clear", Array::new())?;
        let mutator = Closure::once_into_js(move |draft: JsValue| -> Result<(), JsValue> {
            Reflect::set(&draft, &JsValue::from_str("current"), &JsValue::UNDEFINED)?;
            Reflect::set(
                &draft,
                &JsValue::from_str("currentAddress"),
                &JsValue::UNDEFINED,
            )?;
            Ok(())
        });
        mutate_list_now(&clear_state, &mutator.unchecked_into())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    set(face, "clear", &clear.into_js_value())?;

    let stub_state = state.clone();
    let stub_search = Closure::wrap(Box::new(move |implementation: Function| {
        *stub_state.search_stub.borrow_mut() = Some(implementation);
    }) as Box<dyn FnMut(Function)>);
    set(face, "stubSearch", &stub_search.into_js_value())?;

    let search_state = state.clone();
    let search = Closure::wrap(Box::new(
        move |query: String, signal: JsValue| -> Result<Promise, JsValue> {
            record_call(
                &search_state,
                "search",
                Array::of2(&JsValue::from_str(&query), &signal),
            )?;
            let page = if let Some(stub) = search_state.search_stub.borrow().clone() {
                stub.call2(&JsValue::UNDEFINED, &JsValue::from_str(&query), &signal)?
            } else {
                object(&[("items", Array::new().into()), ("hasMore", JsValue::FALSE)])?.into()
            };
            let response = object(&[("ok", JsValue::TRUE), ("value", page)])?;
            let response: JsValue = response.into();
            Ok(Promise::resolve(&response))
        },
    )
        as Box<dyn FnMut(String, JsValue) -> Result<Promise, JsValue>>);
    set(face, "search", &search.into_js_value())?;

    let fork_state = state.clone();
    let fork = Closure::wrap(
        Box::new(move |options: JsValue| -> Result<Promise, JsValue> {
            record_call(&fork_state, "fork", Array::of1(&options))?;
            let id = Reflect::get(&options, &JsValue::from_str("sessionId"))?;
            Ok(Promise::resolve(&id))
        }) as Box<dyn FnMut(JsValue) -> Result<Promise, JsValue>>,
    );
    set(face, "fork", &fork.into_js_value())?;
    Ok(())
}

fn record_call(
    state: &BrowserSessionsState,
    method: &str,
    arguments: Array,
) -> Result<(), JsValue> {
    let call = object(&[
        ("method", JsValue::from_str(method)),
        ("args", arguments.into()),
    ])?;
    state.calls.push(call.as_ref());
    Ok(())
}

fn projection_face() -> Result<JsValue, JsValue> {
    let state = Rc::new(ProjectionState {
        values: RefCell::new(HashMap::new()),
        listeners: RefCell::new(HashMap::new()),
        faces: RefCell::new(HashMap::new()),
    });
    let projections = Object::new();
    let face_state = state.clone();
    let face_of = Closure::wrap(Box::new(move |key: String| -> Result<JsValue, JsValue> {
        if let Some(face) = face_state.faces.borrow().get(&key) {
            return Ok(face.clone());
        }
        let face = Object::new();
        let snapshot_state = face_state.clone();
        let snapshot_key = key.clone();
        let snapshot = Closure::wrap(Box::new(move || {
            snapshot_state
                .values
                .borrow()
                .get(&snapshot_key)
                .cloned()
                .unwrap_or(JsValue::UNDEFINED)
        }) as Box<dyn FnMut() -> JsValue>);
        set(&face, "getSnapshot", &snapshot.into_js_value())?;
        let subscribe_state = face_state.clone();
        let subscribe_key = key.clone();
        let subscribe = Closure::wrap(Box::new(move |listener: Function| -> Function {
            let mut listeners = subscribe_state.listeners.borrow_mut();
            let key_listeners = listeners.entry(subscribe_key.clone()).or_default();
            if !key_listeners
                .iter()
                .any(|registered| Object::is(registered, &listener))
            {
                key_listeners.push(listener.clone());
            }
            drop(listeners);
            let cleanup_state = subscribe_state.clone();
            let cleanup_key = subscribe_key.clone();
            Closure::wrap(Box::new(move || {
                let mut listeners = cleanup_state.listeners.borrow_mut();
                let Some(key_listeners) = listeners.get_mut(&cleanup_key) else {
                    return;
                };
                key_listeners.retain(|registered| !Object::is(registered, &listener));
                if key_listeners.is_empty() {
                    listeners.remove(&cleanup_key);
                }
            }) as Box<dyn FnMut()>)
            .into_js_value()
            .unchecked_into()
        }) as Box<dyn FnMut(Function) -> Function>);
        set(&face, "subscribe", &subscribe.into_js_value())?;
        let face: JsValue = face.into();
        face_state.faces.borrow_mut().insert(key, face.clone());
        Ok(face)
    }) as Box<dyn FnMut(String) -> Result<JsValue, JsValue>>);
    set(&projections, "faceOf", &face_of.into_js_value())?;
    let set_state = state;
    let set_projection = Closure::wrap(Box::new(
        move |key: String, value: JsValue| -> Result<(), JsValue> {
            set_state.values.borrow_mut().insert(key.clone(), value);
            let listeners = set_state
                .listeners
                .borrow()
                .get(&key)
                .cloned()
                .unwrap_or_default();
            for listener in listeners {
                listener.call0(&JsValue::UNDEFINED)?;
            }
            Ok(())
        },
    )
        as Box<dyn FnMut(String, JsValue) -> Result<(), JsValue>>);
    set(&projections, "set", &set_projection.into_js_value())?;
    Ok(projections.into())
}

fn set(target: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    Reflect::set(target, &JsValue::from_str(key), value).map(|_| ())
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let value = Object::new();
    for (key, entry) in entries {
        set(&value, key, entry)?;
    }
    Ok(value)
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let function = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let arguments: Array = arguments.iter().cloned().collect();
    function.apply(value, &arguments)
}

fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    Reflect::get(value, &JsValue::from_str(key))?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} requires string {key:?}")).into())
}
