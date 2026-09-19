//! Credential-gated live mirror of the source E2E probe (disabled upstream).

use seekdeep_web::{WebSearchProvider, WebSearchRequest};
use seekdeep_web_search_deepseek::{
    DEEPSEEK_DEFAULT_API_VERSION, DEEPSEEK_DEFAULT_BASE_URL, DEEPSEEK_DEFAULT_MAX_TOKENS,
    DEEPSEEK_DEFAULT_MAX_USES, DEEPSEEK_DEFAULT_MODEL, DeepSeekSearchProvider,
    DeepSeekSearchProviderOptions,
};

#[tokio::test]
#[ignore = "disabled real-API probe; run deliberately with --ignored"]
async fn returns_citeable_sources_for_live_query() {
    let api_key = std::env::var("DEEPSEEK_API_KEY").expect("DEEPSEEK_API_KEY");
    let base_url = std::env::var("DEEPSEEK_SEARCH_BASE_URL")
        .unwrap_or_else(|_| DEEPSEEK_DEFAULT_BASE_URL.to_owned());
    let model = std::env::var("DEEPSEEK_SEARCH_MODEL")
        .unwrap_or_else(|_| DEEPSEEK_DEFAULT_MODEL.to_owned());
    let provider = DeepSeekSearchProvider::new(move || DeepSeekSearchProviderOptions {
        api_key: Some(api_key.clone()),
        resolve_api_key: None,
        api_key_env: None,
        base_url: base_url.clone(),
        model: model.clone(),
        api_version: DEEPSEEK_DEFAULT_API_VERSION.to_owned(),
        max_tokens: DEEPSEEK_DEFAULT_MAX_TOKENS,
        max_uses: DEEPSEEK_DEFAULT_MAX_USES,
        record_request: None,
    });
    let result = provider
        .search(
            &WebSearchRequest {
                query: "What is DeepSeek Harness?".to_owned(),
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
