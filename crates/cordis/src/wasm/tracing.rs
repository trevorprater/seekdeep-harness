//! Source service tracing and callback binding over JavaScript object identities.

use js_sys::{Array, Function, Object, Proxy, Reflect, Symbol};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use super::{browser_values, get_with_receiver, object};

#[derive(Clone)]
pub(super) struct Tracer {
    face: JsValue,
}

impl Tracer {
    pub(super) const fn new(face: JsValue) -> Self {
        Self { face }
    }

    pub(super) fn trace(&self, value: &JsValue) -> Result<JsValue, JsValue> {
        if !is_object(value) {
            return Ok(value.clone());
        }
        if !own_descriptor(value, &symbol("shadow")?)?.is_undefined() {
            return Reflect::get_prototype_of(value).map(Into::into);
        }
        let tracker = tracker(value)?;
        if !tracker.is_truthy() {
            return Ok(value.clone());
        }
        self.tracked(value, &tracker)
    }

    pub(super) fn bind(&self, callback: &JsValue) -> Result<JsValue, JsValue> {
        let handler = Object::new();
        let tracer = self.clone();
        let apply = Closure::wrap(Box::new(
            move |target: Function, receiver: JsValue, args: Array| {
                Reflect::apply(
                    &target,
                    &tracer.trace(&receiver)?,
                    &tracer.arguments(&args)?,
                )
            },
        )
            as Box<dyn Fn(Function, JsValue, Array) -> Result<JsValue, JsValue>>)
        .into_js_value();
        let tracer = self.clone();
        let construct = Closure::wrap(Box::new(
            move |target: Function, args: Array, new_target: Function| {
                Reflect::construct_with_new_target(&target, &tracer.arguments(&args)?, &new_target)
            },
        )
            as Box<dyn Fn(Function, Array, Function) -> Result<JsValue, JsValue>>)
        .into_js_value();
        Reflect::set(&handler, &"apply".into(), &apply)?;
        Reflect::set(&handler, &"construct".into(), &construct)?;
        let proxy = Reflect::get(&js_sys::global(), &"Proxy".into())?.dyn_into::<Function>()?;
        Reflect::construct(&proxy, &Array::of2(callback, &handler))
    }

    fn arguments(&self, args: &Array) -> Result<Array, JsValue> {
        args.iter().map(|value| self.trace(&value)).collect()
    }

    #[allow(clippy::too_many_lines)] // One Proxy keeps the source get/set/apply rules together.
    pub(super) fn tracked(
        &self,
        value: &JsValue,
        descriptor: &JsValue,
    ) -> Result<JsValue, JsValue> {
        let mut tracer = self.clone();
        if browser_values::get(&tracer.face, &symbol("shadow")?)?.is_truthy()
            && !browser_values::get(descriptor, &"noShadow".into())?.is_truthy()
        {
            tracer.face = Reflect::get_prototype_of(&tracer.face)?.into();
        }
        let handler = Object::new();
        let reader = tracer.clone();
        let read_tracker = descriptor.clone();
        let get = Closure::wrap(
            Box::new(move |target: JsValue, key: JsValue, receiver: JsValue| {
                if Object::is(&key, &symbol("original")?) {
                    return Ok(target);
                }
                let property = browser_values::get(&read_tracker, &"property".into())?;
                if Object::is(&key, &property) {
                    return Ok(reader.face.clone());
                }
                if key.is_symbol() {
                    return get_with_receiver(&target, &key, &receiver);
                }
                if let Some(associated) = reader.associated(&read_tracker, &key)? {
                    return get_with_receiver(
                        &reader.face,
                        &associated,
                        &with_property(&reader.face, &symbol("receiver")?, &receiver)?,
                    );
                }
                let descriptor = property_descriptor(&target, &key)?;
                let mut shadow = None;
                let value =
                    if !descriptor.is_undefined() && Reflect::has(&descriptor, &"value".into())? {
                        Reflect::get(&descriptor, &"value".into())?
                    } else {
                        let property = browser_values::get(&read_tracker, &"property".into())?;
                        let context_receiver = reader.shadow(&target, &property, &receiver)?;
                        let value = get_with_receiver(&target, &key, &context_receiver)?;
                        shadow = Some(context_receiver);
                        value
                    };
                let inner_tracker = if value.is_null() || value.is_undefined() {
                    JsValue::UNDEFINED
                } else {
                    browser_values::get(&value, &symbol("tracker")?)?
                };
                if inner_tracker.is_truthy() {
                    reader.tracked(&value, &inner_tracker)
                } else if !browser_values::get(&read_tracker, &"noShadow".into())?.is_truthy()
                    && value.is_function()
                {
                    let shadow = match shadow {
                        Some(shadow) => shadow,
                        None => reader.shadow(
                            &target,
                            &browser_values::get(&read_tracker, &"property".into())?,
                            &receiver,
                        )?,
                    };
                    reader.method(&value, receiver, shadow)
                } else {
                    Ok(value)
                }
            }) as Box<dyn Fn(JsValue, JsValue, JsValue) -> Result<JsValue, JsValue>>,
        )
        .into_js_value();
        let writer = tracer;
        let write_tracker = descriptor.clone();
        let set = Closure::wrap(Box::new(
            move |target: JsValue, key: JsValue, value: JsValue, receiver: JsValue| {
                if Object::is(&key, &symbol("original")?) {
                    return Ok(false);
                }
                let property = browser_values::get(&write_tracker, &"property".into())?;
                if Object::is(&key, &property) {
                    return Ok(false);
                }
                if key.is_symbol() {
                    return Reflect::set_with_receiver(&target, &key, &value, &receiver);
                }
                if let Some(associated) = writer.associated(&write_tracker, &key)? {
                    return Reflect::set_with_receiver(
                        &writer.face,
                        &associated,
                        &value,
                        &with_property(&writer.face, &symbol("receiver")?, &receiver)?,
                    );
                }
                Reflect::set_with_receiver(
                    &target,
                    &key,
                    &value,
                    &writer.shadow(
                        &target,
                        &browser_values::get(&write_tracker, &"property".into())?,
                        &receiver,
                    )?,
                )
            },
        )
            as Box<dyn Fn(JsValue, JsValue, JsValue, JsValue) -> Result<bool, JsValue>>)
        .into_js_value();
        Reflect::set(&handler, &"get".into(), &get)?;
        Reflect::set(&handler, &"set".into(), &set)?;
        let invoke = Closure::wrap(Box::new(
            move |proxy: JsValue, target: JsValue, receiver: JsValue, args: Array| {
                let invoke = Reflect::get(&target, &symbol("invoke")?)?;
                if invoke.is_truthy() {
                    invoke.dyn_into::<Function>()?.apply(&proxy, &args)
                } else {
                    target.dyn_into::<Function>()?.apply(&receiver, &args)
                }
            },
        )
            as Box<dyn Fn(JsValue, JsValue, JsValue, Array) -> Result<JsValue, JsValue>>)
        .into_js_value();
        let apply = Function::new_with_args("invoke", "return function(target, receiver, args) { return invoke(this.proxy, target, receiver, args); }")
            .call1(&JsValue::UNDEFINED, &invoke)?;
        Reflect::set(&handler, &"apply".into(), &apply)?;
        let proxy = browser_values::proxy(value, &handler)?;
        // The apply trap's self-reference remains a JavaScript-owned cycle.
        Reflect::set(&handler, &"proxy".into(), &proxy)?;
        Ok(proxy)
    }

    fn associated(&self, descriptor: &JsValue, key: &JsValue) -> Result<Option<JsValue>, JsValue> {
        if !browser_values::get(descriptor, &"associate".into())?.is_truthy() {
            return Ok(None);
        }
        let reflect = browser_values::get(&self.face, &"reflect".into())?;
        let props = browser_values::get(&reflect, &"props".into())?;
        let property = Function::new_with_args("associate,key", "return `${associate}.${key}`;");
        let name = property.call2(
            &JsValue::UNDEFINED,
            &browser_values::get(descriptor, &"associate".into())?,
            key,
        )?;
        if !browser_values::get(&props, &name)?.is_truthy() {
            return Ok(None);
        }
        property
            .call2(
                &JsValue::UNDEFINED,
                &browser_values::get(descriptor, &"associate".into())?,
                key,
            )
            .map(Some)
    }

    fn shadow(
        &self,
        target: &JsValue,
        property: &JsValue,
        receiver: &JsValue,
    ) -> Result<JsValue, JsValue> {
        if !property.is_truthy() {
            return Ok(receiver.clone());
        }
        let descriptor = own_descriptor(target, property)?;
        if descriptor.is_undefined() {
            return Ok(receiver.clone());
        }
        let origin = Reflect::get(&descriptor, &"value".into())?;
        if !origin.is_truthy() {
            return Ok(receiver.clone());
        }
        let metadata = Object::new();
        Reflect::set(&metadata, &symbol("shadow")?, &origin)?;
        let extended = Reflect::get(&self.face, &"extend".into())?
            .dyn_into::<Function>()?
            .call1(&self.face, &metadata)?;
        with_property(receiver, property, &extended)
    }

    fn method(
        &self,
        method: &JsValue,
        outer: JsValue,
        shadow: JsValue,
    ) -> Result<JsValue, JsValue> {
        let tracer = self.clone();
        let apply = Closure::wrap(Box::new(
            move |target: Function, receiver: JsValue, args: Array| {
                let receiver = if Object::is(&receiver, &outer) {
                    &shadow
                } else {
                    &receiver
                };
                tracer.trace(&Reflect::apply(&target, receiver, &args)?)
            },
        )
            as Box<dyn Fn(Function, JsValue, Array) -> Result<JsValue, JsValue>>)
        .into_js_value();
        Ok(Proxy::new(method, &object(&[("apply", apply)])?).into())
    }
}

use super::browser_symbols::get as symbol;

fn is_object(value: &JsValue) -> bool {
    !value.is_null() && (value.is_object() || value.is_function())
}

fn tracker(value: &JsValue) -> Result<JsValue, JsValue> {
    if !is_object(value) {
        return Ok(JsValue::UNDEFINED);
    }
    let canonical = Reflect::get(value, &symbol("tracker")?)?;
    if canonical.is_truthy() {
        return Ok(canonical);
    }
    if Reflect::get(value, &Symbol::for_("cordis.service.tracker"))?.as_bool() == Some(true) {
        return object(&[("property", JsValue::from_str("ctx"))]).map(Into::into);
    }
    Ok(JsValue::UNDEFINED)
}

fn own_descriptor(value: &JsValue, key: &JsValue) -> Result<JsValue, JsValue> {
    Reflect::get_own_property_descriptor(value.unchecked_ref::<Object>(), key)
}

use browser_values::property_descriptor;

fn with_property(
    target: &JsValue,
    property: &JsValue,
    value: &JsValue,
) -> Result<JsValue, JsValue> {
    let props = Object::create(&Object::from(JsValue::NULL));
    Reflect::define_property(
        &props,
        property,
        &object(&[("value", value.clone()), ("writable", false.into())])?,
    )?;
    with_props(target, &props)
}

/// Traces a value using the caller's current Context and tracker metadata.
///
/// # Errors
/// Propagates property, prototype, and proxy construction failures.
#[wasm_bindgen(js_name = getTraceable)]
pub fn get_traceable(context: JsValue, value: &JsValue) -> Result<JsValue, JsValue> {
    Tracer::new(context).trace(value)
}

/// Overlays properties while preserving the target's constructor and descriptors.
///
/// # Errors
/// Propagates proxy construction and property access failures.
#[wasm_bindgen(js_name = withProps)]
pub fn with_props(target: &JsValue, props: &JsValue) -> Result<JsValue, JsValue> {
    if !props.is_truthy() {
        return Ok(target.clone());
    }
    let reader = props.clone();
    let get = Closure::wrap(
        Box::new(move |target: JsValue, key: JsValue, receiver: JsValue| {
            if Reflect::has(&reader, &key)? && key.as_string().as_deref() != Some("constructor") {
                get_with_receiver(&reader, &key, &receiver)
            } else {
                get_with_receiver(&target, &key, &receiver)
            }
        }) as Box<dyn Fn(JsValue, JsValue, JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value();
    let writer = props.clone();
    let set = Closure::wrap(Box::new(
        move |target: JsValue, key: JsValue, value: JsValue, receiver: JsValue| {
            if Reflect::has(&writer, &key)? && key.as_string().as_deref() != Some("constructor") {
                Reflect::set_with_receiver(&writer, &key, &value, &receiver)
            } else {
                Reflect::set_with_receiver(&target, &key, &value, &receiver)
            }
        },
    )
        as Box<dyn Fn(JsValue, JsValue, JsValue, JsValue) -> Result<bool, JsValue>>)
    .into_js_value();
    let proxy = browser_values::proxy(target, &object(&[("get", get), ("set", set)])?)?;
    super::remember_context_alias(target, &proxy);
    Ok(proxy)
}
