//! `EventsService` methods use their receiver and support independent service construction.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Promise, Proxy, Reflect, Symbol, WeakMap};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};

use super::{bailed, call, dispatch_update, object, receiver_function, register_update};

thread_local! {
    static PROTOTYPE: RefCell<Option<Object>> = const { RefCell::new(None) };
    static CONTEXT_DISPATCH: WeakMap = WeakMap::new();
    static SERVICE_DISPATCH: WeakMap = WeakMap::new();
}

pub(super) fn configure(prototype: Object) {
    PROTOTYPE.with(|slot| *slot.borrow_mut() = Some(prototype));
}

pub(super) fn remember_context(face: &JsValue, context: super::WasmContext) {
    let dispatch = Closure::wrap(
        Box::new(move |service: JsValue, mode: JsValue, args: Array| {
            super::dispatch_service(Some(&context), &service, &mode, &args)
        }) as Box<dyn Fn(JsValue, JsValue, Array) -> Result<JsValue, JsValue>>,
    )
    .into_js_value();
    CONTEXT_DISPATCH.with(|map| {
        map.set(face.unchecked_ref::<Object>(), &dispatch);
    });
}

pub(super) fn remember_service(service: &JsValue, owner: &JsValue) {
    let dispatch = CONTEXT_DISPATCH.with(|map| map.get(owner.unchecked_ref::<Object>()));
    if dispatch.is_function() {
        SERVICE_DISPATCH.with(|map| {
            map.set(service.unchecked_ref::<Object>(), &dispatch);
        });
    }
}

pub(super) fn prototype() -> Result<Object, JsValue> {
    let prototype = Object::new();
    for name in [
        "dispatch",
        "parallel",
        "emit",
        "serial",
        "bail",
        "waterfall",
        "register",
        "unregister",
        "on",
        "once",
    ] {
        let callback = Closure::wrap(Box::new(move |service: JsValue, args: Array| {
            let result = invoke(&service, name, &args);
            if matches!(name, "serial" | "parallel") {
                result.or_else(|error| Ok(Promise::reject(&error).into()))
            } else {
                result
            }
        })
            as Box<dyn Fn(JsValue, Array) -> Result<JsValue, JsValue>>)
        .into_js_value();
        let (parameters, args) = match name {
            "dispatch" => ("type,args", "[type,args]"),
            "register" => (
                "label,hooks,callback,options",
                "[label,hooks,callback,options]",
            ),
            "unregister" => ("hooks,callback", "[hooks,callback]"),
            "on" | "once" => ("name,listener,options", "[name,listener,options]"),
            _ => ("...args", "args"),
        };
        let method = if matches!(name, "parallel" | "serial") {
            let target = Function::new_no_args(&format!(
                "'use strict'; return ({{ async {name}(...args) {{}} }}).{name};"
            ))
            .call0(&JsValue::UNDEFINED)?;
            let handler = Function::new_with_args(
                "invoke",
                "return { apply(_target, receiver, args) { return invoke(receiver,args); } };",
            )
            .call1(&JsValue::UNDEFINED, &callback)?
            .dyn_into::<Object>()?;
            Proxy::new(&target, &handler).into()
        } else {
            Function::new_with_args(
                "invoke",
                &format!(
                    "'use strict'; return ({{ {name}({parameters}) {{ return invoke(this,{args}); }} }}).{name};"
                ),
            )
            .call1(&JsValue::UNDEFINED, &callback)?
        };
        Reflect::define_property(
            &prototype,
            &name.into(),
            &object(&[
                ("value", method),
                ("writable", JsValue::TRUE),
                ("configurable", JsValue::TRUE),
            ])?,
        )?;
    }
    Ok(prototype)
}

pub(super) fn create(owner: &JsValue, custom_prototype: Option<Object>) -> Result<Object, JsValue> {
    let prototype = match custom_prototype.or_else(|| PROTOTYPE.with(|slot| slot.borrow().clone()))
    {
        Some(prototype) => prototype,
        None => prototype()?,
    };
    let service = Object::create(&prototype);
    for (name, value) in [("ctx", owner.clone()), ("_hooks", Object::new().into())] {
        Reflect::define_property(
            &service,
            &name.into(),
            &object(&[
                ("value", value),
                ("writable", JsValue::TRUE),
                ("enumerable", JsValue::TRUE),
                ("configurable", JsValue::TRUE),
            ])?,
        )?;
    }
    Reflect::define_property(
        &service,
        &super::super::browser_symbols::get("tracker")?,
        &object(&[
            (
                "value",
                object(&[("property", "ctx".into()), ("noShadow", JsValue::TRUE)])?.into(),
            ),
            ("writable", JsValue::TRUE),
        ])?,
    )?;
    remember_service(&service, owner);
    let listener = receiver_function(|receiver, args| {
        if args.get(0).as_string().as_deref() != Some("internal/update")
            || Reflect::get(&args.get(2), &"global".into())?.is_truthy()
        {
            return Ok(JsValue::UNDEFINED);
        }
        register_update(&receiver, &args.get(1), &args.get(2))
    })?;
    method(
        &service,
        "on",
        &Array::of2(&"internal/listener".into(), &listener),
    )?;
    let update = receiver_function(|receiver, args| dispatch_update(&receiver, &args))?;
    method(
        &service,
        "on",
        &Array::of3(
            &"internal/update".into(),
            &update,
            &object(&[("global", JsValue::TRUE), ("prepend", JsValue::TRUE)])?.into(),
        ),
    )?;
    Ok(service)
}

pub(super) fn method(receiver: &JsValue, name: &str, args: &Array) -> Result<JsValue, JsValue> {
    let method = Reflect::get(receiver, &name.into())?
        .dyn_into::<Function>()
        .map_err(|_| js_sys::TypeError::new(&format!("{name} is not a function")))?;
    Reflect::apply(&method, receiver, args)
}

fn invoke(service: &JsValue, operation: &str, args: &Array) -> Result<JsValue, JsValue> {
    match operation {
        "on" => on(service, &args.get(0), &args.get(1), args.get(2)),
        "once" => once(service, &args.get(0), &args.get(1), &args.get(2)),
        "register" => register(
            service,
            &args.get(0),
            &args.get(1),
            &args.get(2),
            &args.get(3),
        ),
        "unregister" => super::unregister(&args.get(0), &args.get(1)),
        "dispatch" => {
            let native = SERVICE_DISPATCH.with(|map| map.get(service.unchecked_ref::<Object>()));
            if native.is_function() {
                Reflect::apply(
                    native.unchecked_ref::<Function>(),
                    &JsValue::UNDEFINED,
                    &Array::of3(service, &args.get(0), &args.get(1)),
                )
            } else {
                super::dispatch_service(
                    None,
                    service,
                    &args.get(0),
                    &args.get(1).dyn_into::<Array>()?,
                )
            }
        }
        "emit" => {
            let callbacks = method(service, "dispatch", &Array::of2(&"emit".into(), args))?;
            let args = args.clone();
            let callback = Closure::wrap(Box::new(move |callback: JsValue| call(&callback, &args))
                as Box<dyn Fn(JsValue) -> Result<JsValue, JsValue>>)
            .into_js_value();
            method(&callbacks, "map", &Array::of1(&callback))?;
            Ok(JsValue::UNDEFINED)
        }
        "bail" => {
            let callbacks = method(service, "dispatch", &Array::of2(&"bail".into(), args))?;
            let cursor = Cursor::new(&callbacks)?;
            while let Some(callback) = cursor.step()? {
                let value = match call(&callback, args) {
                    Ok(value) => value,
                    Err(error) => {
                        let _ = cursor.close();
                        return Err(error);
                    }
                };
                if bailed(&value) {
                    cursor.close()?;
                    return Ok(value);
                }
            }
            Ok(JsValue::UNDEFINED)
        }
        "serial" => {
            let callbacks = method(service, "dispatch", &Array::of2(&"serial".into(), args))?;
            serial(Cursor::new(&callbacks)?, args.clone()).map(Into::into)
        }
        "parallel" => {
            let callbacks = method(service, "dispatch", &Array::of2(&"emit".into(), args))?;
            let args = args.clone();
            let callback = Closure::wrap(Box::new(move |callback: JsValue| -> JsValue {
                Promise::new(&mut |resolve, reject| {
                    let (settle, value) = match call(&callback, &args) {
                        Ok(value) => (resolve, value),
                        Err(error) => (reject, error),
                    };
                    let _ = Reflect::apply(&settle, &JsValue::UNDEFINED, &Array::of1(&value));
                })
                .into()
            }) as Box<dyn Fn(JsValue) -> JsValue>)
            .into_js_value();
            let target =
                Function::new_no_args("return async callback => {};").call0(&JsValue::UNDEFINED)?;
            let handler = Function::new_with_args(
                "invoke",
                "return { apply(_target, _receiver, args) { return invoke(args[0]); } };",
            )
            .call1(&JsValue::UNDEFINED, &callback)?
            .dyn_into::<Object>()?;
            let mapper = Proxy::new(&target, &handler);
            let pending = method(&callbacks, "map", &Array::of1(&mapper))?;
            super::parallel_results(&pending).map(Into::into)
        }
        "waterfall" => {
            let callbacks = method(service, "dispatch", &Array::of2(&"waterfall".into(), args))?;
            super::waterfall_value(&callbacks, args)
        }
        _ => Err(js_sys::TypeError::new("unknown event service method").into()),
    }
}

fn on(
    service: &JsValue,
    name: &JsValue,
    listener: &JsValue,
    options: JsValue,
) -> Result<JsValue, JsValue> {
    let options = if options.is_object() || options.is_null() {
        options
    } else {
        object(&[("prepend", options)])?.into()
    };
    let owner = Reflect::get(service, &"ctx".into())?;
    method(
        &Reflect::get(&owner, &"fiber".into())?,
        "assertActive",
        &Array::new(),
    )?;
    let listener = method(
        &Reflect::get(&owner, &"reflect".into())?,
        "bind",
        &Array::of1(listener),
    )?;
    let interception = Array::of4(&owner, &"internal/listener".into(), name, &listener);
    interception.push(&options);
    let intercepted = method(service, "bail", &interception)?;
    if intercepted.is_truthy() {
        return Ok(intercepted);
    }
    let table = Reflect::get(service, &"_hooks".into())?;
    let mut hooks = Reflect::get(&table, name)?;
    if !hooks.is_truthy() {
        hooks = Array::new().into();
        if !Reflect::set(&table, name, &hooks)? {
            return Err(js_sys::TypeError::new("Cannot assign event hook list").into());
        }
    }
    let printed = if name.is_string() {
        method(
            &Reflect::get(&js_sys::global(), &"JSON".into())?,
            "stringify",
            &Array::of1(name),
        )?
    } else {
        let boxed = super::super::boxed_object(name);
        let to_string = Reflect::get(&boxed, &"toString".into())?.dyn_into::<Function>()?;
        Reflect::apply(&to_string, name, &Array::new())?
    };
    let label = Function::new_with_args("value", "return `ctx.on(${value})`;")
        .call1(&JsValue::UNDEFINED, &printed)?;
    method(
        service,
        "register",
        &Array::of4(&label, &hooks, &listener, &options),
    )
}

fn register(
    service: &JsValue,
    label: &JsValue,
    hooks: &JsValue,
    callback: &JsValue,
    options: &JsValue,
) -> Result<JsValue, JsValue> {
    let push = if Reflect::get(options, &"prepend".into())?.is_truthy() {
        "unshift"
    } else {
        "push"
    };
    let owner = Reflect::get(service, &"ctx".into())?;
    let fiber = Reflect::get(&owner, &"fiber".into())?;
    let (service, hooks, callback, options) = (
        service.clone(),
        hooks.clone(),
        callback.clone(),
        options.clone(),
    );
    let setup = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let owner = Reflect::get(&service, &"ctx".into())?;
        super::push_record(&hooks, &owner, &callback, &options, push)?;
        let (service, hooks, callback) = (service.clone(), hooks.clone(), callback.clone());
        Ok(Closure::wrap(Box::new(move || {
            method(&service, "unregister", &Array::of2(&hooks, &callback))
        }) as Box<dyn Fn() -> Result<JsValue, JsValue>>)
        .into_js_value())
    }) as Box<dyn Fn() -> Result<JsValue, JsValue>>)
    .into_js_value();
    method(&fiber, "effect", &Array::of2(&setup, label))
}

fn once(
    service: &JsValue,
    name: &JsValue,
    listener: &JsValue,
    options: &JsValue,
) -> Result<JsValue, JsValue> {
    let slot = Array::new();
    let (stored, listener) = (slot.clone(), listener.clone());
    let callback = receiver_function(move |receiver, args| {
        if stored.length() == 0 {
            return Err(js_sys::ReferenceError::new(
                "Cannot access 'dispose' before initialization",
            )
            .into());
        }
        call(&stored.get(0), &Array::new())?;
        method(&listener, "apply", &Array::of2(&receiver, &args))
    })?;
    let dispose = method(service, "on", &Array::of3(name, &callback, options))?;
    slot.push(&dispose);
    Ok(dispose)
}

#[derive(Clone)]
struct Cursor {
    iterator: JsValue,
    next: Function,
}

impl Cursor {
    fn new(value: &JsValue) -> Result<Self, JsValue> {
        let iterator = Reflect::get(value, &Symbol::iterator())?
            .dyn_into::<Function>()
            .map_err(|_| js_sys::TypeError::new("callbacks is not iterable"))?;
        let iterator = Reflect::apply(&iterator, value, &Array::new())?;
        let next = Reflect::get(&iterator, &"next".into())?.dyn_into::<Function>()?;
        Ok(Self { iterator, next })
    }
    fn step(&self) -> Result<Option<JsValue>, JsValue> {
        let result = Reflect::apply(&self.next, &self.iterator, &Array::new())?;
        if !result.is_object() && !result.is_function() {
            return Err(js_sys::TypeError::new("iterator result is not an object").into());
        }
        if Reflect::get(&result, &"done".into())?.is_truthy() {
            Ok(None)
        } else {
            Reflect::get(&result, &"value".into()).map(Some)
        }
    }
    fn close(&self) -> Result<(), JsValue> {
        let close = Reflect::get(&self.iterator, &"return".into())?;
        if close.is_null() || close.is_undefined() {
            return Ok(());
        }
        let result = Reflect::apply(
            &close.dyn_into::<Function>()?,
            &self.iterator,
            &Array::new(),
        )?;
        if result.is_object() || result.is_function() {
            Ok(())
        } else {
            Err(js_sys::TypeError::new("iterator return is not an object").into())
        }
    }
}

fn serial(cursor: Cursor, args: Array) -> Result<Promise, JsValue> {
    let Some(callback) = cursor.step()? else {
        return Ok(Promise::resolve(&JsValue::UNDEFINED));
    };
    let result = match call(&callback, &args) {
        Ok(value) => value,
        Err(error) => {
            let _ = cursor.close();
            return Ok(Promise::reject(&error));
        }
    };
    let rejected_cursor = cursor.clone();
    let fulfilled = Closure::wrap(Box::new(move |value: JsValue| -> Result<JsValue, JsValue> {
        if bailed(&value) {
            cursor.close()?;
            Ok(value)
        } else {
            serial(cursor.clone(), args.clone()).map(Into::into)
        }
    }) as Box<dyn Fn(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value();
    let rejected = Closure::wrap(Box::new(move |error: JsValue| -> Result<JsValue, JsValue> {
        let _ = rejected_cursor.close();
        Err(error)
    }) as Box<dyn Fn(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value();
    method(
        &Promise::resolve(&result),
        "then",
        &Array::of2(&fulfilled, &rejected),
    )
    .map(JsValue::unchecked_into)
}
