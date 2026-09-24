//! Shared mutable Cordis symbol keys and public-class metadata reads.

use std::cell::RefCell;

use js_sys::{Function, Object, Reflect, Symbol};
use wasm_bindgen::{JsValue, prelude::wasm_bindgen};

thread_local! {
    static SYMBOLS: Object = {
        let symbols = Object::new();
        for name in ["shadow", "receiver", "original", "metadata", "initHooks", "checkProto", "effect", "filter", "isolate", "intercept", "init", "check", "config", "invoke", "extend", "tracker", "resolveConfig"] {
            Reflect::set(&symbols, &name.into(), &Symbol::for_(&format!("cordis.{name}"))).expect("fresh symbol table is writable");
        }
        symbols
    };
    static CONTEXT: RefCell<Option<Function>> = const { RefCell::new(None) };
    static SERVICE: RefCell<Option<Function>> = const { RefCell::new(None) };
}

/// Returns the shared, mutable source symbol table.
#[wasm_bindgen(js_name = cordisSymbols)]
pub fn symbols() -> Object {
    SYMBOLS.with(Clone::clone)
}

pub(super) fn get(name: &str) -> Result<JsValue, JsValue> {
    SYMBOLS.with(|symbols| Reflect::get(symbols, &name.into()))
}

/// Supplies the public Context class used for static metadata reads.
#[wasm_bindgen(js_name = configureContextConstructor)]
pub fn configure_context_constructor(constructor: Function) {
    CONTEXT.with(|slot| *slot.borrow_mut() = Some(constructor));
}

/// Supplies the public Service class used for static metadata reads.
#[wasm_bindgen(js_name = configureServiceConstructor)]
pub fn configure_service_constructor(constructor: Function) {
    SERVICE.with(|slot| *slot.borrow_mut() = Some(constructor));
}

pub(super) fn context_key(name: &str) -> Result<JsValue, JsValue> {
    let class = CONTEXT.with(|slot| slot.borrow().clone());
    class.map_or_else(
        || Ok(Symbol::for_(&format!("cordis.{name}")).into()),
        |class| Reflect::get(&class, &name.into()),
    )
}

pub(super) fn service_key(name: &str) -> Result<JsValue, JsValue> {
    let class = SERVICE.with(|slot| slot.borrow().clone());
    class.map_or_else(
        || Ok(Symbol::for_(&format!("cordis.{name}")).into()),
        |class| Reflect::get(&class, &name.into()),
    )
}
