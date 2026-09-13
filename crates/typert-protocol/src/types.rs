//! Carrier-independent Typert descriptors and provider contracts.

use std::{any::Any, sync::Arc};

use futures::future::BoxFuture;
use seekdeep_cordis::{Context, fiber::EffectHandle};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de, ser::SerializeMap};
use serde_json::{Map, Value};

/// Dynamically typed live Host object resolved by one lookup provider.
pub type TypertHostObject = Arc<dyn Any + Send + Sync>;

/// One JavaScript-compatible value after structural boundary admission.
///
/// `Undefined` is distinct from JSON `null` and crosses a JSON carrier as an
/// omitted optional field, never as a sentinel value.
#[derive(Clone, Debug, PartialEq)]
pub enum TypertBoundaryValue {
    /// Declared absence.
    Undefined,
    /// A lossless structural JSON value.
    Json(Value),
}

impl TypertBoundaryValue {
    /// Wraps one structural JSON value.
    #[must_use]
    pub const fn json(value: Value) -> Self {
        Self::Json(value)
    }

    /// Borrows the JSON value, or returns `None` for absence.
    #[must_use]
    pub const fn as_json(&self) -> Option<&Value> {
        match self {
            Self::Undefined => None,
            Self::Json(value) => Some(value),
        }
    }

    /// Consumes this value into its optional carrier slot.
    #[must_use]
    pub fn into_optional_json(self) -> Option<Value> {
        match self {
            Self::Undefined => None,
            Self::Json(value) => Some(value),
        }
    }

    /// Whether this value is declared absence.
    #[must_use]
    pub const fn is_undefined(&self) -> bool {
        matches!(self, Self::Undefined)
    }
}

impl From<Value> for TypertBoundaryValue {
    fn from(value: Value) -> Self {
        Self::Json(value)
    }
}

/// One decoded argument handed from the Gateway to a native service adapter.
#[derive(Clone, Debug)]
pub enum TypertHostArgument {
    /// JSON or declared absence.
    Boundary(TypertBoundaryValue),
    /// Live object resolved through a lookup provider.
    Lookup(TypertHostObject),
    /// Carrier cancellation injected after all business arguments.
    Signal(seekdeep_llm::AbortSignal),
}

/// Future returned by one native Remote service method.
pub type TypertInvocationFuture = BoxFuture<'static, anyhow::Result<TypertBoundaryValue>>;

/// Object-safe live service contract consumed by the native Gateway.
pub trait TypertInvocableService: Send + Sync + 'static {
    /// Whether the live service exposes its explicit `typertRemote` binding.
    ///
    /// Source-mode discovery ignores an absent binding, while a strict
    /// descriptor targeting the service reports `binding-invalid`.
    fn has_visible_binding(&self) -> bool {
        true
    }

    /// Exact Cordis service key.
    fn service_key(&self) -> &str;

    /// Exact Remote namespace.
    fn namespace(&self) -> &str;

    /// Declaration-order source-mode markers.
    fn remote_methods(&self) -> Vec<crate::RemoteMethodMarker>;

    /// Source parameter names for conservative source-mode derivation.
    fn parameter_names(&self, implementation: &str) -> Option<Vec<String>>;

    /// Whether one implementation member is callable on this live service.
    fn has_method(&self, implementation: &str) -> bool;

    /// Invoke one implementation with already decoded positional arguments.
    fn invoke(
        self: Arc<Self>,
        implementation: &str,
        arguments: Vec<TypertHostArgument>,
    ) -> TypertInvocationFuture;
}

/// Awaitable lookup outcome.
pub type TypertLookupFuture = BoxFuture<'static, anyhow::Result<Option<TypertHostObject>>>;
/// Lookup callback after the wire identity has passed its codec.
pub type TypertLookupResolver =
    Arc<dyn Fn(TypertBoundaryValue) -> TypertLookupFuture + Send + Sync>;

/// Awaitable Host Context outcome.
pub type TypertHostContextFuture = BoxFuture<'static, anyhow::Result<Option<Context>>>;
/// Composition- or provider-owned Host Context resolver.
pub type TypertHostContextResolver =
    Arc<dyn Fn(TypertBoundaryValue) -> TypertHostContextFuture + Send + Sync>;

/// Client Context identity callback.
pub type TypertClientContextResolver = Arc<dyn Fn(&Context) -> Option<Value> + Send + Sync>;

/// Type-level association between a Host object and its wire identity.
pub trait TypertLookup: Send + Sync + 'static {
    /// Live Host object type.
    type Host: Send + Sync + 'static;
    /// Validated wire identity type.
    type Wire: Send + Sync + 'static;
}

/// Type-level association between a scoped Context kind and wire identity.
pub trait TypertContext: Send + Sync + 'static {
    /// Validated Context identity type.
    type Wire: Send + Sync + 'static;
}

/// Marker implemented by typed Cordis event declarations that are unscoped,
/// one-way, and therefore transportable through Remote forwarding.
pub trait TypertForwardableEvent: Send + Sync + 'static {
    /// Exact Cordis event name.
    const NAME: &'static str;
}

/// Host-assembly allowlist for forwarded Remote events.
pub trait TypertRemoteEventSelection: Send + Sync + 'static {
    /// Whether one existing forwardable event is selected.
    fn contains(event: &str) -> bool;
}

/// Completion of one decoded forwarded event listener.
pub type TypertRemoteEventFuture = BoxFuture<'static, anyhow::Result<()>>;
/// One decoded forwarded event listener.
pub type TypertRemoteEventListener =
    Arc<dyn Fn(Vec<Value>) -> TypertRemoteEventFuture + Send + Sync>;

/// Future returned while mounting one generated Remote contribution.
pub type TypertRemoteMountFuture = BoxFuture<'static, anyhow::Result<TypertDisposer>>;

/// Client Remote capability implemented by a Gateway consumer.
pub trait TypertClientRemote: Send + Sync {
    /// Mounts one generated Host-for-Client contribution.
    fn mount(
        &self,
        context: &Context,
        contribution: TypertRemoteContribution,
    ) -> TypertRemoteMountFuture;

    /// Subscribes to one selected, forwardable Host event.
    ///
    /// # Errors
    ///
    /// Rejects events outside the active Host allowlist or inactive owners.
    fn on(
        &self,
        context: &Context,
        event: &str,
        listener: TypertRemoteEventListener,
    ) -> anyhow::Result<TypertDisposer>;

    /// Dispatches one decoded forwarded frame to contained listeners.
    fn dispatch(&self, event: &str, args: Vec<Value>);
}

/// A Remote call's failure as reported by its carrier.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteFailure {
    /// Carrier-defined open error code.
    pub code: String,
    /// Human-readable failure.
    pub message: String,
    /// Structured failure details.
    pub details: Map<String, Value>,
}

/// Folded Remote business result.
#[derive(Clone, Debug, PartialEq)]
pub enum RemoteResult<T> {
    /// Successful business value.
    Success {
        /// Decoded result.
        value: T,
    },
    /// Carrier-reported business failure.
    Failure {
        /// Structured failure.
        error: RemoteFailure,
    },
}

impl<T: Serialize> Serialize for RemoteResult<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(2))?;
        match self {
            Self::Success { value } => {
                map.serialize_entry("ok", &true)?;
                map.serialize_entry("value", value)?;
            }
            Self::Failure { error } => {
                map.serialize_entry("ok", &false)?;
                map.serialize_entry("error", error)?;
            }
        }
        map.end()
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for RemoteResult<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire<T> {
            Success(SuccessWire<T>),
            Failure(FailureWire),
        }
        match Wire::deserialize(deserializer)? {
            Wire::Success(success) => Ok(Self::Success {
                value: success.value,
            }),
            Wire::Failure(failure) => Ok(Self::Failure {
                error: failure.error,
            }),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SuccessWire<T> {
    #[serde(rename = "ok")]
    _ok: True,
    value: T,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FailureWire {
    #[serde(rename = "ok")]
    _ok: False,
    error: RemoteFailure,
}

struct True;
struct False;

impl<'de> Deserialize<'de> for True {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        if bool::deserialize(deserializer)? {
            Ok(Self)
        } else {
            Err(de::Error::custom("expected true"))
        }
    }
}

impl<'de> Deserialize<'de> for False {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        if bool::deserialize(deserializer)? {
            Err(de::Error::custom("expected false"))
        } else {
            Ok(Self)
        }
    }
}

/// Minimal runtime-schema capability carried by strict generated codecs.
pub trait TypertSchema: Send + Sync + 'static {
    /// Parse and validate one untrusted boundary value.
    ///
    /// # Errors
    ///
    /// Returns the generated schema's boundary diagnostics.
    fn parse(&self, value: TypertBoundaryValue) -> anyhow::Result<TypertBoundaryValue>;

    /// Project a fresh JSON Schema document for this boundary schema.
    ///
    /// # Errors
    ///
    /// Returns projection diagnostics when the schema cannot be represented.
    fn to_json_schema(&self) -> anyhow::Result<Value>;
}

/// Codec attached to one invocation parameter or result.
#[derive(Clone)]
pub enum TypertCodec {
    /// Generated strict schema and its canonical symbol.
    Strict {
        /// Canonical schema symbol.
        type_symbol: String,
        /// Runtime parser.
        schema: Arc<dyn TypertSchema>,
    },
    /// Weak source-launch JSON admission.
    SrcJson,
}

impl std::fmt::Debug for TypertCodec {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Strict { type_symbol, .. } => formatter
                .debug_struct("Strict")
                .field("type_symbol", type_symbol)
                .finish_non_exhaustive(),
            Self::SrcJson => formatter.write_str("SrcJson"),
        }
    }
}

/// Whether one ordered invocation parameter is JSON or a Host lookup.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InvocationParameterSource {
    /// Direct JSON business value.
    Json,
    /// Registered Host lookup identity.
    Lookup,
}

/// One ordered business parameter in a Remote invocation.
#[derive(Clone, Debug)]
pub struct InvocationParameterDescriptor {
    /// Source-level parameter name.
    pub name: String,
    /// Required wire argument key.
    pub wire: String,
    /// JSON or lookup source.
    pub source: InvocationParameterSource,
    /// Lookup key for lookup parameters.
    pub lookup: Option<String>,
    /// Wire codec.
    pub codec: TypertCodec,
    /// Whether an absent field decodes to undefined.
    pub accepts_undefined: Option<bool>,
}

/// Source position retained for generated-definition diagnostics.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InvocationSourceLocation {
    /// Source file.
    pub file: String,
    /// One-based line.
    pub line: u64,
    /// One-based column.
    pub column: u64,
}

/// Receiver selection for one invocation.
#[derive(Clone, Debug)]
pub enum InvocationReceiver {
    /// Invoke the registered service directly.
    Direct,
    /// Resolve a scoped Context receiver.
    Context {
        /// Context-map key.
        context: String,
        /// Wire identity field.
        wire: String,
        /// Wire identity codec.
        codec: TypertCodec,
    },
}

/// Optional projection replacing a direct lookup with the consuming Context.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationScope {
    /// Context-map key.
    pub context: String,
    /// Selected lookup wire field.
    pub wire: String,
}

/// Carrier-independent description of one exported method invocation.
#[derive(Clone, Debug)]
pub struct InvocationDescriptor {
    /// Globally stable generated identity.
    pub id: String,
    /// Owning Cordis service key.
    pub service: String,
    /// Wire namespace.
    pub namespace: String,
    /// Exported endpoint method.
    pub method: String,
    /// Service member when exported under an alias.
    pub implementation: Option<String>,
    /// Receiver selection.
    pub invocation: InvocationReceiver,
    /// Optional consuming-Context projection.
    pub scope: Option<InvocationScope>,
    /// Ordered business parameters.
    pub parameters: Vec<InvocationParameterDescriptor>,
    /// Whether the reserved final Host parameter is `signal`.
    pub cancellation: bool,
    /// Result codec.
    pub result: TypertCodec,
    /// Optional generated source location.
    pub source_location: Option<InvocationSourceLocation>,
}

/// Generated Host contract selected by a Client assembly.
#[derive(Clone, Debug)]
pub struct TypertRemoteContribution {
    /// Package owning the Remote methods.
    pub package: String,
    /// Consumer-side invocation descriptors.
    pub descriptors: Vec<InvocationDescriptor>,
}

/// Runtime provider for one declared Host object lookup.
#[derive(Clone)]
pub struct TypertLookupProvider {
    /// Source parameter recognized by weak parsing.
    pub parameter: String,
    /// Wire field replacing the Host object.
    pub wire: String,
    /// Canonical Host type symbol.
    pub host_type_symbol: String,
    /// Canonical wire type symbol.
    pub wire_type_symbol: String,
    /// Default or configured resolver.
    pub resolve: TypertLookupResolver,
}

impl std::fmt::Debug for TypertLookupProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TypertLookupProvider")
            .field("parameter", &self.parameter)
            .field("wire", &self.wire)
            .field("host_type_symbol", &self.host_type_symbol)
            .field("wire_type_symbol", &self.wire_type_symbol)
            .finish_non_exhaustive()
    }
}

/// Stable declaration retained after a lookup provider unloads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TypertLookupDefinition {
    /// Merge-declared lookup key.
    pub key: String,
    /// Source parameter.
    pub parameter: String,
    /// Wire field.
    pub wire: String,
    /// Canonical Host type symbol.
    pub host_type_symbol: String,
    /// Canonical wire type symbol.
    pub wire_type_symbol: String,
}

/// Host resolver for one scoped Remote Context kind.
#[derive(Clone)]
pub struct TypertHostContextProvider {
    /// Wire identity field.
    pub wire: String,
    /// Canonical wire type symbol.
    pub wire_type_symbol: String,
    /// Default or configured resolver.
    pub resolve: TypertHostContextResolver,
}

impl std::fmt::Debug for TypertHostContextProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TypertHostContextProvider")
            .field("wire", &self.wire)
            .field("wire_type_symbol", &self.wire_type_symbol)
            .finish_non_exhaustive()
    }
}

/// Client resolver for the identity represented by a calling Context.
#[derive(Clone)]
pub struct TypertClientContextBinder {
    /// Read a wire identity or report a wrong scope.
    pub identity: TypertClientContextResolver,
}

impl std::fmt::Debug for TypertClientContextBinder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TypertClientContextBinder")
            .finish_non_exhaustive()
    }
}

/// Runtime registry change vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TypertRegistryChangeKind {
    /// Current-environment descriptor.
    Local,
    /// Consumer-selected Remote descriptor.
    Remote,
    /// Host object lookup.
    Lookup,
    /// Host Context provider or override.
    HostContext,
    /// Client Context binder.
    ClientContext,
}

/// Notification emitted after a runtime registry changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypertRegistryChange {
    /// Changed registry.
    pub kind: TypertRegistryChangeKind,
    /// Endpoint or provider key.
    pub key: String,
}

/// Synchronous contained registry observer.
pub type TypertRegistryListener = Arc<dyn Fn(TypertRegistryChange) + Send + Sync>;

/// Common disposer used by Cordis-owned Typert registrations.
pub type TypertDisposer = EffectHandle;

/// Current-environment invocation-definition contract.
pub trait TypertLocalRegistry: Send + Sync {
    /// Looks up one endpoint.
    fn get(&self, endpoint: &str) -> Option<InvocationDescriptor>;
    /// Reports whether a strict definition has ever existed.
    fn has_seen(&self, endpoint: &str) -> bool;
    /// Returns a registration-order snapshot.
    fn list(&self) -> Vec<InvocationDescriptor>;
    /// Subscribes within the calling Context's fiber.
    ///
    /// # Errors
    ///
    /// Returns an inactive-owner failure.
    fn subscribe(
        &self,
        context: &Context,
        listener: TypertRegistryListener,
    ) -> anyhow::Result<TypertDisposer>;
}

/// Consumer-selected Remote contribution contract.
pub trait TypertRemoteRegistry: Send + Sync {
    /// Registers one generated contribution.
    ///
    /// # Errors
    ///
    /// Rejects invalid or duplicate contributions and inactive owners.
    fn register(
        &self,
        context: &Context,
        contribution: TypertRemoteContribution,
    ) -> anyhow::Result<TypertDisposer>;
    /// Looks up one endpoint.
    fn get(&self, endpoint: &str) -> Option<InvocationDescriptor>;
    /// Returns a registration-order snapshot.
    fn list(&self) -> Vec<InvocationDescriptor>;
    /// Subscribes within the calling Context's fiber.
    ///
    /// # Errors
    ///
    /// Returns an inactive-owner failure.
    fn subscribe(
        &self,
        context: &Context,
        listener: TypertRegistryListener,
    ) -> anyhow::Result<TypertDisposer>;
}

/// Runtime lookup-provider contract.
pub trait TypertLookupRegistry: Send + Sync {
    /// Registers one provider.
    ///
    /// # Errors
    ///
    /// Rejects invalid, conflicting, duplicate, or inactive registrations.
    fn register(
        &self,
        context: &Context,
        key: &str,
        provider: TypertLookupProvider,
    ) -> anyhow::Result<TypertDisposer>;
    /// Configures an overriding resolution policy.
    ///
    /// # Errors
    ///
    /// Rejects invalid, duplicate, or inactive configurations.
    fn configure(
        &self,
        context: &Context,
        key: &str,
        resolver: TypertLookupResolver,
    ) -> anyhow::Result<TypertDisposer>;
    /// Gets one live effective provider.
    fn get(&self, key: &str) -> Option<TypertLookupProvider>;
    /// Returns lifetime-stable declarations.
    fn definitions(&self) -> Vec<TypertLookupDefinition>;
    /// Returns live provider keys.
    fn keys(&self) -> Vec<String>;
    /// Subscribes within the calling Context's fiber.
    ///
    /// # Errors
    ///
    /// Returns an inactive-owner failure.
    fn subscribe(
        &self,
        context: &Context,
        listener: TypertRegistryListener,
    ) -> anyhow::Result<TypertDisposer>;
}

/// Runtime Host and Client Context-provider contract.
pub trait TypertContextRegistry: Send + Sync {
    /// Registers a Host Context provider.
    ///
    /// # Errors
    ///
    /// Rejects invalid, duplicate, or inactive registrations.
    fn register_host(
        &self,
        context: &Context,
        key: &str,
        provider: TypertHostContextProvider,
    ) -> anyhow::Result<TypertDisposer>;
    /// Configures one Host resolution policy.
    ///
    /// # Errors
    ///
    /// Rejects invalid, duplicate, or inactive configurations.
    fn configure_host(
        &self,
        context: &Context,
        key: &str,
        resolver: TypertHostContextResolver,
    ) -> anyhow::Result<TypertDisposer>;
    /// Registers a Client Context binder.
    ///
    /// # Errors
    ///
    /// Rejects invalid, duplicate, or inactive registrations.
    fn register_client(
        &self,
        context: &Context,
        key: &str,
        binder: TypertClientContextBinder,
    ) -> anyhow::Result<TypertDisposer>;
    /// Gets a live effective Host provider.
    fn get_host(&self, key: &str) -> Option<TypertHostContextProvider>;
    /// Gets a live Client binder.
    fn get_client(&self, key: &str) -> Option<TypertClientContextBinder>;
    /// Subscribes within the calling Context's fiber.
    ///
    /// # Errors
    ///
    /// Returns an inactive-owner failure.
    fn subscribe(
        &self,
        context: &Context,
        listener: TypertRegistryListener,
    ) -> anyhow::Result<TypertDisposer>;
}

/// Minimal runtime registry consumed through dependency inversion.
pub trait TypertRegistryContract: Send + Sync {
    /// Current-environment definitions.
    fn local(&self) -> &dyn TypertLocalRegistry;
    /// Consumer-selected Remote definitions.
    fn remotes(&self) -> &dyn TypertRemoteRegistry;
    /// Host object lookups.
    fn lookups(&self) -> &dyn TypertLookupRegistry;
    /// Host and Client Context providers.
    fn contexts(&self) -> &dyn TypertContextRegistry;
}
