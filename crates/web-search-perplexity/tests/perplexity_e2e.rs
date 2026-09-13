//! Credential-gated live mirror of the source E2E probe (disabled upstream).

use seekdeep_web::{WebSearchProvider, WebSearchRequest};
use seekdeep_web_search_perplexity::{
    PERPLEXITY_DEFAULT_BASE_URL, PERPLEXITY_DEFAULT_MAX_TOKENS, PERPLEXITY_DEFAULT_MODEL,
    PerplexitySearchProvider, PerplexitySearchProviderOptions,
};

#[tokio::test]
#[ignore = "disabled real-API probe; run deliberately with --ignored"]
async fn returns_a_generated_answer_and_sources_for_live_query() {
    let api_key = std::env::var("PERPLEXITY_API_KEY").expect("PERPLEXITY_API_KEY");
    let base_url = std::env::var("PERPLEXITY_BASE_URL")
        .unwrap_or_else(|_| PERPLEXITY_DEFAULT_BASE_URL.to_owned());
    let model =
        std::env::var("PERPLEXITY_MODEL").unwrap_or_else(|_| PERPLEXITY_DEFAULT_MODEL.to_owned());
    let provider = PerplexitySearchProvider::new(PerplexitySearchProviderOptions {
        api_key,
        base_url,
        model,
        max_tokens: PERPLEXITY_DEFAULT_MAX_TOKENS,
        search_recency: None,
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
    assert!(result.content.as_deref().is_some_and(|c| !c.is_empty()));
    for source in &result.sources {
        assert!(source.url.starts_with("http://") || source.url.starts_with("https://"));
    }
}
