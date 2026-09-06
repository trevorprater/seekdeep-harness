//! Symbol-keyed hooks share native Fiber ownership without coercing symbol identity to text.

use std::sync::Arc;

use parking_lot::RwLock;
use wasm_bindgen::JsValue;

use super::{Context, EventOptions, browser_events::BrowserHook};
use crate::fiber::{CordisError, EffectHandle};

#[derive(Clone)]
struct SymbolHook {
    options: EventOptions,
    callback: Arc<BrowserHook>,
}

type Entries = Arc<RwLock<Vec<(JsValue, Vec<SymbolHook>)>>>;

#[derive(Default)]
pub(crate) struct SymbolEvents {
    entries: Entries,
}

impl SymbolEvents {
    pub(super) fn register(
        &self,
        context: &Context,
        name: &JsValue,
        label: String,
        options: EventOptions,
        callback: BrowserHook,
    ) -> Result<EffectHandle, CordisError> {
        let callback = Arc::new(callback);
        let hook = SymbolHook {
            options,
            callback: callback.clone(),
        };
        let mut entries = self.entries.write();
        let hooks = if let Some(index) = entries.iter().position(|(key, _)| key == name) {
            &mut entries[index].1
        } else {
            entries.push((name.clone(), Vec::new()));
            &mut entries.last_mut().expect("inserted symbol entry").1
        };
        if options.prepend {
            hooks.insert(0, hook);
        } else {
            hooks.push(hook);
        }
        drop(entries);
        let registry = self.entries.clone();
        let symbol = name.clone();
        let registered = callback.clone();
        let effect = EffectHandle::synchronous(label, move || {
            remove(&registry, &symbol, &registered);
            Ok(())
        });
        match context.own(effect.clone()) {
            Ok(effect) => Ok(effect),
            Err(error) => {
                remove(&self.entries, name, &callback);
                Err(error)
            }
        }
    }

    pub(super) fn snapshot(&self, name: &JsValue) -> Vec<(EventOptions, BrowserHook)> {
        self.entries
            .read()
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, hooks)| {
                hooks
                    .iter()
                    .map(|hook| (hook.options, (*hook.callback).clone()))
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn remove(entries: &Entries, name: &JsValue, callback: &Arc<BrowserHook>) {
    if let Some((_, hooks)) = entries.write().iter_mut().find(|(key, _)| key == name) {
        hooks.retain(|hook| !Arc::ptr_eq(&hook.callback, callback));
    }
}
