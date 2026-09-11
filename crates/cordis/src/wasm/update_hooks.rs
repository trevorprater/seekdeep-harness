//! Browser `DisposableList` identity, mutable storage, and reverse snapshots.

use std::cell::RefCell;

use js_sys::{Array, Function, Map, Object, Reflect, Symbol, WeakMap};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use super::{browser_registry::method, object, set};

thread_local! {
    static PROTOTYPE: RefCell<Option<Object>> = const { RefCell::new(None) };
}

/// Supplies the public class prototype for browser disposable lists.
#[wasm_bindgen(js_name = configureDisposableListPrototype)]
pub fn configure_disposable_list_prototype(prototype: Object) {
    PROTOTYPE.with(|slot| *slot.borrow_mut() = Some(prototype));
}

/// Creates independent serial, entry, and identity storage.
///
/// # Errors
/// Propagates property construction failures.
#[wasm_bindgen(js_name = createDisposableList)]
pub fn create_disposable_list(prototype: Option<Object>) -> Result<Object, JsValue> {
    let prototype = match prototype.or_else(|| PROTOTYPE.with(|slot| slot.borrow().clone())) {
        Some(prototype) => prototype,
        None => disposable_list_prototype()?,
    };
    let list = Object::create(&prototype);
    set(&list, "sn", &0.into())?;
    set(&list, "map", &Map::new())?;
    set(&list, "weak", &WeakMap::new())?;
    Ok(list)
}

pub(super) fn disposable_list() -> Result<JsValue, JsValue> {
    create_disposable_list(None).map(Into::into)
}

/// Builds receiver-driven methods for the public `DisposableList` class.
///
/// # Errors
/// Propagates descriptor construction failures.
#[wasm_bindgen(js_name = disposableListPrototype)]
pub fn disposable_list_prototype() -> Result<Object, JsValue> {
    let prototype = Object::new();
    for (name, key) in [
        ("push", "push".into()),
        ("delete", "delete".into()),
        ("clear", "clear".into()),
        ("length", "length".into()),
        ("iterator", Symbol::iterator().into()),
        ("inspect", Symbol::for_("nodejs.util.inspect.custom").into()),
    ] {
        let callback =
            Closure::wrap(
                Box::new(move |owner: JsValue, args: Array| invoke(&owner, name, &args))
                    as Box<dyn Fn(JsValue, Array) -> Result<JsValue, JsValue>>,
            )
            .into_js_value();
        let function = if name == "length" {
            Function::new_with_args("invoke", "return Object.getOwnPropertyDescriptor({ get length() { return invoke(this,[]); } }, 'length').get;")
                .call1(&JsValue::UNDEFINED, &callback)?
        } else {
            let (parameters, args) = if matches!(name, "push" | "delete") {
                ("value", "[value]")
            } else {
                ("", "[]")
            };
            Function::new_with_args("invoke,key", &format!("'use strict'; return ({{ [key]({parameters}) {{ return invoke(this,{args}); }} }})[key];"))
                .call2(&JsValue::UNDEFINED, &callback, &key)?
        };
        let descriptor = if name == "length" {
            object(&[("get", function), ("configurable", true.into())])?
        } else {
            object(&[
                ("value", function),
                ("writable", true.into()),
                ("configurable", true.into()),
            ])?
        };
        Reflect::define_property(&prototype, &key, &descriptor)?;
    }
    Ok(prototype)
}

fn invoke(owner: &JsValue, name: &str, args: &Array) -> Result<JsValue, JsValue> {
    match name {
        "push" => {
            let serial = Reflect::get(owner, &"sn".into())?;
            let serial = Function::new_with_args("value", "return ++value;")
                .call1(&JsValue::UNDEFINED, &serial)?;
            if !Reflect::set(owner, &"sn".into(), &serial)? {
                return Err(
                    js_sys::TypeError::new("Cannot assign to read only property 'sn'").into(),
                );
            }
            let value = args.get(0);
            method(
                &Reflect::get(owner, &"map".into())?,
                "set",
                &Array::of2(&serial, &value),
            )?;
            method(
                &Reflect::get(owner, &"weak".into())?,
                "set",
                &Array::of2(&value, &serial),
            )?;
            let owner = owner.clone();
            let remove = Closure::wrap(Box::new(move || {
                method(
                    &Reflect::get(&owner, &"map".into())?,
                    "delete",
                    &Array::of1(&serial),
                )
            }) as Box<dyn Fn() -> Result<JsValue, JsValue>>)
            .into_js_value();
            Function::new_with_args("invoke", "return () => invoke();")
                .call1(&JsValue::UNDEFINED, &remove)
        }
        "delete" => {
            let serial = method(
                &Reflect::get(owner, &"weak".into())?,
                "get",
                &Array::of1(&args.get(0)),
            )?;
            if !serial.is_truthy() {
                return Ok(JsValue::FALSE);
            }
            method(
                &Reflect::get(owner, &"map".into())?,
                "delete",
                &Array::of1(&serial),
            )
        }
        "clear" => {
            let iterator = method(
                &Reflect::get(owner, &"map".into())?,
                "values",
                &Array::new(),
            )?;
            let values = spread(&iterator)?;
            method(&Reflect::get(owner, &"map".into())?, "clear", &Array::new())?;
            Ok(values.reverse().into())
        }
        "length" => Reflect::get(&Reflect::get(owner, &"map".into())?, &"size".into()),
        "iterator" => method(
            &Reflect::get(owner, &"map".into())?,
            "values",
            &Array::new(),
        ),
        "inspect" => spread(owner).map(Into::into),
        _ => unreachable!(),
    }
}

pub(super) fn spread(value: &JsValue) -> Result<Array, JsValue> {
    let iterator = Reflect::get(value, &Symbol::iterator())?
        .dyn_into::<Function>()
        .map_err(|_| js_sys::TypeError::new("value is not iterable"))?;
    let iterator = Reflect::apply(&iterator, value, &Array::new())?;
    let next = Reflect::get(&iterator, &"next".into())?.dyn_into::<Function>()?;
    let values = Array::new();
    loop {
        let record = Reflect::apply(&next, &iterator, &Array::new())?;
        if !record.is_object() && !record.is_function() {
            return Err(js_sys::TypeError::new("iterator result is not an object").into());
        }
        if Reflect::get(&record, &"done".into())?.is_truthy() {
            return Ok(values);
        }
        values.push(&Reflect::get(&record, &"value".into())?);
    }
}
