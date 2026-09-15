//! Single-owner Slot ledger with exact declaration, shadowing, and notification lifecycles.

use std::{
    cell::RefCell,
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fmt,
    hash::{Hash, Hasher},
    rc::{Rc, Weak},
};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

/// Stable Slot identity crossing browser inspection and registration boundaries.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SlotName(String);

impl SlotName {
    /// Wraps one exact Slot key.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the exact key spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SlotName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Slot cardinality and dispatch mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SlotKind {
    /// One shadowing cell for the entire Slot.
    Single,
    /// One ordered and shadowable cell per entry id.
    List,
    /// One shadowable cell per dispatch key.
    Keyed,
    /// Selector-routed entries evaluated in priority order.
    Chain,
}

/// Runtime data scope of one Slot declaration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SlotScope {
    /// Global page state.
    Root,
    /// Current Session when one exists.
    SessionMaybe,
    /// Strict Session-bound state.
    Session,
}

/// Runtime dispatch declaration supplied by a parent entry.
#[derive(Clone, Debug, PartialEq)]
pub struct SlotSpec<I> {
    /// Cardinality and dispatch mode.
    pub kind: SlotKind,
    /// Runtime data scope.
    pub scope: SlotScope,
    /// Parent-supplied common inject face.
    pub inject: Option<I>,
}

impl<I> SlotSpec<I> {
    /// Creates a declaration without a common inject face.
    #[must_use]
    pub const fn new(kind: SlotKind, scope: SlotScope) -> Self {
        Self {
            kind,
            scope,
            inject: None,
        }
    }
}

/// Stable identity assigned to a shared Store handle by its host adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StoreHandleId(u64);

impl StoreHandleId {
    /// Wraps one adapter-owned Store handle identity.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
}

/// Store declaration identity relevant to registry validation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotStoreDeclaration {
    /// Framework calls a factory per entry and scope; no shared identity is pinned.
    Factory,
    /// One shared handle whose live mounts must all use one scope.
    Shared(StoreHandleId),
}

/// Type-erased registration options consumed by the portable core.
#[derive(Clone, Debug, PartialEq)]
pub struct SlotRegistrationOptions<I> {
    /// Target declared Slot.
    pub name: SlotName,
    /// Keyed cell identity.
    pub key: Option<String>,
    /// List cell identity.
    pub id: Option<String>,
    /// List display order.
    pub order: Option<f64>,
    /// Whether the mandatory chain selector exists.
    pub has_selector: bool,
    /// Shadowing or chain priority.
    pub priority: Option<f64>,
    /// Child declaration and render-authorization table.
    pub children: IndexMap<SlotName, SlotSpec<I>>,
    /// Store seat identity.
    pub store: Option<SlotStoreDeclaration>,
    /// Dictionary namespace.
    pub locale: Option<String>,
    /// Registrant label used only in diagnostics and inspection.
    pub registrant: Option<String>,
}

impl<I> SlotRegistrationOptions<I> {
    /// Creates the common option shape for one target Slot.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: SlotName::new(name),
            key: None,
            id: None,
            order: None,
            has_selector: false,
            priority: None,
            children: IndexMap::new(),
            store: None,
            locale: None,
            registrant: None,
        }
    }
}

/// Stable identity of one ledger entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SlotEntryId(u64);

impl SlotEntryId {
    /// Wraps an adapter-retained ledger identity.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the numeric identity.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// One stored registration and its host-specific payload.
#[derive(Debug)]
pub struct SlotEntry<P, I> {
    id: SlotEntryId,
    sequence: u64,
    /// Host-specific component, inject, selector, and Store values.
    pub payload: P,
    /// Runtime registration options.
    pub options: SlotRegistrationOptions<I>,
}

impl<P, I> SlotEntry<P, I> {
    /// Stable ledger identity.
    #[must_use]
    pub const fn id(&self) -> SlotEntryId {
        self.id
    }
}

impl<P, I> PartialEq for SlotEntry<P, I> {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl<P, I> Eq for SlotEntry<P, I> {}

impl<P, I> Hash for SlotEntry<P, I> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

/// JSON-safe live occupant returned by Slot inspection.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSlotOccupant {
    /// Plugin or package that registered the entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registrant: Option<String>,
    /// Keyed cell.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// List cell.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// List display order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<f64>,
    /// Shadowing or chain priority.
    pub priority: f64,
    /// Whether the renderer selects this entry.
    pub active: bool,
}

/// JSON-safe live Slot declaration tree.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSlotNode {
    /// Exact Slot key.
    pub name: SlotName,
    /// Cardinality.
    pub kind: SlotKind,
    /// Runtime data scope.
    pub scope: SlotScope,
    /// Diagnostic owner of the declaration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declared_by: Option<String>,
    /// Current registrations in ledger order.
    pub occupants: Vec<LiveSlotOccupant>,
    /// Slots declared by entries mounted here.
    pub children: Vec<LiveSlotNode>,
}

/// Injected microtask scheduler; callbacks must run after the current stack unwinds.
pub trait SlotMicrotaskScheduler {
    /// Queues one notification flush.
    fn queue(&self, callback: Box<dyn FnOnce()>);
}

type Listener = Rc<dyn Fn()>;
type MutationListener = Rc<dyn Fn(&SlotName)>;
type EntryErrorListener<P, I, X> = Rc<dyn Fn(&SlotName, Rc<SlotEntry<P, I>>, &X, bool)>;

struct SlotRecord<P, I> {
    spec: Option<SlotSpec<I>>,
    declared_by: Option<String>,
    parent: Option<SlotName>,
    declaration_epoch: u64,
    entries: Rc<Vec<Rc<SlotEntry<P, I>>>>,
    version: u64,
    listeners: BTreeMap<u64, Listener>,
    declaration_listeners: BTreeMap<u64, Listener>,
}

struct CoreState<P, I, X> {
    records: HashMap<SlotName, SlotRecord<P, I>>,
    record_order: Vec<SlotName>,
    handle_scopes: HashMap<StoreHandleId, (SlotScope, usize)>,
    dirty: BTreeSet<SlotName>,
    flush_scheduled: bool,
    abdicated: HashSet<SlotEntryId>,
    mutate_listeners: BTreeMap<u64, MutationListener>,
    error_listeners: BTreeMap<u64, EntryErrorListener<P, I, X>>,
    next_entry: u64,
    next_listener: u64,
}

/// Pure single-owner Slot registry.
pub struct SlotCore<P, I, X> {
    state: RefCell<CoreState<P, I, X>>,
    empty_entries: Rc<Vec<Rc<SlotEntry<P, I>>>>,
    scheduler: Rc<dyn SlotMicrotaskScheduler>,
}

impl<P, I, X> fmt::Debug for SlotCore<P, I, X> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state.borrow();
        formatter
            .debug_struct("SlotCore")
            .field("records", &state.records.len())
            .field("dirty", &state.dirty)
            .field("flush_scheduled", &state.flush_scheduled)
            .finish_non_exhaustive()
    }
}

/// Load-time registration failure.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct SlotCoreError(String);

impl<P, I, X> SlotCore<P, I, X>
where
    I: Clone,
    P: 'static,
    I: 'static,
    X: 'static,
{
    /// Creates a registry with the a-priori single/root Slot.
    #[must_use]
    pub fn new(scheduler: Rc<dyn SlotMicrotaskScheduler>) -> Rc<Self> {
        let empty_entries = Rc::new(Vec::new());
        let root_name = SlotName::new("root");
        let root = SlotRecord {
            spec: Some(SlotSpec::new(SlotKind::Single, SlotScope::Root)),
            declared_by: Some("(built-in)".to_owned()),
            parent: None,
            declaration_epoch: 1,
            entries: empty_entries.clone(),
            version: 0,
            listeners: BTreeMap::new(),
            declaration_listeners: BTreeMap::new(),
        };
        Rc::new(Self {
            state: RefCell::new(CoreState {
                records: [(root_name.clone(), root)].into_iter().collect(),
                record_order: vec![root_name],
                handle_scopes: HashMap::new(),
                dirty: BTreeSet::new(),
                flush_scheduled: false,
                abdicated: HashSet::new(),
                mutate_listeners: BTreeMap::new(),
                error_listeners: BTreeMap::new(),
                next_entry: 0,
                next_listener: 0,
            }),
            empty_entries,
            scheduler,
        })
    }

    /// Registers one entry and atomically declares its complete child table.
    ///
    /// # Errors
    ///
    /// Rejects undeclared targets, kind-shape violations, occupied cells at the same
    /// priority, duplicate child declarations, and shared Store scope conflicts.
    pub fn register(
        self: &Rc<Self>,
        options: SlotRegistrationOptions<I>,
        payload: P,
    ) -> Result<SlotRegistration<P, I, X>, SlotCoreError> {
        let target_scope = self.validate(&options)?;
        let entry = {
            let mut state = self.state.borrow_mut();
            let target = options.name.clone();
            let kind = state
                .records
                .get(&target)
                .and_then(|record| record.spec.as_ref())
                .map(|spec| spec.kind)
                .ok_or_else(|| not_declared(&target))?;
            state.next_entry = state.next_entry.wrapping_add(1);
            let entry = Rc::new(SlotEntry {
                id: SlotEntryId(state.next_entry),
                sequence: state.next_entry,
                payload,
                options,
            });
            let record = state
                .records
                .get_mut(&target)
                .ok_or_else(|| not_declared(&target))?;
            let mut entries = record.entries.as_ref().clone();
            entries.push(entry.clone());
            entries.sort_by(|left, right| entry_order(kind, left, right));
            record.entries = Rc::new(entries);
            if let Some(SlotStoreDeclaration::Shared(handle)) = entry.options.store {
                state
                    .handle_scopes
                    .entry(handle)
                    .and_modify(|(_, count)| *count += 1)
                    .or_insert((target_scope, 1));
            }
            entry
        };
        let target = entry.options.name.clone();
        self.mark_dirty(target.clone());

        let declarations = entry
            .options
            .children
            .iter()
            .map(|(name, spec)| (name.clone(), spec.clone()))
            .collect::<Vec<_>>();
        if !declarations.is_empty() {
            let declared_by = format!(
                "an entry in \"{}\"{}",
                target,
                entry
                    .options
                    .registrant
                    .as_ref()
                    .map_or(String::new(), |name| format!(" ({name})"))
            );
            for (child, spec) in &declarations {
                let mut state = self.state.borrow_mut();
                let record = record_mut(&mut state, child, &self.empty_entries);
                record.spec = Some(spec.clone());
                record.declared_by = Some(declared_by.clone());
                record.parent = Some(target.clone());
                record.declaration_epoch = record.declaration_epoch.wrapping_add(1);
            }
            for (child, _) in &declarations {
                self.mark_dirty(child.clone());
            }
            for (child, _) in &declarations {
                self.notify_declaration(child);
            }
        }
        Ok(SlotRegistration {
            core: Rc::downgrade(self),
            slot: target,
            entry: entry.id,
        })
    }

    fn validate(&self, options: &SlotRegistrationOptions<I>) -> Result<SlotScope, SlotCoreError> {
        let state = self.state.borrow();
        let Some(record) = state.records.get(&options.name) else {
            return Err(not_declared(&options.name));
        };
        let Some(spec) = &record.spec else {
            return Err(not_declared(&options.name));
        };
        let priority = options.priority.unwrap_or(0.0);
        let occupant = match spec.kind {
            SlotKind::Single => record
                .entries
                .iter()
                .find(|entry| same_number(entry.options.priority.unwrap_or(0.0), priority)),
            SlotKind::Keyed => {
                let Some(key) = &options.key else {
                    return Err(SlotCoreError(format!(
                        "keyed slot \"{}\" requires options.key",
                        options.name
                    )));
                };
                record.entries.iter().find(|entry| {
                    entry.options.key.as_ref() == Some(key)
                        && same_number(entry.options.priority.unwrap_or(0.0), priority)
                })
            }
            SlotKind::List => {
                let Some(id) = &options.id else {
                    return Err(SlotCoreError(format!(
                        "list slot \"{}\" requires options.id",
                        options.name
                    )));
                };
                record.entries.iter().find(|entry| {
                    entry.options.id.as_ref() == Some(id)
                        && same_number(entry.options.priority.unwrap_or(0.0), priority)
                })
            }
            SlotKind::Chain => {
                if !options.has_selector {
                    return Err(SlotCoreError(format!(
                        "chain slot \"{}\" requires options.select",
                        options.name
                    )));
                }
                None
            }
        };
        if let Some(occupant) = occupant {
            return Err(occupied_error(spec.kind, options, occupant, priority));
        }
        for child in options.children.keys() {
            if let Some(first) = state
                .records
                .get(child)
                .filter(|record| record.spec.is_some())
            {
                return Err(SlotCoreError(format!(
                    "slot \"{child}\" is already declared (by {})",
                    first.declared_by.as_deref().unwrap_or("an unknown entry")
                )));
            }
        }
        if let Some(SlotStoreDeclaration::Shared(handle)) = options.store
            && let Some((scope, _)) = state.handle_scopes.get(&handle)
            && *scope != spec.scope
        {
            return Err(SlotCoreError(format!(
                "store handle mounted under \"{}\" (scope \"{}\") is already mounted under scope \"{}\" — one handle, one scope",
                options.name,
                scope_name(spec.scope),
                scope_name(*scope)
            )));
        }
        Ok(spec.scope)
    }

    /// Stable raw entry snapshot, shared across reads until mutation.
    #[must_use]
    pub fn entries(&self, key: &SlotName) -> Rc<Vec<Rc<SlotEntry<P, I>>>> {
        self.state.borrow().records.get(key).map_or_else(
            || self.empty_entries.clone(),
            |record| record.entries.clone(),
        )
    }

    /// Active winner per shadowing cell; chain Slots return their complete ledger.
    #[must_use]
    pub fn entries_of_slot(&self, key: &SlotName) -> Vec<Rc<SlotEntry<P, I>>> {
        let state = self.state.borrow();
        let Some(record) = state.records.get(key) else {
            return Vec::new();
        };
        let Some(spec) = &record.spec else {
            return Vec::new();
        };
        if spec.kind == SlotKind::Chain {
            return record.entries.as_ref().clone();
        }
        let mut seen = HashSet::<Option<String>>::new();
        record
            .entries
            .iter()
            .filter(|entry| !state.abdicated.contains(&entry.id))
            .filter(|entry| {
                let cell = match spec.kind {
                    SlotKind::Keyed => entry.options.key.clone(),
                    SlotKind::List => entry.options.id.clone(),
                    SlotKind::Single => None,
                    SlotKind::Chain => unreachable!("chain returned above"),
                };
                seen.insert(cell)
            })
            .cloned()
            .collect()
    }

    /// Whether an entry identity remains in any ledger record.
    #[must_use]
    pub fn is_live(&self, entry: &SlotEntry<P, I>) -> bool {
        self.state.borrow().records.values().any(|record| {
            record
                .entries
                .iter()
                .any(|candidate| candidate.id == entry.id)
        })
    }

    /// Resolves one stable entry identity from the current ledger.
    #[must_use]
    pub fn entry_by_id(&self, id: SlotEntryId) -> Option<Rc<SlotEntry<P, I>>> {
        self.state
            .borrow()
            .records
            .values()
            .flat_map(|record| record.entries.iter())
            .find(|entry| entry.id == id)
            .cloned()
    }

    /// Current runtime declaration.
    #[must_use]
    pub fn spec(&self, key: &SlotName) -> Option<SlotSpec<I>> {
        self.state
            .borrow()
            .records
            .get(key)
            .and_then(|record| record.spec.clone())
    }

    /// Monotonic declaration lifetime; ordinary entry mutations do not change it.
    #[must_use]
    pub fn declaration_epoch(&self, key: &SlotName) -> u64 {
        self.state
            .borrow()
            .records
            .get(key)
            .map_or(0, |record| record.declaration_epoch)
    }

    /// Monotonic per-key mutation version.
    #[must_use]
    pub fn version(&self, key: &SlotName) -> u64 {
        self.state
            .borrow()
            .records
            .get(key)
            .map_or(0, |record| record.version)
    }

    /// Subscribes to microtask-batched mutations of one key.
    #[must_use]
    pub fn subscribe(
        self: &Rc<Self>,
        key: SlotName,
        listener: Listener,
    ) -> SlotSubscription<P, I, X> {
        let id = self.next_listener();
        let mut state = self.state.borrow_mut();
        record_mut(&mut state, &key, &self.empty_entries)
            .listeners
            .insert(id, listener);
        SlotSubscription {
            core: Rc::downgrade(self),
            key: Some(key),
            id,
            kind: SubscriptionKind::Entries,
        }
    }

    /// Subscribes synchronously to declaration and collapse boundaries.
    #[must_use]
    pub fn subscribe_declaration(
        self: &Rc<Self>,
        key: SlotName,
        listener: Listener,
    ) -> SlotSubscription<P, I, X> {
        let id = self.next_listener();
        let mut state = self.state.borrow_mut();
        record_mut(&mut state, &key, &self.empty_entries)
            .declaration_listeners
            .insert(id, listener);
        SlotSubscription {
            core: Rc::downgrade(self),
            key: Some(key),
            id,
            kind: SubscriptionKind::Declaration,
        }
    }

    /// Observes every mutation synchronously before batched delivery.
    #[must_use]
    pub fn on_mutate(self: &Rc<Self>, listener: MutationListener) -> SlotSubscription<P, I, X> {
        let id = self.next_listener();
        self.state
            .borrow_mut()
            .mutate_listeners
            .insert(id, listener);
        SlotSubscription {
            core: Rc::downgrade(self),
            key: None,
            id,
            kind: SubscriptionKind::Mutation,
        }
    }

    /// Observes contained entry failures synchronously.
    #[must_use]
    pub fn on_entry_error(
        self: &Rc<Self>,
        listener: EntryErrorListener<P, I, X>,
    ) -> SlotSubscription<P, I, X> {
        let id = self.next_listener();
        self.state.borrow_mut().error_listeners.insert(id, listener);
        SlotSubscription {
            core: Rc::downgrade(self),
            key: None,
            id,
            kind: SubscriptionKind::EntryError,
        }
    }

    /// Reports one boundary crash and optionally retires the entry from shadowing projection.
    pub fn report_entry_error(
        self: &Rc<Self>,
        key: &SlotName,
        entry: &Rc<SlotEntry<P, I>>,
        error: &X,
        abdicate: bool,
    ) {
        if abdicate {
            let first = self.state.borrow_mut().abdicated.insert(entry.id);
            if !first {
                return;
            }
            if self.state.borrow().records.contains_key(key) {
                self.mark_dirty(key.clone());
            }
        }
        let listeners = self
            .state
            .borrow()
            .error_listeners
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for listener in listeners {
            listener(key, entry.clone(), error, abdicate);
        }
    }

    /// Exports the current live declaration topology without executable values.
    #[must_use]
    pub fn snapshot(&self, root: Option<&SlotName>) -> Vec<LiveSlotNode> {
        let state = self.state.borrow();
        if let Some(root) = root {
            return build_snapshot(&state, root, &mut HashSet::new())
                .into_iter()
                .collect();
        }
        state
            .record_order
            .iter()
            .filter(|name| {
                state.records.get(*name).is_some_and(|record| {
                    record.spec.is_some()
                        && record.parent.as_ref().is_none_or(|parent| {
                            state
                                .records
                                .get(parent)
                                .is_none_or(|parent| parent.spec.is_none())
                        })
                })
            })
            .filter_map(|name| build_snapshot(&state, name, &mut HashSet::new()))
            .collect()
    }

    fn dispose_entry(self: &Rc<Self>, slot: &SlotName, entry_id: SlotEntryId) {
        let removed = {
            let mut state = self.state.borrow_mut();
            let Some(record) = state.records.get_mut(slot) else {
                return;
            };
            let Some(entry) = record
                .entries
                .iter()
                .find(|entry| entry.id == entry_id)
                .cloned()
            else {
                return;
            };
            record.entries = Rc::new(
                record
                    .entries
                    .iter()
                    .filter(|candidate| candidate.id != entry_id)
                    .cloned()
                    .collect(),
            );
            entry
        };
        self.mark_dirty(slot.clone());
        self.release_entry(&removed);
    }

    fn release_entry(self: &Rc<Self>, entry: &Rc<SlotEntry<P, I>>) {
        if let Some(SlotStoreDeclaration::Shared(handle)) = entry.options.store {
            let mut state = self.state.borrow_mut();
            if let Some((_, count)) = state.handle_scopes.get_mut(&handle) {
                *count -= 1;
                if *count == 0 {
                    state.handle_scopes.remove(&handle);
                }
            }
        }
        for child in entry.options.children.keys() {
            let doomed = {
                let mut state = self.state.borrow_mut();
                let Some(record) = state.records.get_mut(child) else {
                    continue;
                };
                let doomed = record.entries.clone();
                record.spec = None;
                record.declared_by = None;
                record.parent = None;
                record.declaration_epoch = record.declaration_epoch.wrapping_add(1);
                record.entries = self.empty_entries.clone();
                doomed
            };
            self.mark_dirty(child.clone());
            self.notify_declaration(child);
            for nested in doomed.iter() {
                self.release_entry(nested);
            }
        }
    }

    fn mark_dirty(self: &Rc<Self>, key: SlotName) {
        let mutate = {
            let mut state = self.state.borrow_mut();
            let record = record_mut(&mut state, &key, &self.empty_entries);
            record.version = record.version.wrapping_add(1);
            state.mutate_listeners.values().cloned().collect::<Vec<_>>()
        };
        for listener in mutate {
            listener(&key);
        }
        let schedule = {
            let mut state = self.state.borrow_mut();
            state.dirty.insert(key);
            if state.flush_scheduled {
                false
            } else {
                state.flush_scheduled = true;
                true
            }
        };
        if schedule {
            let weak = Rc::downgrade(self);
            self.scheduler.queue(Box::new(move || {
                if let Some(core) = weak.upgrade() {
                    core.flush();
                }
            }));
        }
    }

    fn flush(self: &Rc<Self>) {
        let listeners = {
            let mut state = self.state.borrow_mut();
            state.flush_scheduled = false;
            let dirty = std::mem::take(&mut state.dirty);
            dirty
                .into_iter()
                .flat_map(|key| {
                    state
                        .records
                        .get(&key)
                        .into_iter()
                        .flat_map(|record| record.listeners.values().cloned())
                })
                .collect::<Vec<_>>()
        };
        for listener in listeners {
            listener();
        }
    }

    fn notify_declaration(&self, key: &SlotName) {
        let listeners = self
            .state
            .borrow()
            .records
            .get(key)
            .into_iter()
            .flat_map(|record| record.declaration_listeners.values().cloned())
            .collect::<Vec<_>>();
        for listener in listeners {
            listener();
        }
    }

    fn next_listener(&self) -> u64 {
        let mut state = self.state.borrow_mut();
        state.next_listener = state.next_listener.wrapping_add(1);
        state.next_listener
    }
}

/// Idempotent registration disposer.
pub struct SlotRegistration<P, I, X>
where
    I: Clone + 'static,
    P: 'static,
    X: 'static,
{
    core: Weak<SlotCore<P, I, X>>,
    slot: SlotName,
    entry: SlotEntryId,
}

impl<P, I, X> fmt::Debug for SlotRegistration<P, I, X>
where
    I: Clone + 'static,
    P: 'static,
    X: 'static,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SlotRegistration")
            .field("slot", &self.slot)
            .field("entry", &self.entry)
            .finish_non_exhaustive()
    }
}

impl<P, I, X> SlotRegistration<P, I, X>
where
    I: Clone + 'static,
    P: 'static,
    X: 'static,
{
    /// Stable identity of the registered ledger entry.
    #[must_use]
    pub const fn entry_id(&self) -> SlotEntryId {
        self.entry
    }

    /// Removes the registration and recursively collapses its declarations.
    pub fn dispose(&self) {
        if let Some(core) = self.core.upgrade() {
            core.dispose_entry(&self.slot, self.entry);
        }
    }
}

#[derive(Clone, Copy)]
enum SubscriptionKind {
    Entries,
    Declaration,
    Mutation,
    EntryError,
}

/// Idempotent listener disposer.
pub struct SlotSubscription<P, I, X> {
    core: Weak<SlotCore<P, I, X>>,
    key: Option<SlotName>,
    id: u64,
    kind: SubscriptionKind,
}

impl<P, I, X> SlotSubscription<P, I, X> {
    /// Stops future delivery.
    pub fn dispose(&self) {
        let Some(core) = self.core.upgrade() else {
            return;
        };
        let mut state = core.state.borrow_mut();
        match self.kind {
            SubscriptionKind::Entries => {
                if let Some(record) = self.key.as_ref().and_then(|key| state.records.get_mut(key)) {
                    record.listeners.remove(&self.id);
                }
            }
            SubscriptionKind::Declaration => {
                if let Some(record) = self.key.as_ref().and_then(|key| state.records.get_mut(key)) {
                    record.declaration_listeners.remove(&self.id);
                }
            }
            SubscriptionKind::Mutation => {
                state.mutate_listeners.remove(&self.id);
            }
            SubscriptionKind::EntryError => {
                state.error_listeners.remove(&self.id);
            }
        }
    }
}

fn record_mut<'a, P, I, X>(
    state: &'a mut CoreState<P, I, X>,
    key: &SlotName,
    empty: &Rc<Vec<Rc<SlotEntry<P, I>>>>,
) -> &'a mut SlotRecord<P, I> {
    if !state.records.contains_key(key) {
        state.record_order.push(key.clone());
        state.records.insert(
            key.clone(),
            SlotRecord {
                spec: None,
                declared_by: None,
                parent: None,
                declaration_epoch: 0,
                entries: empty.clone(),
                version: 0,
                listeners: BTreeMap::new(),
                declaration_listeners: BTreeMap::new(),
            },
        );
    }
    state.records.get_mut(key).expect("record inserted")
}

fn not_declared(name: &SlotName) -> SlotCoreError {
    SlotCoreError(format!(
        "slot \"{name}\" is not declared (a parent entry's children table must declare it)"
    ))
}

fn occupied_error<P, I>(
    kind: SlotKind,
    options: &SlotRegistrationOptions<I>,
    occupant: &SlotEntry<P, I>,
    priority: f64,
) -> SlotCoreError {
    let hint = format!(
        "at priority {priority}{} — register at a different priority to shadow it (lowest renders)",
        occupant
            .options
            .registrant
            .as_ref()
            .map_or(String::new(), |registrant| format!(
                " (registered by {registrant})"
            ))
    );
    let message = match kind {
        SlotKind::Single => format!(
            "single slot \"{}\" already has a registration {hint}",
            options.name
        ),
        SlotKind::Keyed => format!(
            "keyed slot \"{}\" already has an entry for key \"{}\" {hint}",
            options.name,
            options.key.as_deref().unwrap_or_default()
        ),
        SlotKind::List => format!(
            "list slot \"{}\" already has an entry with id \"{}\" {hint}",
            options.name,
            options.id.as_deref().unwrap_or_default()
        ),
        SlotKind::Chain => unreachable!("chains have no occupied-cell validation"),
    };
    SlotCoreError(message)
}

fn entry_order<P, I>(
    kind: SlotKind,
    left: &Rc<SlotEntry<P, I>>,
    right: &Rc<SlotEntry<P, I>>,
) -> Ordering {
    let priority = compare_number(
        left.options.priority.unwrap_or(0.0),
        right.options.priority.unwrap_or(0.0),
    );
    if priority != Ordering::Equal {
        return priority;
    }
    if kind == SlotKind::List {
        let order = compare_number(
            left.options.order.unwrap_or(0.0),
            right.options.order.unwrap_or(0.0),
        );
        if order != Ordering::Equal {
            return order;
        }
    }
    left.sequence.cmp(&right.sequence)
}

fn compare_number(left: f64, right: f64) -> Ordering {
    left.partial_cmp(&right).unwrap_or(Ordering::Equal)
}

fn same_number(left: f64, right: f64) -> bool {
    left.partial_cmp(&right) == Some(Ordering::Equal)
}

fn scope_name(scope: SlotScope) -> &'static str {
    match scope {
        SlotScope::Root => "root",
        SlotScope::SessionMaybe => "session-maybe",
        SlotScope::Session => "session",
    }
}

fn build_snapshot<P, I, X>(
    state: &CoreState<P, I, X>,
    name: &SlotName,
    seen: &mut HashSet<SlotName>,
) -> Option<LiveSlotNode> {
    let record = state.records.get(name)?;
    let spec = record.spec.as_ref()?;
    if !seen.insert(name.clone()) {
        return None;
    }
    let active = active_ids(state, record, spec.kind);
    let children = state
        .record_order
        .iter()
        .filter(|child| {
            state.records.get(*child).is_some_and(|candidate| {
                candidate.spec.is_some() && candidate.parent.as_ref() == Some(name)
            })
        })
        .filter_map(|child| build_snapshot(state, child, &mut seen.clone()))
        .collect();
    Some(LiveSlotNode {
        name: name.clone(),
        kind: spec.kind,
        scope: spec.scope,
        declared_by: record.declared_by.clone(),
        occupants: record
            .entries
            .iter()
            .map(|entry| LiveSlotOccupant {
                registrant: entry.options.registrant.clone(),
                key: entry.options.key.clone(),
                id: entry.options.id.clone(),
                order: entry.options.order,
                priority: entry.options.priority.unwrap_or(0.0),
                active: active.contains(&entry.id),
            })
            .collect(),
        children,
    })
}

fn active_ids<P, I, X>(
    state: &CoreState<P, I, X>,
    record: &SlotRecord<P, I>,
    kind: SlotKind,
) -> HashSet<SlotEntryId> {
    if kind == SlotKind::Chain {
        return record.entries.iter().map(|entry| entry.id).collect();
    }
    let mut seen = HashSet::<Option<String>>::new();
    record
        .entries
        .iter()
        .filter(|entry| !state.abdicated.contains(&entry.id))
        .filter(|entry| {
            let cell = match kind {
                SlotKind::Keyed => entry.options.key.clone(),
                SlotKind::List => entry.options.id.clone(),
                SlotKind::Single => None,
                SlotKind::Chain => unreachable!(),
            };
            seen.insert(cell)
        })
        .map(|entry| entry.id)
        .collect()
}
