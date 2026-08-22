//! Client Context allowlist plus Slot and Theme registration policy.

use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
};

use seekdeep_cordis_dynamic_types::DynamicCordisPackage;
use serde_json::{Map, Value};

const CONTEXT_VERBS: &[&str] = &[
    "effect",
    "on",
    "once",
    "provide",
    "timeout",
    "interval",
    "setTimeout",
    "setInterval",
    "throttle",
    "debounce",
];
const TIMER_VERBS: &[&str] = &[
    "timeout",
    "interval",
    "setTimeout",
    "setInterval",
    "throttle",
    "debounce",
];

/// Context property classification returned to the WASM Proxy binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientContextAccess {
    /// Optional `ctx.get` lookup.
    Get,
    /// Lifecycle-safe Context verb.
    Verb,
    /// Direct declared Service property.
    Service,
}

/// Rust-owned declared-Service and Context-verb allowlist.
#[derive(Clone, Debug)]
pub struct ClientContextGuard {
    declared: Arc<BTreeSet<String>>,
}

impl ClientContextGuard {
    /// Builds one immutable declaration set.
    #[must_use]
    pub fn new(declared: impl IntoIterator<Item = String>) -> Self {
        Self {
            declared: Arc::new(declared.into_iter().collect()),
        }
    }

    /// Classifies a property read or returns its exact teaching error.
    ///
    /// `service_exists` distinguishes undeclared live Services from withheld
    /// framework internals.
    ///
    /// # Errors
    ///
    /// Returns declaration or withheld-member teaching text.
    pub fn read(
        &self,
        property: &str,
        service_exists: bool,
    ) -> Result<ClientContextAccess, String> {
        if property == "get" {
            return Ok(ClientContextAccess::Get);
        }
        if CONTEXT_VERBS.contains(&property) {
            return Ok(ClientContextAccess::Verb);
        }
        if self.declared.contains(property) {
            return Ok(ClientContextAccess::Service);
        }
        Err(Self::denied(property, service_exists))
    }

    /// Validates a lazy Context verb immediately before invocation.
    ///
    /// # Errors
    ///
    /// Timer helpers require the plugin to declare `timer`; other lifecycle
    /// verbs are always accepted.
    pub fn invoke_verb(&self, property: &str, timer_exists: bool) -> Result<(), String> {
        if TIMER_VERBS.contains(&property) && !self.declared.contains("timer") {
            Err(Self::denied("timer", timer_exists))
        } else {
            Ok(())
        }
    }

    /// Whether the facade's `in` operator reports one property reachable.
    #[must_use]
    pub fn contains(&self, property: &str) -> bool {
        property == "get"
            || CONTEXT_VERBS.contains(&property)
                && (!TIMER_VERBS.contains(&property) || self.declared.contains("timer"))
            || self.declared.contains(property)
    }

    /// Exact read-only assignment failure.
    #[must_use]
    pub fn assignment_failure(property: &str) -> String {
        format!("dynamic ctx is read-only; cannot assign {property:?}")
    }

    /// Rejects a Context-valued Service result before it reaches JavaScript.
    #[must_use]
    pub fn context_return_failure(service: &str) -> String {
        format!(
            "service \"{service}\" returned a cordis Context, which the dynamic facade does not expose. Operate through your own plugin ctx and the services you declared — never another context."
        )
    }

    fn denied(property: &str, service_exists: bool) -> String {
        if service_exists {
            format!(
                "service \"{property}\" is not declared by your plugin. Declare it on the plugin you return: {{ inject: ['{property}', …], apply(ctx) {{ … }} }} — a plain `function` has no declaration site, so use the object form. The runtime then parks the package if the provider unloads."
            )
        } else {
            format!(
                "dynamic ctx does not expose \"{property}\". Available: ctx.on / ctx.provide / timer helpers after injecting timer, and any service your returned plugin declared in inject (slots and theme are the usual UI seats). Framework internals are withheld by design."
            )
        }
    }
}

/// Page-global descending shadow priority allocator.
#[derive(Debug, Default)]
pub struct ClientPriorityAllocator {
    next: AtomicI64,
}

impl ClientPriorityAllocator {
    /// Returns `-1`, `-2`, … so later registrations sort first.
    pub fn allocate(&self) -> i64 {
        self.next.fetch_sub(1, Ordering::AcqRel) - 1
    }
}

/// One accepted Slot registration and its ledger projection.
#[derive(Clone, Debug, PartialEq)]
pub struct NormalizedSlotRegistration {
    /// Target Slot name.
    pub slot: String,
    /// Options passed to the real registry.
    pub options: Map<String, Value>,
    /// Assigned priority, absent for a priority-free chain registration.
    pub priority: Option<Value>,
}

/// Validates and rewrites one dynamic Slot registration.
///
/// # Errors
///
/// Rejects malformed options and invalid `tool.view.cordis` keys.
pub fn normalize_slot_registration(
    package: &DynamicCordisPackage,
    raw_options: &Value,
    slot_kind: Option<&str>,
    priorities: &ClientPriorityAllocator,
) -> Result<NormalizedSlotRegistration, String> {
    let mut options = raw_options.as_object().cloned().ok_or_else(|| {
        "slots.register(options, component) needs an options object with a `name`".to_owned()
    })?;
    let slot = options
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            "slots.register options need a string `name` (the target slot key)".to_owned()
        })?;
    if slot == "tool.view.cordis" {
        if options.get("key").and_then(Value::as_str) != Some("self") {
            return Err(
                "tool.view.cordis only accepts key \"self\"; the runtime binds it to this Package"
                    .to_owned(),
            );
        }
        options.insert(
            "key".to_owned(),
            Value::String(format!("{}.{}", package.plugin_id, package.package_id)),
        );
    }
    let priority = if slot_kind == Some("chain") {
        options
            .get("priority")
            .filter(|priority| priority.is_number())
            .cloned()
    } else {
        let priority = priorities.allocate();
        let priority = Value::from(priority);
        options.insert("priority".to_owned(), priority.clone());
        Some(priority)
    };
    Ok(NormalizedSlotRegistration {
        slot,
        options,
        priority,
    })
}

/// Package-pinned Theme override call.
#[derive(Clone, Debug, PartialEq)]
pub struct NormalizedThemeOverride {
    /// Forced package identity source.
    pub source: String,
    /// Caller token map, including JSON `null` when explicitly passed.
    pub tokens: Option<Value>,
}

/// Forces Theme layer ownership to this Package while preserving two-argument shape.
///
/// # Errors
///
/// Teaches the two-argument form when an object token map arrives first.
pub fn normalize_theme_override(
    package: &DynamicCordisPackage,
    source: &Value,
    tokens: Option<&Value>,
) -> Result<NormalizedThemeOverride, String> {
    if tokens.is_none() && source.is_object() {
        return Err(
            "theme.overrideTokens(source, tokens) takes two arguments; source is replaced with your package id, so pass any string first and the token map second: overrideTokens('mine', { '--dsw-alias-…': { light: '…', dark: '…' } })"
                .to_owned(),
        );
    }
    Ok(NormalizedThemeOverride {
        source: format!("{}.{}", package.plugin_id, package.package_id),
        tokens: tokens.cloned(),
    })
}
