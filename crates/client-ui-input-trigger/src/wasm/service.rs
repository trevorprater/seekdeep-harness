//! Root browser source roster and lazy per-session controller ownership.

use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

use js_sys::Function;
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use super::{
    call_method, controller::BrowserInputTriggerController, log_error, optional, required,
    required_string,
};
use crate::TriggerChar;

#[derive(Clone)]
pub(crate) struct BrowserSource {
    pub(crate) id: u64,
    pub(crate) value: JsValue,
    pub(crate) trigger: TriggerChar,
    pub(crate) name: String,
    pub(crate) order: f64,
}

struct ServiceState {
    sources: Vec<BrowserSource>,
    controllers: BTreeMap<String, Rc<BrowserInputTriggerController>>,
    next_source: u64,
}

pub(crate) struct BrowserInputTriggerService {
    sessions: JsValue,
    state: RefCell<ServiceState>,
}

impl BrowserInputTriggerService {
    pub(crate) fn new(sessions: JsValue) -> Rc<Self> {
        Rc::new(Self {
            sessions,
            state: RefCell::new(ServiceState {
                sources: Vec::new(),
                controllers: BTreeMap::new(),
                next_source: 0,
            }),
        })
    }

    pub(crate) fn all(&self) -> Vec<BrowserSource> {
        self.state.borrow().sources.clone()
    }

    pub(crate) fn sources(&self, trigger: TriggerChar) -> Vec<BrowserSource> {
        let mut sources = self
            .state
            .borrow()
            .sources
            .iter()
            .filter(|source| source.trigger == trigger)
            .cloned()
            .collect::<Vec<_>>();
        sources.sort_by(|left, right| left.order.total_cmp(&right.order));
        sources
    }

    pub(crate) fn face(service: &Rc<Self>) -> JsValue {
        WasmInputTriggerService::from_inner(service.clone()).into()
    }

    pub(crate) fn dispose_all(&self) {
        let controllers = std::mem::take(&mut self.state.borrow_mut().controllers);
        for controller in controllers.into_values() {
            controller.dispose();
        }
    }

    pub(crate) fn register(self: &Rc<Self>, value: JsValue) -> Result<Function, JsValue> {
        let trigger = parse_trigger(&required_string(&value, "trigger", "trigger source")?)?;
        let name = required_string(&value, "name", "trigger source")?;
        let order = optional(&value, "order")?
            .and_then(|value| value.as_f64())
            .unwrap_or(0.0);
        if self
            .state
            .borrow()
            .sources
            .iter()
            .any(|source| source.trigger == trigger && source.name == name)
        {
            return Err(js_sys::Error::new(&format!(
                "slash source \"{}{name}\" is already registered",
                trigger.as_char()
            ))
            .into());
        }
        let source = {
            let mut state = self.state.borrow_mut();
            state.next_source = state
                .next_source
                .checked_add(1)
                .ok_or_else(|| js_sys::Error::new("slash source id exhausted"))?;
            let source = BrowserSource {
                id: state.next_source,
                value,
                trigger,
                name,
                order,
            };
            state.sources.push(source.clone());
            source
        };
        let controllers = self
            .state
            .borrow()
            .controllers
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for controller in controllers {
            if let Err(error) = controller.source_added(&source) {
                log_error(
                    &format!(
                        "[ui-input-trigger] source {:?}{} late-registration setup failed:",
                        source.trigger.as_char(),
                        source.name
                    ),
                    &error,
                );
            }
        }
        let service = Rc::downgrade(self);
        let source_id = source.id;
        Ok(Closure::wrap(Box::new(move || {
            let Some(service) = service.upgrade() else {
                return;
            };
            let removed = {
                let mut state = service.state.borrow_mut();
                let Some(index) = state
                    .sources
                    .iter()
                    .position(|source| source.id == source_id)
                else {
                    return;
                };
                state.sources.remove(index)
            };
            let controllers = service
                .state
                .borrow()
                .controllers
                .values()
                .cloned()
                .collect::<Vec<_>>();
            for controller in controllers {
                controller.source_removed(&removed);
            }
        }) as Box<dyn FnMut()>)
        .into_js_value()
        .unchecked_into())
    }

    pub(crate) fn session_of(
        self: &Rc<Self>,
        actx: &JsValue,
    ) -> Result<Rc<BrowserInputTriggerController>, JsValue> {
        let id = call_method(&self.sessions, "scopeOf", std::slice::from_ref(actx))?;
        let Some(id) = id.as_string() else {
            return Err(js_sys::Error::new("slash.sessionOf requires a session scope").into());
        };
        if let Some(controller) = self.state.borrow().controllers.get(&id) {
            return Ok(controller.clone());
        }
        let controller =
            BrowserInputTriggerController::new(actx.clone(), id.clone(), Rc::downgrade(self))?;
        self.state
            .borrow_mut()
            .controllers
            .insert(id.clone(), controller.clone());
        let service = Rc::downgrade(self);
        let dispose_controller = controller.clone();
        let cleanup_id = id.clone();
        let setup = Closure::wrap(Box::new(move || -> JsValue {
            let service = service.clone();
            let id = cleanup_id.clone();
            let controller = dispose_controller.clone();
            Closure::wrap(Box::new(move || {
                controller.dispose();
                if let Some(service) = service.upgrade() {
                    service.state.borrow_mut().controllers.remove(&id);
                }
            }) as Box<dyn FnMut()>)
            .into_js_value()
        }) as Box<dyn FnMut() -> JsValue>);
        if let Err(error) = call_method(
            actx,
            "effect",
            &[
                setup.into_js_value(),
                JsValue::from_str("slash: session controller"),
            ],
        ) {
            self.state.borrow_mut().controllers.remove(&id);
            controller.dispose();
            return Err(error);
        }
        Ok(controller)
    }
}

/// Compiled `InputTriggerService` browser face.
#[wasm_bindgen(js_name = __InputTriggerService)]
pub struct WasmInputTriggerService {
    inner: Rc<BrowserInputTriggerService>,
}

#[wasm_bindgen(js_class = __InputTriggerService)]
impl WasmInputTriggerService {
    /// Creates an empty source registry from a Client Context.
    ///
    /// # Errors
    ///
    /// Returns a missing Sessions service.
    #[wasm_bindgen(constructor)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn new(ctx: JsValue) -> Result<WasmInputTriggerService, JsValue> {
        Ok(Self::from_inner(BrowserInputTriggerService::new(required(
            &ctx,
            "sessions",
            "Client Context",
        )?)))
    }

    /// Registers one unique trigger/name source.
    ///
    /// # Errors
    ///
    /// Returns malformed or duplicate-source diagnostics.
    #[wasm_bindgen(js_name = registerSource)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn register_source(&self, source: JsValue) -> Result<Function, JsValue> {
        self.inner.register(source)
    }

    /// Resolves the resident controller for one Session Context.
    ///
    /// # Errors
    ///
    /// Returns when the context has no Session scope or source warmup fails.
    #[wasm_bindgen(js_name = sessionOf)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn session_of(&self, actx: JsValue) -> Result<JsValue, JsValue> {
        self.inner.session_of(&actx)?.face()
    }
}

impl WasmInputTriggerService {
    pub(crate) fn from_inner(inner: Rc<BrowserInputTriggerService>) -> Self {
        Self { inner }
    }
}

fn parse_trigger(value: &str) -> Result<TriggerChar, JsValue> {
    match value {
        "/" => Ok(TriggerChar::Slash),
        "@" => Ok(TriggerChar::At),
        _ => Err(js_sys::Error::new("trigger source trigger must be '/' or '@'").into()),
    }
}
