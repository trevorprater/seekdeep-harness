//! Eager, checkpointable projections over committed session event logs.

/// Display-safe durable failure projection.
pub mod failure_display;

pub use failure_display::display_failure_message;

use std::{collections::HashMap, sync::Arc};

use indexmap::IndexMap;
use parking_lot::Mutex;
use seekdeep_cordis::{Context, EventOptions, EventReply, ServiceKey, fiber::EffectHandle};
use seekdeep_core::session::{Session, SessionEvent};
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Typed context service slot for the session-projection registry.
pub const SESSION_PROJECTIONS: ServiceKey<SessionProjectionRegistry> =
    ServiceKey::new("sessionProjections");

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Result of applying one event to one projection state.
///
/// This makes JavaScript's load-bearing `Object.is(next, previous)` gate
/// explicit: returning [`Self::Unchanged`] advances the watermark without
/// producing change-feed work.
#[derive(Clone, Debug, PartialEq)]
pub enum ProjectionTransition {
    /// The exact prior state remains authoritative.
    Unchanged,
    /// A distinct next plain-JSON state replaces it.
    Changed(Value),
}

impl ProjectionTransition {
    /// Constructs a changed transition from a serializable state value.
    ///
    /// # Errors
    ///
    /// Returns when the state cannot be represented as lossless JSON.
    pub fn changed<T: Serialize>(state: T) -> anyhow::Result<Self> {
        Ok(Self::Changed(serde_json::to_value(state)?))
    }
}

type Init = Arc<dyn Fn() -> anyhow::Result<Value> + Send + Sync>;
type Apply =
    Arc<dyn Fn(&Value, &SessionEvent) -> anyhow::Result<ProjectionTransition> + Send + Sync>;
type View = Arc<dyn Fn(&Value) -> anyhow::Result<Value> + Send + Sync>;

/// One domain's synchronous pure projection fold.
#[derive(Clone)]
pub struct ProjectionDefinition {
    /// Projection key owned by this definition.
    pub key: String,
    /// Persisted-state invalidation version.
    pub state_version: u64,
    init: Init,
    apply: Apply,
    view: View,
}

impl std::fmt::Debug for ProjectionDefinition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProjectionDefinition")
            .field("key", &self.key)
            .field("state_version", &self.state_version)
            .finish_non_exhaustive()
    }
}

impl ProjectionDefinition {
    /// Defines a type-erased plain-JSON fold and validated wire view.
    #[must_use]
    pub fn new<I, A, V>(
        key: impl Into<String>,
        state_version: u64,
        init: I,
        apply: A,
        view: V,
    ) -> Self
    where
        I: Fn() -> anyhow::Result<Value> + Send + Sync + 'static,
        A: Fn(&Value, &SessionEvent) -> anyhow::Result<ProjectionTransition>
            + Send
            + Sync
            + 'static,
        V: Fn(&Value) -> anyhow::Result<Value> + Send + Sync + 'static,
    {
        Self {
            key: key.into(),
            state_version,
            init: Arc::new(init),
            apply: Arc::new(apply),
            view: Arc::new(view),
        }
    }

    /// Produces the plain-JSON state for an empty log.
    ///
    /// # Errors
    ///
    /// Returns the definition's initialization failure.
    pub fn initial_state(&self) -> anyhow::Result<Value> {
        (self.init)()
    }

    /// Applies one committed event to an internal state.
    ///
    /// # Errors
    ///
    /// Returns the definition's transition failure.
    pub fn apply_event(
        &self,
        state: &Value,
        event: &SessionEvent,
    ) -> anyhow::Result<ProjectionTransition> {
        (self.apply)(state, event)
    }

    /// Produces and validates the wire-facing whole value.
    ///
    /// # Errors
    ///
    /// Returns the definition's view or schema failure.
    pub fn project(&self, state: &Value) -> anyhow::Result<Value> {
        (self.view)(state)
    }
}

/// One consistent read cut over every live projection unit.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectionSnapshot {
    /// Last event sequence reflected by every value, or `-1` for an empty log.
    pub as_of_seq: i64,
    /// Whole schema-validated wire value by registered key.
    pub values: IndexMap<String, Value>,
}

/// One persisted projection-state shortcut.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionCheckpointRow {
    /// Definition state version that produced the state.
    pub ver: u64,
    /// Last event sequence folded into the state, or `-1` for an empty log.
    pub seq: i64,
    /// Detached plain-JSON internal state.
    pub val: Value,
}

/// Projection checkpoint rows keyed by projection key.
pub type ProjectionCheckpoint = IndexMap<String, ProjectionCheckpointRow>;

/// A cold restore's served values and refreshed durable shortcut.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectionRestore {
    /// Values at the supplied log cut.
    pub snapshot: ProjectionSnapshot,
    /// Rows refreshed to that same cut.
    pub checkpoint: ProjectionCheckpoint,
}

/// Change-feed callback for one changed unit.
pub type ProjectionChangeListener =
    Arc<dyn Fn(Arc<Session>, &str, &Value, u64) -> anyhow::Result<()> + Send + Sync + 'static>;

#[derive(Clone, Debug)]
struct UnitCell {
    state: Value,
    observed_seq: i64,
}

#[derive(Debug)]
struct SessionCell {
    session: std::sync::Weak<Session>,
    cell: UnitCell,
}

#[derive(Debug)]
struct Registration {
    definition: ProjectionDefinition,
    cells: HashMap<usize, SessionCell>,
    refs: u64,
}

#[derive(Default)]
struct RegistryState {
    registrations: IndexMap<String, Registration>,
    listeners: IndexMap<Uuid, ProjectionChangeListener>,
}

/// Registry that eagerly drives every projection over committed events.
pub struct SessionProjectionRegistry {
    state: Arc<Mutex<RegistryState>>,
}

impl std::fmt::Debug for SessionProjectionRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionProjectionRegistry")
            .field(
                "keys",
                &self.state.lock().registrations.keys().collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

impl SessionProjectionRegistry {
    /// Installs the service and its single `session/event` drive subscription.
    ///
    /// # Errors
    ///
    /// Returns when the service slot is occupied or the context is inactive.
    pub fn install(context: &Context) -> anyhow::Result<Arc<Self>> {
        let registry = Arc::new(Self {
            state: Arc::new(Mutex::new(RegistryState::default())),
        });
        context.provide(SESSION_PROJECTIONS, registry.clone())?;
        let weak = Arc::downgrade(&registry);
        context.events().on_sync(
            context,
            "session/event",
            move |_, args| {
                let Some(registry) = weak.upgrade() else {
                    return Ok(EventReply::Undefined);
                };
                let session = args
                    .get::<Session>(0)
                    .ok_or_else(|| anyhow::anyhow!("session/event lacks a session"))?;
                let event = args
                    .get::<SessionEvent>(1)
                    .ok_or_else(|| anyhow::anyhow!("session/event lacks an event"))?;
                registry.drive(&session, &event)?;
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )?;
        Ok(registry)
    }

    /// Registers a projection on the calling context's lifecycle fiber.
    ///
    /// Equal-key/equal-version registrants share one definition and cache;
    /// the last disposer removes both. A version mismatch fails loudly.
    ///
    /// # Errors
    ///
    /// Returns for an invalid state version, an incompatible existing unit,
    /// or inactive ownership.
    pub fn register(
        &self,
        context: &Context,
        definition: ProjectionDefinition,
    ) -> anyhow::Result<EffectHandle> {
        anyhow::ensure!(
            definition.state_version <= MAX_SAFE_INTEGER,
            "session projection {:?} stateVersion must be a non-negative integer, got {}",
            definition.key,
            definition.state_version
        );
        let key = definition.key.clone();
        {
            let mut state = self.state.lock();
            if let Some(existing) = state.registrations.get_mut(&key) {
                anyhow::ensure!(
                    existing.definition.state_version == definition.state_version,
                    "session projection key {:?} is already registered at stateVersion {}; refusing to share it with stateVersion {}",
                    key,
                    existing.definition.state_version,
                    definition.state_version
                );
                existing.refs += 1;
            } else {
                state.registrations.insert(
                    key.clone(),
                    Registration {
                        definition,
                        cells: HashMap::new(),
                        refs: 1,
                    },
                );
            }
        }

        let state = self.state.clone();
        let disposal_key = key.clone();
        let effect = EffectHandle::synchronous("sessionProjections.register()", move || {
            unregister(&state, &disposal_key);
            Ok(())
        });
        match context.own(effect.clone()) {
            Ok(effect) => Ok(effect),
            Err(error) => {
                unregister(&self.state, &key);
                Err(error.into())
            }
        }
    }

    /// Subscribes to changed projection values on the calling fiber.
    ///
    /// # Errors
    ///
    /// Returns when the context is inactive.
    pub fn on_changed(
        &self,
        context: &Context,
        listener: ProjectionChangeListener,
    ) -> anyhow::Result<EffectHandle> {
        let id = Uuid::now_v7();
        self.state.lock().listeners.insert(id, listener);
        let state = self.state.clone();
        let effect = EffectHandle::synchronous("sessionProjections.onChanged()", move || {
            state.lock().listeners.shift_remove(&id);
            Ok(())
        });
        match context.own(effect.clone()) {
            Ok(effect) => Ok(effect),
            Err(error) => {
                self.state.lock().listeners.shift_remove(&id);
                Err(error.into())
            }
        }
    }

    /// Reads one consistent projection cut for a live session.
    ///
    /// Missing cells are built lazily from the complete in-memory log.
    ///
    /// # Errors
    ///
    /// Returns an init, fold, or view-schema failure.
    pub fn snapshot(&self, session: &Arc<Session>) -> anyhow::Result<ProjectionSnapshot> {
        let events = session.events();
        let mut values = IndexMap::new();
        let mut state = self.state.lock();
        for registration in state.registrations.values_mut() {
            let cell = cell_for(registration, session, &events)?;
            values.insert(
                registration.definition.key.clone(),
                (registration.definition.view)(&cell.state)?,
            );
        }
        Ok(ProjectionSnapshot {
            as_of_seq: sequence_before(session.seq()),
            values,
        })
    }

    /// Captures detached internal states at the live session watermark.
    ///
    /// # Errors
    ///
    /// Returns an init or fold failure while lazily building a cell.
    pub fn checkpoint(&self, session: &Arc<Session>) -> anyhow::Result<ProjectionCheckpoint> {
        let events = session.events();
        let mut rows = IndexMap::new();
        let mut state = self.state.lock();
        for registration in state.registrations.values_mut() {
            let cell = cell_for(registration, session, &events)?;
            rows.insert(
                registration.definition.key.clone(),
                ProjectionCheckpointRow {
                    ver: registration.definition.state_version,
                    seq: cell.observed_seq,
                    val: cell.state.clone(),
                },
            );
        }
        Ok(rows)
    }

    /// Computes the one-below read anchor for a cold restore.
    #[must_use]
    pub fn restore_floor(&self, checkpoint: &ProjectionCheckpoint) -> Option<u64> {
        let state = self.state.lock();
        let mut floor: Option<i64> = None;
        for registration in state.registrations.values() {
            let row = checkpoint.get(&registration.definition.key);
            let need = row.map_or(0, |row| {
                if row.ver == registration.definition.state_version {
                    row.seq.saturating_add(1).max(0)
                } else {
                    0
                }
            });
            floor = Some(floor.map_or(need, |current| current.min(need)));
        }
        floor.map(|value| u64::try_from(value.saturating_sub(1).max(0)).unwrap_or(0))
    }

    /// Serves version-compatible checkpoint views without reading a log.
    ///
    /// # Errors
    ///
    /// Returns a unit view-schema failure.
    pub fn view_checkpoint(
        &self,
        checkpoint: &ProjectionCheckpoint,
    ) -> anyhow::Result<IndexMap<String, Value>> {
        let state = self.state.lock();
        let mut values = IndexMap::new();
        for registration in state.registrations.values() {
            let definition = &registration.definition;
            let Some(row) = checkpoint.get(&definition.key) else {
                continue;
            };
            if row.ver == definition.state_version {
                values.insert(definition.key.clone(), (definition.view)(&row.val)?);
            }
        }
        Ok(values)
    }

    /// Restores every unit from compatible rows plus a stored event suffix.
    ///
    /// A discarded row is only safe when `base_seq == 0`; otherwise the
    /// caller must re-read the full log. Returned rows and values share one
    /// exact supplied-log cut.
    ///
    /// # Errors
    ///
    /// Returns when a suffix cannot soundly restore a unit or a fold/view
    /// function rejects the supplied data.
    pub fn restore(
        &self,
        checkpoint: &ProjectionCheckpoint,
        events: &[SessionEvent],
        base_seq: u64,
    ) -> anyhow::Result<ProjectionRestore> {
        let base_seq = i64::try_from(base_seq)
            .map_err(|_| anyhow::anyhow!("projection base seq exceeds the supported range"))?;
        let end_seq = events.last().map_or(base_seq - 1, |event| {
            i64::try_from(event.seq).unwrap_or(i64::MAX)
        });
        let state = self.state.lock();
        let mut values = IndexMap::new();
        let mut refreshed = IndexMap::new();
        for registration in state.registrations.values() {
            let definition = &registration.definition;
            let row = checkpoint.get(&definition.key);
            let usable_row = row.filter(|row| {
                row.ver == definition.state_version && row.seq >= base_seq - 1 && row.seq <= end_seq
            });
            anyhow::ensure!(
                usable_row.is_some() || base_seq == 0,
                "session projection {:?} cannot restore from seq {base_seq}: its checkpoint row is missing, version-mismatched, or beyond the supplied log end; re-read from seq 0",
                definition.key
            );
            let mut projection_state = if let Some(row) = usable_row {
                row.val.clone()
            } else {
                (definition.init)()?
            };
            let from = if let Some(row) = usable_row {
                row.seq
            } else {
                base_seq - 1
            };
            for event in events {
                if i64::try_from(event.seq).unwrap_or(i64::MAX) > from
                    && let ProjectionTransition::Changed(next) =
                        (definition.apply)(&projection_state, event)?
                {
                    projection_state = next;
                }
            }
            values.insert(
                definition.key.clone(),
                (definition.view)(&projection_state)?,
            );
            refreshed.insert(
                definition.key.clone(),
                ProjectionCheckpointRow {
                    ver: definition.state_version,
                    seq: end_seq,
                    val: projection_state,
                },
            );
        }
        Ok(ProjectionRestore {
            snapshot: ProjectionSnapshot {
                as_of_seq: end_seq,
                values,
            },
            checkpoint: refreshed,
        })
    }

    fn drive(&self, session: &Arc<Session>, event: &SessionEvent) -> anyhow::Result<()> {
        let keys = self
            .state
            .lock()
            .registrations
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for key in keys {
            let notification = {
                let mut state = self.state.lock();
                let has_listeners = !state.listeners.is_empty();
                let Some(registration) = state.registrations.get_mut(&key) else {
                    continue;
                };
                let identity = session_identity(session);
                purge_dead_cells(&mut registration.cells);
                if !registration.cells.contains_key(&identity) {
                    let events = session.events();
                    let prefix_len = usize::try_from(event.seq)
                        .unwrap_or(usize::MAX)
                        .min(events.len());
                    let cell = build_cell(&registration.definition, &events[..prefix_len])?;
                    registration.cells.insert(
                        identity,
                        SessionCell {
                            session: Arc::downgrade(session),
                            cell,
                        },
                    );
                }
                let session_cell = registration
                    .cells
                    .get_mut(&identity)
                    .expect("cell was inserted");
                let next = (registration.definition.apply)(&session_cell.cell.state, event)?;
                let changed = match next {
                    ProjectionTransition::Unchanged => false,
                    ProjectionTransition::Changed(next) => {
                        session_cell.cell.state = next;
                        true
                    }
                };
                session_cell.cell.observed_seq = i64::try_from(event.seq).unwrap_or(i64::MAX);
                if changed && has_listeners {
                    Some((
                        (registration.definition.view)(&session_cell.cell.state)?,
                        state.listeners.values().cloned().collect::<Vec<_>>(),
                    ))
                } else {
                    None
                }
            };
            if let Some((value, listeners)) = notification {
                for listener in listeners {
                    listener(session.clone(), &key, &value, event.seq)?;
                }
            }
        }
        Ok(())
    }
}

fn unregister(state: &Mutex<RegistryState>, key: &str) {
    let mut state = state.lock();
    let Some(registration) = state.registrations.get_mut(key) else {
        return;
    };
    registration.refs = registration.refs.saturating_sub(1);
    if registration.refs == 0 {
        state.registrations.shift_remove(key);
    }
}

fn session_identity(session: &Arc<Session>) -> usize {
    Arc::as_ptr(session) as usize
}

fn purge_dead_cells(cells: &mut HashMap<usize, SessionCell>) {
    cells.retain(|_, cell| cell.session.strong_count() > 0);
}

fn build_cell(
    definition: &ProjectionDefinition,
    events: &[SessionEvent],
) -> anyhow::Result<UnitCell> {
    let mut state = (definition.init)()?;
    for event in events {
        if let ProjectionTransition::Changed(next) = (definition.apply)(&state, event)? {
            state = next;
        }
    }
    Ok(UnitCell {
        state,
        observed_seq: events
            .last()
            .map_or(-1, |event| i64::try_from(event.seq).unwrap_or(i64::MAX)),
    })
}

fn cell_for(
    registration: &mut Registration,
    session: &Arc<Session>,
    events: &[SessionEvent],
) -> anyhow::Result<UnitCell> {
    let identity = session_identity(session);
    purge_dead_cells(&mut registration.cells);
    if !registration.cells.contains_key(&identity) {
        let cell = build_cell(&registration.definition, events)?;
        registration.cells.insert(
            identity,
            SessionCell {
                session: Arc::downgrade(session),
                cell,
            },
        );
    }
    Ok(registration
        .cells
        .get(&identity)
        .expect("cell was inserted")
        .cell
        .clone())
}

fn sequence_before(next_seq: u64) -> i64 {
    i64::try_from(next_seq)
        .unwrap_or(i64::MAX)
        .saturating_sub(1)
}

/// Registers the registry package's explained empty invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register("seekdeep-session-projection", InvariantInstaller::noop())
}

#[cfg(test)]
mod tests {
    use seekdeep_core::{
        session::{AppendOptions, SessionId},
        session_store::{CreateSessionOptions, SessionStore},
    };
    use seekdeep_invariants::InvariantConfig;
    use serde_json::{Map, json};

    use super::*;

    fn marks_definition() -> ProjectionDefinition {
        ProjectionDefinition::new(
            "test/marks",
            1,
            || Ok(Value::Null),
            |state, event| {
                if event.event_type == "test/mark" {
                    ProjectionTransition::changed(event.data.clone())
                } else {
                    let _ = state;
                    Ok(ProjectionTransition::Unchanged)
                }
            },
            |state| {
                let value = if state.is_null() {
                    json!({ "marks": [] })
                } else {
                    state.clone()
                };
                anyhow::ensure!(
                    value
                        .get("marks")
                        .and_then(Value::as_array)
                        .is_some_and(|marks| marks.iter().all(Value::is_string)),
                    "test/marks view violates its schema"
                );
                Ok(value)
            },
        )
    }

    fn count_definition(version: u64) -> ProjectionDefinition {
        ProjectionDefinition::new(
            "test/count",
            version,
            || Ok(json!(0)),
            |state, _| {
                ProjectionTransition::changed(
                    state
                        .as_u64()
                        .ok_or_else(|| anyhow::anyhow!("count state is not an integer"))?
                        + 1,
                )
            },
            |state| {
                anyhow::ensure!(state.as_u64().is_some(), "count view violates its schema");
                Ok(state.clone())
            },
        )
    }

    fn setup() -> (
        Context,
        Arc<SessionStore>,
        Arc<SessionProjectionRegistry>,
        Arc<Session>,
    ) {
        let context = Context::new();
        let sessions = SessionStore::install(&context).expect("sessions");
        let projections = SessionProjectionRegistry::install(&context).expect("projections");
        let session = sessions
            .create(
                &context,
                Some(SessionId::new("projection-test")),
                CreateSessionOptions::default(),
            )
            .expect("session");
        (context, sessions, projections, session)
    }

    fn mark(session: &Session, marks: &[&str]) -> SessionEvent {
        session
            .append(
                "test/mark",
                json!({ "marks": marks }),
                AppendOptions {
                    ignorable: true,
                    ..AppendOptions::default()
                },
            )
            .expect("append mark")
    }

    fn unrelated(session: &Session) -> SessionEvent {
        session
            .append(
                "test/unrelated",
                json!({}),
                AppendOptions {
                    ignorable: true,
                    ..AppendOptions::default()
                },
            )
            .expect("append unrelated")
    }

    fn event(seq: u64, event_type: &str, data: Value) -> SessionEvent {
        SessionEvent {
            event_type: event_type.to_owned(),
            seq,
            time: i64::try_from(seq).unwrap_or(i64::MAX),
            data,
            source_event_seqs: None,
            surface_op: None,
            ignorable: Some(true),
        }
    }

    #[test]
    fn eagerly_drives_lazily_builds_and_keeps_per_session_cells() {
        let (context, sessions, projections, session) = setup();
        mark(&session, &["pre-registration"]);
        projections
            .register(&context, marks_definition())
            .expect("register marks");
        assert_eq!(
            projections.snapshot(&session).expect("snapshot").values["test/marks"],
            json!({ "marks": ["pre-registration"] })
        );
        mark(&session, &["after"]);
        assert_eq!(
            projections.snapshot(&session).expect("snapshot").values["test/marks"],
            json!({ "marks": ["after"] })
        );

        let other = sessions
            .create(
                &context,
                Some(SessionId::new("projection-other")),
                CreateSessionOptions::default(),
            )
            .expect("other session");
        mark(&other, &["other"]);
        assert_eq!(
            projections
                .snapshot(&session)
                .expect("first snapshot")
                .values["test/marks"],
            json!({ "marks": ["after"] })
        );
        assert_eq!(
            projections.snapshot(&other).expect("other snapshot").values["test/marks"],
            json!({ "marks": ["other"] })
        );
    }

    #[test]
    fn empty_snapshot_and_change_gate_match_source() {
        let (context, _sessions, projections, session) = setup();
        projections
            .register(&context, marks_definition())
            .expect("marks");
        let snapshot = projections.snapshot(&session).expect("empty snapshot");
        assert_eq!(snapshot.as_of_seq, -1);
        assert_eq!(snapshot.values["test/marks"], json!({ "marks": [] }));

        let seen = Arc::new(Mutex::new(Vec::new()));
        let observed = seen.clone();
        projections
            .on_changed(
                &context,
                Arc::new(move |changed_session, key, value, seq| {
                    observed.lock().push((
                        changed_session.id().clone(),
                        key.to_owned(),
                        value.clone(),
                        seq,
                    ));
                    Ok(())
                }),
            )
            .expect("listener");
        let marked = mark(&session, &["a"]);
        unrelated(&session);
        assert_eq!(
            *seen.lock(),
            [(
                session.id().clone(),
                "test/marks".to_owned(),
                json!({ "marks": ["a"] }),
                marked.seq,
            )]
        );
        assert_eq!(
            projections.snapshot(&session).expect("snapshot").as_of_seq,
            1
        );
    }

    #[tokio::test]
    async fn shared_registration_counts_versions_and_hmr_ownership() {
        let (context, _sessions, projections, session) = setup();
        let first = projections
            .register(&context, marks_definition())
            .expect("first");
        let second = projections
            .register(&context, marks_definition())
            .expect("second");
        assert!(
            projections
                .register(&context, count_definition(MAX_SAFE_INTEGER + 1))
                .is_err()
        );
        let mismatch = ProjectionDefinition {
            state_version: 9,
            ..marks_definition()
        };
        assert!(
            projections
                .register(&context, mismatch)
                .expect_err("version mismatch")
                .to_string()
                .contains("already registered at stateVersion 1")
        );
        mark(&session, &["kept"]);
        first.dispose().await.expect("first dispose");
        assert!(
            projections
                .snapshot(&session)
                .expect("still registered")
                .values
                .contains_key("test/marks")
        );
        second.dispose().await.expect("second dispose");
        assert!(
            projections
                .snapshot(&session)
                .expect("removed")
                .values
                .is_empty()
        );

        let child_fiber = seekdeep_cordis::Fiber::active_child("projection plugin");
        let child = context.with_fiber(child_fiber.clone());
        projections
            .register(&child, marks_definition())
            .expect("child registration");
        let notifications = Arc::new(Mutex::new(0_u64));
        let observed = notifications.clone();
        projections
            .on_changed(
                &child,
                Arc::new(move |_, _, _, _| {
                    *observed.lock() += 1;
                    Ok(())
                }),
            )
            .expect("child listener");
        mark(&session, &["live"]);
        child_fiber.dispose().await.expect("dispose child");
        mark(&session, &["after-dispose"]);
        assert_eq!(*notifications.lock(), 1);
        assert!(
            projections
                .snapshot(&session)
                .expect("removed child")
                .values
                .is_empty()
        );
    }

    #[test]
    fn checkpoint_is_detached_and_view_checkpoint_filters_versions() {
        let (context, _sessions, projections, session) = setup();
        projections
            .register(&context, marks_definition())
            .expect("marks");
        projections
            .register(&context, count_definition(7))
            .expect("count");
        let marked = mark(&session, &["a"]);
        let mut checkpoint = projections.checkpoint(&session).expect("checkpoint");
        assert_eq!(
            checkpoint["test/marks"],
            ProjectionCheckpointRow {
                ver: 1,
                seq: i64::try_from(marked.seq).expect("seq"),
                val: json!({ "marks": ["a"] }),
            }
        );
        checkpoint
            .get_mut("test/marks")
            .and_then(|row| row.val.get_mut("marks"))
            .and_then(Value::as_array_mut)
            .expect("marks array")
            .push(json!("INJECTED"));
        assert_eq!(
            projections.snapshot(&session).expect("uncorrupted").values["test/marks"],
            json!({ "marks": ["a"] })
        );

        let rows = IndexMap::from([
            (
                "test/marks".to_owned(),
                ProjectionCheckpointRow {
                    ver: 1,
                    seq: 4,
                    val: json!({ "marks": ["stored"] }),
                },
            ),
            (
                "test/count".to_owned(),
                ProjectionCheckpointRow {
                    ver: 99,
                    seq: 4,
                    val: json!(5),
                },
            ),
        ]);
        let viewed = projections.view_checkpoint(&rows).expect("view checkpoint");
        assert_eq!(viewed["test/marks"], json!({ "marks": ["stored"] }));
        assert!(!viewed.contains_key("test/count"));
    }

    #[test]
    fn restore_floor_and_suffix_restore_enforce_honest_log_end() {
        let (context, _sessions, projections, _session) = setup();
        assert_eq!(projections.restore_floor(&IndexMap::new()), None);
        projections
            .register(&context, marks_definition())
            .expect("marks");
        projections
            .register(&context, count_definition(1))
            .expect("count");
        assert_eq!(projections.restore_floor(&IndexMap::new()), Some(0));
        let rows = IndexMap::from([
            (
                "test/marks".to_owned(),
                ProjectionCheckpointRow {
                    ver: 1,
                    seq: 10,
                    val: json!({ "marks": [] }),
                },
            ),
            (
                "test/count".to_owned(),
                ProjectionCheckpointRow {
                    ver: 1,
                    seq: 5,
                    val: json!(6),
                },
            ),
        ]);
        assert_eq!(projections.restore_floor(&rows), Some(5));

        let suffix_rows = IndexMap::from([
            (
                "test/marks".to_owned(),
                ProjectionCheckpointRow {
                    ver: 1,
                    seq: 4,
                    val: json!({ "marks": ["done"] }),
                },
            ),
            (
                "test/count".to_owned(),
                ProjectionCheckpointRow {
                    ver: 1,
                    seq: 2,
                    val: json!(3),
                },
            ),
        ]);
        let tail = [
            event(3, "test/unrelated", json!({})),
            event(4, "test/unrelated", json!({})),
        ];
        let restored = projections
            .restore(&suffix_rows, &tail, 3)
            .expect("suffix restore");
        assert_eq!(restored.snapshot.as_of_seq, 4);
        assert_eq!(
            restored.snapshot.values["test/marks"],
            json!({ "marks": ["done"] })
        );
        assert_eq!(restored.snapshot.values["test/count"], json!(5));

        let overreaching = IndexMap::from([(
            "test/count".to_owned(),
            ProjectionCheckpointRow {
                ver: 1,
                seq: 9,
                val: json!(10),
            },
        )]);
        let only_count_context = Context::new();
        let only_count = SessionProjectionRegistry::install(&only_count_context).expect("registry");
        only_count
            .register(&only_count_context, count_definition(1))
            .expect("count");
        assert_eq!(only_count.restore_floor(&overreaching), Some(9));
        assert!(
            only_count
                .restore(&overreaching, &[], 9)
                .expect_err("shrunk log")
                .to_string()
                .contains("re-read from seq 0")
        );
        let full = [
            event(0, "test/unrelated", json!({})),
            event(1, "test/unrelated", json!({})),
        ];
        assert_eq!(
            only_count
                .restore(&overreaching, &full, 0)
                .expect("full refold")
                .snapshot
                .values["test/count"],
            json!(2)
        );
    }

    #[test]
    fn restore_refolds_version_mismatch_only_from_zero_and_refreshes_rows() {
        let (context, _sessions, projections, _session) = setup();
        projections
            .register(&context, marks_definition())
            .expect("marks");
        projections
            .register(&context, count_definition(1))
            .expect("count");
        let rows = IndexMap::from([
            (
                "test/marks".to_owned(),
                ProjectionCheckpointRow {
                    ver: 1,
                    seq: 2,
                    val: json!({ "marks": ["old"] }),
                },
            ),
            (
                "test/count".to_owned(),
                ProjectionCheckpointRow {
                    ver: 99,
                    seq: 2,
                    val: json!(3),
                },
            ),
        ]);
        let tail = [
            event(3, "test/mark", json!({ "marks": ["new"] })),
            event(4, "test/unrelated", json!({})),
        ];
        assert!(projections.restore(&rows, &tail, 3).is_err());

        let full = [
            event(0, "test/unrelated", json!({})),
            event(1, "test/mark", json!({ "marks": ["old"] })),
            event(2, "test/mark", json!({ "marks": ["old", "2"] })),
            tail[0].clone(),
            tail[1].clone(),
        ];
        let restored = projections.restore(&rows, &full, 0).expect("full restore");
        assert_eq!(
            restored.snapshot.values["test/marks"],
            json!({ "marks": ["new"] })
        );
        assert_eq!(restored.snapshot.values["test/count"], json!(5));
        assert_eq!(restored.checkpoint["test/count"].seq, 4);
        assert_eq!(restored.checkpoint["test/count"].val, json!(5));
    }

    #[test]
    fn view_validation_fails_loudly() {
        let (context, _sessions, projections, session) = setup();
        let invalid = ProjectionDefinition::new(
            "invalid",
            1,
            || Ok(Value::Null),
            |_, _| Ok(ProjectionTransition::Unchanged),
            |_| anyhow::bail!("wire schema rejected view"),
        );
        projections.register(&context, invalid).expect("register");
        assert!(
            projections
                .snapshot(&session)
                .expect_err("invalid view")
                .to_string()
                .contains("wire schema rejected view")
        );
    }

    #[tokio::test]
    async fn invariant_companion_reserves_and_releases_package() {
        let context = Context::new();
        let invariants =
            InvariantRegistry::install(&context, &InvariantConfig::default()).expect("invariants");
        let registration = register_invariant(&invariants).expect("register invariant");
        registration.await_ready().await.expect("ready");
        assert!(invariants.is_registered("seekdeep-session-projection"));
        registration.dispose().await.expect("dispose");
        assert!(!invariants.is_registered("seekdeep-session-projection"));
    }

    #[test]
    fn checkpoint_wire_names_are_exact() {
        let snapshot = ProjectionSnapshot {
            as_of_seq: -1,
            values: IndexMap::from([("x".to_owned(), json!(1))]),
        };
        assert_eq!(
            serde_json::to_value(snapshot).expect("snapshot JSON"),
            json!({ "asOfSeq": -1, "values": { "x": 1 } })
        );
        let row = ProjectionCheckpointRow {
            ver: 1,
            seq: -1,
            val: Value::Object(Map::new()),
        };
        assert_eq!(
            serde_json::to_value(row).expect("row JSON"),
            json!({ "ver": 1, "seq": -1, "val": {} })
        );
    }
}
