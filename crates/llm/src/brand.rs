//! Stable cross-boundary identifiers.

seekdeep_util::string_brand!(
    /// Stable message identity.
    pub struct MessageId;
);
seekdeep_util::string_brand!(
    /// Model tool-call correlation identity.
    pub struct CallId;
);
seekdeep_util::string_brand!(
    /// Provider-issued diagnostic request identity.
    pub struct ProviderRequestId;
);
seekdeep_util::string_brand!(
    /// Adapter-owned reasoning-effort identity.
    pub struct ReasoningEffortId;
);
seekdeep_util::string_brand!(
    /// Registered provider-route identity.
    pub struct ProviderId;
);
seekdeep_util::string_brand!(
    /// Provider-owned model identity.
    pub struct ModelId;
);
pub use seekdeep_identity::SessionId;
