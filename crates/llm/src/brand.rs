//! Stable cross-boundary identifiers.

macro_rules! string_id {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            #[doc = concat!("Brands a raw `", stringify!($name), "` string.")]
            #[must_use]
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// Returns the opaque protocol string.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

string_id!(MessageId, "Stable message identity.");
string_id!(CallId, "Model tool-call correlation identity.");
string_id!(
    ProviderRequestId,
    "Provider-issued diagnostic request identity."
);
string_id!(
    ReasoningEffortId,
    "Adapter-owned reasoning-effort identity."
);
