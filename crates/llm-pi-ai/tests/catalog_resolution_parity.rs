//! Catalog/profile merge parity tests independent of the generated catalog snapshot.

use std::collections::HashMap;

use seekdeep_llm::{ModelId, ProviderId};
use seekdeep_llm_pi_ai::{
    catalog::{
        CATALOG_SOURCE_COMMIT, CatalogIndex, CatalogProvider, PI_AI_CATALOG_VERSION,
        PiCompatProfile, PiModality, PiModel, PiModelCost, PiModelFields, PiModelProfile,
        PiReasoningEfforts, PiThinkingFormat, PiThinkingLevel, RouteCatalogRequest,
        builtin_catalog, resolve_route_models,
    },
    replay::PiApi,
};
use serde_json::{Map, Value, json};

fn model(id: &str, api: &str) -> PiModel {
    PiModel {
        id: ModelId::new(id),
        name: format!("Catalog {id}"),
        api: PiApi::new(api),
        base_url: "https://catalog.test/v1".to_owned(),
        provider: ProviderId::new("catalog"),
        reasoning: true,
        input: vec![PiModality::Text, PiModality::Image],
        cost: PiModelCost {
            input: 1.0,
            output: 2.0,
            cache_read: 0.5,
            cache_write: 0.25,
        },
        context_window: 100_000,
        max_tokens: 8_192,
        thinking_level_map: Some(Map::from_iter([
            ("off".to_owned(), json!("none")),
            ("high".to_owned(), json!("high")),
        ])),
        compat: Some(Map::from_iter([
            ("supportsStore".to_owned(), json!(false)),
            (
                "requiresReasoningContentOnAssistantMessages".to_owned(),
                json!(true),
            ),
        ])),
        extra: Map::from_iter([("futureCatalogField".to_owned(), json!({"kept":true}))]),
    }
}

fn catalog() -> CatalogIndex {
    CatalogIndex::new(vec![CatalogProvider {
        id: ProviderId::new("catalog"),
        name: "Catalog".to_owned(),
        base_url: Some("https://provider.test/v1".to_owned()),
        listed: true,
        api_key_name: Some("Catalog API key".to_owned()),
        oauth: None,
        models: vec![
            model("first", "openai-completions"),
            model("second", "openai-completions"),
        ],
    }])
    .unwrap()
}

fn profile(id: &str, fields: PiModelFields) -> PiModelProfile {
    PiModelProfile {
        id: ModelId::new(id),
        fields,
    }
}

#[test]
fn generated_snapshot_is_complete_pinned_and_directory_scoped() {
    let catalog = builtin_catalog();
    assert_eq!(PI_AI_CATALOG_VERSION, "0.82.1");
    assert_eq!(
        CATALOG_SOURCE_COMMIT,
        "37200a934324dd7167ec8a8d3ac1fd01e2239909"
    );
    assert_eq!(catalog.provider_ids().len(), 37);
    assert!(catalog.provider_ids().iter().any(|id| id == "deepseek"));
    assert!(!catalog.provider_ids().iter().any(|id| id == "radius"));
    assert!(catalog.provider("radius").is_some());
    let model_count = catalog
        .provider_ids()
        .iter()
        .filter_map(|id| catalog.provider(id))
        .map(|provider| provider.models.len())
        .sum::<usize>()
        + catalog.provider("radius").unwrap().models.len();
    assert_eq!(model_count, 1_109);
    let deepseek = catalog.provider("deepseek").unwrap();
    assert_eq!(deepseek.models.len(), 2);
    assert_eq!(deepseek.api_key_name.as_deref(), Some("DeepSeek API key"));
    assert_eq!(deepseek.models[0].id.as_str(), "deepseek-v4-flash");
    assert_eq!(deepseek.models[0].context_window, 1_000_000);
}

#[test]
fn hand_declared_route_uses_route_defaults_and_only_explicit_request_caps() {
    let mut request = RouteCatalogRequest::new(ProviderId::new("acme-gateway"));
    request.api = Some(PiApi::new("openai-completions"));
    request.base_url = Some("https://acme.test/v1".to_owned());
    request.default_context_window = 4_096;
    request.default_max_tokens = 256;
    request.default_input = vec![PiModality::Text, PiModality::Image];
    request.models = vec![
        profile("bare", PiModelFields::default()),
        profile(
            "sized",
            PiModelFields {
                name: Some("Sized".to_owned()),
                context_window: Some(8_192),
                max_tokens: Some(512),
                input: vec![PiModality::Text],
                ..PiModelFields::default()
            },
        ),
    ];
    let resolved = resolve_route_models(&CatalogIndex::default(), &request).unwrap();
    assert_eq!(resolved.models.len(), 2);
    assert_eq!(resolved.models[0].context_window, 4_096);
    assert_eq!(resolved.models[0].max_tokens, 256);
    assert_eq!(
        resolved.models[0].input,
        vec![PiModality::Text, PiModality::Image]
    );
    assert!(
        !resolved
            .configured_max_tokens
            .contains_key(&ModelId::new("bare"))
    );
    assert_eq!(
        resolved.configured_max_tokens.get(&ModelId::new("sized")),
        Some(&512)
    );
    assert_eq!(resolved.models[1].name, "Sized");
    assert_eq!(resolved.models[1].input, vec![PiModality::Text]);
}

#[test]
fn catalog_inheritance_and_overrides_preserve_unknown_upstream_fields() {
    let untouched = resolve_route_models(
        &catalog(),
        &RouteCatalogRequest::new(ProviderId::new("catalog")),
    )
    .unwrap();
    assert_eq!(
        untouched.models,
        catalog().provider("catalog").unwrap().models
    );

    let mut request = RouteCatalogRequest::new(ProviderId::new("catalog"));
    request.model_overrides.push((
        "first".to_owned(),
        PiModelFields {
            name: Some("Proxied".to_owned()),
            max_tokens: Some(4_096),
            input: vec![],
            ..PiModelFields::default()
        },
    ));
    let resolved = resolve_route_models(&catalog(), &request).unwrap();
    assert_eq!(resolved.models.len(), 2);
    let first = &resolved.models[0];
    assert_eq!(first.name, "Proxied");
    assert_eq!(first.context_window, 100_000);
    assert_eq!(first.input, vec![PiModality::Text, PiModality::Image]);
    assert_eq!(first.extra["futureCatalogField"], json!({"kept":true}));
    assert_eq!(
        first.compat.as_ref().unwrap()["supportsStore"],
        json!(false)
    );
    assert_eq!(resolved.models[1], model("second", "openai-completions"));
    assert_eq!(
        resolved.configured_max_tokens.get(&ModelId::new("first")),
        Some(&4_096)
    );
}

#[test]
fn same_protocol_catalog_supplies_api_and_provider_endpoint_for_new_models() {
    let mut request = RouteCatalogRequest::new(ProviderId::new("catalog"));
    request.models = vec![profile("new-release", PiModelFields::default())];
    let resolved = resolve_route_models(&catalog(), &request).unwrap();
    assert_eq!(resolved.models[0].api.as_str(), "openai-completions");
    assert_eq!(resolved.models[0].base_url, "https://provider.test/v1");
}

#[test]
fn rejects_every_misplaced_or_underspecified_declaration() {
    let cases = [
        (
            {
                let mut request = RouteCatalogRequest::new(ProviderId::new("custom"));
                request.base_url = Some("https://x".to_owned());
                request.models = vec![profile("m", PiModelFields::default())];
                request
            },
            "needs an api",
        ),
        (
            {
                let mut request = RouteCatalogRequest::new(ProviderId::new("custom"));
                request.api = Some(PiApi::new("openai-completions"));
                request.models = vec![profile("m", PiModelFields::default())];
                request
            },
            "needs a baseURL",
        ),
        (
            {
                let mut request = RouteCatalogRequest::new(ProviderId::new("custom"));
                request.api = Some(PiApi::new("openai-completions"));
                request.base_url = Some("https://x".to_owned());
                request.models = vec![
                    profile("dup", PiModelFields::default()),
                    profile("dup", PiModelFields::default()),
                ];
                request
            },
            "more than once",
        ),
        (
            {
                let mut request = RouteCatalogRequest::new(ProviderId::new("custom"));
                request.api = Some(PiApi::new("openai-completions"));
                request.base_url = Some("https://x".to_owned());
                request.default_input.clear();
                request.models = vec![profile("m", PiModelFields::default())];
                request
            },
            "defaultInput must name at least one modality",
        ),
    ];
    for (request, expected) in cases {
        let error = resolve_route_models(&CatalogIndex::default(), &request).unwrap_err();
        assert!(error.to_string().contains(expected), "{error:#}");
    }

    let mut unknown_override = RouteCatalogRequest::new(ProviderId::new("catalog"));
    unknown_override
        .model_overrides
        .push(("ghost".to_owned(), PiModelFields::default()));
    assert!(
        resolve_route_models(&catalog(), &unknown_override)
            .unwrap_err()
            .to_string()
            .contains("installed catalog does not describe")
    );
}

#[test]
fn reasoning_declarations_are_complete_and_validate_wire_values() {
    let declared = HashMap::from([
        (PiThinkingLevel::Off, None),
        (PiThinkingLevel::Low, Some("low".to_owned())),
        (PiThinkingLevel::Max, Some("ultra".to_owned())),
    ]);
    let mut request = RouteCatalogRequest::new(ProviderId::new("custom"));
    request.api = Some(PiApi::new("openai-completions"));
    request.base_url = Some("https://x".to_owned());
    request.models = vec![profile(
        "thinker",
        PiModelFields {
            reasoning_efforts: Some(PiReasoningEfforts::Declared(declared)),
            ..PiModelFields::default()
        },
    )];
    let model = resolve_route_models(&CatalogIndex::default(), &request)
        .unwrap()
        .models
        .remove(0);
    assert!(model.reasoning);
    assert_eq!(
        model.thinking_level_map.unwrap(),
        Map::from_iter([
            ("minimal".to_owned(), Value::Null),
            ("low".to_owned(), json!("low")),
            ("medium".to_owned(), Value::Null),
            ("high".to_owned(), Value::Null),
            ("xhigh".to_owned(), Value::Null),
            ("max".to_owned(), json!("ultra")),
        ])
    );

    for efforts in [
        PiReasoningEfforts::Empty,
        PiReasoningEfforts::Declared(HashMap::from([(PiThinkingLevel::Off, None)])),
        PiReasoningEfforts::Declared(HashMap::from([(PiThinkingLevel::High, None)])),
        PiReasoningEfforts::Declared(HashMap::from([(
            PiThinkingLevel::High,
            Some(String::new()),
        )])),
    ] {
        request.models[0].fields.reasoning_efforts = Some(efforts);
        assert!(resolve_route_models(&CatalogIndex::default(), &request).is_err());
    }
}

#[test]
fn compat_switches_merge_only_into_openai_completions_models() {
    let mut request = RouteCatalogRequest::new(ProviderId::new("catalog"));
    request.models = vec![profile(
        "first",
        PiModelFields {
            compat: Some(PiCompatProfile {
                thinking_format: Some(PiThinkingFormat::DeepSeek),
                supports_reasoning_effort: Some(false),
            }),
            ..PiModelFields::default()
        },
    )];
    let resolved_model = resolve_route_models(&catalog(), &request)
        .unwrap()
        .models
        .remove(0);
    let compat = resolved_model.compat.unwrap();
    assert_eq!(compat["supportsStore"], json!(false));
    assert_eq!(compat["thinkingFormat"], json!("deepseek"));
    assert_eq!(compat["supportsReasoningEffort"], json!(false));

    let mixed = CatalogIndex::new(vec![CatalogProvider {
        id: ProviderId::new("other"),
        name: "Other".to_owned(),
        base_url: None,
        listed: true,
        api_key_name: Some("Other API key".to_owned()),
        oauth: None,
        models: vec![model("response", "openai-responses")],
    }])
    .unwrap();
    let mut invalid = RouteCatalogRequest::new(ProviderId::new("other"));
    invalid.models = vec![profile(
        "response",
        PiModelFields {
            compat: Some(PiCompatProfile {
                thinking_format: Some(PiThinkingFormat::OpenAi),
                supports_reasoning_effort: None,
            }),
            ..PiModelFields::default()
        },
    )];
    let error = resolve_route_models(&mixed, &invalid).unwrap_err();
    assert!(error.to_string().contains("only on openai-completions"));
}
