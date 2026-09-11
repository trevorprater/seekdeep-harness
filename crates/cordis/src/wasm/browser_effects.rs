//! Browser effect setup, ownership transfer, and awaitable cleanup.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use js_sys::{Array, Function, Object, Promise, Reflect, Symbol, WeakMap};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};
use wasm_bindgen_futures::{JsFuture, future_to_promise};

use super::{js_anyhow, object, set};
use crate::fiber::EffectHandle;

thread_local! {
    static INERTIA: WeakMap = WeakMap::new();
}

fn method(value: &JsValue, name: &str, args: &Array) -> Result<JsValue, JsValue> {
    let function = Reflect::get(value, &name.into())?
        .dyn_into::<Function>()
        .map_err(|_| js_sys::TypeError::new(&format!("{name} is not a function")))?;
    Reflect::apply(&function, value, args)
}

fn thenable(value: &JsValue) -> Result<bool, JsValue> {
    Ok((value.is_object() || value.is_function()) && Reflect::has(value, &"then".into())?)
}

fn invalid_effect() -> JsValue {
    js_sys::TypeError::new("Invalid effect").into()
}

fn callback(body: impl Fn(JsValue) -> Result<JsValue, JsValue> + 'static) -> JsValue {
    Closure::wrap(Box::new(body) as Box<dyn Fn(JsValue) -> Result<JsValue, JsValue>>)
        .into_js_value()
}

fn disposer(body: impl Fn() -> Result<JsValue, JsValue> + 'static) -> Result<JsValue, JsValue> {
    let invoke =
        Closure::wrap(Box::new(body) as Box<dyn Fn() -> Result<JsValue, JsValue>>).into_js_value();
    Function::new_with_args("invoke", "return () => invoke();").call1(&JsValue::UNDEFINED, &invoke)
}

fn promise_then(
    value: &JsValue,
    fulfilled: &JsValue,
    rejected: &JsValue,
) -> Result<JsValue, JsValue> {
    method(
        &Promise::resolve(value),
        "then",
        &Array::of2(fulfilled, rejected),
    )
}

pub(crate) fn run_disposable(dispose: &JsValue) -> Result<JsValue, JsValue> {
    run_disposable_with(dispose, None)
}

fn run_disposable_with(
    dispose: &JsValue,
    composition: Option<&super::browser_stack::Composition>,
) -> Result<JsValue, JsValue> {
    let function = dispose
        .clone()
        .dyn_into::<Function>()
        .map_err(|_| invalid_effect())?;
    let result = if let Some(composition) = composition {
        composition.call(&function, &JsValue::UNDEFINED, &Array::new())?
    } else {
        Reflect::apply(&function, &JsValue::UNDEFINED, &Array::new())?
    };
    let inertia = INERTIA.with(|entries| entries.get(dispose.unchecked_ref::<Object>()));
    if inertia.is_function() {
        let pending = Reflect::apply(
            inertia.unchecked_ref::<Function>(),
            &JsValue::UNDEFINED,
            &Array::new(),
        )?;
        if !pending.is_null() && !pending.is_undefined() {
            return Ok(pending);
        }
    }
    Ok(result)
}

/// The public list is the browser ownership ledger, including native registrations.
pub(crate) fn install(owner: &JsValue) -> Result<(), JsValue> {
    Reflect::set(
        owner,
        &"_disposables".into(),
        &super::update_hooks::disposable_list()?,
    )?;
    Ok(())
}

pub(super) fn finish_root_initialization(owner: &JsValue) -> Result<(), JsValue> {
    let list = Reflect::get(owner, &"_disposables".into())?;
    // Built-in services outlive root restarts; constructor registrations are not user effects.
    method(&list, "clear", &Array::new())?;
    Ok(())
}

pub(crate) fn snapshot(owner: &JsValue) -> Result<Vec<EffectHandle>, JsValue> {
    let list = Reflect::get(owner, &"_disposables".into())?;
    let cleared = method(&list, "clear", &Array::new())?;
    let outer = super::browser_stack::owner_stack(owner)?;
    // Native teardown reverses its captured batch; DisposableList.clear already did so.
    Ok(Array::from(&cleared)
        .iter()
        .rev()
        .map(|dispose| {
            let outer = outer.clone();
            EffectHandle::new("browser disposer", move || {
                Box::pin(async move {
                    let composition = super::browser_stack::Composition::disposal(outer)
                        .map_err(|error| js_anyhow(&error))?;
                    let value = composition
                        .run(|| run_disposable_with(&dispose, Some(&composition)))
                        .map_err(|error| js_anyhow(&error))?;
                    JsFuture::from(Promise::resolve(&value))
                        .await
                        .map_err(|error| js_anyhow(&error))?;
                    Ok(())
                })
            })
        })
        .collect())
}

pub(super) fn diagnostics(owner: &JsValue) -> Result<Array, JsValue> {
    let list = Reflect::get(owner, &"_disposables".into())?;
    let result = Array::new();
    for dispose in super::update_hooks::spread(&list)?.iter() {
        let metadata = Reflect::get(&dispose, &super::browser_symbols::get("effect")?)?;
        if metadata.is_truthy() {
            result.push(&metadata);
        }
    }
    Ok(result)
}

pub(crate) fn register_native(owner: &JsValue, effect: &EffectHandle) -> Result<(), JsValue> {
    let label = effect.label().to_owned();
    let cleanup = super::native_effect_disposer(effect.clone());
    let setup = callback(move |_| Ok(cleanup.clone().into()));
    let wrapper = effect_on_owner(
        owner.clone(),
        &setup,
        label.into(),
        super::browser_stack::capture_outer()?,
    )?;
    effect.set_browser_disposer(&wrapper);
    Ok(())
}

pub(super) fn remove_native(owner: &JsValue, effect: &EffectHandle) -> Result<(), JsValue> {
    if let Some(dispose) = effect.browser_disposer() {
        let list = Reflect::get(owner, &"_disposables".into())?;
        method(&list, "delete", &Array::of1(&dispose))?;
    }
    Ok(())
}

struct Barrier {
    promise: Promise,
    resolve: Function,
    reject: Function,
}

impl Barrier {
    fn new() -> Self {
        let mut callbacks = None;
        let promise = Promise::new(&mut |resolve, reject| callbacks = Some((resolve, reject)));
        let (resolve, reject) = callbacks.expect("Promise constructor invokes its executor");
        Self {
            promise,
            resolve,
            reject,
        }
    }
}

struct Effect {
    owner: JsValue,
    metadata: Object,
    disposables: RefCell<Vec<JsValue>>,
    runner: Object,
    disposing: Cell<bool>,
    disposal_task: RefCell<JsValue>,
    task: RefCell<JsValue>,
    executing: Cell<bool>,
    setup_failed: Cell<bool>,
    setup_barrier: RefCell<Option<Barrier>>,
    in_flight: RefCell<JsValue>,
    remove: RefCell<JsValue>,
}

impl Effect {
    fn active(&self) -> Result<bool, JsValue> {
        Reflect::get(&self.runner, &"epoch".into()).map(|value| value.is_truthy())
    }

    fn deactivate(&self) -> Result<bool, JsValue> {
        let active = self.active()?;
        super::browser_values::set(&self.runner, &"epoch".into(), &JsValue::FALSE)?;
        Ok(active)
    }

    fn collect(&self, dispose: &JsValue) -> Result<JsValue, JsValue> {
        self.disposables.borrow_mut().push(dispose.clone());
        let list = Reflect::get(&self.owner, &"_disposables".into())?;
        method(&list, "delete", &Array::of1(dispose))?;
        let metadata = Reflect::get(dispose, &super::browser_symbols::get("effect")?)?;
        if metadata.is_truthy() {
            let children = Reflect::get(&self.metadata, &"children".into())?;
            // The source reads the effect marker again after checking its truthiness.
            let metadata = Reflect::get(dispose, &super::browser_symbols::get("effect")?)?;
            method(&children, "push", &Array::of1(&metadata))?;
        }
        Ok(JsValue::UNDEFINED)
    }

    fn dispose(&self) -> Result<JsValue, JsValue> {
        if self.disposing.replace(true) {
            return Ok(self.disposal_task.borrow().clone());
        }
        let disposables = std::mem::take(&mut *self.disposables.borrow_mut());
        let mut task = JsValue::UNDEFINED;
        for dispose in disposables.into_iter().rev() {
            if task.is_truthy() {
                task = method(
                    &task,
                    "then",
                    &Array::of1(&callback(move |_| run_disposable(&dispose))),
                )?;
            } else {
                let result = run_disposable(&dispose)?;
                if thenable(&result)? {
                    task = result;
                }
            }
        }
        *self.disposal_task.borrow_mut() = task.clone();
        Ok(task)
    }

    fn remove(&self) -> Result<(), JsValue> {
        let remove = self.remove.borrow().clone();
        if remove.is_function() {
            Reflect::apply(
                remove.unchecked_ref::<Function>(),
                &JsValue::UNDEFINED,
                &Array::new(),
            )?;
        }
        Ok(())
    }

    fn finalize(
        self: &Rc<Self>,
        body: impl FnOnce() -> Result<JsValue, JsValue>,
    ) -> Result<JsValue, JsValue> {
        let result = match body() {
            Ok(result) => result,
            Err(error) => {
                self.remove()?;
                return Err(error);
            }
        };
        if thenable(&result)? {
            let effect = self.clone();
            let identity = Rc::new(RefCell::new(JsValue::UNDEFINED));
            let pending_identity = identity.clone();
            let finish = callback(move |_| {
                effect.remove()?;
                if *effect.in_flight.borrow() == *pending_identity.borrow() {
                    *effect.in_flight.borrow_mut() = JsValue::UNDEFINED;
                }
                Ok(JsValue::UNDEFINED)
            });
            let pending = method(&Promise::resolve(&result), "finally", &Array::of1(&finish))?;
            *identity.borrow_mut() = pending.clone();
            *self.in_flight.borrow_mut() = pending.clone();
            Ok(pending)
        } else {
            self.remove()?;
            Ok(result)
        }
    }

    fn dispose_after(self: &Rc<Self>, setup: &JsValue) -> Result<JsValue, JsValue> {
        let effect = self.clone();
        let fulfilled = callback(move |_| effect.dispose());
        let effect = self.clone();
        let rejected = callback(move |reason| {
            let cleanup = effect.dispose()?;
            promise_then(
                &cleanup,
                &callback(move |_| Err(reason.clone())),
                &JsValue::UNDEFINED,
            )
        });
        promise_then(setup, &fulfilled, &rejected)
    }

    fn wrapper(self: &Rc<Self>) -> Result<JsValue, JsValue> {
        if !self.deactivate()? {
            return Ok(if self.setup_failed.get() {
                self.in_flight.borrow().clone()
            } else {
                JsValue::UNDEFINED
            });
        }
        self.finalize(|| {
            if self.executing.get() {
                let promise = {
                    let mut barrier = self.setup_barrier.borrow_mut();
                    barrier.get_or_insert_with(Barrier::new).promise.clone()
                };
                self.dispose_after(&promise)
            } else {
                let task = self.task.borrow().clone();
                if task.is_truthy() {
                    self.dispose_after(&task)
                } else {
                    self.dispose()
                }
            }
        })
    }

    fn dispose_async(self: &Rc<Self>) -> Result<JsValue, JsValue> {
        if !self.deactivate()? {
            return Ok(JsValue::UNDEFINED);
        }
        self.finalize(|| self.dispose())
    }

    fn log(&self, error: &JsValue) -> Result<JsValue, JsValue> {
        let context = Reflect::get(&self.owner, &"ctx".into())?;
        let logger = Reflect::get(&context, &"logger".into())?;
        method(&logger, "error", &Array::of1(error))
    }

    fn register_wrapper(self: &Rc<Self>) -> Result<JsValue, JsValue> {
        let called = self.clone();
        let wrapper = disposer(move || called.wrapper())?;
        Reflect::define_property(
            wrapper.unchecked_ref::<Object>(),
            &super::browser_symbols::get("effect")?,
            &object(&[
                ("value", self.metadata.clone().into()),
                ("writable", true.into()),
            ])?,
        )?;
        let pending = self.clone();
        let inertia = callback(move |_| Ok(pending.in_flight.borrow().clone()));
        INERTIA.with(|entries| entries.set(wrapper.unchecked_ref::<Object>(), &inertia));
        let list = Reflect::get(&self.owner, &"_disposables".into())?;
        *self.remove.borrow_mut() = method(&list, "push", &Array::of1(&wrapper))?;
        Ok(wrapper)
    }

    fn install_then(self: &Rc<Self>, wrapper: &JsValue) -> Result<(), JsValue> {
        let settled = self.clone();
        let cleanup = disposer(move || settled.dispose_async())?;
        Reflect::define_property(
            cleanup.unchecked_ref::<Object>(),
            &"name".into(),
            &object(&[
                ("value", "disposeAsync".into()),
                ("configurable", true.into()),
            ])?,
        )?;
        let awaited = self.clone();
        let invoke = Closure::wrap(Box::new(move |_: JsValue, args: Array| -> JsValue {
            let result = (|| {
                let cleanup = cleanup.clone();
                let task = awaited.task.borrow().clone();
                let result = promise_then(
                    &task,
                    &callback(move |_| Ok(cleanup.clone())),
                    &JsValue::UNDEFINED,
                )?;
                method(&result, "then", &Array::of2(&args.get(0), &args.get(1)))
            })();
            match result {
                Ok(result) => {
                    future_to_promise(
                        async move { JsFuture::from(Promise::resolve(&result)).await },
                    )
                    .into()
                }
                Err(error) => Promise::reject(&error).into(),
            }
        }) as Box<dyn Fn(JsValue, Array) -> JsValue>)
        .into_js_value();
        let target = Function::new_no_args("return async (onFulfilled, onRejected) => {};")
            .call0(&JsValue::UNDEFINED)?;
        let handler = Function::new_with_args(
            "invoke",
            "return { apply(_target, receiver, args) { return invoke(receiver,args); } };",
        )
        .call1(&JsValue::UNDEFINED, &invoke)?
        .dyn_into::<Object>()?;
        set(
            wrapper.unchecked_ref(),
            "then",
            &js_sys::Proxy::new(&target, &handler),
        )
    }
}

pub(super) fn effect_on_owner(
    owner: JsValue,
    setup: &JsValue,
    label: JsValue,
    outer: JsValue,
) -> Result<Function, JsValue> {
    let metadata = object(&[("label", label), ("children", Array::new().into())])?;
    let runner = object(&[
        ("execute", setup.clone()),
        ("epoch", JsValue::TRUE),
        ("collect", JsValue::UNDEFINED),
        ("getOuterStack", outer),
    ])?;
    let effect = Rc::new(Effect {
        owner,
        metadata,
        disposables: RefCell::default(),
        runner,
        disposing: Cell::new(false),
        disposal_task: RefCell::new(JsValue::UNDEFINED),
        task: RefCell::new(JsValue::UNDEFINED),
        executing: Cell::new(true),
        setup_failed: Cell::new(false),
        setup_barrier: RefCell::default(),
        in_flight: RefCell::new(JsValue::UNDEFINED),
        remove: RefCell::new(JsValue::UNDEFINED),
    });
    let collector = effect.clone();
    super::browser_values::set(
        &effect.runner,
        &"collect".into(),
        &callback(move |dispose| collector.collect(&dispose)),
    )?;
    let wrapper = effect.register_wrapper()?;
    let result = method(&effect.owner, "_execute", &Array::of1(&effect.runner));
    let task = match result {
        Ok(task) => task,
        Err(reason) => {
            effect.executing.set(false);
            effect.setup_failed.set(true);
            effect.deactivate()?;
            let cleanup = effect.finalize(|| effect.dispose());
            let rejected = effect
                .setup_barrier
                .borrow()
                .as_ref()
                .map(|barrier| barrier.reject.clone());
            if let Some(reject) = rejected {
                Reflect::apply(&reject, &JsValue::UNDEFINED, &Array::of1(&reason))?;
            }
            let cleanup = cleanup?;
            if thenable(&cleanup)? {
                let logged = effect.clone();
                method(
                    &cleanup,
                    "catch",
                    &Array::of1(&callback(move |error| logged.log(&error))),
                )?;
            }
            return Err(reason);
        }
    };
    *effect.task.borrow_mut() = task.clone();
    effect.executing.set(false);
    let barrier = effect
        .setup_barrier
        .borrow()
        .as_ref()
        .map(|barrier| (barrier.resolve.clone(), barrier.reject.clone()));
    if let Some((resolve, reject)) = barrier {
        promise_then(&task, &resolve, &reject)?;
    }
    if !task.is_null() && !task.is_undefined() {
        let rejected = effect.clone();
        let cleanup = method(
            &task,
            "catch",
            &Array::of1(&callback(move |_| {
                if rejected.active()? {
                    rejected.finalize(|| rejected.dispose())
                } else {
                    rejected.dispose()
                }
            })),
        )?;
        let logged = effect.clone();
        method(
            &cleanup,
            "catch",
            &Array::of1(&callback(move |error| logged.log(&error))),
        )?;
    }
    effect.install_then(&wrapper)?;
    Ok(wrapper.unchecked_into())
}

/// Executes the source's result-shape precedence without normalizing an iterable into a promise.
pub(super) fn execute_result(
    result: &JsValue,
    collect: JsValue,
    active: Rc<dyn Fn() -> Result<bool, JsValue>>,
    composition: super::browser_stack::Composition,
) -> Result<JsValue, JsValue> {
    execute_result_inner(result, collect, active, composition)
}

fn execute_result_inner(
    result: &JsValue,
    collect: JsValue,
    active: Rc<dyn Fn() -> Result<bool, JsValue>>,
    composition: super::browser_stack::Composition,
) -> Result<JsValue, JsValue> {
    if result.is_function() {
        return composition.call(&collect, &JsValue::UNDEFINED, &Array::of1(result));
    }
    let collection = composition.clone();
    let safe = callback(move |dispose| {
        if dispose.is_function() {
            collection.call(&collect, &JsValue::UNDEFINED, &Array::of1(&dispose))?;
        } else if !dispose.is_null() && !dispose.is_undefined() {
            return Err(collection.invalid_effect());
        }
        Ok(JsValue::UNDEFINED)
    });
    if result.is_null() || result.is_undefined() {
        return Ok(JsValue::UNDEFINED);
    }
    if !result.is_object() {
        return Err(composition.invalid_effect());
    }
    if composition.has(result, &"then".into())? {
        return composition.method(result, &"then".into(), &Array::of1(&safe));
    }
    let synchronous = composition.has(result, &Symbol::iterator())?;
    let key: JsValue = if synchronous {
        Symbol::iterator().into()
    } else {
        Symbol::async_iterator().into()
    };
    if !synchronous && !composition.has(result, &key)? {
        return Err(composition.invalid_effect());
    }
    if synchronous {
        composition.capture()?;
    }
    let iterator = composition.method(result, &key, &Array::new())?;
    if synchronous {
        loop {
            let result = composition.method(&iterator, &"next".into(), &Array::new())?;
            let value = composition.get(&result, &"value".into())?;
            Reflect::apply(
                safe.unchecked_ref::<Function>(),
                &JsValue::UNDEFINED,
                &Array::of1(&value),
            )?;
            if composition.get(&result, &"done".into())?.is_truthy() {
                return Ok(JsValue::UNDEFINED);
            }
        }
    }
    Ok(future_to_promise(async move {
        composition.capture()?;
        while active()? {
            let result = composition.method(&iterator, &"next".into(), &Array::new())?;
            let result = JsFuture::from(Promise::resolve(&result)).await?;
            let value = composition.get(&result, &"value".into())?;
            Reflect::apply(
                safe.unchecked_ref::<Function>(),
                &JsValue::UNDEFINED,
                &Array::of1(&value),
            )?;
            if composition.get(&result, &"done".into())?.is_truthy() {
                break;
            }
        }
        Ok(JsValue::UNDEFINED)
    })
    .into())
}
