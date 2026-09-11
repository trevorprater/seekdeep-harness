//! Browser accessor definitions, mixin ownership, and receiver-driven reflection methods.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Reflect, WeakMap};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use super::{
    WasmContext, browser_registry::method, browser_symbols::get as symbol,
    browser_values as values, object, tracing::Tracer,
};

mod mixin;
mod provider;
use super::browser_values::for_each;

thread_local! {
    static ROOTS: WeakMap = WeakMap::new();
    static PROTOTYPE: RefCell<Option<Object>> = const { RefCell::new(None) };
}

/// Supplies the reflection class prototype shared by browser Context roots.
#[wasm_bindgen(js_name = configureReflectServicePrototype)]
pub fn configure_prototype(prototype: Object) {
    PROTOTYPE.with(|slot| *slot.borrow_mut() = Some(prototype));
}

/// Initializes an independent reflection service and its owned mixin definitions.
///
/// # Errors
/// Propagates field construction, effect admission, and mixin registration failures.
#[wasm_bindgen(js_name = initializeReflectService)]
pub fn initialize_service(instance: &JsValue, context: &JsValue) -> Result<(), JsValue> {
    initialize_fields(
        instance,
        context,
        &Object::create(&Object::from(JsValue::NULL)),
        &Object::create(&Object::from(JsValue::NULL)),
    )?;
    install_mixins(instance)
}

fn initialize_fields(
    instance: &JsValue,
    context: &JsValue,
    store: &JsValue,
    props: &JsValue,
) -> Result<(), JsValue> {
    for (key, value) in [("ctx", context), ("store", store), ("props", props)] {
        values::set(instance, &key.into(), value)?;
    }
    values::define(
        instance,
        &symbol("tracker")?,
        &object(&[("property", "ctx".into()), ("noShadow", true.into())])?.into(),
    )
}

pub(super) fn check_service(props: &JsValue, name: &str) -> Result<(), JsValue> {
    let definition = values::get(props, &name.into())?;
    if definition.is_truthy() {
        let kind = values::get(&values::get(props, &name.into())?, &"type".into())?;
        if kind.as_string().as_deref() != Some("service") {
            let kind = values::template_string(&values::get(
                &values::get(props, &name.into())?,
                &"type".into(),
            )?)?;
            return Err(js_sys::Error::new(&format!(
                "property \"{name}\" is already declared as {kind}"
            ))
            .into());
        }
    } else {
        let existing = values::get(props, &name.into())?;
        if existing.is_null() || existing.is_undefined() {
            values::set(
                props,
                &name.into(),
                &object(&[("type", "service".into())])?.into(),
            )?;
        }
    }
    values::set(
        props,
        &name.into(),
        &object(&[("type", "service".into())])?.into(),
    )
}

pub(super) fn root(context: &WasmContext) -> Result<JsValue, JsValue> {
    let owner = context.root();
    let existing = ROOTS.with(|roots| roots.get(owner.unchecked_ref::<Object>()));
    if !existing.is_undefined() {
        return Ok(existing);
    }
    let prototype = match PROTOTYPE.with(|slot| slot.borrow().clone()) {
        Some(prototype) => prototype,
        None => prototype()?,
    };
    let service = Object::create(&prototype);
    let records = context.inner.browser_services();
    initialize_fields(&service, &owner, &records.store, &records.props)?;
    ROOTS.with(|roots| roots.set(context.root().unchecked_ref::<Object>(), &service));
    Ok(service.into())
}

pub(super) fn install(context: &WasmContext) -> Result<(), JsValue> {
    let service = root(context)?;
    install_mixins(&service)
}

fn install_mixins(service: &JsValue) -> Result<(), JsValue> {
    for (source, names) in [
        (
            "reflect",
            &["get", "set", "provide", "accessor", "mixin"][..],
        ),
        ("fiber", &["runtime", "effect"][..]),
        ("registry", &["inject", "plugin"][..]),
        (
            "events",
            &[
                "on",
                "once",
                "parallel",
                "emit",
                "serial",
                "bail",
                "waterfall",
            ][..],
        ),
    ] {
        let names: Array = names.iter().map(|name| JsValue::from_str(name)).collect();
        method(service, "mixin", &Array::of2(&source.into(), &names))?;
    }
    Ok(())
}

/// Builds receiver-driven method descriptors for the reflection class.
///
/// # Errors
/// Propagates JavaScript descriptor construction failures.
#[wasm_bindgen(js_name = reflectServicePrototype)]
pub fn prototype() -> Result<Object, JsValue> {
    let prototype = Object::new();
    for (name, params, args) in [
        ("get", "name,strict=true", "[name,strict]"),
        ("_getImpl", "name,strict=true", "[name,strict]"),
        ("set", "name,value,error", "[name,value,error]"),
        ("provide", "name,value,check", "[name,value,check]"),
        ("notify", "names,filter=undefined", "[names,filter]"),
        ("accessor", "name,options", "[name,options]"),
        ("mixin", "source,mixins", "[source,mixins]"),
        ("trace", "value", "[value]"),
        ("bind", "callback", "[callback]"),
    ] {
        let invoke = Closure::wrap(Box::new(move |receiver: JsValue, args: Array| {
            invoke(&receiver, name, &args)
        })
            as Box<dyn Fn(JsValue, Array) -> Result<JsValue, JsValue>>)
        .into_js_value();
        let function = Function::new_with_args("invoke", &format!(
            "'use strict'; return ({{ {name}({params}) {{ return invoke(this,{args}); }} }}).{name};"
        )).call1(&JsValue::UNDEFINED, &invoke)?;
        Reflect::define_property(
            &prototype,
            &name.into(),
            &object(&[
                ("value", function),
                ("writable", true.into()),
                ("configurable", true.into()),
            ])?,
        )?;
    }
    Ok(prototype)
}

fn invoke(service: &JsValue, name: &str, args: &Array) -> Result<JsValue, JsValue> {
    match name {
        "accessor" => accessor(service, &args.get(0), &args.get(1)),
        "mixin" => mixin(service, &args.get(0), &args.get(1)),
        "_getImpl" => implementation(service, &args.get(0), &args.get(1)),
        "get" => {
            let context = values::get(service, &"ctx".into())?;
            let record = method(service, "_getImpl", args)?;
            let value = if record.is_null() || record.is_undefined() {
                JsValue::UNDEFINED
            } else {
                values::get(&record, &"value".into())?
            };
            Tracer::new(context).trace(&value)
        }
        "set" => set(service, &args.get(0), &args.get(1)),
        "trace" => Tracer::new(values::get(service, &"ctx".into())?).trace(&args.get(0)),
        "bind" => bind(service, &args.get(0)),
        "notify" => notify(service, &args.get(0), &args.get(1)),
        "provide" => provide(service, &args.get(0), &args.get(1), &args.get(2)),
        _ => unreachable!("reflection prototype has a closed method set"),
    }
}

fn provide(
    service: &JsValue,
    name: &JsValue,
    value: &JsValue,
    check: &JsValue,
) -> Result<JsValue, JsValue> {
    let (owner, name, value, check) = (service.clone(), name.clone(), value.clone(), check.clone());
    let label = label("provide", &name)?;
    let setup = Closure::wrap(Box::new(move || {
        let context = values::get(&owner, &"ctx".into())?;
        let native = super::context_core(&context, &JsValue::UNDEFINED)?;
        let store = values::get(&owner, &"store".into())?;
        let props = values::get(&owner, &"props".into())?;
        if !name.is_string()
            || native.is_undefined()
            || !method(
                &native,
                "reflectionRecordsMatch",
                &Array::of4(&store, &props, &name, &context),
            )?
            .is_truthy()
        {
            return provider::register(&owner, &name, &value, &check);
        }
        method(
            &native,
            "provideResource",
            &Array::of4(&name, &value, &check, &owner),
        )
    }) as Box<dyn Fn() -> Result<JsValue, JsValue>>)
    .into_js_value();
    effect(service, &setup, &label)
}

pub(super) fn notify_waiters(service: &JsValue, name: &str) -> Result<JsValue, JsValue> {
    let fibers = method(service, "notify", &Array::of1(&Array::of1(&name.into())))?;
    let await_fiber =
        Closure::wrap(
            Box::new(move |fiber: JsValue| method(&fiber, "await", &Array::new()))
                as Box<dyn Fn(JsValue) -> Result<JsValue, JsValue>>,
        )
        .into_js_value();
    method(&fibers, "map", &Array::of1(&await_fiber))
}

fn notify(service: &JsValue, names: &JsValue, filter: &JsValue) -> Result<JsValue, JsValue> {
    let filter = if filter.is_undefined() {
        let owner = service.clone();
        Closure::wrap(Box::new(move |context: JsValue, name: JsValue| {
            Ok(
                values::get(&values::get(&context, &symbol("isolate")?)?, &name)?
                    == values::get(
                        &values::get(&values::get(&owner, &"ctx".into())?, &symbol("isolate")?)?,
                        &name,
                    )?,
            )
        })
            as Box<dyn Fn(JsValue, JsValue) -> Result<bool, JsValue>>)
        .into_js_value()
    } else {
        filter.clone()
    };
    let registry = values::get(&values::get(service, &"ctx".into())?, &"registry".into())?;
    let runtimes = method(&registry, "values", &Array::new())?;
    let affected = Array::new();
    for_each(&runtimes, |runtime| {
        for_each(&values::get(&runtime, &"fibers".into())?, |fiber| {
            let mut changed = false;
            for_each(names, |name| {
                if Reflect::has(&values::get(&fiber, &"inject".into())?, &name)?
                    && call(
                        &filter,
                        &Array::of2(&values::get(&fiber, &"ctx".into())?, &name),
                    )?
                    .is_truthy()
                {
                    changed = true;
                    method(&fiber, "_checkImpl", &Array::of1(&name))?;
                }
                Ok(())
            })?;
            if changed {
                method(&fiber, "_refresh", &Array::new())?;
                affected.push(&fiber);
            }
            Ok(())
        })
    })?;
    for_each(names, |name| {
        let context = values::get(service, &"ctx".into())?;
        let receiver = Object::create(context.unchecked_ref::<Object>());
        let (name_for_filter, callback) = (name.clone(), filter.clone());
        let selected = Closure::wrap(Box::new(move |target: JsValue| {
            call(&callback, &Array::of2(&target, &name_for_filter))
        })
            as Box<dyn Fn(JsValue) -> Result<JsValue, JsValue>>)
        .into_js_value();
        values::set(&receiver, &symbol("filter")?, &selected)?;
        let events = values::get(&values::get(service, &"ctx".into())?, &"events".into())?;
        let implementation = method(service, "_getImpl", &Array::of2(&name, &JsValue::FALSE))?;
        let value = if implementation.is_null() || implementation.is_undefined() {
            JsValue::UNDEFINED
        } else {
            values::get(&implementation, &"value".into())?
        };
        method(
            &events,
            "emit",
            &Array::of4(&receiver, &"internal/service".into(), &name, &value),
        )?;
        Ok(())
    })?;
    Ok(affected.into())
}

fn call(callback: &JsValue, args: &Array) -> Result<JsValue, JsValue> {
    let callback = callback
        .clone()
        .dyn_into::<Function>()
        .map_err(|_| js_sys::TypeError::new("callback is not a function"))?;
    Reflect::apply(&callback, &JsValue::UNDEFINED, args)
}

fn implementation(service: &JsValue, name: &JsValue, strict: &JsValue) -> Result<JsValue, JsValue> {
    let context = values::get(service, &"ctx".into())?;
    let key = values::get(&values::get(&context, &symbol("isolate")?)?, name)?;
    let record = if key.is_truthy() {
        values::get(&values::get(service, &"store".into())?, &key)?
    } else {
        key
    };
    if !record.is_truthy() {
        return Ok(JsValue::UNDEFINED);
    }
    if strict.is_truthy()
        && values::get(&values::get(&record, &"fiber".into())?, &"state".into())?.as_f64()
            != Some(2.0)
    {
        return Ok(JsValue::UNDEFINED);
    }
    Ok(record)
}

fn set(service: &JsValue, name: &JsValue, value: &JsValue) -> Result<JsValue, JsValue> {
    let context = values::get(service, &"ctx".into())?;
    let key = values::get(&values::get(&context, &symbol("isolate")?)?, name)?;
    let record = values::get(&values::get(service, &"store".into())?, &key)?;
    if !record.is_truthy() {
        let display = values::template_string(name)?;
        return Err(js_sys::Error::new(&format!(
            "cannot set property \"{display}\" without provide"
        ))
        .into());
    }
    if values::get(&record, &"fiber".into())?
        != values::get(&values::get(service, &"ctx".into())?, &"fiber".into())?
    {
        let display = values::template_string(name)?;
        return Err(js_sys::Error::new(&format!(
            "cannot set property \"{display}\" in multiple fibers"
        ))
        .into());
    }
    values::set(&record, &"value".into(), value)?;
    Ok(JsValue::TRUE)
}

fn accessor(service: &JsValue, name: &JsValue, options: &JsValue) -> Result<JsValue, JsValue> {
    let (owner, name, options) = (service.clone(), name.clone(), options.clone());
    let label = label("accessor", &name)?;
    let setup = Closure::wrap(Box::new(move || {
        let props = values::get(&owner, &"props".into())?;
        if Reflect::has(&props, &name)? {
            let kind = values::get(&values::get(&props, &name)?, &"type".into())?;
            let message = Function::new_with_args(
                "name,kind",
                "return `property \"${name}\" is already declared as ${kind}`;",
            )
            .call2(&JsValue::UNDEFINED, &name, &kind)?;
            return Err(js_sys::Error::new(&message.as_string().unwrap_or_default()).into());
        }
        let definition = object(&[("type", "accessor".into())])?;
        values::spread_into(&definition, &options)?;
        values::set(&props, &name, &definition)?;
        let name = name.clone();
        Ok(Closure::wrap(Box::new(move || {
            Reflect::delete_property(props.unchecked_ref::<Object>(), &name).map(JsValue::from_bool)
        }) as Box<dyn Fn() -> Result<JsValue, JsValue>>)
        .into_js_value())
    }) as Box<dyn Fn() -> Result<JsValue, JsValue>>)
    .into_js_value();
    effect(service, &setup, &label)
}

fn effect(service: &JsValue, setup: &JsValue, label: &JsValue) -> Result<JsValue, JsValue> {
    let context = values::get(service, &"ctx".into())?;
    let fiber = values::get(&context, &"fiber".into())?;
    method(&fiber, "effect", &Array::of2(setup, label))
}

fn bind(service: &JsValue, callback: &JsValue) -> Result<JsValue, JsValue> {
    let owner = service.clone();
    let apply = Closure::wrap(
        Box::new(move |target: Function, receiver: JsValue, args: Array| {
            let receiver = method(&owner, "trace", &Array::of1(&receiver))?;
            let args = traced_arguments(&owner, &args)?;
            Reflect::apply(&target, &receiver, &args)
        }) as Box<dyn Fn(Function, JsValue, Array) -> Result<JsValue, JsValue>>,
    )
    .into_js_value();
    let owner = service.clone();
    let construct = Closure::wrap(Box::new(
        move |target: Function, args: Array, new_target: Function| {
            Reflect::construct_with_new_target(
                &target,
                &traced_arguments(&owner, &args)?,
                &new_target,
            )
        },
    )
        as Box<dyn Fn(Function, Array, Function) -> Result<JsValue, JsValue>>)
    .into_js_value();
    values::proxy(
        callback,
        &object(&[("apply", apply), ("construct", construct)])?,
    )
}

fn traced_arguments(service: &JsValue, args: &Array) -> Result<Array, JsValue> {
    let owner = service.clone();
    let trace =
        Closure::wrap(
            Box::new(move |value: JsValue| method(&owner, "trace", &Array::of1(&value)))
                as Box<dyn Fn(JsValue) -> Result<JsValue, JsValue>>,
        )
        .into_js_value();
    method(args, "map", &Array::of1(&trace)).map(wasm_bindgen::JsCast::unchecked_into)
}

fn label(operation: &str, value: &JsValue) -> Result<JsValue, JsValue> {
    let json = values::get(&js_sys::global(), &"JSON".into())?;
    let value = method(&json, "stringify", &Array::of1(value))?;
    Ok(format!("ctx.{operation}({})", values::template_string(&value)?).into())
}

fn mixin(service: &JsValue, source: &JsValue, mixins: &JsValue) -> Result<JsValue, JsValue> {
    let label = label("mixin", source)?;
    let setup = mixin::setup(service, source, mixins)?;
    effect(service, &setup, &label)
}

fn mixin_options(source: &JsValue, key: &JsValue) -> Result<JsValue, JsValue> {
    let (read_source, read_key) = (source.clone(), key.clone());
    let read = receiver_function(move |context, args| {
        let service = values::get(&context, &read_source)?;
        if service.is_null() || service.is_undefined() {
            return Ok(service);
        }
        let receiver = mixin_receiver(&args.get(0), &service)?;
        let value = super::get_with_receiver(&service, &read_key, &receiver)?;
        if value.is_function() {
            method(&value, "bind", &Array::of1(&receiver))
        } else {
            Ok(value)
        }
    })?;
    let (write_source, write_key) = (source.clone(), key.clone());
    let write = receiver_function(move |context, args| {
        let service = values::get(&context, &write_source)?;
        let receiver = mixin_receiver(&args.get(1), &service)?;
        Reflect::set_with_receiver(&service, &write_key, &args.get(0), &receiver)
            .map(JsValue::from_bool)
    })?;
    object(&[("get", read), ("set", write)]).map(Into::into)
}

fn mixin_receiver(receiver: &JsValue, service: &JsValue) -> Result<JsValue, JsValue> {
    if receiver.is_truthy() {
        super::tracing::with_props(receiver, service)
    } else {
        Ok(service.clone())
    }
}

fn receiver_function(
    body: impl Fn(JsValue, Array) -> Result<JsValue, JsValue> + 'static,
) -> Result<JsValue, JsValue> {
    let invoke =
        Closure::wrap(Box::new(body) as Box<dyn Fn(JsValue, Array) -> Result<JsValue, JsValue>>)
            .into_js_value();
    Function::new_with_args(
        "invoke",
        "'use strict'; return function(...args) { return invoke(this,args); };",
    )
    .call1(&JsValue::UNDEFINED, &invoke)
}
