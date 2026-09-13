//! Credential-gated live mirror of the source E2E probe (disabled upstream).

use seekdeep_web::{WebSearchProvider, WebSearchRequest};
use seekdeep_web_search_exa::{
    EXA_DEFAULT_BASE_URL, EXA_DEFAULT_HIGHLIGHTS_PER_RESULT, EXA_DEFAULT_SEARCH_TYPE,
    ExaSearchProvider, ExaSearchProviderOptions,
};

#[tokio::test]
#[ignore = "disabled real-API probe; run deliberately with --ignored"]
async fn returns_sources_for_live_query() {
    let api_key = std::env::var("EXA_API_KEY").expect("EXA_API_KEY");
    let base_url =
        std::env::var("EXA_BASE_URL").unwrap_or_else(|_| EXA_DEFAULT_BASE_URL.to_owned());
    let provider = ExaSearchProvider::new(ExaSearchProviderOptions {
        api_key,
        base_url,
        search_type: EXA_DEFAULT_SEARCH_TYPE,
        num_results: None,
        highlights_per_result: EXA_DEFAULT_HIGHLIGHTS_PER_RESULT,
    });
    let result = provider
        .search(
            &WebSearchRequest {
                query: "DeepSeek Harness".to_owned(),
                max_results: Some(5),
            },
            None,
        )
        .await
        .expect("search");
    assert!(!result.sources.is_empty());
    for source in &result.sources {
        assert!(source.url.starts_with("http://") || source.url.starts_with("https://"));
    }
}
