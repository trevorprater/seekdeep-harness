//! Browser schema/package reflection with fiber-owned local invocations.

mod validation;

use std::{cell::RefCell, collections::HashSet, rc::Rc};

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

type Rows = Vec<(String, JsValue)>;

#[derive(Default)]
struct State {
    schemas: Rows,
    packages: Rows,
    descriptors: Rows,
    ids: HashSet<String>,
    history: HashSet<String>,
    listeners: Vec<Function>,
}

/// Rust-owned live values behind the browser Typert reflection facade.
#[wasm_bindgen]
pub struct WasmTypertSchemaRegistry {
    state: Rc<RefCell<State>>,
    context: JsValue,
}

#[wasm_bindgen]
impl WasmTypertSchemaRegistry {
    /// Atomically publishes a package face, its schema records, and invocations.
    ///
    /// # Errors
    /// Rejects duplicate or malformed records and propagates effect setup failures.
    pub fn register(&self, context: &JsValue, contribution: &JsValue) -> Result<JsValue, JsValue> {
        let package = string(contribution, "package")?;
        validation::segment("package name", &package)?;
        let face = get(contribution, "face")?;
        if face != "host" && face != "client" {
            return Err(error(&format!(
                "typert: invalid face {} — expected \"host\" or \"client\"",
                json(&face)?
            )));
        }
        let package_key = format!("{package}#{}", face.as_string().unwrap_or_default());
        if lookup(&self.state.borrow().packages, &package_key).is_some() {
            return Err(error(&format!(
                "typert: package face \"{package_key}\" is already registered"
            )));
        }
        let package_record = record(&[
            ("package", package.clone().into()),
            ("face", face.clone()),
            ("key", package_key.clone().into()),
            ("model", get(contribution, "model")?),
        ])?;
        let mut schemas = Vec::new();
        for schema in values(&get(contribution, "schemas")?)? {
            let name = string(&schema, "name")?;
            validation::segment("schema name", &name)?;
            let key = format!("{package}#{name}");
            if lookup(&schemas, &key).is_some()
                || lookup(&self.state.borrow().schemas, &key).is_some()
            {
                return Err(error(&format!(
                    "typert: schema \"{key}\" is already registered"
                )));
            }
            let value = Object::assign(&Object::new(), &Object::from(schema));
            for (field, value_field) in [
                ("package", package.clone().into()),
                ("face", face.clone()),
                ("key", key.clone().into()),
            ] {
                Reflect::set(&value, &field.into(), &value_field)?;
            }
            schemas.push((key, value.into()));
        }
        let invocations = values(&get(contribution, "invocations")?)?;
        self.validate_descriptors(&invocations)?;
        self.publish(context, package_key, package_record, schemas, invocations)
    }

    /// Returns the live schema record without copying its schema object.
    pub fn get(&self, key: &str) -> JsValue {
        lookup(&self.state.borrow().schemas, key).unwrap_or(JsValue::UNDEFINED)
    }

    /// Requires a schema and distinguishes malformed, absent-package, and absent-schema keys.
    ///
    /// # Errors
    /// Returns the source diagnostic for an unresolved key.
    pub fn resolve(&self, key: &str) -> Result<JsValue, JsValue> {
        if let Some(value) = lookup(&self.state.borrow().schemas, key) {
            return Ok(value);
        }
        let Some((package, name)) = key
            .split_once('#')
            .filter(|(package, name)| !package.is_empty() && !name.is_empty())
        else {
            return Err(error(&format!(
                "typert: invalid schema key \"{key}\" — expected \"<package>#<name>\""
            )));
        };
        let packages = self.state.borrow().packages.clone();
        for (_, record) in packages {
            if get(&record, "package")? == package {
                return Err(error(&format!(
                    "typert: cannot resolve \"{key}\" — package \"{package}\" is registered but contributes no schema named \"{name}\""
                )));
            }
        }
        Err(error(&format!(
            "typert: cannot resolve \"{key}\" — package \"{package}\" has no registered contribution"
        )))
    }

    /// Enumerates live schema records in registration order.
    ///
    /// # Errors
    /// Propagates invalid filter-property access.
    pub fn list(&self, filter: &JsValue) -> Result<Array, JsValue> {
        let rows = self.state.borrow().schemas.clone();
        filtered(&rows, filter)
    }

    /// Returns one package face; an omitted face selects Host reflection.
    ///
    /// # Errors
    /// Propagates the face value's JavaScript string-coercion failure.
    #[wasm_bindgen(js_name = getPackage)]
    pub fn get_package(&self, package: &str, face: &JsValue) -> Result<JsValue, JsValue> {
        let face = if face.is_undefined() {
            "host".to_owned()
        } else {
            Function::new_with_args("value", "return `${value}`;")
                .call1(&JsValue::UNDEFINED, face)?
                .as_string()
                .unwrap_or_default()
        };
        Ok(
            lookup(&self.state.borrow().packages, &format!("{package}#{face}"))
                .unwrap_or(JsValue::UNDEFINED),
        )
    }

    /// Enumerates live package records in registration order.
    ///
    /// # Errors
    /// Propagates invalid filter-property access.
    #[wasm_bindgen(js_name = listPackages)]
    pub fn list_packages(&self, filter: &JsValue) -> Result<Array, JsValue> {
        let rows = self.state.borrow().packages.clone();
        filtered(&rows, filter)
    }

    /// Projects a live Zod schema on each call without caching its result.
    ///
    /// # Errors
    /// Propagates resolution and schema-projection failures.
    #[wasm_bindgen(js_name = toJSONSchema)]
    pub fn to_json_schema(&self, key: &str, params: &JsValue) -> Result<JsValue, JsValue> {
        let schema = get(&self.resolve(key)?, "schema")?;
        call(&schema, "toJSONSchema", std::slice::from_ref(params))
    }

    /// Returns the exact registered local invocation descriptor.
    #[wasm_bindgen(js_name = localGet)]
    pub fn local_get(&self, endpoint: &str) -> JsValue {
        lookup(&self.state.borrow().descriptors, endpoint).unwrap_or(JsValue::UNDEFINED)
    }

    /// Retains endpoint history after its contribution is withdrawn.
    #[wasm_bindgen(js_name = localHasSeen)]
    pub fn local_has_seen(&self, endpoint: &str) -> bool {
        self.state.borrow().history.contains(endpoint)
    }

    /// Enumerates live local descriptors in registration order.
    #[wasm_bindgen(js_name = localList)]
    pub fn local_list(&self) -> Array {
        self.state
            .borrow()
            .descriptors
            .iter()
            .map(|(_, value)| value.clone())
            .collect()
    }

    /// Owns one deduplicated observer registration in the calling fiber.
    ///
    /// # Errors
    /// Rejects a non-callable observer or failed effect registration.
    #[wasm_bindgen(js_name = localSubscribe)]
    pub fn local_subscribe(
        &self,
        context: &JsValue,
        listener: &JsValue,
    ) -> Result<JsValue, JsValue> {
        let listener = listener.clone().dyn_into::<Function>()?;
        let state = self.state.clone();
        let setup = Closure::wrap(Box::new(move || {
            if !state
                .borrow()
                .listeners
                .iter()
                .any(|value| Object::is(value.as_ref(), listener.as_ref()))
            {
                state.borrow_mut().listeners.push(listener.clone());
            }
            let state = state.clone();
            let listener = listener.clone();
            Closure::wrap(Box::new(move || {
                state
                    .borrow_mut()
                    .listeners
                    .retain(|value| !Object::is(value.as_ref(), listener.as_ref()));
            }) as Box<dyn FnMut()>)
            .into_js_value()
        }) as Box<dyn FnMut() -> JsValue>);
        call(
            context,
            "effect",
            &[setup.into_js_value(), "typert registry subscription".into()],
        )
    }
}

impl WasmTypertSchemaRegistry {
    fn validate_descriptors(&self, descriptors: &[JsValue]) -> Result<(), JsValue> {
        let mut endpoints = HashSet::new();
        let mut ids = HashSet::new();
        for descriptor in descriptors {
            validation::invocation(descriptor)?;
            let endpoint = endpoint(descriptor)?;
            let id = string(descriptor, "id")?;
            if !endpoints.insert(endpoint.clone())
                || lookup(&self.state.borrow().descriptors, &endpoint).is_some()
            {
                return Err(error(&format!(
                    "typert: local endpoint \"{endpoint}\" is already registered"
                )));
            }
            if !ids.insert(id.clone()) || self.state.borrow().ids.contains(&id) {
                return Err(error(&format!(
                    "typert: local invocation id \"{id}\" is already registered"
                )));
            }
        }
        Ok(())
    }

    fn publish(
        &self,
        context: &JsValue,
        key: String,
        package: JsValue,
        schemas: Rows,
        descriptors: Vec<JsValue>,
    ) -> Result<JsValue, JsValue> {
        let state = self.state.clone();
        let report_context = self.context.clone();
        let setup = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
            let descriptors: Vec<_> = descriptors
                .iter()
                .map(|value| Ok((endpoint(value)?, string(value, "id")?, value.clone())))
                .collect::<Result<_, JsValue>>()?;
            {
                let mut state = state.borrow_mut();
                state.packages.push((key.clone(), package.clone()));
                state.schemas.extend(schemas.clone());
                for (endpoint, id, value) in &descriptors {
                    state.descriptors.push((endpoint.clone(), value.clone()));
                    state.ids.insert(id.clone());
                    state.history.insert(endpoint.clone());
                }
            }
            emit(
                &state,
                &report_context,
                descriptors.iter().map(|(key, _, _)| key.as_str()),
            )?;
            let (state, context, key, package, schemas) = (
                state.clone(),
                report_context.clone(),
                key.clone(),
                package.clone(),
                schemas.clone(),
            );
            let dispose = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
                let mut removed = Vec::new();
                {
                    let mut state = state.borrow_mut();
                    state.packages.retain(|(candidate, value)| {
                        candidate != &key || !Object::is(value, &package)
                    });
                    for (key, value) in &schemas {
                        state.schemas.retain(|(candidate, record)| {
                            candidate != key || !Object::is(record, value)
                        });
                    }
                    for (key, id, value) in &descriptors {
                        if lookup(&state.descriptors, key)
                            .is_some_and(|record| Object::is(&record, value))
                        {
                            state.descriptors.retain(|(candidate, _)| candidate != key);
                            state.ids.remove(id);
                            removed.push(key.clone());
                        }
                    }
                }
                emit(&state, &context, removed.iter().map(String::as_str))
            }) as Box<dyn FnMut() -> Result<(), JsValue>>);
            Ok(dispose.into_js_value())
        }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
        call(
            context,
            "effect",
            &[setup.into_js_value(), "typert.register()".into()],
        )
    }
}

pub(crate) fn install(service: &Object, context: &JsValue) -> Result<(), JsValue> {
    let core = WasmTypertSchemaRegistry {
        state: Rc::new(RefCell::new(State::default())),
        context: context.clone(),
    };
    let factory = Function::new_with_args(
        "service,core,ctx",
        r"
service.ctx = ctx;
Object.defineProperty(service, Symbol.for('cordis.service.tracker'), { value: true });
service.register = function (value) { return core.register(this.ctx, value); };
service.get = key => core.get(key);
service.resolve = key => core.resolve(key);
service.list = filter => core.list(filter);
service.getPackage = (name, face) => core.getPackage(name, face);
service.listPackages = filter => core.listPackages(filter);
service.toJSONSchema = (key, params) => core.toJSONSchema(key, params);
Object.defineProperty(service, 'local', { configurable: true, get() {
  const ctx = this.ctx;
  return { get: key => core.localGet(key), hasSeen: key => core.localHasSeen(key),
    list: () => core.localList(), subscribe: listener => core.localSubscribe(ctx, listener) };
}});
",
    );
    factory.call3(&JsValue::UNDEFINED, service, &core.into(), context)?;
    Ok(())
}

fn emit<'a>(
    state: &Rc<RefCell<State>>,
    context: &JsValue,
    keys: impl Iterator<Item = &'a str>,
) -> Result<(), JsValue> {
    for key in keys {
        let change = record(&[("kind", "local".into()), ("key", key.into())])?;
        let listeners = state.borrow().listeners.clone();
        for listener in listeners {
            if let Err(error_value) = listener.call1(&JsValue::UNDEFINED, &change) {
                let logger = get(context, "logger")?;
                if logger.is_undefined() {
                    web_sys::console::warn_1(&error_value);
                } else {
                    call(
                        &logger,
                        "warn",
                        &[format!("typert: local observer for \"{key}\" failed").into()],
                    )?;
                    call(&logger, "warn", &[error_value])?;
                }
            }
        }
    }
    Ok(())
}

fn filtered(rows: &Rows, filter: &JsValue) -> Result<Array, JsValue> {
    let filter = if filter.is_undefined() {
        Object::new().into()
    } else {
        filter.clone()
    };
    let result = Array::new();
    for (_, value) in rows {
        if (get(&filter, "package")?.is_undefined()
            || get(value, "package")? == get(&filter, "package")?)
            && (get(&filter, "face")?.is_undefined()
                || get(value, "face")? == get(&filter, "face")?)
        {
            result.push(value);
        }
    }
    Ok(result)
}

fn lookup(rows: &Rows, key: &str) -> Option<JsValue> {
    rows.iter()
        .find(|(candidate, _)| candidate == key)
        .map(|(_, value)| value.clone())
}
fn endpoint(value: &JsValue) -> Result<String, JsValue> {
    Ok(format!(
        "{}/{}",
        string(value, "namespace")?,
        string(value, "method")?
    ))
}
fn get(value: &JsValue, key: &str) -> Result<JsValue, JsValue> {
    Reflect::get(value, &key.into())
}
fn string(value: &JsValue, key: &str) -> Result<String, JsValue> {
    get(value, key)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{key} must be a string")).into())
}
fn error(message: &str) -> JsValue {
    js_sys::Error::new(message).into()
}
fn json(value: &JsValue) -> Result<String, JsValue> {
    Ok(js_sys::JSON::stringify(value)?
        .as_string()
        .unwrap_or_else(|| "undefined".into()))
}
fn values(value: &JsValue) -> Result<Vec<JsValue>, JsValue> {
    js_sys::try_iter(value)?
        .ok_or_else(|| js_sys::TypeError::new("value is not iterable"))?
        .collect()
}
fn record(entries: &[(&str, JsValue)]) -> Result<JsValue, JsValue> {
    let result = Object::new();
    for (key, value) in entries {
        Reflect::set(&result, &(*key).into(), value)?;
    }
    Ok(result.into())
}
fn call(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    get(value, name)?
        .dyn_into::<Function>()?
        .apply(value, &arguments.iter().cloned().collect::<Array>())
}
