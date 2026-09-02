//! Live Typert Remote dispatch over native Cordis services and providers.

pub mod client;
pub mod invariant;
pub mod types;

use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use indexmap::IndexMap;
use parking_lot::Mutex;
use seekdeep_client_connection::{
    ConnectionRpcAuthority, HOST_CONNECTION, HostConnectionService, RpcError, RpcHandler,
    RpcHandlerFuture, RpcResult, SharedRpcRegistration,
};
use seekdeep_cordis::{Context, Plugin, ServiceKey, fiber::EffectHandle};
use seekdeep_llm::AbortSignal;
use seekdeep_typert_protocol::{
    InvocationDescriptor, InvocationParameterDescriptor, InvocationParameterSource,
    InvocationReceiver, RemoteFailure, RemoteMethodMarker, TypertBoundaryValue, TypertCodec,
    TypertContextRegistry as _, TypertHostArgument, TypertInvocableService,
    TypertLocalRegistry as _, TypertLookupDefinition, TypertLookupFailure,
    TypertLookupRegistry as _,
};
use seekdeep_typert_registry::{TYPERT, TypertRegistry};
use serde_json::{Map, Value};
use uuid::Uuid;

pub use types::*;

/// Typed Cordis slot corresponding to `ctx.typertGateway`.
pub const TYPERT_GATEWAY: ServiceKey<TypertGatewayService> = ServiceKey::new("typertGateway");
/// Typed directory slot used to re-resolve native Typert services by Context.
pub const TYPERT_SERVICES: ServiceKey<TypertServiceDirectory> = ServiceKey::new("typertServices");
/// Loader plugin identity.
pub const PLUGIN_NAME: &str = "api-gateway";
/// Gateway requires the Typert registry.
pub const PLUGIN_INJECT: &[&str] = &["typert"];

/// Dynamic service resolver preserving Cordis Context rebinding.
pub type TypertServiceResolver =
    Arc<dyn Fn(&Context) -> Option<Arc<dyn TypertInvocableService>> + Send + Sync>;

struct ServiceEntry {
    owner: Uuid,
    resolver: TypertServiceResolver,
}

/// Explicit native equivalent of Cordis reflection over Remote-capable services.
#[derive(Default)]
pub struct TypertServiceDirectory {
    entries: Mutex<IndexMap<String, ServiceEntry>>,
    revision: AtomicU64,
}

impl std::fmt::Debug for TypertServiceDirectory {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TypertServiceDirectory")
            .field("keys", &self.entries.lock().keys().collect::<Vec<_>>())
            .field("revision", &self.revision.load(Ordering::Acquire))
            .finish()
    }
}

impl TypertServiceDirectory {
    /// Constructs an empty unprovided directory.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Publishes this exact directory.
    ///
    /// # Errors
    ///
    /// Returns duplicate-service or inactive-owner failures.
    pub fn provide(
        self: &Arc<Self>,
        context: &Context,
    ) -> Result<EffectHandle, seekdeep_cordis::CordisError> {
        context.provide(TYPERT_SERVICES, self.clone())
    }

    /// Registers one Context-rebound service resolver.
    ///
    /// # Errors
    ///
    /// Rejects duplicate keys and inactive ownership.
    pub fn register(
        self: &Arc<Self>,
        context: &Context,
        service_key: impl Into<String>,
        resolver: TypertServiceResolver,
    ) -> anyhow::Result<EffectHandle> {
        let service_key = service_key.into();
        anyhow::ensure!(
            !service_key.is_empty(),
            "Typert service key must not be empty"
        );
        let owner = Uuid::now_v7();
        {
            let mut entries = self.entries.lock();
            anyhow::ensure!(
                !entries.contains_key(&service_key),
                "Typert service {service_key:?} is already registered"
            );
            entries.insert(service_key.clone(), ServiceEntry { owner, resolver });
            self.revision.fetch_add(1, Ordering::AcqRel);
        }
        let directory = self.clone();
        let disposal_key = service_key.clone();
        let effect = EffectHandle::synchronous("typertServices.register()", move || {
            let mut entries = directory.entries.lock();
            if entries
                .get(&disposal_key)
                .is_some_and(|entry| entry.owner == owner)
            {
                entries.shift_remove(&disposal_key);
                directory.revision.fetch_add(1, Ordering::AcqRel);
            }
            Ok(())
        });
        match context.own(effect.clone()) {
            Ok(effect) => Ok(effect),
            Err(error) => {
                let mut entries = self.entries.lock();
                if entries
                    .get(&service_key)
                    .is_some_and(|entry| entry.owner == owner)
                {
                    entries.shift_remove(&service_key);
                    self.revision.fetch_add(1, Ordering::AcqRel);
                }
                Err(error.into())
            }
        }
    }

    fn resolve(
        &self,
        context: &Context,
        service_key: &str,
    ) -> Option<Arc<dyn TypertInvocableService>> {
        let resolver = self.entries.lock().get(service_key)?.resolver.clone();
        resolver(context)
    }

    fn live(&self, context: &Context) -> Vec<(String, Arc<dyn TypertInvocableService>)> {
        let resolvers = self
            .entries
            .lock()
            .iter()
            .map(|(key, entry)| (key.clone(), entry.resolver.clone()))
            .collect::<Vec<_>>();
        resolvers
            .into_iter()
            .filter_map(|(key, resolver)| resolver(context).map(|service| (key, service)))
            .collect()
    }

    fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }
}

/// Registers one typed Cordis service with the Host Remote directory.
///
/// The resolver re-reads the calling Context on every invocation so scoped
/// Cordis rebinding and provider replacement keep their source semantics.
///
/// # Errors
///
/// Returns when the Gateway directory is unavailable, the service key is
/// already registered, or the current lifecycle owner is inactive.
pub fn register_invocable_service<Service>(
    context: &Context,
    key: ServiceKey<Service>,
) -> anyhow::Result<EffectHandle>
where
    Service: TypertInvocableService,
{
    let directory = context
        .get(TYPERT_SERVICES)
        .ok_or_else(|| anyhow::anyhow!("Typert service directory is unavailable"))?;
    directory.register(
        context,
        key.name(),
        Arc::new(move |context| {
            context
                .get(key)
                .map(|service| service as Arc<dyn TypertInvocableService>)
        }),
    )
}

/// Registers one typed Cordis service when the Host Remote directory is live.
///
/// Domain plugins use this optional seam so their source-visible dependency
/// lists and standalone behavior do not depend on the deployment's Gateway.
///
/// # Errors
///
/// Returns duplicate-service or inactive-owner failures when the directory is
/// present; returns `Ok(None)` when this composition does not mount a Gateway.
pub fn register_invocable_service_if_available<Service>(
    context: &Context,
    key: ServiceKey<Service>,
) -> anyhow::Result<Option<EffectHandle>>
where
    Service: TypertInvocableService,
{
    let Some(directory) = context.get(TYPERT_SERVICES) else {
        return Ok(None);
    };
    directory
        .register(
            context,
            key.name(),
            Arc::new(move |context| {
                context
                    .get(key)
                    .map(|service| service as Arc<dyn TypertInvocableService>)
            }),
        )
        .map(Some)
}

/// Dispatch failure produced outside the invoked business method.
#[derive(Debug)]
pub struct TypertGatewayError {
    /// Stable machine-readable category.
    pub code: TypertGatewayErrorCode,
    /// Canonical endpoint.
    pub endpoint: String,
    /// Affected wire field.
    pub field: Option<String>,
    message: String,
    cause: Option<anyhow::Error>,
}

impl TypertGatewayError {
    fn new(
        code: TypertGatewayErrorCode,
        endpoint: &str,
        message: impl Into<String>,
        field: Option<&str>,
        cause: Option<anyhow::Error>,
    ) -> Self {
        Self {
            code,
            endpoint: endpoint.to_owned(),
            field: field.map(str::to_owned),
            message: message.into(),
            cause,
        }
    }
}

impl std::fmt::Display for TypertGatewayError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "typert gateway: {}: {}",
            self.endpoint, self.message
        )
    }
}

impl std::error::Error for TypertGatewayError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause.as_ref().map(AsRef::as_ref)
    }
}

#[derive(Debug)]
struct RemoteInvocationCancelled {
    endpoint: String,
    cause: anyhow::Error,
}

impl std::fmt::Display for RemoteInvocationCancelled {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Remote invocation {:?} was aborted",
            self.endpoint
        )
    }
}

impl std::error::Error for RemoteInvocationCancelled {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.cause.as_ref())
    }
}

/// Host dispatcher for strict generated definitions and conservative source markers.
pub struct TypertGatewayService {
    context: Context,
    registry: Arc<TypertRegistry>,
    services: Arc<TypertServiceDirectory>,
    src_claims: Mutex<Option<SrcClaimsCache>>,
    connection_binding: Mutex<Option<GatewayConnectionBinding>>,
}

struct GatewayConnectionBinding {
    connection: Arc<HostConnectionService>,
    registration: SharedRpcRegistration,
}

struct SrcClaimsCache {
    service_revision: u64,
    directory_revision: u64,
    claims: HashSet<String>,
}

impl std::fmt::Debug for TypertGatewayService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TypertGatewayService")
            .finish_non_exhaustive()
    }
}

impl TypertGatewayService {
    /// Constructs an unprovided Gateway over exact dependencies.
    #[must_use]
    pub fn new(
        context: &Context,
        registry: Arc<TypertRegistry>,
        services: Arc<TypertServiceDirectory>,
    ) -> Arc<Self> {
        Arc::new(Self {
            context: context.clone(),
            registry,
            services,
            src_claims: Mutex::new(None),
            connection_binding: Mutex::new(None),
        })
    }

    /// Publishes this exact Gateway.
    ///
    /// # Errors
    ///
    /// Returns duplicate-service or inactive-owner failures.
    pub fn provide(
        self: &Arc<Self>,
        context: &Context,
    ) -> Result<EffectHandle, seekdeep_cordis::CordisError> {
        context.provide(TYPERT_GATEWAY, self.clone())
    }

    /// Rebinds the Gateway to the calling Cordis Context.
    ///
    /// Cordis' TypeScript service proxy performs this rebinding implicitly on
    /// property access. Native callers make the same receiver selection
    /// explicit so direct services and scoped service slots resolve from the
    /// caller rather than from the Context that installed the Gateway.
    #[must_use]
    pub fn for_context(&self, context: &Context) -> Arc<Self> {
        Arc::new(Self {
            context: context.clone(),
            registry: self.registry.clone(),
            services: self.services.clone(),
            src_claims: Mutex::new(None),
            connection_binding: Mutex::new(None),
        })
    }

    /// Whether this Gateway owns one syntactically valid endpoint.
    #[must_use]
    pub fn claims_endpoint(&self, endpoint: &str) -> bool {
        let Some((namespace, method)) = endpoint.split_once('/') else {
            return false;
        };
        if namespace.is_empty() || method.is_empty() || method.contains('/') {
            return false;
        }
        if self.registry.local().get(endpoint).is_some() || self.registry.local().has_seen(endpoint)
        {
            return true;
        }
        self.src_claims().contains(endpoint)
    }

    fn refresh_connection_binding(self: &Arc<Self>, context: &Context) -> anyhow::Result<()> {
        let current = context.get_relaxed(HOST_CONNECTION);
        {
            let binding = self.connection_binding.lock();
            if binding.as_ref().is_some_and(|binding| {
                current
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(&binding.connection, current))
            }) {
                return Ok(());
            }
        }
        if let Some(previous) = self.connection_binding.lock().take() {
            previous.registration.withdraw();
        }
        let Some(connection) = current else {
            return Ok(());
        };
        let registration = install_connection_interceptor(context, &connection, self)?;
        *self.connection_binding.lock() = Some(GatewayConnectionBinding {
            connection,
            registration,
        });
        Ok(())
    }

    fn src_claims(&self) -> HashSet<String> {
        let service_revision = self.context.service_revision();
        let directory_revision = self.services.revision();
        let mut cache = self.src_claims.lock();
        if let Some(current) = cache.as_ref()
            && current.service_revision == service_revision
            && current.directory_revision == directory_revision
        {
            return current.claims.clone();
        }
        let claims: HashSet<String> = self
            .services
            .live(&self.context)
            .into_iter()
            .filter(|(_, service)| service.has_visible_binding())
            .flat_map(|(_, service)| {
                let namespace = service.namespace().to_owned();
                service.remote_methods().into_iter().map(move |marker| {
                    format!(
                        "{namespace}/{}",
                        marker.export_name.as_deref().unwrap_or(&marker.method)
                    )
                })
            })
            .collect();
        *cache = Some(SrcClaimsCache {
            service_revision,
            directory_revision,
            claims: claims.clone(),
        });
        claims
    }

    /// Invokes one live Remote method without a carrier envelope.
    ///
    /// # Errors
    ///
    /// Returns typed Gateway boundary failures, lookup policy failures, or the
    /// invoked business error unchanged.
    pub async fn invoke(
        &self,
        request: InvokeRemoteRequest,
    ) -> anyhow::Result<TypertBoundaryValue> {
        let endpoint = format!("{}/{}", request.namespace, request.method);
        let descriptor = self.resolve_descriptor(&request.namespace, &request.method, &endpoint)?;
        assert_exact_arguments(&request.args, &descriptor, &endpoint)?;
        let receiver_context = self
            .resolve_receiver_context(&descriptor, &request.args, &endpoint)
            .await?;
        let service = self
            .services
            .resolve(&receiver_context, &descriptor.service)
            .ok_or_else(|| {
                gateway(
                    TypertGatewayErrorCode::ServiceUnavailable,
                    &endpoint,
                    format!("active Service {:?} is unavailable", descriptor.service),
                    None,
                    None,
                )
            })?;
        validate_binding(service.as_ref(), &descriptor, &endpoint)?;
        let mut arguments =
            Vec::with_capacity(descriptor.parameters.len() + usize::from(descriptor.cancellation));
        for parameter in &descriptor.parameters {
            arguments.push(
                self.resolve_parameter(parameter, &request.args, &endpoint)
                    .await?,
            );
        }
        if descriptor.cancellation {
            arguments.push(TypertHostArgument::Signal(
                request.signal.clone().unwrap_or_default(),
            ));
        }
        let implementation = descriptor
            .implementation
            .as_deref()
            .unwrap_or(&descriptor.method);
        if !service.has_method(implementation) {
            return Err(gateway(
                TypertGatewayErrorCode::MethodUnavailable,
                &endpoint,
                format!(
                    "active Service {:?} has no callable method {implementation:?}",
                    descriptor.service
                ),
                None,
                None,
            ));
        }
        let result = service.clone().invoke(implementation, arguments).await;
        let result = match result {
            Ok(result) => result,
            Err(error) if request.signal.as_ref().is_some_and(AbortSignal::is_aborted) => {
                return Err(RemoteInvocationCancelled {
                    endpoint,
                    cause: error,
                }
                .into());
            }
            Err(error) => return Err(error),
        };
        if result.is_undefined() && matches!(descriptor.result, TypertCodec::SrcJson) {
            return Ok(result);
        }
        decode(
            &descriptor.result,
            result,
            TypertGatewayErrorCode::ResultInvalid,
            &endpoint,
            "result",
        )
    }

    /// Dispatches one decoded JSON carrier payload and folds failures into an RPC result.
    pub async fn invoke_rpc(
        &self,
        endpoint: &str,
        payload: Value,
        signal: AbortSignal,
    ) -> GatewayRpcResult {
        let Some((namespace, method)) = endpoint.split_once('/') else {
            return rpc_failure(&anyhow::anyhow!("invalid Remote endpoint {endpoint:?}"));
        };
        if namespace.is_empty() || method.is_empty() || method.contains('/') {
            return rpc_failure(&anyhow::anyhow!("invalid Remote endpoint {endpoint:?}"));
        }
        let Value::Object(mut payload) = payload else {
            return invalid_rpc_payload();
        };
        if payload.len() != 1 {
            return invalid_rpc_payload();
        }
        let Some(Value::Object(args)) = payload.remove("args") else {
            return invalid_rpc_payload();
        };
        let request = InvokeRemoteRequest {
            namespace: namespace.to_owned(),
            method: method.to_owned(),
            args: args
                .into_iter()
                .map(|(key, value)| (key, TypertBoundaryValue::Json(value)))
                .collect(),
            signal: Some(signal),
        };
        match self.invoke(request).await {
            Ok(value) => GatewayRpcResult::Success {
                value: value.into_optional_json(),
            },
            Err(error) => rpc_failure(&error),
        }
    }

    fn resolve_descriptor(
        &self,
        namespace: &str,
        method: &str,
        endpoint: &str,
    ) -> anyhow::Result<InvocationDescriptor> {
        if let Some(strict) = self.registry.local().get(endpoint) {
            return Ok(strict);
        }
        if self.registry.local().has_seen(endpoint) {
            return Err(gateway(
                TypertGatewayErrorCode::DefinitionUnavailable,
                endpoint,
                "its strict definition was withdrawn and SRC fallback is forbidden",
                None,
                None,
            ));
        }
        let mut candidates = Vec::new();
        for (registered_key, service) in self.services.live(&self.context) {
            if !service.has_visible_binding() {
                continue;
            }
            if registered_key != service.service_key() {
                return Err(gateway(
                    TypertGatewayErrorCode::BindingInvalid,
                    endpoint,
                    format!("Service {registered_key:?} has an inconsistent typertRemote binding"),
                    None,
                    None,
                ));
            }
            if service.namespace() != namespace {
                continue;
            }
            if let Some(marker) = service.remote_methods().into_iter().find(|candidate| {
                candidate
                    .export_name
                    .as_deref()
                    .unwrap_or(&candidate.method)
                    == method
            }) {
                candidates.push(self.src_descriptor(
                    service.as_ref(),
                    &marker,
                    method,
                    endpoint,
                )?);
            }
        }
        match candidates.len() {
            0 => Err(gateway(
                TypertGatewayErrorCode::InvocationUnavailable,
                endpoint,
                "no active Remote method exports this endpoint",
                None,
                None,
            )),
            1 => Ok(candidates.remove(0)),
            _ => {
                let mut services = candidates
                    .iter()
                    .map(|candidate| candidate.service.as_str())
                    .collect::<Vec<_>>();
                services.sort_unstable();
                Err(gateway(
                    TypertGatewayErrorCode::AmbiguousEndpoint,
                    endpoint,
                    format!(
                        "multiple active Services export this endpoint: {}",
                        services.join(", ")
                    ),
                    None,
                    None,
                ))
            }
        }
    }

    fn src_descriptor(
        &self,
        service: &dyn TypertInvocableService,
        marker: &RemoteMethodMarker,
        method: &str,
        endpoint: &str,
    ) -> anyhow::Result<InvocationDescriptor> {
        let names = service.parameter_names(&marker.method).ok_or_else(|| {
            gateway(
                TypertGatewayErrorCode::MethodUnavailable,
                endpoint,
                format!(
                    "Remote marker has no implementation method {:?}",
                    marker.method
                ),
                None,
                None,
            )
        })?;
        validate_src_names(&names, endpoint, &marker.method)?;
        let signal_index = names.iter().position(|name| name == "signal");
        if signal_index.is_some_and(|index| index + 1 != names.len()) {
            return Err(gateway(
                TypertGatewayErrorCode::SignatureInvalid,
                endpoint,
                "SRC cancellation parameter signal must be the final parameter",
                Some("signal"),
                None,
            ));
        }
        let cancellation = signal_index.is_some();
        let business_names = if cancellation {
            &names[..names.len() - 1]
        } else {
            &names
        };
        let definitions = self.registry.lookups().definitions();
        let mut wires = HashSet::new();
        let parameters = src_parameters(business_names, &definitions, &mut wires, endpoint)?;
        let invocation = self.src_invocation(marker, &mut wires, endpoint)?;
        Ok(InvocationDescriptor {
            id: format!("src:{}#{endpoint}", service.service_key()),
            service: service.service_key().to_owned(),
            namespace: service.namespace().to_owned(),
            method: method.to_owned(),
            implementation: (marker.method != method).then(|| marker.method.clone()),
            invocation,
            scope: None,
            parameters,
            cancellation,
            result: TypertCodec::SrcJson,
            source_location: None,
        })
    }

    fn src_invocation(
        &self,
        marker: &RemoteMethodMarker,
        wires: &mut HashSet<String>,
        endpoint: &str,
    ) -> anyhow::Result<InvocationReceiver> {
        let seekdeep_typert_protocol::RemoteInvocationMarker::Context { context } =
            &marker.invocation
        else {
            return Ok(InvocationReceiver::Direct);
        };
        let provider = self.registry.contexts().get_host(context).ok_or_else(|| {
            gateway(
                TypertGatewayErrorCode::ContextUnavailable,
                endpoint,
                format!("Context provider {context:?} is unavailable"),
                None,
                None,
            )
        })?;
        if !wires.insert(provider.wire.clone()) {
            return Err(gateway(
                TypertGatewayErrorCode::SignatureInvalid,
                endpoint,
                format!(
                    "Context identity conflicts with wire field {:?}",
                    provider.wire
                ),
                Some(&provider.wire),
                None,
            ));
        }
        Ok(InvocationReceiver::Context {
            context: context.clone(),
            wire: provider.wire,
            codec: TypertCodec::SrcJson,
        })
    }

    async fn resolve_receiver_context(
        &self,
        descriptor: &InvocationDescriptor,
        args: &IndexMap<String, TypertBoundaryValue>,
        endpoint: &str,
    ) -> anyhow::Result<Context> {
        let InvocationReceiver::Context {
            context,
            wire,
            codec,
        } = &descriptor.invocation
        else {
            return Ok(self.context.clone());
        };
        let provider = self.registry.contexts().get_host(context).ok_or_else(|| {
            gateway(
                TypertGatewayErrorCode::ContextUnavailable,
                endpoint,
                format!("Context provider {context:?} is unavailable"),
                None,
                None,
            )
        })?;
        validate_provider(
            &provider.wire,
            &provider.wire_type_symbol,
            wire,
            codec,
            context,
            endpoint,
        )?;
        let identity = decode(
            codec,
            args.get(wire)
                .cloned()
                .unwrap_or(TypertBoundaryValue::Undefined),
            TypertGatewayErrorCode::InputInvalid,
            endpoint,
            wire,
        )?;
        match (provider.resolve)(identity).await {
            Ok(Some(context)) => Ok(context),
            Ok(None) => Err(gateway(
                TypertGatewayErrorCode::ContextNotFound,
                endpoint,
                format!("Context provider {context:?} did not resolve the requested identity"),
                Some(wire),
                None,
            )),
            Err(error) if error.downcast_ref::<TypertLookupFailure>().is_some() => Err(error),
            Err(error) => Err(gateway(
                TypertGatewayErrorCode::ContextFailed,
                endpoint,
                format!("Context provider {context:?} failed"),
                Some(wire),
                Some(error),
            )),
        }
    }

    async fn resolve_parameter(
        &self,
        parameter: &InvocationParameterDescriptor,
        args: &IndexMap<String, TypertBoundaryValue>,
        endpoint: &str,
    ) -> anyhow::Result<TypertHostArgument> {
        let Some(value) = args.get(&parameter.wire).cloned() else {
            return Ok(TypertHostArgument::Boundary(TypertBoundaryValue::Undefined));
        };
        let value = decode(
            &parameter.codec,
            value,
            TypertGatewayErrorCode::InputInvalid,
            endpoint,
            &parameter.wire,
        )?;
        if parameter.source == InvocationParameterSource::Json {
            return Ok(TypertHostArgument::Boundary(value));
        }
        let key = parameter.lookup.as_deref().ok_or_else(|| {
            gateway(
                TypertGatewayErrorCode::LookupUnavailable,
                endpoint,
                format!("lookup parameter {:?} has no provider key", parameter.name),
                Some(&parameter.wire),
                None,
            )
        })?;
        let provider = self.registry.lookups().get(key).ok_or_else(|| {
            gateway(
                TypertGatewayErrorCode::LookupUnavailable,
                endpoint,
                format!("lookup provider {key:?} is unavailable"),
                Some(&parameter.wire),
                None,
            )
        })?;
        validate_provider(
            &provider.wire,
            &provider.wire_type_symbol,
            &parameter.wire,
            &parameter.codec,
            key,
            endpoint,
        )?;
        match (provider.resolve)(value).await {
            Ok(Some(object)) => Ok(TypertHostArgument::Lookup(object)),
            Ok(None) => Err(gateway(
                TypertGatewayErrorCode::LookupNotFound,
                endpoint,
                format!("lookup provider {key:?} did not resolve the requested identity"),
                Some(&parameter.wire),
                None,
            )),
            Err(error) if error.downcast_ref::<TypertLookupFailure>().is_some() => Err(error),
            Err(error) => Err(gateway(
                TypertGatewayErrorCode::LookupFailed,
                endpoint,
                format!("lookup provider {key:?} failed"),
                Some(&parameter.wire),
                Some(error),
            )),
        }
    }
}

fn invalid_rpc_payload() -> GatewayRpcResult {
    rpc_failure(&anyhow::anyhow!(
        "Remote payload must contain exactly one plain-object args field"
    ))
}

/// Installs the service directory and Host Gateway over an existing registry.
///
/// # Errors
///
/// Returns missing dependency, duplicate-service, or inactive-owner failures.
pub fn install(
    context: &Context,
) -> anyhow::Result<(Arc<TypertServiceDirectory>, Arc<TypertGatewayService>)> {
    let registry = context
        .get(TYPERT)
        .ok_or_else(|| anyhow::anyhow!("api gateway requires typert"))?;
    let services = TypertServiceDirectory::new();
    services.provide(context)?;
    let gateway = TypertGatewayService::new(context, registry, services.clone());
    gateway.provide(context)?;
    let binding_context = context.clone();
    let weak_gateway = Arc::downgrade(&gateway);
    context.on_service_change(move || {
        let Some(gateway) = weak_gateway.upgrade() else {
            return;
        };
        if let Err(error) = gateway.refresh_connection_binding(&binding_context) {
            tracing::error!(%error, "api gateway: Connection binding reconciliation failed");
        }
    })?;
    gateway.refresh_connection_binding(context)?;
    Ok((services, gateway))
}

/// Builds the Loader-compatible Typert API gateway plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(PLUGIN_NAME, PLUGIN_INJECT.iter().copied(), |context, _| {
        Box::pin(async move {
            install(&context)?;
            Ok(())
        })
    })
}

/// Mounts the Host Gateway claim on Connection's shared `/api` channel.
///
/// # Errors
///
/// Returns duplicate-interceptor or inactive-owner failures.
pub fn install_connection_interceptor(
    context: &Context,
    connection: &Arc<HostConnectionService>,
    gateway: &Arc<TypertGatewayService>,
) -> anyhow::Result<SharedRpcRegistration> {
    let claims = gateway.clone();
    let matcher = Arc::new(move |endpoint: &str| claims.claims_endpoint(endpoint));
    let dispatch = gateway.clone();
    let handler: RpcHandler = Arc::new(move |endpoint, payload, signal| {
        let dispatch = dispatch.clone();
        Box::pin(async move {
            Ok(
                match dispatch.invoke_rpc(&endpoint, payload, signal).await {
                    GatewayRpcResult::Success { value } => RpcResult::Success { value },
                    GatewayRpcResult::Failure { error } => RpcResult::Failure {
                        error: RpcError {
                            code: error.code,
                            message: error.message,
                            details: error.details,
                        },
                    },
                },
            )
        }) as RpcHandlerFuture
    });
    connection.intercept(
        context,
        "/api",
        matcher,
        handler,
        ConnectionRpcAuthority::TrustedHost,
    )
}

/// Carrier-folded Host Gateway result with omission distinct from JSON `null`.
#[derive(Clone, Debug, PartialEq)]
pub enum GatewayRpcResult {
    /// Successful optional business value.
    Success {
        /// Omitted for declared `undefined`; `Some(Null)` is explicit `null`.
        value: Option<Value>,
    },
    /// Structured carrier failure.
    Failure {
        /// Stable carrier error.
        error: RemoteFailure,
    },
}

fn rpc_failure(error: &anyhow::Error) -> GatewayRpcResult {
    if let Some(cancelled) = error.downcast_ref::<RemoteInvocationCancelled>() {
        return GatewayRpcResult::Failure {
            error: RemoteFailure {
                code: "cancelled".to_owned(),
                message: cancelled.to_string(),
                details: Map::new(),
            },
        };
    }
    if let Some(policy) = error.downcast_ref::<TypertLookupFailure>()
        && let Ok(failure) = serde_json::from_value::<RemoteFailure>(policy.failure.clone())
    {
        return GatewayRpcResult::Failure { error: failure };
    }
    GatewayRpcResult::Failure {
        error: RemoteFailure {
            code: "internal".to_owned(),
            message: error.to_string(),
            details: Map::new(),
        },
    }
}

fn src_parameters(
    names: &[String],
    definitions: &[TypertLookupDefinition],
    wires: &mut HashSet<String>,
    endpoint: &str,
) -> anyhow::Result<Vec<InvocationParameterDescriptor>> {
    names
        .iter()
        .map(|name| {
            let matches = definitions
                .iter()
                .filter(|definition| definition.parameter == *name)
                .collect::<Vec<_>>();
            if matches.len() > 1 {
                return Err(gateway(
                    TypertGatewayErrorCode::SignatureInvalid,
                    endpoint,
                    format!("parameter {name:?} matches multiple lookup providers"),
                    Some(name),
                    None,
                ));
            }
            let parameter = matches.first().map_or_else(
                || InvocationParameterDescriptor {
                    name: name.clone(),
                    wire: name.clone(),
                    source: InvocationParameterSource::Json,
                    lookup: None,
                    codec: TypertCodec::SrcJson,
                    accepts_undefined: None,
                },
                |definition| InvocationParameterDescriptor {
                    name: name.clone(),
                    wire: definition.wire.clone(),
                    source: InvocationParameterSource::Lookup,
                    lookup: Some(definition.key.clone()),
                    codec: TypertCodec::SrcJson,
                    accepts_undefined: None,
                },
            );
            if !wires.insert(parameter.wire.clone()) {
                return Err(gateway(
                    TypertGatewayErrorCode::SignatureInvalid,
                    endpoint,
                    format!("multiple parameters use wire field {:?}", parameter.wire),
                    Some(&parameter.wire),
                    None,
                ));
            }
            Ok(parameter)
        })
        .collect()
}

fn validate_binding(
    service: &dyn TypertInvocableService,
    descriptor: &InvocationDescriptor,
    endpoint: &str,
) -> anyhow::Result<()> {
    if !service.has_visible_binding() {
        return Err(gateway(
            TypertGatewayErrorCode::BindingInvalid,
            endpoint,
            format!(
                "Service {:?} has no visible typertRemote binding",
                descriptor.service
            ),
            None,
            None,
        ));
    }
    if service.service_key() != descriptor.service || service.namespace() != descriptor.namespace {
        return Err(gateway(
            TypertGatewayErrorCode::BindingInvalid,
            endpoint,
            format!(
                "Service {:?} has an inconsistent typertRemote binding",
                descriptor.service
            ),
            None,
            None,
        ));
    }
    Ok(())
}

fn validate_provider(
    provider_wire: &str,
    provider_type: &str,
    descriptor_wire: &str,
    codec: &TypertCodec,
    key: &str,
    endpoint: &str,
) -> anyhow::Result<()> {
    let matches = provider_wire == descriptor_wire
        && match codec {
            TypertCodec::Strict { type_symbol, .. } => provider_type == type_symbol,
            TypertCodec::SrcJson => true,
        };
    if !matches {
        return Err(gateway(
            TypertGatewayErrorCode::ProviderMismatch,
            endpoint,
            format!("provider {key:?} does not match its strict definition"),
            Some(descriptor_wire),
            None,
        ));
    }
    Ok(())
}

fn validate_src_names(names: &[String], endpoint: &str, method: &str) -> anyhow::Result<()> {
    let mut unique = HashSet::new();
    for name in names {
        if !is_identifier(name) || !unique.insert(name) {
            return Err(gateway(
                TypertGatewayErrorCode::SignatureInvalid,
                endpoint,
                format!(
                    "SRC method {method:?} must use unique identifier parameters without destructuring, defaults, or rest"
                ),
                None,
                None,
            ));
        }
    }
    Ok(())
}

fn is_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    matches!(first, '$' | '_' | 'A'..='Z' | 'a'..='z')
        && chars
            .all(|character| character == '$' || character == '_' || character.is_alphanumeric())
}

fn assert_exact_arguments(
    args: &IndexMap<String, TypertBoundaryValue>,
    descriptor: &InvocationDescriptor,
    endpoint: &str,
) -> anyhow::Result<()> {
    let mut expected = descriptor
        .parameters
        .iter()
        .map(|parameter| parameter.wire.as_str())
        .collect::<HashSet<_>>();
    if let InvocationReceiver::Context { wire, .. } = &descriptor.invocation {
        expected.insert(wire);
    }
    let extra = args
        .keys()
        .filter(|key| !expected.contains(key.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let accepts_missing = descriptor
        .parameters
        .iter()
        .filter(|parameter| {
            parameter.source == InvocationParameterSource::Json
                && (parameter.accepts_undefined == Some(true)
                    || matches!(parameter.codec, TypertCodec::SrcJson))
        })
        .map(|parameter| parameter.wire.as_str())
        .collect::<HashSet<_>>();
    let missing = expected
        .iter()
        .filter(|key| !args.contains_key(**key) && !accepts_missing.contains(**key))
        .copied()
        .collect::<Vec<_>>();
    if missing.is_empty() && extra.is_empty() {
        return Ok(());
    }
    let mut clauses = Vec::new();
    if !missing.is_empty() {
        clauses.push(format!("missing {}", quoted(&missing)));
    }
    if !extra.is_empty() {
        clauses.push(format!(
            "unexpected {}",
            quoted(&extra.iter().map(String::as_str).collect::<Vec<_>>())
        ));
    }
    Err(gateway(
        TypertGatewayErrorCode::ArgumentsInvalid,
        endpoint,
        format!(
            "args fields do not match the descriptor: {}",
            clauses.join("; ")
        ),
        None,
        None,
    ))
}

fn quoted(values: &[&str]) -> String {
    values
        .iter()
        .map(|value| format!("{value:?}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn decode(
    codec: &TypertCodec,
    value: TypertBoundaryValue,
    code: TypertGatewayErrorCode,
    endpoint: &str,
    field: &str,
) -> anyhow::Result<TypertBoundaryValue> {
    match codec {
        TypertCodec::Strict { schema, .. } => schema.parse(value).map_err(|cause| {
            gateway(
                code,
                endpoint,
                if code == TypertGatewayErrorCode::InputInvalid {
                    format!("wire field {field:?} failed boundary validation")
                } else {
                    "business result failed boundary validation".to_owned()
                },
                Some(field),
                Some(cause),
            )
        }),
        TypertCodec::SrcJson if value.is_undefined() => Err(gateway(
            code,
            endpoint,
            if code == TypertGatewayErrorCode::InputInvalid {
                format!("wire field {field:?} failed boundary validation")
            } else {
                "business result failed boundary validation".to_owned()
            },
            Some(field),
            Some(anyhow::anyhow!("undefined is not JSON-safe")),
        )),
        TypertCodec::SrcJson => Ok(value),
    }
}

fn gateway(
    code: TypertGatewayErrorCode,
    endpoint: &str,
    message: impl Into<String>,
    field: Option<&str>,
    cause: Option<anyhow::Error>,
) -> anyhow::Error {
    TypertGatewayError::new(code, endpoint, message, field, cause).into()
}
