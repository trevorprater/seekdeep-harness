//! Browser Workspaces double with observable state, typed defaults, and replaceable stubs.

use std::{cell::RefCell, rc::Rc};

use js_sys::{Array, Function, Map, Object, Promise, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

struct BrowserWorkspaceState {
    snapshot: RefCell<JsValue>,
    listeners: RefCell<Vec<Function>>,
    calls: Array,
    stubs: Map,
    stabilize: Function,
    produce: Function,
}

/// Constructs and publishes the browser Workspaces double as `ctx.workspaces`.
///
/// `stabilize(callback)` owns test-framework flushing and `produce(base, mutator)` owns immutable
/// draft replacement, matching the two injected dependencies of the source test runtime.
///
/// # Errors
///
/// Returns malformed factory, Context, observable, or action failures.
#[wasm_bindgen(js_name = installTestWorkspaces)]
#[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
pub fn install_test_workspaces(
    context: JsValue,
    stabilizer: JsValue,
    produce: JsValue,
) -> Result<JsValue, JsValue> {
    let face = workspaces_face(stabilizer, produce)?;
    call_method(
        &context,
        "provide",
        &[JsValue::from_str("workspaces"), face.clone()],
    )?;
    Ok(face)
}

/// Constructs the public Workspaces double without publishing it.
///
/// # Errors
///
/// Returns malformed stabilization, draft, observable, or action failures.
#[wasm_bindgen(js_name = createTestWorkspaces)]
#[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
pub fn create_test_workspaces(stabilizer: JsValue, produce: JsValue) -> Result<JsValue, JsValue> {
    workspaces_face(stabilizer, produce)
}

#[allow(clippy::too_many_lines)]
fn workspaces_face(stabilizer: JsValue, produce: JsValue) -> Result<JsValue, JsValue> {
    let stabilizer = stabilizer
        .dyn_into::<Function>()
        .map_err(|_| js_sys::TypeError::new("TestWorkspaces stabilize must be a function"))?;
    let produce = produce
        .dyn_into::<Function>()
        .map_err(|_| js_sys::TypeError::new("TestWorkspaces produce must be a function"))?;
    let state = Rc::new(BrowserWorkspaceState {
        snapshot: RefCell::new(initial_snapshot()?.into()),
        listeners: RefCell::new(Vec::new()),
        calls: Array::new(),
        stubs: Map::new(),
        stabilize: stabilizer,
        produce,
    });
    let list = observable_face(&state)?;
    let face = Object::new();
    set(&face, "list", &list)?;
    set(&face, "calls", state.calls.as_ref())?;

    let stub_state = state.clone();
    let stub = Closure::wrap(Box::new(move |method: String, implementation: Function| {
        stub_state
            .stubs
            .set(&JsValue::from_str(&method), implementation.as_ref());
    }) as Box<dyn FnMut(String, Function)>);
    set(&face, "stub", &stub.into_js_value())?;

    let update_state = state.clone();
    let update = Closure::wrap(Box::new(move |mutator: Function| -> Promise {
        update_snapshot(&update_state, mutator)
    }) as Box<dyn FnMut(Function) -> Promise>);
    set(&face, "update", &update.into_js_value())?;

    let connect_state = state.clone();
    let connect = Closure::wrap(Box::new(move |workspace_id: JsValue| -> Promise {
        record(
            &connect_state,
            "connectWorkspace",
            Array::of1(&workspace_id),
        );
        if let Some(stub) = stub_of(&connect_state, "connectWorkspace") {
            return promise_call(&stub, &[workspace_id]);
        }
        Promise::resolve(&JsValue::from_str(&format!(
            "session-of-{}",
            js_string(&workspace_id)
        )))
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    set(&face, "connectWorkspace", &connect.into_js_value())?;

    let start_state = state.clone();
    let start = Closure::wrap(
        Box::new(move |workspace_id: JsValue| -> Result<(), JsValue> {
            record(&start_state, "startSession", Array::of1(&workspace_id));
            if let Some(stub) = stub_of(&start_state, "startSession") {
                stub.call1(&JsValue::UNDEFINED, &workspace_id)?;
            }
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>,
    );
    set(&face, "startSession", &start.into_js_value())?;

    let create_state = state.clone();
    let create = Closure::wrap(Box::new(move |input: JsValue| -> Promise {
        record(&create_state, "create", Array::of1(&input));
        if let Some(stub) = stub_of(&create_state, "create") {
            return promise_call(&stub, &[input]);
        }
        let created = Reflect::get(&input, &JsValue::from_str("path")).and_then(|path| {
            let created: JsValue = object(&[
                (
                    "workspaceId",
                    JsValue::from_str(&format!("ws-{}", js_string(&path))),
                ),
                ("title", path.clone()),
                ("path", path),
                ("sessionIds", Array::new().into()),
            ])?
            .into();
            Ok(created)
        });
        match created {
            Ok(created) => Promise::resolve(&created),
            Err(error) => Promise::reject(&error),
        }
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    set(&face, "create", &create.into_js_value())?;

    install_unary_void(&face, &state, "openPath")?;
    install_zero_optional_string(&face, &state, "pickDirectory")?;

    let list_state = state.clone();
    let list_directory = Closure::wrap(Box::new(move |path: JsValue, signal: JsValue| -> Promise {
        record(&list_state, "listDirectory", Array::of2(&path, &signal));
        if let Some(stub) = stub_of(&list_state, "listDirectory") {
            return promise_call(&stub, &[path, signal]);
        }
        match default_listing() {
            Ok(listing) => Promise::resolve(&listing),
            Err(error) => Promise::reject(&error),
        }
    }) as Box<dyn FnMut(JsValue, JsValue) -> Promise>);
    set(&face, "listDirectory", &list_directory.into_js_value())?;

    let create_directory_state = state.clone();
    let create_directory = Closure::wrap(Box::new(move |path: JsValue, name: JsValue| -> Promise {
        record(
            &create_directory_state,
            "createDirectory",
            Array::of2(&path, &name),
        );
        if let Some(stub) = stub_of(&create_directory_state, "createDirectory") {
            return promise_call(&stub, &[path, name]);
        }
        Promise::resolve(&JsValue::from_str(&format!(
            "{}/{}",
            js_string(&path),
            js_string(&name)
        )))
    }) as Box<dyn FnMut(JsValue, JsValue) -> Promise>);
    set(&face, "createDirectory", &create_directory.into_js_value())?;

    let rename_state = state.clone();
    let rename = Closure::wrap(
        Box::new(move |workspace_id: JsValue, title: JsValue| -> Promise {
            record(&rename_state, "rename", Array::of2(&workspace_id, &title));
            if let Some(stub) = stub_of(&rename_state, "rename") {
                return promise_call(&stub, &[workspace_id, title]);
            }
            match object(&[
                ("workspaceId", workspace_id),
                ("title", title.clone()),
                (
                    "path",
                    JsValue::from_str(&format!("/{}", js_string(&title))),
                ),
                ("sessionIds", Array::new().into()),
            ]) {
                Ok(view) => {
                    let view: JsValue = view.into();
                    Promise::resolve(&view)
                }
                Err(error) => Promise::reject(&error),
            }
        }) as Box<dyn FnMut(JsValue, JsValue) -> Promise>,
    );
    set(&face, "rename", &rename.into_js_value())?;

    install_unary_void(&face, &state, "delete")?;
    install_binary_void(&face, &state, "insertBefore")?;

    let insert_session_state = state.clone();
    let insert_session = Closure::wrap(Box::new(
        move |workspace_id: JsValue, session_id: JsValue, before: JsValue| -> Promise {
            record(
                &insert_session_state,
                "insertSessionBefore",
                Array::of3(&workspace_id, &session_id, &before),
            );
            if let Some(stub) = stub_of(&insert_session_state, "insertSessionBefore") {
                return promise_call(&stub, &[workspace_id, session_id, before]);
            }
            match object(&[
                ("workspaceId", workspace_id),
                ("title", JsValue::from_str("")),
                ("path", JsValue::from_str("")),
                ("sessionIds", Array::of1(&session_id).into()),
            ]) {
                Ok(view) => {
                    let view: JsValue = view.into();
                    Promise::resolve(&view)
                }
                Err(error) => Promise::reject(&error),
            }
        },
    )
        as Box<dyn FnMut(JsValue, JsValue, JsValue) -> Promise>);
    set(
        &face,
        "insertSessionBefore",
        &insert_session.into_js_value(),
    )?;

    let archive_state = state;
    let archive = Closure::wrap(Box::new(move |session_id: JsValue| -> Promise {
        record(&archive_state, "archiveSession", Array::of1(&session_id));
        if let Some(stub) = stub_of(&archive_state, "archiveSession") {
            return promise_call(&stub, &[session_id]);
        }
        let mutation_state = archive_state.clone();
        let mutation = Closure::once_into_js(move || -> Result<(), JsValue> {
            let current = mutation_state.snapshot.borrow().clone();
            let next = Object::assign(&Object::new(), &Object::from(current.clone()));
            let archived = Array::from(&Reflect::get(
                &current,
                &JsValue::from_str("archivedSessionIds"),
            )?);
            archived.push(&session_id);
            set(&next, "archivedSessionIds", archived.as_ref())?;
            publish(&mutation_state, next.into())?;
            Ok(())
        });
        stabilize(&archive_state, &mutation)
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    set(&face, "archiveSession", &archive.into_js_value())?;

    Ok(face.into())
}

fn observable_face(state: &Rc<BrowserWorkspaceState>) -> Result<JsValue, JsValue> {
    let face = Object::new();
    let snapshot_state = state.clone();
    let snapshot =
        Closure::wrap(Box::new(move || snapshot_state.snapshot.borrow().clone())
            as Box<dyn FnMut() -> JsValue>);
    set(&face, "getSnapshot", &snapshot.into_js_value())?;
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
    Ok(face.into())
}

fn update_snapshot(state: &Rc<BrowserWorkspaceState>, mutator: Function) -> Promise {
    let mutation_state = state.clone();
    let produce = state.produce.clone();
    let mutation = Closure::once_into_js(move || -> Result<(), JsValue> {
        let next = produce.call2(
            &JsValue::UNDEFINED,
            &mutation_state.snapshot.borrow(),
            &mutator,
        )?;
        publish(&mutation_state, next)?;
        Ok(())
    });
    stabilize(state, &mutation)
}

fn stabilize(state: &BrowserWorkspaceState, mutation: &JsValue) -> Promise {
    match state.stabilize.call1(&JsValue::UNDEFINED, mutation) {
        Ok(result) => Promise::resolve(&result),
        Err(error) => Promise::reject(&error),
    }
}

fn publish(state: &BrowserWorkspaceState, next: JsValue) -> Result<(), JsValue> {
    *state.snapshot.borrow_mut() = next;
    let listeners = state.listeners.borrow().clone();
    for listener in listeners {
        listener.call0(&JsValue::UNDEFINED)?;
    }
    Ok(())
}

fn install_unary_void(
    face: &Object,
    state: &Rc<BrowserWorkspaceState>,
    method: &'static str,
) -> Result<(), JsValue> {
    let action_state = state.clone();
    let action = Closure::wrap(Box::new(move |argument: JsValue| -> Promise {
        record(&action_state, method, Array::of1(&argument));
        stub_of(&action_state, method).map_or_else(
            || Promise::resolve(&JsValue::UNDEFINED),
            |stub| promise_call(&stub, &[argument]),
        )
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    set(face, method, &action.into_js_value())
}

fn install_binary_void(
    face: &Object,
    state: &Rc<BrowserWorkspaceState>,
    method: &'static str,
) -> Result<(), JsValue> {
    let action_state = state.clone();
    let action = Closure::wrap(Box::new(move |first: JsValue, second: JsValue| -> Promise {
        record(&action_state, method, Array::of2(&first, &second));
        stub_of(&action_state, method).map_or_else(
            || Promise::resolve(&JsValue::UNDEFINED),
            |stub| promise_call(&stub, &[first, second]),
        )
    }) as Box<dyn FnMut(JsValue, JsValue) -> Promise>);
    set(face, method, &action.into_js_value())
}

fn install_zero_optional_string(
    face: &Object,
    state: &Rc<BrowserWorkspaceState>,
    method: &'static str,
) -> Result<(), JsValue> {
    let action_state = state.clone();
    let action = Closure::wrap(Box::new(move || -> Promise {
        record(&action_state, method, Array::new());
        stub_of(&action_state, method).map_or_else(
            || Promise::resolve(&JsValue::NULL),
            |stub| promise_call(&stub, &[]),
        )
    }) as Box<dyn FnMut() -> Promise>);
    set(face, method, &action.into_js_value())
}

fn record(state: &BrowserWorkspaceState, method: &str, arguments: Array) {
    if let Ok(call) = object(&[
        ("method", JsValue::from_str(method)),
        ("args", arguments.into()),
    ]) {
        state.calls.push(call.as_ref());
    }
}

fn stub_of(state: &BrowserWorkspaceState, method: &str) -> Option<Function> {
    state
        .stubs
        .get(&JsValue::from_str(method))
        .dyn_into::<Function>()
        .ok()
}

fn promise_call(function: &Function, arguments: &[JsValue]) -> Promise {
    let arguments: Array = arguments.iter().cloned().collect();
    match function.apply(&JsValue::UNDEFINED, &arguments) {
        Ok(result) => Promise::resolve(&result),
        Err(error) => Promise::reject(&error),
    }
}

fn initial_snapshot() -> Result<Object, JsValue> {
    object(&[
        ("items", Array::new().into()),
        ("archivedSessionIds", Array::new().into()),
        ("state", JsValue::from_str("idle")),
        ("phase", JsValue::from_str("ready")),
        ("error", JsValue::NULL),
        ("baselinesReady", JsValue::TRUE),
        ("recentWorkspaceId", JsValue::UNDEFINED),
    ])
}

fn default_listing() -> Result<JsValue, JsValue> {
    let crumbs = Array::new();
    for (name, path) in [("/", "/"), ("home", "/home"), ("test", "/home/test")] {
        crumbs.push(
            object(&[
                ("name", JsValue::from_str(name)),
                ("path", JsValue::from_str(path)),
                ("hidden", JsValue::FALSE),
            ])?
            .as_ref(),
        );
    }
    object(&[
        ("path", JsValue::from_str("/home/test")),
        ("home", JsValue::from_str("/home/test")),
        ("crumbs", crumbs.into()),
        ("entries", Array::new().into()),
        ("truncated", JsValue::FALSE),
    ])
    .map(Into::into)
}

fn js_string(value: &JsValue) -> String {
    js_sys::JsString::from(value.clone())
        .as_string()
        .unwrap_or_default()
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let function = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let arguments: Array = arguments.iter().cloned().collect();
    function.apply(value, &arguments)
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let value = Object::new();
    for (key, entry) in entries {
        set(&value, key, entry)?;
    }
    Ok(value)
}

fn set(target: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    Reflect::set(target, &JsValue::from_str(key), value).map(|_| ())
}
