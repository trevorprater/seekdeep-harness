//! V8 module namespaces for native worker compatibility imports.

use std::cell::RefCell;

thread_local! {
    static WORKER_THREADS: RefCell<Option<v8::Global<v8::Value>>> = const { RefCell::new(None) };
}

const WORKER_MODULE: &str = "const api = __seekdeep_worker_threads_module__; export const parentPort = api.parentPort; export default api;";

/// Keeps the per-run namespace alive until worker evaluation has stopped.
pub(crate) struct WorkerModules;

impl WorkerModules {
    pub(crate) fn install(scope: &mut v8::PinScope) -> Result<Self, String> {
        let namespace = worker_namespace(scope)?;
        WORKER_THREADS.with(|slot| {
            assert!(slot.borrow().is_none(), "one module registry per worker");
            *slot.borrow_mut() = Some(namespace);
        });
        scope.set_host_import_module_dynamically_callback(import_module);
        Ok(Self)
    }
}

impl Drop for WorkerModules {
    fn drop(&mut self) {
        WORKER_THREADS.with(|slot| slot.borrow_mut().take());
    }
}

fn worker_namespace(scope: &mut v8::PinScope) -> Result<v8::Global<v8::Value>, String> {
    v8::tc_scope!(let caught, scope);
    let name =
        v8::String::new(caught, "node:worker_threads").ok_or("cannot allocate module name")?;
    let code = v8::String::new(caught, WORKER_MODULE).ok_or("cannot allocate worker module")?;
    let origin = v8::ScriptOrigin::new(
        caught,
        name.into(),
        0,
        0,
        false,
        0,
        None,
        false,
        false,
        true,
        None,
    );
    let mut source = v8::script_compiler::Source::new(code, Some(&origin));
    let module = v8::script_compiler::compile_module(caught, &mut source)
        .ok_or("cannot compile worker module")?;
    if module.instantiate_module(caught, |_, _, _, _| None) != Some(true) {
        return Err("cannot instantiate worker module".to_owned());
    }
    let evaluated = module
        .evaluate(caught)
        .ok_or("cannot evaluate worker module")?;
    let promise = v8::Local::<v8::Promise>::try_from(evaluated)
        .map_err(|_| "worker module evaluation did not return a promise")?;
    if promise.state() != v8::PromiseState::Fulfilled {
        return Err("worker module did not settle synchronously".to_owned());
    }
    let namespace = module.get_module_namespace();
    Ok(v8::Global::new(caught, namespace))
}

fn import_module<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    _options: v8::Local<'s, v8::Data>,
    _resource_name: v8::Local<'s, v8::Value>,
    specifier: v8::Local<'s, v8::String>,
    attributes: v8::Local<'s, v8::FixedArray>,
) -> Option<v8::Local<'s, v8::Promise>> {
    let resolver = v8::PromiseResolver::new(scope)?;
    let promise = resolver.get_promise(scope);
    let specifier = specifier.to_rust_string_lossy(scope);
    if matches!(specifier.as_str(), "node:worker_threads" | "worker_threads") {
        if let Some((code, message)) = attribute_error(scope, attributes) {
            let message = v8::String::new(scope, &message)?;
            let error = v8::Exception::type_error(scope, message);
            let object = v8::Local::<v8::Object>::try_from(error).ok()?;
            let key = v8::String::new(scope, "code")?;
            let code = v8::String::new(scope, code)?;
            object.set(scope, key.into(), code.into())?;
            resolver.reject(scope, error)?;
            return Some(promise);
        }
        let namespace = WORKER_THREADS.with(|slot| slot.borrow().clone())?;
        let namespace = v8::Local::new(scope, namespace);
        resolver.resolve(scope, namespace)?;
    } else {
        let quoted = serde_json::to_string(&specifier).ok()?;
        let message = v8::String::new(
            scope,
            &format!("Native Code Mode does not implement module {quoted}"),
        )?;
        let error = v8::Exception::error(scope, message);
        resolver.reject(scope, error)?;
    }
    Some(promise)
}

fn attribute_error(
    scope: &mut v8::PinScope,
    attributes: v8::Local<v8::FixedArray>,
) -> Option<(&'static str, String)> {
    for index in (0..attributes.length()).step_by(2) {
        let key = v8::Local::<v8::String>::try_from(attributes.get(scope, index)?).ok()?;
        if key.to_rust_string_lossy(scope) != "type" {
            continue;
        }
        let value = v8::Local::<v8::String>::try_from(attributes.get(scope, index + 1)?).ok()?;
        let value = value.to_rust_string_lossy(scope);
        return match value.as_str() {
            // The source bootstrap has already populated Node's JavaScript module cache.
            "javascript" => None,
            "json" => Some((
                "ERR_IMPORT_ATTRIBUTE_TYPE_INCOMPATIBLE",
                "Module \"node:worker_threads\" is not of type \"json\"".to_owned(),
            )),
            _ => Some((
                "ERR_IMPORT_ATTRIBUTE_UNSUPPORTED",
                format!(
                    "Import attribute \"type\" with value \"{value}\" is not supported in node:worker_threads"
                ),
            )),
        };
    }
    None
}
