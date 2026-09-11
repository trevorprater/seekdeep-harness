//! Browser logger instances, exporter ownership, and source message identity.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Reflect, WeakRef};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use super::{browser_registry::method, browser_symbols, browser_values as values};

pub(super) mod format;

thread_local! {
    static LOGGER: RefCell<Option<Function>> = const { RefCell::new(None) };
    static SERVICE: RefCell<Option<Function>> = const { RefCell::new(None) };
    static CLOCK: RefCell<Option<Function>> = const { RefCell::new(None) };
}

/// Connects the public logger classes and the browser's clock boundary.
#[wasm_bindgen(js_name = configureLoggerBoundary)]
pub fn configure_logger_boundary(logger: Function, service: Function, clock: Function) {
    LOGGER.with(|slot| *slot.borrow_mut() = Some(logger));
    SERVICE.with(|slot| *slot.borrow_mut() = Some(service));
    CLOCK.with(|slot| *slot.borrow_mut() = Some(clock));
}

pub(super) fn logger_class() -> Result<Function, JsValue> {
    LOGGER
        .with(|slot| slot.borrow().clone())
        .ok_or_else(|| js_sys::Error::new("logger class is not configured").into())
}

pub(super) fn log_error(context: &JsValue, error: &JsValue) -> Result<(), JsValue> {
    let logger = values::get(context, &"logger".into())?;
    method(&logger, "error", &Array::of1(error))?;
    Ok(())
}

pub(super) fn create_service(context: &JsValue) -> Result<JsValue, JsValue> {
    if let Some(class) = SERVICE.with(|slot| slot.borrow().clone()) {
        return Reflect::construct(&class, &Array::of1(context));
    }
    // Direct Rust/WASM consumers can configure their own classes before creating roots.
    Ok(JsValue::UNDEFINED)
}

fn increment(owner: &JsValue, key: &str) -> Result<JsValue, JsValue> {
    let value = values::get(owner, &key.into())?;
    let next =
        Function::new_with_args("value", "return ++value;").call1(&JsValue::UNDEFINED, &value)?;
    values::set(owner, &key.into(), &next)?;
    Ok(next)
}

pub(super) fn is_error(value: &JsValue) -> Result<bool, JsValue> {
    let class = Reflect::get(&js_sys::global(), &"Error".into())?;
    Function::new_with_args("value,constructor", "return value instanceof constructor;")
        .call2(&JsValue::UNDEFINED, value, &class)
        .map(|value| value.is_truthy())
}

/// Applies logger options and creates severity methods through the current prototype.
///
/// # Errors
/// Propagates option, factory, and assignment failures.
#[wasm_bindgen(js_name = initializeLogger)]
pub fn initialize_logger(instance: &JsValue, options: &JsValue) -> Result<(), JsValue> {
    values::assign(instance, &Array::of1(options))?;
    for (name, level) in [("error", 0), ("info", 1), ("warn", 2), ("debug", 3)] {
        let callback = method(
            instance,
            "_method",
            &Array::of2(&name.into(), &level.into()),
        )?;
        values::set(instance, &name.into(), &callback)?;
    }
    Ok(())
}

/// Creates one severity function retaining its Logger receiver.
///
/// # Errors
/// Propagates callback construction failures.
#[wasm_bindgen(js_name = loggerMethod)]
pub fn logger_method(logger: JsValue, kind: JsValue, level: JsValue) -> Result<JsValue, JsValue> {
    let callback = Closure::wrap(
        Box::new(move |args: Array| emit(&logger, &kind, &level, &args))
            as Box<dyn Fn(Array) -> Result<(), JsValue>>,
    )
    .into_js_value();
    Function::new_with_args("invoke", "return (...args) => invoke(args);")
        .call1(&JsValue::UNDEFINED, &callback)
}

fn emit(logger: &JsValue, kind: &JsValue, level: &JsValue, args: &Array) -> Result<(), JsValue> {
    let first = args.get(0);
    if args.length() == 1 && is_error(&first)? {
        if values::get(&first, &"cause".into())?.is_truthy() {
            invoke_severity(
                logger,
                kind,
                &Array::of1(&values::get(&first, &"cause".into())?),
            )?;
        } else if is_error(&first)? && Array::is_array(&values::get(&first, &"errors".into())?) {
            let (logger, kind) = (logger.clone(), kind.clone());
            let callback = Closure::wrap(Box::new(move |error: JsValue| {
                invoke_severity(&logger, &kind, &Array::of1(&error))
            })
                as Box<dyn Fn(JsValue) -> Result<JsValue, JsValue>>)
            .into_js_value();
            method(
                &values::get(&first, &"errors".into())?,
                "forEach",
                &Array::of1(&callback),
            )?;
            return Ok(());
        }
    }
    let service = values::get(logger, &"service".into())?;
    let sn = increment(&service, "_snMessage")?;
    let clock = CLOCK
        .with(|slot| slot.borrow().clone())
        .ok_or_else(|| js_sys::Error::new("logger clock is not configured"))?;
    let ts = Reflect::apply(&clock, &JsValue::UNDEFINED, &Array::new())?;
    let service = values::get(logger, &"service".into())?;
    let exporters = values::get(&service, &"exporters".into())?;
    let iterator = method(&exporters, "values", &Array::new())?;
    let iterator = js_sys::try_iter(&iterator)?
        .ok_or_else(|| js_sys::TypeError::new("exporters are not iterable"))?;
    for exporter in iterator {
        let exporter = exporter?;
        let threshold = target_level(&exporter, logger)?;
        if Function::new_with_args("threshold,level", "return threshold < level;")
            .call2(&JsValue::UNDEFINED, &threshold, level)?
            .is_truthy()
        {
            continue;
        }
        let message = super::object(&[
            ("sn", sn.clone()),
            ("ts", ts.clone()),
            ("type", kind.clone()),
            ("level", level.clone()),
            ("name", values::get(logger, &"name".into())?),
        ])?;
        values::assign(&message, &Array::of1(&values::get(logger, &"meta".into())?))?;
        values::set(&message, &"args".into(), args)?;
        method(&exporter, "export", &Array::of1(&message))?;
    }
    Ok(())
}

fn invoke_severity(logger: &JsValue, kind: &JsValue, args: &Array) -> Result<JsValue, JsValue> {
    let callback = values::get(logger, kind)?.dyn_into::<Function>()?;
    Reflect::apply(&callback, logger, args)
}

fn target_level(exporter: &JsValue, logger: &JsValue) -> Result<JsValue, JsValue> {
    let levels = values::get(exporter, &"levels".into())?;
    if !levels.is_null() && !levels.is_undefined() {
        let level = values::get(&levels, &values::get(logger, &"name".into())?)?;
        if !level.is_null() && !level.is_undefined() {
            return Ok(level);
        }
    }
    let levels = values::get(exporter, &"levels".into())?;
    if !levels.is_null() && !levels.is_undefined() {
        let level = values::get(&levels, &"default".into())?;
        if !level.is_null() && !level.is_undefined() {
            return Ok(level);
        }
    }
    let level = values::get(logger, &"level".into())?;
    Ok(if level.is_null() || level.is_undefined() {
        1.into()
    } else {
        level
    })
}

/// Creates the callable logger service and its buffer exporter.
///
/// # Errors
/// Propagates prototype, field, and exporter registration failures.
#[wasm_bindgen(js_name = initializeLoggerService)]
pub fn initialize_logger_service(
    instance: &JsValue,
    context: &JsValue,
) -> Result<JsValue, JsValue> {
    let tracker = super::object(&[("property", "ctx".into()), ("noShadow", true.into())])?;
    let prototype = Reflect::get_prototype_of(instance)?;
    let function = Reflect::get(&js_sys::global(), &"Function".into())?;
    let prototype =
        values::join_prototype(&prototype, &Reflect::get(&function, &"prototype".into())?)?;
    let service = super::browser_service::create_callable(
        &"logger".into(),
        &prototype,
        tracker.clone().into(),
    )?;
    values::assign(&service, &Array::of1(instance))?;
    values::set(&service, &"ctx".into(), context)?;
    values::define(&service, &browser_symbols::get("tracker")?, &tracker)?;
    let buffered = service.clone();
    let export = Closure::wrap(Box::new(move |message: JsValue| -> Result<(), JsValue> {
        method(
            &values::get(&buffered, &"buffer".into())?,
            "push",
            &Array::of1(&message),
        )?;
        let length = values::get(&values::get(&buffered, &"buffer".into())?, &"length".into())?;
        let size = values::get(&buffered, &"bufferSize".into())?;
        if Function::new_with_args("length,size", "return length > size;")
            .call2(&JsValue::UNDEFINED, &length, &size)?
            .is_truthy()
        {
            let size = values::get(&buffered, &"bufferSize".into())?;
            let start = Function::new_with_args("value", "return -value;")
                .call1(&JsValue::UNDEFINED, &size)?;
            let buffer = method(
                &values::get(&buffered, &"buffer".into())?,
                "slice",
                &Array::of1(&start),
            )?;
            values::set(&buffered, &"buffer".into(), &buffer)?;
        }
        Ok(())
    }) as Box<dyn Fn(JsValue) -> Result<(), JsValue>>)
    .into_js_value();
    let exporter = super::object(&[("colors", 3.into()), ("export", export)])?;
    method(&service, "exporter", &Array::of1(&exporter))?;
    Ok(service)
}

/// Registers an exporter using the source's current-counter disposal behavior.
///
/// # Errors
/// Propagates effect admission and map failures.
#[wasm_bindgen(js_name = loggerExporter)]
pub fn logger_exporter(service: JsValue, exporter: JsValue) -> Result<JsValue, JsValue> {
    let context = values::get(&service, &"ctx".into())?;
    let setup = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let exporters = values::get(&service, &"exporters".into())?;
        let serial = increment(&service, "_snExporter")?;
        method(&exporters, "set", &Array::of2(&serial, &exporter))?;
        let service = service.clone();
        let remove = Closure::wrap(Box::new(move || {
            let exporters = values::get(&service, &"exporters".into())?;
            method(
                &exporters,
                "delete",
                &Array::of1(&values::get(&service, &"_snExporter".into())?),
            )
        }) as Box<dyn Fn() -> Result<JsValue, JsValue>>)
        .into_js_value();
        Ok(remove)
    }) as Box<dyn Fn() -> Result<JsValue, JsValue>>)
    .into_js_value();
    method(
        &context,
        "effect",
        &Array::of2(&setup, &"ctx.logger.exporter()".into()),
    )
}

/// Merges the logger intercept chain in ancestor-first order.
///
/// # Errors
/// Propagates intercept traversal and assignment failures.
#[wasm_bindgen(js_name = loggerConfig)]
pub fn logger_config(service: &JsValue) -> Result<JsValue, JsValue> {
    let context = values::get(service, &"ctx".into())?;
    let mut intercept = values::get(&context, &browser_symbols::get("intercept")?)?;
    let configs = Array::new();
    while Reflect::has(&intercept, &"logger".into())? {
        if Object::has_own(intercept.unchecked_ref::<Object>(), &"logger".into()) {
            configs.unshift(&values::get(&intercept, &"logger".into())?);
        }
        intercept = Reflect::get_prototype_of(&intercept)?.into();
    }
    values::assign(&Object::new(), &configs)
}

/// Creates a named logger with the current shadow-aware Fiber metadata.
///
/// # Errors
/// Propagates config, naming, and constructor failures.
#[wasm_bindgen(js_name = loggerInvoke)]
pub fn logger_invoke(service: &JsValue, name: &JsValue) -> Result<JsValue, JsValue> {
    let config = method(service, "_resolveConfig", &Array::new())?;
    let context = values::get(service, &"ctx".into())?;
    let origin = values::get(&context, &browser_symbols::get("shadow")?)?;
    let context = if origin.is_null() || origin.is_undefined() {
        values::get(service, &"ctx".into())?
    } else {
        origin
    };
    let fiber = values::get(&context, &"fiber".into())?;
    let mut name = name.clone();
    if name.is_null() || name.is_undefined() {
        name = values::get(&config, &"name".into())?;
    }
    if name.is_null() || name.is_undefined() {
        name = format::hyphenate(&values::get(&fiber, &"name".into())?)?;
    }
    let metadata = super::object(&[(
        "fiber",
        WeakRef::new(fiber.unchecked_ref::<Object>()).into(),
    )])?;
    let options = super::object(&[
        ("name", name),
        ("level", values::get(&config, &"level".into())?),
        ("meta", metadata.into()),
    ])?;
    Reflect::construct(&logger_class()?, &Array::of2(&options, service))
}

/// Calls a severity method through the service's current callable entrypoint.
///
/// # Errors
/// Propagates callable and severity invocation failures.
#[wasm_bindgen(js_name = loggerForward)]
pub fn logger_forward(service: &JsValue, kind: &JsValue, args: &Array) -> Result<JsValue, JsValue> {
    let logger = Reflect::apply(
        service.unchecked_ref::<Function>(),
        &JsValue::UNDEFINED,
        &Array::new(),
    )?;
    invoke_severity(&logger, kind, args)
}
