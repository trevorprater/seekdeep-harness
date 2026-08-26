//! JavaScript-bound Remote mount lifecycle.

use std::{cell::RefCell, rc::Rc};

use js_sys::{Array, Function, Promise, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, future_to_promise};

use crate::INJECT;

thread_local! {
    static CONTRIBUTIONS: RefCell<Option<Vec<JsValue>>> = const { RefCell::new(None) };
}

/// Configures the five generated Remote contributions at module materialization.
///
/// # Errors
///
/// Rejects a non-array or wrong-cardinality module factory handoff.
#[wasm_bindgen(js_name = configureApiRemotes)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_api_remotes(contributions: JsValue) -> Result<(), JsValue> {
    if !Array::is_array(&contributions) {
        return Err(js_error(
            "api-remotes: generated contributions must be an array",
        ));
    }
    let contributions = Array::from(&contributions).to_vec();
    if contributions.len() != 5 {
        return Err(js_error(&format!(
            "api-remotes: expected five generated contributions, got {}",
            contributions.len()
        )));
    }
    CONTRIBUTIONS.with(|slot| *slot.borrow_mut() = Some(contributions));
    Ok(())
}

/// Mounts every selected namespace and resolves to its reverse-order disposer.
#[wasm_bindgen(js_name = applyApiRemotes)]
#[allow(clippy::needless_pass_by_value)]
pub fn apply_api_remotes(ctx: JsValue) -> Promise {
    future_to_promise(async move {
        let remote = required_service(&ctx, "remote")?;
        let contributions = CONTRIBUTIONS
            .with(|slot| slot.borrow().clone())
            .ok_or_else(|| {
                js_error("api-remotes module factory did not configure generated contributions")
            })?;
        let mut disposers = Vec::new();
        for contribution in contributions {
            match await_method(&remote, "$mount", &[contribution]).await {
                Ok(disposer) => disposers.push(disposer.dyn_into::<Function>()?),
                Err(error) => {
                    dispose_reverse(&mut disposers).await?;
                    return Err(error);
                }
            }
        }
        let disposers = Rc::new(RefCell::new(disposers));
        let disposer = Closure::wrap(Box::new(move || -> Promise {
            let disposers = Rc::clone(&disposers);
            future_to_promise(async move {
                let ordered = {
                    let mut disposers = disposers.borrow_mut();
                    disposers.reverse();
                    disposers.clone()
                };
                dispose_ordered(&ordered).await?;
                Ok(JsValue::UNDEFINED)
            })
        }) as Box<dyn FnMut() -> Promise>);
        Ok(disposer.into_js_value())
    })
}

/// Exact static inject list.
#[wasm_bindgen(js_name = apiRemotesInject)]
pub fn api_remotes_inject() -> Array {
    let values = Array::new();
    for dependency in INJECT {
        values.push(&JsValue::from_str(dependency));
    }
    values
}

async fn dispose_reverse(disposers: &mut [Function]) -> Result<(), JsValue> {
    disposers.reverse();
    dispose_ordered(disposers).await
}

async fn dispose_ordered(disposers: &[Function]) -> Result<(), JsValue> {
    for disposer in disposers {
        let result = disposer.call0(&JsValue::UNDEFINED)?;
        JsFuture::from(Promise::resolve(&result)).await?;
    }
    Ok(())
}

async fn await_method(
    value: &JsValue,
    name: &str,
    arguments: &[JsValue],
) -> Result<JsValue, JsValue> {
    let result = call_method(value, name, arguments)?;
    JsFuture::from(Promise::resolve(&result)).await
}

fn required_service(ctx: &JsValue, name: &str) -> Result<JsValue, JsValue> {
    let service = call_method(ctx, "get", &[JsValue::from_str(name)])?;
    if service.is_undefined() || service.is_null() {
        Err(js_error(&format!(
            "api-remotes requires Client Service {name:?}"
        )))
    } else {
        Ok(service)
    }
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    method.apply(value, &args)
}

fn js_error(message: &str) -> JsValue {
    js_sys::Error::new(message).into()
}
