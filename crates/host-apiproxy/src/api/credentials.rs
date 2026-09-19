//! Value-free credential-state and write-only credential request contracts.

use std::collections::BTreeMap;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    host::EmptyRequest,
    rpc::ContractError,
    sessions::{
        optional_string, require_array, require_bool, require_field, require_object, require_string,
    },
};

/// Maximum credential references in one describe batch.
pub const CREDENTIAL_DESCRIBE_MAX_REFS: usize = 64;

/// One credential reference's structurally value-free state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialView {
    /// Whether any layer supplies a non-empty value.
    pub configured: bool,
    /// Optional winning provider layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Whether the writable layer can affect this reference.
    pub writable: bool,
}

impl CredentialView {
    fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        optional_string(object, "source", "$.source", false)?;
        Ok(Self {
            configured: require_bool(object, "configured", "$.configured")?,
            source: object
                .get("source")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            writable: require_bool(object, "writable", "$.writable")?,
        })
    }
}

/// `credentials.describe` request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialsDescribeRequest {
    /// At most 64 portable environment-style reference names.
    pub refs: Vec<String>,
}

impl CredentialsDescribeRequest {
    /// Parses a batched credential describe request.
    ///
    /// # Errors
    ///
    /// Returns an error for a non-array, more than 64 refs, or an invalid reference name.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        let refs = require_array(object, "refs", "$.refs")?;
        if refs.len() > CREDENTIAL_DESCRIBE_MAX_REFS {
            return Err(ContractError::new("$.refs", "array is too long"));
        }
        let mut parsed = Vec::with_capacity(refs.len());
        for (index, value) in refs.iter().enumerate() {
            let reference = value
                .as_str()
                .ok_or_else(|| ContractError::new(format!("$.refs[{index}]"), "expected string"))?;
            validate_ref(reference, &format!("$.refs[{index}]"))?;
            parsed.push(reference.to_owned());
        }
        Ok(Self { refs: parsed })
    }
}

/// `credentials.describe` response value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialsDescribeValue {
    /// Credential states keyed by requested reference.
    pub credentials: BTreeMap<String, CredentialView>,
}

impl CredentialsDescribeValue {
    /// Parses a credential-state map.
    ///
    /// # Errors
    ///
    /// Returns an error unless every map value is a valid value-free credential view.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        let credentials = require_object(
            require_field(object, "credentials", "$.credentials")?,
            "$.credentials",
        )?;
        let mut parsed = BTreeMap::new();
        for (reference, view) in credentials {
            parsed.insert(reference.clone(), CredentialView::parse(view)?);
        }
        Ok(Self {
            credentials: parsed,
        })
    }
}

/// `credentials.set` request; the only direction a credential value crosses.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialsSetRequest {
    /// Portable reference name.
    #[serde(rename = "ref")]
    pub reference: String,
    /// Non-empty secret value.
    pub value: String,
}

impl CredentialsSetRequest {
    /// Parses a credential set request.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid reference or empty/non-string value.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        let reference = require_string(object, "ref", "$.ref", false)?;
        validate_ref(reference, "$.ref")?;
        Ok(Self {
            reference: reference.to_owned(),
            value: require_string(object, "value", "$.value", true)?.to_owned(),
        })
    }
}

/// `credentials.unset` request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialsUnsetRequest {
    /// Portable reference name.
    #[serde(rename = "ref")]
    pub reference: String,
}

impl CredentialsUnsetRequest {
    /// Parses a credential unset request.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid reference name.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        let reference = require_string(object, "ref", "$.ref", false)?;
        validate_ref(reference, "$.ref")?;
        Ok(Self {
            reference: reference.to_owned(),
        })
    }
}

/// `credentials.set` response.
pub type CredentialsSetValue = EmptyRequest;
/// `credentials.unset` response.
pub type CredentialsUnsetValue = EmptyRequest;

fn validate_ref(reference: &str, path: &str) -> Result<(), ContractError> {
    let pattern = Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").expect("constant regex");
    if !pattern.is_match(reference) {
        return Err(ContractError::new(path, "invalid credential reference"));
    }
    Ok(())
}
