//! Model directory and selection Rust/WASM UI semantics.

mod directory;
#[cfg(target_arch = "wasm32")]
mod wasm;

pub use directory::*;
#[cfg(target_arch = "wasm32")]
pub use wasm::*;

/// Compiled composer model selector stylesheet.
pub const MODEL_SELECT_STYLES: &str = include_str!("../data/model-select.css");

use seekdeep_client_ui_commands::SelectOption;
use serde::{Deserialize, Serialize};

/// Stable Host plugin identity.
pub const NAME: &str = "client-ui-model-selection";
/// Dictionary namespace.
pub const MODEL_NS: &str = "model";
/// Key, Simplified Chinese, and English values in source order.
pub const MODEL_LOCALES: [(&str, &str, &str); 17] = [
    (
        "command.description",
        "选择本会话使用的模型",
        "Select the model for this conversation",
    ),
    (
        "option.loadError",
        "目录加载失败：{message}",
        "Catalog failed to load: {message}",
    ),
    ("trigger.fallback", "选择模型", "Select model"),
    ("trigger.selectAria", "选择模型", "Select model"),
    (
        "trigger.aria",
        "选择模型，当前 {model}",
        "Select model, current {model}",
    ),
    (
        "trigger.ariaEffort",
        "选择模型，当前 {model}，推理等级 {effort}",
        "Select model, current {model}, reasoning effort {effort}",
    ),
    ("menu.aria", "模型与推理等级", "Model and reasoning effort"),
    ("menu.model", "模型", "Model"),
    ("menu.effort", "推理等级", "Effort"),
    ("effort.providerDefault", "Default", "Default"),
    (
        "status.loading",
        "正在刷新模型列表…",
        "Refreshing model list…",
    ),
    (
        "error.action",
        "模型操作失败：{message}",
        "Model operation failed: {message}",
    ),
    ("action.reload", "重新加载", "Reload"),
    (
        "warning.groupLoad",
        "{name} 加载失败：{message}",
        "{name} failed to load: {message}",
    ),
    ("empty.models", "没有可用的模型。", "No models available."),
    (
        "blocked.composer",
        "当前模型不可用，请先选择模型",
        "This model is unavailable — select one to continue",
    ),
    (
        "empty.efforts",
        "当前模型未提供推理等级。",
        "This model provides no reasoning effort levels.",
    ),
];

macro_rules! wire_id {
    ($name:ident) => {
        #[doc = concat!("Branded ", stringify!($name), " wire identity.")]
        #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Brands one exact wire string.
            #[must_use]
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// Exact wire string.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

wire_id!(ModelProviderId);
wire_id!(ModelId);
wire_id!(ReasoningEffortId);

/// Complete provider/model/effort selection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSelection {
    /// Provider route.
    pub provider: ModelProviderId,
    /// Provider-owned model.
    pub model: ModelId,
    /// Optional adapter-owned effort.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffortId>,
}

/// One effort choice.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningEffort {
    /// Stable effort id.
    pub id: ReasoningEffortId,
    /// Display name.
    pub name: String,
    /// Optional description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Model reasoning metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelReasoning {
    /// Supported efforts.
    pub efforts: Vec<ReasoningEffort>,
    /// Optional configured default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_effort: Option<ReasoningEffortId>,
}

/// One advertised model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelEntry {
    /// Provider-owned model id.
    pub id: ModelId,
    /// Display name.
    pub name: String,
    /// Optional description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional reasoning vocabulary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ModelReasoning>,
}

/// One provider group.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProviderGroup {
    /// Provider id.
    pub id: ModelProviderId,
    /// Display name.
    pub name: String,
    /// Advertised models.
    pub models: Vec<ModelEntry>,
}

/// Provider-local catalog failure.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCatalogFailure {
    /// Provider id.
    pub id: ModelProviderId,
    /// Provider display name.
    pub name: String,
    /// Failure text.
    pub message: String,
}

/// Successfully loaded Session model directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionModels {
    /// Current next-step selection.
    pub current: ModelSelection,
    /// Whether a route serves the current provider.
    pub routable: bool,
    /// Usable provider groups.
    pub groups: Vec<ModelProviderGroup>,
    /// Provider-local failures.
    pub failures: Vec<ModelCatalogFailure>,
}

/// Directory operation lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelDirectoryStatus {
    /// No operation active.
    Idle,
    /// Directory load active.
    Loading,
    /// Latest operation succeeded.
    Ready,
    /// Selection active.
    Selecting,
    /// Latest operation failed.
    Error,
}

/// Shared per-session directory snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelDirectoryState {
    /// Current selection, absent before first load/reset.
    pub current: Option<ModelSelection>,
    /// Host routability, absent before first load/reset.
    pub routable: Option<bool>,
    /// Last good groups.
    pub groups: Vec<ModelProviderGroup>,
    /// Latest provider-local failures.
    pub failures: Vec<ModelCatalogFailure>,
    /// Operation lifecycle.
    pub status: ModelDirectoryStatus,
    /// Whole-operation error.
    pub error: Option<String>,
}

impl Default for ModelDirectoryState {
    fn default() -> Self {
        Self {
            current: None,
            routable: None,
            groups: Vec::new(),
            failures: Vec::new(),
            status: ModelDirectoryStatus::Idle,
            error: None,
        }
    }
}

/// Opaque popup row id; consumers resolve by lookup and never parse it.
#[must_use]
pub fn row_id(provider_id: &ModelProviderId, model_id: &ModelId) -> String {
    format!("{}/{}", provider_id.as_str(), model_id.as_str())
}

/// Flattens usable groups followed by visible, unselectable provider failures.
#[must_use]
pub fn options_of(
    directory: &SessionModels,
    load_error: impl Fn(&str) -> String,
) -> Vec<SelectOption> {
    match try_options_of::<std::convert::Infallible>(directory, |message| Ok(load_error(message))) {
        Ok(options) => options,
        Err(error) => match error {},
    }
}

/// Flattens model rows while permitting fallible provider-failure localization.
///
/// # Errors
///
/// Returns the localization callback's error for the first provider failure.
pub fn try_options_of<E>(
    directory: &SessionModels,
    load_error: impl Fn(&str) -> Result<String, E>,
) -> Result<Vec<SelectOption>, E> {
    let mut rows = Vec::new();
    for group in &directory.groups {
        for model in &group.models {
            rows.push(SelectOption {
                id: row_id(&group.id, &model.id),
                label: model.name.clone(),
                detail: Some(model.description.as_ref().map_or_else(
                    || group.name.clone(),
                    |description| format!("{} · {description}", group.name),
                )),
                active: (directory.current.provider == group.id
                    && directory.current.model == model.id)
                    .then_some(true),
                confirmation: None,
            });
        }
    }
    for failure in &directory.failures {
        rows.push(SelectOption {
            id: format!("failure/{}", failure.id.as_str()),
            label: failure.name.clone(),
            detail: Some(load_error(&failure.message)?),
            active: None,
            confirmation: None,
        });
    }
    Ok(rows)
}

/// Resolves an opaque picked row back to its complete selection.
#[must_use]
pub fn selection_of(state: &ModelDirectoryState, id: &str) -> Option<ModelSelection> {
    for group in &state.groups {
        for model in &group.models {
            if row_id(&group.id, &model.id) != id {
                continue;
            }
            let same_route = state
                .current
                .as_ref()
                .is_some_and(|current| current.provider == group.id && current.model == model.id);
            let default_effort = || {
                model
                    .reasoning
                    .as_ref()
                    .and_then(|reasoning| reasoning.default_effort.clone())
            };
            let reasoning_effort = if same_route {
                state
                    .current
                    .as_ref()
                    .and_then(|current| current.reasoning_effort.clone())
                    .or_else(default_effort)
            } else {
                default_effort()
            };
            return Some(ModelSelection {
                provider: group.id.clone(),
                model: model.id.clone(),
                reasoning_effort,
            });
        }
    }
    None
}

/// Builds the no-op Host half of this pure Client plugin.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn host_plugin() -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new(NAME, std::iter::empty::<String>(), |_, _| {
        Box::pin(async { Ok(()) })
    })
}
