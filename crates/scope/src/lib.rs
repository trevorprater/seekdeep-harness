//! Opaque scope identities, ancestry, event routing, and insertion-ordered stores.

use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
};

use parking_lot::RwLock;
use seekdeep_cordis::{Context, EventArgs, EventSubjectToken, Fiber};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Package-owned scoped-dispatch invariants.
pub mod invariant;
/// Generated scope-filtered event catalog.
pub mod scoped_events;
/// Insertion-ordered named and anonymous registry entries.
pub mod store;

const SCOPE_META: &str = "seekdeep.scope";
const SCOPE_CARRIER_META: &str = "seekdeep.scope.carrier";

/// Opaque process-local scope identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ScopeKey(Uuid);

impl ScopeKey {
    /// Mints a fresh scope identity.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    /// Returns the underlying opaque UUID.
    #[must_use]
    pub fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl Default for ScopeKey {
    fn default() -> Self {
        Self::new()
    }
}

fn parents() -> &'static RwLock<HashMap<ScopeKey, ScopeKey>> {
    static PARENTS: OnceLock<RwLock<HashMap<ScopeKey, ScopeKey>>> = OnceLock::new();
    PARENTS.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Privileged handle that alone may move one already-bound parent link.
#[derive(Debug)]
pub struct ScopeParentBinding {
    key: ScopeKey,
}

impl ScopeParentBinding {
    /// Re-links the bound key after checking the resulting chain for cycles.
    ///
    /// # Errors
    ///
    /// Returns when the link would make the key its own ancestor.
    pub fn rebind(&self, parent: ScopeKey) -> anyhow::Result<()> {
        link_parent(self.key, parent)
    }

    /// Removes the parent link owned by this binding.
    ///
    /// Returns whether the link was still present. Repeated calls are safe.
    pub fn unbind(&self) -> bool {
        parents().write().remove(&self.key).is_some()
    }
}

/// Binds one key to a parent exactly once.
///
/// # Errors
///
/// Returns for an already-bound key or a cycle.
pub fn bind_scope_parent(key: ScopeKey, parent: ScopeKey) -> anyhow::Result<ScopeParentBinding> {
    let mut parent_map = parents().write();
    if parent_map.contains_key(&key) {
        anyhow::bail!(
            "seekdeep-scope: scope key is already bound to a parent; re-linking requires the binding returned by the original bind"
        );
    }
    link_parent_locked(&mut parent_map, key, parent)?;
    Ok(ScopeParentBinding { key })
}

fn link_parent(key: ScopeKey, parent: ScopeKey) -> anyhow::Result<()> {
    link_parent_locked(&mut parents().write(), key, parent)
}

fn link_parent_locked(
    parent_map: &mut HashMap<ScopeKey, ScopeKey>,
    key: ScopeKey,
    parent: ScopeKey,
) -> anyhow::Result<()> {
    let mut cursor = Some(parent);
    while let Some(current) = cursor {
        anyhow::ensure!(
            current != key,
            "seekdeep-scope: scope parent link would form a cycle"
        );
        cursor = parent_map.get(&current).copied();
    }
    parent_map.insert(key, parent);
    Ok(())
}

/// Returns one key's enclosing scope.
#[must_use]
pub fn scope_parent_of(key: ScopeKey) -> Option<ScopeKey> {
    parents().read().get(&key).copied()
}

/// Returns `[key, parent, grandparent, ...]` nearest first.
#[must_use]
pub fn scope_chain_of(key: Option<ScopeKey>) -> Vec<ScopeKey> {
    let parents = parents().read();
    let mut chain = Vec::new();
    let mut cursor = key;
    while let Some(current) = cursor {
        chain.push(current);
        cursor = parents.get(&current).copied();
    }
    chain
}

/// A minted scope and its shared quiescent lifecycle boundary.
#[derive(Debug)]
pub struct Scope {
    /// Context used for scope-owned registrations.
    pub context: Context,
    fiber: Arc<Fiber>,
}

impl Scope {
    /// Disposes every scope-owned effect in reverse order.
    ///
    /// Racing callers join effect-level disposal and observe the same outcome.
    ///
    /// # Errors
    ///
    /// Returns the shared aggregate disposal failure when any owned effect fails.
    pub async fn dispose(&self) -> anyhow::Result<()> {
        self.fiber.dispose().await
    }

    /// Returns the exact backing fiber for ordered composite ownership.
    #[must_use]
    pub fn fiber(&self) -> Arc<Fiber> {
        self.fiber.clone()
    }
}

/// Mints a synchronously usable scoped child context.
///
/// # Errors
///
/// Returns when optional parent binding fails.
pub fn create_scope(
    context: &Context,
    key: ScopeKey,
    parent: Option<ScopeKey>,
) -> anyhow::Result<Scope> {
    if let Some(parent) = parent {
        bind_scope_parent(key, parent)?;
    }
    let fiber = Fiber::active_child("scope");
    let context = context
        .with_fiber(fiber.clone())
        .with_meta(SCOPE_META, Value::String(key.0.to_string()));
    Ok(Scope { context, fiber })
}

/// Reads the nearest scope tag carried by a context.
#[must_use]
pub fn scope_of(context: &Context) -> Option<ScopeKey> {
    context
        .meta(SCOPE_META)
        .and_then(|value| value.as_str().map(str::to_owned))
        .and_then(|value| Uuid::parse_str(&value).ok())
        .map(ScopeKey)
}

/// Builds a routing context admitting untagged listeners and listeners at the
/// dispatch key or one of its ancestors.
#[must_use]
pub fn scope_target(context: &Context, key: Option<ScopeKey>) -> Context {
    let target = context.with_event_filter(move |listener| {
        let Some(tag) = scope_of(listener) else {
            return true;
        };
        scope_chain_of(key).contains(&tag)
    });
    target.with_meta(
        SCOPE_CARRIER_META,
        key.map_or(Value::Null, |key| Value::String(key.0.to_string())),
    )
}

/// Returns whether [`scope_target`] created this routing context.
#[must_use]
pub fn is_scope_carrier(context: &Context) -> bool {
    context.meta(SCOPE_CARRIER_META).is_some()
}

/// Reads a carrier's routing key, or `None` for an unkeyed/non-carrier context.
#[must_use]
pub fn carrier_key_of(context: &Context) -> Option<ScopeKey> {
    if !is_scope_carrier(context) {
        return None;
    }
    context
        .meta(SCOPE_CARRIER_META)
        .and_then(|value| value.as_str().map(str::to_owned))
        .and_then(|value| Uuid::parse_str(&value).ok())
        .map(ScopeKey)
}

/// Attaches the payload subject used to verify one scoped dispatch carrier.
#[must_use]
pub fn scoped_event_args(subject: ScopeKey, args: EventArgs) -> EventArgs {
    args.with_scope_subject(EventSubjectToken::new(subject.as_uuid()))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use seekdeep_cordis::{EventArgs, EventOptions, EventReply};
    use tokio::sync::oneshot;

    use super::*;

    #[test]
    fn parent_chain_rebinds_only_through_binding_and_rejects_cycles() {
        let preset_a = ScopeKey::new();
        let preset_b = ScopeKey::new();
        let agent = ScopeKey::new();
        let binding = bind_scope_parent(agent, preset_a).expect("bind");
        assert!(bind_scope_parent(agent, preset_b).is_err());
        binding.rebind(preset_b).expect("rebind");
        assert_eq!(scope_chain_of(Some(agent)), [agent, preset_b]);
        let child = ScopeKey::new();
        bind_scope_parent(child, agent).expect("child");
        assert!(binding.rebind(child).is_err());
    }

    #[test]
    fn binding_unbind_is_idempotent_and_removes_only_its_link() {
        let parent = ScopeKey::new();
        let child = ScopeKey::new();
        let binding = bind_scope_parent(child, parent).unwrap();
        assert_eq!(scope_parent_of(child), Some(parent));
        assert!(binding.unbind());
        assert_eq!(scope_parent_of(child), None);
        assert!(!binding.unbind());
    }

    #[tokio::test]
    async fn routes_events_up_the_scope_chain() {
        let root = Context::new();
        let preset_key = ScopeKey::new();
        let agent_key = ScopeKey::new();
        let other_key = ScopeKey::new();
        let preset = create_scope(&root, preset_key, None).expect("preset");
        let agent = create_scope(&root, agent_key, Some(preset_key)).expect("agent");
        let other = create_scope(&root, other_key, None).expect("other");
        let seen = Arc::new(parking_lot::Mutex::new(Vec::new()));
        for (context, label) in [
            (&root, "global"),
            (&preset.context, "preset"),
            (&agent.context, "agent"),
            (&other.context, "other"),
        ] {
            let seen = seen.clone();
            context
                .events()
                .on_sync(
                    context,
                    "scope/ping",
                    move |_, _| {
                        seen.lock().push(label);
                        Ok(EventReply::Undefined)
                    },
                    EventOptions::default(),
                )
                .expect("listen");
        }
        root.events()
            .emit(
                &scope_target(&root, Some(agent_key)),
                "scope/ping",
                &EventArgs::new(),
            )
            .expect("emit");
        let mut observed = seen.lock().clone();
        observed.sort_unstable();
        assert_eq!(observed, ["agent", "global", "preset"]);
        agent.dispose().await.expect("dispose agent");
        preset.dispose().await.expect("dispose preset");
        other.dispose().await.expect("dispose other");
    }

    #[test]
    fn routing_carriers_are_distinct_from_scoped_registration_contexts() {
        let root = Context::new();
        let key = ScopeKey::new();
        let scope = create_scope(&root, key, None).unwrap();
        let carrier = scope_target(&root, Some(key));
        let unkeyed = scope_target(&root, None);
        assert!(!is_scope_carrier(&root));
        assert!(!is_scope_carrier(&scope.context));
        assert!(is_scope_carrier(&carrier));
        assert!(is_scope_carrier(&unkeyed));
        assert_eq!(carrier_key_of(&carrier), Some(key));
        assert_eq!(carrier_key_of(&unkeyed), None);
        assert_eq!(scope_of(&carrier), None);
        assert_eq!(scope_of(&scope.context), Some(key));
    }

    #[tokio::test]
    async fn tags_inherit_nearest_and_scope_effects_are_immediately_usable() {
        let root = Context::new();
        let outer_key = ScopeKey::new();
        let inner_key = ScopeKey::new();
        let outer = create_scope(&root, outer_key, None).unwrap();
        let inner = create_scope(&outer.context, inner_key, None).unwrap();
        assert_eq!(scope_of(&root), None);
        assert_eq!(scope_of(&outer.context), Some(outer_key));
        assert_eq!(scope_of(&outer.context.clone()), Some(outer_key));
        assert_eq!(scope_of(&inner.context), Some(inner_key));

        let disposed = Arc::new(AtomicBool::new(false));
        let effect_disposed = disposed.clone();
        inner
            .context
            .own(seekdeep_cordis::fiber::EffectHandle::synchronous(
                "synchronous scope effect",
                move || {
                    effect_disposed.store(true, Ordering::Release);
                    Ok(())
                },
            ))
            .unwrap();
        inner.dispose().await.unwrap();
        assert!(disposed.load(Ordering::Acquire));
        outer.dispose().await.unwrap();
    }

    #[tokio::test]
    async fn public_and_raw_scope_disposal_share_one_quiescence_boundary() {
        let root = Context::new();
        let scope = Arc::new(create_scope(&root, ScopeKey::new(), None).unwrap());
        let (started, started_rx) = oneshot::channel();
        let (release, release_rx) = oneshot::channel();
        scope
            .context
            .own(seekdeep_cordis::fiber::EffectHandle::new(
                "delayed scope effect",
                move || {
                    Box::pin(async move {
                        let _ = started.send(());
                        let _ = release_rx.await;
                        Ok(())
                    })
                },
            ))
            .unwrap();
        let raw_fiber = scope.fiber();
        let raw = tokio::spawn(async move { raw_fiber.dispose().await });
        started_rx.await.unwrap();
        let public_scope = scope.clone();
        let mut public = tokio::spawn(async move { public_scope.dispose().await });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut public)
                .await
                .is_err()
        );
        release.send(()).unwrap();
        raw.await.unwrap().unwrap();
        public.await.unwrap().unwrap();
        scope.dispose().await.unwrap();
    }

    #[tokio::test]
    async fn target_preserves_base_filter_and_global_listener_semantics() {
        let root = Context::new();
        let key = ScopeKey::new();
        let scope = create_scope(&root, key, None).unwrap();
        let ordinary = Arc::new(AtomicBool::new(false));
        let ordinary_seen = ordinary.clone();
        scope
            .context
            .events()
            .on_sync(
                &scope.context,
                "scope/filter",
                move |_, _| {
                    ordinary_seen.store(true, Ordering::Release);
                    Ok(EventReply::Undefined)
                },
                EventOptions::default(),
            )
            .unwrap();
        let global = Arc::new(AtomicBool::new(false));
        let global_seen = global.clone();
        scope
            .context
            .events()
            .on_sync(
                &scope.context,
                "scope/filter",
                move |_, _| {
                    global_seen.store(true, Ordering::Release);
                    Ok(EventReply::Undefined)
                },
                EventOptions {
                    global: true,
                    ..EventOptions::default()
                },
            )
            .unwrap();
        let vetoing_base = root.with_event_filter(|_| false);
        root.events()
            .emit(
                &scope_target(&vetoing_base, Some(key)),
                "scope/filter",
                &EventArgs::new(),
            )
            .unwrap();
        assert!(!ordinary.load(Ordering::Acquire));
        assert!(global.load(Ordering::Acquire));
        scope.dispose().await.unwrap();
    }
}
