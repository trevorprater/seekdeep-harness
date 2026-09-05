//! JavaScript Proxy shell delegating Client Guard policy to Rust/WASM.

use std::{rc::Rc, sync::Arc};

use js_sys::{Array, Function, Reflect};
use seekdeep_cordis_dynamic_types::{
    CordisDynamicPackageId, CordisDynamicPluginId, CordisDynamicPluginRunId, DynamicCordisPackage,
};
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{
    ClientContextAccess, ClientContextGuard, ClientPriorityAllocator, normalize_slot_registration,
    normalize_theme_override,
};

/// Rust policy consumed by one dynamic Client Context Proxy.
#[wasm_bindgen]
pub struct WasmClientGuardPolicy {
    package: DynamicCordisPackage,
    context: ClientContextGuard,
    priorities: Arc<ClientPriorityAllocator>,
}

#[wasm_bindgen]
impl WasmClientGuardPolicy {
    /// Creates policy for one exact package and declared Service set.
    #[wasm_bindgen(constructor)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn new(
        plugin_id: String,
        package_id: String,
        plugin_run_id: String,
        name: String,
        declared: Array,
    ) -> Self {
        let declared = declared
            .iter()
            .filter_map(|value| value.as_string())
            .collect::<Vec<_>>();
        Self {
            package: DynamicCordisPackage {
                plugin_id: CordisDynamicPluginId::new(plugin_id),
                package_id: CordisDynamicPackageId::new(package_id),
                plugin_run_id: CordisDynamicPluginRunId::new(plugin_run_id),
                name,
            },
            context: ClientContextGuard::new(declared),
            priorities: Arc::new(ClientPriorityAllocator::default()),
        }
    }

    fn read(&self, property: &str, service_exists: bool) -> Result<String, JsValue> {
        self.context
            .read(property, service_exists)
            .map(|access| {
                match access {
                    ClientContextAccess::Get => "get",
                    ClientContextAccess::Verb => "verb",
                    ClientContextAccess::Service => "service",
                }
                .to_owned()
            })
            .map_err(|message| js_sys::Error::new(&message).into())
    }

    fn contains(&self, property: &str) -> bool {
        self.context.contains(property)
    }

    fn invoke_verb(&self, property: &str, timer_exists: bool) -> Result<(), JsValue> {
        self.context
            .invoke_verb(property, timer_exists)
            .map_err(|message| js_sys::Error::new(&message).into())
    }

    fn normalize_slot(
        &self,
        options_json: &str,
        slot_kind: Option<&str>,
    ) -> Result<String, JsValue> {
        let options = serde_json::from_str(options_json)
            .map_err(|error| js_sys::Error::new(&error.to_string()))?;
        let normalized =
            normalize_slot_registration(&self.package, &options, slot_kind, &self.priorities)
                .map_err(|message| js_sys::Error::new(&message))?;
        serde_json::to_string(&serde_json::json!({
            "slot": normalized.slot,
            "options": normalized.options,
            "priority": normalized.priority,
        }))
        .map_err(|error| js_sys::Error::new(&error.to_string()).into())
    }

    fn normalize_theme(
        &self,
        source_json: &str,
        tokens_json: Option<&str>,
    ) -> Result<String, JsValue> {
        let source = serde_json::from_str(source_json)
            .map_err(|error| js_sys::Error::new(&error.to_string()))?;
        let tokens = tokens_json
            .map(serde_json::from_str)
            .transpose()
            .map_err(|error| js_sys::Error::new(&error.to_string()))?;
        let normalized = normalize_theme_override(&self.package, &source, tokens.as_ref())
            .map_err(|message| js_sys::Error::new(&message))?;
        let mut output = serde_json::json!({"source": normalized.source});
        if let Some(tokens) = normalized.tokens {
            output
                .as_object_mut()
                .expect("literal object")
                .insert("tokens".to_owned(), tokens);
        }
        serde_json::to_string(&output)
            .map_err(|error| js_sys::Error::new(&error.to_string()).into())
    }
}

impl WasmClientGuardPolicy {
    pub(crate) fn with_priorities(
        package: DynamicCordisPackage,
        declared: Vec<String>,
        priorities: Arc<ClientPriorityAllocator>,
    ) -> Self {
        Self {
            package,
            context: ClientContextGuard::new(declared),
            priorities,
        }
    }
}

/// Builds the whitelisting Proxy around one real Client Cordis Context.
///
/// # Errors
///
/// Returns JavaScript construction or reflection failures before the Proxy is
/// published.
#[wasm_bindgen(js_name = createCordisClientContext)]
#[allow(clippy::needless_pass_by_value)]
pub fn create_client_context(
    ctx: JsValue,
    policy: WasmClientGuardPolicy,
    ledger: Array,
    claim: Function,
    report_failure: Function,
    is_context: Function,
) -> Result<JsValue, JsValue> {
    let policy = Rc::new(policy);
    let read_policy = {
        let policy = policy.clone();
        Closure::wrap(
            Box::new(move |property: String, exists: bool| policy.read(&property, exists))
                as Box<dyn FnMut(String, bool) -> Result<String, JsValue>>,
        )
    };
    let contains_policy = {
        let policy = policy.clone();
        Closure::wrap(Box::new(move |property: String| policy.contains(&property))
            as Box<dyn FnMut(String) -> bool>)
    };
    let verb_policy = {
        let policy = policy.clone();
        Closure::wrap(Box::new(move |property: String, timer_exists: bool| {
            policy.invoke_verb(&property, timer_exists)
        })
            as Box<dyn FnMut(String, bool) -> Result<(), JsValue>>)
    };
    let assignment_policy = Closure::wrap(Box::new(move |property: String| {
        ClientContextGuard::assignment_failure(&property)
    }) as Box<dyn FnMut(String) -> String>);
    let context_policy = Closure::wrap(Box::new(move |service: String| {
        ClientContextGuard::context_return_failure(&service)
    }) as Box<dyn FnMut(String) -> String>);
    let slot_policy = {
        let policy = policy.clone();
        Closure::wrap(Box::new(move |options: String, kind: Option<String>| {
            policy.normalize_slot(&options, kind.as_deref())
        })
            as Box<dyn FnMut(String, Option<String>) -> Result<String, JsValue>>)
    };
    let theme_policy = Closure::wrap(Box::new(move |source: String, tokens: Option<String>| {
        policy.normalize_theme(&source, tokens.as_deref())
    })
        as Box<dyn FnMut(String, Option<String>) -> Result<String, JsValue>>);
    let factory = construct_function(
        &[
            "ctx",
            "readPolicy",
            "containsPolicy",
            "verbPolicy",
            "assignmentPolicy",
            "contextPolicy",
            "slotPolicy",
            "themePolicy",
            "ledger",
            "claim",
            "reportFailure",
            "isContext",
        ],
        PROXY_BODY,
    )?;
    let arguments = Array::new();
    arguments.push(&ctx);
    arguments.push(&read_policy.into_js_value());
    arguments.push(&contains_policy.into_js_value());
    arguments.push(&verb_policy.into_js_value());
    arguments.push(&assignment_policy.into_js_value());
    arguments.push(&context_policy.into_js_value());
    arguments.push(&slot_policy.into_js_value());
    arguments.push(&theme_policy.into_js_value());
    arguments.push(&ledger);
    arguments.push(&claim);
    arguments.push(&report_failure);
    arguments.push(&is_context);
    factory.apply(&JsValue::UNDEFINED, &arguments)
}

const PROXY_BODY: &str = r"
const reject = error => { reportFailure(error); throw error; };
const denyContext = (value, service) => {
  if (isContext(value)) return reject(new Error(contextPolicy(service)));
  return value;
};
const guardService = (service, name) => new Proxy(service, {
  get(target, property) {
    const value = Reflect.get(target, property, target);
    if (typeof value !== 'function') return denyContext(value, name);
    return (...args) => {
      const result = Reflect.apply(value, target, args);
      return result instanceof Promise
        ? result.then(value => denyContext(value, name))
        : denyContext(result, name);
    };
  },
});
const guardSlots = slots => new Proxy(slots, {
  get(target, property) {
    const value = Reflect.get(target, property, target);
    if (property !== 'register') {
      if (typeof value !== 'function') return denyContext(value, 'slots');
      return (...args) => denyContext(Reflect.apply(value, target, args), 'slots');
    }
    return (options, component) => {
      try {
        const name = options && typeof options === 'object' ? options.name : undefined;
        const spec = typeof name === 'string' ? target.spec(name) : undefined;
        const normalized = JSON.parse(slotPolicy(
          JSON.stringify(options) ?? 'null',
          spec?.kind,
        ));
        const dispose = Reflect.apply(value, target, [normalized.options, component]);
        ledger.push({ slot: normalized.slot, priority: normalized.priority ?? undefined });
        claim(component);
        return dispose;
      } catch (error) { return reject(error); }
    };
  },
});
const guardTheme = theme => new Proxy(theme, {
  get(target, property) {
    const value = Reflect.get(target, property, target);
    if (property !== 'overrideTokens') {
      if (typeof value !== 'function') return denyContext(value, 'theme');
      return (...args) => {
        const result = Reflect.apply(value, target, args);
        return result instanceof Promise
          ? result.then(value => denyContext(value, 'theme'))
          : denyContext(result, 'theme');
      };
    }
    return function(source, tokens) {
      try {
        const normalized = JSON.parse(themePolicy(
          JSON.stringify(source) ?? 'null',
          arguments.length < 2 ? undefined : JSON.stringify(tokens) ?? 'null',
        ));
        const dispose = Reflect.apply(value, target, [normalized.source, normalized.tokens]);
        ctx.effect(() => dispose, 'cordis-client-runner: dynamic theme override layer');
        return dispose;
      } catch (error) { return reject(error); }
    };
  },
});
const readService = (name, requireDeclaration) => {
  const service = ctx.get(name);
  if (requireDeclaration) {
    try { readPolicy(name, service !== undefined); }
    catch (error) { return reject(error); }
  }
  const guarded = denyContext(service, name);
  if (guarded === null || (typeof guarded !== 'object' && typeof guarded !== 'function')) return guarded;
  if (name === 'slots') return guardSlots(guarded);
  if (name === 'theme') return guardTheme(guarded);
  return guardService(guarded, name);
};
return new Proxy({}, {
  get(_target, property) {
    if (property === 'get') return name => readService(name, false);
    if (typeof property !== 'string') return undefined;
    let access;
    try { access = readPolicy(property, ctx.get(property) !== undefined); }
    catch (error) { return reject(error); }
    if (access === 'verb') return (...args) => {
      try { verbPolicy(property, ctx.get('timer') !== undefined); }
      catch (error) { return reject(error); }
      return Reflect.apply(ctx[property], ctx, args);
    };
    return readService(property, true);
  },
  set(_target, property) { return reject(new Error(assignmentPolicy(String(property)))); },
  has(_target, property) { return typeof property === 'string' && containsPolicy(property); },
});
";

fn construct_function(parameters: &[&str], body: &str) -> Result<Function, JsValue> {
    let constructor =
        Reflect::get(&js_sys::global(), &JsValue::from_str("Function"))?.dyn_into::<Function>()?;
    let arguments = Array::new();
    for parameter in parameters {
        arguments.push(&JsValue::from_str(parameter));
    }
    arguments.push(&JsValue::from_str(body));
    Reflect::construct(&constructor, &arguments)?.dyn_into::<Function>()
}
