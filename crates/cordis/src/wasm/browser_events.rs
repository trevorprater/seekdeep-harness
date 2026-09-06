//! JavaScript receiver, synchronous-return, and filter semantics over the core hook ledger.

use std::{
    sync::Arc,
    task::{Context as TaskContext, Poll},
};

use futures::{future::join_all, task::noop_waker_ref};
use js_sys::{Array, Function, Promise, Reflect, Symbol};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};
use wasm_bindgen_futures::{JsFuture, future_to_promise};

use super::{WasmContext, event_args_from_js, event_reply_to_js, js_error, wrap_detached_context};

type BrowserCallback = Arc<dyn Fn(&JsValue, &Array) -> Result<JsValue, JsValue> + Send + Sync>;

#[derive(Clone)]
pub(crate) struct BrowserHook {
    pub owner: JsValue,
    pub callback: BrowserCallback,
}

pub(super) fn dispatch(context: &WasmContext, mode: &str, args: &Array) -> Result<Array, JsValue> {
    let first = args.get(0);
    let receiver = if first.is_object() || first.is_function() || first.is_null() {
        args.shift()
    } else {
        JsValue::NULL
    };
    let name = args
        .shift()
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("event name must be a string"))?;
    if !name.starts_with("internal/") {
        let diagnostic = Array::of4(
            &JsValue::from_str("internal/dispatch"),
            &JsValue::from_str(mode),
            &JsValue::from_str(&name),
            args,
        );
        diagnostic.push(&receiver);
        invoke(context, "emit", &diagnostic)?;
    }
    let filter = if receiver.is_null() || receiver.is_undefined() {
        JsValue::UNDEFINED
    } else {
        Reflect::get(&receiver, &Symbol::for_("cordis.filter"))?
    };
    let callbacks = Array::new();
    for hook in context.inner.events().browser_hooks(&name) {
        let (owner, callback): (JsValue, BrowserCallback) = if let Some(browser) = hook.browser {
            (browser.owner, browser.callback)
        } else {
            let owner = wrap_detached_context(
                hook.owner,
                context.metadata.clone(),
                context.root_face.clone(),
            )
            .map_err(js_error)?;
            let dispatch_context = context.inner.clone();
            let callback: BrowserCallback = Arc::new(move |_, args| {
                let mut future =
                    (hook.listener)(dispatch_context.clone(), event_args_from_js(args));
                let mut task = TaskContext::from_waker(noop_waker_ref());
                match future.as_mut().poll(&mut task) {
                    Poll::Ready(result) => result.map(event_reply_to_js).map_err(js_error),
                    Poll::Pending => Ok(future_to_promise(async move {
                        future.await.map(event_reply_to_js).map_err(js_error)
                    })
                    .into()),
                }
            });
            (owner, callback)
        };
        if !hook.options.global && filter.is_truthy() {
            let filter = filter
                .clone()
                .dyn_into::<Function>()
                .map_err(|_| js_sys::TypeError::new("event receiver filter must be callable"))?;
            if !filter.call1(&receiver, &owner)?.is_truthy() {
                continue;
            }
        }
        let receiver = receiver.clone();
        let bound = Closure::wrap(Box::new(move |args: Array| callback(&receiver, &args))
            as Box<dyn Fn(Array) -> Result<JsValue, JsValue>>)
        .into_js_value();
        callbacks.push(&variadic(&bound)?);
    }
    Ok(callbacks)
}

fn variadic(callback: &JsValue) -> Result<JsValue, JsValue> {
    // Only JavaScript can expose a variadic function; dispatch policy stays in Rust.
    Function::new_with_args(
        "callback",
        "return function (...args) { return callback(args); }",
    )
    .call1(&JsValue::UNDEFINED, callback)
}

fn call(callback: &JsValue, args: &Array) -> Result<JsValue, JsValue> {
    callback
        .clone()
        .unchecked_into::<Function>()
        .apply(&JsValue::UNDEFINED, args)
}

fn bailed(value: &JsValue) -> bool {
    !value.is_null() && !value.is_undefined() && value.as_bool() != Some(false)
}

pub(super) fn invoke(context: &WasmContext, mode: &str, args: &Array) -> Result<JsValue, JsValue> {
    // Source parallel dispatch reports "emit" to internal/dispatch observers.
    let callbacks = dispatch(
        context,
        if mode == "parallel" { "emit" } else { mode },
        args,
    )?;
    match mode {
        "emit" => {
            for callback in callbacks.iter() {
                call(&callback, args)?;
            }
            Ok(JsValue::UNDEFINED)
        }
        "bail" => {
            for callback in callbacks.iter() {
                let result = call(&callback, args)?;
                if bailed(&result) {
                    return Ok(result);
                }
            }
            Ok(JsValue::UNDEFINED)
        }
        "serial" => {
            let args = args.clone();
            Ok(future_to_promise(async move {
                for callback in callbacks.iter() {
                    let result = JsFuture::from(Promise::resolve(&call(&callback, &args)?)).await?;
                    if bailed(&result) {
                        return Ok(result);
                    }
                }
                Ok(JsValue::UNDEFINED)
            })
            .into())
        }
        "parallel" => {
            let pending = callbacks
                .iter()
                .map(|callback| {
                    let result = call(&callback, args);
                    async move { JsFuture::from(Promise::resolve(&result?)).await }
                })
                .collect::<Vec<_>>();
            Ok(future_to_promise(async move {
                let errors = Array::new();
                for result in join_all(pending).await {
                    if let Err(error) = result {
                        errors.push(&error);
                    }
                }
                if errors.length() == 0 {
                    Ok(JsValue::UNDEFINED)
                } else {
                    Err(js_sys::AggregateError::new(&errors.to_vec()).into())
                }
            })
            .into())
        }
        _ => Err(js_sys::TypeError::new("unsupported browser event dispatch mode").into()),
    }
}
