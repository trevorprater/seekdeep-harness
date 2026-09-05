//! Opaque identities persisted by the retry executor.

seekdeep_util::string_brand!(
    /// Stable identity shared by every attempt in one provider-policy chain.
    pub struct RetryId;
);

seekdeep_util::string_brand!(
    /// Canonical identity of every behavior-affecting resolved-policy field.
    pub struct RetryPolicyKey;
);
