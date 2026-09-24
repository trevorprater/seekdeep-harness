//! Carrier-independent Gateway request and failure contracts.

use indexmap::IndexMap;
use seekdeep_llm::AbortSignal;
use seekdeep_typert_protocol::TypertBoundaryValue;
use serde::{Deserialize, Serialize};

/// One Remote request after a carrier decoded its envelope.
#[derive(Clone, Debug)]
pub struct InvokeRemoteRequest {
    /// Remote namespace.
    pub namespace: String,
    /// Exported method.
    pub method: String,
    /// Exact named wire values, including explicit `Undefined` for direct calls.
    pub args: IndexMap<String, TypertBoundaryValue>,
    /// Optional carrier or direct-caller cancellation.
    pub signal: Option<AbortSignal>,
}

/// Stable infrastructure and boundary failures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TypertGatewayErrorCode {
    /// Multiple active source-mode services claim an endpoint.
    AmbiguousEndpoint,
    /// Named fields do not exactly match the descriptor.
    ArgumentsInvalid,
    /// Live service binding is missing or inconsistent.
    BindingInvalid,
    /// Context provider threw.
    ContextFailed,
    /// Context identity was not found.
    ContextNotFound,
    /// Context provider is absent.
    ContextUnavailable,
    /// A previously observed strict definition was withdrawn.
    DefinitionUnavailable,
    /// Parameter codec rejected a value.
    InputInvalid,
    /// No active Remote method exports the endpoint.
    InvocationUnavailable,
    /// Lookup provider threw.
    LookupFailed,
    /// Lookup identity was not found.
    LookupNotFound,
    /// Lookup provider is absent or descriptor lacks its key.
    LookupUnavailable,
    /// Service implementation member is unavailable.
    MethodUnavailable,
    /// Provider metadata disagrees with the strict descriptor.
    ProviderMismatch,
    /// Result codec rejected a business value.
    ResultInvalid,
    /// Active Cordis service is unavailable.
    ServiceUnavailable,
    /// Source-mode method signature is not representable.
    SignatureInvalid,
}
