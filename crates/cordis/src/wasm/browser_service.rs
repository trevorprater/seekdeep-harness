//! Service construction, callable instances, and context-specific configuration.

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use super::{browser_registry::method, browser_values as values, tracing::Tracer};

use super::browser_symbols::get as symbol;

/// Creates the source callable service function with an explicit tracing descriptor.
///
/// # Errors
/// Propagates function, name, and prototype construction failures.
#[wasm_bindgen(js_name = createCallable)]
pub fn create_callable(
    name: &JsValue,
    prototype: &JsValue,
    tracker: JsValue,
) -> Result<JsValue, JsValue> {
    let invoke = Closure::wrap(
        Box::new(move |target: JsValue, receiver: JsValue, args: Array| {
            let context = values::get(&target, &"ctx".into())?;
            let proxy = Tracer::new(context).tracked(&target, &tracker)?;
            if !values::get(&target, &symbol("invoke")?)?.is_truthy() {
                return Reflect::apply(target.unchecked_ref::<Function>(), &receiver, &args);
            }
            let body = values::get(&target, &symbol("invoke")?)?;
            method(&body, "apply", &Array::of2(&proxy, &args))
        }) as Box<dyn Fn(JsValue, JsValue, Array) -> Result<JsValue, JsValue>>,
    )
    .into_js_value();
    let function = Function::new_with_args(
        "invoke",
        "const self = function(...args) { return invoke(self,this,args); }; return self;",
    )
    .call1(&JsValue::UNDEFINED, &invoke)?;
    values::define(&function, &"name".into(), name)?;
    let object = Reflect::get(&js_sys::global(), &"Object".into())?;
    method(&object, "setPrototypeOf", &Array::of2(&function, prototype))
}

/// Initializes the Service base instance and publishes its availability predicate.
///
/// # Errors
/// Propagates constructor, field, and service-registration failures unchanged.
#[wasm_bindgen(js_name = initializeService)]
pub fn initialize_service(
    instance: &JsValue,
    context: &JsValue,
    name: &JsValue,
) -> Result<JsValue, JsValue> {
    let name = if name.is_null() || name.is_undefined() {
        values::get(
            &values::get(instance, &"constructor".into())?,
            &"provide".into(),
        )?
    } else {
        name.clone()
    };
    let tracker = super::object(&[("associate", name.clone()), ("property", "ctx".into())])?;
    let value = if values::get(instance, &symbol("invoke")?)?.is_truthy() {
        let prototype = Reflect::get_prototype_of(instance)?;
        let function = Reflect::get(&js_sys::global(), &"Function".into())?;
        let prototype =
            values::join_prototype(&prototype, &Reflect::get(&function, &"prototype".into())?)?;
        create_callable(&name, &prototype, tracker.clone().into())?
    } else {
        instance.clone()
    };
    values::set(&value, &"ctx".into(), context)?;
    values::set(&value, &"name".into(), &name)?;
    values::define(&value, &symbol("tracker")?, &tracker)?;
    let context = values::get(&value, &"ctx".into())?;
    let reflection = values::get(&context, &"reflect".into())?;
    let check = values::get(instance, &symbol("check")?)?;
    method(&reflection, "provide", &Array::of3(&name, &value, &check))?;
    Ok(value)
}

/// Tests whether a Context uses the same service isolation label.
///
/// # Errors
/// Propagates Context, service-name, and isolation-map getter failures.
#[wasm_bindgen(js_name = serviceFilter)]
pub fn service_filter(service: &JsValue, context: &JsValue) -> Result<bool, JsValue> {
    let scoped = values::get(context, &symbol("isolate")?)?;
    let left = values::get(&scoped, &values::get(service, &"name".into())?)?;
    let origin = values::get(service, &"ctx".into())?;
    let scoped = values::get(&origin, &symbol("isolate")?)?;
    let right = values::get(&scoped, &values::get(service, &"name".into())?)?;
    Ok(left == right)
}

/// Derives a service instance while retaining callable behavior and prototype identity.
///
/// # Errors
/// Propagates property and assignment failures.
#[wasm_bindgen(js_name = extendService)]
pub fn extend_service(service: &JsValue, props: &JsValue) -> Result<JsValue, JsValue> {
    let value =
        if values::get(service, &super::browser_symbols::service_key("invoke")?)?.is_truthy() {
            create_callable(
                &values::get(service, &"name".into())?,
                service,
                values::get(service, &symbol("tracker")?)?,
            )?
        } else {
            Object::create(service.unchecked_ref::<Object>()).into()
        };
    values::assign(&value, &Array::of1(props))
}

/// Resolves inherited service intercepts with the source's merge order and receiver.
///
/// # Errors
/// Propagates intercept traversal, merge, and assignment failures.
#[wasm_bindgen(js_name = resolveServiceConfig)]
pub fn resolve_service_config(
    service: &JsValue,
    base: &JsValue,
    head: &JsValue,
) -> Result<JsValue, JsValue> {
    let context = values::get(service, &"ctx".into())?;
    let mut intercept = values::get(&context, &super::browser_symbols::context_key("intercept")?)?;
    let configs = Array::new();
    while Reflect::has(&intercept, &values::get(service, &"name".into())?)? {
        let name = values::get(service, &"name".into())?;
        if Object::has_own(intercept.unchecked_ref::<Object>(), &name) {
            configs.unshift(&values::get(
                &intercept,
                &values::get(service, &"name".into())?,
            )?);
        }
        intercept = Reflect::get_prototype_of(&intercept)?.into();
    }
    if base.is_truthy() {
        configs.unshift(base);
    }
    if head.is_truthy() {
        configs.push(head);
    }
    let config = values::get(service, &"Config".into())?;
    if !config.is_null()
        && !config.is_undefined()
        && values::get(&config, &"merge".into())?.is_truthy()
    {
        return method(&values::get(service, &"Config".into())?, "merge", &configs);
    }
    values::assign(&Object::new(), &configs)
}

/// Recognizes services through original or cloned constructor prototype chains.
///
/// # Errors
/// Propagates constructor and prototype getter failures.
#[wasm_bindgen(js_name = serviceHasInstance)]
pub fn service_has_instance(class: &JsValue, instance: &JsValue) -> Result<bool, JsValue> {
    if !instance.is_truthy() {
        return Ok(false);
    }
    let mut constructor = values::get(instance, &"constructor".into())?;
    while constructor.is_truthy() {
        let prototype = values::get(&constructor, &"prototype".into())?;
        constructor = if prototype.is_null() || prototype.is_undefined() {
            JsValue::UNDEFINED
        } else {
            values::get(&prototype, &"constructor".into())?
        };
        if constructor == *class {
            return Ok(true);
        }
        if constructor.is_truthy() {
            constructor = Object::get_prototype_of(constructor.unchecked_ref::<Object>()).into();
        }
    }
    Ok(false)
}
