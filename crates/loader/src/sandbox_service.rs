//! Explicit JSON adapters for Rust Services visible to model-authored JavaScript.

use std::{collections::BTreeMap, sync::Arc};

use futures::future::BoxFuture;
use indexmap::IndexMap;
use parking_lot::Mutex;
use seekdeep_cordis::{Context, ServiceKey, fiber::EffectHandle};
use serde_json::Value;

/// Asynchronous JSON method exposed by one adapted Rust Service.
pub type SandboxServiceMethod =
    Arc<dyn Fn(Vec<Value>) -> BoxFuture<'static, anyhow::Result<Value>> + Send + Sync>;

/// Injected policy for completing an adapted asynchronous method from a worker thread.
pub trait SandboxServiceDispatcher: Send + Sync + 'static {
    /// Runs one method future to completion outside the JavaScript worker.
    ///
    /// # Errors
    ///
    /// Returns method failures or dispatcher lifecycle failures.
    fn dispatch(&self, future: BoxFuture<'static, anyhow::Result<Value>>) -> anyhow::Result<Value>;
}

/// Tokio boundary dispatcher captured by native application composition.
#[derive(Clone, Debug)]
pub struct TokioSandboxServiceDispatcher {
    handle: tokio::runtime::Handle,
}

impl TokioSandboxServiceDispatcher {
    /// Captures the current Tokio runtime.
    ///
    /// # Errors
    ///
    /// Returns when called outside a Tokio runtime.
    pub fn current() -> anyhow::Result<Arc<Self>> {
        Ok(Arc::new(Self {
            handle: tokio::runtime::Handle::try_current()?,
        }))
    }
}

impl SandboxServiceDispatcher for TokioSandboxServiceDispatcher {
    fn dispatch(&self, future: BoxFuture<'static, anyhow::Result<Value>>) -> anyhow::Result<Value> {
        self.handle.block_on(future)
    }
}

/// One allowlisted Rust Service adapter.
#[derive(Clone)]
pub struct SandboxServiceRegistration {
    /// Cordis Service name.
    pub name: String,
    /// Read-only JSON fields visible beside methods.
    pub projection: Value,
    /// Explicit callable method directory.
    pub methods: BTreeMap<String, SandboxServiceMethod>,
}

impl std::fmt::Debug for SandboxServiceRegistration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SandboxServiceRegistration")
            .field("name", &self.name)
            .field("projection", &self.projection)
            .field("methods", &self.methods.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl SandboxServiceRegistration {
    /// Creates an adapter with no callable methods.
    #[must_use]
    pub fn new(name: impl Into<String>, projection: Value) -> Self {
        Self {
            name: name.into(),
            projection,
            methods: BTreeMap::new(),
        }
    }

    /// Adds or replaces one allowlisted method.
    #[must_use]
    pub fn method(mut self, name: impl Into<String>, method: SandboxServiceMethod) -> Self {
        self.methods.insert(name.into(), method);
        self
    }
}

struct StoredRegistration {
    generation: u64,
    registration: SandboxServiceRegistration,
}

#[derive(Default)]
struct RegistryState {
    registrations: IndexMap<String, StoredRegistration>,
    next_generation: u64,
}

/// Cordis service containing explicit native-Service sandbox adapters.
pub struct SandboxServiceRegistry {
    dispatcher: Arc<dyn SandboxServiceDispatcher>,
    state: Arc<Mutex<RegistryState>>,
}

impl std::fmt::Debug for SandboxServiceRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SandboxServiceRegistry")
            .field("registrations", &self.state.lock().registrations.len())
            .finish_non_exhaustive()
    }
}

/// Cordis slot for native Service adapters.
pub const SANDBOX_SERVICES: ServiceKey<SandboxServiceRegistry> = ServiceKey::new("sandboxServices");

impl SandboxServiceRegistry {
    /// Creates an unprovided registry over an injected dispatcher.
    #[must_use]
    pub fn new(dispatcher: Arc<dyn SandboxServiceDispatcher>) -> Arc<Self> {
        Arc::new(Self {
            dispatcher,
            state: Arc::new(Mutex::new(RegistryState::default())),
        })
    }

    /// Provides the adapter registry for `context`'s lifetime.
    ///
    /// # Errors
    ///
    /// Returns ordinary duplicate-Service or inactive-owner failures.
    pub fn install(
        context: &Context,
        dispatcher: Arc<dyn SandboxServiceDispatcher>,
    ) -> Result<Arc<Self>, seekdeep_cordis::CordisError> {
        let registry = Self::new(dispatcher);
        context.provide(SANDBOX_SERVICES, registry.clone())?;
        Ok(registry)
    }

    /// Registers one exact adapter as a reversible effect.
    ///
    /// # Errors
    ///
    /// Rejects empty names, duplicate adapters, and inactive owners.
    pub fn register(
        &self,
        owner: &Context,
        registration: SandboxServiceRegistration,
    ) -> anyhow::Result<EffectHandle> {
        anyhow::ensure!(
            !registration.name.trim().is_empty(),
            "sandbox Service adapter name must not be empty"
        );
        let (name, generation) = {
            let mut state = self.state.lock();
            anyhow::ensure!(
                !state.registrations.contains_key(&registration.name),
                "sandbox Service adapter {:?} is already registered",
                registration.name
            );
            state.next_generation += 1;
            let generation = state.next_generation;
            let name = registration.name.clone();
            state.registrations.insert(
                name.clone(),
                StoredRegistration {
                    generation,
                    registration,
                },
            );
            (name, generation)
        };
        let state = self.state.clone();
        let cleanup_name = name.clone();
        let effect =
            EffectHandle::synchronous(format!("sandboxServices.register({name:?})"), move || {
                let mut state = state.lock();
                if state
                    .registrations
                    .get(&cleanup_name)
                    .is_some_and(|stored| stored.generation == generation)
                {
                    state.registrations.shift_remove(&cleanup_name);
                }
                Ok(())
            });
        if let Err(error) = owner.own(effect.clone()) {
            let mut state = self.state.lock();
            if state
                .registrations
                .get(&name)
                .is_some_and(|stored| stored.generation == generation)
            {
                state.registrations.shift_remove(&name);
            }
            return Err(error.into());
        }
        Ok(effect)
    }

    /// Snapshots every adapter in registration order.
    #[must_use]
    pub fn list(&self) -> Vec<SandboxServiceRegistration> {
        self.state
            .lock()
            .registrations
            .values()
            .map(|stored| stored.registration.clone())
            .collect()
    }

    pub(crate) fn call(
        &self,
        registration: &SandboxServiceRegistration,
        method: &str,
        args: Vec<Value>,
    ) -> anyhow::Result<Value> {
        let callback = registration.methods.get(method).ok_or_else(|| {
            anyhow::anyhow!(
                "sandbox Service {:?} exposes no method {:?}",
                registration.name,
                method
            )
        })?;
        self.dispatcher.dispatch(callback(args))
    }
}
