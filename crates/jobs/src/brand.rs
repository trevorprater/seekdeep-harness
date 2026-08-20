//! The owned branded job id, carried across the registry, the model-facing
//! control surface, and the client wire.

seekdeep_util::string_brand!(
    /// Identifies a background job. The registry generates a kind-ordinal id;
    /// predictable ids rely on owner authorization rather than secrecy.
    pub struct JobId;
);
