//! Redacted user-settings wire contracts.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{
    host::EmptyRequest,
    rpc::ContractError,
    sessions::{
        parse_array, require_array, require_bool, require_field, require_literal_true,
        require_nonempty_string, require_number, require_object, require_string,
    },
};

/// When a settings owner applies changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SettingsApplies {
    /// Apply immediately.
    Live,
    /// Apply after restart.
    Restart,
}

/// One schema-declared redacted secret slot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsSecretView {
    /// Path from section root to removed field.
    pub path: Vec<String>,
    /// Whether the slot currently holds a value.
    pub set: bool,
}

impl SettingsSecretView {
    fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        let path = require_array(object, "path", "$.path")?;
        if !path.iter().all(Value::is_string) {
            return Err(ContractError::new("$.path", "expected string array"));
        }
        Ok(Self {
            path: path
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect(),
            set: require_bool(object, "set", "$.set")?,
        })
    }
}

/// Redacted wire view of one registered settings namespace.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsNamespaceView {
    /// Non-empty namespace key.
    pub ns: String,
    /// Serialized schemastery envelope.
    pub schema: Value,
    /// Redacted resolved value.
    pub value: Value,
    /// Optional redacted composition base; explicit null is preserved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<Value>,
    /// Optional redacted raw user section; explicit null is preserved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<Value>,
    /// Live or restart application posture.
    pub applies: SettingsApplies,
    /// Every schema-declared secret slot.
    pub secrets: Vec<SettingsSecretView>,
    /// Monotonic raw-user-section revision.
    pub revision: f64,
}

impl SettingsNamespaceView {
    /// Parses one redacted settings namespace view.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed required members, applies mode, secrets, or revision.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        let applies = match require_string(object, "applies", "$.applies", false)? {
            "live" => SettingsApplies::Live,
            "restart" => SettingsApplies::Restart,
            _ => return Err(ContractError::new("$.applies", "unknown applies mode")),
        };
        Ok(Self {
            ns: require_nonempty_string(object, "ns", "$.ns")?.to_owned(),
            schema: require_field(object, "schema", "$.schema")?.clone(),
            value: require_field(object, "value", "$.value")?.clone(),
            base: object.get("base").cloned(),
            user: object.get("user").cloned(),
            applies,
            secrets: parse_array(
                require_array(object, "secrets", "$.secrets")?,
                SettingsSecretView::parse,
                "$.secrets",
            )?,
            revision: require_number(object, "revision", "$.revision")?,
        })
    }
}

impl<'de> Deserialize<'de> for SettingsNamespaceView {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// `settings.describe` request.
pub type SettingsDescribeRequest = EmptyRequest;
/// `settings.openDocument` request.
pub type SettingsOpenDocumentRequest = EmptyRequest;

/// `settings.describe` response value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsDescribeValue {
    /// Whether the provider accepts writes.
    pub writable: bool,
    /// Whether a local document exists without exposing its path.
    pub has_document: bool,
    /// Registered namespace views.
    pub namespaces: Vec<SettingsNamespaceView>,
}

impl SettingsDescribeValue {
    /// Parses a `settings.describe` response value.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed capability flags or namespace rows.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        Ok(Self {
            writable: require_bool(object, "writable", "$.writable")?,
            has_document: require_bool(object, "hasDocument", "$.hasDocument")?,
            namespaces: parse_array(
                require_array(object, "namespaces", "$.namespaces")?,
                SettingsNamespaceView::parse,
                "$.namespaces",
            )?,
        })
    }
}

/// `settings.openDocument` response.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsOpenDocumentValue {
    /// Must be literal true.
    pub opened: bool,
}

impl SettingsOpenDocumentValue {
    /// Parses a successful settings-document open response.
    ///
    /// # Errors
    ///
    /// Returns an error unless `opened` is literal true.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_literal_true(object, "opened", "$.opened")?;
        Ok(Self { opened: true })
    }
}

/// Merge patch request for `settings.update`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsUpdateRequest {
    /// Non-empty namespace.
    pub ns: String,
    /// Object patch, including write-only secrets when supplied.
    pub patch: Map<String, Value>,
    /// Optional optimistic-concurrency revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<f64>,
}

impl SettingsUpdateRequest {
    /// Parses a `settings.update` request.
    ///
    /// # Errors
    ///
    /// Returns an error for empty namespace, non-object patch, or malformed revision.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        Ok(Self {
            ns: require_nonempty_string(object, "ns", "$.ns")?.to_owned(),
            patch: require_object(require_field(object, "patch", "$.patch")?, "$.patch")?.clone(),
            expected_revision: optional_number(object, "expectedRevision", "$.expectedRevision")?,
        })
    }
}

/// Wholesale replacement request for `settings.replace`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsReplaceRequest {
    /// Non-empty namespace.
    pub ns: String,
    /// Complete raw user section.
    pub section: Map<String, Value>,
    /// Optional optimistic-concurrency revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<f64>,
}

impl SettingsReplaceRequest {
    /// Parses a `settings.replace` request.
    ///
    /// # Errors
    ///
    /// Returns an error for empty namespace, non-object section, or malformed revision.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        Ok(Self {
            ns: require_nonempty_string(object, "ns", "$.ns")?.to_owned(),
            section: require_object(require_field(object, "section", "$.section")?, "$.section")?
                .clone(),
            expected_revision: optional_number(object, "expectedRevision", "$.expectedRevision")?,
        })
    }
}

/// One path-addressed settings edit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum SettingsPathOpView {
    /// Set a value, including null.
    Set {
        /// Path from section root; empty addresses root.
        path: Vec<String>,
        /// Exact replacement value.
        value: Value,
    },
    /// Remove a value.
    Unset {
        /// Path from section root; empty addresses root.
        path: Vec<String>,
    },
}

impl SettingsPathOpView {
    fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        let path = require_array(object, "path", "$.path")?;
        if !path.iter().all(Value::is_string) {
            return Err(ContractError::new("$.path", "expected string array"));
        }
        let path = path
            .iter()
            .filter_map(Value::as_str)
            .map(ToOwned::to_owned)
            .collect();
        match require_string(object, "op", "$.op", false)? {
            "set" => Ok(Self::Set {
                path,
                value: require_field(object, "value", "$.value")?.clone(),
            }),
            "unset" => Ok(Self::Unset { path }),
            _ => Err(ContractError::new(
                "$.op",
                "unknown settings path operation",
            )),
        }
    }
}

/// Path-operation request for `settings.mutate`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsMutateRequest {
    /// Non-empty namespace.
    pub ns: String,
    /// Ordered path operations.
    pub ops: Vec<SettingsPathOpView>,
    /// Optional optimistic-concurrency revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<f64>,
}

impl SettingsMutateRequest {
    /// Parses a `settings.mutate` request.
    ///
    /// # Errors
    ///
    /// Returns an error for empty namespace, malformed operations, or malformed revision.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        Ok(Self {
            ns: require_nonempty_string(object, "ns", "$.ns")?.to_owned(),
            ops: parse_array(
                require_array(object, "ops", "$.ops")?,
                SettingsPathOpView::parse,
                "$.ops",
            )?,
            expected_revision: optional_number(object, "expectedRevision", "$.expectedRevision")?,
        })
    }
}

/// `settings.update` response.
pub type SettingsUpdateValue = SettingsNamespaceView;
/// `settings.replace` response.
pub type SettingsReplaceValue = SettingsNamespaceView;
/// `settings.mutate` response.
pub type SettingsMutateValue = SettingsNamespaceView;

fn optional_number(
    object: &Map<String, Value>,
    name: &str,
    path: &str,
) -> Result<Option<f64>, ContractError> {
    object
        .get(name)
        .map(|_| require_number(object, name, path))
        .transpose()
}
