//! Service definition for the web access capability seam (ctx.web).

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use seekdeep_cordis::{Context, Plugin, ServiceKey, fiber::EffectHandle};
use seekdeep_llm::AbortSignal;
use seekdeep_schemastery::Schema;
use serde::{Deserialize, Serialize};

use crate::types::{
    WebFetchProvider, WebFetchRequest, WebFetchResult, WebSearchProvider, WebSearchRequest,
    WebSearchResult, web_error,
};

/// Typed Cordis slot corresponding to ctx.web.
pub const WEB: ServiceKey<WebRuntime> = ServiceKey::new("web");

/// Cordis plugin name.
pub const NAME: &str = "web";

/// Services required by the web seam.
pub const INJECT: &[&str] = &[];

/// Config for the web seam.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct WebRuntimeConfig {
    /// Explicit search provider id.
    pub search_provider: Option<String>,
    /// Explicit fetch provider id.
    pub fetch_provider: Option<String>,
}

/// The source-compatible admission schema for `WebRuntimeConfig`.
#[must_use]
pub fn config_schema() -> Schema {
    Schema::object([
        ("searchProvider", Schema::string()),
        ("fetchProvider", Schema::string()),
    ])
}

/// The web access service: registries and provider-selecting execution.
pub struct WebRuntime {
    search_providers: Arc<Mutex<HashMap<String, Arc<dyn WebSearchProvider>>>>,
    fetch_providers: Arc<Mutex<HashMap<String, Arc<dyn WebFetchProvider>>>>,
    search_provider_id: Option<String>,
    fetch_provider_id: Option<String>,
}

impl WebRuntime {
    /// Builds and publishes the web runtime service.
    ///
    /// # Errors
    ///
    /// Returns duplicate-service or inactive-owner failures.
    pub fn new(context: &Context, config: &WebRuntimeConfig) -> anyhow::Result<Arc<Self>> {
        let runtime = Arc::new(Self {
            search_providers: Arc::new(Mutex::new(HashMap::new())),
            fetch_providers: Arc::new(Mutex::new(HashMap::new())),
            search_provider_id: config.search_provider.clone().or_else(|| {
                std::env::var("DSH_WEB_SEARCH_PROVIDER")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
            }),
            fetch_provider_id: config.fetch_provider.clone().or_else(|| {
                std::env::var("DSH_WEB_FETCH_PROVIDER")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
            }),
        });
        context.provide(WEB, runtime.clone())?;
        Ok(runtime)
    }

    /// Registers a search provider.
    ///
    /// # Errors
    ///
    /// Returns a duplicate-provider or inactive-owner failure.
    pub fn register_search_provider(
        &self,
        caller: &Context,
        provider: Arc<dyn WebSearchProvider>,
    ) -> anyhow::Result<()> {
        let id = provider.id().to_owned();
        {
            let mut map = self.search_providers.lock();
            anyhow::ensure!(
                !map.contains_key(&id),
                web_error(
                    format!("a web provider with id \"{id}\" is already registered"),
                    "WEB_DUPLICATE_PROVIDER"
                )
            );
            map.insert(id.clone(), provider);
        }
        own_unregister(caller, self.search_providers.clone(), id);
        Ok(())
    }

    /// Registers a fetch provider.
    ///
    /// # Errors
    ///
    /// Returns a duplicate-provider or inactive-owner failure.
    pub fn register_fetch_provider(
        &self,
        caller: &Context,
        provider: Arc<dyn WebFetchProvider>,
    ) -> anyhow::Result<()> {
        let id = provider.id().to_owned();
        {
            let mut map = self.fetch_providers.lock();
            anyhow::ensure!(
                !map.contains_key(&id),
                web_error(
                    format!("a web provider with id \"{id}\" is already registered"),
                    "WEB_DUPLICATE_PROVIDER"
                )
            );
            map.insert(id.clone(), provider);
        }
        own_unregister(caller, self.fetch_providers.clone(), id);
        Ok(())
    }

    /// Runs one search through the selected provider.
    ///
    /// # Errors
    ///
    /// Returns provider-selection or provider-execution failures.
    pub async fn search(
        &self,
        request: &WebSearchRequest,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<WebSearchResult> {
        let provider =
            resolve_provider(self.search_provider_id.as_deref(), &self.search_providers)?;
        let result = provider.search(request, signal).await?;
        Ok(cap_sources(result, request.max_results))
    }

    /// Retrieves one URL through the selected provider.
    ///
    /// # Errors
    ///
    /// Returns provider-selection or provider-execution failures.
    pub async fn fetch(
        &self,
        request: &WebFetchRequest,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<WebFetchResult> {
        let provider = resolve_provider(self.fetch_provider_id.as_deref(), &self.fetch_providers)?;
        provider.fetch(request, signal).await
    }
}

/// Registers a reverse effect on the caller's context that removes a provider on teardown.
fn own_unregister<T: Send + Sync + ?Sized + 'static>(
    caller: &Context,
    store: Arc<Mutex<HashMap<String, Arc<T>>>>,
    id: String,
) {
    let _ = caller.own(EffectHandle::new("web.registerProvider()", move || {
        Box::pin(async move {
            store.lock().remove(&id);
            Ok(())
        })
    }));
}

trait Resolvable {
    fn id(&self) -> &str;
    fn available(&self) -> bool;
}

impl Resolvable for dyn WebSearchProvider {
    fn id(&self) -> &str {
        WebSearchProvider::id(self)
    }

    fn available(&self) -> bool {
        WebSearchProvider::available(self)
    }
}

impl Resolvable for dyn WebFetchProvider {
    fn id(&self) -> &str {
        WebFetchProvider::id(self)
    }

    fn available(&self) -> bool {
        WebFetchProvider::available(self)
    }
}

fn resolve_provider<P: Resolvable + ?Sized>(
    configured_id: Option<&str>,
    providers: &Mutex<HashMap<String, Arc<P>>>,
) -> anyhow::Result<Arc<P>> {
    let providers = providers.lock();
    if let Some(configured_id) = configured_id {
        let Some(provider) = providers.get(configured_id) else {
            anyhow::bail!(web_error(
                format!("configured web provider \"{configured_id}\" is not registered"),
                "WEB_PROVIDER_CONFIGURED_MISSING"
            ));
        };
        if !provider.available() {
            anyhow::bail!(web_error(
                format!(
                    "configured web provider \"{configured_id}\" is registered but unavailable"
                ),
                "WEB_PROVIDER_CONFIGURED_UNAVAILABLE"
            ));
        }
        return Ok(provider.clone());
    }
    let usable: Vec<Arc<P>> = providers
        .values()
        .filter(|provider| provider.available())
        .cloned()
        .collect();
    match usable.as_slice() {
        [] => anyhow::bail!(web_error(
            "no usable web provider is registered",
            "WEB_PROVIDER_UNAVAILABLE"
        )),
        [single] => Ok(single.clone()),
        _ => {
            let ids = usable
                .iter()
                .map(|provider| provider.id().to_owned())
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::bail!(web_error(
                format!(
                    "multiple usable web providers are registered ({ids}); configure one explicitly"
                ),
                "WEB_PROVIDER_AMBIGUOUS"
            ));
        }
    }
}

#[allow(clippy::cast_possible_truncation)]
fn cap_sources(mut result: WebSearchResult, max_results: Option<u64>) -> WebSearchResult {
    let Some(max_results) = max_results else {
        return result;
    };
    if result.sources.len() <= max_results as usize {
        return result;
    }
    result.sources.truncate(max_results as usize);
    result.truncated = true;
    result
}

/// Builds the loader-compatible web seam plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, config| {
        Box::pin(async move {
            let config: WebRuntimeConfig = serde_json::from_value(config)?;
            WebRuntime::new(&context, &config)?;
            Ok(())
        })
    })
    .with_config_validator(|value: &serde_json::Value| {
        config_schema()
            .resolve(value)
            .map_err(|error| anyhow::anyhow!("{error}"))
    })
}
