//! Credential-reference capability seam.

use std::sync::Arc;

use async_trait::async_trait;
use seekdeep_cordis::{Context, EventArgs, ServiceKey, fiber::EffectHandle};
use seekdeep_invariants::InvariantError;
use serde::{Deserialize, Serialize};

/// Package-owned credential lifecycle invariant.
pub mod invariant;

pub use invariant::{INVARIANT_NAME, register_invariant};

seekdeep_util::string_brand!(
    /// Nominal reference to one provider-owned credential.
    pub struct CredentialRef;
);

/// Typed Cordis seat corresponding to `ctx.credentials`.
pub const CREDENTIALS: ServiceKey<CredentialService> = ServiceKey::new("credentials");
const REF_PATTERN: &str = "/^[A-Za-z_][A-Za-z0-9_]*$/";

/// Validates and brands a POSIX shell identifier.
///
/// # Errors
///
/// Rejects empty, digit-leading, non-ASCII, or punctuation-bearing values.
pub fn credential_ref(value: impl Into<String>) -> anyhow::Result<CredentialRef> {
    let value = value.into();
    let mut characters = value.bytes();
    let valid = characters
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && characters.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    anyhow::ensure!(valid, "credential ref \"{value}\" must match {REF_PATTERN}");
    Ok(CredentialRef::new(value))
}

/// One current non-empty value and its provider-defined source layer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedCredential {
    /// Secret value. Configuration surfaces must never receive this structure.
    pub value: String,
    /// Provider-defined layer identifier.
    pub source: String,
}

/// Value-free configuration facts for one credential reference.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialInfo {
    /// Whether resolution currently returns a non-empty value.
    pub configured: bool,
    /// Supplying source when configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Whether a provider-managed write would currently succeed.
    pub writable: bool,
}

/// Provider implementation over one or more secret-bearing source layers.
#[async_trait]
pub trait CredentialProvider: Send + Sync + 'static {
    /// Resolves a current non-empty value.
    async fn resolve(
        &self,
        reference: &CredentialRef,
    ) -> anyhow::Result<Option<ResolvedCredential>>;

    /// Describes configuration without revealing the value.
    async fn describe(&self, reference: &CredentialRef) -> anyhow::Result<CredentialInfo>;

    /// Stores one non-empty value in the writable layer.
    async fn set(&self, reference: &CredentialRef, value: &str) -> anyhow::Result<()>;

    /// Removes one value from the writable layer.
    async fn unset(&self, reference: &CredentialRef) -> anyhow::Result<()>;
}

/// Commit-event emitter supplied to provider implementations.
#[derive(Clone, Debug)]
pub struct CredentialNotifier {
    context: Context,
}

impl CredentialNotifier {
    /// Creates a notifier bound to the provider's lifecycle context.
    #[must_use]
    pub fn new(context: &Context) -> Self {
        Self {
            context: context.clone(),
        }
    }

    /// Fans a committed update out while containing ordinary observer failures.
    ///
    /// # Errors
    ///
    /// Returns the first synchronous invariant failure after every synchronous
    /// observer has run. Ordinary failures are logged and contained.
    pub fn notify_updated(&self, reference: &CredentialRef) -> anyhow::Result<()> {
        let emission = self.context.events().prepare_emit(
            &self.context,
            "credentials/updated",
            &EventArgs::one(reference.clone()),
        )?;
        let mut invariant_failure = None;
        emission.emit_contained(|error| {
            if error.downcast_ref::<InvariantError>().is_some() && invariant_failure.is_none() {
                invariant_failure = Some(error);
            } else {
                tracing::warn!(credential = %reference, %error, "credentials/updated listener failed");
            }
        });
        if let Some(error) = invariant_failure {
            return Err(error);
        }
        Ok(())
    }
}

/// Live provider facade published through Cordis.
pub struct CredentialService {
    provider: Arc<dyn CredentialProvider>,
}

impl std::fmt::Debug for CredentialService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CredentialService")
            .finish_non_exhaustive()
    }
}

impl CredentialService {
    /// Wraps one provider implementation.
    #[must_use]
    pub fn new(provider: Arc<dyn CredentialProvider>) -> Arc<Self> {
        Arc::new(Self { provider })
    }

    /// Publishes the service for the owner context's lifetime.
    ///
    /// # Errors
    ///
    /// Returns duplicate-service or inactive-owner failures.
    pub fn provide(self: &Arc<Self>, context: &Context) -> anyhow::Result<EffectHandle> {
        Ok(context.provide(CREDENTIALS, self.clone())?)
    }

    /// Resolves one reference for the current operation.
    ///
    /// # Errors
    ///
    /// Propagates provider failures.
    pub async fn resolve(
        &self,
        reference: &CredentialRef,
    ) -> anyhow::Result<Option<ResolvedCredential>> {
        self.provider.resolve(reference).await
    }

    /// Describes one reference without exposing its value.
    ///
    /// # Errors
    ///
    /// Propagates provider failures.
    pub async fn describe(&self, reference: &CredentialRef) -> anyhow::Result<CredentialInfo> {
        self.provider.describe(reference).await
    }

    /// Stores one non-empty credential value.
    ///
    /// # Errors
    ///
    /// Propagates provider validation, storage, or invariant failures.
    pub async fn set(&self, reference: &CredentialRef, value: &str) -> anyhow::Result<()> {
        self.provider.set(reference, value).await
    }

    /// Removes one provider-managed credential value.
    ///
    /// # Errors
    ///
    /// Propagates provider storage or invariant failures.
    pub async fn unset(&self, reference: &CredentialRef) -> anyhow::Result<()> {
        self.provider.unset(reference).await
    }
}
