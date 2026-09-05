//! Client projection of generated Typert Remote descriptors.

use std::{
    collections::{HashMap, HashSet},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

use futures::{FutureExt, future::BoxFuture};
use indexmap::IndexMap;
use parking_lot::Mutex;
use seekdeep_client_connection::RpcResult;
pub use seekdeep_client_connection::{
    CLIENT_CONNECTION, ClientConnection, ClientConnectionFuture, ClientConnectionHandle,
};
use seekdeep_cordis::{Context, ServiceKey, fiber::EffectHandle};
use seekdeep_llm::AbortSignal;
use seekdeep_typert_protocol::{
    InvocationDescriptor, InvocationParameterSource, InvocationReceiver, RemoteFailure,
    RemoteResult, TypertBoundaryValue, TypertClientRemote, TypertCodec, TypertContextRegistry as _,
    TypertRemoteContribution, TypertRemoteEventListener, TypertRemoteRegistry as _,
};
use seekdeep_typert_registry::{TYPERT, TypertRegistry};
use serde_json::{Map, Value};
use uuid::Uuid;

/// Typed Cordis slot corresponding to Client `ctx.remote`.
pub const CLIENT_REMOTE: ServiceKey<ClientRemoteService> = ServiceKey::new("remote");

/// One raw argument accepted by a generated Client method.
#[derive(Clone, Debug)]
pub enum ClientRemoteArgument {
    /// JSON or declared `undefined` business value.
    Boundary(TypertBoundaryValue),
    /// Optional final cancellation slot; `None` is explicit JavaScript `undefined`.
    Signal(Option<AbortSignal>),
}

impl From<TypertBoundaryValue> for ClientRemoteArgument {
    fn from(value: TypertBoundaryValue) -> Self {
        Self::Boundary(value)
    }
}

#[derive(Clone)]
struct MountToken {
    active: Arc<AtomicBool>,
    abort: AbortSignal,
}

impl MountToken {
    fn new() -> Self {
        Self {
            active: Arc::new(AtomicBool::new(true)),
            abort: AbortSignal::default(),
        }
    }

    fn active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    fn withdraw(&self) -> bool {
        if self.active.swap(false, Ordering::AcqRel) {
            self.abort.abort();
            true
        } else {
            false
        }
    }
}

#[derive(Clone)]
struct ScopedProjection {
    context: String,
    wire: String,
    codec: TypertCodec,
    parameter_index: Option<usize>,
}

#[derive(Clone)]
struct DirectMethod {
    descriptor: InvocationDescriptor,
    token: MountToken,
}

#[derive(Clone)]
struct ScopedMethod {
    method: DirectMethod,
    projection: ScopedProjection,
}

#[derive(Clone, Default)]
struct RemoteMethodRecord {
    direct: Option<DirectMethod>,
    scoped: Option<ScopedMethod>,
}

#[derive(Clone, Copy)]
enum MethodKind {
    Direct,
    Scoped,
}

#[derive(Clone)]
struct InstalledVariant {
    namespace: String,
    method: String,
    kind: MethodKind,
    token: MountToken,
}

struct MountedDescriptor {
    token: MountToken,
    variants: Vec<InstalledVariant>,
}

#[derive(Clone)]
struct NamespaceHandle {
    service: Arc<RemoteNamespaceService>,
    provision: EffectHandle,
}

struct Subscription {
    id: Uuid,
    listener: TypertRemoteEventListener,
}

#[derive(Default)]
struct ClientState {
    namespaces: IndexMap<String, NamespaceHandle>,
    subscriptions: HashMap<String, Vec<Subscription>>,
}

struct ClientCore {
    owner_context: Context,
    registry: Arc<TypertRegistry>,
    state: Mutex<ClientState>,
    mutations: tokio::sync::Mutex<()>,
}

/// Concrete generated Client Remote service.
pub struct ClientRemoteService {
    core: Arc<ClientCore>,
}

impl std::fmt::Debug for ClientRemoteService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClientRemoteService")
            .field(
                "namespaces",
                &self.core.state.lock().namespaces.keys().collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

impl ClientRemoteService {
    /// Installs the Client face over live Typert and Connection services.
    ///
    /// # Errors
    ///
    /// Returns missing-dependency, duplicate-service, or inactive-owner failures.
    pub fn install(context: &Context) -> anyhow::Result<Arc<Self>> {
        let registry = context
            .get(TYPERT)
            .ok_or_else(|| anyhow::anyhow!("client api requires typert"))?;
        anyhow::ensure!(
            context.get(CLIENT_CONNECTION).is_some(),
            "client api requires connection"
        );
        let service = Arc::new(Self {
            core: Arc::new(ClientCore {
                owner_context: context.clone(),
                registry,
                state: Mutex::new(ClientState::default()),
                mutations: tokio::sync::Mutex::new(()),
            }),
        });
        context.provide(CLIENT_REMOTE, service.clone())?;
        let core = service.core.clone();
        context.own(EffectHandle::synchronous(
            "api-gateway.client.subscriptions",
            move || {
                core.state.lock().subscriptions.clear();
                Ok(())
            },
        ))?;
        Ok(service)
    }

    /// Returns the currently mounted concrete namespace service.
    #[must_use]
    pub fn namespace(&self, namespace: &str) -> Option<Arc<RemoteNamespaceService>> {
        self.core
            .state
            .lock()
            .namespaces
            .get(namespace)
            .map(|handle| handle.service.clone())
    }

    /// Binds one currently mounted generated method to its caller Context.
    ///
    /// # Errors
    ///
    /// Returns when the namespace or method is no longer mounted.
    pub fn method(
        &self,
        caller: &Context,
        namespace: &str,
        method: &str,
    ) -> anyhow::Result<ClientRemoteMethod> {
        let namespace = self.namespace(namespace).ok_or_else(|| {
            anyhow::anyhow!("client api: Remote method {namespace}/{method} is no longer mounted")
        })?;
        namespace.bind(caller, method)
    }

    /// Invokes one currently mounted generated method.
    ///
    /// # Errors
    ///
    /// Returns Client assembly, arity, binder, or input-boundary failures.
    pub async fn invoke(
        &self,
        caller: &Context,
        namespace: &str,
        method: &str,
        arguments: Vec<ClientRemoteArgument>,
    ) -> anyhow::Result<RemoteResult<TypertBoundaryValue>> {
        self.method(caller, namespace, method)?
            .invoke(arguments)
            .await
    }

    async fn mount_owned(
        &self,
        caller: &Context,
        contribution: TypertRemoteContribution,
    ) -> anyhow::Result<EffectHandle> {
        let _mutation = self.core.mutations.lock().await;
        validate_contribution(&self.core, &contribution)?;
        let remote = self
            .core
            .registry
            .remotes()
            .register(caller, contribution.clone())?;
        let mut mounted = Vec::new();
        for descriptor in contribution.descriptors {
            match install_descriptor(&self.core, &descriptor) {
                Ok(descriptor) => mounted.push(descriptor),
                Err(error) => {
                    cleanup_mount(&self.core, mounted, &remote).await;
                    return Err(error);
                }
            }
        }
        let core = self.core.clone();
        let cleanup_remote = remote.clone();
        let effect = EffectHandle::new(
            format!("api-gateway.client.$mount({:?})", contribution.package),
            move || {
                Box::pin(async move {
                    let _mutation = core.mutations.lock().await;
                    cleanup_mount(&core, mounted, &cleanup_remote).await;
                    Ok(())
                })
            },
        );
        if let Err(error) = caller.own(effect.clone()) {
            effect.dispose().await?;
            return Err(error.into());
        }
        Ok(effect)
    }

    fn subscribe(
        &self,
        caller: &Context,
        event: &str,
        listener: TypertRemoteEventListener,
    ) -> anyhow::Result<EffectHandle> {
        let id = Uuid::now_v7();
        self.core
            .state
            .lock()
            .subscriptions
            .entry(event.to_owned())
            .or_default()
            .push(Subscription { id, listener });
        let core = self.core.clone();
        let event = event.to_owned();
        let disposal_event = event.clone();
        let effect =
            EffectHandle::synchronous(format!("api-gateway.client.$on({event:?})"), move || {
                if let Some(listeners) = core.state.lock().subscriptions.get_mut(&disposal_event)
                    && let Some(index) = listeners.iter().position(|entry| entry.id == id)
                {
                    listeners.remove(index);
                }
                Ok(())
            });
        match caller.own(effect.clone()) {
            Ok(effect) => Ok(effect),
            Err(error) => {
                if let Some(listeners) =
                    self.core.state.lock().subscriptions.get_mut(event.as_str())
                    && let Some(index) = listeners.iter().position(|entry| entry.id == id)
                {
                    listeners.remove(index);
                }
                Err(error.into())
            }
        }
    }

    fn deliver(&self, event: &str, args: &[Value]) {
        let listeners = self
            .core
            .state
            .lock()
            .subscriptions
            .get(event)
            .map(|listeners| {
                listeners
                    .iter()
                    .map(|entry| entry.listener.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for listener in listeners {
            let invocation = catch_unwind(AssertUnwindSafe(|| listener(args.to_vec())));
            let event = event.to_owned();
            match invocation {
                Ok(future) => spawn_contained_listener(event, future),
                Err(panic) => tracing::error!(
                    event,
                    error = %panic_message(&panic),
                    "client api: Remote event listener threw"
                ),
            }
        }
    }
}

impl TypertClientRemote for ClientRemoteService {
    fn mount(
        &self,
        context: &Context,
        contribution: TypertRemoteContribution,
    ) -> seekdeep_typert_protocol::TypertRemoteMountFuture {
        let context = context.clone();
        let core = self.core.clone();
        Box::pin(async move {
            let service = Self { core };
            service.mount_owned(&context, contribution).await
        })
    }

    fn on(
        &self,
        context: &Context,
        event: &str,
        listener: TypertRemoteEventListener,
    ) -> anyhow::Result<EffectHandle> {
        self.subscribe(context, event, listener)
    }

    fn dispatch(&self, event: &str, args: Vec<Value>) {
        self.deliver(event, &args);
    }
}

fn spawn_contained_listener(event: String, future: BoxFuture<'static, anyhow::Result<()>>) {
    let contained = async move {
        match AssertUnwindSafe(future).catch_unwind().await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::error!(
                event,
                error = %error,
                "client api: Remote event listener threw"
            ),
            Err(panic) => tracing::error!(
                event,
                error = %panic_message(&panic),
                "client api: Remote event listener threw"
            ),
        }
    };
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn(contained);
    } else {
        std::thread::spawn(move || futures::executor::block_on(contained));
    }
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
        .unwrap_or_else(|| "non-string panic".to_owned())
}

/// Concrete namespace service containing generated methods only.
pub struct RemoteNamespaceService {
    namespace: String,
    core: Weak<ClientCore>,
    methods: Mutex<IndexMap<String, RemoteMethodRecord>>,
}

impl std::fmt::Debug for RemoteNamespaceService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemoteNamespaceService")
            .field("namespace", &self.namespace)
            .field("methods", &self.methods.lock().keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl RemoteNamespaceService {
    /// Namespace name without the `remote.` service prefix.
    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// Whether this namespace currently owns either method variant.
    #[must_use]
    pub fn has_method(&self, method: &str) -> bool {
        self.methods.lock().contains_key(method)
    }

    /// Captures the current direct/scoped pair and caller Context.
    ///
    /// # Errors
    ///
    /// Returns when the method property is no longer mounted.
    pub fn bind(&self, caller: &Context, method: &str) -> anyhow::Result<ClientRemoteMethod> {
        let current = self.methods.lock().get(method).cloned().ok_or_else(|| {
            anyhow::anyhow!(
                "client api: Remote method {}/{} is no longer mounted",
                self.namespace,
                method
            )
        })?;
        let core = self.core.upgrade().ok_or_else(|| {
            anyhow::anyhow!(
                "client api: Remote method {}/{} is no longer mounted",
                self.namespace,
                method
            )
        })?;
        Ok(ClientRemoteMethod {
            core,
            caller: caller.clone(),
            namespace: self.namespace.clone(),
            method: method.to_owned(),
            direct: current.direct,
            scoped: current.scoped,
        })
    }

    fn empty(&self) -> bool {
        self.methods.lock().is_empty()
    }

    fn has_variant(&self, kind: MethodKind, method: &str) -> bool {
        let methods = self.methods.lock();
        let Some(record) = methods.get(method) else {
            return false;
        };
        match kind {
            MethodKind::Direct => record.direct.is_some(),
            MethodKind::Scoped => record.scoped.is_some(),
        }
    }

    fn install(
        &self,
        kind: MethodKind,
        descriptor: InvocationDescriptor,
        projection: Option<ScopedProjection>,
        token: MountToken,
    ) -> anyhow::Result<()> {
        ensure_method_available(&self.namespace, &descriptor.method)?;
        let mut methods = self.methods.lock();
        let record = methods.entry(descriptor.method.clone()).or_default();
        match kind {
            MethodKind::Direct => {
                anyhow::ensure!(
                    record.direct.is_none(),
                    "client api: direct method {}/{} is already mounted",
                    self.namespace,
                    descriptor.method
                );
                record.direct = Some(DirectMethod { descriptor, token });
            }
            MethodKind::Scoped => {
                anyhow::ensure!(
                    record.scoped.is_none(),
                    "client api: scoped method {}/{} is already mounted",
                    self.namespace,
                    descriptor.method
                );
                record.scoped = Some(ScopedMethod {
                    method: DirectMethod { descriptor, token },
                    projection: projection.expect("scoped installation requires a projection"),
                });
            }
        }
        Ok(())
    }

    fn remove(&self, kind: MethodKind, method: &str, token: &MountToken) {
        let mut methods = self.methods.lock();
        let Some(record) = methods.get_mut(method) else {
            return;
        };
        let matches = match kind {
            MethodKind::Direct => record
                .direct
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(&current.token.active, &token.active)),
            MethodKind::Scoped => record
                .scoped
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(&current.method.token.active, &token.active)),
        };
        if !matches {
            return;
        }
        match kind {
            MethodKind::Direct => record.direct = None,
            MethodKind::Scoped => record.scoped = None,
        }
        if record.direct.is_none() && record.scoped.is_none() {
            methods.shift_remove(method);
        }
    }
}

/// One generated Client method captured from a namespace service.
pub struct ClientRemoteMethod {
    core: Arc<ClientCore>,
    caller: Context,
    namespace: String,
    method: String,
    direct: Option<DirectMethod>,
    scoped: Option<ScopedMethod>,
}

impl std::fmt::Debug for ClientRemoteMethod {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClientRemoteMethod")
            .field(
                "endpoint",
                &format_args!("{}/{}", self.namespace, self.method),
            )
            .finish_non_exhaustive()
    }
}

impl ClientRemoteMethod {
    /// Invokes the captured direct/scoped method pair.
    ///
    /// # Errors
    ///
    /// Returns arity, binder, Connection, or input-boundary failures.
    pub async fn invoke(
        &self,
        arguments: Vec<ClientRemoteArgument>,
    ) -> anyhow::Result<RemoteResult<TypertBoundaryValue>> {
        if let Some(scoped) = &self.scoped {
            let identity = self
                .core
                .registry
                .contexts()
                .get_client(&scoped.projection.context)
                .and_then(|binder| (binder.identity)(&self.caller));
            if let Some(identity) = identity {
                return invoke_descriptor(
                    &self.core,
                    &self.caller,
                    &scoped.method,
                    Some(&scoped.projection),
                    arguments,
                    Some(identity),
                )
                .await;
            }
        }
        if let Some(direct) = &self.direct {
            return invoke_descriptor(&self.core, &self.caller, direct, None, arguments, None)
                .await;
        }
        if let Some(scoped) = &self.scoped {
            return invoke_descriptor(
                &self.core,
                &self.caller,
                &scoped.method,
                Some(&scoped.projection),
                arguments,
                None,
            )
            .await;
        }
        anyhow::bail!("client api: Remote method is no longer mounted")
    }
}

fn validate_contribution(
    core: &Arc<ClientCore>,
    contribution: &TypertRemoteContribution,
) -> anyhow::Result<()> {
    let mut direct: HashMap<&str, HashSet<&str>> = HashMap::new();
    let mut scoped: HashMap<&str, HashSet<&str>> = HashMap::new();
    for descriptor in &contribution.descriptors {
        require_strict_descriptor(descriptor)?;
        if matches!(descriptor.invocation, InvocationReceiver::Direct) {
            add_candidate(core, &mut direct, descriptor, MethodKind::Direct)?;
        }
        if scoped_projection(descriptor)?.is_some() {
            add_candidate(core, &mut scoped, descriptor, MethodKind::Scoped)?;
        }
    }
    let mut namespaces = direct
        .keys()
        .chain(scoped.keys())
        .copied()
        .collect::<HashSet<_>>();
    for namespace in namespaces.drain() {
        let existing = core.state.lock().namespaces.get(namespace).cloned();
        if existing.is_none() {
            anyhow::ensure!(
                !remote_service_reserved(namespace),
                "client api: namespace {namespace:?} conflicts with the Remote service"
            );
            anyhow::ensure!(
                !core.owner_context.has_named(&remote_service_key(namespace)),
                "client api: namespace {namespace:?} conflicts with an existing Remote namespace"
            );
        }
        let methods = direct
            .get(namespace)
            .into_iter()
            .flat_map(|methods| methods.iter())
            .chain(
                scoped
                    .get(namespace)
                    .into_iter()
                    .flat_map(|methods| methods.iter()),
            )
            .copied()
            .collect::<HashSet<_>>();
        for method in methods {
            ensure_method_available(namespace, method)?;
        }
    }
    Ok(())
}

fn add_candidate<'a>(
    core: &Arc<ClientCore>,
    table: &mut HashMap<&'a str, HashSet<&'a str>>,
    descriptor: &'a InvocationDescriptor,
    kind: MethodKind,
) -> anyhow::Result<()> {
    let methods = table.entry(&descriptor.namespace).or_default();
    let label = match kind {
        MethodKind::Direct => "direct",
        MethodKind::Scoped => "scoped",
    };
    anyhow::ensure!(
        methods.insert(&descriptor.method),
        "client api: contribution repeats {label} method {}",
        endpoint_of(descriptor)
    );
    let namespace = core
        .state
        .lock()
        .namespaces
        .get(&descriptor.namespace)
        .cloned();
    anyhow::ensure!(
        namespace
            .as_ref()
            .is_none_or(|namespace| !namespace.service.has_variant(kind, &descriptor.method)),
        "client api: {label} method {} is already mounted",
        endpoint_of(descriptor)
    );
    Ok(())
}

fn install_descriptor(
    core: &Arc<ClientCore>,
    descriptor: &InvocationDescriptor,
) -> anyhow::Result<MountedDescriptor> {
    let token = MountToken::new();
    let mut mounted = MountedDescriptor {
        token: token.clone(),
        variants: Vec::new(),
    };
    if matches!(descriptor.invocation, InvocationReceiver::Direct) {
        install_variant(
            core,
            descriptor.clone(),
            None,
            MethodKind::Direct,
            token.clone(),
        )?;
        mounted.variants.push(InstalledVariant {
            namespace: descriptor.namespace.clone(),
            method: descriptor.method.clone(),
            kind: MethodKind::Direct,
            token: token.clone(),
        });
    }
    if let Some(projection) = scoped_projection(descriptor)? {
        install_variant(
            core,
            descriptor.clone(),
            Some(projection),
            MethodKind::Scoped,
            token.clone(),
        )?;
        mounted.variants.push(InstalledVariant {
            namespace: descriptor.namespace.clone(),
            method: descriptor.method.clone(),
            kind: MethodKind::Scoped,
            token,
        });
    }
    Ok(mounted)
}

fn install_variant(
    core: &Arc<ClientCore>,
    descriptor: InvocationDescriptor,
    projection: Option<ScopedProjection>,
    kind: MethodKind,
    token: MountToken,
) -> anyhow::Result<()> {
    let namespace = get_or_create_namespace(core, &descriptor.namespace)?;
    namespace
        .service
        .install(kind, descriptor, projection, token)
}

fn get_or_create_namespace(core: &Arc<ClientCore>, name: &str) -> anyhow::Result<NamespaceHandle> {
    if let Some(namespace) = core.state.lock().namespaces.get(name).cloned() {
        return Ok(namespace);
    }
    let service = Arc::new(RemoteNamespaceService {
        namespace: name.to_owned(),
        core: Arc::downgrade(core),
        methods: Mutex::new(IndexMap::new()),
    });
    let provision = core
        .owner_context
        .provide_named(&remote_service_key(name), service.clone())?;
    let handle = NamespaceHandle { service, provision };
    core.state
        .lock()
        .namespaces
        .insert(name.to_owned(), handle.clone());
    Ok(handle)
}

async fn cleanup_mount(
    core: &Arc<ClientCore>,
    mounted: Vec<MountedDescriptor>,
    remote: &EffectHandle,
) {
    let mut empty = IndexMap::<String, EffectHandle>::new();
    for descriptor in mounted.into_iter().rev() {
        if !descriptor.token.withdraw() {
            continue;
        }
        for variant in descriptor.variants.into_iter().rev() {
            let namespace = core
                .state
                .lock()
                .namespaces
                .get(&variant.namespace)
                .cloned();
            let Some(namespace) = namespace else {
                continue;
            };
            namespace
                .service
                .remove(variant.kind, &variant.method, &variant.token);
            if namespace.service.empty() {
                let removed = core
                    .state
                    .lock()
                    .namespaces
                    .shift_remove(&variant.namespace);
                if let Some(removed) = removed {
                    empty.insert(variant.namespace, removed.provision);
                }
            }
        }
    }
    for (_, provision) in empty.into_iter().rev() {
        if let Err(error) = provision.dispose().await {
            tracing::warn!(%error, "client api: namespace disposal failed");
        }
    }
    if let Err(error) = remote.dispose().await {
        tracing::warn!(%error, "client api: Remote registry disposal failed");
    }
}

async fn invoke_descriptor(
    core: &Arc<ClientCore>,
    caller: &Context,
    method: &DirectMethod,
    projection: Option<&ScopedProjection>,
    values: Vec<ClientRemoteArgument>,
    bound_identity: Option<Value>,
) -> anyhow::Result<RemoteResult<TypertBoundaryValue>> {
    let descriptor = &method.descriptor;
    let endpoint = endpoint_of(descriptor);
    if !method.token.active() {
        return Ok(withdrawn(&endpoint));
    }
    let prepared = prepare_invocation(
        core,
        caller,
        descriptor,
        projection,
        &values,
        bound_identity,
        &endpoint,
    )?;
    let connection = core
        .owner_context
        .get(CLIENT_CONNECTION)
        .ok_or_else(|| anyhow::anyhow!("client api: {endpoint} has no active Connection"))?;
    let signal = prepared.caller_signal.map_or_else(
        || method.token.abort.clone(),
        |caller| AbortSignal::fuse(&method.token.abort, &caller),
    );
    let result = connection
        .call(
            "/api",
            &endpoint,
            Value::Object(Map::from_iter([(
                "args".to_owned(),
                Value::Object(prepared.args),
            )])),
            signal,
        )
        .await;
    let result = match result {
        Ok(result) => result,
        Err(error) => return Ok(carrier_failure(&endpoint, &error)),
    };
    if !method.token.active() {
        return Ok(withdrawn(&endpoint));
    }
    match result {
        RpcResult::Failure { error } => Ok(RemoteResult::Failure {
            error: RemoteFailure {
                code: error.code,
                message: error.message,
                details: error.details,
            },
        }),
        RpcResult::Success { value } => {
            let value = value.map_or(TypertBoundaryValue::Undefined, TypertBoundaryValue::Json);
            match parse(&descriptor.result, value, &endpoint, "result") {
                Ok(value) => Ok(RemoteResult::Success { value }),
                Err(error) => Ok(carrier_failure(&endpoint, &error)),
            }
        }
    }
}

struct PreparedInvocation {
    args: Map<String, Value>,
    caller_signal: Option<AbortSignal>,
}

fn prepare_invocation(
    core: &Arc<ClientCore>,
    caller: &Context,
    descriptor: &InvocationDescriptor,
    projection: Option<&ScopedProjection>,
    values: &[ClientRemoteArgument],
    bound_identity: Option<Value>,
    endpoint: &str,
) -> anyhow::Result<PreparedInvocation> {
    let expected = descriptor.parameters.len()
        - usize::from(
            projection
                .and_then(|projection| projection.parameter_index)
                .is_some(),
        );
    let has_caller_signal = descriptor.cancellation && values.len() == expected + 1;
    if values.len() != expected && !has_caller_signal {
        let contract = if descriptor.cancellation {
            format!("{expected} business argument(s) plus an optional AbortSignal")
        } else {
            format!("{expected} argument(s)")
        };
        anyhow::bail!(
            "client api: {endpoint} expected {contract}, got {}",
            values.len()
        );
    }
    let mut args = Map::new();
    if let Some(projection) = projection {
        let identity = if let Some(identity) = bound_identity {
            identity
        } else {
            let binder = core
                .registry
                .contexts()
                .get_client(&projection.context)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "client api: {endpoint} has no Client Context binder for {:?}",
                        projection.context
                    )
                })?;
            (binder.identity)(caller).ok_or_else(|| {
                anyhow::anyhow!(
                    "client api: {endpoint} requires a {:?} Context",
                    projection.context
                )
            })?
        };
        let identity = parse(
            &projection.codec,
            TypertBoundaryValue::Json(identity),
            endpoint,
            &projection.wire,
        )?;
        if let TypertBoundaryValue::Json(identity) = identity {
            args.insert(projection.wire.clone(), identity);
        }
    }
    let mut value_index = 0;
    for (parameter_index, parameter) in descriptor.parameters.iter().enumerate() {
        if projection.and_then(|projection| projection.parameter_index) == Some(parameter_index) {
            continue;
        }
        let value = values.get(value_index).ok_or_else(|| {
            anyhow::anyhow!("client api: {endpoint} is missing a generated argument")
        })?;
        let ClientRemoteArgument::Boundary(value) = value else {
            anyhow::bail!("client api: {endpoint} rejected {:?}", parameter.wire);
        };
        let value = parse(&parameter.codec, value.clone(), endpoint, &parameter.wire)?;
        if let TypertBoundaryValue::Json(value) = value {
            args.insert(parameter.wire.clone(), value);
        }
        value_index += 1;
    }
    let caller_signal = if has_caller_signal {
        match &values[expected] {
            ClientRemoteArgument::Signal(signal) => signal.clone(),
            ClientRemoteArgument::Boundary(TypertBoundaryValue::Undefined) => None,
            ClientRemoteArgument::Boundary(_) => {
                anyhow::bail!(
                    "client api: {endpoint} expected an optional AbortSignal as its final argument"
                )
            }
        }
    } else {
        None
    };
    Ok(PreparedInvocation {
        args,
        caller_signal,
    })
}

fn scoped_projection(
    descriptor: &InvocationDescriptor,
) -> anyhow::Result<Option<ScopedProjection>> {
    if let InvocationReceiver::Context {
        context,
        wire,
        codec,
    } = &descriptor.invocation
    {
        return Ok(Some(ScopedProjection {
            context: context.clone(),
            wire: wire.clone(),
            codec: codec.clone(),
            parameter_index: None,
        }));
    }
    let Some(scope) = &descriptor.scope else {
        return Ok(None);
    };
    let lookups = descriptor
        .parameters
        .iter()
        .enumerate()
        .filter(|(_, parameter)| parameter.source == InvocationParameterSource::Lookup)
        .collect::<Vec<_>>();
    let selected = (lookups.len() == 1)
        .then(|| lookups[0])
        .filter(|(_, parameter)| {
            parameter.wire == scope.wire
                && parameter.lookup.as_deref() == Some(scope.context.as_str())
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "client api: generated Remote {} scope must select its only lookup parameter",
                endpoint_of(descriptor)
            )
        })?;
    Ok(Some(ScopedProjection {
        context: scope.context.clone(),
        wire: scope.wire.clone(),
        codec: selected.1.codec.clone(),
        parameter_index: Some(selected.0),
    }))
}

fn require_strict_descriptor(descriptor: &InvocationDescriptor) -> anyhow::Result<()> {
    let endpoint = endpoint_of(descriptor);
    require_strict_codec(&descriptor.result, &endpoint, "result")?;
    for parameter in &descriptor.parameters {
        require_strict_codec(&parameter.codec, &endpoint, &parameter.wire)?;
    }
    if let InvocationReceiver::Context { wire, codec, .. } = &descriptor.invocation {
        require_strict_codec(codec, &endpoint, wire)?;
    }
    Ok(())
}

fn require_strict_codec(codec: &TypertCodec, endpoint: &str, field: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        matches!(codec, TypertCodec::Strict { .. }),
        "client api: generated Remote {endpoint} field {field:?} has no strict codec"
    );
    Ok(())
}

fn parse(
    codec: &TypertCodec,
    value: TypertBoundaryValue,
    endpoint: &str,
    field: &str,
) -> anyhow::Result<TypertBoundaryValue> {
    let TypertCodec::Strict { schema, .. } = codec else {
        anyhow::bail!(
            "client api: generated Remote {endpoint} field {field:?} has no strict codec"
        );
    };
    schema
        .parse(value)
        .map_err(|cause| ClientBoundaryError {
            message: format!("client api: {endpoint} rejected {field:?}"),
            cause,
        })
        .map_err(Into::into)
}

#[derive(Debug)]
struct ClientBoundaryError {
    message: String,
    cause: anyhow::Error,
}

impl std::fmt::Display for ClientBoundaryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ClientBoundaryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.cause.as_ref())
    }
}

fn withdrawn(endpoint: &str) -> RemoteResult<TypertBoundaryValue> {
    internal_failure(format!(
        "client api: Remote method {endpoint} is no longer mounted"
    ))
}

fn carrier_failure(
    endpoint: &str,
    error: &dyn std::fmt::Display,
) -> RemoteResult<TypertBoundaryValue> {
    internal_failure(format!("client api: {endpoint} failed: {error}"))
}

fn internal_failure(message: String) -> RemoteResult<TypertBoundaryValue> {
    RemoteResult::Failure {
        error: RemoteFailure {
            code: "internal".to_owned(),
            message,
            details: Map::new(),
        },
    }
}

fn ensure_method_available(namespace: &str, method: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !namespace_method_reserved(method),
        "client api: method {:?} conflicts with its namespace service",
        format!("{namespace}/{method}")
    );
    Ok(())
}

fn remote_service_key(namespace: &str) -> String {
    format!("remote.{namespace}")
}

fn endpoint_of(descriptor: &InvocationDescriptor) -> String {
    format!("{}/{}", descriptor.namespace, descriptor.method)
}

fn remote_service_reserved(name: &str) -> bool {
    matches!(
        name,
        "core"
            | "mount"
            | "namespace"
            | "method"
            | "invoke"
            | "subscribe"
            | "deliver"
            | "$mount"
            | "$on"
            | "$dispatch"
            | "toString"
            | "valueOf"
            | "hasOwnProperty"
            | "constructor"
            | "__proto__"
    )
}

fn namespace_method_reserved(name: &str) -> bool {
    matches!(
        name,
        "ctx"
            | "empty"
            | "invokeRemote"
            | "methods"
            | "name"
            | "namespace"
            | "assertMethodAvailable"
            | "has"
            | "hasMethod"
            | "install"
            | "installDirect"
            | "installScoped"
            | "remove"
            | "bind"
            | "toString"
            | "toLocaleString"
            | "valueOf"
            | "hasOwnProperty"
            | "isPrototypeOf"
            | "propertyIsEnumerable"
            | "constructor"
            | "__defineGetter__"
            | "__defineSetter__"
            | "__lookupGetter__"
            | "__lookupSetter__"
            | "__proto__"
    )
}
