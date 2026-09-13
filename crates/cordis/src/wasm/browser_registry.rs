//! Browser callback identities, shared runtime records, and registry inspection.

use std::cell::RefCell;

use js_sys::{Array, Function, Map, Object, Reflect, WeakMap};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use super::{object, set};

thread_local! {
    static ROOTS: WeakMap = WeakMap::new();
    static FIBER_REMOVERS: WeakMap = WeakMap::new();
    static FIBER_RUNTIMES: WeakMap = WeakMap::new();
    static PROTOTYPE: RefCell<Option<Object>> = const { RefCell::new(None) };
}

pub(super) fn method(receiver: &JsValue, name: &str, args: &Array) -> Result<JsValue, JsValue> {
    let function = Reflect::get(receiver, &name.into())?
        .dyn_into::<Function>()
        .map_err(|_| js_sys::TypeError::new(&format!("{name} is not a function")))?;
    Reflect::apply(&function, receiver, args)
}

/// Supplies the public registry class prototype used by root Contexts.
#[wasm_bindgen(js_name = configureRegistryServicePrototype)]
pub fn configure_registry_service_prototype(prototype: Object) {
    PROTOTYPE.with(|slot| *slot.borrow_mut() = Some(prototype));
}

/// Creates an independent registry using the given Context for new plugin owners.
///
/// # Errors
/// Propagates property construction failures.
#[wasm_bindgen(js_name = createRegistryService)]
pub fn create_registry_service(
    context: &JsValue,
    prototype: Option<Object>,
) -> Result<Object, JsValue> {
    let prototype = match prototype.or_else(|| PROTOTYPE.with(|slot| slot.borrow().clone())) {
        Some(prototype) => prototype,
        None => registry_service_prototype()?,
    };
    let service = Object::create(&prototype);
    for (name, value) in [
        ("ctx", context.clone()),
        ("_counter", 0.into()),
        ("_internal", Map::new().into()),
    ] {
        set(&service, name, &value)?;
    }
    Reflect::define_property(
        &service,
        &super::browser_symbols::get("tracker")?,
        &object(&[
            (
                "value",
                object(&[("property", "ctx".into()), ("noShadow", true.into())])?.into(),
            ),
            ("writable", true.into()),
        ])?,
    )?;
    Ok(service)
}

pub(super) fn root(context: &JsValue) -> Result<JsValue, JsValue> {
    let cached = ROOTS.with(|roots| roots.get(context.unchecked_ref::<Object>()));
    if !cached.is_undefined() {
        return Ok(cached);
    }
    let service = create_registry_service(context, None)?;
    ROOTS.with(|roots| roots.set(context.unchecked_ref::<Object>(), &service));
    Ok(service.into())
}

/// Builds receiver-driven descriptors for the public registry class.
///
/// # Errors
/// Propagates JavaScript descriptor construction failures.
#[wasm_bindgen(js_name = registryServicePrototype)]
pub fn registry_service_prototype() -> Result<Object, JsValue> {
    let prototype = Object::new();
    for name in [
        "counter", "size", "resolve", "get", "has", "delete", "keys", "values", "entries",
        "forEach", "inject", "plugin",
    ] {
        let invoke = Closure::wrap(Box::new(move |receiver: JsValue, args: Array| {
            invoke(&receiver, name, &args)
        })
            as Box<dyn Fn(JsValue, Array) -> Result<JsValue, JsValue>>)
        .into_js_value();
        let (parameters, args) = match name {
            "resolve" | "get" | "has" | "delete" => ("plugin", "[plugin]"),
            "forEach" => ("callback", "[callback]"),
            "inject" => ("inject,callback", "[inject,callback]"),
            "plugin" => (
                "plugin,config,getOuterStack=capture()",
                "[plugin,config,getOuterStack]",
            ),
            _ => ("", "[]"),
        };
        let getter = matches!(name, "counter" | "size");
        let function = Function::new_with_args("invoke,capture", &format!(
            "'use strict'; return ({{ {name}({parameters}) {{ return invoke(this,{args}); }} }}).{name};"
        )).call2(&JsValue::UNDEFINED, &invoke, super::browser_stack::builder()?.as_ref())?;
        if getter {
            Reflect::define_property(
                function.unchecked_ref::<Object>(),
                &"name".into(),
                &object(&[
                    ("value", format!("get {name}").into()),
                    ("configurable", true.into()),
                ])?,
            )?;
        }
        let descriptor = if getter {
            object(&[("get", function), ("configurable", true.into())])?
        } else {
            object(&[
                ("value", function),
                ("configurable", true.into()),
                ("writable", true.into()),
            ])?
        };
        Reflect::define_property(&prototype, &name.into(), &descriptor)?;
    }
    Ok(prototype)
}

fn resolve(plugin: &JsValue) -> JsValue {
    if plugin.is_function() {
        return plugin.clone();
    }
    if !plugin.is_null() && plugin.is_object() {
        let result = (|| {
            if Reflect::get(plugin, &"apply".into())?.is_function() {
                Reflect::get(plugin, &"apply".into())
            } else {
                Ok(JsValue::UNDEFINED)
            }
        })();
        return result.unwrap_or(JsValue::UNDEFINED);
    }
    JsValue::UNDEFINED
}

fn invoke(service: &JsValue, name: &str, args: &Array) -> Result<JsValue, JsValue> {
    if name == "resolve" {
        return Ok(resolve(&args.get(0)));
    }
    if name == "counter" {
        let current = Reflect::get(service, &"_counter".into())?;
        let next = Function::new_with_args("value", "return ++value;")
            .call1(&JsValue::UNDEFINED, &current)?;
        super::browser_values::set(service, &"_counter".into(), &next)?;
        return Ok(next);
    }
    if name == "inject" {
        let callback = args.get(1);
        let plugin = object(&[
            ("inject", args.get(0)),
            ("apply", callback.clone()),
            ("name", Reflect::get(&callback, &"name".into())?),
        ])?;
        return method(service, "plugin", &Array::of1(&plugin));
    }
    if name == "plugin" {
        return plugin(service, &args.get(0), &args.get(1), &args.get(2));
    }
    if name == "size" {
        let internal = Reflect::get(service, &"_internal".into())?;
        return Reflect::get(&internal, &"size".into());
    }
    if matches!(name, "keys" | "values" | "entries" | "forEach") {
        let internal = Reflect::get(service, &"_internal".into())?;
        return method(&internal, name, args);
    }
    let key = method(service, "resolve", &Array::of1(&args.get(0)))?;
    if name == "has" {
        return Ok(JsValue::from_bool(
            key.is_truthy()
                && method(
                    &Reflect::get(service, &"_internal".into())?,
                    "has",
                    &Array::of1(&key),
                )?
                .is_truthy(),
        ));
    }
    let runtime = if key.is_truthy() {
        method(
            &Reflect::get(service, &"_internal".into())?,
            "get",
            &Array::of1(&key),
        )?
    } else {
        key.clone()
    };
    if name == "get" {
        return Ok(runtime);
    }
    if !runtime.is_truthy() {
        return Ok(JsValue::UNDEFINED);
    }
    method(
        &Reflect::get(service, &"_internal".into())?,
        "delete",
        &Array::of1(&key),
    )?;
    let fibers = Reflect::get(&runtime, &"fibers".into())?;
    super::browser_values::for_each(&fibers, |fiber| {
        method(&fiber, "dispose", &Array::new()).map(|_| ())
    })?;
    Ok(runtime)
}

fn plugin(
    service: &JsValue,
    descriptor: &JsValue,
    config: &JsValue,
    stack: &JsValue,
) -> Result<JsValue, JsValue> {
    let callback = method(service, "resolve", &Array::of1(descriptor))?;
    if !callback.is_truthy() {
        return Err(js_sys::Error::new(&format!(
            "invalid plugin, expect function or object with an \"apply\" method, received {}",
            descriptor.js_typeof().as_string().unwrap_or_default(),
        ))
        .into());
    }
    let context = Reflect::get(service, &"ctx".into())?;
    method(
        &Reflect::get(&context, &"fiber".into())?,
        "assertActive",
        &Array::new(),
    )?;
    let internal = Reflect::get(service, &"_internal".into())?;
    let mut runtime = method(&internal, "get", &Array::of1(&callback))?;
    if !runtime.is_truthy() {
        let name = Reflect::get(descriptor, &"name".into())?;
        runtime = object(&[
            (
                "name",
                if name.as_string().as_deref() == Some("apply") {
                    JsValue::UNDEFINED
                } else {
                    name
                },
            ),
            ("callback", callback),
            ("fibers", super::update_hooks::disposable_list()?),
            ("Config", Reflect::get(descriptor, &"Config".into())?),
        ])?
        .into();
        let callback = Reflect::get(&runtime, &"callback".into())?;
        method(
            &Reflect::get(service, &"_internal".into())?,
            "set",
            &Array::of2(&callback, &runtime),
        )?;
    }
    let inject = resolve_inject(
        &Reflect::get(descriptor, &"inject".into())?,
        JsValue::UNDEFINED,
    )?;
    let native = Reflect::get(&context, &"__seekdeepContext".into())?;
    let args = Array::of4(&runtime, &inject, config, &context);
    args.push(stack);
    args.push(&super::browser_fiber_api::prototype().map_or(JsValue::UNDEFINED, Into::into));
    method(&native, "mountRuntime", &args)
}

/// Normalizes array, object, and class-inherited dependency declarations.
///
/// # Errors
/// Propagates prototype, key, and value getter failures unchanged.
#[wasm_bindgen(js_name = resolveInject)]
pub fn resolve_inject(inject: &JsValue, result: JsValue) -> Result<JsValue, JsValue> {
    let result = if result.is_undefined() {
        Object::create(&Object::from(JsValue::NULL)).into()
    } else {
        result
    };
    if !inject.is_truthy() {
        return Ok(result);
    }
    if Array::is_array(inject) {
        super::browser_values::for_each(inject, |name| {
            super::browser_values::set(&result, &name, &JsValue::NULL)
        })?;
    } else {
        if Reflect::has(inject, &super::browser_symbols::get("checkProto")?)? {
            let parent = Object::get_prototype_of(inject.unchecked_ref::<Object>());
            let inherited = resolve_inject(&parent, JsValue::UNDEFINED)?;
            super::browser_values::assign(&result, &Array::of1(&inherited))?;
        }
        for name in Object::keys(inject.unchecked_ref::<Object>()).iter() {
            let value = Reflect::get(inject, &name)?;
            super::browser_values::set(
                &result,
                &name,
                &if value.is_null() || value.is_undefined() {
                    JsValue::NULL
                } else {
                    value
                },
            )?;
        }
    }
    Ok(result)
}

pub(super) fn attach(runtime: &JsValue, fiber: &JsValue) -> Result<(), JsValue> {
    let remove = method(
        &Reflect::get(runtime, &"fibers".into())?,
        "push",
        &Array::of1(fiber),
    )?;
    FIBER_REMOVERS.with(|entries| entries.set(fiber.unchecked_ref::<Object>(), &remove));
    FIBER_RUNTIMES.with(|entries| entries.set(fiber.unchecked_ref::<Object>(), runtime));
    Ok(())
}

pub(super) fn inherit_intercepts(
    parent: &JsValue,
    child: &JsValue,
    inject: &JsValue,
) -> Result<(), JsValue> {
    let entries = Object::entries(inject.unchecked_ref::<Object>());
    if entries.length() == 0 {
        return Ok(());
    }
    let key = super::browser_symbols::context_key("intercept")?;
    let inherited = Reflect::get(parent, &super::browser_symbols::context_key("intercept")?)?;
    let intercepts = Object::create(inherited.unchecked_ref::<Object>());
    super::browser_values::set(child, &key, &intercepts)?;
    for entry in entries.iter() {
        let entry = entry.unchecked_into::<Array>();
        let name = entry.get(0);
        let value = entry.get(1);
        if !value.is_null() && !value.is_undefined() {
            let intercepts =
                Reflect::get(child, &super::browser_symbols::context_key("intercept")?)?;
            super::browser_values::set(&intercepts, &name, &value)?;
        }
    }
    Ok(())
}

pub(super) fn tracks(fiber: &JsValue) -> Result<bool, JsValue> {
    let context = Reflect::get(fiber, &"ctx".into())?;
    let registry = Reflect::get(&context, &"registry".into())?;
    let runtimes = method(&registry, "values", &Array::new())?;
    let runtimes = super::update_hooks::spread(&runtimes)?;
    for runtime in runtimes.iter() {
        let fibers = Reflect::get(&runtime, &"fibers".into())?;
        for candidate in super::update_hooks::spread(&fibers)?.iter() {
            if candidate == *fiber {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

pub(super) fn withdraw(fiber: &JsValue) -> Result<(), JsValue> {
    let context = Reflect::get(fiber, &"ctx".into())?;
    let registry = Reflect::get(&context, &"registry".into())?;
    let runtime = FIBER_RUNTIMES.with(|entries| entries.get(fiber.unchecked_ref::<Object>()));
    let callback = Reflect::get(&runtime, &"callback".into())?;
    if method(&registry, "has", &Array::of1(&callback))?.is_truthy() {
        let remove = FIBER_REMOVERS.with(|entries| entries.get(fiber.unchecked_ref::<Object>()));
        Reflect::apply(
            remove.unchecked_ref::<Function>(),
            &JsValue::UNDEFINED,
            &Array::new(),
        )?;
        let fibers = Reflect::get(&runtime, &"fibers".into())?;
        if !Reflect::get(&fibers, &"length".into())?.is_truthy() {
            method(&registry, "delete", &Array::of1(&callback))?;
        }
    }
    Ok(())
}
