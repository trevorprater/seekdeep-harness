//! Language Server Protocol provider registry and semantic query seam.

mod types;

pub use types::{
    LspHover, LspLocation, LspOperation, LspPosition, LspProvider, LspProviderQuery,
    LspQueryRequest, LspQueryResult, LspRange,
};

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use indexmap::IndexMap;
use parking_lot::Mutex;
use seekdeep_cordis::{Context, Plugin, PluginFiber, ServiceKey, fiber::EffectHandle};
use seekdeep_llm::AbortSignal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Typed Cordis slot corresponding to `ctx.lsp`.
pub const LSP: ServiceKey<Lsp> = ServiceKey::new("lsp");
/// Loader-facing service plugin name.
pub const NAME: &str = "lsp";
/// The registry has no required services.
pub const INJECT: &[&str] = &[];

/// Invalid provider registration.
pub const LSP_INVALID_PROVIDER: &str = "LSP_INVALID_PROVIDER";
/// Provider id or extension reservation conflict.
pub const LSP_CONFLICT: &str = "LSP_CONFLICT";
/// No provider handles the requested extension.
pub const LSP_UNAVAILABLE: &str = "LSP_UNAVAILABLE";
/// A server returned a structurally invalid semantic result.
pub const LSP_MALFORMED_RESPONSE: &str = "LSP_MALFORMED_RESPONSE";

/// Opaque provider identity reserved atomically with extension mappings.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LspProviderId(String);

impl LspProviderId {
    /// Brands a provider spelling without validation.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the exact wire spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for LspProviderId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Structured LSP failure carrying a stable machine-routable code.
#[derive(Debug, Error)]
#[error("{message}")]
pub struct LspError {
    message: String,
    code: &'static str,
}

impl LspError {
    /// Creates one structured LSP failure.
    #[must_use]
    pub fn new(message: impl Into<String>, code: &'static str) -> Self {
        Self {
            message: message.into(),
            code,
        }
    }

    /// Stable routing code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }

    /// Human-readable message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Clone)]
struct Route {
    provider_id: LspProviderId,
    provider: Arc<dyn LspProvider>,
    language_id: String,
}

#[derive(Default)]
struct Registry {
    provider_ids: HashSet<LspProviderId>,
    routes: HashMap<String, Route>,
}

/// Provider registry and normalized semantic-query router.
#[derive(Default)]
pub struct Lsp {
    registry: Arc<Mutex<Registry>>,
}

impl std::fmt::Debug for Lsp {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let registry = self.registry.lock();
        formatter
            .debug_struct("Lsp")
            .field("providers", &registry.provider_ids.len())
            .field("routes", &registry.routes.len())
            .finish()
    }
}

impl Lsp {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers one provider atomically under the caller lifecycle.
    ///
    /// # Errors
    ///
    /// Returns structured invalid-provider or conflict failures, or inactive
    /// caller ownership after rolling every provisional reservation back.
    pub fn register_provider(
        &self,
        caller: &Context,
        provider: Arc<dyn LspProvider>,
    ) -> anyhow::Result<EffectHandle> {
        let id = provider.id().clone();
        if id.as_str().trim().is_empty() {
            return Err(LspError::new(
                "an LSP provider id must be a non-empty string",
                LSP_INVALID_PROVIDER,
            )
            .into());
        }
        let mappings = provider.extension_to_language();
        if mappings.is_empty() {
            return Err(LspError::new(
                format!("LSP provider \"{id}\" registers no file extensions"),
                LSP_INVALID_PROVIDER,
            )
            .into());
        }
        let mut pending = IndexMap::new();
        for (raw_extension, language_id) in mappings {
            let extension = normalize_extension(raw_extension);
            if !valid_extension(&extension) {
                return Err(LspError::new(
                    format!("LSP provider \"{id}\" maps an invalid extension \"{raw_extension}\""),
                    LSP_INVALID_PROVIDER,
                )
                .into());
            }
            if language_id.trim().is_empty() {
                return Err(LspError::new(
                    format!(
                        "LSP provider \"{id}\" maps extension \"{extension}\" to an empty language id"
                    ),
                    LSP_INVALID_PROVIDER,
                )
                .into());
            }
            if pending
                .insert(extension.clone(), language_id.clone())
                .is_some()
            {
                return Err(LspError::new(
                    format!("LSP provider \"{id}\" maps extension \"{extension}\" more than once"),
                    LSP_INVALID_PROVIDER,
                )
                .into());
            }
        }

        let extensions = {
            let mut registry = self.registry.lock();
            if registry.provider_ids.contains(&id) {
                return Err(LspError::new(
                    format!("an LSP provider with id \"{id}\" is already registered"),
                    LSP_CONFLICT,
                )
                .into());
            }
            if let Some(extension) = pending
                .keys()
                .find(|extension| registry.routes.contains_key(*extension))
            {
                return Err(LspError::new(
                    format!("extension \"{extension}\" is already handled by another LSP provider"),
                    LSP_CONFLICT,
                )
                .into());
            }
            registry.provider_ids.insert(id.clone());
            for (extension, language_id) in &pending {
                registry.routes.insert(
                    extension.clone(),
                    Route {
                        provider_id: id.clone(),
                        provider: provider.clone(),
                        language_id: language_id.clone(),
                    },
                );
            }
            pending.into_keys().collect::<Vec<_>>()
        };
        drop(provider);
        let registry = self.registry.clone();
        let cleanup_id = id.clone();
        let cleanup_extensions = extensions.clone();
        let effect = EffectHandle::synchronous("lsp.registerProvider()", move || {
            unregister(&registry, &cleanup_id, &cleanup_extensions);
            Ok(())
        });
        if let Err(error) = caller.own(effect.clone()) {
            unregister(&self.registry, &id, &extensions);
            return Err(error.into());
        }
        Ok(effect)
    }

    /// Selects by final extension and forwards one semantic query.
    ///
    /// # Errors
    ///
    /// Returns `LSP_UNAVAILABLE` for an unowned extension, or the provider's
    /// query failure.
    pub async fn query(
        &self,
        request: LspQueryRequest,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<LspQueryResult> {
        let route = self
            .registry
            .lock()
            .routes
            .get(&final_extension(&request.file_path))
            .cloned()
            .ok_or_else(|| {
                LspError::new(
                    format!("no LSP provider handles \"{}\"", request.file_path),
                    LSP_UNAVAILABLE,
                )
            })?;
        route
            .provider
            .query(LspProviderQuery::new(request, route.language_id), signal)
            .await
    }

    /// Publishes this registry on the exact context.
    ///
    /// # Errors
    ///
    /// Returns duplicate-service or inactive-owner failures.
    pub fn provide(self: &Arc<Self>, context: &Context) -> anyhow::Result<EffectHandle> {
        Ok(context.provide(LSP, self.clone())?)
    }
}

fn unregister(registry: &Mutex<Registry>, id: &LspProviderId, extensions: &[String]) {
    let mut registry = registry.lock();
    registry.provider_ids.remove(id);
    for extension in extensions {
        if registry
            .routes
            .get(extension)
            .is_some_and(|route| &route.provider_id == id)
        {
            registry.routes.remove(extension);
        }
    }
}

/// Extracts a normalized final extension from POSIX or Windows paths.
#[must_use]
pub fn final_extension(file_path: &str) -> String {
    let base = file_path.rsplit(['/', '\\']).next().unwrap_or(file_path);
    let Some(dot) = base.rfind('.') else {
        return String::new();
    };
    if dot == 0 {
        return String::new();
    }
    base[dot..].to_lowercase()
}

fn normalize_extension(extension: &str) -> String {
    let extension = extension.to_lowercase();
    if extension.starts_with('.') {
        extension
    } else {
        format!(".{extension}")
    }
}

fn valid_extension(extension: &str) -> bool {
    extension
        .strip_prefix('.')
        .is_some_and(|rest| !rest.is_empty() && !rest.contains(['.', '/', '\\']))
}

/// Builds the loader-compatible LSP service plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, _| {
        Box::pin(async move {
            let service = Arc::new(Lsp::new());
            service.provide(&context)?;
            Ok(())
        })
    })
}

/// Mounts the LSP service as a lifecycle-owned plugin fiber.
///
/// # Errors
///
/// Returns inactive-context failures.
pub fn install(context: &Context) -> anyhow::Result<Arc<PluginFiber>> {
    Ok(context.plugin(plugin(), serde_json::Value::Null)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_id_is_an_unvalidated_transparent_brand() {
        for value in ["provider", "", " with spaces ", "中文"] {
            let id = LspProviderId::new(value);
            assert_eq!(id.as_str(), value);
            assert_eq!(serde_json::to_value(&id).unwrap(), value);
            assert_eq!(
                serde_json::from_value::<LspProviderId>(serde_json::json!(value)).unwrap(),
                id
            );
        }
    }
}
