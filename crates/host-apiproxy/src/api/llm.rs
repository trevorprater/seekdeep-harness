//! Host-scoped LLM provider topology and discovery contracts.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    host::EmptyRequest,
    rpc::ContractError,
    sessions::{
        ModelCatalogFailure, ModelProviderGroup, optional_nonempty_string, parse_array,
        require_array, require_bool, require_nonempty_string, require_object, require_string,
    },
};

/// Authentication setup declared by an adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderAuthentication {
    /// API key.
    ApiKey,
    /// Provider-native browser/device flow.
    ProviderNative,
    /// ChatGPT/Codex OAuth credentials.
    CodexOauth,
}

/// One configurable provider wire view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigurableProviderView {
    /// Non-empty provider route.
    pub provider: String,
    /// Non-empty display name.
    pub display_name: String,
    /// Settings namespace, permitted to be empty for an undeclared live route.
    pub settings_ns: String,
    /// Path to provider profile inside the namespace.
    pub settings_path: Vec<String>,
    /// Optional declared authentication setup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authentication: Option<ProviderAuthentication>,
    /// Whether the route is live.
    pub active: bool,
    /// Optional distinction between declared and discovered route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared: Option<bool>,
}

impl ConfigurableProviderView {
    fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_nonempty_string(object, "provider", "$.provider")?;
        require_nonempty_string(object, "displayName", "$.displayName")?;
        require_string(object, "settingsNs", "$.settingsNs", false)?;
        let path = require_array(object, "settingsPath", "$.settingsPath")?;
        if !path.iter().all(Value::is_string) {
            return Err(ContractError::new(
                "$.settingsPath",
                "expected string array",
            ));
        }
        if let Some(authentication) = object.get("authentication")
            && !matches!(
                authentication.as_str(),
                Some("api-key" | "provider-native" | "codex-oauth")
            )
        {
            return Err(ContractError::new(
                "$.authentication",
                "unknown authentication setup",
            ));
        }
        require_bool(object, "active", "$.active")?;
        if object.contains_key("declared") {
            require_bool(object, "declared", "$.declared")?;
        }
        serde_json::from_value(value.clone())
            .map_err(|error| ContractError::new("$", error.to_string()))
    }
}

/// `llm.providers` request.
pub type LlmProvidersRequest = EmptyRequest;
/// `llm.models` request.
pub type LlmModelsRequest = EmptyRequest;

/// `llm.providers` response value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmProvidersValue {
    /// Provider views in directory declaration order plus undeclared live routes.
    pub providers: Vec<ConfigurableProviderView>,
}

impl LlmProvidersValue {
    /// Parses an `llm.providers` response value.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing list or malformed provider row.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        Ok(Self {
            providers: parse_array(
                require_array(object, "providers", "$.providers")?,
                ConfigurableProviderView::parse,
                "$.providers",
            )?,
        })
    }
}

/// `llm.models` response value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmModelsValue {
    /// Successfully loaded provider groups.
    pub groups: Vec<ModelProviderGroup>,
    /// Provider-local failures.
    pub failures: Vec<ModelCatalogFailure>,
}

impl LlmModelsValue {
    /// Parses an `llm.models` response value.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed model groups or failure rows.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        Ok(Self {
            groups: parse_array(
                require_array(object, "groups", "$.groups")?,
                ModelProviderGroup::parse,
                "$.groups",
            )?,
            failures: parse_array(
                require_array(object, "failures", "$.failures")?,
                ModelCatalogFailure::parse,
                "$.failures",
            )?,
        })
    }
}

/// One model advertised by a draft endpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredModelView {
    /// Non-empty accepted model id.
    pub id: String,
    /// Optional non-empty display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Optional positive combined context size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// Optional positive output cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
}

impl DiscoveredModelView {
    fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_nonempty_string(object, "id", "$.id")?;
        optional_nonempty_string(object, "name", "$.name")?;
        super::sessions::optional_positive_integer(object, "contextWindow", "$.contextWindow")?;
        super::sessions::optional_positive_integer(object, "maxTokens", "$.maxTokens")?;
        serde_json::from_value(value.clone())
            .map_err(|error| ContractError::new("$", error.to_string()))
    }
}

/// Draft endpoint request for `llm.discoverModels`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmDiscoverModelsRequest {
    /// Non-empty adapter-family settings namespace.
    pub settings_ns: String,
    /// Optional non-empty route being edited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Optional non-empty draft endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "baseURL")]
    pub base_url: Option<String>,
    /// Optional non-empty draft API style.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
    /// Optional non-empty write-only interrogation key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

impl LlmDiscoverModelsRequest {
    /// Parses an `llm.discoverModels` request.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty settings namespace or any present empty/non-string draft field.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_nonempty_string(object, "settingsNs", "$.settingsNs")?;
        for (name, path) in [
            ("provider", "$.provider"),
            ("baseURL", "$.baseURL"),
            ("api", "$.api"),
            ("apiKey", "$.apiKey"),
        ] {
            optional_nonempty_string(object, name, path)?;
        }
        serde_json::from_value(value.clone())
            .map_err(|error| ContractError::new("$", error.to_string()))
    }
}

/// `llm.discoverModels` response value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmDiscoverModelsValue {
    /// Advertised candidate models.
    pub models: Vec<DiscoveredModelView>,
}

impl LlmDiscoverModelsValue {
    /// Parses an `llm.discoverModels` response value.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing model list or malformed candidate row.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        Ok(Self {
            models: parse_array(
                require_array(object, "models", "$.models")?,
                DiscoveredModelView::parse,
                "$.models",
            )?,
        })
    }
}
