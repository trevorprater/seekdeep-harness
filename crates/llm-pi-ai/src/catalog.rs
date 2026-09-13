//! Lossless materialization of configured routes over an installed pi-ai catalog.

use std::{
    collections::{HashMap, HashSet},
    io::Read,
    sync::LazyLock,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use flate2::read::GzDecoder;
use seekdeep_llm::{ModelId, ProviderId};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};

use crate::replay::PiApi;

mod generated {
    include!("catalog_snapshot.rs");
}

/// Context capacity assumed when neither profile nor catalog supplies one.
pub const DEFAULT_CONTEXT_WINDOW: u64 = 262_144;
/// Output capability assumed when neither profile nor catalog supplies one.
pub const DEFAULT_MAX_TOKENS: u64 = 32_768;

/// One request modality understood by pi-ai.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PiModality {
    /// Text input.
    #[serde(rename = "text")]
    Text,
    /// Raster image input.
    #[serde(rename = "image")]
    Image,
}

/// Every request modality, in source catalog order.
pub const MODALITIES: [PiModality; 2] = [PiModality::Text, PiModality::Image];

/// One selectable pi-ai thinking level.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PiThinkingLevel {
    /// Disable reasoning.
    #[serde(rename = "off")]
    Off,
    /// Minimal reasoning.
    #[serde(rename = "minimal")]
    Minimal,
    /// Low reasoning.
    #[serde(rename = "low")]
    Low,
    /// Medium reasoning.
    #[serde(rename = "medium")]
    Medium,
    /// High reasoning.
    #[serde(rename = "high")]
    High,
    /// Extra-high reasoning.
    #[serde(rename = "xhigh")]
    XHigh,
    /// Maximum reasoning.
    #[serde(rename = "max")]
    Max,
}

impl PiThinkingLevel {
    /// Canonical settings and wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }
}

/// Thinking levels in pi-ai escalation order.
pub const THINKING_LEVELS: [PiThinkingLevel; 7] = [
    PiThinkingLevel::Off,
    PiThinkingLevel::Minimal,
    PiThinkingLevel::Low,
    PiThinkingLevel::Medium,
    PiThinkingLevel::High,
    PiThinkingLevel::XHigh,
    PiThinkingLevel::Max,
];

/// One configurable OpenAI-completions reasoning dispatch format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PiThinkingFormat {
    /// `OpenAI` native format.
    #[serde(rename = "openai")]
    OpenAi,
    /// `DeepSeek` format.
    #[serde(rename = "deepseek")]
    DeepSeek,
    /// `OpenRouter` format.
    #[serde(rename = "openrouter")]
    OpenRouter,
    /// Together format.
    #[serde(rename = "together")]
    Together,
    /// Z.ai format.
    #[serde(rename = "zai")]
    Zai,
    /// Qwen format.
    #[serde(rename = "qwen")]
    Qwen,
    /// String-wrapped thinking format.
    #[serde(rename = "string-thinking")]
    StringThinking,
    /// Ant Ling format.
    #[serde(rename = "ant-ling")]
    AntLing,
}

impl PiThinkingFormat {
    /// Canonical settings spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::DeepSeek => "deepseek",
            Self::OpenRouter => "openrouter",
            Self::Together => "together",
            Self::Zai => "zai",
            Self::Qwen => "qwen",
            Self::StringThinking => "string-thinking",
            Self::AntLing => "ant-ling",
        }
    }
}

/// Configurable reasoning compatibility switches.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiCompatProfile {
    /// Endpoint reasoning format.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_format: Option<PiThinkingFormat>,
    /// Whether the endpoint accepts `reasoning_effort`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_reasoning_effort: Option<bool>,
}

impl PiCompatProfile {
    fn is_empty(&self) -> bool {
        self.thinking_format.is_none() && self.supports_reasoning_effort.is_none()
    }
}

/// One configured model's reasoning declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PiReasoningEfforts {
    /// Explicitly non-reasoning.
    Disabled,
    /// A valueless or empty declaration, rejected during resolution.
    Empty,
    /// Offered levels and their optional wire spellings.
    Declared(HashMap<PiThinkingLevel, Option<String>>),
}

/// Lossless native pi-ai model value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiModel {
    /// Provider model identity.
    pub id: ModelId,
    /// Display name.
    pub name: String,
    /// Wire protocol.
    pub api: PiApi,
    /// Per-model endpoint.
    pub base_url: String,
    /// Owning provider route.
    pub provider: ProviderId,
    /// Whether the model reasons.
    pub reasoning: bool,
    /// Accepted modalities.
    pub input: Vec<PiModality>,
    /// Catalog pricing retained for pi-ai even though Harness does not use it.
    pub cost: PiModelCost,
    /// Combined input/output capacity.
    pub context_window: u64,
    /// Output capacity.
    pub max_tokens: u64,
    /// Provider-specific thinking spellings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_level_map: Option<Map<String, Value>>,
    /// Protocol-specific compatibility fields, retained losslessly.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compat: Option<Map<String, Value>>,
    /// Future or provider-specific pi-ai fields retained by base spread.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Installed model pricing retained for native pi-ai behavior.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiModelCost {
    /// Input price.
    pub input: f64,
    /// Output price.
    pub output: f64,
    /// Cache-read price.
    pub cache_read: f64,
    /// Cache-write price.
    pub cache_write: f64,
}

/// Installed OAuth presentation metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogOAuth {
    /// Provider-native authentication name.
    pub name: String,
    /// Optional login action label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub login_label: Option<String>,
}

/// Installed provider facts used during route materialization.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogProvider {
    /// Provider identity.
    pub id: ProviderId,
    /// Provider display name.
    pub name: String,
    /// Provider-level endpoint display metadata.
    pub base_url: Option<String>,
    /// Whether this provider appears in the configurable-provider directory.
    pub listed: bool,
    /// Provider-native API-key presentation name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key_name: Option<String>,
    /// Provider-native OAuth presentation metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth: Option<CatalogOAuth>,
    /// Installed models in catalog order.
    pub models: Vec<PiModel>,
}

/// Immutable installed catalog index.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CatalogIndex {
    providers: HashMap<String, CatalogProvider>,
    order: Vec<String>,
}

impl CatalogIndex {
    /// Builds an ordered provider index.
    ///
    /// # Errors
    ///
    /// Rejects duplicate provider identities.
    pub fn new(providers: Vec<CatalogProvider>) -> anyhow::Result<Self> {
        let mut index = HashMap::with_capacity(providers.len());
        let mut order = Vec::with_capacity(providers.len());
        for provider in providers {
            let id = provider.id.as_str().to_owned();
            let listed = provider.listed;
            anyhow::ensure!(
                index.insert(id.clone(), provider).is_none(),
                "duplicate pi-ai catalog provider \"{id}\""
            );
            if listed {
                order.push(id);
            }
        }
        Ok(Self {
            providers: index,
            order,
        })
    }

    /// Looks up one installed provider.
    #[must_use]
    pub fn provider(&self, id: &str) -> Option<&CatalogProvider> {
        self.providers.get(id)
    }

    /// Installed provider identities in catalog order.
    #[must_use]
    pub fn provider_ids(&self) -> &[String] {
        &self.order
    }
}

/// Installed pi-ai catalog version captured by the generated snapshot.
pub const PI_AI_CATALOG_VERSION: &str = "0.82.1";
/// Pinned `DeepSeek Harness` source commit that selected the catalog.
pub const CATALOG_SOURCE_COMMIT: &str = "37200a934324dd7167ec8a8d3ac1fd01e2239909";

static BUILTIN_CATALOG: LazyLock<CatalogIndex> = LazyLock::new(|| {
    let compressed = STANDARD
        .decode(generated::CATALOG_GZIP_BASE64)
        .expect("generated pi-ai catalog is valid base64");
    let mut json = Vec::new();
    GzDecoder::new(compressed.as_slice())
        .read_to_end(&mut json)
        .expect("generated pi-ai catalog is valid gzip");
    let digest = format!("{:x}", Sha256::digest(&json));
    assert_eq!(
        digest,
        generated::CATALOG_JSON_SHA256,
        "generated pi-ai catalog checksum drifted"
    );
    let providers = serde_json::from_slice(&json).expect("generated pi-ai catalog schema is valid");
    CatalogIndex::new(providers).expect("generated pi-ai provider ids are unique")
});

/// Returns the immutable installed pi-ai provider/model snapshot.
///
/// # Panics
///
/// Panics if the committed generated asset is corrupt, has a mismatched
/// checksum, or violates the Rust catalog schema.
#[must_use]
pub fn builtin_catalog() -> &'static CatalogIndex {
    &BUILTIN_CATALOG
}

/// Fields shared by configured model entries and catalog overrides.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PiModelFields {
    /// Display name override.
    pub name: Option<String>,
    /// Context capacity override.
    pub context_window: Option<u64>,
    /// Output capacity and per-request default override.
    pub max_tokens: Option<u64>,
    /// Modality declaration; empty means inherit.
    pub input: Vec<PiModality>,
    /// Reasoning declaration; absent means inherit.
    pub reasoning_efforts: Option<PiReasoningEfforts>,
    /// Model-level compatibility switches.
    pub compat: Option<PiCompatProfile>,
    /// Unknown schema fields retained so forbidden override `id` can be rejected.
    pub extra: Map<String, Value>,
}

/// One configured model entry.
#[derive(Clone, Debug, PartialEq)]
pub struct PiModelProfile {
    /// Model identity.
    pub id: ModelId,
    /// Configured fields.
    pub fields: PiModelFields,
}

/// Route-level catalog facts after adapter-owned defaults resolve.
#[derive(Clone, Debug, PartialEq)]
pub struct RouteCatalogRequest {
    /// Route identity.
    pub provider: ProviderId,
    /// Protocol override.
    pub api: Option<PiApi>,
    /// Endpoint override.
    pub base_url: Option<String>,
    /// Replacement model list; empty serves the installed catalog.
    pub models: Vec<PiModelProfile>,
    /// Installed-catalog overrides in settings order.
    pub model_overrides: Vec<(String, PiModelFields)>,
    /// Route-level compatibility switches.
    pub compat: Option<PiCompatProfile>,
    /// Capacity fallback.
    pub default_context_window: u64,
    /// Output fallback.
    pub default_max_tokens: u64,
    /// Modality fallback.
    pub default_input: Vec<PiModality>,
}

impl RouteCatalogRequest {
    /// Creates a route request with source defaults.
    #[must_use]
    pub fn new(provider: ProviderId) -> Self {
        Self {
            provider,
            api: None,
            base_url: None,
            models: Vec::new(),
            model_overrides: Vec::new(),
            compat: None,
            default_context_window: DEFAULT_CONTEXT_WINDOW,
            default_max_tokens: DEFAULT_MAX_TOKENS,
            default_input: vec![PiModality::Text],
        }
    }
}

/// Materialized route plus explicit per-request output defaults.
#[derive(Clone, Debug, PartialEq)]
pub struct RouteCatalog {
    /// Models in configuration or installed-catalog order.
    pub models: Vec<PiModel>,
    /// Only caps explicitly named by configuration.
    pub configured_max_tokens: HashMap<ModelId, u64>,
}

/// Materializes one route by spreading configured fields over installed models.
///
/// # Errors
///
/// Rejects every underspecified, duplicate, incompatible, or misplaced route
/// and model declaration at this configuration-resolution boundary.
#[allow(clippy::too_many_lines)] // Mirrors one atomic source resolution transaction.
pub fn resolve_route_models(
    catalog: &CatalogIndex,
    request: &RouteCatalogRequest,
) -> anyhow::Result<RouteCatalog> {
    let provider = request.provider.as_str();
    let installed = catalog.provider(provider);
    let defaults = installed
        .map(|entry| {
            entry
                .models
                .iter()
                .map(|model| (model.id.as_str().to_owned(), model))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    if request.default_input.is_empty() {
        return Err(invalid(
            provider,
            "defaultInput must name at least one modality",
        ));
    }

    for (id, fields) in &request.model_overrides {
        if id.is_empty() {
            return Err(invalid(
                provider,
                "has a modelOverrides entry with an empty model id",
            ));
        }
        if defaults.is_empty() {
            return Err(invalid(
                provider,
                format!(
                    "sets modelOverrides for \"{id}\", but the installed catalog does not describe this route; a declared route spells every model out in its models list"
                ),
            ));
        }
        if !request.models.is_empty() {
            return Err(invalid(
                provider,
                format!(
                    "sets modelOverrides for \"{id}\" beside a models list; models already replaces the served catalog, so declare the fields on its entries"
                ),
            ));
        }
        if !defaults.contains_key(id) {
            return Err(invalid(
                provider,
                format!(
                    "modelOverrides names \"{id}\", which the installed catalog does not describe"
                ),
            ));
        }
        if fields.extra.contains_key("id") {
            return Err(invalid(
                provider,
                format!("modelOverrides entry \"{id}\" sets \"id\", which is the dict key"),
            ));
        }
    }

    let entries = if request.models.is_empty() {
        installed
            .map(|provider| {
                provider
                    .models
                    .iter()
                    .map(|model| PiModelProfile {
                        id: model.id.clone(),
                        fields: request
                            .model_overrides
                            .iter()
                            .find(|(id, _)| id == model.id.as_str())
                            .map_or_else(PiModelFields::default, |(_, fields)| fields.clone()),
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    } else {
        request.models.clone()
    };
    if entries.is_empty() {
        return Err(invalid(
            provider,
            "resolves no models; the installed catalog does not describe this route, so its models must be listed in configuration",
        ));
    }

    let shared_api = shared_catalog_api(defaults.values().copied());
    let mut seen = HashSet::new();
    let mut configured_max_tokens = HashMap::new();
    let mut models = Vec::with_capacity(entries.len());
    for entry in entries {
        let id = entry.id.as_str();
        if id.is_empty() {
            return Err(invalid(provider, "has a model with an empty id"));
        }
        if !seen.insert(id.to_owned()) {
            return Err(invalid(
                provider,
                format!("lists model \"{id}\" more than once"),
            ));
        }
        let base = defaults.get(id).copied();
        let api = request
            .api
            .clone()
            .or_else(|| base.map(|model| model.api.clone()))
            .or_else(|| shared_api.clone())
            .ok_or_else(|| {
                invalid(
                    provider,
                    format!(
                        "model \"{id}\" needs an api; the installed catalog does not describe it, so set the route's api to the wire protocol its endpoint speaks"
                    ),
                )
            })?;
        let base_url = request
            .base_url
            .clone()
            .or_else(|| base.map(|model| model.base_url.clone()))
            .or_else(|| installed.and_then(|provider| provider.base_url.clone()))
            .ok_or_else(|| {
                invalid(
                    provider,
                    format!(
                        "model \"{id}\" needs a baseURL; the installed catalog does not describe this route"
                    ),
                )
            })?;
        let context_window = entry
            .fields
            .context_window
            .or_else(|| base.map(|model| model.context_window))
            .unwrap_or(request.default_context_window);
        if context_window == 0 {
            return Err(invalid(
                provider,
                format!("model \"{id}\" contextWindow must be a positive integer"),
            ));
        }
        let max_tokens = entry
            .fields
            .max_tokens
            .or_else(|| base.map(|model| model.max_tokens))
            .unwrap_or(request.default_max_tokens);
        if max_tokens == 0 {
            return Err(invalid(
                provider,
                format!("model \"{id}\" maxTokens must be a positive integer"),
            ));
        }
        if let Some(configured) = entry.fields.max_tokens {
            configured_max_tokens.insert(entry.id.clone(), configured);
        }

        let mut model = base.cloned().unwrap_or_else(|| PiModel {
            id: entry.id.clone(),
            name: id.to_owned(),
            api: api.clone(),
            base_url: base_url.clone(),
            provider: request.provider.clone(),
            reasoning: false,
            input: request.default_input.clone(),
            cost: PiModelCost::default(),
            context_window,
            max_tokens,
            thinking_level_map: None,
            compat: None,
            extra: Map::new(),
        });
        model.id = entry.id.clone();
        model.name = entry
            .fields
            .name
            .clone()
            .unwrap_or_else(|| base.map_or_else(|| id.to_owned(), |model| model.name.clone()));
        model.api = api.clone();
        model.provider = request.provider.clone();
        model.base_url = base_url;
        model.input = if entry.fields.input.is_empty() {
            base.map_or_else(
                || request.default_input.clone(),
                |model| model.input.clone(),
            )
        } else {
            entry.fields.input.clone()
        };
        model.context_window = context_window;
        model.max_tokens = max_tokens;
        resolve_reasoning(provider, &entry, base, &mut model)?;
        resolve_compat(provider, &entry, request.compat.as_ref(), base, &mut model)?;
        models.push(model);
    }

    if request
        .compat
        .as_ref()
        .is_some_and(|compat| !compat.is_empty())
        && !models
            .iter()
            .any(|model| model.api.as_str() == "openai-completions")
    {
        return Err(invalid(
            provider,
            "sets compat reasoning switches, but no model on the route speaks openai-completions; thinkingFormat and supportsReasoningEffort exist only on that protocol",
        ));
    }
    Ok(RouteCatalog {
        models,
        configured_max_tokens,
    })
}

fn shared_catalog_api<'a>(models: impl Iterator<Item = &'a PiModel>) -> Option<PiApi> {
    let mut apis = models.map(|model| model.api.clone()).collect::<Vec<_>>();
    apis.dedup_by(|left, right| left == right);
    (apis.len() == 1).then(|| apis.remove(0))
}

fn resolve_reasoning(
    provider: &str,
    entry: &PiModelProfile,
    base: Option<&PiModel>,
    model: &mut PiModel,
) -> anyhow::Result<()> {
    let Some(efforts) = &entry.fields.reasoning_efforts else {
        model.reasoning = base.is_some_and(|base| base.reasoning);
        return Ok(());
    };
    match efforts {
        PiReasoningEfforts::Disabled => {
            model.reasoning = false;
            Ok(())
        }
        PiReasoningEfforts::Empty => Err(invalid(
            provider,
            format!(
                "model \"{}\" has an empty reasoningEfforts; declare the offered levels, set false for a non-reasoning model, or omit the field to keep the installed catalog's capability",
                entry.id.as_str()
            ),
        )),
        PiReasoningEfforts::Declared(declared) if declared.is_empty() => Err(invalid(
            provider,
            format!(
                "model \"{}\" has an empty reasoningEfforts; declare the offered levels, set false for a non-reasoning model, or omit the field to keep the installed catalog's capability",
                entry.id.as_str()
            ),
        )),
        PiReasoningEfforts::Declared(declared) => {
            for (level, wire) in declared {
                match wire {
                    None if *level != PiThinkingLevel::Off => {
                        return Err(invalid(
                            provider,
                            format!(
                                "model \"{}\" reasoningEfforts.{} needs the wire value dispatch should send; only \"off\" may leave it empty",
                                entry.id.as_str(),
                                level.as_str()
                            ),
                        ));
                    }
                    Some(wire) if wire.is_empty() => {
                        return Err(invalid(
                            provider,
                            format!(
                                "model \"{}\" reasoningEfforts.{} must not be an empty string",
                                entry.id.as_str(),
                                level.as_str()
                            ),
                        ));
                    }
                    None | Some(_) => {}
                }
            }
            if !declared.keys().any(|level| *level != PiThinkingLevel::Off) {
                return Err(invalid(
                    provider,
                    format!(
                        "model \"{}\" reasoningEfforts offers no level beyond \"off\"; declare a thinking level, or set reasoningEfforts to false for a non-reasoning model",
                        entry.id.as_str()
                    ),
                ));
            }
            let mut map = Map::new();
            for level in THINKING_LEVELS {
                match declared.get(&level) {
                    None => {
                        map.insert(level.as_str().to_owned(), Value::Null);
                    }
                    Some(Some(wire)) => {
                        map.insert(level.as_str().to_owned(), Value::String(wire.clone()));
                    }
                    Some(None) => {}
                }
            }
            model.reasoning = true;
            model.thinking_level_map = Some(map);
            Ok(())
        }
    }
}

fn resolve_compat(
    provider: &str,
    entry: &PiModelProfile,
    route: Option<&PiCompatProfile>,
    base: Option<&PiModel>,
    model: &mut PiModel,
) -> anyhow::Result<()> {
    let thinking_format = entry
        .fields
        .compat
        .as_ref()
        .and_then(|compat| compat.thinking_format)
        .or_else(|| route.and_then(|compat| compat.thinking_format));
    let supports_effort = entry
        .fields
        .compat
        .as_ref()
        .and_then(|compat| compat.supports_reasoning_effort)
        .or_else(|| route.and_then(|compat| compat.supports_reasoning_effort));
    if thinking_format.is_none() && supports_effort.is_none() {
        return Ok(());
    }
    if model.api.as_str() != "openai-completions" {
        if entry
            .fields
            .compat
            .as_ref()
            .is_some_and(|compat| !compat.is_empty())
        {
            return Err(invalid(
                provider,
                format!(
                    "model \"{}\" sets compat reasoning switches, but its api is \"{}\"; thinkingFormat and supportsReasoningEffort exist only on openai-completions",
                    entry.id.as_str(),
                    model.api.as_str()
                ),
            ));
        }
        return Ok(());
    }
    let mut compat = if base.is_some_and(|base| base.api == model.api) {
        base.and_then(|base| base.compat.clone())
            .unwrap_or_default()
    } else {
        Map::new()
    };
    if let Some(format) = thinking_format {
        compat.insert(
            "thinkingFormat".to_owned(),
            Value::String(format.as_str().to_owned()),
        );
    }
    if let Some(supported) = supports_effort {
        compat.insert("supportsReasoningEffort".to_owned(), Value::Bool(supported));
    }
    model.compat = Some(compat);
    Ok(())
}

fn invalid(provider: &str, detail: impl AsRef<str>) -> anyhow::Error {
    anyhow::anyhow!("llm-pi-ai: provider \"{provider}\" {}", detail.as_ref())
}
