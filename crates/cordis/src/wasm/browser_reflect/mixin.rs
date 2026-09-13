//! Lazy mixin traversal with JavaScript iterator and generator completion semantics.

use std::{cell::Cell, rc::Rc};

use js_sys::{Array, Function, Reflect, Symbol};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};

use super::{method, mixin_options, object, values};

pub(super) fn setup(
    service: &JsValue,
    source: &JsValue,
    mixins: &JsValue,
) -> Result<JsValue, JsValue> {
    let (service, source, mixins) = (service.clone(), source.clone(), mixins.clone());
    let start = Closure::wrap(Box::new(move || iterator(&service, &source, &mixins))
        as Box<dyn Fn() -> Result<JsValue, JsValue>>)
    .into_js_value();
    Function::new_with_args(
        "start",
        "'use strict'; return function* () { yield* start(); };",
    )
    .call1(&JsValue::UNDEFINED, &start)
}

struct Entries {
    iterator: JsValue,
    next: JsValue,
    done: Cell<bool>,
}

impl Entries {
    fn new(value: &JsValue) -> Result<Self, JsValue> {
        let iterator = call(
            &values::get(value, &Symbol::iterator())?,
            value,
            &Array::new(),
        )?;
        require_object(&iterator)?;
        Ok(Self {
            next: values::get(&iterator, &"next".into())?,
            iterator,
            done: Cell::new(false),
        })
    }

    fn step(&self) -> Result<Option<JsValue>, JsValue> {
        if self.done.get() {
            return Ok(None);
        }
        let result = (|| {
            let record = call(&self.next, &self.iterator, &Array::new())?;
            require_object(&record)?;
            if values::get(&record, &"done".into())?.is_truthy() {
                self.done.set(true);
                return Ok(None);
            }
            values::get(&record, &"value".into()).map(Some)
        })();
        if result.is_err() {
            self.done.set(true);
        }
        result
    }

    fn close(&self) -> Result<(), JsValue> {
        if self.done.replace(true) {
            return Ok(());
        }
        let finish = values::get(&self.iterator, &"return".into())?;
        if finish.is_null() || finish.is_undefined() {
            return Ok(());
        }
        let result = call(&finish, &self.iterator, &Array::new())?;
        require_object(&result)
    }
}

fn iterator(service: &JsValue, source: &JsValue, mixins: &JsValue) -> Result<JsValue, JsValue> {
    let entries = if Array::is_array(mixins) {
        let pair =
            Closure::wrap(Box::new(move |key: JsValue| Array::of2(&key, &key))
                as Box<dyn Fn(JsValue) -> Array>)
            .into_js_value();
        method(mixins, "map", &Array::of1(&pair))?
    } else {
        method(
            &values::get(&js_sys::global(), &"Object".into())?,
            "entries",
            &Array::of1(mixins),
        )?
    };
    let entries = Rc::new(Entries::new(&entries)?);
    let reader = entries.clone();
    let (service, source) = (service.clone(), source.clone());
    let destructure = Function::new_with_args(
        "entry",
        "'use strict'; const [key, name] = entry; return [key, name];",
    );
    let next = Closure::wrap(Box::new(move || {
        let Some(entry) = reader.step()? else {
            return result(true, &JsValue::UNDEFINED);
        };
        let yielded = (|| {
            let pair = destructure.call1(&JsValue::UNDEFINED, &entry)?;
            let key = values::get(&pair, &0.into())?;
            let name = values::get(&pair, &1.into())?;
            let options = mixin_options(&source, &key)?;
            method(&service, "accessor", &Array::of2(&name, &options))
        })();
        match yielded {
            Ok(value) => result(false, &value),
            Err(error) => {
                let _ = reader.close();
                Err(error)
            }
        }
    }) as Box<dyn Fn() -> Result<JsValue, JsValue>>)
    .into_js_value();
    let finished = entries.clone();
    let finish = Closure::wrap(Box::new(move |value: JsValue| {
        finished.close()?;
        result(true, &value)
    }) as Box<dyn Fn(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value();
    let reject = Closure::wrap(Box::new(move |error: JsValue| {
        let _ = entries.close();
        Err(error)
    }) as Box<dyn Fn(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value();
    let iterator = object(&[("next", next), ("return", finish), ("throw", reject)])?;
    values::set(
        &iterator,
        &Symbol::iterator(),
        &Function::new_no_args("'use strict'; return this;"),
    )?;
    Ok(iterator.into())
}

fn result(done: bool, value: &JsValue) -> Result<JsValue, JsValue> {
    object(&[("done", done.into()), ("value", value.clone())]).map(Into::into)
}

fn call(target: &JsValue, receiver: &JsValue, args: &Array) -> Result<JsValue, JsValue> {
    let target = target
        .clone()
        .dyn_into::<Function>()
        .map_err(|_| js_sys::TypeError::new("iterator method is not a function"))?;
    Reflect::apply(&target, receiver, args)
}

fn require_object(value: &JsValue) -> Result<(), JsValue> {
    if value.is_object() || value.is_function() {
        Ok(())
    } else {
        Err(js_sys::TypeError::new("iterator result is not an object").into())
    }
}
