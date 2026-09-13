//! Behavioral mirror of `packages/web/web/tests/web.spec.ts`.

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use seekdeep_cordis::{Context, Fiber};
use seekdeep_llm::{AbortSignal, HarnessError};
use seekdeep_web::{
    WEB, WEB_ERROR_NAME, WebFetchBody, WebFetchProvider, WebFetchRequest, WebFetchResult,
    WebRuntime, WebRuntimeConfig, WebSearchProvider, WebSearchRequest, WebSearchResult,
    WebSearchSource, web_error,
};

#[derive(Debug)]
struct SearchProvider {
    id: String,
    available: bool,
    result: WebSearchResult,
    signals: Arc<Mutex<Vec<Option<AbortSignal>>>>,
}

#[async_trait]
impl WebSearchProvider for SearchProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn available(&self) -> bool {
        self.available
    }

    async fn search(
        &self,
        _request: &WebSearchRequest,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<WebSearchResult> {
        self.signals.lock().push(signal);
        Ok(self.result.clone())
    }
}

#[derive(Debug)]
struct FetchProvider {
    id: String,
    available: bool,
    result: WebFetchResult,
}

#[async_trait]
impl WebFetchProvider for FetchProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn available(&self) -> bool {
        self.available
    }

    async fn fetch(
        &self,
        _request: &WebFetchRequest,
        _signal: Option<AbortSignal>,
    ) -> anyhow::Result<WebFetchResult> {
        Ok(self.result.clone())
    }
}

fn search_provider(id: &str, available: bool, marker: &str) -> Arc<SearchProvider> {
    Arc::new(SearchProvider {
        id: id.to_owned(),
        available,
        result: WebSearchResult {
            content: Some(marker.to_owned()),
            sources: Vec::new(),
            truncated: false,
        },
        signals: Arc::new(Mutex::new(Vec::new())),
    })
}

fn fetch_provider(id: &str, available: bool, marker: &str) -> Arc<FetchProvider> {
    Arc::new(FetchProvider {
        id: id.to_owned(),
        available,
        result: WebFetchResult {
            url: "https://example.com".to_owned(),
            status_code: 200,
            body: WebFetchBody::Text {
                content: marker.to_owned(),
            },
            truncated: false,
        },
    })
}

fn mount_runtime(config: &WebRuntimeConfig) -> (Context, Arc<WebRuntime>) {
    let context = Context::new();
    let runtime = WebRuntime::new(&context, config).expect("web runtime");
    assert!(Arc::ptr_eq(&context.get(WEB).expect("ctx.web"), &runtime));
    (context, runtime)
}

fn search_request(max_results: Option<u64>) -> WebSearchRequest {
    WebSearchRequest {
        query: "q".to_owned(),
        max_results,
    }
}

fn fetch_request() -> WebFetchRequest {
    WebFetchRequest {
        url: "https://example.com".to_owned(),
    }
}

fn harness_error(error: &anyhow::Error) -> &HarnessError {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<HarnessError>())
        .expect("HarnessError")
}

async fn assert_search_code(runtime: &WebRuntime, code: &str) {
    let error = runtime
        .search(&search_request(None), None)
        .await
        .expect_err(code);
    assert_eq!(harness_error(&error).code(), code);
    assert_eq!(harness_error(&error).name(), WEB_ERROR_NAME);
}

#[tokio::test]
async fn registrations_return_disposers_use_independent_namespaces_and_follow_fiber_lifetime()
-> anyhow::Result<()> {
    let (context, runtime) = mount_runtime(&WebRuntimeConfig::default());
    let registration =
        runtime.register_search_provider(&context, search_provider("exa", true, "exa"))?;
    assert_eq!(
        runtime
            .search(&search_request(None), None)
            .await?
            .content
            .as_deref(),
        Some("exa")
    );
    registration.dispose().await?;
    assert_search_code(&runtime, "WEB_PROVIDER_UNAVAILABLE").await;

    let search =
        runtime.register_search_provider(&context, search_provider("shared", true, "search"))?;
    let fetch =
        runtime.register_fetch_provider(&context, fetch_provider("shared", true, "fetch"))?;
    assert_eq!(
        runtime.fetch(&fetch_request(), None).await?.body,
        WebFetchBody::Text {
            content: "fetch".to_owned()
        }
    );
    search.dispose().await?;
    fetch.dispose().await?;

    let child_fiber = Fiber::active_child("web provider");
    let child = context.with_fiber(child_fiber.clone());
    runtime.register_search_provider(&child, search_provider("child", true, "child"))?;
    assert_eq!(
        runtime
            .search(&search_request(None), None)
            .await?
            .content
            .as_deref(),
        Some("child")
    );
    child_fiber.dispose().await?;
    assert_search_code(&runtime, "WEB_PROVIDER_UNAVAILABLE").await;

    let inactive_fiber = Fiber::active_child("inactive");
    inactive_fiber.dispose().await?;
    let inactive = context.with_fiber(inactive_fiber);
    assert!(
        runtime
            .register_search_provider(&inactive, search_provider("rollback", true, "bad"))
            .is_err()
    );
    runtime.register_search_provider(&context, search_provider("rollback", true, "good"))?;
    Ok(())
}

#[tokio::test]
async fn duplicate_and_unavailable_provider_errors_have_exact_codes() -> anyhow::Result<()> {
    let (context, runtime) = mount_runtime(&WebRuntimeConfig::default());
    runtime.register_search_provider(&context, search_provider("exa", true, "exa"))?;
    let duplicate = runtime
        .register_search_provider(&context, search_provider("exa", true, "again"))
        .expect_err("duplicate");
    assert_eq!(harness_error(&duplicate).code(), "WEB_DUPLICATE_PROVIDER");

    let (empty_context, empty) = mount_runtime(&WebRuntimeConfig::default());
    assert_search_code(&empty, "WEB_PROVIDER_UNAVAILABLE").await;
    empty.register_search_provider(
        &empty_context,
        search_provider("disabled", false, "disabled"),
    )?;
    assert_search_code(&empty, "WEB_PROVIDER_UNAVAILABLE").await;
    Ok(())
}

#[tokio::test]
async fn configured_selection_distinguishes_missing_unavailable_and_usable() -> anyhow::Result<()> {
    let (missing_context, missing) = mount_runtime(&WebRuntimeConfig {
        search_provider: Some("perplexity".to_owned()),
        fetch_provider: None,
    });
    missing.register_search_provider(&missing_context, search_provider("exa", true, "exa"))?;
    assert_search_code(&missing, "WEB_PROVIDER_CONFIGURED_MISSING").await;

    let (unavailable_context, unavailable) = mount_runtime(&WebRuntimeConfig {
        search_provider: Some("exa".to_owned()),
        fetch_provider: None,
    });
    unavailable
        .register_search_provider(&unavailable_context, search_provider("exa", false, "exa"))?;
    assert_search_code(&unavailable, "WEB_PROVIDER_CONFIGURED_UNAVAILABLE").await;

    let (selected_context, selected) = mount_runtime(&WebRuntimeConfig {
        search_provider: Some("perplexity".to_owned()),
        fetch_provider: None,
    });
    selected.register_search_provider(&selected_context, search_provider("exa", true, "exa"))?;
    selected.register_search_provider(
        &selected_context,
        search_provider("perplexity", true, "perplexity"),
    )?;
    assert_eq!(
        selected
            .search(&search_request(None), None)
            .await?
            .content
            .as_deref(),
        Some("perplexity")
    );
    Ok(())
}

#[tokio::test]
async fn auto_selection_is_order_independent_and_ambiguity_lists_registration_order()
-> anyhow::Result<()> {
    let (context, runtime) = mount_runtime(&WebRuntimeConfig::default());
    runtime.register_search_provider(&context, search_provider("exa", true, "exa"))?;
    runtime
        .register_search_provider(&context, search_provider("perplexity", false, "perplexity"))?;
    assert_eq!(
        runtime
            .search(&search_request(None), None)
            .await?
            .content
            .as_deref(),
        Some("exa")
    );

    let (reverse_context, reverse) = mount_runtime(&WebRuntimeConfig::default());
    reverse.register_search_provider(
        &reverse_context,
        search_provider("perplexity", true, "perplexity"),
    )?;
    reverse.register_search_provider(&reverse_context, search_provider("exa", false, "exa"))?;
    assert_eq!(
        reverse
            .search(&search_request(None), None)
            .await?
            .content
            .as_deref(),
        Some("perplexity")
    );

    let (ambiguous_context, ambiguous) = mount_runtime(&WebRuntimeConfig::default());
    ambiguous.register_search_provider(&ambiguous_context, search_provider("exa", true, "exa"))?;
    ambiguous.register_search_provider(
        &ambiguous_context,
        search_provider("perplexity", true, "perplexity"),
    )?;
    let error = ambiguous
        .search(&search_request(None), None)
        .await
        .expect_err("ambiguous");
    assert_eq!(harness_error(&error).code(), "WEB_PROVIDER_AMBIGUOUS");
    assert!(error.to_string().contains("exa, perplexity"), "{error}");
    Ok(())
}

#[tokio::test]
async fn search_returns_provider_data_forwards_signal_and_enforces_max_results()
-> anyhow::Result<()> {
    let (context, runtime) = mount_runtime(&WebRuntimeConfig::default());
    let provider = Arc::new(SearchProvider {
        id: "exa".to_owned(),
        available: true,
        result: WebSearchResult {
            content: Some("answer".to_owned()),
            sources: ["1", "2", "3"]
                .into_iter()
                .map(|id| WebSearchSource {
                    url: format!("https://{id}"),
                    title: None,
                    snippet: None,
                    published_at: None,
                })
                .collect(),
            truncated: false,
        },
        signals: Arc::new(Mutex::new(Vec::new())),
    });
    runtime.register_search_provider(&context, provider.clone())?;
    let signal = AbortSignal::default();
    let result = runtime
        .search(&search_request(Some(2)), Some(signal.clone()))
        .await?;
    assert_eq!(result.content.as_deref(), Some("answer"));
    assert_eq!(result.sources.len(), 2);
    assert!(result.truncated);
    assert_eq!(provider.signals.lock().as_slice(), &[Some(signal)]);

    let within = runtime.search(&search_request(Some(8)), None).await?;
    assert_eq!(within.sources.len(), 3);
    assert!(!within.truncated);
    let unbounded = runtime.search(&search_request(None), None).await?;
    assert_eq!(unbounded.sources.len(), 3);
    assert!(!unbounded.truncated);
    Ok(())
}

#[tokio::test]
async fn fetch_resolves_independently_and_missing_fetch_has_unavailable_code() -> anyhow::Result<()>
{
    let (context, runtime) = mount_runtime(&WebRuntimeConfig::default());
    runtime.register_search_provider(&context, search_provider("search", true, "search"))?;
    let error = runtime
        .fetch(&fetch_request(), None)
        .await
        .expect_err("missing fetch");
    assert_eq!(harness_error(&error).code(), "WEB_PROVIDER_UNAVAILABLE");

    runtime.register_fetch_provider(&context, fetch_provider("http", true, "http"))?;
    let fetched = runtime.fetch(&fetch_request(), None).await?;
    assert_eq!(fetched.status_code, 200);
    assert_eq!(
        fetched.body,
        WebFetchBody::Text {
            content: "http".to_owned()
        }
    );
    Ok(())
}

#[test]
fn web_error_is_named_harness_error_with_open_string_code() {
    let error = web_error("boom", "WEB_INVALID_URL");
    assert_eq!(error.name(), WEB_ERROR_NAME);
    assert_eq!(error.code(), "WEB_INVALID_URL");
}
