//! Runtime registry for generated Typert reflection and Remote dependencies.

pub mod invariant;
pub mod types;

use std::{
    collections::{HashMap, HashSet},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
};

use indexmap::{IndexMap, IndexSet};
use parking_lot::Mutex;
use seekdeep_cordis::{Context, ServiceKey, fiber::EffectHandle};
use seekdeep_typert_protocol::{
    InvocationDescriptor, InvocationParameterSource, InvocationReceiver, TypertClientContextBinder,
    TypertContextRegistry as TypertContextRegistryContract, TypertDisposer,
    TypertHostContextProvider, TypertHostContextResolver,
    TypertLocalRegistry as TypertLocalRegistryContract, TypertLookupDefinition,
    TypertLookupProvider, TypertLookupRegistry as TypertLookupRegistryContract,
    TypertLookupResolver, TypertRegistryChange, TypertRegistryChangeKind, TypertRegistryContract,
    TypertRegistryListener, TypertRemoteContribution,
    TypertRemoteRegistry as TypertRemoteRegistryContract, is_typert_remote_segment,
};
use serde_json::Value;
use uuid::Uuid;

pub use types::*;

/// Typed Cordis slot corresponding to `ctx.typert`.
pub const TYPERT: ServiceKey<TypertRegistry> = ServiceKey::new("typert");

/// Compose the global key of one generated schema.
#[must_use]
pub fn typert_key(package_name: &str, name: &str) -> String {
    format!("{package_name}#{name}")
}

/// Compose the identity of one package-face model.
#[must_use]
pub fn typert_package_key(package_name: &str, face: TypertFace) -> String {
    format!("{package_name}#{}", face.as_str())
}

/// Compose the endpoint key used by invocation registries.
#[must_use]
pub fn typert_endpoint(descriptor: &InvocationDescriptor) -> String {
    format!("{}/{}", descriptor.namespace, descriptor.method)
}

#[derive(Clone)]
struct DescriptorEntry {
    descriptor: InvocationDescriptor,
    owner: Uuid,
}

#[derive(Default)]
struct DescriptorState {
    entries: IndexMap<String, DescriptorEntry>,
    ids: HashMap<String, String>,
    history: IndexSet<String>,
}

impl DescriptorState {
    fn validate(&self, kind: &str, descriptors: &[InvocationDescriptor]) -> anyhow::Result<()> {
        let mut endpoints = HashSet::new();
        let mut ids = HashSet::new();
        for descriptor in descriptors {
            validate_invocation(descriptor)?;
            let endpoint = typert_endpoint(descriptor);
            anyhow::ensure!(
                endpoints.insert(endpoint.clone()) && !self.entries.contains_key(&endpoint),
                "typert: {kind} endpoint {endpoint:?} is already registered"
            );
            anyhow::ensure!(
                ids.insert(descriptor.id.clone()) && !self.ids.contains_key(&descriptor.id),
                "typert: {kind} invocation id {:?} is already registered",
                descriptor.id
            );
        }
        Ok(())
    }

    fn commit(&mut self, owner: Uuid, descriptors: &[InvocationDescriptor]) {
        for descriptor in descriptors {
            let endpoint = typert_endpoint(descriptor);
            self.ids.insert(descriptor.id.clone(), endpoint.clone());
            self.history.insert(endpoint.clone());
            self.entries.insert(
                endpoint,
                DescriptorEntry {
                    descriptor: descriptor.clone(),
                    owner,
                },
            );
        }
    }

    fn withdraw(&mut self, owner: Uuid, descriptors: &[InvocationDescriptor]) -> Vec<String> {
        let mut removed = Vec::new();
        for descriptor in descriptors {
            let endpoint = typert_endpoint(descriptor);
            if self
                .entries
                .get(&endpoint)
                .is_some_and(|entry| entry.owner == owner)
            {
                self.entries.shift_remove(&endpoint);
                if self.ids.get(&descriptor.id) == Some(&endpoint) {
                    self.ids.remove(&descriptor.id);
                }
                removed.push(endpoint);
            }
        }
        removed
    }
}

struct Owned<T> {
    value: T,
    owner: Uuid,
}

#[derive(Default)]
struct State {
    schemas: IndexMap<String, Owned<TypertSchemaRecord>>,
    packages: IndexMap<String, Owned<TypertPackageRecord>>,
    local: DescriptorState,
    remote: DescriptorState,
    remote_packages: HashMap<String, Uuid>,
    lookup_providers: IndexMap<String, Owned<TypertLookupProvider>>,
    lookup_resolvers: HashMap<String, Owned<TypertLookupResolver>>,
    lookup_definitions: IndexMap<String, TypertLookupDefinition>,
    context_hosts: HashMap<String, Owned<TypertHostContextProvider>>,
    context_host_resolvers: HashMap<String, Owned<TypertHostContextResolver>>,
    context_clients: HashMap<String, Owned<TypertClientContextBinder>>,
}

#[derive(Default)]
struct ChangeSource {
    listeners: Mutex<IndexMap<Uuid, TypertRegistryListener>>,
}

impl ChangeSource {
    fn subscribe(
        self: &Arc<Self>,
        context: &Context,
        listener: TypertRegistryListener,
    ) -> anyhow::Result<EffectHandle> {
        let id = Uuid::now_v7();
        self.listeners.lock().insert(id, listener);
        let source = self.clone();
        let effect = EffectHandle::synchronous("typert registry subscription", move || {
            source.listeners.lock().shift_remove(&id);
            Ok(())
        });
        match context.own(effect.clone()) {
            Ok(effect) => Ok(effect),
            Err(error) => {
                self.listeners.lock().shift_remove(&id);
                Err(error.into())
            }
        }
    }

    fn emit(&self, change: &TypertRegistryChange) {
        let listeners = self.listeners.lock().values().cloned().collect::<Vec<_>>();
        for listener in listeners {
            if let Err(panic) = catch_unwind(AssertUnwindSafe(|| listener(change.clone()))) {
                tracing::warn!(
                    kind = ?change.kind,
                    key = %change.key,
                    error = %panic_message(&panic),
                    "typert registry observer failed"
                );
            }
        }
    }
}

#[derive(Default)]
struct Changes {
    local: Arc<ChangeSource>,
    remote: Arc<ChangeSource>,
    lookup: Arc<ChangeSource>,
    contexts: Arc<ChangeSource>,
}

struct RegistryCore {
    state: Mutex<State>,
    changes: Changes,
}

impl Default for RegistryCore {
    fn default() -> Self {
        Self {
            state: Mutex::new(State::default()),
            changes: Changes::default(),
        }
    }
}

/// Registry of generated schemas, reflection, invocations, and providers.
pub struct TypertRegistry {
    core: Arc<RegistryCore>,
    local: TypertLocalRegistry,
    remotes: TypertRemoteRegistry,
    lookups: TypertLookupRegistry,
    contexts: TypertContextRegistry,
}

impl std::fmt::Debug for TypertRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TypertRegistry")
            .finish_non_exhaustive()
    }
}

impl TypertRegistry {
    /// Constructs an unprovided empty registry.
    #[must_use]
    pub fn new() -> Arc<Self> {
        let core = Arc::new(RegistryCore::default());
        Arc::new(Self {
            local: TypertLocalRegistry { core: core.clone() },
            remotes: TypertRemoteRegistry { core: core.clone() },
            lookups: TypertLookupRegistry { core: core.clone() },
            contexts: TypertContextRegistry { core: core.clone() },
            core,
        })
    }

    /// Publishes this exact registry on `ctx.typert`.
    ///
    /// # Errors
    ///
    /// Returns duplicate-service or inactive-owner failures.
    pub fn provide(
        self: &Arc<Self>,
        context: &Context,
    ) -> Result<EffectHandle, seekdeep_cordis::CordisError> {
        context.provide(TYPERT, self.clone())
    }

    /// Registers generated schemas, reflection, and local invocations atomically.
    ///
    /// # Errors
    ///
    /// Rejects malformed or duplicate identities and inactive ownership.
    pub fn register(
        &self,
        context: &Context,
        contribution: &TypertContribution,
    ) -> anyhow::Result<TypertDisposer> {
        validate_segment("package name", &contribution.package)?;
        let package_key = typert_package_key(&contribution.package, contribution.face);
        let owner = Uuid::now_v7();
        let package_record = TypertPackageRecord {
            package: contribution.package.clone(),
            face: contribution.face,
            key: package_key.clone(),
            model: contribution.model.clone(),
        };
        let schema_records = make_schema_records(contribution)?;
        {
            let mut state = self.core.state.lock();
            anyhow::ensure!(
                !state.packages.contains_key(&package_key),
                "typert: package face {package_key:?} is already registered"
            );
            let mut batch = HashSet::new();
            for record in &schema_records {
                anyhow::ensure!(
                    batch.insert(record.key.clone()) && !state.schemas.contains_key(&record.key),
                    "typert: schema {:?} is already registered",
                    record.key
                );
            }
            state.local.validate("local", &contribution.invocations)?;
            state.packages.insert(
                package_key.clone(),
                Owned {
                    value: package_record,
                    owner,
                },
            );
            for record in &schema_records {
                state.schemas.insert(
                    record.key.clone(),
                    Owned {
                        value: record.clone(),
                        owner,
                    },
                );
            }
            state.local.commit(owner, &contribution.invocations);
        }
        let core = self.core.clone();
        let invocations = contribution.invocations.clone();
        let schema_keys = schema_records
            .iter()
            .map(|record| record.key.clone())
            .collect::<Vec<_>>();
        let effect = EffectHandle::synchronous("typert.register()", move || {
            let removed = {
                let mut state = core.state.lock();
                if state
                    .packages
                    .get(&package_key)
                    .is_some_and(|entry| entry.owner == owner)
                {
                    state.packages.shift_remove(&package_key);
                }
                for key in &schema_keys {
                    if state
                        .schemas
                        .get(key)
                        .is_some_and(|entry| entry.owner == owner)
                    {
                        state.schemas.shift_remove(key);
                    }
                }
                state.local.withdraw(owner, &invocations)
            };
            emit_keys(
                &core.changes.local,
                TypertRegistryChangeKind::Local,
                removed,
            );
            Ok(())
        });
        if let Err(error) = context.own(effect.clone()) {
            rollback_contribution(
                &self.core,
                owner,
                &package_key_for(contribution),
                &schema_records,
                &contribution.invocations,
            );
            return Err(error.into());
        }
        emit_descriptors(
            &self.core.changes.local,
            TypertRegistryChangeKind::Local,
            &contribution.invocations,
        );
        Ok(effect)
    }

    /// Looks up one live schema record.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<TypertSchemaRecord> {
        self.core
            .state
            .lock()
            .schemas
            .get(key)
            .map(|entry| entry.value.clone())
    }

    /// Resolves one required schema record.
    ///
    /// # Errors
    ///
    /// Distinguishes malformed keys, absent packages, and absent schema names.
    pub fn resolve(&self, key: &str) -> anyhow::Result<TypertSchemaRecord> {
        if let Some(record) = self.get(key) {
            return Ok(record);
        }
        let Some((package, name)) = key.split_once('#') else {
            anyhow::bail!("typert: invalid schema key {key:?} — expected \"<package>#<name>\"");
        };
        anyhow::ensure!(
            !package.is_empty() && !name.is_empty(),
            "typert: invalid schema key {key:?} — expected \"<package>#<name>\""
        );
        let state = self.core.state.lock();
        if state
            .packages
            .values()
            .any(|candidate| candidate.value.package == package)
        {
            anyhow::bail!(
                "typert: cannot resolve {key:?} — package {package:?} is registered but contributes no schema named {name:?}"
            );
        }
        anyhow::bail!(
            "typert: cannot resolve {key:?} — package {package:?} has no registered contribution"
        )
    }

    /// Enumerates live schemas in registration order.
    #[must_use]
    pub fn list(&self, filter: &TypertSchemaFilter) -> Vec<TypertSchemaRecord> {
        self.core
            .state
            .lock()
            .schemas
            .values()
            .filter(|entry| matches_filter(&entry.value.package, entry.value.face, filter))
            .map(|entry| entry.value.clone())
            .collect()
    }

    /// Looks up generated reflection for one package face.
    #[must_use]
    pub fn get_package(&self, package: &str, face: TypertFace) -> Option<TypertPackageRecord> {
        self.core
            .state
            .lock()
            .packages
            .get(&typert_package_key(package, face))
            .map(|entry| entry.value.clone())
    }

    /// Enumerates live package reflection in registration order.
    #[must_use]
    pub fn list_packages(&self, filter: &TypertPackageFilter) -> Vec<TypertPackageRecord> {
        self.core
            .state
            .lock()
            .packages
            .values()
            .filter(|entry| matches_filter(&entry.value.package, entry.value.face, filter))
            .map(|entry| entry.value.clone())
            .collect()
    }

    /// Projects a fresh JSON Schema document.
    ///
    /// # Errors
    ///
    /// Returns schema resolution or projection diagnostics.
    pub fn to_json_schema(&self, key: &str) -> anyhow::Result<Value> {
        self.resolve(key)?.schema.to_json_schema()
    }

    /// Current-environment invocation view.
    #[must_use]
    pub const fn local(&self) -> &TypertLocalRegistry {
        &self.local
    }

    /// Consumer-selected Remote contribution view.
    #[must_use]
    pub const fn remotes(&self) -> &TypertRemoteRegistry {
        &self.remotes
    }

    /// Host object lookup view.
    #[must_use]
    pub const fn lookups(&self) -> &TypertLookupRegistry {
        &self.lookups
    }

    /// Host and Client Context provider view.
    #[must_use]
    pub const fn contexts(&self) -> &TypertContextRegistry {
        &self.contexts
    }
}

/// Installs and publishes a Host or Client registry implementation.
///
/// # Errors
///
/// Returns duplicate-service or inactive-owner failures.
pub fn install(context: &Context) -> anyhow::Result<Arc<TypertRegistry>> {
    let registry = TypertRegistry::new();
    registry.provide(context)?;
    Ok(registry)
}

/// Current-environment invocation registry view.
#[derive(Clone)]
pub struct TypertLocalRegistry {
    core: Arc<RegistryCore>,
}

impl TypertLocalRegistryContract for TypertLocalRegistry {
    fn get(&self, endpoint: &str) -> Option<InvocationDescriptor> {
        self.core
            .state
            .lock()
            .local
            .entries
            .get(endpoint)
            .map(|entry| entry.descriptor.clone())
    }

    fn has_seen(&self, endpoint: &str) -> bool {
        self.core.state.lock().local.history.contains(endpoint)
    }

    fn list(&self) -> Vec<InvocationDescriptor> {
        self.core
            .state
            .lock()
            .local
            .entries
            .values()
            .map(|entry| entry.descriptor.clone())
            .collect()
    }

    fn subscribe(
        &self,
        context: &Context,
        listener: TypertRegistryListener,
    ) -> anyhow::Result<TypertDisposer> {
        self.core.changes.local.subscribe(context, listener)
    }
}

/// Consumer-selected Remote invocation registry view.
#[derive(Clone)]
pub struct TypertRemoteRegistry {
    core: Arc<RegistryCore>,
}

impl TypertRemoteRegistryContract for TypertRemoteRegistry {
    fn register(
        &self,
        context: &Context,
        contribution: TypertRemoteContribution,
    ) -> anyhow::Result<TypertDisposer> {
        validate_segment("Remote package name", &contribution.package)?;
        let owner = Uuid::now_v7();
        {
            let mut state = self.core.state.lock();
            anyhow::ensure!(
                !state.remote_packages.contains_key(&contribution.package),
                "typert: Remote package {:?} is already registered",
                contribution.package
            );
            state.remote.validate("remote", &contribution.descriptors)?;
            state
                .remote_packages
                .insert(contribution.package.clone(), owner);
            state.remote.commit(owner, &contribution.descriptors);
        }
        let core = self.core.clone();
        let package = contribution.package.clone();
        let descriptors = contribution.descriptors.clone();
        let effect =
            EffectHandle::synchronous(format!("typert.remotes.register({package:?})"), move || {
                let removed = {
                    let mut state = core.state.lock();
                    if state.remote_packages.get(&package) == Some(&owner) {
                        state.remote_packages.remove(&package);
                    }
                    state.remote.withdraw(owner, &descriptors)
                };
                emit_keys(
                    &core.changes.remote,
                    TypertRegistryChangeKind::Remote,
                    removed,
                );
                Ok(())
            });
        if let Err(error) = context.own(effect.clone()) {
            rollback_remote(&self.core, owner, &contribution);
            return Err(error.into());
        }
        emit_descriptors(
            &self.core.changes.remote,
            TypertRegistryChangeKind::Remote,
            &contribution.descriptors,
        );
        Ok(effect)
    }

    fn get(&self, endpoint: &str) -> Option<InvocationDescriptor> {
        self.core
            .state
            .lock()
            .remote
            .entries
            .get(endpoint)
            .map(|entry| entry.descriptor.clone())
    }

    fn list(&self) -> Vec<InvocationDescriptor> {
        self.core
            .state
            .lock()
            .remote
            .entries
            .values()
            .map(|entry| entry.descriptor.clone())
            .collect()
    }

    fn subscribe(
        &self,
        context: &Context,
        listener: TypertRegistryListener,
    ) -> anyhow::Result<TypertDisposer> {
        self.core.changes.remote.subscribe(context, listener)
    }
}

/// Runtime Host object lookup registry view.
#[derive(Clone)]
pub struct TypertLookupRegistry {
    core: Arc<RegistryCore>,
}

impl TypertLookupRegistryContract for TypertLookupRegistry {
    fn register(
        &self,
        context: &Context,
        key: &str,
        provider: TypertLookupProvider,
    ) -> anyhow::Result<TypertDisposer> {
        validate_segment("lookup key", key)?;
        validate_wire_name("lookup parameter", &provider.parameter)?;
        validate_wire_name("lookup wire field", &provider.wire)?;
        validate_nonempty("lookup Host type symbol", &provider.host_type_symbol)?;
        validate_nonempty("lookup wire type symbol", &provider.wire_type_symbol)?;
        let definition = TypertLookupDefinition {
            key: key.to_owned(),
            parameter: provider.parameter.clone(),
            wire: provider.wire.clone(),
            host_type_symbol: provider.host_type_symbol.clone(),
            wire_type_symbol: provider.wire_type_symbol.clone(),
        };
        let owner = Uuid::now_v7();
        {
            let mut state = self.core.state.lock();
            anyhow::ensure!(
                !state.lookup_providers.contains_key(key),
                "typert: lookup {key:?} is already registered"
            );
            if let Some(known) = state.lookup_definitions.get(key) {
                anyhow::ensure!(
                    known == &definition,
                    "typert: lookup {key:?} changed its wire declaration during this registry lifetime"
                );
            }
            state.lookup_definitions.insert(key.to_owned(), definition);
            state.lookup_providers.insert(
                key.to_owned(),
                Owned {
                    value: provider,
                    owner,
                },
            );
        }
        owned_provider_effect(
            context,
            &self.core,
            key,
            owner,
            TypertRegistryChangeKind::Lookup,
            ProviderTable::Lookup,
        )
    }

    fn configure(
        &self,
        context: &Context,
        key: &str,
        resolver: TypertLookupResolver,
    ) -> anyhow::Result<TypertDisposer> {
        validate_segment("lookup key", key)?;
        let owner = Uuid::now_v7();
        {
            let mut state = self.core.state.lock();
            anyhow::ensure!(
                !state.lookup_resolvers.contains_key(key),
                "typert: lookup {key:?} resolver is already configured"
            );
            state.lookup_resolvers.insert(
                key.to_owned(),
                Owned {
                    value: resolver,
                    owner,
                },
            );
        }
        owned_provider_effect(
            context,
            &self.core,
            key,
            owner,
            TypertRegistryChangeKind::Lookup,
            ProviderTable::LookupResolver,
        )
    }

    fn get(&self, key: &str) -> Option<TypertLookupProvider> {
        let state = self.core.state.lock();
        let provider = state.lookup_providers.get(key)?.value.clone();
        let Some(resolver) = state.lookup_resolvers.get(key) else {
            return Some(provider);
        };
        Some(TypertLookupProvider {
            resolve: resolver.value.clone(),
            ..provider
        })
    }

    fn definitions(&self) -> Vec<TypertLookupDefinition> {
        self.core
            .state
            .lock()
            .lookup_definitions
            .values()
            .cloned()
            .collect()
    }

    fn keys(&self) -> Vec<String> {
        self.core
            .state
            .lock()
            .lookup_providers
            .keys()
            .cloned()
            .collect()
    }

    fn subscribe(
        &self,
        context: &Context,
        listener: TypertRegistryListener,
    ) -> anyhow::Result<TypertDisposer> {
        self.core.changes.lookup.subscribe(context, listener)
    }
}

/// Runtime Host and Client Context provider registry view.
#[derive(Clone)]
pub struct TypertContextRegistry {
    core: Arc<RegistryCore>,
}

impl TypertContextRegistryContract for TypertContextRegistry {
    fn register_host(
        &self,
        context: &Context,
        key: &str,
        provider: TypertHostContextProvider,
    ) -> anyhow::Result<TypertDisposer> {
        validate_segment("Context key", key)?;
        validate_wire_name("Context wire field", &provider.wire)?;
        validate_nonempty("Context wire type symbol", &provider.wire_type_symbol)?;
        register_context_provider(
            context,
            &self.core,
            key,
            owner_host(provider),
            ProviderTable::ContextHost,
            "host-context",
        )
    }

    fn configure_host(
        &self,
        context: &Context,
        key: &str,
        resolver: TypertHostContextResolver,
    ) -> anyhow::Result<TypertDisposer> {
        validate_segment("Context key", key)?;
        register_context_provider(
            context,
            &self.core,
            key,
            ContextProviderValue::HostResolver(resolver),
            ProviderTable::ContextHostResolver,
            "host-context resolver",
        )
    }

    fn register_client(
        &self,
        context: &Context,
        key: &str,
        binder: TypertClientContextBinder,
    ) -> anyhow::Result<TypertDisposer> {
        validate_segment("Context key", key)?;
        register_context_provider(
            context,
            &self.core,
            key,
            ContextProviderValue::Client(binder),
            ProviderTable::ContextClient,
            "client-context",
        )
    }

    fn get_host(&self, key: &str) -> Option<TypertHostContextProvider> {
        let state = self.core.state.lock();
        let provider = state.context_hosts.get(key)?.value.clone();
        let Some(resolver) = state.context_host_resolvers.get(key) else {
            return Some(provider);
        };
        Some(TypertHostContextProvider {
            resolve: resolver.value.clone(),
            ..provider
        })
    }

    fn get_client(&self, key: &str) -> Option<TypertClientContextBinder> {
        self.core
            .state
            .lock()
            .context_clients
            .get(key)
            .map(|entry| entry.value.clone())
    }

    fn subscribe(
        &self,
        context: &Context,
        listener: TypertRegistryListener,
    ) -> anyhow::Result<TypertDisposer> {
        self.core.changes.contexts.subscribe(context, listener)
    }
}

impl TypertRegistryContract for TypertRegistry {
    fn local(&self) -> &dyn TypertLocalRegistryContract {
        &self.local
    }

    fn remotes(&self) -> &dyn TypertRemoteRegistryContract {
        &self.remotes
    }

    fn lookups(&self) -> &dyn TypertLookupRegistryContract {
        &self.lookups
    }

    fn contexts(&self) -> &dyn TypertContextRegistryContract {
        &self.contexts
    }
}

#[derive(Clone, Copy)]
enum ProviderTable {
    Lookup,
    LookupResolver,
    ContextHost,
    ContextHostResolver,
    ContextClient,
}

fn owned_provider_effect(
    context: &Context,
    core: &Arc<RegistryCore>,
    key: &str,
    owner: Uuid,
    kind: TypertRegistryChangeKind,
    table: ProviderTable,
) -> anyhow::Result<EffectHandle> {
    let key = key.to_owned();
    let disposal_key = key.clone();
    let disposal_core = core.clone();
    let effect = EffectHandle::synchronous("typert provider registration", move || {
        if remove_provider(&disposal_core, &disposal_key, owner, table) {
            emit_change(&disposal_core, kind, disposal_key);
        }
        Ok(())
    });
    match context.own(effect.clone()) {
        Ok(effect) => {
            emit_change(core, kind, key);
            Ok(effect)
        }
        Err(error) => {
            remove_provider(core, &key, owner, table);
            Err(error.into())
        }
    }
}

enum ContextProviderValue {
    Host(TypertHostContextProvider),
    HostResolver(TypertHostContextResolver),
    Client(TypertClientContextBinder),
}

fn owner_host(provider: TypertHostContextProvider) -> ContextProviderValue {
    ContextProviderValue::Host(provider)
}

fn register_context_provider(
    context: &Context,
    core: &Arc<RegistryCore>,
    key: &str,
    value: ContextProviderValue,
    table: ProviderTable,
    label: &str,
) -> anyhow::Result<EffectHandle> {
    let owner = Uuid::now_v7();
    {
        let mut state = core.state.lock();
        match value {
            ContextProviderValue::Host(provider) => {
                anyhow::ensure!(
                    !state.context_hosts.contains_key(key),
                    "typert: host-context provider {key:?} is already registered"
                );
                state.context_hosts.insert(
                    key.to_owned(),
                    Owned {
                        value: provider,
                        owner,
                    },
                );
            }
            ContextProviderValue::HostResolver(resolver) => {
                anyhow::ensure!(
                    !state.context_host_resolvers.contains_key(key),
                    "typert: host-context {key:?} resolver is already configured"
                );
                state.context_host_resolvers.insert(
                    key.to_owned(),
                    Owned {
                        value: resolver,
                        owner,
                    },
                );
            }
            ContextProviderValue::Client(binder) => {
                anyhow::ensure!(
                    !state.context_clients.contains_key(key),
                    "typert: client-context provider {key:?} is already registered"
                );
                state.context_clients.insert(
                    key.to_owned(),
                    Owned {
                        value: binder,
                        owner,
                    },
                );
            }
        }
    }
    let effect = owned_provider_effect(
        context,
        core,
        key,
        owner,
        match table {
            ProviderTable::ContextClient => TypertRegistryChangeKind::ClientContext,
            ProviderTable::ContextHost | ProviderTable::ContextHostResolver => {
                TypertRegistryChangeKind::HostContext
            }
            ProviderTable::Lookup | ProviderTable::LookupResolver => unreachable!(),
        },
        table,
    )?;
    let _ = label;
    Ok(effect)
}

fn remove_provider(core: &RegistryCore, key: &str, owner: Uuid, table: ProviderTable) -> bool {
    let mut state = core.state.lock();
    match table {
        ProviderTable::Lookup => remove_owned(&mut state.lookup_providers, key, owner),
        ProviderTable::LookupResolver => remove_hash_owned(&mut state.lookup_resolvers, key, owner),
        ProviderTable::ContextHost => remove_hash_owned(&mut state.context_hosts, key, owner),
        ProviderTable::ContextHostResolver => {
            remove_hash_owned(&mut state.context_host_resolvers, key, owner)
        }
        ProviderTable::ContextClient => remove_hash_owned(&mut state.context_clients, key, owner),
    }
}

fn remove_owned<T>(table: &mut IndexMap<String, Owned<T>>, key: &str, owner: Uuid) -> bool {
    if table.get(key).is_some_and(|entry| entry.owner == owner) {
        table.shift_remove(key);
        true
    } else {
        false
    }
}

fn remove_hash_owned<T>(table: &mut HashMap<String, Owned<T>>, key: &str, owner: Uuid) -> bool {
    if table.get(key).is_some_and(|entry| entry.owner == owner) {
        table.remove(key);
        true
    } else {
        false
    }
}

fn emit_change(core: &RegistryCore, kind: TypertRegistryChangeKind, key: String) {
    let source = match kind {
        TypertRegistryChangeKind::Local => &core.changes.local,
        TypertRegistryChangeKind::Remote => &core.changes.remote,
        TypertRegistryChangeKind::Lookup => &core.changes.lookup,
        TypertRegistryChangeKind::HostContext | TypertRegistryChangeKind::ClientContext => {
            &core.changes.contexts
        }
    };
    source.emit(&TypertRegistryChange { kind, key });
}

fn emit_keys(source: &ChangeSource, kind: TypertRegistryChangeKind, keys: Vec<String>) {
    for key in keys {
        source.emit(&TypertRegistryChange { kind, key });
    }
}

fn emit_descriptors(
    source: &ChangeSource,
    kind: TypertRegistryChangeKind,
    descriptors: &[InvocationDescriptor],
) {
    emit_keys(
        source,
        kind,
        descriptors.iter().map(typert_endpoint).collect(),
    );
}

fn rollback_contribution(
    core: &RegistryCore,
    owner: Uuid,
    package_key: &str,
    schemas: &[TypertSchemaRecord],
    descriptors: &[InvocationDescriptor],
) {
    let mut state = core.state.lock();
    if state
        .packages
        .get(package_key)
        .is_some_and(|entry| entry.owner == owner)
    {
        state.packages.shift_remove(package_key);
    }
    for schema in schemas {
        if state
            .schemas
            .get(&schema.key)
            .is_some_and(|entry| entry.owner == owner)
        {
            state.schemas.shift_remove(&schema.key);
        }
    }
    state.local.withdraw(owner, descriptors);
}

fn make_schema_records(
    contribution: &TypertContribution,
) -> anyhow::Result<Vec<TypertSchemaRecord>> {
    contribution
        .schemas
        .iter()
        .map(|schema| {
            validate_segment("schema name", &schema.name)?;
            Ok(TypertSchemaRecord {
                package: contribution.package.clone(),
                face: contribution.face,
                name: schema.name.clone(),
                key: typert_key(&contribution.package, &schema.name),
                schema: schema.schema.clone(),
            })
        })
        .collect()
}

fn package_key_for(contribution: &TypertContribution) -> String {
    typert_package_key(&contribution.package, contribution.face)
}

fn rollback_remote(core: &RegistryCore, owner: Uuid, contribution: &TypertRemoteContribution) {
    let mut state = core.state.lock();
    if state.remote_packages.get(&contribution.package) == Some(&owner) {
        state.remote_packages.remove(&contribution.package);
    }
    state.remote.withdraw(owner, &contribution.descriptors);
}

fn matches_filter(package: &str, face: TypertFace, filter: &TypertSchemaFilter) -> bool {
    filter
        .package
        .as_deref()
        .is_none_or(|expected| expected == package)
        && filter.face.is_none_or(|expected| expected == face)
}

fn validate_invocation(descriptor: &InvocationDescriptor) -> anyhow::Result<()> {
    validate_nonempty("invocation id", &descriptor.id)?;
    validate_segment("invocation service key", &descriptor.service)?;
    validate_wire_name("invocation namespace", &descriptor.namespace)?;
    validate_wire_name("invocation method", &descriptor.method)?;
    if let Some(implementation) = &descriptor.implementation {
        validate_wire_name("invocation implementation method", implementation)?;
    }
    validate_codec(&descriptor.result, &format!("{} result", descriptor.id))?;
    let mut wires = HashSet::new();
    for parameter in &descriptor.parameters {
        validate_wire_name("parameter name", &parameter.name)?;
        validate_wire_name("parameter wire field", &parameter.wire)?;
        anyhow::ensure!(
            wires.insert(parameter.wire.clone()),
            "typert: invocation {:?} repeats wire field {:?}",
            descriptor.id,
            parameter.wire
        );
        match parameter.source {
            InvocationParameterSource::Lookup => {
                anyhow::ensure!(
                    parameter.accepts_undefined.is_none(),
                    "typert: invocation {:?} lookup parameter {:?} cannot accept undefined",
                    descriptor.id,
                    parameter.name
                );
                let lookup = parameter.lookup.as_deref().ok_or_else(|| {
                    anyhow::anyhow!(
                        "typert: invocation {:?} lookup parameter {:?} has no lookup key",
                        descriptor.id,
                        parameter.name
                    )
                })?;
                validate_segment("lookup key", lookup)?;
            }
            InvocationParameterSource::Json => anyhow::ensure!(
                parameter.lookup.is_none(),
                "typert: invocation {:?} JSON parameter {:?} declares a lookup key",
                descriptor.id,
                parameter.name
            ),
        }
        validate_codec(
            &parameter.codec,
            &format!("{} parameter {}", descriptor.id, parameter.name),
        )?;
    }
    if let Some(scope) = &descriptor.scope {
        anyhow::ensure!(
            matches!(descriptor.invocation, InvocationReceiver::Direct),
            "typert: invocation {:?} Context receiver cannot declare a direct scope projection",
            descriptor.id
        );
        validate_segment("scope Context key", &scope.context)?;
        validate_wire_name("scope wire field", &scope.wire)?;
        let lookups = descriptor
            .parameters
            .iter()
            .filter(|parameter| parameter.source == InvocationParameterSource::Lookup)
            .collect::<Vec<_>>();
        anyhow::ensure!(
            lookups.len() == 1
                && lookups[0].wire == scope.wire
                && lookups[0].lookup.as_deref() == Some(scope.context.as_str()),
            "typert: invocation {:?} scope wire {:?} must select its only lookup parameter",
            descriptor.id,
            scope.wire
        );
    }
    if let InvocationReceiver::Context {
        context,
        wire,
        codec,
    } = &descriptor.invocation
    {
        validate_segment("Context key", context)?;
        validate_wire_name("Context wire field", wire)?;
        anyhow::ensure!(
            wires.insert(wire.clone()),
            "typert: invocation {:?} repeats wire field {wire:?}",
            descriptor.id
        );
        validate_codec(codec, &format!("{} Context", descriptor.id))?;
    }
    Ok(())
}

fn validate_codec(
    codec: &seekdeep_typert_protocol::TypertCodec,
    subject: &str,
) -> anyhow::Result<()> {
    if let seekdeep_typert_protocol::TypertCodec::Strict { type_symbol, .. } = codec {
        validate_nonempty(&format!("{subject} type symbol"), type_symbol)?;
    }
    Ok(())
}

fn validate_wire_name(subject: &str, value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        is_typert_remote_segment(value),
        "typert: invalid {subject} {value:?} — must contain only RPC endpoint segment characters"
    );
    Ok(())
}

fn validate_segment(subject: &str, value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !value.is_empty() && !value.contains('#'),
        "typert: invalid {subject} {value:?} — must be nonempty and must not contain \"#\""
    );
    Ok(())
}

fn validate_nonempty(subject: &str, value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !value.is_empty(),
        "typert: invalid {subject} — must be nonempty"
    );
    Ok(())
}

fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    panic
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| {
            panic
                .downcast_ref::<&str>()
                .map(|message| (*message).to_owned())
        })
        .unwrap_or_else(|| "<non-string panic>".to_owned())
}
