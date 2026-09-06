//! Source service tracing and callback binding over JavaScript object identities.

use js_sys::{Array, Function, Object, Proxy, Reflect, Symbol};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};

use super::{Context, get_with_receiver, object};

#[derive(Clone)]
pub(super) struct Tracer {
    context: Context,
    face: JsValue,
}

impl Tracer {
    pub(super) const fn new(context: Context, face: JsValue) -> Self {
        Self { context, face }
    }

    pub(super) fn trace(&self, value: &JsValue) -> Result<JsValue, JsValue> {
        if !is_object(value) {
            return Ok(value.clone());
        }
        if !own_descriptor(value, &symbol("shadow"))?.is_undefined() {
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
                target.apply(&tracer.trace(&receiver)?, &tracer.arguments(&args)?)
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
        Ok(Proxy::new(callback, &handler).into())
    }

    fn arguments(&self, args: &Array) -> Result<Array, JsValue> {
        args.iter().map(|value| self.trace(&value)).collect()
    }

    #[allow(clippy::too_many_lines)] // One Proxy keeps the source get/set/apply rules together.
    fn tracked(&self, value: &JsValue, descriptor: &JsValue) -> Result<JsValue, JsValue> {
        let no_shadow = Reflect::get(descriptor, &"noShadow".into())?.is_truthy();
        let mut tracer = self.clone();
        if Reflect::get(&tracer.face, &symbol("shadow"))?.is_truthy() && !no_shadow {
            tracer.face = Reflect::get_prototype_of(&tracer.face)?.into();
        }
        let property = Reflect::get(descriptor, &"property".into())?;
        let associate = Reflect::get(descriptor, &"associate".into())?;
        let handler = Object::new();
        let reader = tracer.clone();
        let read_property = property.clone();
        let read_associate = associate.clone();
        let get = Closure::wrap(
            Box::new(move |target: JsValue, key: JsValue, receiver: JsValue| {
                if Object::is(&key, &symbol("original")) {
                    return Ok(target);
                }
                if Object::is(&key, &read_property) {
                    return Ok(reader.face.clone());
                }
                if key.is_symbol() {
                    return get_with_receiver(&target, &key, &receiver);
                }
                if let Some(associated) = reader.associated(&read_associate, &key) {
                    return get_with_receiver(
                        &reader.face,
                        &associated.into(),
                        &with_property(&reader.face, &symbol("receiver"), &receiver)?,
                    );
                }
                let descriptor = property_descriptor(&target, &key)?;
                let mut shadow = None;
                let value =
                    if !descriptor.is_undefined() && Reflect::has(&descriptor, &"value".into())? {
                        Reflect::get(&descriptor, &"value".into())?
                    } else {
                        let context_receiver = reader.shadow(&target, &read_property, &receiver)?;
                        let value = get_with_receiver(&target, &key, &context_receiver)?;
                        shadow = Some(context_receiver);
                        value
                    };
                let inner_tracker = tracker(&value)?;
                if inner_tracker.is_truthy() {
                    reader.tracked(&value, &inner_tracker)
                } else if !no_shadow && value.is_function() {
                    let shadow = match shadow {
                        Some(shadow) => shadow,
                        None => reader.shadow(&target, &read_property, &receiver)?,
                    };
                    reader.method(&value, receiver, shadow)
                } else {
                    Ok(value)
                }
            }) as Box<dyn Fn(JsValue, JsValue, JsValue) -> Result<JsValue, JsValue>>,
        )
        .into_js_value();
        let writer = tracer;
        let set = Closure::wrap(Box::new(
            move |target: JsValue, key: JsValue, value: JsValue, receiver: JsValue| {
                if Object::is(&key, &symbol("original")) || Object::is(&key, &property) {
                    return Ok(false);
                }
                if key.is_symbol() {
                    return Reflect::set_with_receiver(&target, &key, &value, &receiver);
                }
                if let Some(associated) = writer.associated(&associate, &key) {
                    return Reflect::set_with_receiver(
                        &writer.face,
                        &associated.into(),
                        &value,
                        &with_property(&writer.face, &symbol("receiver"), &receiver)?,
                    );
                }
                Reflect::set_with_receiver(
                    &target,
                    &key,
                    &value,
                    &writer.shadow(&target, &property, &receiver)?,
                )
            },
        )
            as Box<dyn Fn(JsValue, JsValue, JsValue, JsValue) -> Result<bool, JsValue>>)
        .into_js_value();
        Reflect::set(&handler, &"get".into(), &get)?;
        Reflect::set(&handler, &"set".into(), &set)?;
        let invoke = Closure::wrap(Box::new(
            move |proxy: JsValue, target: JsValue, receiver: JsValue, args: Array| {
                let invoke = Reflect::get(&target, &symbol("invoke"))?;
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
        let proxy: JsValue = Proxy::new(value, &handler).into();
        // The apply trap's self-reference remains a JavaScript-owned cycle.
        Reflect::set(&handler, &"proxy".into(), &proxy)?;
        Ok(proxy)
    }

    fn associated(&self, associate: &JsValue, key: &JsValue) -> Option<String> {
        let name = format!("{}.{}", associate.as_string()?, key.as_string()?);
        self.context.has_property(&name).then_some(name)
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
        Reflect::set(&metadata, &symbol("shadow"), &origin)?;
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
                tracer.trace(&target.apply(receiver, &args)?)
            },
        )
            as Box<dyn Fn(Function, JsValue, Array) -> Result<JsValue, JsValue>>)
        .into_js_value();
        Ok(Proxy::new(method, &object(&[("apply", apply)])?).into())
    }
}

fn symbol(name: &str) -> JsValue {
    Symbol::for_(&format!("cordis.{name}")).into()
}

fn is_object(value: &JsValue) -> bool {
    !value.is_null() && (value.is_object() || value.is_function())
}

fn tracker(value: &JsValue) -> Result<JsValue, JsValue> {
    if !is_object(value) {
        return Ok(JsValue::UNDEFINED);
    }
    let canonical = Reflect::get(value, &symbol("tracker"))?;
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

fn property_descriptor(value: &JsValue, key: &JsValue) -> Result<JsValue, JsValue> {
    let mut current = value.clone();
    while !current.is_null() {
        let descriptor = own_descriptor(&current, key)?;
        if !descriptor.is_undefined() {
            return Ok(descriptor);
        }
        current = Reflect::get_prototype_of(&current)?.into();
    }
    Ok(JsValue::UNDEFINED)
}

fn with_property(
    target: &JsValue,
    property: &JsValue,
    value: &JsValue,
) -> Result<JsValue, JsValue> {
    let read_key = property.clone();
    let read_value = value.clone();
    let get = Closure::wrap(
        Box::new(move |target: JsValue, key: JsValue, receiver: JsValue| {
            if Object::is(&key, &read_key) {
                Ok(read_value.clone())
            } else {
                get_with_receiver(&target, &key, &receiver)
            }
        }) as Box<dyn Fn(JsValue, JsValue, JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value();
    let property = property.clone();
    let set = Closure::wrap(Box::new(
        move |target: JsValue, key: JsValue, value: JsValue, receiver: JsValue| {
            if Object::is(&key, &property) {
                Ok(false)
            } else {
                Reflect::set_with_receiver(&target, &key, &value, &receiver)
            }
        },
    )
        as Box<dyn Fn(JsValue, JsValue, JsValue, JsValue) -> Result<bool, JsValue>>)
    .into_js_value();
    Ok(Proxy::new(target, &object(&[("get", get), ("set", set)])?).into())
}
