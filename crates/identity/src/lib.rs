//! Target-portable identities shared by Host and Rust/WASM Client crates.

/// Opaque RPC correlation identity minted by the request initiator.
#[repr(transparent)]
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct RpcId(String);

impl RpcId {
    /// Brands an arbitrary string without runtime validation, matching the protocol contract.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the opaque identifier string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RpcId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Stable durable model-message identity.
#[repr(transparent)]
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct MessageId(String);

impl MessageId {
    /// Brands one exact protocol string.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the ordinary protocol string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Removes the nominal brand.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

impl std::fmt::Display for MessageId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::ops::Deref for MessageId {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<str> for MessageId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::borrow::Borrow<str> for MessageId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl From<String> for MessageId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for MessageId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

/// Session identity stamped onto routed model and Client requests.
#[repr(transparent)]
#[derive(
    Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct SessionId(String);

impl SessionId {
    /// Brands one exact protocol string.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the ordinary protocol string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Removes the nominal brand.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::ops::Deref for SessionId {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<str> for SessionId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::borrow::Borrow<str> for SessionId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl From<String> for SessionId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for SessionId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}
