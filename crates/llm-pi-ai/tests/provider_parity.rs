//! Provider construction and authentication inheritance parity tests.

use seekdeep_llm::ProviderId;
use seekdeep_llm_pi_ai::{
    catalog::builtin_catalog,
    provider::{PiProtocol, PiProviderDispatch, ProviderSpec, build_provider, supported_protocols},
    replay::PiApi,
};

fn spec(provider: &str) -> ProviderSpec {
    ProviderSpec {
        provider: ProviderId::new(provider),
        display_name: provider.to_owned(),
        api: None,
        base_url: None,
        models: builtin_catalog()
            .provider(provider)
            .map(|provider| provider.models.clone())
            .unwrap_or_default(),
        names_credential: false,
    }
}

#[test]
fn supported_protocol_order_is_stable_and_deliberately_narrow() {
    assert_eq!(
        supported_protocols(),
        vec![
            "openai-completions",
            "openai-responses",
            "anthropic-messages"
        ]
    );
    for unsupported in [
        "bedrock-converse-stream",
        "google-vertex",
        "azure-openai-responses",
        "openai-codex-responses",
    ] {
        assert!(!supported_protocols().contains(&unsupported));
    }
}

#[test]
fn catalog_route_reuses_native_dispatch_models_endpoint_and_auth() {
    let mut input = spec("deepseek");
    input.display_name = "DeepSeek Route".to_owned();
    input.base_url = Some("https://proxy.test/v1".to_owned());
    let provider = build_provider(builtin_catalog(), input.clone()).unwrap();
    assert_eq!(provider.id, input.provider);
    assert_eq!(provider.name, "DeepSeek Route");
    assert_eq!(provider.base_url.as_deref(), Some("https://proxy.test/v1"));
    assert_eq!(provider.models, input.models);
    assert_eq!(
        provider.auth.api_key_name.as_deref(),
        Some("DeepSeek API key")
    );
    assert!(matches!(
        provider.dispatch,
        PiProviderDispatch::Catalog { ref provider } if provider.as_str() == "deepseek"
    ));
}

#[test]
fn explicit_protocol_repoints_catalog_but_keeps_provider_native_auth() {
    let mut input = spec("openai");
    input.api = Some(PiApi::new("openai-completions"));
    let provider = build_provider(builtin_catalog(), input).unwrap();
    assert_eq!(
        provider.auth.api_key_name.as_deref(),
        Some("OpenAI API key")
    );
    assert!(matches!(
        provider.dispatch,
        PiProviderDispatch::Protocol(PiProtocol::OpenAiCompletions)
    ));
}

#[test]
fn hand_declared_routes_require_supported_protocol_and_get_harness_auth() {
    let missing = build_provider(builtin_catalog(), spec("acme-gateway")).unwrap_err();
    assert!(missing.to_string().contains("api \"undefined\""));
    let mut unsupported = spec("acme-gateway");
    unsupported.api = Some(PiApi::new("quantum-telepathy"));
    assert!(
        build_provider(builtin_catalog(), unsupported)
            .unwrap_err()
            .to_string()
            .contains("supported protocols are")
    );
    let mut declared = spec("acme-gateway");
    declared.display_name = "Acme".to_owned();
    declared.api = Some(PiApi::new("openai-responses"));
    let provider = build_provider(builtin_catalog(), declared).unwrap();
    assert_eq!(provider.auth.api_key_name.as_deref(), Some("Acme"));
    assert!(provider.auth.oauth.is_none());
    assert!(matches!(
        provider.dispatch,
        PiProviderDispatch::Protocol(PiProtocol::OpenAiResponses)
    ));
}

#[test]
fn oauth_only_codex_adds_harness_key_method_only_when_profile_names_one() {
    let keyless = build_provider(builtin_catalog(), spec("openai-codex")).unwrap();
    assert!(keyless.auth.api_key_name.is_none());
    assert_eq!(
        keyless.auth.oauth.as_ref().unwrap().name,
        "OpenAI (ChatGPT Plus/Pro)"
    );

    let mut keyed = spec("openai-codex");
    keyed.display_name = "Codex Route".to_owned();
    keyed.names_credential = true;
    let keyed = build_provider(builtin_catalog(), keyed).unwrap();
    assert_eq!(keyed.auth.api_key_name.as_deref(), Some("Codex Route"));
    assert_eq!(
        keyed.auth.oauth.as_ref().unwrap().name,
        "OpenAI (ChatGPT Plus/Pro)"
    );
}
