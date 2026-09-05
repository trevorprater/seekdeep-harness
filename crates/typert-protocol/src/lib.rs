//! Compiler-independent Typert declarations shared by services and carriers.

pub mod invariant;
pub mod types;

use std::sync::Arc;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

pub use types::*;

/// Declares a direct Remote method while compile-checking the implementation.
///
/// ```compile_fail
/// struct Service;
/// let _ = seekdeep_typert_protocol::typert_remote_method!(Service, missing);
/// ```
#[macro_export]
macro_rules! typert_remote_method {
    ($service:ty, $method:ident) => {{
        let _implementation = <$service>::$method;
        $crate::RemoteMethodMarker::direct(stringify!($method), None)
            .expect("Rust method names are valid Typert endpoint segments")
    }};
    ($service:ty, $method:ident => $export:literal) => {{
        let _implementation = <$service>::$method;
        $crate::RemoteMethodMarker::direct(stringify!($method), Some($export))
            .expect("the generated Typert export name must be valid")
    }};
}

/// Declares a scoped Remote method while compile-checking the implementation.
#[macro_export]
macro_rules! typert_remote_scope_method {
    ($service:ty, $method:ident, $context:literal) => {{
        let _implementation = <$service>::$method;
        $crate::RemoteMethodMarker::scoped(stringify!($method), $context, None)
            .expect("the generated Typert Context key must be valid")
    }};
    ($service:ty, $method:ident, $context:literal => $export:literal) => {{
        let _implementation = <$service>::$method;
        $crate::RemoteMethodMarker::scoped(stringify!($method), $context, Some($export))
            .expect("the generated Typert Context and export names must be valid")
    }};
}

/// Test one generated Remote name against the shared RPC segment grammar.
#[must_use]
pub fn is_typert_remote_segment(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$' | b'.' | b'-'))
}

/// A lookup policy rejection carrying adapter-owned structured failure data.
#[derive(Clone, Debug, PartialEq)]
pub struct TypertLookupFailure {
    /// Adapter-owned failure returned to the caller.
    pub failure: serde_json::Value,
}

impl TypertLookupFailure {
    /// Wraps one policy failure without exposing the rejected identity.
    #[must_use]
    pub const fn new(failure: serde_json::Value) -> Self {
        Self { failure }
    }
}

impl std::fmt::Display for TypertLookupFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Typert lookup policy rejected the requested identity")
    }
}

impl std::error::Error for TypertLookupFailure {}

/// Options for an explicit service-to-Gateway binding.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TypertGatewayBindingOptions {
    /// Wire namespace; defaults to the Cordis service key.
    pub namespace: Option<String>,
}

/// Visible declaration binding one live service to a key and namespace.
#[derive(Clone, Debug)]
pub struct TypertGatewayBinding<Service: ?Sized> {
    /// Exact live service instance.
    pub service: Arc<Service>,
    /// Cordis service key.
    pub service_key: String,
    /// Wire namespace.
    pub namespace: String,
}

impl<Service: ?Sized> PartialEq for TypertGatewayBinding<Service> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.service, &other.service)
            && self.service_key == other.service_key
            && self.namespace == other.namespace
    }
}

impl<Service: ?Sized> Eq for TypertGatewayBinding<Service> {}

/// Binds one live service instance to a validated Cordis key and namespace.
///
/// # Errors
///
/// Rejects names outside the RPC endpoint segment grammar.
pub fn bind_typert_remote<Service: ?Sized>(
    service: Arc<Service>,
    service_key: impl Into<String>,
    options: TypertGatewayBindingOptions,
) -> anyhow::Result<TypertGatewayBinding<Service>> {
    let service_key = service_key.into();
    validate_name("service key", &service_key)?;
    let namespace = options.namespace.unwrap_or_else(|| service_key.clone());
    validate_name("namespace", &namespace)?;
    Ok(TypertGatewayBinding {
        service,
        service_key,
        namespace,
    })
}

/// Invocation mode recorded by a Remote declaration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum RemoteInvocationMarker {
    /// Invoke the registered service directly.
    Direct,
    /// Resolve the receiver from a declared Context kind.
    Context {
        /// Context-map key.
        context: String,
    },
}

/// One Remote method declaration discovered for a live service.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteMethodMarker {
    /// Public implementation member.
    pub method: String,
    /// Optional endpoint alias.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export_name: Option<String>,
    /// Receiver selection mode.
    pub invocation: RemoteInvocationMarker,
}

impl RemoteMethodMarker {
    /// Declares a direct Remote method.
    ///
    /// # Errors
    ///
    /// Rejects malformed method and export names.
    pub fn direct(method: impl Into<String>, export_name: Option<&str>) -> anyhow::Result<Self> {
        Self::new(method.into(), export_name, RemoteInvocationMarker::Direct)
    }

    /// Declares a Context-resolved Remote method.
    ///
    /// # Errors
    ///
    /// Rejects malformed method, Context, and export names.
    pub fn scoped(
        method: impl Into<String>,
        context: impl Into<String>,
        export_name: Option<&str>,
    ) -> anyhow::Result<Self> {
        let context = context.into();
        validate_name("Scope key", &context)?;
        Self::new(
            method.into(),
            export_name,
            RemoteInvocationMarker::Context { context },
        )
    }

    fn new(
        method: String,
        export_name: Option<&str>,
        invocation: RemoteInvocationMarker,
    ) -> anyhow::Result<Self> {
        validate_name("Remote method name", &method)?;
        let export_name = export_name
            .filter(|name| *name != method)
            .map(str::to_owned);
        if let Some(name) = &export_name {
            validate_name("Remote export name", name)?;
        }
        Ok(Self {
            method,
            export_name,
            invocation,
        })
    }
}

/// Private declaration table used by generated or handwritten Rust services.
#[derive(Clone, Debug, Default)]
pub struct RemoteMethodTable {
    markers: IndexMap<String, RemoteMethodMarker>,
}

impl RemoteMethodTable {
    /// Records one marker idempotently.
    ///
    /// # Errors
    ///
    /// Rejects a conflicting marker for the same implementation member.
    pub fn mark(&mut self, marker: RemoteMethodMarker) -> anyhow::Result<()> {
        if let Some(current) = self.markers.get(&marker.method) {
            anyhow::ensure!(
                current == &marker,
                "typert-protocol: Remote method {:?} has conflicting invocation markers",
                marker.method
            );
            return Ok(());
        }
        self.markers.insert(marker.method.clone(), marker);
        Ok(())
    }

    /// Returns a detached declaration-order snapshot.
    #[must_use]
    pub fn snapshot(&self) -> Vec<RemoteMethodMarker> {
        self.markers.values().cloned().collect()
    }
}

/// Native Rust equivalent of `TypertRemoteService` plus Remote decorators.
///
/// Implementations expose immutable declarations explicitly; no runtime
/// reflection, constructor mutation, or unsafe self-reference is required.
pub trait TypertRemoteService: Send + Sync + 'static {
    /// Exact Cordis service key.
    fn typert_service_key(&self) -> &str;

    /// Wire namespace, defaulting to the service key.
    fn typert_namespace(&self) -> &str {
        self.typert_service_key()
    }

    /// Declaration-order Remote method snapshot.
    fn remote_methods(&self) -> Vec<RemoteMethodMarker>;

    /// Creates a visible binding for this exact live instance.
    ///
    /// # Errors
    ///
    /// Rejects malformed service or namespace names.
    fn typert_remote(self: &Arc<Self>) -> anyhow::Result<TypertGatewayBinding<Self>> {
        bind_typert_remote(
            self.clone(),
            self.typert_service_key(),
            TypertGatewayBindingOptions {
                namespace: Some(self.typert_namespace().to_owned()),
            },
        )
    }
}

fn validate_name(subject: &str, value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        is_typert_remote_segment(value),
        "typert-protocol: {subject} must contain only RPC endpoint segment characters"
    );
    Ok(())
}
