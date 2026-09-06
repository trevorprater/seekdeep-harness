//! JavaScript receiver, synchronous-return, and filter semantics over the core hook ledger.

use std::{
    sync::Arc,
    task::{Context as TaskContext, Poll},
};

use futures::task::noop_waker_ref;
use js_sys::{Array, Function, Promise, Reflect, Symbol};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};
use wasm_bindgen_futures::{JsFuture, future_to_promise};

use super::{
    Context, EventArgs, EventOptions, FiberState, WasmContext, effect_disposer, event_args_from_js,
    event_args_to_js, event_reply_from_js, event_reply_to_js, js_anyhow, js_error, object,
    wrap_context, wrap_detached_context,
};

type BrowserCallback = Arc<dyn Fn(&JsValue, &Array) -> Result<JsValue, JsValue> + Send + Sync>;

#[derive(Clone)]
pub(crate) struct BrowserHook {
    pub owner: JsValue,
    pub callback: BrowserCallback,
}

pub(super) fn install(context: &WasmContext, owner: &JsValue) -> Result<(), JsValue> {
    let listener = receiver_function(move |receiver, args| {
        if args.get(0).as_string().as_deref() != Some("internal/update")
            || Reflect::get(&args.get(2), &"global".into())?.is_truthy()
        {
            return Ok(JsValue::UNDEFINED);
        }
        register_update(&receiver, &args.get(1), &args.get(2))
    })?;
    register_hook(
        context,
        "internal/listener".into(),
        listener,
        EventOptions::default(),
        owner.clone(),
    )?;
    let update = receiver_function(move |receiver, args| dispatch_update(&receiver, &args))?;
    register_hook(
        context,
        "internal/update".into(),
        update,
        EventOptions {
            global: true,
            prepend: true,
        },
        owner.clone(),
    )?;
    Ok(())
}

fn receiver_function(
    callback: impl Fn(JsValue, Array) -> Result<JsValue, JsValue> + 'static,
) -> Result<Function, JsValue> {
    let callback = Closure::wrap(
        Box::new(callback) as Box<dyn Fn(JsValue, Array) -> Result<JsValue, JsValue>>
    )
    .into_js_value();
    Function::new_with_args(
        "callback",
        "'use strict'; return function (...args) { return callback(this, args); }",
    )
    .call1(&JsValue::UNDEFINED, &callback)?
    .dyn_into()
}

fn register_update(
    owner: &JsValue,
    listener: &JsValue,
    options: &JsValue,
) -> Result<JsValue, JsValue> {
    let fiber = Reflect::get(owner, &"fiber".into())?;
    let hooks = Reflect::get(&fiber, &"_hooks".into())?;
    let key = JsValue::from_str("internal/update");
    let mut list = Reflect::get(&hooks, &key)?;
    if list.is_null() || list.is_undefined() {
        list = super::update_hooks::disposable_list()?;
        Reflect::set(&hooks, &key, &list)?;
    }
    let method = if Reflect::get(options, &"prepend".into())?.is_truthy() {
        "unshift"
    } else {
        "push"
    };
    Reflect::get(&list, &method.into())?
        .dyn_into::<Function>()
        .map_err(|_| js_sys::TypeError::new("hooks[method] is not a function"))?
        .call1(&list, listener)
}

fn dispatch_update(receiver: &JsValue, args: &Array) -> Result<JsValue, JsValue> {
    let hooks = Reflect::get(receiver, &"_hooks".into())?;
    let list = Reflect::get(&hooks, &"internal/update".into())?;
    let callbacks = Array::new();
    if list.is_truthy() {
        for callback in Array::from(&list).iter() {
            callbacks.push(&callback.dyn_into::<Function>()?.bind0(receiver));
        }
    }
    let args = args.slice(0, args.length());
    let inner = args.pop().dyn_into::<Function>()?.bind0(receiver);
    args.push(&inner);
    waterfall(&callbacks, &args)
}

pub(super) fn register(
    context: &WasmContext,
    name: String,
    listener: JsValue,
    options: JsValue,
    owner: Option<JsValue>,
) -> Result<JsValue, JsValue> {
    if context.inner.fiber().state() == FiberState::Disposed {
        return Err(js_sys::Error::new(&crate::CordisError::InactiveEffect.to_string()).into());
    }
    let listener = listener
        .dyn_into::<Function>()
        .map_err(|_| js_sys::TypeError::new("ctx.on listener must be a function"))?;
    let options = if options.is_object() || options.is_null() {
        options
    } else {
        object(&[("prepend", options)])?.into()
    };
    let owner = match owner {
        Some(owner) => owner,
        None => wrap_context(context.clone_for_binding())?,
    };
    let listener = super::tracing::Tracer::new(context.inner.clone(), owner.clone())
        .bind(&listener)?
        .dyn_into::<Function>()?;
    let interception = Array::of4(
        &owner,
        &JsValue::from_str("internal/listener"),
        &JsValue::from_str(&name),
        &listener,
    );
    interception.push(&options);
    let intercepted = invoke(context, "bail", &interception)?;
    if intercepted.is_truthy() {
        return Ok(intercepted);
    }
    let registration = EventOptions {
        prepend: Reflect::get(&options, &JsValue::from_str("prepend"))?.is_truthy(),
        global: Reflect::get(&options, &JsValue::from_str("global"))?.is_truthy(),
    };
    register_hook(context, name, listener, registration, owner)
}

fn register_hook(
    context: &WasmContext,
    name: String,
    listener: Function,
    registration: EventOptions,
    owner: JsValue,
) -> Result<JsValue, JsValue> {
    let browser_listener = listener.clone();
    let browser = BrowserHook {
        owner,
        callback: Arc::new(move |receiver, args| browser_listener.apply(receiver, args)),
    };
    let root_face = context.root_face.clone();
    let metadata = context.metadata.clone();
    let callback = move |context: Context, args: EventArgs| {
        let listener = listener.clone();
        let root_face = root_face.clone();
        let metadata = metadata.clone();
        Box::pin(async move {
            let this = wrap_detached_context(context, metadata, root_face)?;
            let returned = listener
                .apply(&this, &event_args_to_js(&args))
                .map_err(|error| js_anyhow(&error))?;
            let settled = JsFuture::from(Promise::resolve(&returned))
                .await
                .map_err(|error| js_anyhow(&error))?;
            Ok(event_reply_from_js(settled))
        }) as crate::events::ListenerFuture
    };
    let effect = context
        .inner
        .events()
        .on_browser(&context.inner, name, callback, registration, browser)
        .map_err(|error| js_sys::Error::new(&error.to_string()))?;
    Ok(effect_disposer(effect).into())
}

pub(super) fn once(
    context: &WasmContext,
    name: String,
    listener: &JsValue,
    options: JsValue,
    owner: Option<JsValue>,
) -> Result<JsValue, JsValue> {
    let disposer = Array::new();
    let invoke = Closure::wrap(Box::new(
        move |receiver: JsValue, args: Array, listener: JsValue, slot: Array| {
            if slot.length() == 0 {
                return Err(js_sys::ReferenceError::new(
                    "Cannot access 'dispose' before initialization",
                )
                .into());
            }
            call(&slot.get(0), &Array::new())?;
            listener
                .dyn_into::<Function>()
                .map_err(|_| js_sys::TypeError::new("event listener must be callable"))?
                .apply(&receiver, &args)
        },
    )
        as Box<dyn Fn(JsValue, Array, JsValue, Array) -> Result<JsValue, JsValue>>)
    .into_js_value();
    let wrapper = Function::new_with_args(
        "invoke,listener,slot",
        "'use strict'; return function (...args) { return invoke(this, args, listener, slot); }",
    )
    .apply(
        &JsValue::UNDEFINED,
        &Array::of3(&invoke, listener, &disposer),
    )?;
    let registered = register(context, name, wrapper, options, owner)?;
    disposer.push(&registered);
    Ok(registered)
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
    );
    let callbacks = match callbacks {
        Ok(callbacks) => callbacks,
        Err(error) if matches!(mode, "serial" | "parallel") => {
            return Ok(Promise::reject(&error).into());
        }
        Err(error) => return Err(error),
    };
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
        "serial" => Ok(serial(callbacks, args.clone())?.into()),
        "parallel" => {
            let pending = Array::new();
            for callback in callbacks.iter() {
                pending.push(&match call(&callback, args) {
                    Ok(value) => Promise::resolve(&value).into(),
                    Err(error) => Promise::reject(&error).into(),
                });
            }
            let settled =
                Closure::wrap(Box::new(move |results: Array| -> Result<JsValue, JsValue> {
                    let errors = Array::new();
                    for result in results.iter() {
                        if Reflect::get(&result, &JsValue::from_str("status"))?
                            .as_string()
                            .as_deref()
                            == Some("rejected")
                        {
                            errors.push(&Reflect::get(&result, &JsValue::from_str("reason"))?);
                        }
                    }
                    if errors.length() == 0 {
                        Ok(JsValue::UNDEFINED)
                    } else {
                        Err(js_sys::AggregateError::new(&errors.to_vec()).into())
                    }
                })
                    as Box<dyn Fn(Array) -> Result<JsValue, JsValue>>)
                .into_js_value();
            then(&Promise::all_settled(&pending), &settled).map(Into::into)
        }
        "waterfall" => waterfall(&callbacks, args),
        _ => Err(js_sys::TypeError::new("unsupported browser event dispatch mode").into()),
    }
}

fn then(promise: &Promise, continuation: &JsValue) -> Result<Promise, JsValue> {
    Reflect::get(promise, &JsValue::from_str("then"))?
        .dyn_into::<Function>()?
        .call1(promise, continuation)
        .map(wasm_bindgen::JsCast::unchecked_into)
}

fn serial(callbacks: Array, args: Array) -> Result<Promise, JsValue> {
    let callback = callbacks.shift();
    if callback.is_undefined() {
        return Ok(Promise::resolve(&JsValue::UNDEFINED));
    }
    let result = match call(&callback, &args) {
        Ok(result) => result,
        Err(error) => return Ok(Promise::reject(&error)),
    };
    let settled = Closure::wrap(Box::new(move |value: JsValue| -> Result<JsValue, JsValue> {
        if bailed(&value) {
            Ok(value)
        } else {
            serial(callbacks.clone(), args.clone()).map(Into::into)
        }
    }) as Box<dyn Fn(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value();
    then(&Promise::resolve(&result), &settled)
}

fn waterfall(callbacks: &Array, args: &Array) -> Result<JsValue, JsValue> {
    let inner = args.pop();
    let invoke = Closure::wrap(
        Box::new(move |callbacks: Array, args: Array, inner: JsValue| {
            let callback = callbacks.shift();
            call(
                if callback.is_undefined() {
                    &inner
                } else {
                    &callback
                },
                &args,
            )
        }) as Box<dyn Fn(Array, Array, JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value();
    // The continuation's self-reference is a JavaScript-only cycle, collectible by the browser.
    let next = Function::new_with_args(
        "invoke,callbacks,args,inner",
        "return function next() { return invoke(callbacks, args, inner); }",
    )
    .apply(
        &JsValue::UNDEFINED,
        &Array::of4(&invoke, callbacks, args, &inner),
    )?;
    args.push(&next);
    call(&next, &Array::new())
}
