//! Calling-fiber lookup and Context providers with retained wire declarations.

use std::{cell::RefCell, rc::Rc};

use js_sys::{Array, Function, Object, Promise};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use super::{call, error, get, json, record, string, validation};

#[derive(Clone, Copy)]
enum Table {
    Lookup,
    LookupResolver,
    Host,
    HostResolver,
    Client,
}

impl Table {
    fn parse(value: &str) -> Result<Self, JsValue> {
        match value {
            "lookup" => Ok(Self::Lookup),
            "lookup-resolver" => Ok(Self::LookupResolver),
            "host-context" => Ok(Self::Host),
            "host-resolver" => Ok(Self::HostResolver),
            "client-context" => Ok(Self::Client),
            _ => Err(error("unknown Typert provider table")),
        }
    }
    fn index(self) -> usize {
        match self {
            Self::Lookup => 0,
            Self::LookupResolver => 1,
            Self::Host => 2,
            Self::HostResolver => 3,
            Self::Client => 4,
        }
    }
    fn group(self) -> usize {
        match self {
            Self::Lookup | Self::LookupResolver => 0,
            Self::Host | Self::HostResolver | Self::Client => 1,
        }
    }
    fn kind(self) -> &'static str {
        match self {
            Self::Lookup | Self::LookupResolver => "lookup",
            Self::Host | Self::HostResolver => "host-context",
            Self::Client => "client-context",
        }
    }
}

struct Entry {
    key: String,
    owner: Rc<()>,
    value: JsValue,
}

#[derive(Default)]
struct State {
    tables: [Vec<Entry>; 5],
    definitions: Vec<(String, JsValue)>,
    listeners: [Vec<JsValue>; 2],
}

/// Rust-owned dependency provider storage used by the browser registry facade.
#[wasm_bindgen]
pub struct WasmTypertProviders {
    state: Rc<RefCell<State>>,
    context: JsValue,
}

#[wasm_bindgen]
impl WasmTypertProviders {
    /// Registers a provider or resolver under the calling fiber.
    ///
    /// # Errors
    /// Rejects malformed declarations, duplicate ownership, or lifetime wire drift.
    pub fn register(
        &self,
        ctx: &JsValue,
        table: &str,
        key: &str,
        value: &JsValue,
    ) -> Result<JsValue, JsValue> {
        let table = Table::parse(table)?;
        validate(table, key, value)?;
        if find(&self.state.borrow(), table, key).is_some() {
            let message = match table {
                Table::Lookup => format!("typert: lookup \"{key}\" is already registered"),
                Table::LookupResolver => {
                    format!("typert: lookup \"{key}\" resolver is already configured")
                }
                Table::HostResolver => {
                    format!("typert: host-context \"{key}\" resolver is already configured")
                }
                Table::Host | Table::Client => format!(
                    "typert: {} provider \"{key}\" is already registered",
                    table.kind()
                ),
            };
            return Err(error(&message));
        }
        let definition = if matches!(table, Table::Lookup) {
            let definition = lookup_definition(key, value)?;
            let known = self
                .state
                .borrow()
                .definitions
                .iter()
                .find(|(candidate, _)| candidate == key)
                .map(|(_, value)| value.clone());
            if let Some(known) = known {
                for field in ["parameter", "wire", "hostTypeSymbol", "wireTypeSymbol"] {
                    if get(&known, field)? != get(&definition, field)? {
                        return Err(error(&format!(
                            "typert: lookup \"{key}\" changed its wire declaration during this registry lifetime"
                        )));
                    }
                }
            }
            Some(definition)
        } else {
            None
        };
        let label_key = json(&JsValue::from_str(key))?;
        let (state, context, key, value) = (
            self.state.clone(),
            self.context.clone(),
            key.to_owned(),
            value.clone(),
        );
        let owner = Rc::new(());
        let setup = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
            {
                let mut state = state.borrow_mut();
                if let Some(definition) = &definition {
                    if let Some((_, existing)) = state
                        .definitions
                        .iter_mut()
                        .find(|(candidate, _)| candidate == &key)
                    {
                        *existing = definition.clone();
                    } else {
                        state.definitions.push((key.clone(), definition.clone()));
                    }
                }
                state.tables[table.index()].push(Entry {
                    key: key.clone(),
                    owner: owner.clone(),
                    value: value.clone(),
                });
            }
            emit(&state, &context, table, &key)?;
            let (state, context, key, owner) =
                (state.clone(), context.clone(), key.clone(), owner.clone());
            Ok(Closure::wrap(Box::new(move || -> Result<(), JsValue> {
                let removed = {
                    let mut state = state.borrow_mut();
                    let rows = &mut state.tables[table.index()];
                    let before = rows.len();
                    rows.retain(|entry| entry.key != key || !Rc::ptr_eq(&entry.owner, &owner));
                    before != rows.len()
                };
                if removed {
                    emit(&state, &context, table, &key)?;
                }
                Ok(())
            }) as Box<dyn FnMut() -> Result<(), JsValue>>)
            .into_js_value())
        }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
        let operation = match table {
            Table::Lookup => "lookups.register",
            Table::LookupResolver => "lookups.configure",
            Table::HostResolver => "contexts.configureHost",
            Table::Host | Table::Client => "contexts.register",
        };
        call(
            ctx,
            "effect",
            &[
                setup.into_js_value(),
                format!("typert.{operation}({label_key})").into(),
            ],
        )
    }

    /// Gets a provider, applying the current resolver without mutating its declaration.
    ///
    /// # Errors
    /// Propagates provider property access or malformed table selection.
    pub fn get(&self, table: &str, key: &str) -> Result<JsValue, JsValue> {
        let table = Table::parse(table)?;
        let provider = find(&self.state.borrow(), table, key);
        let Some(provider) = provider.filter(|value| !value.is_undefined()) else {
            return Ok(JsValue::UNDEFINED);
        };
        let resolver_table = match table {
            Table::Lookup => Table::LookupResolver,
            Table::Host => Table::HostResolver,
            _ => return Ok(provider),
        };
        let Some(resolver) = find(&self.state.borrow(), resolver_table, key) else {
            return Ok(provider);
        };
        let fields: &[&str] = match table {
            Table::Lookup => &["parameter", "wire", "hostTypeSymbol", "wireTypeSymbol"],
            _ => &["wire", "wireTypeSymbol"],
        };
        let projected = Object::new();
        for field in fields {
            js_sys::Reflect::set(&projected, &(*field).into(), &get(&provider, field)?)?;
        }
        let resolve = Closure::wrap(Box::new(move |id: JsValue| -> Promise {
            let result = resolver.dyn_ref::<Function>().map_or_else(
                || Err(js_sys::TypeError::new("resolver is not a function").into()),
                |resolver| resolver.call1(&JsValue::UNDEFINED, &id),
            );
            match result {
                Ok(value) => Promise::resolve(&value),
                Err(error) => Promise::reject(&error),
            }
        }) as Box<dyn Fn(JsValue) -> Promise>);
        js_sys::Reflect::set(&projected, &"resolve".into(), &resolve.into_js_value())?;
        Ok(projected.into())
    }

    /// Lists active lookup keys in registration order.
    pub fn keys(&self) -> Array {
        self.state.borrow().tables[Table::Lookup.index()]
            .iter()
            .map(|entry| JsValue::from_str(&entry.key))
            .collect()
    }

    /// Retains wire declarations after provider withdrawal.
    pub fn definitions(&self) -> Array {
        self.state
            .borrow()
            .definitions
            .iter()
            .map(|(_, value)| value.clone())
            .collect()
    }

    /// Owns an observer in the lookup or shared Context change stream.
    ///
    /// # Errors
    /// Propagates effect registration failures.
    pub fn subscribe(
        &self,
        ctx: &JsValue,
        contexts: bool,
        listener: &JsValue,
    ) -> Result<JsValue, JsValue> {
        let group = usize::from(contexts);
        let (state, listener) = (self.state.clone(), listener.clone());
        let setup = Closure::wrap(Box::new(move || {
            if !state.borrow().listeners[group]
                .iter()
                .any(|value| Object::is(value, &listener))
            {
                state.borrow_mut().listeners[group].push(listener.clone());
            }
            let (state, listener) = (state.clone(), listener.clone());
            Closure::wrap(Box::new(move || {
                state.borrow_mut().listeners[group].retain(|value| !Object::is(value, &listener));
            }) as Box<dyn FnMut()>)
            .into_js_value()
        }) as Box<dyn FnMut() -> JsValue>);
        call(
            ctx,
            "effect",
            &[setup.into_js_value(), "typert registry subscription".into()],
        )
    }
}

fn validate(table: Table, key: &str, value: &JsValue) -> Result<(), JsValue> {
    match table {
        Table::Lookup => {
            validation::segment("lookup key", key)?;
            validation::segment("lookup parameter", &string(value, "parameter")?)?;
            validation::wire("lookup wire field", &string(value, "wire")?)?;
            validation::nonempty("lookup Host type symbol", &string(value, "hostTypeSymbol")?)?;
            validation::nonempty("lookup wire type symbol", &string(value, "wireTypeSymbol")?)?;
        }
        Table::LookupResolver => validation::segment("lookup key", key)?,
        Table::Host => {
            validation::segment("Context key", key)?;
            validation::wire("Context wire field", &string(value, "wire")?)?;
            validation::nonempty(
                "Context wire type symbol",
                &string(value, "wireTypeSymbol")?,
            )?;
        }
        Table::HostResolver | Table::Client => validation::segment("Context key", key)?,
    }
    Ok(())
}

fn lookup_definition(key: &str, value: &JsValue) -> Result<JsValue, JsValue> {
    record(&[
        ("key", key.into()),
        ("parameter", get(value, "parameter")?),
        ("wire", get(value, "wire")?),
        ("hostTypeSymbol", get(value, "hostTypeSymbol")?),
        ("wireTypeSymbol", get(value, "wireTypeSymbol")?),
    ])
}

fn find(state: &State, table: Table, key: &str) -> Option<JsValue> {
    state.tables[table.index()]
        .iter()
        .find(|entry| entry.key == key)
        .map(|entry| entry.value.clone())
}

fn emit(
    state: &Rc<RefCell<State>>,
    context: &JsValue,
    table: Table,
    key: &str,
) -> Result<(), JsValue> {
    let listeners = state.borrow().listeners[table.group()].clone();
    let change = record(&[("kind", table.kind().into()), ("key", key.into())])?;
    for listener in listeners {
        let result = listener.dyn_ref::<Function>().map_or_else(
            || Err(js_sys::TypeError::new("listener is not a function").into()),
            |listener| listener.call1(&JsValue::UNDEFINED, &change),
        );
        if let Err(failure) = result {
            let logger = get(context, "logger")?;
            if logger.is_undefined() {
                web_sys::console::warn_1(&failure);
            } else {
                call(
                    &logger,
                    "warn",
                    &[format!("typert: {} observer for \"{key}\" failed", table.kind()).into()],
                )?;
                call(&logger, "warn", &[failure])?;
            }
        }
    }
    Ok(())
}

pub(super) fn install(service: &Object, ctx: &JsValue) -> Result<(), JsValue> {
    let core = WasmTypertProviders {
        state: Rc::new(RefCell::new(State::default())),
        context: ctx.clone(),
    };
    Function::new_with_args("service,core", r"
Object.defineProperty(service, 'lookups', { configurable: true, get() {
  const ctx = this.ctx;
  return { register: (key, value) => core.register(ctx, 'lookup', key, value),
    configure: (key, value) => core.register(ctx, 'lookup-resolver', key, value),
    get: key => core.get('lookup', key), keys: () => core.keys(), definitions: () => core.definitions(),
    subscribe: listener => core.subscribe(ctx, false, listener) };
}});
Object.defineProperty(service, 'contexts', { configurable: true, get() {
  const ctx = this.ctx;
  return { registerHost: (key, value) => core.register(ctx, 'host-context', key, value),
    configureHost: (key, value) => core.register(ctx, 'host-resolver', key, value),
    registerClient: (key, value) => core.register(ctx, 'client-context', key, value),
    getHost: key => core.get('host-context', key), getClient: key => core.get('client-context', key),
    subscribe: listener => core.subscribe(ctx, true, listener) };
}});
").call2(&JsValue::UNDEFINED, service, &core.into())?;
    Ok(())
}
