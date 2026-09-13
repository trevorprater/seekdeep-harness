//! Source stack composition over caller frames captured at the JavaScript boundary.

use js_sys::{Array, Function, Object, Reflect, WeakMap};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use super::{browser_values as values, update_hooks};

thread_local! {
    static OWNERS: WeakMap = WeakMap::new();
    static BUILDER: std::cell::RefCell<Option<Function>> = const { std::cell::RefCell::new(None) };
    static EXECUTION_BOUNDARY: Function = boundary("cordisExecutionBoundary");
    static DISPOSAL_BOUNDARY: Function = boundary("cordisDisposalBoundary");
    static CURRENT: std::cell::RefCell<Vec<Composition>> = const { std::cell::RefCell::new(Vec::new()) };
}

struct CurrentComposition;

impl Drop for CurrentComposition {
    fn drop(&mut self) {
        CURRENT.with(|current| {
            current.borrow_mut().pop();
        });
    }
}

fn boundary(name: &str) -> Function {
    Function::new_no_args(&format!("return function {name}(info,target,key,args,mode) {{ const operation = mode === 'capture' ? () => new Error() : mode === 'construct' ? () => Reflect.construct(target,args) : mode === 'call' ? () => Reflect.apply(target,key,args) : mode === 'get' ? () => target[key] : mode === 'has' ? () => key in target : () => Reflect.apply(target[key],target,args); const result = operation(); if (mode === 'capture') info.error = result; return result; }};"))
        .call0(&JsValue::UNDEFINED).expect("static stack boundary is valid").unchecked_into()
}

pub(super) fn remember_owner(owner: &JsValue, outer: &JsValue) -> Result<(), JsValue> {
    let record = super::object(&[("outer", outer.clone())])?;
    OWNERS.with(|owners| owners.set(owner.unchecked_ref::<Object>(), &record));
    Ok(())
}

pub(super) fn owner_stack(owner: &JsValue) -> Result<JsValue, JsValue> {
    let runner = values::get(owner, &"_runner".into())?;
    if !runner.is_undefined() {
        return values::get(&runner, &"getOuterStack".into());
    }
    let mut current = owner.clone();
    while current.is_object() || current.is_function() {
        let record = OWNERS.with(|owners| owners.get(current.unchecked_ref::<Object>()));
        if !record.is_undefined() {
            return values::get(&record, &"outer".into());
        }
        current = Reflect::get_prototype_of(&current)?.into();
    }
    Function::new_no_args("return () => [];").call0(&JsValue::UNDEFINED)
}

/// Supplies the caller-frame capture function used by browser lifecycle entrypoints.
#[wasm_bindgen(js_name = configureStackBuilder)]
pub fn configure_stack_builder(builder: Function) {
    BUILDER.with(|slot| *slot.borrow_mut() = Some(builder));
}

pub(super) fn builder() -> Result<Function, JsValue> {
    if let Some(builder) = BUILDER.with(|slot| slot.borrow().clone()) {
        return Ok(builder);
    }
    Function::new_no_args("return () => [];")
        .call0(&JsValue::UNDEFINED)?
        .dyn_into()
}

pub(super) fn capture_outer() -> Result<JsValue, JsValue> {
    Reflect::apply(&builder()?, &JsValue::UNDEFINED, &Array::new())
}

#[derive(Clone)]
pub(super) struct Composition {
    info: JsValue,
    outer: JsValue,
    boundary: Function,
}

impl Composition {
    pub(super) fn current() -> Option<Self> {
        CURRENT.with(|current| current.borrow().last().cloned())
    }
    pub(super) fn new(outer: JsValue) -> Result<Self, JsValue> {
        let result = Self {
            info: stack_info(JsValue::UNDEFINED)?.into(),
            outer: if outer.is_undefined() {
                capture_outer()?
            } else {
                outer
            },
            boundary: EXECUTION_BOUNDARY.with(Clone::clone),
        };
        result.capture()?;
        Ok(result)
    }

    pub(super) fn disposal(outer: JsValue) -> Result<Self, JsValue> {
        let result = Self {
            info: stack_info(JsValue::UNDEFINED)?.into(),
            outer: if outer.is_undefined() {
                capture_outer()?
            } else {
                outer
            },
            boundary: DISPOSAL_BOUNDARY.with(Clone::clone),
        };
        result.capture()?;
        Ok(result)
    }

    fn invoke(
        &self,
        target: &JsValue,
        key: &JsValue,
        args: &JsValue,
        mode: &str,
    ) -> Result<JsValue, JsValue> {
        let parameters = Array::of4(&self.info, target, key, args);
        parameters.push(&mode.into());
        Reflect::apply(&self.boundary, &JsValue::UNDEFINED, &parameters)
    }

    pub(super) fn capture(&self) -> Result<(), JsValue> {
        self.invoke(
            &JsValue::UNDEFINED,
            &JsValue::UNDEFINED,
            &JsValue::UNDEFINED,
            "capture",
        )?;
        Ok(())
    }

    pub(super) fn get(&self, target: &JsValue, key: &JsValue) -> Result<JsValue, JsValue> {
        self.invoke(target, key, &JsValue::UNDEFINED, "get")
    }

    pub(super) fn has(&self, target: &JsValue, key: &JsValue) -> Result<bool, JsValue> {
        self.invoke(target, key, &JsValue::UNDEFINED, "has")
            .map(|value| value.is_truthy())
    }

    pub(super) fn invalid_effect(&self) -> JsValue {
        let failure = Function::new_no_args("throw new TypeError('Invalid effect');");
        self.call(&failure, &JsValue::UNDEFINED, &Array::new())
            .expect_err("effect validation always throws")
    }

    pub(super) fn method(
        &self,
        target: &JsValue,
        key: &JsValue,
        args: &Array,
    ) -> Result<JsValue, JsValue> {
        self.invoke(target, key, args, "method")
    }

    pub(super) fn call(
        &self,
        function: &JsValue,
        receiver: &JsValue,
        args: &Array,
    ) -> Result<JsValue, JsValue> {
        self.invoke(function, receiver, args, "call")
    }

    pub(super) fn construct(&self, function: &JsValue, args: &Array) -> Result<JsValue, JsValue> {
        self.invoke(function, &JsValue::UNDEFINED, args, "construct")
    }

    pub(super) fn run(
        &self,
        callback: impl FnOnce() -> Result<JsValue, JsValue>,
    ) -> Result<JsValue, JsValue> {
        let result = {
            CURRENT.with(|current| current.borrow_mut().push(self.clone()));
            let _scope = CurrentComposition;
            callback()
        }
        .and_then(|result| compose_stack_result(&result, self.info.clone(), self.outer.clone()));
        result.or_else(|error| handle_stack_error(&self.info, &error, &self.outer))
    }
}

fn method(value: &JsValue, name: &str, args: &Array) -> Result<JsValue, JsValue> {
    let function = values::get(value, &name.into())?
        .dyn_into::<Function>()
        .map_err(|_| js_sys::TypeError::new(&format!("{name} is not a function")))?;
    Reflect::apply(&function, value, args)
}

fn frames(error: &JsValue) -> Result<JsValue, JsValue> {
    method(
        &values::get(error, &"stack".into())?,
        "split",
        &Array::of1(&"\n".into()),
    )
}

/// Reads the captured stack lazily and applies the source's caller offset.
///
/// # Errors
/// Propagates stack getter, offset conversion, and array operation failures.
#[wasm_bindgen(js_name = outerStackFrames)]
pub fn outer_stack_frames(error: &JsValue, offset: &JsValue) -> Result<JsValue, JsValue> {
    let lines = frames(error)?;
    let offset = Function::new_with_args("offset", "return 3 + offset;")
        .call1(&JsValue::UNDEFINED, offset)?;
    method(&lines, "slice", &Array::of1(&offset))
}

/// Creates mutable stack information passed to a composed callback.
///
/// # Errors
/// Propagates property construction failures.
#[wasm_bindgen(js_name = stackInfo)]
pub fn stack_info(error: JsValue) -> Result<Object, JsValue> {
    super::object(&[("offset", 1.into()), ("error", error)])
}

/// Composes rejections while preserving synchronous results and arbitrary thenable returns.
///
/// # Errors
/// Propagates result inspection and immediate thenable failures.
#[wasm_bindgen(js_name = composeStackResult)]
pub fn compose_stack_result(
    result: &JsValue,
    info: JsValue,
    outer: JsValue,
) -> Result<JsValue, JsValue> {
    if !values::is_object(result).is_truthy() || !Reflect::has(result, &"then".into())? {
        return Ok(result.clone());
    }
    let rejected =
        Closure::wrap(
            Box::new(move |reason: JsValue| handle_stack_error(&info, &reason, &outer))
                as Box<dyn Fn(JsValue) -> Result<JsValue, JsValue>>,
        )
        .into_js_value();
    method(result, "then", &Array::of2(&JsValue::UNDEFINED, &rejected))
}

fn replace_tail(lines: &JsValue, index: &JsValue, outer: &JsValue) -> Result<JsValue, JsValue> {
    let args = Array::of2(index, &f64::INFINITY.into());
    let outer = outer
        .clone()
        .dyn_into::<Function>()
        .map_err(|_| js_sys::TypeError::new("getOuterStack is not a function"))?;
    let supplied = Reflect::apply(&outer, &JsValue::UNDEFINED, &Array::new())?;
    for frame in update_hooks::spread(&supplied)?.iter() {
        args.push(&frame);
    }
    method(lines, "splice", &args)?;
    method(lines, "join", &Array::of1(&"\n".into()))
}

/// Rewrites a captured error stack or wraps a malformed thrown value.
///
/// # Errors
/// Always throws the resulting error; failures while reading or composing stacks propagate.
#[wasm_bindgen(js_name = handleStackError)]
pub fn handle_stack_error(
    info: &JsValue,
    reason: &JsValue,
    outer: &JsValue,
) -> Result<JsValue, JsValue> {
    let inner = frames(&values::get(info, &"error".into())?)?;
    let stack = if reason.is_null() || reason.is_undefined() {
        JsValue::UNDEFINED
    } else {
        values::get(reason, &"stack".into())?
    };
    if !stack.is_string() {
        let class = Reflect::get(&js_sys::global(), &"Error".into())?.dyn_into::<Function>()?;
        let error = Reflect::construct(&class, &Array::of1(reason))?;
        let lines = frames(&error)?;
        let stack = replace_tail(&lines, &1.into(), outer)?;
        values::set(&error, &"stack".into(), &stack)?;
        return Err(error);
    }
    let lines = frames(reason)?;
    let mut index = method(
        &lines,
        "indexOf",
        &Array::of1(&values::get(&inner, &2.into())?),
    )?;
    if index == JsValue::from_f64(-1.0) {
        return Err(reason.clone());
    }
    let subtract = Function::new_with_args("a,b", "return a - b;");
    index = subtract.call2(
        &JsValue::UNDEFINED,
        &index,
        &values::get(info, &"offset".into())?,
    )?;
    let positive = Function::new_with_args("index", "return index > 0;");
    while positive.call1(&JsValue::UNDEFINED, &index)?.is_truthy() {
        let previous = subtract.call2(&JsValue::UNDEFINED, &index, &1.into())?;
        let line = values::get(&lines, &previous)?;
        if !method(&line, "endsWith", &Array::of1(&" (<anonymous>)".into()))?.is_truthy() {
            break;
        }
        index = previous;
    }
    let stack = replace_tail(&lines, &index, outer)?;
    values::set(reason, &"stack".into(), &stack)?;
    Err(reason.clone())
}
