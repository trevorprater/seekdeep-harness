//! Ordered Host inspection providers and first-answer-wins Client query routing.

use std::{collections::HashSet, sync::Arc};

use futures::future::BoxFuture;
use indexmap::IndexMap;
use parking_lot::Mutex;
use seekdeep_cordis::{Context, EventArgs, ServiceKey, fiber::EffectHandle};
use seekdeep_llm::{AbortSignal, SessionId};
use seekdeep_tools::{assert_supported_json_schema, validate_json_schema_value_at};
use serde_json::{Map, Value};
use tokio::sync::oneshot;

use crate::{
    CordisInspectMethodManifest, CordisInspectPlatform, CordisInspectProviderManifest,
    CordisInspectProviderView, CordisInspectQueryRequest, CordisInspectQueryResolution,
    CordisInspectQueryResolved, CordisInspectRequestId, CordisInspectResolveAck,
};

/// Cordis service slot for the process-wide inspection directory and router.
pub const CORDIS_INSPECT: ServiceKey<CordisInspectRegistryService> =
    ServiceKey::new("cordisInspect");

/// Context supplied to a native Host inspection query.
#[derive(Clone, Debug)]
pub struct HostCordisInspectQueryContext {
    /// Tool-call cancellation.
    pub signal: AbortSignal,
    /// Session whose scoped runtime is being inspected.
    pub session_id: SessionId,
}

/// One native Host inspection method dispatcher.
pub type HostCordisInspectQuery = Arc<
    dyn Fn(
            String,
            Option<Value>,
            HostCordisInspectQueryContext,
        ) -> BoxFuture<'static, anyhow::Result<Value>>
        + Send
        + Sync,
>;

/// Local Host registration paired with its serializable provider manifest.
#[derive(Clone)]
pub struct HostCordisInspectProviderRegistration {
    /// Provider and explicit method directory.
    pub manifest: CordisInspectProviderManifest,
    /// Executes one declared method.
    pub query: HostCordisInspectQuery,
}

impl std::fmt::Debug for HostCordisInspectProviderRegistration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HostCordisInspectProviderRegistration")
            .field("manifest", &self.manifest)
            .finish_non_exhaustive()
    }
}

struct RegisteredProvider {
    generation: u64,
    registration: HostCordisInspectProviderRegistration,
}

struct PendingClientQuery {
    request: CordisInspectQueryRequest,
    method: CordisInspectMethodManifest,
    settle: oneshot::Sender<CordisInspectQueryResolution>,
}

#[derive(Default)]
struct InspectState {
    providers: IndexMap<String, RegisteredProvider>,
    pending: IndexMap<CordisInspectRequestId, PendingClientQuery>,
    client_manifest: Option<Vec<CordisInspectProviderManifest>>,
    next_request: u64,
    next_registration: u64,
}

/// Registry and cross-page router behind model-visible Cordis inspection.
pub struct CordisInspectRegistryService {
    context: Context,
    state: Arc<Mutex<InspectState>>,
}

impl std::fmt::Debug for CordisInspectRegistryService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state.lock();
        formatter
            .debug_struct("CordisInspectRegistryService")
            .field("providers", &state.providers.len())
            .field("pending", &state.pending.len())
            .finish_non_exhaustive()
    }
}

impl CordisInspectRegistryService {
    /// Creates an empty process-local registry rooted in `context`.
    #[must_use]
    pub fn new(context: Context) -> Arc<Self> {
        Arc::new(Self {
            context,
            state: Arc::new(Mutex::new(InspectState::default())),
        })
    }

    /// Registers one Host provider as a reversible Cordis effect.
    ///
    /// # Errors
    ///
    /// Rejects invalid manifests, duplicate provider IDs, and inactive owners.
    pub fn register(
        &self,
        owner: &Context,
        registration: HostCordisInspectProviderRegistration,
    ) -> anyhow::Result<EffectHandle> {
        let manifest = validate_manifest(&registration.manifest)?;
        let (generation, provider_id) = {
            let mut state = self.state.lock();
            anyhow::ensure!(
                !state.providers.contains_key(&manifest.id),
                "Host Cordis inspect provider \"{}\" is already registered",
                manifest.id
            );
            state.next_registration += 1;
            let generation = state.next_registration;
            let provider_id = manifest.id.clone();
            state.providers.insert(
                provider_id.clone(),
                RegisteredProvider {
                    generation,
                    registration: HostCordisInspectProviderRegistration {
                        manifest,
                        query: registration.query,
                    },
                },
            );
            (generation, provider_id)
        };
        let state = self.state.clone();
        let cleanup_id = provider_id.clone();
        let effect = EffectHandle::synchronous(
            format!("cordisInspect.register({provider_id:?})"),
            move || {
                let mut state = state.lock();
                if state
                    .providers
                    .get(&cleanup_id)
                    .is_some_and(|provider| provider.generation == generation)
                {
                    state.providers.shift_remove(&cleanup_id);
                }
                Ok(())
            },
        );
        if let Err(error) = owner.own(effect.clone()) {
            let mut state = self.state.lock();
            if state
                .providers
                .get(&provider_id)
                .is_some_and(|provider| provider.generation == generation)
            {
                state.providers.shift_remove(&provider_id);
            }
            return Err(error.into());
        }
        Ok(effect)
    }

    /// Replaces the complete mirrored Client provider directory atomically.
    ///
    /// # Errors
    ///
    /// Rejects malformed providers, methods, schemas, or repeated IDs.
    pub fn sync_client_manifest(
        &self,
        providers: &[CordisInspectProviderManifest],
    ) -> anyhow::Result<()> {
        let mut ids = HashSet::new();
        let mut validated = Vec::with_capacity(providers.len());
        for provider in providers {
            let manifest = validate_manifest(provider)?;
            anyhow::ensure!(
                ids.insert(manifest.id.clone()),
                "Client Cordis inspect manifest repeats provider \"{}\"",
                manifest.id
            );
            validated.push(manifest);
        }
        self.state.lock().client_manifest = Some(validated);
        Ok(())
    }

    /// Returns Host providers in registration order followed by Client providers.
    #[must_use]
    pub fn list(&self) -> Vec<CordisInspectProviderView> {
        let state = self.state.lock();
        state
            .providers
            .values()
            .map(|provider| view(CordisInspectPlatform::Host, &provider.registration.manifest))
            .chain(
                state
                    .client_manifest
                    .iter()
                    .flatten()
                    .map(|provider| view(CordisInspectPlatform::Client, provider)),
            )
            .collect()
    }

    /// Executes one declared Host or Client query with schema validation.
    ///
    /// # Errors
    ///
    /// Rejects missing providers or methods, invalid input/output, provider
    /// failures, and cancellation.
    pub async fn query(
        &self,
        platform: CordisInspectPlatform,
        provider_id: &str,
        method_name: &str,
        input: Option<Value>,
        session_id: &SessionId,
        signal: AbortSignal,
    ) -> anyhow::Result<Value> {
        match platform {
            CordisInspectPlatform::Host => {
                self.query_host(provider_id, method_name, input, session_id, signal)
                    .await
            }
            CordisInspectPlatform::Client => {
                self.query_client(provider_id, method_name, input, session_id, signal)
                    .await
            }
        }
    }

    /// Accepts the first valid Client result for one exact Session query.
    #[must_use]
    pub fn resolve_client_query(
        &self,
        session_id: &SessionId,
        request_id: &CordisInspectRequestId,
        resolution: CordisInspectQueryResolution,
    ) -> CordisInspectResolveAck {
        let CordisInspectQueryResolution::Success { data } = resolution else {
            return CordisInspectResolveAck { accepted: false };
        };
        let (data, pending) = {
            let mut state = self.state.lock();
            let Some(pending) = state.pending.get(request_id) else {
                return CordisInspectResolveAck { accepted: false };
            };
            if pending.request.agent_id != *session_id {
                return CordisInspectResolveAck { accepted: false };
            }
            let Ok(data) =
                validate_output("Client", &pending.request.provider, &pending.method, data)
            else {
                return CordisInspectResolveAck { accepted: false };
            };
            let Some(pending) = state.pending.shift_remove(request_id) else {
                return CordisInspectResolveAck { accepted: false };
            };
            (data, pending)
        };
        let _ = pending
            .settle
            .send(CordisInspectQueryResolution::Success { data });
        self.announce_resolved(request_id);
        CordisInspectResolveAck { accepted: true }
    }

    async fn query_host(
        &self,
        provider_id: &str,
        method_name: &str,
        input: Option<Value>,
        session_id: &SessionId,
        signal: AbortSignal,
    ) -> anyhow::Result<Value> {
        let (method, query) = {
            let state = self.state.lock();
            let registration = state.providers.get(provider_id).ok_or_else(|| {
                anyhow::anyhow!("Host Cordis inspect provider \"{provider_id}\" is not registered")
            })?;
            let method = find_method(&registration.registration.manifest, method_name)?.clone();
            (method, registration.registration.query.clone())
        };
        validate_input("Host", provider_id, &method, input.as_ref())?;
        ensure_not_aborted(&signal)?;
        let data = query(
            method_name.to_owned(),
            input,
            HostCordisInspectQueryContext {
                signal: signal.clone(),
                session_id: session_id.clone(),
            },
        )
        .await?;
        ensure_not_aborted(&signal)?;
        validate_output("Host", provider_id, &method, data)
    }

    async fn query_client(
        &self,
        provider_id: &str,
        method_name: &str,
        input: Option<Value>,
        session_id: &SessionId,
        signal: AbortSignal,
    ) -> anyhow::Result<Value> {
        ensure_not_aborted(&signal)?;
        let (request, receiver) = {
            let mut state = self.state.lock();
            let provider = state
                .client_manifest
                .as_ref()
                .and_then(|providers| providers.iter().find(|provider| provider.id == provider_id))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Client Cordis inspect provider \"{provider_id}\" is not registered"
                    )
                })?;
            let method = find_method(provider, method_name)?.clone();
            validate_input("Client", provider_id, &method, input.as_ref())?;
            state.next_request += 1;
            let request_id = CordisInspectRequestId::new(format!("inspect-{}", state.next_request));
            let request = CordisInspectQueryRequest {
                request_id: request_id.clone(),
                agent_id: session_id.clone(),
                provider: provider_id.to_owned(),
                method: method_name.to_owned(),
                input,
            };
            let (sender, receiver) = oneshot::channel();
            state.pending.insert(
                request_id,
                PendingClientQuery {
                    request: request.clone(),
                    method,
                    settle: sender,
                },
            );
            (request, receiver)
        };
        let _ = self.context.events().emit(
            &self.context,
            "cordis/inspect-query",
            &EventArgs::one(request.clone()),
        );
        let resolution = tokio::select! {
            biased;
            resolution = receiver => resolution.map_err(|_| {
                anyhow::anyhow!("Client inspect query channel closed before settlement")
            })?,
            () = signal.cancelled() => {
                self.cancel_client_query(&request);
                CordisInspectQueryResolution::Failure {
                    reason: crate::CordisInspectFailureReason::Cancelled,
                    message: format!(
                        "Client inspect query {provider_id}.{method_name} was cancelled"
                    ),
                }
            },
        };
        match resolution {
            CordisInspectQueryResolution::Success { data } => Ok(data),
            CordisInspectQueryResolution::Failure { message, .. } => {
                anyhow::bail!("{provider_id}.{method_name}: {message}")
            }
        }
    }

    fn cancel_client_query(&self, request: &CordisInspectQueryRequest) {
        let pending = self.state.lock().pending.shift_remove(&request.request_id);
        let Some(pending) = pending else {
            return;
        };
        let _ = pending.settle.send(CordisInspectQueryResolution::Failure {
            reason: crate::CordisInspectFailureReason::Cancelled,
            message: format!(
                "Client inspect query {}.{} was cancelled",
                request.provider, request.method
            ),
        });
        self.announce_resolved(&request.request_id);
    }

    fn announce_resolved(&self, request_id: &CordisInspectRequestId) {
        let _ = self.context.events().emit(
            &self.context,
            "cordis/inspect-query-resolved",
            &EventArgs::one(CordisInspectQueryResolved {
                request_id: request_id.clone(),
            }),
        );
    }
}

fn view(
    platform: CordisInspectPlatform,
    manifest: &CordisInspectProviderManifest,
) -> CordisInspectProviderView {
    CordisInspectProviderView {
        id: manifest.id.clone(),
        description: manifest.description.clone(),
        methods: manifest.methods.clone(),
        platform,
    }
}

fn validate_manifest(
    manifest: &CordisInspectProviderManifest,
) -> anyhow::Result<CordisInspectProviderManifest> {
    anyhow::ensure!(
        !manifest.id.trim().is_empty(),
        "Cordis inspect provider id must not be empty"
    );
    anyhow::ensure!(
        !manifest.description.trim().is_empty(),
        "Cordis inspect provider \"{}\" needs a description",
        manifest.id
    );
    let mut names = HashSet::new();
    for method in &manifest.methods {
        anyhow::ensure!(
            !method.name.trim().is_empty(),
            "Cordis inspect provider \"{}\" has an empty method name",
            manifest.id
        );
        anyhow::ensure!(
            names.insert(method.name.clone()),
            "Cordis inspect provider \"{}\" repeats method \"{}\"",
            manifest.id,
            method.name
        );
        anyhow::ensure!(
            !method.description.trim().is_empty(),
            "Cordis inspect method {}.{} needs a description",
            manifest.id,
            method.name
        );
        assert_supported_json_schema(method.input_schema.clone())?;
        assert_supported_json_schema(method.output_schema.clone())?;
    }
    Ok(manifest.clone())
}

fn find_method<'a>(
    manifest: &'a CordisInspectProviderManifest,
    name: &str,
) -> anyhow::Result<&'a CordisInspectMethodManifest> {
    manifest
        .methods
        .iter()
        .find(|method| method.name == name)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Cordis inspect provider \"{}\" has no method \"{name}\"",
                manifest.id
            )
        })
}

fn validate_input(
    platform: &str,
    provider: &str,
    method: &CordisInspectMethodManifest,
    input: Option<&Value>,
) -> anyhow::Result<()> {
    let schema = assert_supported_json_schema(method.input_schema.clone())?;
    let empty = Value::Object(Map::new());
    let violations = validate_json_schema_value_at(&schema, input.unwrap_or(&empty), "input");
    anyhow::ensure!(
        violations.is_empty(),
        "{platform} Cordis inspect {provider}.{} rejected input: {}",
        method.name,
        violations.join("; ")
    );
    Ok(())
}

fn validate_output(
    platform: &str,
    provider: &str,
    method: &CordisInspectMethodManifest,
    data: Value,
) -> anyhow::Result<Value> {
    anyhow::ensure!(
        is_lossless_json(&data),
        "{platform} Cordis inspect {provider}.{} returned a non-JSON value",
        method.name
    );
    let schema = assert_supported_json_schema(method.output_schema.clone())?;
    let violations = validate_json_schema_value_at(&schema, &data, "output");
    anyhow::ensure!(
        violations.is_empty(),
        "{platform} Cordis inspect {provider}.{} returned invalid output: {}",
        method.name,
        violations.join("; ")
    );
    Ok(data)
}

fn is_lossless_json(value: &Value) -> bool {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            Value::Number(number)
                if number
                    .as_f64()
                    .is_some_and(|value| value == 0.0 && value.is_sign_negative()) =>
            {
                return false;
            }
            Value::Array(values) => pending.extend(values),
            Value::Object(values) => pending.extend(values.values()),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
    true
}

fn ensure_not_aborted(signal: &AbortSignal) -> anyhow::Result<()> {
    anyhow::ensure!(!signal.is_aborted(), "This operation was aborted");
    Ok(())
}
