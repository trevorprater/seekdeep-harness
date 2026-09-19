//! Insertion-ordered independently owned entry tables and scoped layers.

use std::{collections::HashMap, sync::Arc};

use indexmap::IndexMap;
use parking_lot::{Mutex, RwLock};
use seekdeep_cordis::{Context, fiber::EffectHandle};
use uuid::Uuid;

use crate::{ScopeKey, scope_chain_of, scope_of};

type UndoFn = Box<dyn FnOnce() + Send + 'static>;

/// Idempotent exact-entry undo.
#[derive(Clone)]
pub struct EntryUndo(Arc<Mutex<Option<UndoFn>>>);

impl std::fmt::Debug for EntryUndo {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("EntryUndo").finish_non_exhaustive()
    }
}

impl EntryUndo {
    /// Creates an idempotent wrapper around one synchronous undo operation.
    pub fn new(undo: impl FnOnce() + Send + 'static) -> Self {
        Self(Arc::new(Mutex::new(Some(Box::new(undo)))))
    }

    /// Removes the exact insertion once.
    pub fn dispose(&self) {
        if let Some(undo) = self.0.lock().take() {
            undo();
        }
    }
}

struct NamedEntry<V> {
    name: String,
    id: Uuid,
    value: V,
    active: bool,
}

struct NamedGeneration<V> {
    entries: Vec<NamedEntry<V>>,
    active: HashMap<String, usize>,
}

impl<V> Default for NamedGeneration<V> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            active: HashMap::new(),
        }
    }
}

struct NamedInner<V> {
    generation: Arc<Mutex<NamedGeneration<V>>>,
}

/// Insertion-ordered named values with exact idempotent undo.
///
/// Iterators remain live while their nonempty generation exists: they see
/// later insertions and skip removals. Draining the table detaches them from
/// the next generation, matching JavaScript `Map` iteration.
pub struct NamedEntries<V> {
    inner: Arc<Mutex<NamedInner<V>>>,
    duplicate: Arc<dyn Fn(&str) -> anyhow::Error + Send + Sync>,
}

impl<V> std::fmt::Debug for NamedEntries<V> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NamedEntries")
            .field("empty", &self.is_empty_unbounded())
            .finish_non_exhaustive()
    }
}

impl<V> NamedEntries<V> {
    fn is_empty_unbounded(&self) -> bool {
        self.inner.lock().generation.lock().active.is_empty()
    }
}

impl<V: Clone + Send + 'static> NamedEntries<V> {
    /// Creates an empty table with caller-owned duplicate diagnostics.
    pub fn new(duplicate: impl Fn(&str) -> anyhow::Error + Send + Sync + 'static) -> Self {
        Self {
            inner: Arc::new(Mutex::new(NamedInner {
                generation: Arc::new(Mutex::new(NamedGeneration::default())),
            })),
            duplicate: Arc::new(duplicate),
        }
    }

    /// Inserts one unique name.
    ///
    /// # Errors
    ///
    /// Returns the table's duplicate diagnostic when occupied.
    pub fn insert(&self, name: impl Into<String>, value: V) -> anyhow::Result<EntryUndo> {
        let name = name.into();
        let id = Uuid::now_v7();
        let generation = {
            let inner = self.inner.lock();
            let generation = inner.generation.clone();
            let mut data = generation.lock();
            if data.active.contains_key(&name) {
                return Err((self.duplicate)(&name));
            }
            let index = data.entries.len();
            data.entries.push(NamedEntry {
                name: name.clone(),
                id,
                value,
                active: true,
            });
            data.active.insert(name.clone(), index);
            drop(data);
            generation
        };
        let inner = self.inner.clone();
        Ok(EntryUndo::new(move || {
            let mut table = inner.lock();
            let mut data = generation.lock();
            let Some(index) = data.active.get(&name).copied() else {
                return;
            };
            if data.entries[index].id != id {
                return;
            }
            data.entries[index].active = false;
            data.active.remove(&name);
            if data.active.is_empty() && Arc::ptr_eq(&table.generation, &generation) {
                table.generation = Arc::new(Mutex::new(NamedGeneration::default()));
            }
        }))
    }

    /// Clones one retained value.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<V> {
        let inner = self.inner.lock();
        let data = inner.generation.lock();
        let index = *data.active.get(name)?;
        Some(data.entries[index].value.clone())
    }

    /// Returns whether a name is occupied.
    #[must_use]
    pub fn contains_key(&self, name: &str) -> bool {
        self.inner
            .lock()
            .generation
            .lock()
            .active
            .contains_key(name)
    }

    /// Iterates live names in insertion order.
    #[must_use]
    pub fn keys(&self) -> NamedKeys<V> {
        NamedKeys::new(self.inner.lock().generation.clone())
    }

    /// Iterates live entries in insertion order.
    #[must_use]
    pub fn entries(&self) -> NamedEntryIter<V> {
        NamedEntryIter::new(self.inner.lock().generation.clone())
    }

    /// Iterates live values in insertion order.
    #[must_use]
    pub fn values(&self) -> NamedValues<V> {
        NamedValues::new(self.inner.lock().generation.clone())
    }

    /// Whether the current generation is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.is_empty_unbounded()
    }
}

macro_rules! named_iterator {
    ($name:ident, $item:ty, $map:expr) => {
        /// Live iterator over one named-table generation.
        pub struct $name<V> {
            generation: Arc<Mutex<NamedGeneration<V>>>,
            cursor: usize,
            done: bool,
        }

        impl<V> $name<V> {
            fn new(generation: Arc<Mutex<NamedGeneration<V>>>) -> Self {
                Self {
                    generation,
                    cursor: 0,
                    done: false,
                }
            }
        }

        impl<V: Clone> Iterator for $name<V> {
            type Item = $item;

            fn next(&mut self) -> Option<Self::Item> {
                if self.done {
                    return None;
                }
                let data = self.generation.lock();
                while self.cursor < data.entries.len() {
                    let entry = &data.entries[self.cursor];
                    self.cursor += 1;
                    if entry.active {
                        return Some(($map)(entry));
                    }
                }
                self.done = true;
                None
            }
        }
    };
}

named_iterator!(NamedKeys, String, |entry: &NamedEntry<V>| entry
    .name
    .clone());
named_iterator!(NamedEntryIter, (String, V), |entry: &NamedEntry<V>| (
    entry.name.clone(),
    entry.value.clone()
));
named_iterator!(NamedValues, V, |entry: &NamedEntry<V>| entry.value.clone());

struct AnonymousEntry<V> {
    id: Uuid,
    value: V,
    active: bool,
}

struct AnonymousGeneration<V> {
    entries: Vec<AnonymousEntry<V>>,
    active: usize,
}

impl<V> Default for AnonymousGeneration<V> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            active: 0,
        }
    }
}

struct AnonymousInner<V> {
    generation: Arc<Mutex<AnonymousGeneration<V>>>,
}

/// Insertion-ordered anonymous values with independent identities.
pub struct AnonymousEntries<V> {
    inner: Arc<Mutex<AnonymousInner<V>>>,
}

impl<V> std::fmt::Debug for AnonymousEntries<V> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AnonymousEntries")
            .field("empty", &self.inner.lock().generation.lock().active.eq(&0))
            .finish()
    }
}

impl<V> Default for AnonymousEntries<V> {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(AnonymousInner {
                generation: Arc::new(Mutex::new(AnonymousGeneration::default())),
            })),
        }
    }
}

impl<V: Clone + Send + 'static> AnonymousEntries<V> {
    /// Appends an independently owned value.
    pub fn append(&self, value: V) -> EntryUndo {
        let id = Uuid::now_v7();
        let generation = {
            let inner = self.inner.lock();
            let generation = inner.generation.clone();
            let mut data = generation.lock();
            data.entries.push(AnonymousEntry {
                id,
                value,
                active: true,
            });
            data.active += 1;
            drop(data);
            generation
        };
        let inner = self.inner.clone();
        EntryUndo::new(move || {
            let mut table = inner.lock();
            let mut data = generation.lock();
            let Some(entry) = data
                .entries
                .iter_mut()
                .find(|entry| entry.id == id && entry.active)
            else {
                return;
            };
            entry.active = false;
            data.active -= 1;
            if data.active == 0 && Arc::ptr_eq(&table.generation, &generation) {
                table.generation = Arc::new(Mutex::new(AnonymousGeneration::default()));
            }
        })
    }

    /// Iterates live values in insertion order.
    #[must_use]
    pub fn values(&self) -> AnonymousValues<V> {
        AnonymousValues {
            generation: self.inner.lock().generation.clone(),
            cursor: 0,
            done: false,
        }
    }

    /// Whether the current generation is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.lock().generation.lock().active == 0
    }
}

/// Live iterator over one anonymous-table generation.
pub struct AnonymousValues<V> {
    generation: Arc<Mutex<AnonymousGeneration<V>>>,
    cursor: usize,
    done: bool,
}

impl<V: Clone> Iterator for AnonymousValues<V> {
    type Item = V;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let data = self.generation.lock();
        while self.cursor < data.entries.len() {
            let entry = &data.entries[self.cursor];
            self.cursor += 1;
            if entry.active {
                return Some(entry.value.clone());
            }
        }
        self.done = true;
        None
    }
}

/// One scope's aggregate contribution to a registry.
pub trait ScopeLayer: Send + Sync + 'static {
    /// Whether every table in this layer is empty.
    fn is_empty(&self) -> bool;
}

type LayerFactory<L> = dyn Fn(Option<ScopeKey>) -> anyhow::Result<L> + Send + Sync;
type ChangeCallback = dyn Fn() -> anyhow::Result<()> + Send + Sync;

/// Global and exact-scope layers for one registry.
pub struct ScopedLayers<L: ScopeLayer> {
    /// Eagerly constructed context-global layer.
    pub global: Arc<L>,
    scoped: Arc<RwLock<HashMap<ScopeKey, Arc<L>>>>,
    create_layer: Arc<LayerFactory<L>>,
    on_change: Arc<ChangeCallback>,
}

impl<L: ScopeLayer> std::fmt::Debug for ScopedLayers<L> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScopedLayers")
            .field("scoped_len", &self.scoped.read().len())
            .finish_non_exhaustive()
    }
}

impl<L: ScopeLayer> ScopedLayers<L> {
    /// Constructs global state eagerly with infallible callbacks.
    pub fn new(
        create_layer: impl Fn(Option<ScopeKey>) -> L + Send + Sync + 'static,
        on_change: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let global = Arc::new(create_layer(None));
        Self {
            global,
            scoped: Arc::new(RwLock::new(HashMap::new())),
            create_layer: Arc::new(move |scope| Ok(create_layer(scope))),
            on_change: Arc::new(move || {
                on_change();
                Ok(())
            }),
        }
    }

    /// Constructs global state eagerly with fallible callbacks.
    ///
    /// # Errors
    ///
    /// Returns a failure from the global layer factory.
    pub fn try_new(
        create_layer: impl Fn(Option<ScopeKey>) -> anyhow::Result<L> + Send + Sync + 'static,
        on_change: impl Fn() -> anyhow::Result<()> + Send + Sync + 'static,
    ) -> anyhow::Result<Self> {
        let create_layer: Arc<LayerFactory<L>> = Arc::new(create_layer);
        let global = Arc::new(create_layer(None)?);
        Ok(Self {
            global,
            scoped: Arc::new(RwLock::new(HashMap::new())),
            create_layer,
            on_change: Arc::new(on_change),
        })
    }

    /// Reads an existing exact-scope overlay without creating it.
    #[must_use]
    pub fn peek(&self, scope: Option<ScopeKey>) -> Option<Arc<L>> {
        self.scoped.read().get(&scope?).cloned()
    }

    /// Returns existing ancestry overlays farthest-first, exact scope last.
    #[must_use]
    pub fn chain_layers(&self, scope: Option<ScopeKey>) -> Vec<Arc<L>> {
        scope_chain_of(scope)
            .into_iter()
            .rev()
            .filter_map(|key| self.scoped.read().get(&key).cloned())
            .collect()
    }

    /// Merges global named entries and scope shadows nearest-last.
    #[must_use]
    pub fn merge<V: Clone + Send + 'static>(
        &self,
        scope: Option<ScopeKey>,
        pick: impl Fn(&L) -> &NamedEntries<V>,
    ) -> IndexMap<String, V> {
        let mut merged = pick(&self.global).entries().collect::<IndexMap<_, _>>();
        for layer in self.chain_layers(scope) {
            merged.extend(pick(&layer).entries());
        }
        merged
    }

    /// Attaches one synchronous layer mutation to its context's ownership.
    ///
    /// Initial notification is transactional: if it fails, the mutation is
    /// undone, its empty scoped layer is reclaimed, and disposal notification
    /// still runs.
    ///
    /// # Errors
    ///
    /// Returns factory, mutation, notification, or inactive-context failures.
    pub fn effect(
        &self,
        context: &Context,
        action: impl FnOnce(&L) -> anyhow::Result<EntryUndo>,
        options: LayerEffectOptions,
    ) -> anyhow::Result<EffectHandle> {
        let scope = scope_of(context);
        let (layer, created) = self.layer_for_mutation(scope)?;
        let undo = match action(&layer) {
            Ok(undo) => undo,
            Err(error) => {
                if created && layer.is_empty() {
                    remove_exact_layer(&self.scoped, scope, &layer);
                }
                return Err(error);
            }
        };

        if options.notify
            && let Err(error) = (self.on_change)()
        {
            undo.dispose();
            if layer.is_empty() {
                remove_exact_layer(&self.scoped, scope, &layer);
            }
            if let Err(rollback_error) = (self.on_change)() {
                return Err(error.context(format!(
                    "rollback change notification also failed: {rollback_error:#}"
                )));
            }
            return Err(error);
        }

        let scoped = self.scoped.clone();
        let on_change = self.on_change.clone();
        let notify = options.notify;
        let effect = EffectHandle::synchronous(options.label, move || {
            undo.dispose();
            if layer.is_empty() {
                remove_exact_layer(&scoped, scope, &layer);
            }
            if notify {
                on_change()?;
            }
            Ok(())
        });
        match context.own(effect.clone()) {
            Ok(effect) => Ok(effect),
            Err(error) => {
                futures::executor::block_on(effect.dispose()).ok();
                Err(error.into())
            }
        }
    }

    fn layer_for_mutation(&self, scope: Option<ScopeKey>) -> anyhow::Result<(Arc<L>, bool)> {
        let Some(scope) = scope else {
            return Ok((self.global.clone(), false));
        };
        let mut scoped = self.scoped.write();
        if let Some(layer) = scoped.get(&scope) {
            return Ok((layer.clone(), false));
        }
        let layer = Arc::new((self.create_layer)(Some(scope))?);
        scoped.insert(scope, layer.clone());
        Ok((layer, true))
    }
}

/// Effect labeling and change-notification behavior.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerEffectOptions {
    /// Diagnostic label retained by the Cordis effect.
    pub label: String,
    /// Whether setup and teardown notify the registry.
    pub notify: bool,
}

impl LayerEffectOptions {
    /// Creates options with notification enabled.
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            notify: true,
        }
    }

    /// Selects whether setup and teardown notify the registry.
    #[must_use]
    pub fn notify(mut self, notify: bool) -> Self {
        self.notify = notify;
        self
    }
}

fn remove_exact_layer<L: ScopeLayer>(
    scoped: &RwLock<HashMap<ScopeKey, Arc<L>>>,
    scope: Option<ScopeKey>,
    layer: &Arc<L>,
) {
    let Some(scope) = scope else {
        return;
    };
    let mut scoped = scoped.write();
    if scoped
        .get(&scope)
        .is_some_and(|current| Arc::ptr_eq(current, layer))
    {
        scoped.remove(&scope);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::create_scope;

    struct TestLayer {
        named: NamedEntries<i32>,
        anonymous: AnonymousEntries<String>,
    }

    impl TestLayer {
        fn new(scope: Option<ScopeKey>) -> Self {
            Self {
                named: NamedEntries::new(move |name| {
                    anyhow::anyhow!(
                        "{} duplicate: {name}",
                        if scope.is_some() { "scoped" } else { "global" }
                    )
                }),
                anonymous: AnonymousEntries::default(),
            }
        }
    }

    impl ScopeLayer for TestLayer {
        fn is_empty(&self) -> bool {
            self.named.is_empty() && self.anonymous.is_empty()
        }
    }

    #[test]
    fn named_entries_have_live_generations_and_exact_idempotent_undo() {
        let entries = NamedEntries::new(|name| anyhow::anyhow!("duplicate: {name}"));
        let undo_a = entries.insert("a", 1).expect("a");
        let mut values = entries.values();
        assert_eq!(values.next(), Some(1));
        let undo_b = entries.insert("b", 2).expect("b");
        assert_eq!(values.collect::<Vec<_>>(), [2]);
        assert_eq!(entries.keys().collect::<Vec<_>>(), ["a", "b"]);
        assert!(entries.insert("a", 3).is_err());
        undo_a.dispose();
        entries.insert("a", 3).expect("replacement");
        undo_a.dispose();
        undo_b.dispose();
        assert_eq!(entries.entries().collect::<Vec<_>>(), [("a".to_owned(), 3)]);
    }

    #[test]
    fn draining_detaches_old_iterators_from_replacements() {
        let named = NamedEntries::new(|name| anyhow::anyhow!("duplicate: {name}"));
        let undo = named.insert("first", 1).expect("first");
        let mut values = named.values();
        assert_eq!(values.next(), Some(1));
        undo.dispose();
        named.insert("replacement", 2).expect("replacement");
        assert_eq!(values.next(), None);
        assert_eq!(named.values().collect::<Vec<_>>(), [2]);

        let anonymous = AnonymousEntries::default();
        let undo = anonymous.append(1);
        let mut values = anonymous.values();
        assert_eq!(values.next(), Some(1));
        undo.dispose();
        anonymous.append(2);
        assert_eq!(values.next(), None);
        assert_eq!(anonymous.values().collect::<Vec<_>>(), [2]);
    }

    #[tokio::test]
    async fn scoped_layers_shadow_reclaim_notify_and_rollback() {
        let root = Context::new();
        let parent_key = ScopeKey::new();
        let child_key = ScopeKey::new();
        let parent = create_scope(&root, parent_key, None).expect("parent");
        let child = create_scope(&root, child_key, Some(parent_key)).expect("child");
        let changes = Arc::new(AtomicUsize::new(0));
        let notify_changes = changes.clone();
        let layers = ScopedLayers::new(TestLayer::new, move || {
            notify_changes.fetch_add(1, Ordering::SeqCst);
        });
        layers.global.named.insert("a", 1).expect("global a");
        layers
            .global
            .named
            .insert("shared", 1)
            .expect("global shared");
        let parent_effect = layers
            .effect(
                &parent.context,
                |layer| layer.named.insert("shared", 2),
                LayerEffectOptions::new("parent"),
            )
            .expect("parent effect");
        let child_effect = layers
            .effect(
                &child.context,
                |layer| layer.named.insert("tail", 3),
                LayerEffectOptions::new("child").notify(false),
            )
            .expect("child effect");
        assert_eq!(
            layers.merge(Some(child_key), |layer| &layer.named),
            IndexMap::from([
                ("a".to_owned(), 1),
                ("shared".to_owned(), 2),
                ("tail".to_owned(), 3),
            ])
        );
        assert_eq!(changes.load(Ordering::SeqCst), 1);
        child_effect.dispose().await.expect("child dispose");
        assert!(layers.peek(Some(child_key)).is_none());
        parent_effect.dispose().await.expect("parent dispose");
        assert!(layers.peek(Some(parent_key)).is_none());
        assert_eq!(changes.load(Ordering::SeqCst), 2);

        let attempts = Arc::new(AtomicUsize::new(0));
        let notify_attempts = attempts.clone();
        let failing = ScopedLayers::try_new(
            |scope| Ok(TestLayer::new(scope)),
            move || {
                if notify_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    anyhow::bail!("change failed");
                }
                Ok(())
            },
        )
        .expect("layers");
        let result = failing.effect(
            &child.context,
            |layer| layer.named.insert("rollback", 1),
            LayerEffectOptions::new("rollback"),
        );
        assert!(result.is_err());
        assert!(failing.peek(Some(child_key)).is_none());
        assert_eq!(attempts.load(Ordering::SeqCst), 2);

        child.dispose().await.expect("child scope");
        parent.dispose().await.expect("parent scope");
    }

    #[tokio::test]
    async fn scoped_layers_notify_in_order_and_reclaim_anonymous_layers() {
        let root = Context::new();
        let key = ScopeKey::new();
        let scope = create_scope(&root, key, None).expect("scope");
        let events = Arc::new(parking_lot::Mutex::new(Vec::<&'static str>::new()));
        let effect_events = events.clone();
        let layers = ScopedLayers::new(TestLayer::new, move || {
            effect_events.lock().push("notify");
        });

        let action_events = events.clone();
        let undo_events = events.clone();
        let effect = layers
            .effect(
                &scope.context,
                move |layer| {
                    action_events.lock().push("action");
                    let undo = layer.anonymous.append("kept".to_owned());
                    Ok(EntryUndo::new(move || {
                        undo_events.lock().push("undo");
                        undo.dispose();
                    }))
                },
                LayerEffectOptions::new("store.order"),
            )
            .expect("effect");

        assert_eq!(events.lock().clone(), ["action", "notify"]);
        effect.dispose().await.expect("dispose");
        effect.dispose().await.expect("idempotent dispose");
        assert_eq!(
            events.lock().clone(),
            ["action", "notify", "undo", "notify"]
        );
        assert!(layers.peek(Some(key)).is_none());
        scope.dispose().await.expect("scope");
    }
}
