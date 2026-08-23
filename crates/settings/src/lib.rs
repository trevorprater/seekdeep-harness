//! Abstract user-settings seam (`ctx.settings`).
//!
//! Providers own one raw document of namespace sections. Registrants layer
//! schema defaults, composition configuration, and the user section; writes
//! validate before persistence and commit only after storage succeeds.

use std::{
    collections::HashMap,
    future::Future,
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use async_trait::async_trait;
use futures::{FutureExt as _, future::BoxFuture};
use indexmap::IndexMap;
use parking_lot::{Mutex, RwLock};
use seekdeep_cordis::{
    Context, EventArgs, Fiber, FiberState, Plugin, PluginFiber, ServiceKey,
    fiber::{DisposeFuture, EffectHandle},
};
use seekdeep_invariants::InvariantError;
use seekdeep_schemastery::Schema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;
use tokio::sync::Notify;
use uuid::Uuid;

mod redact;
mod types;

/// Package-owned invariant companion.
pub mod invariant;

pub use invariant::{INVARIANT_NAME, register_invariant};
pub use redact::{RedactedSecret, RedactedValue, redact_secrets};
pub use types::{
    SETTINGS_DOCUMENT_UPDATED_EVENT, SETTINGS_UPDATED_EVENT, SettingsApplies, SettingsNamespace,
    SettingsUpdateSource,
};

/// Typed Cordis seat corresponding to `ctx.settings`.
pub const SETTINGS: ServiceKey<SettingsService> = ServiceKey::new("settings");
const NAMESPACE_PATTERN: &str = "/^[a-z][a-z0-9-]*$/";

/// Validates and brands a lowercase kebab-case namespace.
///
/// # Errors
///
/// Rejects empty, uppercase, digit-leading, underscore-bearing, and otherwise
/// non-kebab values.
pub fn settings_namespace(value: impl Into<String>) -> anyhow::Result<SettingsNamespace> {
    let value = value.into();
    let mut bytes = value.bytes();
    let valid = bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    anyhow::ensure!(
        valid,
        "settings namespace \"{value}\" must match {NAMESPACE_PATTERN}"
    );
    Ok(SettingsNamespace::new(value))
}

/// Structural equality over JSON-compatible data.
///
/// JSON object order is irrelevant, arrays remain positional, and all JSON
/// numbers compare with JavaScript number semantics (`1` equals `1.0`). This
/// is the service's single change-detection relation and is public so invariant
/// and compatibility layers need not restate it.
#[must_use]
pub fn deep_equal_json(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(left), Value::Bool(right)) => left == right,
        (Value::Number(left), Value::Number(right)) => left.as_f64() == right.as_f64(),
        (Value::String(left), Value::String(right)) => left == right,
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| deep_equal_json(left, right))
        }
        (Value::Object(left), Value::Object(right)) => deep_equal_maps(left, right),
        _ => false,
    }
}

fn deep_equal_maps(left: &Map<String, Value>, right: &Map<String, Value>) -> bool {
    left.len() == right.len()
        && left.iter().all(|(key, left)| {
            right
                .get(key)
                .is_some_and(|right| deep_equal_json(left, right))
        })
}

fn deep_equal_sections(
    left: Option<&Map<String, Value>>,
    right: Option<&Map<String, Value>>,
) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => deep_equal_maps(left, right),
        (None, None) => true,
        _ => false,
    }
}

/// Complete raw settings document.
pub type SettingsDocument = Map<String, Value>;

/// Provider storage primitives beneath [`SettingsService`].
#[async_trait]
pub trait SettingsStorage: Send + Sync + 'static {
    /// Whether writes are accepted.
    fn writable(&self) -> bool;

    /// Absolute local document path when this storage is one file.
    fn document_path(&self) -> Option<&Path> {
        None
    }

    /// Materializes and returns a local document path when supported.
    async fn prepare_document(&self) -> anyhow::Result<Option<PathBuf>> {
        Ok(self.document_path().map(Path::to_path_buf))
    }

    /// Loads the current complete document.
    async fn load(&self) -> anyhow::Result<SettingsDocument>;

    /// Durably stores one complete namespace section.
    async fn persist(
        &self,
        namespace: &SettingsNamespace,
        section: &Map<String, Value>,
    ) -> anyhow::Result<()>;
}

/// Owner-supplied validation beyond what a schema can express.
pub type SettingsValidator = Arc<dyn Fn(&Value) -> anyhow::Result<()> + Send + Sync>;
type WatchCallback =
    Arc<dyn Fn(Value, Value) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync + 'static>;

/// Registration options beyond the namespace schema.
#[derive(Clone, Default)]
pub struct SettingsRegisterOptions {
    /// Composition-layer values below the user section.
    pub base: Option<Value>,
    /// Owner-declared effect timing.
    pub applies: SettingsApplies,
    /// Cross-field or owner-capability validation over the resolved value.
    pub validate: Option<SettingsValidator>,
}

impl std::fmt::Debug for SettingsRegisterOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SettingsRegisterOptions")
            .field("base", &self.base)
            .field("applies", &self.applies)
            .field("validate", &self.validate.as_ref().map(|_| "<validator>"))
            .finish()
    }
}

/// One registered namespace surfaced to configuration UIs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsDescriptor {
    /// Registered namespace.
    pub ns: SettingsNamespace,
    /// Canonical Schemastery wire graph.
    pub schema: Value,
    /// Current resolved value.
    pub value: Value,
    /// Monotonic raw-section revision.
    pub revision: u64,
    /// Composition base layer, when declared.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<Value>,
    /// Raw user section, when present and well formed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<Value>,
    /// Owner-declared effect timing.
    pub applies: SettingsApplies,
    /// Secret slots, only when redaction was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secrets: Option<Vec<RedactedSecret>>,
}

/// One path-addressed edit to a namespace's user section.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum SettingsPathOp {
    /// Assign a value, creating intermediate objects as needed.
    Set {
        /// Path from the section root; empty addresses the root.
        path: Vec<String>,
        /// JSON value to assign.
        value: Value,
    },
    /// Remove a value; an absent path is a no-op.
    Unset {
        /// Path from the section root; empty resets the whole section.
        path: Vec<String>,
    },
}

/// A write refused because the namespace moved after an editor read it.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error(
    "settings namespace \"{namespace}\" changed since it was read (expected revision {expected}, now {actual})"
)]
pub struct SettingsConflictError {
    /// Stable machine code.
    pub code: &'static str,
    /// Namespace whose write was refused.
    pub namespace: SettingsNamespace,
    /// Revision supplied by the caller.
    pub expected: u64,
    /// Current revision.
    pub actual: u64,
}

impl SettingsConflictError {
    fn new(namespace: SettingsNamespace, expected: u64, actual: u64) -> Self {
        Self {
            code: "SETTINGS_CONFLICT",
            namespace,
            expected,
            actual,
        }
    }
}

struct Watcher {
    id: Uuid,
    callback: WatchCallback,
    tail: tokio::sync::Mutex<()>,
    active: AtomicBool,
}

struct Registration {
    id: Uuid,
    ns: SettingsNamespace,
    schema: Schema,
    base: Option<Value>,
    applies: SettingsApplies,
    validate: Option<SettingsValidator>,
    resolved: RwLock<Value>,
    revision: AtomicU64,
    watchers: Mutex<IndexMap<Uuid, Arc<Watcher>>>,
    write_queue: tokio::sync::Mutex<()>,
}

#[derive(Default)]
struct State {
    document: SettingsDocument,
    registrations: IndexMap<SettingsNamespace, Arc<Registration>>,
}

#[derive(Default)]
struct ActivityState {
    stopped: bool,
    count: usize,
}

#[derive(Default)]
struct Activity {
    state: Mutex<ActivityState>,
    changed: Notify,
}

impl Activity {
    fn begin(self: &Arc<Self>) -> Option<ActivityGuard> {
        let mut state = self.state.lock();
        if state.stopped {
            return None;
        }
        state.count += 1;
        Some(ActivityGuard {
            activity: self.clone(),
        })
    }

    fn is_stopped(&self) -> bool {
        self.state.lock().stopped
    }

    async fn stop_and_drain(&self) {
        self.state.lock().stopped = true;
        loop {
            let notified = self.changed.notified();
            if self.state.lock().count == 0 {
                return;
            }
            notified.await;
        }
    }
}

struct ActivityGuard {
    activity: Arc<Activity>,
}

impl Drop for ActivityGuard {
    fn drop(&mut self) {
        let mut state = self.activity.state.lock();
        state.count -= 1;
        if state.count == 0 {
            self.activity.changed.notify_waiters();
        }
    }
}

/// Live provider facade published through Cordis.
pub struct SettingsService {
    context: Context,
    storage: Arc<dyn SettingsStorage>,
    state: Mutex<State>,
    activity: Arc<Activity>,
}

impl std::fmt::Debug for SettingsService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state.lock();
        formatter
            .debug_struct("SettingsService")
            .field("registrations", &state.registrations.len())
            .field("stopped", &self.activity.is_stopped())
            .finish_non_exhaustive()
    }
}

impl SettingsService {
    /// Loads a provider before publishing the service and installs drain-first
    /// teardown on the provider context.
    ///
    /// # Errors
    ///
    /// Returns provider load, duplicate-service, or inactive-owner failures.
    pub async fn install(
        context: &Context,
        storage: Arc<dyn SettingsStorage>,
    ) -> anyhow::Result<Arc<Self>> {
        let fiber = Fiber::active_child("settings");
        let child = context.with_fiber(fiber.clone());
        let service = match Self::install_scoped(&child, storage).await {
            Ok(service) => service,
            Err(error) => {
                return match fiber.dispose().await {
                    Ok(()) => Err(error),
                    Err(cleanup) => Err(anyhow::anyhow!(
                        "{error:#}: settings installation rollback failed: {cleanup:#}"
                    )),
                };
            }
        };
        let cleanup = fiber.clone();
        let effect = EffectHandle::new("settings", move || -> DisposeFuture {
            Box::pin(async move { cleanup.dispose().await })
        });
        match context.own(effect) {
            Ok(_) => Ok(service),
            Err(error) => match fiber.dispose().await {
                Ok(()) => Err(error.into()),
                Err(cleanup) => Err(anyhow::anyhow!(
                    "{error}: settings ownership rollback failed: {cleanup:#}"
                )),
            },
        }
    }

    async fn install_scoped(
        context: &Context,
        storage: Arc<dyn SettingsStorage>,
    ) -> anyhow::Result<Arc<Self>> {
        let document = storage.load().await?;
        let service = Arc::new(Self {
            context: context.clone(),
            storage,
            state: Mutex::new(State {
                document,
                ..State::default()
            }),
            activity: Arc::new(Activity::default()),
        });
        let activity = service.activity.clone();
        context.own(EffectHandle::new(
            "settings drain",
            move || -> DisposeFuture {
                Box::pin(async move {
                    activity.stop_and_drain().await;
                    Ok(())
                })
            },
        ))?;
        context.provide(SETTINGS, service.clone())?;
        Ok(service)
    }

    /// Absolute local settings document path, when supported.
    #[must_use]
    pub fn document_path(&self) -> Option<PathBuf> {
        self.storage.document_path().map(Path::to_path_buf)
    }

    /// Whether the mounted provider currently accepts writes.
    #[must_use]
    pub fn writable(&self) -> bool {
        self.storage.writable()
    }

    /// Prepares the provider's local document for a native editor.
    ///
    /// # Errors
    ///
    /// Propagates provider materialization failures.
    pub async fn prepare_document(&self) -> anyhow::Result<Option<PathBuf>> {
        self.storage.prepare_document().await
    }

    /// Returns a weak publisher suitable for a provider watch task.
    #[must_use]
    pub fn publisher(self: &Arc<Self>) -> SettingsPublisher {
        SettingsPublisher {
            service: Arc::downgrade(self),
        }
    }

    /// Registers one namespace on the caller's lifecycle context.
    ///
    /// # Errors
    ///
    /// Returns duplicate, stored-section, schema, owner-validation, or
    /// inactive-context failures.
    pub fn register(
        self: &Arc<Self>,
        owner: &Context,
        ns: &SettingsNamespace,
        schema: Schema,
        options: SettingsRegisterOptions,
    ) -> anyhow::Result<SettingsScope> {
        let stored_section = {
            let state = self.state.lock();
            anyhow::ensure!(
                !state.registrations.contains_key(ns),
                "settings namespace \"{ns}\" is already registered"
            );
            section(&state.document, ns)?.cloned()
        };
        let resolved = resolve(
            &schema,
            options.base.as_ref(),
            stored_section.as_ref(),
            options.validate.as_ref(),
        )?;
        let registration = Arc::new(Registration {
            id: Uuid::now_v7(),
            ns: ns.clone(),
            schema,
            base: options.base,
            applies: options.applies,
            validate: options.validate,
            resolved: RwLock::new(resolved),
            revision: AtomicU64::new(0),
            watchers: Mutex::new(IndexMap::new()),
            write_queue: tokio::sync::Mutex::new(()),
        });
        {
            let mut state = self.state.lock();
            anyhow::ensure!(
                !state.registrations.contains_key(ns),
                "settings namespace \"{ns}\" is already registered"
            );
            state.registrations.insert(ns.clone(), registration.clone());
        }
        let service = Arc::downgrade(self);
        let registration_id = registration.id;
        let disposal_ns = ns.clone();
        let effect = EffectHandle::synchronous(format!("settings.register(\"{ns}\")"), move || {
            if let Some(service) = service.upgrade() {
                let mut state = service.state.lock();
                if state
                    .registrations
                    .get(&disposal_ns)
                    .is_some_and(|entry| entry.id == registration_id)
                {
                    state.registrations.shift_remove(&disposal_ns);
                }
            }
            Ok(())
        });
        if let Err(error) = owner.own(effect.clone()) {
            self.remove_registration(ns, registration.id);
            return Err(error.into());
        }
        Ok(SettingsScope {
            service: Arc::downgrade(self),
            registration,
            _registration_effect: effect,
        })
    }

    fn remove_registration(&self, ns: &SettingsNamespace, id: Uuid) {
        let mut state = self.state.lock();
        if state
            .registrations
            .get(ns)
            .is_some_and(|entry| entry.id == id)
        {
            state.registrations.shift_remove(ns);
        }
    }

    /// Reads one registered namespace's authoritative resolved value.
    #[must_use]
    pub fn get(&self, ns: &SettingsNamespace) -> Option<Value> {
        let registration = self.state.lock().registrations.get(ns).cloned()?;
        let value = registration.resolved.read().clone();
        Some(value)
    }

    /// Describes all namespaces in registration order.
    #[must_use]
    pub fn describe(&self, redact: bool) -> Vec<SettingsDescriptor> {
        let (document, registrations) = {
            let state = self.state.lock();
            (
                state.document.clone(),
                state.registrations.values().cloned().collect::<Vec<_>>(),
            )
        };
        registrations
            .into_iter()
            .map(|registration| {
                let user = section(&document, &registration.ns)
                    .ok()
                    .flatten()
                    .map(|value| Value::Object(value.clone()));
                let value = registration.resolved.read().clone();
                let mut descriptor = SettingsDescriptor {
                    ns: registration.ns.clone(),
                    schema: registration.schema.to_json(),
                    value: value.clone(),
                    revision: registration.revision.load(Ordering::Acquire),
                    base: registration.base.clone(),
                    user,
                    applies: registration.applies,
                    secrets: None,
                };
                if redact {
                    let redacted = redact_secrets(&registration.schema, Some(&value));
                    descriptor.value = redacted.value.unwrap_or(Value::Null);
                    descriptor.base = descriptor
                        .base
                        .as_ref()
                        .and_then(|base| redact_secrets(&registration.schema, Some(base)).value);
                    descriptor.user = descriptor
                        .user
                        .as_ref()
                        .and_then(|user| redact_secrets(&registration.schema, Some(user)).value);
                    descriptor.secrets = Some(redacted.secrets);
                }
                descriptor
            })
            .collect()
    }

    /// Merges a patch into one user section.
    ///
    /// # Errors
    ///
    /// Returns lifecycle, conflict, validation, or persistence failures.
    pub async fn update(
        self: &Arc<Self>,
        ns: &SettingsNamespace,
        patch: Value,
        expected_revision: Option<u64>,
    ) -> anyhow::Result<()> {
        self.write(
            ns,
            WriteInput::Merge(object_input(ns, &patch, "update")?),
            expected_revision,
        )
        .await
    }

    /// Replaces one user section wholesale.
    ///
    /// # Errors
    ///
    /// Returns lifecycle, conflict, validation, or persistence failures.
    pub async fn replace(
        self: &Arc<Self>,
        ns: &SettingsNamespace,
        section: Value,
        expected_revision: Option<u64>,
    ) -> anyhow::Result<()> {
        self.write(
            ns,
            WriteInput::Replace(object_input(ns, &section, "replace")?),
            expected_revision,
        )
        .await
    }

    /// Applies ordered path edits to one user section.
    ///
    /// # Errors
    ///
    /// Returns lifecycle, conflict, root-shape, validation, or persistence failures.
    pub async fn mutate(
        self: &Arc<Self>,
        ns: &SettingsNamespace,
        operations: Vec<SettingsPathOp>,
        expected_revision: Option<u64>,
    ) -> anyhow::Result<()> {
        self.write(ns, WriteInput::Mutate(operations), expected_revision)
            .await
    }

    async fn write(
        self: &Arc<Self>,
        ns: &SettingsNamespace,
        input: WriteInput,
        expected_revision: Option<u64>,
    ) -> anyhow::Result<()> {
        let verb = input.verb();
        let registration = self
            .state
            .lock()
            .registrations
            .get(ns)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("settings namespace \"{ns}\" is not registered"))?;
        anyhow::ensure!(
            !self.activity.is_stopped(),
            "settings service is disposed: \"{ns}\" cannot be written"
        );
        anyhow::ensure!(
            self.storage.writable(),
            "settings provider is read-only: \"{ns}\" cannot be updated in-process"
        );
        let _activity = self.activity.begin().ok_or_else(|| {
            anyhow::anyhow!("settings service is disposed: \"{ns}\" cannot be written")
        })?;
        let _queue = registration.write_queue.lock().await;
        anyhow::ensure!(
            !self.activity.is_stopped(),
            "settings service was disposed before the queued \"{ns}\" {verb} ran"
        );
        {
            let state = self.state.lock();
            anyhow::ensure!(
                state
                    .registrations
                    .get(ns)
                    .is_some_and(|entry| entry.id == registration.id),
                "settings namespace \"{ns}\" registration was disposed before the queued {verb} ran"
            );
        }
        let current = {
            let state = self.state.lock();
            section(&state.document, ns)?.cloned().unwrap_or_default()
        };
        let actual = registration.revision.load(Ordering::Acquire);
        if let Some(expected) = expected_revision
            && expected != actual
        {
            return Err(SettingsConflictError::new(ns.clone(), expected, actual).into());
        }
        let next_section = match input {
            WriteInput::Merge(patch) => merge_maps(&current, &patch),
            WriteInput::Replace(section) => section,
            WriteInput::Mutate(operations) => operations
                .iter()
                .try_fold(current.clone(), |section, operation| {
                    apply_path_op(&section, operation)
                })?,
        };
        let next = resolve(
            &registration.schema,
            registration.base.as_ref(),
            Some(&next_section),
            registration.validate.as_ref(),
        )?;
        self.storage.persist(ns, &next_section).await?;
        let still_owner = {
            let mut state = self.state.lock();
            state
                .document
                .insert(ns.as_str().to_owned(), Value::Object(next_section.clone()));
            state
                .registrations
                .get(ns)
                .is_some_and(|entry| entry.id == registration.id)
                && !self.activity.is_stopped()
        };
        if still_owner {
            self.bump_revision(&registration, Some(&current), Some(&next_section))?;
            self.commit(&registration, next, SettingsUpdateSource::Update)?;
        }
        Ok(())
    }

    /// Publishes a complete detached document observed by a provider.
    ///
    /// Invalid namespaces keep their last good value; other namespaces still
    /// commit. Invariant-coded event failures propagate.
    ///
    /// # Errors
    ///
    /// Returns a synchronous invariant event failure.
    pub fn publish(
        self: &Arc<Self>,
        document: SettingsDocument,
        source: SettingsUpdateSource,
    ) -> anyhow::Result<()> {
        let (before, registrations) = {
            let mut state = self.state.lock();
            let before = state
                .registrations
                .values()
                .map(|registration| {
                    (
                        registration.ns.clone(),
                        section(&state.document, &registration.ns)
                            .ok()
                            .flatten()
                            .cloned(),
                    )
                })
                .collect::<HashMap<_, _>>();
            let registrations = state.registrations.values().cloned().collect::<Vec<_>>();
            state.document = document;
            (before, registrations)
        };
        for registration in registrations {
            let after = {
                let state = self.state.lock();
                match section(&state.document, &registration.ns) {
                    Ok(value) => value.cloned(),
                    Err(error) => {
                        tracing::warn!(namespace = %registration.ns, "settings: keeping last good value after invalid stored section");
                        tracing::warn!(%error, "settings stored section error");
                        continue;
                    }
                }
            };
            let next = match resolve(
                &registration.schema,
                registration.base.as_ref(),
                after.as_ref(),
                registration.validate.as_ref(),
            ) {
                Ok(next) => next,
                Err(error) => {
                    tracing::warn!(namespace = %registration.ns, "settings: keeping last good value after invalid stored section");
                    tracing::warn!(%error, "settings stored section error");
                    continue;
                }
            };
            self.bump_revision(
                &registration,
                before.get(&registration.ns).and_then(Option::as_ref),
                after.as_ref(),
            )?;
            self.commit(&registration, next, source)?;
        }
        Ok(())
    }

    fn bump_revision(
        &self,
        registration: &Registration,
        before: Option<&Map<String, Value>>,
        after: Option<&Map<String, Value>>,
    ) -> anyhow::Result<()> {
        if deep_equal_sections(before, after) {
            return Ok(());
        }
        let revision = registration.revision.fetch_add(1, Ordering::AcqRel) + 1;
        self.emit_contained(
            SETTINGS_DOCUMENT_UPDATED_EVENT,
            &EventArgs::from_values(vec![Arc::new(registration.ns.clone()), Arc::new(revision)]),
            &registration.ns,
        )
    }

    fn commit(
        self: &Arc<Self>,
        registration: &Arc<Registration>,
        next: Value,
        source: SettingsUpdateSource,
    ) -> anyhow::Result<()> {
        let previous = registration.resolved.read().clone();
        if deep_equal_json(&previous, &next) {
            return Ok(());
        }
        *registration.resolved.write() = next.clone();
        let watchers = registration
            .watchers
            .lock()
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for watcher in watchers {
            let Some(activity) = self.activity.begin() else {
                continue;
            };
            let service = Arc::downgrade(self);
            let ns = registration.ns.clone();
            let next = next.clone();
            let previous = previous.clone();
            tokio::spawn(async move {
                let _activity = activity;
                let _tail = watcher.tail.lock().await;
                if !watcher.active.load(Ordering::Acquire)
                    || service
                        .upgrade()
                        .is_none_or(|service| service.activity.is_stopped())
                {
                    return;
                }
                let callback = watcher.callback.clone();
                let future = catch_unwind(AssertUnwindSafe(|| callback(next, previous)));
                let result = match future {
                    Ok(future) => AssertUnwindSafe(future).catch_unwind().await,
                    Err(payload) => {
                        tracing::warn!(namespace = %ns, "settings watcher panicked: {}", panic_message(&payload));
                        return;
                    }
                };
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        tracing::warn!(namespace = %ns, %error, "settings watcher failed");
                    }
                    Err(payload) => {
                        tracing::warn!(namespace = %ns, "settings watcher panicked: {}", panic_message(&payload));
                    }
                }
            });
        }
        self.emit_contained(
            SETTINGS_UPDATED_EVENT,
            &EventArgs::from_values(vec![
                Arc::new(registration.ns.clone()),
                Arc::new(next),
                Arc::new(previous),
                Arc::new(source),
            ]),
            &registration.ns,
        )
    }

    fn emit_contained(
        &self,
        event: &str,
        args: &EventArgs,
        ns: &SettingsNamespace,
    ) -> anyhow::Result<()> {
        let emission = self
            .context
            .events()
            .prepare_emit(&self.context, event, args)?;
        let mut invariant_failure = None;
        emission.emit_contained(|error| {
            if error.downcast_ref::<InvariantError>().is_some() && invariant_failure.is_none() {
                invariant_failure = Some(error);
            } else {
                tracing::warn!(namespace = %ns, %error, event, "settings listener failed");
            }
        });
        invariant_failure.map_or(Ok(()), Err)
    }
}

/// Weak provider-facing publication handle.
#[derive(Clone, Debug)]
pub struct SettingsPublisher {
    service: Weak<SettingsService>,
}

impl SettingsPublisher {
    /// Publishes a provider-observed complete document when the service lives.
    ///
    /// # Errors
    ///
    /// Returns synchronous invariant event failures.
    pub fn publish(&self, document: SettingsDocument) -> anyhow::Result<()> {
        if let Some(service) = self.service.upgrade() {
            service.publish(document, SettingsUpdateSource::Provider)?;
        }
        Ok(())
    }
}

/// Owner-facing handle for one registered namespace.
pub struct SettingsScope {
    service: Weak<SettingsService>,
    registration: Arc<Registration>,
    _registration_effect: EffectHandle,
}

type SettingsSourceGetter = Arc<dyn Fn() -> Value + Send + Sync>;

/// Current source selected by [`install_settings_section`].
#[derive(Clone)]
pub struct SettingsSectionSource {
    current: Arc<RwLock<SettingsSourceGetter>>,
}

impl std::fmt::Debug for SettingsSectionSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SettingsSectionSource")
            .field("value", &self.get())
            .finish_non_exhaustive()
    }
}

impl SettingsSectionSource {
    /// Reads the currently authoritative resolved or composition value.
    #[must_use]
    pub fn get(&self) -> Value {
        let getter = self.current.read().clone();
        getter()
    }
}

/// Dynamic optional-settings consumer installation.
#[derive(Debug)]
pub struct InstalledSettingsSection {
    /// Current source, initialized to the composition entry.
    pub source: SettingsSectionSource,
    /// Dependency-driven helper plugin owned by the consumer context.
    pub fiber: Arc<PluginFiber>,
}

/// Installs the canonical optional-settings consumer wiring.
///
/// While a settings service exists, the helper registers `ns`, selects the
/// resolved scope, and invokes `on_change` for attachment and live commits.
/// When the provider disappears it restores `entry`; teardown of the consumer
/// itself stays silent.
///
/// # Errors
///
/// Returns an inactive-context plugin-mount failure.
pub fn install_settings_section(
    context: &Context,
    ns: &SettingsNamespace,
    schema: Schema,
    entry: Value,
    validate: Option<SettingsValidator>,
    on_change: Arc<dyn Fn() -> anyhow::Result<()> + Send + Sync>,
) -> anyhow::Result<InstalledSettingsSection> {
    let initial = entry.clone();
    let current: Arc<RwLock<SettingsSourceGetter>> =
        Arc::new(RwLock::new(Arc::new(move || initial.clone())));
    let source = SettingsSectionSource {
        current: current.clone(),
    };
    let consumer_fiber = context.fiber().clone();
    let plugin_ns = ns.clone();
    let plugin = Plugin::new(
        format!("settings-section:{ns}"),
        ["settings"],
        move |child, _| {
            let current = current.clone();
            let entry = entry.clone();
            let schema = schema.clone();
            let ns = plugin_ns.clone();
            let validate = validate.clone();
            let on_change = on_change.clone();
            let consumer_fiber = consumer_fiber.clone();
            Box::pin(async move {
                let settings = child.get(SETTINGS).ok_or_else(|| {
                    anyhow::anyhow!("settings service disappeared during attachment")
                })?;
                let scope = settings.register(
                    &child,
                    &ns,
                    schema,
                    SettingsRegisterOptions {
                        base: Some(entry.clone()),
                        validate,
                        ..SettingsRegisterOptions::default()
                    },
                )?;
                let resolved_registration = scope.registration.clone();
                *current.write() = Arc::new(move || resolved_registration.resolved.read().clone());
                let fallback_current = current.clone();
                let fallback_entry = entry.clone();
                let fallback_change = on_change.clone();
                let fallback_consumer = consumer_fiber.clone();
                child.own(EffectHandle::synchronous(
                    format!("settings-section:{ns}:fallback"),
                    move || {
                        if matches!(
                            fallback_consumer.state(),
                            FiberState::Unloading | FiberState::Disposed
                        ) {
                            return Ok(());
                        }
                        *fallback_current.write() = Arc::new(move || fallback_entry.clone());
                        fallback_change()
                    },
                ))?;
                on_change()?;
                let watch_change = on_change.clone();
                let watch_consumer = consumer_fiber.clone();
                let watcher = scope.watch(move |_, _| {
                    let on_change = watch_change.clone();
                    let consumer = watch_consumer.clone();
                    async move {
                        if matches!(
                            consumer.state(),
                            FiberState::Unloading | FiberState::Disposed
                        ) {
                            return Ok(());
                        }
                        on_change()
                    }
                });
                child.own(EffectHandle::synchronous(
                    format!("settings-section:{ns}:watch"),
                    move || {
                        watcher.dispose();
                        Ok(())
                    },
                ))?;
                Ok(())
            })
        },
    );
    let fiber = context.plugin(plugin, Value::Null)?;
    Ok(InstalledSettingsSection { source, fiber })
}

impl std::fmt::Debug for SettingsScope {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SettingsScope")
            .field("namespace", &self.registration.ns)
            .finish_non_exhaustive()
    }
}

impl SettingsScope {
    /// Current resolved value, retained even after registration disposal.
    #[must_use]
    pub fn get(&self) -> Value {
        self.registration.resolved.read().clone()
    }

    /// Adds a serialized asynchronous watcher.
    #[must_use]
    pub fn watch<F, Fut>(&self, callback: F) -> SettingsWatchHandle
    where
        F: Fn(Value, Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        let callback: WatchCallback =
            Arc::new(move |next, previous| Box::pin(callback(next, previous)));
        let watcher = Arc::new(Watcher {
            id: Uuid::now_v7(),
            callback,
            tail: tokio::sync::Mutex::new(()),
            active: AtomicBool::new(true),
        });
        self.registration
            .watchers
            .lock()
            .insert(watcher.id, watcher.clone());
        SettingsWatchHandle {
            registration: Arc::downgrade(&self.registration),
            watcher,
        }
    }

    /// Merges a patch without a revision expectation.
    ///
    /// # Errors
    ///
    /// Propagates service write failures.
    pub async fn update(&self, patch: Value) -> anyhow::Result<()> {
        self.service()?
            .update(&self.registration.ns, patch, None)
            .await
    }

    /// Replaces the section without a revision expectation.
    ///
    /// # Errors
    ///
    /// Propagates service write failures.
    pub async fn replace(&self, section: Value) -> anyhow::Result<()> {
        self.service()?
            .replace(&self.registration.ns, section, None)
            .await
    }

    fn service(&self) -> anyhow::Result<Arc<SettingsService>> {
        self.service
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("settings service is disposed"))
    }
}

/// Manual watcher disposer; disposal is idempotent.
pub struct SettingsWatchHandle {
    registration: Weak<Registration>,
    watcher: Arc<Watcher>,
}

impl std::fmt::Debug for SettingsWatchHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SettingsWatchHandle")
            .field("watcher", &self.watcher.id)
            .field("active", &self.watcher.active.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl SettingsWatchHandle {
    /// Prevents queued and future callback starts.
    pub fn dispose(&self) {
        self.watcher.active.store(false, Ordering::Release);
        if let Some(registration) = self.registration.upgrade() {
            registration.watchers.lock().shift_remove(&self.watcher.id);
        }
    }
}

enum WriteInput {
    Merge(Map<String, Value>),
    Replace(Map<String, Value>),
    Mutate(Vec<SettingsPathOp>),
}

impl WriteInput {
    const fn verb(&self) -> &'static str {
        match self {
            Self::Merge(_) => "update",
            Self::Replace(_) => "replace",
            Self::Mutate(_) => "mutate",
        }
    }
}

fn object_input(
    ns: &SettingsNamespace,
    value: &Value,
    verb: &str,
) -> anyhow::Result<Map<String, Value>> {
    value
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("settings {verb} for \"{ns}\" must be a plain object"))
}

fn section<'a>(
    document: &'a SettingsDocument,
    ns: &SettingsNamespace,
) -> anyhow::Result<Option<&'a Map<String, Value>>> {
    let Some(value) = document.get(ns.as_str()) else {
        return Ok(None);
    };
    value
        .as_object()
        .map(Some)
        .ok_or_else(|| anyhow::anyhow!("settings section \"{ns}\" must be an object of keys"))
}

fn resolve(
    schema: &Schema,
    base: Option<&Value>,
    section: Option<&Map<String, Value>>,
    validate: Option<&SettingsValidator>,
) -> anyhow::Result<Value> {
    let section = section.map(|section| Value::Object(section.clone()));
    let candidate = merge_layers(base, section.as_ref());
    let value = schema.resolve(&candidate)?;
    if let Some(validate) = validate {
        validate(&value)?;
    }
    Ok(value)
}

fn merge_layers(under: Option<&Value>, over: Option<&Value>) -> Value {
    match (under, over) {
        (_, Some(over)) if !over.is_object() => over.clone(),
        (Some(Value::Object(under)), Some(Value::Object(over))) => {
            let mut output = under.clone();
            for (key, value) in over {
                let merged = output.get(key).map_or_else(
                    || value.clone(),
                    |under| merge_layers(Some(under), Some(value)),
                );
                output.insert(key.clone(), merged);
            }
            Value::Object(output)
        }
        (_, Some(over)) => over.clone(),
        (Some(under), None) => under.clone(),
        (None, None) => Value::Null,
    }
}

fn merge_maps(under: &Map<String, Value>, over: &Map<String, Value>) -> Map<String, Value> {
    merge_layers(
        Some(&Value::Object(under.clone())),
        Some(&Value::Object(over.clone())),
    )
    .as_object()
    .cloned()
    .expect("object merge returns object")
}

fn apply_path_op(
    section: &Map<String, Value>,
    operation: &SettingsPathOp,
) -> anyhow::Result<Map<String, Value>> {
    let (path, value) = match operation {
        SettingsPathOp::Set { path, value } => (path, Some(value)),
        SettingsPathOp::Unset { path } => (path, None),
    };
    let Some((head, rest)) = path.split_first() else {
        return match value {
            None => Ok(Map::new()),
            Some(Value::Object(object)) => Ok(object.clone()),
            Some(_) => {
                anyhow::bail!("settings mutate: setting the section root requires a plain object")
            }
        };
    };
    let mut output = section.clone();
    if rest.is_empty() {
        if let Some(value) = value {
            output.insert(head.clone(), value.clone());
        } else {
            output.remove(head);
        }
        return Ok(output);
    }
    let child = section.get(head).and_then(Value::as_object);
    if child.is_none() && value.is_none() {
        return Ok(output);
    }
    let nested = match value {
        Some(value) => SettingsPathOp::Set {
            path: rest.to_vec(),
            value: value.clone(),
        },
        None => SettingsPathOp::Unset {
            path: rest.to_vec(),
        },
    };
    let empty = Map::new();
    output.insert(
        head.clone(),
        Value::Object(apply_path_op(child.unwrap_or(&empty), &nested)?),
    );
    Ok(output)
}

fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    payload.downcast_ref::<&str>().map_or_else(
        || {
            payload
                .downcast_ref::<String>()
                .cloned()
                .unwrap_or_else(|| "non-string panic".to_owned())
        },
        |message| (*message).to_owned(),
    )
}
