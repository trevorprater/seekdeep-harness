//! Public Fiber construction and receiver-driven effect methods.

use std::{cell::RefCell, sync::Arc};

use js_sys::{Array, Function, Object, Promise, Reflect, WeakMap};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;

use super::{FaceSlot, browser_fiber::BrowserLifecycle, object};

thread_local! {
    static PROTOTYPE: RefCell<Option<Object>> = const { RefCell::new(None) };
    static CONTROLLERS: WeakMap = WeakMap::new();
    static BACKENDS: WeakMap = WeakMap::new();
}

pub(super) fn bind_backend(owner: &JsValue, backend: &JsValue) {
    BACKENDS.with(|bindings| bindings.set(owner.unchecked_ref::<Object>(), backend));
}

pub(super) fn bind_lifecycle(owner: &JsValue, lifecycle: BrowserLifecycle) {
    let invoke = Closure::wrap(Box::new(move |owner: JsValue, name: String, args: Array| {
        dispatch(&lifecycle, owner, &name, &args)
    })
        as Box<dyn Fn(JsValue, String, Array) -> Result<JsValue, JsValue>>)
    .into_js_value();
    CONTROLLERS.with(|controllers| controllers.set(owner.unchecked_ref::<Object>(), &invoke));
}

fn dispatch(
    lifecycle: &BrowserLifecycle,
    owner: JsValue,
    name: &str,
    args: &Array,
) -> Result<JsValue, JsValue> {
    match name {
        "update" => BrowserLifecycle::update(&owner, &args.get(0), &args.get(1)),
        "restart" => BrowserLifecycle::restart(&owner).map(Into::into),
        "syncState" => {
            lifecycle.synchronize_state(&owner, &args.get(0));
            Ok(JsValue::UNDEFINED)
        }
        "await" => Ok(BrowserLifecycle::wait(owner).into()),
        "_refresh" => BrowserLifecycle::refresh(&owner).map(|()| JsValue::UNDEFINED),
        "_setEpoch" => lifecycle
            .set_epoch_value(&owner, &args.get(0))
            .map(|()| JsValue::UNDEFINED),
        "_reload" => lifecycle.reload(owner).map(Into::into),
        "_unload" => Ok(lifecycle.unload(owner).into()),
        _ => Err(js_sys::TypeError::new("unknown Fiber operation").into()),
    }
}

pub(super) fn synchronize_state(owner: &JsValue, state: &JsValue) -> Result<(), JsValue> {
    let mut current = owner.clone();
    while current.is_object() || current.is_function() {
        let invoke =
            CONTROLLERS.with(|controllers| controllers.get(current.unchecked_ref::<Object>()));
        if invoke.is_function() {
            Reflect::apply(
                invoke.unchecked_ref::<Function>(),
                &JsValue::UNDEFINED,
                &Array::of3(owner, &"syncState".into(), &Array::of1(state)),
            )?;
            break;
        }
        current = Reflect::get_prototype_of(&current)?.into();
    }
    Ok(())
}

/// Dispatches lifecycle operations while retaining an awaitable handle's receiver.
///
/// # Errors
/// Propagates lifecycle and malformed-receiver failures.
#[wasm_bindgen(js_name = fiberInvoke)]
pub fn invoke(owner: &JsValue, name: &str, args: &Array) -> Result<JsValue, JsValue> {
    let result = (|| {
        let mut current = owner.clone();
        while current.is_object() || current.is_function() {
            let invoke =
                CONTROLLERS.with(|controllers| controllers.get(current.unchecked_ref::<Object>()));
            if invoke.is_function() {
                return Reflect::apply(
                    invoke.unchecked_ref::<Function>(),
                    &JsValue::UNDEFINED,
                    &Array::of3(owner, &name.into(), args),
                );
            }
            current = Reflect::get_prototype_of(&current)?.into();
        }
        dispatch(
            &BrowserLifecycle::detached(owner),
            owner.clone(),
            name,
            args,
        )
    })();
    if matches!(name, "await" | "restart" | "_reload" | "_unload") {
        result.or_else(|error| Ok(Promise::reject(&error).into()))
    } else {
        result
    }
}

/// Supplies the public Fiber prototype used by roots and registry mounts.
#[wasm_bindgen(js_name = configureFiberPrototype)]
pub fn configure_fiber_prototype(prototype: Object) {
    PROTOTYPE.with(|slot| *slot.borrow_mut() = Some(prototype));
}

pub(super) fn prototype() -> Option<Object> {
    PROTOTYPE.with(|slot| slot.borrow().clone())
}

/// Constructs a Fiber directly, without adding its runtime to the registry.
///
/// # Errors
/// Propagates Context, runtime, dependency, and publication failures.
#[wasm_bindgen(js_name = createFiber)]
pub fn create_fiber(
    parent: &JsValue,
    config: JsValue,
    inject: JsValue,
    runtime: &JsValue,
    stack: &JsValue,
    prototype: Option<Object>,
) -> Result<JsValue, JsValue> {
    if !runtime.is_truthy() {
        let native =
            crate::Fiber::root_with_disposal_scheduling(crate::DisposalScheduling::Concurrent);
        return root_face(parent, &native, config, inject, runtime, prototype, stack);
    }
    let native = Reflect::get(parent, &"__seekdeepContext".into())?;
    let args = Array::of4(runtime, &inject, &config, parent);
    args.push(stack);
    args.push(&prototype.map_or(JsValue::UNDEFINED, Into::into));
    let handle = super::browser_registry::method(&native, "mountRuntime", &args)?;
    Ok(Object::get_prototype_of(handle.unchecked_ref::<Object>()).into())
}

/// Checks the receiver's current public uid.
///
/// # Errors
/// Returns the public inactive-effect error or a uid getter failure.
#[wasm_bindgen(js_name = fiberAssertActive)]
pub fn fiber_assert_active(owner: &JsValue) -> Result<(), JsValue> {
    super::browser_fiber::assert_active(owner)
}

/// Refreshes one dependency snapshot through the current reflection service.
///
/// # Errors
/// Propagates reflection and logging failures, retaining availability-error identity.
#[wasm_bindgen(js_name = fiberCheckImpl)]
pub fn fiber_check_impl(owner: &JsValue, name: &JsValue) -> Result<JsValue, JsValue> {
    BrowserLifecycle::check_implementation(owner, name)
}

/// Registers a cleanup-aware effect using the receiver's disposable list.
///
/// # Errors
/// Propagates admission, setup, and list failures unchanged.
#[wasm_bindgen(js_name = fiberEffect)]
pub fn fiber_effect(
    owner: JsValue,
    setup: &JsValue,
    label: JsValue,
    outer: JsValue,
) -> Result<Function, JsValue> {
    super::context_effect(owner, setup, label, outer)
}

/// Returns the receiver's current effect metadata trees.
///
/// # Errors
/// Propagates list and metadata getter failures.
#[wasm_bindgen(js_name = fiberGetEffects)]
pub fn fiber_get_effects(owner: &JsValue) -> Result<Array, JsValue> {
    super::browser_effects::diagnostics(owner)
}

/// Resolves the display name through runtime and parent records.
///
/// # Errors
/// Propagates runtime and parent getter failures unchanged.
#[wasm_bindgen(js_name = fiberName)]
pub fn fiber_name(owner: &JsValue) -> Result<JsValue, JsValue> {
    let mut current = owner.clone();
    loop {
        let runtime = Reflect::get(&current, &"runtime".into())?;
        if !runtime.is_null() && !runtime.is_undefined() {
            let name = Reflect::get(&runtime, &"name".into())?;
            if name.is_truthy() {
                return Ok(name);
            }
        }
        let parent = Reflect::get(&current, &"parent".into())?;
        current = Reflect::get(&parent, &"fiber".into())?;
        let parent = Reflect::get(&current, &"parent".into())?;
        if current == Reflect::get(&parent, &"fiber".into())? {
            return Ok("root".into());
        }
    }
}

pub(super) fn root_face(
    context: &JsValue,
    native: &Arc<crate::Fiber>,
    config: JsValue,
    inject: JsValue,
    runtime: &JsValue,
    custom_prototype: Option<Object>,
    stack: &JsValue,
) -> Result<JsValue, JsValue> {
    let face = custom_prototype
        .or_else(prototype)
        .map_or_else(Object::new, |prototype| Object::create(&prototype));
    for (name, value) in [
        ("parent", context.clone()),
        ("inject", inject),
        ("runtime", runtime.clone()),
        ("uid", 0.into()),
        ("ctx", context.clone()),
        ("config", JsValue::UNDEFINED),
        ("_config", config),
        ("state", 2.into()),
        ("dispose", JsValue::UNDEFINED),
        ("store", Object::create(&Object::from(JsValue::NULL)).into()),
        ("inertia", JsValue::UNDEFINED),
        (
            "_hooks",
            Object::create(&Object::from(JsValue::NULL)).into(),
        ),
        ("_disposables", JsValue::UNDEFINED),
        ("context", context.clone()),
        ("_error", JsValue::UNDEFINED),
        ("_runner", JsValue::UNDEFINED),
        (
            "_store",
            Object::create(&Object::from(JsValue::NULL)).into(),
        ),
    ] {
        super::browser_fiber::data_field(&face, name, value)?;
    }
    native.install_browser_effects(&face)?;
    super::browser_runner::install(&face, runtime, stack)?;
    super::browser_stack::remember_owner(&face, stack)?;
    let core: FaceSlot = super::empty_face_slot();
    *core.lock() = Some(face.clone().into());
    let lifecycle = BrowserLifecycle::new_root(native.clone(), core.clone());
    bind_lifecycle(&face, lifecycle.clone());
    if prototype().is_none() {
        install_root_methods(&face, &lifecycle)?;
    }
    let changed = lifecycle.clone();
    let settled = lifecycle.clone();
    native.observe_browser(crate::fiber::BrowserFiberObserver {
        changed: Arc::new(move |state| changed.phase(state)),
        prepare: Arc::new(|_| {}),
        dispose: Arc::new(move || {
            let owner = core.lock().clone().unwrap_or(JsValue::UNDEFINED);
            let result = BrowserLifecycle::restart(&owner);
            Box::pin(async move {
                JsFuture::from(result.map_err(|error| super::js_anyhow(&error))?)
                    .await
                    .map(|_| ())
                    .map_err(|error| super::js_anyhow(&error))
            })
        }),
        settled: Arc::new(move || settled.settled()),
    });
    let dispose = Function::new_with_args("owner", "return () => owner.restart();")
        .call1(&JsValue::UNDEFINED, &face)?;
    super::browser_fiber::data_field(&face, "dispose", dispose)?;
    Ok(face.into())
}

pub(super) fn install_root_methods(
    face: &Object,
    lifecycle: &BrowserLifecycle,
) -> Result<(), JsValue> {
    for name in [
        "assertActive",
        "effect",
        "getEffects",
        "_execute",
        "_getState",
        "_updateState",
        "_checkImpl",
        "_refresh",
        "_setEpoch",
        "_resolveConfig",
        "_reload",
        "_unload",
        "await",
        "restart",
        "update",
        "name",
    ] {
        let lifecycle = lifecycle.clone();
        let invoke = Closure::wrap(Box::new(
            move |owner: JsValue, args: Array| -> Result<JsValue, JsValue> {
                match name {
                    "assertActive" => fiber_assert_active(&owner).map(|()| JsValue::UNDEFINED),
                    "effect" => {
                        fiber_effect(owner, &args.get(0), args.get(1), args.get(2)).map(Into::into)
                    }
                    "getEffects" => fiber_get_effects(&owner).map(Into::into),
                    "_execute" => super::browser_runner::execute(&owner, &args.get(0)),
                    "_getState" => super::browser_runner::get_state(&owner).map(JsValue::from),
                    "_updateState" => super::browser_runner::update_state(&owner, &args.get(0))
                        .map(|()| JsValue::UNDEFINED),
                    "_checkImpl" => fiber_check_impl(&owner, &args.get(0)),
                    "_refresh" => BrowserLifecycle::refresh(&owner).map(|()| JsValue::UNDEFINED),
                    "_setEpoch" => lifecycle
                        .set_epoch_value(&owner, &args.get(0))
                        .map(|()| JsValue::UNDEFINED),
                    "_resolveConfig" => super::browser_runner::resolve_config(&owner, &args.get(0)),
                    "_reload" => lifecycle.reload(owner).map(Into::into),
                    "_unload" => Ok(lifecycle.unload(owner).into()),
                    "await" => Ok(BrowserLifecycle::wait(owner).into()),
                    "restart" => Ok(BrowserLifecycle::restart(&owner)
                        .unwrap_or_else(|error| Promise::reject(&error))
                        .into()),
                    "update" => BrowserLifecycle::update(&owner, &args.get(0), &args.get(1)),
                    "name" => fiber_name(&owner),
                    _ => unreachable!(),
                }
            },
        )
            as Box<dyn Fn(JsValue, Array) -> Result<JsValue, JsValue>>)
        .into_js_value();
        let body = if name == "effect" {
            "'use strict'; return function effect(execute,label=undefined) { return invoke(this,[execute,label,capture()]); };".to_owned()
        } else {
            format!(
                "'use strict'; return function {name}(...args) {{ return invoke(this,args); }};"
            )
        };
        let function = Function::new_with_args("invoke,capture", &body).call2(
            &JsValue::UNDEFINED,
            &invoke,
            super::browser_stack::builder()?.as_ref(),
        )?;
        let descriptor = if name == "name" {
            object(&[("get", function), ("configurable", true.into())])?
        } else {
            object(&[
                ("value", function),
                ("writable", true.into()),
                ("configurable", true.into()),
            ])?
        };
        Reflect::define_property(face, &name.into(), &descriptor)?;
    }
    Ok(())
}
