//! Promise reactions preserve synchronous start and live-array cleanup iteration.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use js_sys::{Function, Promise};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};

type Disposers = Rc<RefCell<Vec<JsValue>>>;

struct Mount {
    remote: JsValue,
    contributions: Vec<JsValue>,
    index: Cell<usize>,
    disposers: Disposers,
    resolve: Function,
    reject: Function,
}

pub(super) fn mount(remote: &JsValue, contributions: &[JsValue]) -> Promise {
    Promise::new(&mut |resolve, reject| {
        mount_next(&Rc::new(Mount {
            remote: remote.clone(),
            contributions: contributions.to_vec(),
            index: Cell::new(0),
            disposers: Rc::new(RefCell::new(Vec::new())),
            resolve,
            reject,
        }));
    })
}

fn mount_next(state: &Rc<Mount>) {
    let Some(contribution) = state.contributions.get(state.index.get()) else {
        let disposers = state.disposers.clone();
        let cleanup = Closure::wrap(Box::new(move || {
            disposers.borrow_mut().reverse();
            dispose(&disposers)
        }) as Box<dyn Fn() -> Promise>);
        finish(&state.resolve, &cleanup.into_js_value());
        return;
    };
    let value =
        match super::call_method(&state.remote, "$mount", std::slice::from_ref(contribution)) {
            Ok(value) => value,
            Err(error) => {
                rollback(state, error);
                return;
            }
        };
    let success = state.clone();
    let failure = state.clone();
    if let Err(error) = after(
        &value,
        move |value| {
            success.disposers.borrow_mut().push(value);
            success.index.set(success.index.get() + 1);
            mount_next(&success);
        },
        move |error| rollback(&failure, error),
    ) {
        rollback(state, error);
    }
}

fn rollback(state: &Mount, error: JsValue) {
    state.disposers.borrow_mut().reverse();
    let cleanup = dispose(&state.disposers);
    let rejected = state.reject.clone();
    let cleanup_rejected = state.reject.clone();
    if let Err(error) = after(
        &cleanup,
        move |_| finish(&rejected, &error),
        move |error| finish(&cleanup_rejected, &error),
    ) {
        finish(&state.reject, &error);
    }
}

struct Disposal {
    items: Disposers,
    index: Cell<usize>,
    resolve: Function,
    reject: Function,
}

fn dispose(items: &Disposers) -> Promise {
    Promise::new(&mut |resolve, reject| {
        dispose_next(&Rc::new(Disposal {
            items: items.clone(),
            index: Cell::new(0),
            resolve,
            reject,
        }));
    })
}

fn dispose_next(state: &Rc<Disposal>) {
    let item = state.items.borrow().get(state.index.get()).cloned();
    let Some(item) = item else {
        finish(&state.resolve, &JsValue::UNDEFINED);
        return;
    };
    state.index.set(state.index.get() + 1);
    let value = item
        .dyn_into::<Function>()
        .map_err(|_| js_sys::TypeError::new("dispose is not a function").into())
        .and_then(|function| function.call0(&JsValue::UNDEFINED));
    let value = match value {
        Ok(value) => value,
        Err(error) => {
            finish(&state.reject, &error);
            return;
        }
    };
    let success = state.clone();
    let rejected = state.reject.clone();
    if let Err(error) = after(
        &value,
        move |_| dispose_next(&success),
        move |error| finish(&rejected, &error),
    ) {
        finish(&state.reject, &error);
    }
}

fn after(
    value: &JsValue,
    success: impl FnOnce(JsValue) + 'static,
    failure: impl FnOnce(JsValue) + 'static,
) -> Result<(), JsValue> {
    super::call_method(
        &Promise::resolve(value),
        "then",
        &[
            Closure::once(success).into_js_value(),
            Closure::once(failure).into_js_value(),
        ],
    )?;
    Ok(())
}

fn finish(callback: &Function, value: &JsValue) {
    let _ = callback.call1(&JsValue::UNDEFINED, value);
}
