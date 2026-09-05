//! Browser bindings for caller-owned Conversation Definition registries.

use std::{cell::RefCell, rc::Rc};

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{
    ConversationEventRegistry, ConversationNodeDefinition, ConversationViewDefinition,
    ConversationViewRegistry, RuntimeDisposer,
};

type EventSnapshot = Rc<Vec<Rc<ConversationNodeDefinition<JsValue>>>>;
type ViewSnapshot = Rc<Vec<Rc<ConversationViewDefinition<JsValue>>>>;

/// Browser Conversation Event Definition registry.
#[wasm_bindgen(js_name = ConversationEventRegistry)]
pub struct WasmConversationEventRegistry {
    registry: Rc<ConversationEventRegistry<JsValue>>,
    cache: RefCell<Option<(EventSnapshot, Array)>>,
}

#[wasm_bindgen(js_class = ConversationEventRegistry)]
impl WasmConversationEventRegistry {
    /// Creates an empty registry.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            registry: ConversationEventRegistry::new(),
            cache: RefCell::new(None),
        }
    }

    /// Direct registration primitive used by caller-bound faces.
    ///
    /// # Errors
    ///
    /// Returns target/builder and duplicate-kind diagnostics.
    #[allow(clippy::needless_pass_by_value)]
    pub fn register(&self, definition: JsValue) -> Result<Function, JsValue> {
        let definition = event_definition(definition)?;
        self.registry
            .register(definition)
            .map(runtime_disposer)
            .map_err(|error| js_sys::Error::new(&error.to_string()).into())
    }

    /// Registers the sole unmatched-event fallback.
    ///
    /// # Errors
    ///
    /// Returns target/builder, missing-target, and duplicate-fallback diagnostics.
    #[wasm_bindgen(js_name = registerFallback)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn register_fallback(&self, definition: JsValue) -> Result<Function, JsValue> {
        let definition = event_definition(definition)?;
        self.registry
            .register_fallback(definition)
            .map(runtime_disposer)
            .map_err(|error| js_sys::Error::new(&error.to_string()).into())
    }

    /// Stable Definition array until mutation.
    pub fn entries(&self) -> Array {
        let snapshot = self.registry.entries();
        let mut cache = self.cache.borrow_mut();
        if let Some((current, value)) = &*cache
            && Rc::ptr_eq(current, &snapshot)
        {
            return value.clone();
        }
        let value = Array::new();
        for definition in snapshot.iter() {
            value.push(&definition.payload);
        }
        *cache = Some((snapshot, value.clone()));
        value
    }

    /// Current fallback object.
    #[wasm_bindgen(js_name = fallbackEntry)]
    pub fn fallback_entry(&self) -> JsValue {
        self.registry
            .fallback()
            .map_or(JsValue::UNDEFINED, |definition| definition.payload.clone())
    }

    /// Synchronously subscribes to every registry change.
    pub fn subscribe(&self, listener: Function) -> Function {
        runtime_disposer(self.registry.subscribe(Rc::new(move || {
            call_or_throw(listener.call0(&JsValue::UNDEFINED));
        })))
    }

    /// Caller-bound register/fallback face using `caller.effect` ownership.
    ///
    /// # Errors
    ///
    /// Returns JavaScript object-construction failures.
    #[wasm_bindgen(js_name = faceFor)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn face_for(&self, caller: JsValue) -> Result<JsValue, JsValue> {
        let face = Object::new();
        let registry = self.registry.clone();
        let register_caller = caller.clone();
        let register = Closure::wrap(Box::new(move |definition: JsValue| {
            let definition = event_definition(definition)?;
            let key = definition.kind.clone();
            let registry = registry.clone();
            own_effect(
                &register_caller,
                &format!("conversationEvents.register({key:?})"),
                move || {
                    registry
                        .register(definition)
                        .map_err(|error| js_sys::Error::new(&error.to_string()).into())
                },
            )
        })
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
        set(&face, "register", &register.into_js_value())?;
        let registry = self.registry.clone();
        let fallback_caller = caller;
        let fallback = Closure::wrap(Box::new(move |definition: JsValue| {
            let definition = event_definition(definition)?;
            let key = definition.kind.clone();
            let registry = registry.clone();
            own_effect(
                &fallback_caller,
                &format!("conversationEvents.registerFallback({key:?})"),
                move || {
                    registry
                        .register_fallback(definition)
                        .map_err(|error| js_sys::Error::new(&error.to_string()).into())
                },
            )
        })
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
        set(&face, "registerFallback", &fallback.into_js_value())?;
        Ok(face.into())
    }
}

impl Default for WasmConversationEventRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl WasmConversationEventRegistry {
    pub(crate) fn core_registry(&self) -> Rc<ConversationEventRegistry<JsValue>> {
        self.registry.clone()
    }
}

/// Browser Conversation View Definition registry.
#[wasm_bindgen(js_name = ConversationViewRegistry)]
pub struct WasmConversationViewRegistry {
    registry: Rc<ConversationViewRegistry<JsValue>>,
    cache: RefCell<Option<(ViewSnapshot, Array)>>,
}

#[wasm_bindgen(js_class = ConversationViewRegistry)]
impl WasmConversationViewRegistry {
    /// Creates an empty registry.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            registry: ConversationViewRegistry::new(),
            cache: RefCell::new(None),
        }
    }

    /// Registers one uniquely targeted view Definition.
    ///
    /// # Errors
    ///
    /// Returns missing-target and duplicate-target diagnostics.
    #[allow(clippy::needless_pass_by_value)]
    pub fn register(&self, definition: JsValue) -> Result<Function, JsValue> {
        self.registry
            .register(view_definition(definition)?)
            .map(runtime_disposer)
            .map_err(|error| js_sys::Error::new(&error.to_string()).into())
    }

    /// Stable view Definition array until mutation.
    pub fn entries(&self) -> Array {
        let snapshot = self.registry.entries();
        let mut cache = self.cache.borrow_mut();
        if let Some((current, value)) = &*cache
            && Rc::ptr_eq(current, &snapshot)
        {
            return value.clone();
        }
        let value = Array::new();
        for definition in snapshot.iter() {
            value.push(&definition.payload);
        }
        *cache = Some((snapshot, value.clone()));
        value
    }

    /// Synchronously subscribes to every registry change.
    pub fn subscribe(&self, listener: Function) -> Function {
        runtime_disposer(self.registry.subscribe(Rc::new(move || {
            call_or_throw(listener.call0(&JsValue::UNDEFINED));
        })))
    }

    /// Caller-bound register face using `caller.effect` ownership.
    ///
    /// # Errors
    ///
    /// Returns JavaScript object-construction failures.
    #[wasm_bindgen(js_name = faceFor)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn face_for(&self, caller: JsValue) -> Result<JsValue, JsValue> {
        let face = Object::new();
        let registry = self.registry.clone();
        let register = Closure::wrap(Box::new(move |definition: JsValue| {
            let definition = view_definition(definition)?;
            let key = definition.target.clone();
            let registry = registry.clone();
            own_effect(
                &caller,
                &format!("conversationViews.register({key:?})"),
                move || {
                    registry
                        .register(definition)
                        .map_err(|error| js_sys::Error::new(&error.to_string()).into())
                },
            )
        })
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
        set(&face, "register", &register.into_js_value())?;
        Ok(face.into())
    }
}

impl Default for WasmConversationViewRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl WasmConversationViewRegistry {
    pub(crate) fn core_registry(&self) -> Rc<ConversationViewRegistry<JsValue>> {
        self.registry.clone()
    }
}

fn event_definition(value: JsValue) -> Result<ConversationNodeDefinition<JsValue>, JsValue> {
    let kind = required_string(&value, "kind")?;
    let target = optional_string(&value, "target")?;
    let has_view_builder =
        !Reflect::get(&value, &JsValue::from_str("buildViewNode"))?.is_undefined();
    Ok(ConversationNodeDefinition {
        kind,
        target,
        has_view_builder,
        payload: value,
    })
}

fn view_definition(value: JsValue) -> Result<ConversationViewDefinition<JsValue>, JsValue> {
    Ok(ConversationViewDefinition {
        target: required_string(&value, "target")?,
        payload: value,
    })
}

fn own_effect(
    caller: &JsValue,
    label: &str,
    setup: impl FnOnce() -> Result<RuntimeDisposer, JsValue> + 'static,
) -> Result<JsValue, JsValue> {
    let mut setup = Some(setup);
    let installer = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let setup = setup
            .take()
            .ok_or_else(|| js_sys::Error::new("Conversation registry effect installed twice"))?;
        Ok(runtime_disposer(setup()?).into())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        caller,
        "effect",
        &[installer.into_js_value(), JsValue::from_str(label)],
    )
}

fn runtime_disposer(disposer: RuntimeDisposer) -> Function {
    Closure::wrap(Box::new(move || disposer.dispose()) as Box<dyn FnMut()>)
        .into_js_value()
        .unchecked_into()
}

fn optional_string(value: &JsValue, key: &str) -> Result<Option<String>, JsValue> {
    let value = Reflect::get(value, &JsValue::from_str(key))?;
    if value.is_undefined() {
        return Ok(None);
    }
    value.as_string().map(Some).ok_or_else(|| {
        js_sys::Error::new(&format!("Conversation Definition {key:?} must be a string")).into()
    })
}

fn required_string(value: &JsValue, key: &str) -> Result<String, JsValue> {
    optional_string(value, key)?.ok_or_else(|| {
        js_sys::Error::new(&format!("Conversation Definition {key:?} is required")).into()
    })
}

fn required(value: &JsValue, key: &str) -> Result<JsValue, JsValue> {
    let value = Reflect::get(value, &JsValue::from_str(key))?;
    if value.is_undefined() || value.is_null() {
        Err(js_sys::Error::new(&format!("Conversation registry requires {key:?}")).into())
    } else {
        Ok(value)
    }
}

fn call_method(value: &JsValue, method: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = required(value, method)?.dyn_into::<Function>()?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    method.apply(value, &args)
}

fn call_or_throw(result: Result<JsValue, JsValue>) {
    if let Err(error) = result {
        wasm_bindgen::throw_val(error);
    }
}

fn set(object: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    if Reflect::set(object, &JsValue::from_str(key), value)? {
        Ok(())
    } else {
        Err(js_sys::Error::new(&format!("failed to set {key:?}")).into())
    }
}
