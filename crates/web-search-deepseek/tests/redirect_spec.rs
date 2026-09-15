//! Parity mirror of the source redirect.spec.ts redirect-policy suite.

mod support;

use seekdeep_llm::HarnessError;
use seekdeep_web::{WebSearchProvider, WebSearchRequest};
use seekdeep_web_search_deepseek::{DeepSeekSearchProvider, DeepSeekSearchProviderOptions};
use support::{MockServer, ResponseSpec};

fn provider(base_url: String) -> DeepSeekSearchProvider {
    DeepSeekSearchProvider::new(move || DeepSeekSearchProviderOptions {
        api_key: Some("redirect-test-key".to_owned()),
        resolve_api_key: None,
        api_key_env: None,
        base_url: base_url.clone(),
        model: "deepseek-chat".to_owned(),
        api_version: "2023-06-01".to_owned(),
        max_tokens: 32,
        max_uses: 1,
        record_request: None,
    })
}

fn code(error: &anyhow::Error) -> &str {
    error
        .downcast_ref::<HarnessError>()
        .map_or("?", HarnessError::code)
}

#[tokio::test]
async fn rejects_redirects_before_contacting_location() {
    for status in [301_u16, 302, 303, 307, 308] {
        let target = MockServer::start(ResponseSpec::plain(204, "")).await;
        let redirect = MockServer::start(
            ResponseSpec::plain(status, "").header("location", format!("{}/collect", target.url)),
        )
        .await;
        let provider = provider(redirect.url.clone());
        let err = provider
            .search(
                &WebSearchRequest {
                    query: "private redirect query".to_owned(),
                    max_results: None,
                },
                None,
            )
            .await
            .expect_err("redirect");
        assert_eq!(code(&err), "WEB_PROVIDER_ERROR");
        assert!(
            target.take_requests().is_empty(),
            "target must not be contacted for status {status}"
        );
    }
}
