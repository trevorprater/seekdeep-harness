//! Rust-owned Vitest invariant startup interception over real Cordis fibers.

use std::rc::Rc;

use js_sys::{Array, Function, Map, Object, Promise, Reflect, WeakMap, WeakSet};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::test_invariant_paths::{
    TEST_INVARIANT_READY_SERVICE, test_invariant_companion_paths, uses_manual_invariant_tree,
};

use super::{
    browser_registry::{method, resolve_inject},
    browser_values::{define_data, set, spread_into},
    object,
};

const ATTACHMENT_COMPANION: &str = "../packages/attachment/attachment-local/src/invariant.ts";

/// Dependency key that gates ordinary test plugins on companion activation.
#[wasm_bindgen(js_name = testInvariantReadyService)]
pub fn readiness_service() -> String {
    TEST_INVARIANT_READY_SERVICE.to_owned()
}

struct Configuration {
    original_plugin: Function,
    invariant_registry: JsValue,
    attachment_store: JsValue,
    validation_error: JsValue,
    test_path: Function,
    companions: Object,
    hosts: WeakMap,
}

/// Recognizes suites that explicitly own their invariant topology.
#[wasm_bindgen(js_name = usesManualInvariantTree)]
pub fn uses_manual_tree(test_path: &str) -> bool {
    uses_manual_invariant_tree(test_path)
}

/// Resolves source-shaped companion keys from the live lazy-loader map.
///
/// # Errors
/// Preserves missing companion errors.
#[wasm_bindgen(js_name = testInvariantCompanionPaths)]
pub fn companion_paths(test_path: &str, companions: &Object) -> Result<Array, JsValue> {
    let paths = Object::keys(companions)
        .iter()
        .filter_map(|key| key.as_string())
        .collect::<Vec<_>>();
    let paths = test_invariant_companion_paths(test_path, paths)
        .map_err(|error| js_sys::Error::new(&error))?;
    Ok(paths.iter().map(JsValue::from).collect())
}

/// Installs the automatic test invariant host and returns its reversible disposer.
///
/// # Errors
/// Preserves registry property, callback, and lifecycle failures.
#[wasm_bindgen(js_name = installTestInvariantHost)]
pub fn install(
    registry_prototype: Object,
    invariant_registry: JsValue,
    attachment_store: JsValue,
    validation_error: JsValue,
    test_path: Function,
    companions: Object,
) -> Result<Function, JsValue> {
    let original_plugin = field(&registry_prototype, "plugin")?
        .dyn_into::<Function>()
        .map_err(|_| {
            js_sys::TypeError::new("RegistryService.prototype.plugin is not a function")
        })?;
    let config = Rc::new(Configuration {
        original_plugin: original_plugin.clone(),
        invariant_registry,
        attachment_store,
        validation_error,
        test_path,
        companions,
        hosts: WeakMap::new(),
    });
    let invoke = Closure::wrap(Box::new(
        move |registry: JsValue,
              plugin: JsValue,
              value: JsValue,
              outer_stack: JsValue|
              -> Result<JsValue, JsValue> {
            intercepted_plugin(&config, &registry, &plugin, &value, &outer_stack)
        },
    )
        as Box<dyn Fn(JsValue, JsValue, JsValue, JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value();
    let wrapper = Function::new_with_args("invoke", "'use strict'; return function(plugin,config,getOuterStack){ return invoke(this,plugin,config,getOuterStack); };")
        .call1(&JsValue::UNDEFINED, &invoke)?;
    set(&registry_prototype, &"plugin".into(), &wrapper)?;
    let dispose = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if field(&registry_prototype, "plugin")? == wrapper {
            set(&registry_prototype, &"plugin".into(), &original_plugin)?;
        }
        Ok(())
    }) as Box<dyn Fn() -> Result<(), JsValue>>)
    .into_js_value();
    Ok(dispose.unchecked_into())
}

fn intercepted_plugin(
    config: &Rc<Configuration>,
    registry: &JsValue,
    plugin: &JsValue,
    value: &JsValue,
    outer_stack: &JsValue,
) -> Result<JsValue, JsValue> {
    let path = current_test_path(config)?;
    if uses_manual_invariant_tree(&path) {
        return call_original(config, registry, plugin, value, outer_stack);
    }
    let ctx = field(registry, "ctx")?;
    let root = field(&ctx, "root")?;
    let mut host = config.hosts.get(root.unchecked_ref::<Object>());
    if host.is_undefined() {
        host = start_host(config, &root)?;
    }
    let callback = method(registry, "resolve", &Array::of1(plugin))?;
    let existing = if callback.is_undefined() {
        JsValue::UNDEFINED
    } else {
        field(&host, "byCallback")?
            .unchecked_into::<Map>()
            .get(&callback)
    };
    if !existing.is_undefined() {
        return if has_barrier_owner(&host, &ctx)? {
            Ok(existing)
        } else {
            join_startup(
                &existing,
                &field(&host, "ready")?,
                false,
                &config.validation_error,
            )
        };
    }
    if has_barrier_owner(&host, &ctx)? || callback.is_undefined() {
        return call_original(config, registry, plugin, value, outer_stack);
    }
    let wrapped = with_readiness(plugin, &callback)?;
    let fiber = call_original(config, registry, &wrapped, value, outer_stack)?;
    let raw = field(&field(&fiber, "ctx")?, "fiber")?;
    let initially_pending = field(&raw, "state")?.as_f64() == Some(0.0);
    field(&host, "barrierOwners")?
        .unchecked_into::<WeakSet>()
        .add(raw.unchecked_ref::<Object>());
    join_startup(
        &fiber,
        &field(&host, "ready")?,
        initially_pending,
        &config.validation_error,
    )
}

fn current_test_path(config: &Configuration) -> Result<String, JsValue> {
    Ok(config
        .test_path
        .call0(&JsValue::UNDEFINED)?
        .as_string()
        .unwrap_or_default())
}

fn call_original(
    config: &Configuration,
    registry: &JsValue,
    plugin: &JsValue,
    value: &JsValue,
    outer_stack: &JsValue,
) -> Result<JsValue, JsValue> {
    Reflect::apply(
        &config.original_plugin,
        registry,
        &Array::of3(plugin, value, outer_stack),
    )
}

fn mount(
    config: &Configuration,
    root: &JsValue,
    host: &JsValue,
    plugin: &JsValue,
    value: &JsValue,
) -> Result<JsValue, JsValue> {
    let registry = field(root, "registry")?;
    let fiber = call_original(config, &registry, plugin, value, &JsValue::UNDEFINED)?;
    let callback = method(&registry, "resolve", &Array::of1(plugin))?;
    if callback.is_undefined() {
        return Err(
            js_sys::Error::new("test invariants: companion is not a valid Cordis plugin").into(),
        );
    }
    field(host, "byCallback")?
        .unchecked_into::<Map>()
        .set(&callback, &fiber);
    let raw = field(&field(&fiber, "ctx")?, "fiber")?;
    field(host, "barrierOwners")?
        .unchecked_into::<WeakSet>()
        .add(raw.unchecked_ref::<Object>());
    Ok(fiber)
}

fn start_host(config: &Rc<Configuration>, root: &JsValue) -> Result<JsValue, JsValue> {
    let host: JsValue = object(&[
        ("byCallback", Map::new().into()),
        ("barrierOwners", WeakSet::new().into()),
    ])?
    .into();
    let service = mount(
        config,
        root,
        &host,
        &config.invariant_registry,
        &object(&[("enabled", JsValue::TRUE)])?.into(),
    )?;
    let paths = companion_paths(&current_test_path(config)?, &config.companions)?
        .iter()
        .filter_map(|path| path.as_string())
        .collect::<Vec<_>>();
    let service_ready = require_active(&service, "invariant service");
    let ready_config = config.clone();
    let ready_root = root.clone();
    let ready_host = host.clone();
    let begin = Closure::once_into_js(move |_value: JsValue| -> Result<JsValue, JsValue> {
        let attachment = if paths.iter().any(|path| path == ATTACHMENT_COMPANION) {
            Some(mount(
                &ready_config,
                &ready_root,
                &ready_host,
                &ready_config.attachment_store,
                &JsValue::UNDEFINED,
            )?)
        } else {
            None
        };
        let loads = Array::new();
        for path in &paths {
            loads.push(&load_companion(&ready_config, path));
        }
        let after_config = ready_config.clone();
        let after_root = ready_root.clone();
        let after_host = ready_host.clone();
        let loaded = Closure::once_into_js(move |modules: JsValue| -> Result<JsValue, JsValue> {
            let modules = modules.unchecked_into::<Array>();
            let mut fibers = Vec::new();
            for (index, path) in paths.iter().enumerate() {
                let module = modules.get(
                    u32::try_from(index)
                        .map_err(|_| js_sys::RangeError::new("too many invariant companions"))?,
                );
                fibers.push((
                    mount(
                        &after_config,
                        &after_root,
                        &after_host,
                        &module,
                        &JsValue::UNDEFINED,
                    )?,
                    path.clone(),
                ));
            }
            let startup = Array::new();
            if let Some(attachment) = attachment {
                startup.push(&require_active(&attachment, "test attachment store"));
            }
            for (fiber, path) in fibers {
                startup.push(&require_active(&fiber, &path));
            }
            let provide =
                Closure::once_into_js(move |_value: JsValue| -> Result<JsValue, JsValue> {
                    method(
                        &after_root,
                        "provide",
                        &Array::of2(&TEST_INVARIANT_READY_SERVICE.into(), &JsValue::TRUE),
                    )?;
                    Ok(JsValue::UNDEFINED)
                });
            then(&Promise::all(&startup), &provide)
        });
        then(&Promise::all(&loads), &loaded)
    });
    let ready = then(&service_ready, &begin)?;
    set(&host, &"ready".into(), &ready)?;
    config.hosts.set(root.unchecked_ref::<Object>(), &host);
    Ok(host)
}

fn load_companion(config: &Configuration, path: &str) -> Promise {
    let load = (|| {
        let load = Reflect::get(&config.companions, &path.into())?;
        if load.is_undefined() {
            return Err(js_sys::Error::new(&format!(
                "test invariants: selected companion vanished at {path}"
            ))
            .into());
        }
        load.dyn_into::<Function>()
            .map_err(|_| js_sys::TypeError::new("load is not a function"))?
            .call0(&JsValue::UNDEFINED)
    })();
    let promise = match load {
        Ok(module) => Promise::resolve(&module),
        Err(error) => return Promise::reject(&error),
    };
    let path = path.to_owned();
    let validate = Closure::once_into_js(move |module: JsValue| -> Result<JsValue, JsValue> {
        let inject = field(&module, "inject")?;
        if !method(&inject, "includes", &Array::of1(&"invariants".into()))?.is_truthy() {
            return Err(js_sys::Error::new(&format!(
                "test invariants: {path} must inject the invariant service"
            ))
            .into());
        }
        Ok(module)
    });
    then(&promise, &validate).map_or_else(|error| Promise::reject(&error), JsValue::unchecked_into)
}

fn require_active(fiber: &JsValue, label: &str) -> Promise {
    let awaited = match method(fiber, "await", &Array::new()) {
        Ok(value) => Promise::resolve(&value),
        Err(error) => return Promise::reject(&error),
    };
    let fiber = fiber.clone();
    let label = label.to_owned();
    let inspect = Closure::once_into_js(move |_value: JsValue| -> Result<JsValue, JsValue> {
        if field(&fiber, "state")?.as_f64() != Some(2.0) {
            return Err(js_sys::Error::new(&format!(
                "test invariants: {label} settled without becoming active"
            ))
            .into());
        }
        Ok(JsValue::UNDEFINED)
    });
    then(&awaited, &inspect).map_or_else(|error| Promise::reject(&error), JsValue::unchecked_into)
}

fn has_barrier_owner(host: &JsValue, ctx: &JsValue) -> Result<bool, JsValue> {
    let owners = field(host, "barrierOwners")?.unchecked_into::<WeakSet>();
    let mut fiber = field(ctx, "fiber")?;
    loop {
        if owners.has(fiber.unchecked_ref::<Object>()) {
            let state = field(&fiber, "state")?.as_f64();
            if matches!(state, Some(1.0 | 2.0)) {
                return Ok(true);
            }
        }
        let parent = field(&field(&fiber, "parent")?, "fiber")?;
        if parent == fiber {
            return Ok(false);
        }
        fiber = parent;
    }
}

fn with_readiness(plugin: &JsValue, callback: &JsValue) -> Result<JsValue, JsValue> {
    let inject = resolve_inject(&field(plugin, "inject")?, JsValue::UNDEFINED)?;
    let merged: JsValue = Object::new().into();
    spread_into(&merged, &inject)?;
    define_data(
        &merged,
        &TEST_INVARIANT_READY_SERVICE.into(),
        &JsValue::NULL,
    )?;
    let wrapped: JsValue = object(&[("apply", callback.clone()), ("inject", merged)])?.into();
    for key in ["name", "Config", "provide", "intercept"] {
        if !field(plugin, key)?.is_undefined() {
            define_data(&wrapped, &key.into(), &field(plugin, key)?)?;
        }
    }
    Ok(wrapped)
}

fn join_startup(
    fiber: &JsValue,
    ready: &JsValue,
    dispose_validation: bool,
    validation_error: &JsValue,
) -> Result<JsValue, JsValue> {
    let raw = field(&field(fiber, "ctx")?, "fiber")?;
    let validation_error = validation_error.clone();
    let start = Closure::once_into_js(move |_value: JsValue| -> Result<JsValue, JsValue> {
        let promise = match method(&raw, "await", &Array::new()) {
            Ok(value) => Promise::resolve(&value),
            Err(error) => Promise::reject(&error),
        };
        let reject = Closure::once_into_js(move |error: JsValue| -> Result<JsValue, JsValue> {
            if dispose_validation && instance_of(&error, &validation_error)? {
                let disposal = method(&raw, "dispose", &Array::new())?;
                let rethrow =
                    Closure::once_into_js(move |_value: JsValue| -> Result<JsValue, JsValue> {
                        Err(error)
                    });
                return then(&Promise::resolve(&disposal), &rethrow);
            }
            Err(error)
        });
        method(&promise, "then", &Array::of2(&JsValue::UNDEFINED, &reject))
    });
    let readiness = then(ready, &start)?;
    let joined: JsValue = Object::create(fiber.unchecked_ref::<Object>()).into();
    let bound = method(&field(&readiness, "then")?, "bind", &Array::of1(&readiness))?;
    set(&joined, &"then".into(), &bound)?;
    Ok(joined)
}

fn instance_of(value: &JsValue, constructor: &JsValue) -> Result<bool, JsValue> {
    thread_local! { static INSTANCE_OF: Function = Function::new_with_args("value,constructor", "return value instanceof constructor;"); }
    INSTANCE_OF
        .with(|check| check.call2(&JsValue::UNDEFINED, value, constructor))
        .map(|value| value.is_truthy())
}

fn field(value: &JsValue, name: &str) -> Result<JsValue, JsValue> {
    Reflect::get(value, &name.into())
}
fn then(promise: &JsValue, callback: &JsValue) -> Result<JsValue, JsValue> {
    method(promise, "then", &Array::of1(callback))
}

/// Creates the source test-only attachment service with rejecting image methods.
///
/// # Errors
/// Preserves base-class construction and property-definition failures.
#[wasm_bindgen(js_name = createTestInvariantAttachmentStore)]
pub fn create_attachment_store(base: &Function) -> Result<Function, JsValue> {
    let initialize = Closure::wrap(Box::new(move |instance: JsValue| -> Result<(), JsValue> {
        let limits = object(&[
            ("maxImageBytes", 1.into()),
            ("maxImagesPerMessage", 1.into()),
            ("maxMessageImageBytes", 1.into()),
            ("maxImagePixels", 1.into()),
            ("mediaTypes", Array::of1(&"image/png".into()).into()),
        ])?;
        define_data(&instance, &"imageLimits".into(), &limits)
    }) as Box<dyn Fn(JsValue) -> Result<(), JsValue>>)
    .into_js_value();
    let constructor = Function::new_with_args("Base,initialize", "return class TestAttachmentStore extends Base { constructor(...args) { super(...args); initialize(this); } };").call2(&JsValue::UNDEFINED, base, &initialize)?.unchecked_into::<Function>();
    let prototype = field(&constructor, "prototype")?;
    for (method_name, verb) in [
        ("validateImage", "validate"),
        ("saveImage", "save"),
        ("readImage", "read"),
    ] {
        let reject = Closure::wrap(Box::new(move |_input: JsValue| -> Promise {
            Promise::reject(&js_sys::Error::new(&format!(
                "test invariant attachment store does not {verb} images"
            )))
        }) as Box<dyn Fn(JsValue) -> Promise>)
        .into_js_value();
        let method = Function::new_with_args(
            "reject",
            &format!(
                "return ({{ {method_name}(_input) {{ return reject(_input); }} }}).{method_name};"
            ),
        )
        .call1(&JsValue::UNDEFINED, &reject)?;
        Reflect::define_property(
            prototype.unchecked_ref::<Object>(),
            &method_name.into(),
            &object(&[
                ("value", method),
                ("writable", JsValue::TRUE),
                ("configurable", JsValue::TRUE),
            ])?,
        )?;
    }
    Ok(constructor)
}
