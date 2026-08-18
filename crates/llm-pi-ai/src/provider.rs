//! Provider construction, authentication inheritance, and protocol dispatch.

use seekdeep_llm::ProviderId;

use crate::{
    catalog::{CatalogIndex, CatalogOAuth, PiModel},
    replay::PiApi,
};

/// Hand-declared protocols with complete key/endpoint/header authentication.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PiProtocol {
    /// `OpenAI` Chat Completions.
    OpenAiCompletions,
    /// `OpenAI` Responses.
    OpenAiResponses,
    /// Anthropic Messages.
    AnthropicMessages,
}

impl PiProtocol {
    /// Canonical pi-ai API identity.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiCompletions => "openai-completions",
            Self::OpenAiResponses => "openai-responses",
            Self::AnthropicMessages => "anthropic-messages",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "openai-completions" => Some(Self::OpenAiCompletions),
            "openai-responses" => Some(Self::OpenAiResponses),
            "anthropic-messages" => Some(Self::AnthropicMessages),
            _ => None,
        }
    }
}

/// Stable configured-protocol offer order.
pub const SUPPORTED_PROTOCOLS: [PiProtocol; 3] = [
    PiProtocol::OpenAiCompletions,
    PiProtocol::OpenAiResponses,
    PiProtocol::AnthropicMessages,
];

/// Provider authentication methods retained without secrets.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PiProviderAuth {
    /// API-key resolution status name.
    pub api_key_name: Option<String>,
    /// Provider-native OAuth presentation metadata.
    pub oauth: Option<CatalogOAuth>,
}

/// Rust dispatch ownership for one built provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PiProviderDispatch {
    /// Reuse all implementations owned by this installed provider.
    Catalog {
        /// Installed provider identity.
        provider: ProviderId,
    },
    /// Use one configured-route protocol implementation.
    Protocol(PiProtocol),
}

/// Built provider value registered into the Rust adapter model collection.
#[derive(Clone, Debug, PartialEq)]
pub struct PiProvider {
    /// Route identity.
    pub id: ProviderId,
    /// Display name.
    pub name: String,
    /// Provider-level endpoint display metadata.
    pub base_url: Option<String>,
    /// Secret-free authentication methods.
    pub auth: PiProviderAuth,
    /// Materialized models.
    pub models: Vec<PiModel>,
    /// Dispatch implementation owner.
    pub dispatch: PiProviderDispatch,
}

/// Resolved route facts read by provider construction.
#[derive(Clone, Debug, PartialEq)]
pub struct ProviderSpec {
    /// Route identity.
    pub provider: ProviderId,
    /// Display name.
    pub display_name: String,
    /// Explicit protocol override.
    pub api: Option<PiApi>,
    /// Endpoint override.
    pub base_url: Option<String>,
    /// Materialized models.
    pub models: Vec<PiModel>,
    /// Whether configuration names a credential reference.
    pub names_credential: bool,
}

/// Returns configured protocols in stable source order.
#[must_use]
pub fn supported_protocols() -> Vec<&'static str> {
    SUPPORTED_PROTOCOLS
        .iter()
        .map(|protocol| protocol.as_str())
        .collect()
}

/// Builds one catalog-reusing or configured-protocol provider.
///
/// # Errors
///
/// Rejects an absent or unsupported protocol when catalog reuse does not own
/// the route.
pub fn build_provider(catalog: &CatalogIndex, spec: ProviderSpec) -> anyhow::Result<PiProvider> {
    let installed = catalog.provider(spec.provider.as_str());
    if let Some(installed) = installed
        && spec.api.is_none()
    {
        let auth = route_auth(&spec, Some(installed));
        return Ok(PiProvider {
            id: spec.provider,
            name: spec.display_name,
            base_url: spec.base_url.or_else(|| installed.base_url.clone()),
            auth,
            models: spec.models,
            dispatch: PiProviderDispatch::Catalog {
                provider: installed.id.clone(),
            },
        });
    }
    let protocol = spec
        .api
        .as_ref()
        .and_then(|api| PiProtocol::parse(api.as_str()))
        .ok_or_else(|| {
            let api = spec.api.as_ref().map_or("undefined", PiApi::as_str);
            anyhow::anyhow!(
                "llm-pi-ai: provider \"{}\" names api \"{api}\", which this build cannot serve; supported protocols are {}",
                spec.provider.as_str(),
                supported_protocols().join(", ")
            )
        })?;
    let auth = route_auth(&spec, installed);
    Ok(PiProvider {
        id: spec.provider,
        name: spec.display_name,
        base_url: spec.base_url,
        auth,
        models: spec.models,
        dispatch: PiProviderDispatch::Protocol(protocol),
    })
}

fn route_auth(
    spec: &ProviderSpec,
    installed: Option<&crate::catalog::CatalogProvider>,
) -> PiProviderAuth {
    let Some(installed) = installed else {
        return PiProviderAuth {
            api_key_name: Some(spec.display_name.clone()),
            oauth: None,
        };
    };
    if installed.api_key_name.is_some() || !spec.names_credential {
        return PiProviderAuth {
            api_key_name: installed.api_key_name.clone(),
            oauth: installed.oauth.clone(),
        };
    }
    PiProviderAuth {
        api_key_name: Some(spec.display_name.clone()),
        oauth: installed.oauth.clone(),
    }
}
