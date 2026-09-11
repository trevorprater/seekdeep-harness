//! JavaScript receiver, synchronous-return, and filter semantics over the core hook ledger.

use std::{
    sync::Arc,
    task::{Context as TaskContext, Poll},
};

use futures::task::noop_waker_ref;
use js_sys::{Array, Function, Object, Promise, Reflect};
use parking_lot::Mutex;
use uuid::Uuid;
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};
use wasm_bindgen_futures::{JsFuture, future_to_promise};

use super::{
    Context, EventOptions, FiberState, WasmContext, effect_disposer, event_args_from_js,
    event_args_to_js, event_reply_from_js, event_reply_to_js, js_anyhow, js_error, object,
    wrap_context, wrap_detached_context,
};

mod service_api;

pub(super) fn service_prototype() -> Result<Object, JsValue> {
    service_api::prototype()
}
pub(super) fn configure_service_prototype(prototype: Object) {
    service_api::configure(prototype);
}
pub(super) fn create_service(
    owner: &JsValue,
    prototype: Option<Object>,
) -> Result<Object, JsValue> {
    service_api::create(owner, prototype)
}
pub(super) fn remember_context(face: &JsValue, context: WasmContext) {
    service_api::remember_context(face, context);
}
pub(super) fn remember_service(service: &JsValue, owner: &JsValue) {
    service_api::remember_service(service, owner);
}
pub(super) fn is_bailed(value: &JsValue) -> bool {
    bailed(value)
}

type BrowserCallback = Arc<dyn Fn(&JsValue, &Array) -> Result<JsValue, JsValue> + Send + Sync>;

type ContextFace = Arc<dyn Fn(Context) -> Result<JsValue, JsValue> + Send + Sync>;

struct NativeRegistration {
    id: Uuid,
    list: JsValue,
    callback: JsValue,
    hook: crate::events::Hook,
}

#[derive(Clone)]
pub(crate) struct HookTable {
    pub service: Object,
    context_face: ContextFace,
    native: Arc<Mutex<Vec<NativeRegistration>>>,
}

impl HookTable {
    fn value(&self) -> Result<JsValue, JsValue> {
        Reflect::get(&self.service, &"_hooks".into())
    }

    pub(crate) fn insert_native(
        &self,
        name: &str,
        hook: crate::events::Hook,
    ) -> Result<(), JsValue> {
        let owner = (self.context_face)(hook.owner.clone())?;
        let listener = native_callback(hook.clone(), hook.owner.clone());
        let callback = receiver_function(move |receiver, args| listener(&receiver, &args))?;
        let options = object(&[
            ("global", hook.options.global.into()),
            ("prepend", hook.options.prepend.into()),
        ])?;
        let list = self.list(&name.into())?;
        insert_record(&list, &owner, &callback, &options)?;
        self.native.lock().push(NativeRegistration {
            id: hook.id,
            list,
            callback: callback.into(),
            hook,
        });
        Ok(())
    }

    pub(crate) fn remove_native(&self, id: Uuid) -> Result<(), JsValue> {
        let entry = {
            let mut native = self.native.lock();
            native
                .iter()
                .position(|entry| entry.id == id)
                .map(|index| native.remove(index))
        };
        if let Some(entry) = entry {
            unregister(&entry.list, &entry.callback)?;
        }
        Ok(())
    }

    fn list(&self, name: &JsValue) -> Result<JsValue, JsValue> {
        let table = self.value()?;
        let list = Reflect::get(&table, name)?;
        if list.is_truthy() {
            return Ok(list);
        }
        let list = Array::new();
        if !Reflect::set(&table, name, &list)? {
            return Err(js_sys::TypeError::new("Cannot assign event hook list").into());
        }
        Ok(list.into())
    }

    pub(crate) fn selected_native(
        &self,
        context: &Context,
        name: &str,
        mode: crate::events::DispatchMode,
    ) -> anyhow::Result<Vec<crate::events::Hook>> {
        let receiver = if matches!(name, "internal/plugin" | "internal/status") {
            JsValue::NULL
        } else {
            (self.context_face)(context.clone()).map_err(|error| js_anyhow(&error))?
        };
        let records = self
            .records(&name.into(), &receiver)
            .map_err(|error| js_anyhow(&error))?;
        let mut selected = Vec::new();
        for record in records.iter() {
            let callback =
                Reflect::get(&record, &"callback".into()).map_err(|error| js_anyhow(&error))?;
            let native = self
                .native
                .lock()
                .iter()
                .find(|entry| entry.callback == callback)
                .map(|entry| entry.hook.clone());
            if let Some(hook) = native {
                selected.push(hook);
                continue;
            }
            let callback =
                bind_callback(&callback, &receiver).map_err(|error| js_anyhow(&error))?;
            selected.push(crate::events::Hook {
                id: Uuid::nil(),
                owner: context.clone(),
                options: EventOptions {
                    global: true,
                    prepend: false,
                },
                listener: Arc::new(move |_, args| {
                    let result = call(&callback, &event_args_to_js(&args));
                    Box::pin(async move {
                        let value = result.map_err(|error| js_anyhow(&error))?;
                        if mode == crate::events::DispatchMode::Bail {
                            return Ok(event_reply_from_js(value));
                        }
                        let settled = JsFuture::from(Promise::resolve(&value))
                            .await
                            .map_err(|error| js_anyhow(&error))?;
                        Ok(event_reply_from_js(settled))
                    })
                }),
            });
        }
        Ok(selected)
    }

    fn records(&self, name: &JsValue, receiver: &JsValue) -> Result<Array, JsValue> {
        filtered_records(&self.value()?, name, receiver)?.dyn_into()
    }
}

fn filtered_records(
    table: &JsValue,
    name: &JsValue,
    receiver: &JsValue,
) -> Result<JsValue, JsValue> {
    let filter = if receiver.is_null() || receiver.is_undefined() {
        JsValue::UNDEFINED
    } else {
        Reflect::get(receiver, &super::browser_symbols::context_key("filter")?)?
    };
    let list = Reflect::get(table, name)?;
    let list = if list.is_truthy() {
        list
    } else {
        Array::new().into()
    };
    let receiver = receiver.clone();
    let predicate = Closure::wrap(Box::new(move |hook: JsValue| -> Result<bool, JsValue> {
        if Reflect::get(&hook, &"global".into())?.is_truthy() || !filter.is_truthy() {
            return Ok(true);
        }
        let owner = Reflect::get(&hook, &"ctx".into())?;
        filter
            .clone()
            .dyn_into::<Function>()
            .map_err(|_| js_sys::TypeError::new("event receiver filter must be callable"))?
            .call1(&receiver, &owner)
            .map(|value| value.is_truthy())
    }) as Box<dyn Fn(JsValue) -> Result<bool, JsValue>>)
    .into_js_value();
    service_api::method(&list, "filter", &Array::of1(&predicate))
}

fn native_callback(hook: crate::events::Hook, context: Context) -> BrowserCallback {
    Arc::new(move |_, args| {
        let mut future = (hook.listener)(context.clone(), event_args_from_js(args));
        let mut task = TaskContext::from_waker(noop_waker_ref());
        match future.as_mut().poll(&mut task) {
            Poll::Ready(result) => result.map(event_reply_to_js).map_err(js_error),
            Poll::Pending => Ok(future_to_promise(async move {
                future.await.map(event_reply_to_js).map_err(js_error)
            })
            .into()),
        }
    })
}

fn insert_record(
    list: &JsValue,
    owner: &JsValue,
    callback: &JsValue,
    options: &JsValue,
) -> Result<(), JsValue> {
    let method = if Reflect::get(options, &"prepend".into())?.is_truthy() {
        "unshift"
    } else {
        "push"
    };
    push_record(list, owner, callback, options, method)
}

fn push_record(
    list: &JsValue,
    owner: &JsValue,
    callback: &JsValue,
    options: &JsValue,
    method: &str,
) -> Result<(), JsValue> {
    let record = object(&[("ctx", owner.clone()), ("callback", callback.clone())])?;
    if !options.is_null() && !options.is_undefined() {
        let options = super::boxed_object(options);
        for key in Reflect::own_keys(&options)?.iter() {
            let descriptor = Reflect::get_own_property_descriptor(&options, &key)?;
            if Reflect::get(&descriptor, &"enumerable".into())?.is_truthy() {
                Reflect::define_property(
                    &record,
                    &key,
                    &object(&[
                        ("value", Reflect::get(&options, &key)?),
                        ("writable", JsValue::TRUE),
                        ("enumerable", JsValue::TRUE),
                        ("configurable", JsValue::TRUE),
                    ])?,
                )?;
            }
        }
    }
    Reflect::get(list, &method.into())?
        .dyn_into::<Function>()
        .map_err(|_| js_sys::TypeError::new("hooks[method] is not a function"))?
        .call1(list, &record)?;
    Ok(())
}

pub(super) fn unregister(list: &JsValue, callback: &JsValue) -> Result<JsValue, JsValue> {
    let callback = callback.clone();
    let predicate = Closure::wrap(Box::new(move |hook: JsValue| -> Result<bool, JsValue> {
        Ok(Reflect::get(&hook, &"callback".into())? == callback)
    }) as Box<dyn Fn(JsValue) -> Result<bool, JsValue>>)
    .into_js_value();
    let index = Reflect::get(list, &"findIndex".into())?
        .dyn_into::<Function>()?
        .call1(list, &predicate)?;
    if index.as_f64().is_some_and(|index| index >= 0.0) {
        Reflect::get(list, &"splice".into())?
            .dyn_into::<Function>()?
            .call2(list, &index, &1.into())?;
        Ok(JsValue::TRUE)
    } else {
        Ok(JsValue::UNDEFINED)
    }
}

pub(super) fn register_explicit(
    context: &WasmContext,
    label: String,
    list: JsValue,
    callback: JsValue,
    options: &JsValue,
    owner: &JsValue,
) -> Result<JsValue, JsValue> {
    if context.inner.fiber().state() == FiberState::Disposed {
        return Err(super::browser_errors::inactive());
    }
    insert_record(&list, owner, &callback, options)?;
    let rollback_list = list.clone();
    let rollback_callback = callback.clone();
    let effect = crate::fiber::EffectHandle::synchronous(label, move || {
        unregister(&list, &callback).map_err(|error| js_anyhow(&error))?;
        Ok(())
    });
    if let Err(error) = context.inner.own(effect.clone()) {
        unregister(&rollback_list, &rollback_callback)?;
        return Err(js_sys::Error::new(&error.to_string()).into());
    }
    Ok(effect_disposer(effect).into())
}

pub(super) fn table(context: &WasmContext) -> HookTable {
    context
        .inner
        .events()
        .browser_table
        .lock()
        .clone()
        .expect("browser event table installed")
}

pub(super) fn install(context: &WasmContext, owner: &JsValue) -> Result<(), JsValue> {
    let metadata = context.metadata.clone();
    let root_face = context.root_face.clone();
    let service = create_service(owner, None)?;
    let table = HookTable {
        service,
        context_face: Arc::new(move |context| {
            wrap_detached_context(context, metadata.clone(), root_face.clone()).map_err(js_error)
        }),
        native: Arc::default(),
    };
    for (name, hook) in context.inner.events().browser_initial_hooks() {
        table.insert_native(&name, hook)?;
    }
    *context.inner.events().browser_table.lock() = Some(table);
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
    let method = Reflect::get(&list, &method.into())?
        .dyn_into::<Function>()
        .map_err(|_| js_sys::TypeError::new("hooks[method] is not a function"))?;
    Reflect::apply(&method, &list, &Array::of1(listener))
}

fn dispatch_update(receiver: &JsValue, args: &Array) -> Result<JsValue, JsValue> {
    let hooks = Reflect::get(receiver, &"_hooks".into())?;
    let list = Reflect::get(&hooks, &"internal/update".into())?;
    let callbacks = Array::new();
    if list.is_truthy() {
        let iterator = js_sys::try_iter(&list)?
            .ok_or_else(|| js_sys::TypeError::new("update hooks are not iterable"))?;
        for callback in iterator {
            callbacks.push(&callback?);
        }
    }
    let args = args.slice(0, args.length());
    let inner = args.pop();
    let invoke = Closure::wrap(Box::new(
        move |callbacks: Array, receiver: JsValue, args: Array, inner: JsValue| {
            let callback = callbacks.shift();
            let callback = if callback.is_null() || callback.is_undefined() {
                inner
            } else {
                callback
            };
            let forwarded = Array::of1(&receiver);
            for arg in args.iter() {
                forwarded.push(&arg);
            }
            service_api::method(&callback, "call", &forwarded)
        },
    )
        as Box<dyn Fn(Array, JsValue, Array, JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value();
    let make_next = Function::new_with_args(
        "invoke,callbacks,receiver,args,inner",
        "return function next() { return invoke(callbacks,receiver,args,inner); };",
    );
    let values = Array::of4(&invoke, &callbacks, receiver, &args);
    values.push(&inner);
    let next = Reflect::apply(&make_next, &JsValue::UNDEFINED, &values)?;
    args.push(&next);
    call(&next, &Array::new())
}

pub(super) fn register(
    context: &WasmContext,
    name: &JsValue,
    listener: &JsValue,
    options: &JsValue,
    owner: Option<JsValue>,
) -> Result<JsValue, JsValue> {
    let owner = match owner {
        Some(owner) => owner,
        None => wrap_context(context.clone_for_binding())?,
    };
    service_api::method(
        &context.events_face(&owner)?,
        "on",
        &Array::of3(name, listener, options),
    )
}

pub(super) fn once(
    context: &WasmContext,
    name: &JsValue,
    listener: &JsValue,
    options: &JsValue,
    owner: Option<JsValue>,
) -> Result<JsValue, JsValue> {
    let owner = match owner {
        Some(owner) => owner,
        None => wrap_context(context.clone_for_binding())?,
    };
    service_api::method(
        &context.events_face(&owner)?,
        "once",
        &Array::of3(name, listener, options),
    )
}

pub(super) fn dispatch(context: &WasmContext, mode: &str, args: &Array) -> Result<Array, JsValue> {
    let service = context.events_face(&wrap_context(context.clone_for_binding())?)?;
    dispatch_service(Some(context), &service, &mode.into(), args)?.dyn_into()
}

fn dispatch_service(
    context: Option<&WasmContext>,
    service: &JsValue,
    mode: &JsValue,
    args: &Array,
) -> Result<JsValue, JsValue> {
    let first = args.get(0);
    let receiver = if first.is_object() || first.is_function() || first.is_null() {
        args.shift()
    } else {
        JsValue::NULL
    };
    let name = args.shift();
    if !is_internal(&name)? {
        let diagnostic = Array::of4(&JsValue::from_str("internal/dispatch"), mode, &name, args);
        diagnostic.push(&receiver);
        service_api::method(service, "emit", &diagnostic)?;
    }
    let hooks = filtered_records(&Reflect::get(service, &"_hooks".into())?, &name, &receiver)?;
    let context = context.map(WasmContext::clone_for_binding);
    let mapper = Closure::wrap(Box::new(move |hook: JsValue| -> Result<JsValue, JsValue> {
        let callback = Reflect::get(&hook, &"callback".into())?;
        let native = context
            .as_ref()
            .and_then(|context| context.inner.events().browser_table.lock().clone())
            .and_then(|table| {
                table
                    .native
                    .lock()
                    .iter()
                    .find(|entry| entry.callback == callback)
                    .map(|entry| entry.hook.clone())
            });
        if let Some(hook) = native {
            let listener = native_callback(
                hook,
                context.as_ref().expect("native Context").inner.clone(),
            );
            let callback = receiver_function(move |receiver, args| listener(&receiver, &args))?;
            return Ok(callback.bind0(&receiver).into());
        }
        bind_callback(&callback, &receiver)
    }) as Box<dyn Fn(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value();
    service_api::method(&hooks, "map", &Array::of1(&mapper))
}

fn is_internal(name: &JsValue) -> Result<bool, JsValue> {
    if name.is_null() || name.is_undefined() {
        let kind = if name.is_null() { "null" } else { "undefined" };
        return Err(js_sys::TypeError::new(&format!(
            "Cannot read properties of {kind} (reading 'startsWith')"
        ))
        .into());
    }
    let method = super::get_with_receiver(
        super::boxed_object(name).as_ref(),
        &"startsWith".into(),
        name,
    )?;
    let method = method
        .dyn_into::<Function>()
        .map_err(|_| js_sys::TypeError::new("name.startsWith is not a function"))?;
    Reflect::apply(&method, name, &Array::of1(&"internal/".into())).map(|value| value.is_truthy())
}

fn bind_callback(callback: &JsValue, receiver: &JsValue) -> Result<JsValue, JsValue> {
    let bind = Reflect::get(callback, &"bind".into())?
        .dyn_into::<Function>()
        .map_err(|_| js_sys::TypeError::new("hook.callback.bind is not a function"))?;
    Reflect::apply(&bind, callback, &Array::of1(receiver))
}

fn call(callback: &JsValue, args: &Array) -> Result<JsValue, JsValue> {
    let function = callback
        .clone()
        .dyn_into::<Function>()
        .map_err(|_| js_sys::TypeError::new("cb is not a function"))?;
    Reflect::apply(&function, &JsValue::UNDEFINED, args)
}

fn bailed(value: &JsValue) -> bool {
    !value.is_null() && !value.is_undefined() && value.as_bool() != Some(false)
}

pub(super) fn invoke(context: &WasmContext, mode: &str, args: &Array) -> Result<JsValue, JsValue> {
    let service = context.events_face(&wrap_context(context.clone_for_binding())?)?;
    service_api::method(&service, mode, args)
}

fn parallel_results(pending: &JsValue) -> Result<Promise, JsValue> {
    let settled = Closure::wrap(Box::new(move |results: Array| -> Result<JsValue, JsValue> {
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
    }) as Box<dyn Fn(Array) -> Result<JsValue, JsValue>>)
    .into_js_value();
    then(
        &Promise::all_settled(pending.unchecked_ref::<Array>()),
        &settled,
    )
}

fn then(promise: &Promise, continuation: &JsValue) -> Result<Promise, JsValue> {
    Reflect::get(promise, &JsValue::from_str("then"))?
        .dyn_into::<Function>()?
        .call1(promise, continuation)
        .map(wasm_bindgen::JsCast::unchecked_into)
}

fn waterfall_value(callbacks: &JsValue, args: &Array) -> Result<JsValue, JsValue> {
    let inner = args.pop();
    let invoke = Closure::wrap(
        Box::new(move |callbacks: JsValue, args: Array, inner: JsValue| {
            let callback = service_api::method(&callbacks, "shift", &Array::new())?;
            call(
                if callback.is_undefined() || callback.is_null() {
                    &inner
                } else {
                    &callback
                },
                &args,
            )
        }) as Box<dyn Fn(JsValue, Array, JsValue) -> Result<JsValue, JsValue>>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn browser_context() -> (Context, WasmContext, JsValue) {
        let inner = Context::new();
        let root_face = super::super::empty_face_slot();
        let binding = WasmContext::new(
            inner.clone(),
            Object::new().into(),
            root_face.clone(),
            super::super::empty_face_slot(),
        );
        let face = wrap_context(binding.clone_for_binding()).unwrap();
        *root_face.lock() = Some(face.clone());
        *binding.fiber_face.lock() =
            Some(super::super::root_fiber_face(&face, inner.fiber()).unwrap());
        install(&binding, &face).unwrap();
        (inner, binding, face)
    }

    #[wasm_bindgen_test]
    fn native_payloads_and_browser_table_mutations_share_one_dispatch() {
        struct Payload(u32);

        let (inner, binding, face) = browser_context();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let native_calls = calls.clone();
        let effect = inner
            .events()
            .on_sync(
                &inner,
                "test/native-table",
                move |_, args| {
                    native_calls.lock().push(args.get::<Payload>(0).unwrap().0);
                    Ok(crate::EventReply::Undefined)
                },
                EventOptions::default(),
            )
            .unwrap();
        let args = crate::EventArgs::one(Payload(41));
        inner
            .events()
            .emit(&inner, "test/native-table", &args)
            .unwrap();
        assert_eq!(*calls.lock(), [41]);
        let table = table(&binding);
        let list = table.list(&"test/native-table".into()).unwrap();
        let hook = Array::from(&list).get(0);
        let native_callback = Reflect::get(&hook, &"callback".into()).unwrap();
        let changed_calls = calls.clone();
        let replacement = Closure::wrap(Box::new(move || {
            changed_calls.lock().push(99);
        }) as Box<dyn Fn()>)
        .into_js_value();
        Reflect::set(&hook, &"callback".into(), &replacement).unwrap();
        inner
            .events()
            .emit(&inner, "test/native-table", &crate::EventArgs::new())
            .unwrap();
        invoke(&binding, "emit", &Array::of1(&"test/native-table".into())).unwrap();
        assert_eq!(*calls.lock(), [41, 99, 99]);
        Reflect::set(&hook, &"callback".into(), &native_callback).unwrap();
        futures::executor::block_on(effect.dispose()).unwrap();
        assert_eq!(Array::from(&list).length(), 0);
        inner
            .events()
            .emit(&inner, "test/native-table", &args)
            .unwrap();
        assert_eq!(*calls.lock(), [41, 99, 99]);
        let custom =
            Function::new_no_args("return { bind(receiver) { if (!receiver) throw new Error('missing receiver'); return () => 17; } };")
                .call0(&JsValue::UNDEFINED)
                .unwrap();
        let custom_list = table.list(&"test/custom-native-bind".into()).unwrap();
        insert_record(&custom_list, &face, &custom, &Object::new()).unwrap();
        let reply = inner
            .events()
            .bail(&inner, "test/custom-native-bind", &crate::EventArgs::new())
            .unwrap();
        let crate::BailReply::Settled(crate::EventReply::Value(reply)) = reply else {
            panic!("custom bind did not return its native bail value");
        };
        assert_eq!(
            reply.downcast_ref::<JsValue>().unwrap().as_f64(),
            Some(17.0)
        );
        let promise = Promise::resolve(&JsValue::from_f64(29.0));
        let returned = promise.clone();
        let callback = receiver_function(move |_, _| Ok(returned.clone().into())).unwrap();
        let promises = table.list(&"test/native-bail-promise".into()).unwrap();
        insert_record(&promises, &face, &callback, &Object::new()).unwrap();
        let reply = inner
            .events()
            .bail(&inner, "test/native-bail-promise", &crate::EventArgs::new())
            .unwrap();
        let crate::BailReply::Settled(crate::EventReply::Value(reply)) = reply else {
            panic!("native bail wrapped the returned Promise");
        };
        assert_eq!(
            reply.downcast_ref::<JsValue>().unwrap(),
            &JsValue::from(promise)
        );
    }
}
