//! Browser console sink implemented in Rust/WASM.

use std::sync::Arc;

use js_sys::{Array, Function, Reflect};
use seekdeep_cordis::{Context, Plugin, fiber::EffectHandle};
use wasm_bindgen::{JsCast as _, JsValue};

use crate::{
    Config, ConsoleMethod, ConsoleRecord, ConsoleSink, INJECT, NAME, install_browser_with_sink,
};

/// Installs the browser-native console exporter.
///
/// # Errors
///
/// Returns config, browser-console lookup, or lifecycle failures.
pub fn install_browser(context: &Context, config: &Config) -> anyhow::Result<EffectHandle> {
    let console = Reflect::get(&js_sys::global(), &JsValue::from_str("console"))
        .map_err(|_| anyhow::anyhow!("browser console is unavailable"))?;
    let sink: ConsoleSink = Arc::new(move |record| {
        let ConsoleRecord::Browser {
            method,
            prefix,
            args,
        } = record
        else {
            return;
        };
        let method = match method {
            ConsoleMethod::Error => "error",
            ConsoleMethod::Warn => "warn",
            ConsoleMethod::Log => "log",
        };
        let Ok(function) = Reflect::get(&console, &JsValue::from_str(method)) else {
            return;
        };
        let Some(function) = function.dyn_ref::<Function>() else {
            return;
        };
        let values = Array::new();
        values.push(&JsValue::from_str(&prefix));
        for value in args {
            values.push(
                &serde_wasm_bindgen::to_value(&value)
                    .unwrap_or_else(|_| JsValue::from_str(&value.to_string())),
            );
        }
        let _ = function.apply(&console, &values);
    });
    install_browser_with_sink(context, config, sink)
}

/// Builds the browser Loader plugin.
#[must_use]
pub fn browser_plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, config| {
        Box::pin(async move {
            let config = serde_json::from_value(config)?;
            install_browser(&context, &config)?;
            Ok(())
        })
    })
    .with_config_validator(crate::normalize_config)
}
