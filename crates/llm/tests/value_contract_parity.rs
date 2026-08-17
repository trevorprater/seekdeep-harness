//! Provider-neutral value, identity, validation, and wire-contract parity.

use std::{collections::BTreeMap, error::Error as _};

use seekdeep_llm::{
    APP_IDENTITY, ApiKeyCheck, AppIdentity, CallId, ContentBlock, FinishReason,
    INVALID_CREDENTIAL_CODE, LlmCallConfig, LlmError, Message, MessageRole, MessageSource,
    ProviderRequestId, ReasoningEffortId, SessionId, assert_usable_api_key, attribution_headers,
    attribution_headers_for, bound_context_summary, call_config_equals, content_has_image,
    error_chain, is_context_window_exceeded_error, is_harness_error, is_quota_exceeded_error,
    normalize_api_key, resolve_retry_policy, user_agent_for,
};
use serde_json::json;

fn config() -> LlmCallConfig {
    LlmCallConfig {
        provider: "p".into(),
        model: "m".into(),
        reasoning_effort: None,
        temperature: None,
        max_tokens: None,
        stop: None,
    }
}

fn assert_float(actual: f64, expected: f64) {
    assert!((actual - expected).abs() <= f64::EPSILON);
}

#[test]
fn api_key_normalization_and_refusal_match_every_source_partition() {
    assert_eq!(
        normalize_api_key("sk-0123456789abcdef"),
        ApiKeyCheck::Usable("sk-0123456789abcdef".into())
    );
    assert_eq!(
        normalize_api_key("  sk-abc\t\n"),
        ApiKeyCheck::Usable("sk-abc".into())
    );
    for raw in ["", "   ", "\t"] {
        assert_eq!(normalize_api_key(raw), ApiKeyCheck::Empty);
    }
    for raw in [
        "sk-😀abc",
        "sk-你好",
        "sk-abc，",
        "sk-abc def",
        "sk-abc\x01",
        "sk-café",
    ] {
        assert_eq!(normalize_api_key(raw), ApiKeyCheck::IllegalCharacters);
    }
    assert_eq!(normalize_api_key("!~"), ApiKeyCheck::Usable("!~".into()));
    assert_eq!(
        assert_usable_api_key("  sk-abc  ", "llm-deepseek", "DEEPSEEK_API_KEY").unwrap(),
        "sk-abc"
    );
    let blank = assert_usable_api_key("   ", "llm-deepseek", "DEEPSEEK_API_KEY").unwrap_err();
    assert_eq!(blank.code(), INVALID_CREDENTIAL_CODE);
    assert!(blank.to_string().contains("DEEPSEEK_API_KEY is blank"));
    let secret =
        assert_usable_api_key("sk-😀supersecret", "llm-deepseek", "DEEPSEEK_API_KEY").unwrap_err();
    assert_eq!(secret.code(), INVALID_CREDENTIAL_CODE);
    assert!(!secret.to_string().contains("supersecret"));
}

#[test]
fn attribution_uses_only_static_renamed_public_product_facts() {
    assert_eq!(APP_IDENTITY.product, "seekdeep-harness");
    assert_eq!(APP_IDENTITY.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(
        APP_IDENTITY.url,
        "https://github.com/deepseek-ai/seekdeep-harness"
    );
    assert_eq!(
        attribution_headers(),
        BTreeMap::from([(
            "user-agent".into(),
            format!(
                "seekdeep-harness/{} (+https://github.com/deepseek-ai/seekdeep-harness)",
                env!("CARGO_PKG_VERSION")
            )
        )])
    );
    let custom = AppIdentity {
        product: "white-label",
        version: "1.2.3",
        url: "https://example.invalid/app",
    };
    assert_eq!(
        user_agent_for(&custom),
        "white-label/1.2.3 (+https://example.invalid/app)"
    );
    assert_eq!(attribution_headers_for(&custom).len(), 1);
}

#[test]
fn call_config_compares_every_epoch_field_and_stop_position() {
    let base = config();
    assert!(call_config_equals(&base, &base));
    let mutations = [
        LlmCallConfig {
            provider: "x".into(),
            ..base.clone()
        },
        LlmCallConfig {
            model: "x".into(),
            ..base.clone()
        },
        LlmCallConfig {
            reasoning_effort: Some(ReasoningEffortId::new("high")),
            ..base.clone()
        },
        LlmCallConfig {
            temperature: Some(0.5),
            ..base.clone()
        },
        LlmCallConfig {
            max_tokens: Some(1),
            ..base.clone()
        },
        LlmCallConfig {
            stop: Some(vec!["a".into()]),
            ..base.clone()
        },
    ];
    for mutation in mutations {
        assert!(!call_config_equals(&base, &mutation));
    }
    let mut left = base.clone();
    left.stop = Some(vec!["a".into(), "b".into()]);
    let mut right = left.clone();
    assert!(call_config_equals(&left, &right));
    right.stop = Some(vec!["b".into(), "a".into()]);
    assert!(!call_config_equals(&left, &right));
}

#[test]
fn provider_and_model_newtypes_keep_the_exact_string_wire_contract() {
    let config = LlmCallConfig {
        provider: seekdeep_llm::ProviderId::new("deepseek-official"),
        model: seekdeep_llm::ModelId::new("deepseek-v4-flash"),
        reasoning_effort: None,
        temperature: None,
        max_tokens: None,
        stop: None,
    };
    assert_eq!(
        serde_json::to_value(&config).unwrap(),
        json!({
            "provider": "deepseek-official",
            "model": "deepseek-v4-flash"
        })
    );
    assert_eq!(
        serde_json::from_value::<LlmCallConfig>(json!({
            "provider": "deepseek-official",
            "model": "deepseek-v4-flash"
        }))
        .unwrap(),
        config
    );
}

#[test]
fn message_construction_detaches_inputs_fixes_tags_and_correlates_tool_results() {
    let mut caller_content = vec![ContentBlock::Text {
        text: "before".into(),
    }];
    let message = Message::new(
        MessageRole::User,
        caller_content.clone(),
        MessageSource::user(),
    );
    caller_content[0] = ContentBlock::Text {
        text: "after".into(),
    };
    assert_ne!(message.content(), caller_content);
    assert!(!message.id().as_str().is_empty());

    let assistant = Message::assistant(Vec::new(), "provider", "model");
    assert_eq!(assistant.role(), MessageRole::Assistant);
    assert_eq!(assistant.source().kind, "model");
    assert_eq!(assistant.source().fields["provider"], "provider");
    assert_eq!(assistant.source().fields["model"], "model");

    let call_id = CallId::new("call-1");
    let result = Message::tool_result(
        &call_id,
        vec![ContentBlock::Text {
            text: "done".into(),
        }],
        true,
    );
    assert_eq!(result.role(), MessageRole::User);
    assert_eq!(result.source().fields["callId"], "call-1");
    let ContentBlock::ToolResult {
        tool_call_id,
        is_error,
        ..
    } = &result.content()[0]
    else {
        panic!("tool-result block");
    };
    assert_eq!(tool_call_id, &call_id);
    assert_eq!(*is_error, Some(true));

    let augmented = Message::new_with_fields(
        MessageRole::User,
        Vec::new(),
        MessageSource::plugin("extension"),
        json!({"pluginMessageField": {"lossless": true}})
            .as_object()
            .unwrap()
            .clone(),
    );
    let wire = serde_json::to_value(&augmented).unwrap();
    assert_eq!(wire["pluginMessageField"], json!({"lossless": true}));
    assert_eq!(serde_json::from_value::<Message>(wire).unwrap(), augmented);

    let reserved = Message::new_with_fields(
        MessageRole::User,
        Vec::new(),
        MessageSource::user(),
        json!({
            "id": "forged",
            "role": "assistant",
            "content": [{"type": "text", "text": "forged"}],
            "source": {"kind": "model"},
            "extension": true
        })
        .as_object()
        .unwrap()
        .clone(),
    );
    let reserved_wire = serde_json::to_value(&reserved).unwrap();
    assert_ne!(reserved_wire["id"], "forged");
    assert_eq!(reserved_wire["role"], "user");
    assert_eq!(reserved_wire["content"], json!([]));
    assert_eq!(reserved_wire["source"], json!({"kind": "user"}));
    assert_eq!(reserved_wire["extension"], true);

    let session_id = SessionId::new("session-1");
    assert_eq!(
        serde_json::to_value(&session_id).unwrap(),
        json!("session-1")
    );
    assert_eq!(
        serde_json::from_value::<SessionId>(json!("session-1")).unwrap(),
        session_id
    );
}

#[test]
fn content_and_summary_helpers_cover_nested_images_and_utf16_boundaries() {
    let image: ContentBlock = serde_json::from_value(json!({
        "type": "image",
        "attachment": {
            "attachmentId": "attachment-1",
            "mediaType": "image/png",
            "bytes": 1,
            "width": 1,
            "height": 1
        }
    }))
    .unwrap();
    assert!(content_has_image(&[ContentBlock::ToolResult {
        tool_call_id: CallId::new("call"),
        content: vec![image],
        is_error: None,
    }]));
    assert!(!content_has_image(&[ContentBlock::Text {
        text: "x".into()
    }]));
    assert_eq!(bound_context_summary("short"), "short");
    let bounded = bound_context_summary(&"😀".repeat(100));
    assert!(bounded.encode_utf16().count() <= 120);
    assert!(bounded.ends_with('…'));
}

#[test]
fn llm_error_validation_and_failure_facts_are_exact_and_serializable() {
    let request_id = ProviderRequestId::new("request-1");
    let error = LlmError::new(
        "busy",
        "RATE_LIMIT",
        Some(429),
        Some(250.5),
        Some(request_id.clone()),
    )
    .unwrap();
    assert_eq!(error.to_string(), "busy");
    assert_eq!(error.code(), "RATE_LIMIT");
    assert_eq!(error.failure().status, Some(429));
    assert_eq!(error.failure().provider_retry_after_ms, Some(250.5));
    assert_eq!(error.failure().request_id, Some(request_id));
    assert!(LlmError::new("", "CODE", None, None, None).is_err());
    assert!(LlmError::new("message", "", None, None, None).is_err());
    assert!(LlmError::new("message", "CODE", Some(99), None, None).is_err());
    assert!(LlmError::new("message", "CODE", Some(600), None, None).is_err());
    assert!(LlmError::new("message", "CODE", None, Some(0.0), None).is_err());
    assert!(LlmError::new("message", "CODE", None, Some(f64::NAN), None).is_err());
    assert!(
        LlmError::new(
            "message",
            "CODE",
            None,
            None,
            Some(ProviderRequestId::new(""))
        )
        .is_err()
    );

    let harness = seekdeep_llm::HarnessError::new("wrapper", "UNKNOWN");
    let llm = LlmError::simple("boom", "AUTH");
    let ordinary = std::io::Error::other("ordinary");
    assert!(is_harness_error(&harness));
    assert!(is_harness_error(&llm));
    assert!(!is_harness_error(&ordinary));
    assert_eq!(harness.name(), "HarnessError");
    assert_eq!(llm.name(), "LlmError");
    assert_eq!(llm.message(), "boom");
}

#[test]
fn context_quota_and_error_chain_classifiers_match_the_source_examples() {
    for detail in [
        "context_length_exceeded maximum context length",
        "context-window-overflowed",
        "This model maximum context length is 128000 tokens",
        "input is too long for this model",
        "request too large for model context",
        "input exceeds the model context window limit",
    ] {
        assert!(is_context_window_exceeded_error(detail), "{detail}");
    }
    for detail in [
        "invalid request: malformed tool arguments",
        "invalid input: temperature exceeds maximum allowed value",
        "input exceeds maximum allowed value",
        "context window size must be positive",
    ] {
        assert!(!is_context_window_exceeded_error(detail), "{detail}");
    }
    for detail in [
        "insufficient_quota",
        "account balance depleted",
        "usage-limit-exceeded",
        "out of credits",
        "You exceeded your current quota",
    ] {
        assert!(is_quota_exceeded_error(detail), "{detail}");
    }
    assert!(!is_quota_exceeded_error("HTTP 429: rate limit reached"));

    let wrapped = seekdeep_llm::HarnessError::new("fetch failed", "TRANSPORT").with_cause(
        std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "connect ECONNREFUSED 127.0.0.1:443",
        ),
    );
    assert_eq!(
        error_chain(&wrapped),
        "fetch failed: connect ECONNREFUSED 127.0.0.1:443"
    );
    assert_eq!(
        wrapped.source().unwrap().to_string(),
        "connect ECONNREFUSED 127.0.0.1:443"
    );
}

#[test]
fn retry_policy_resolves_all_modes_and_rejects_the_complete_invalid_matrix() {
    assert_eq!(
        serde_json::to_value(resolve_retry_policy(None, "provider.retryPolicy").unwrap()).unwrap(),
        json!({
            "mode": "normal",
            "maxRetries": 2,
            "retryableCodes": ["EMPTY_RESPONSE", "RATE_LIMIT", "SERVER", "TIMEOUT", "TRANSPORT"],
            "initialDelayMs": 500.0,
            "maxDelayMs": 10000.0,
            "jitterRatio": 0.1
        })
    );
    let configured = json!({
        "mode": "normal",
        "maxRetries": 4,
        "retryableCodes": ["BUSY"],
        "backoff": {"initialDelayMs": 25, "maxDelayMs": 100, "jitterRatio": 0}
    });
    let normal = resolve_retry_policy(Some(&configured), "provider.retryPolicy").unwrap();
    assert_eq!(normal.max_retries(), Some(4));
    assert_eq!(normal.retryable_codes().unwrap(), ["BUSY"]);
    assert_float(normal.initial_delay_ms(), 25.0);
    assert_float(normal.max_delay_ms(), 100.0);
    assert_float(normal.jitter_ratio(), 0.0);
    let always =
        resolve_retry_policy(Some(&json!({"mode": "always"})), "provider.retryPolicy").unwrap();
    assert_eq!(always.max_retries(), None);
    assert_eq!(always.retryable_codes(), None);
    assert_float(always.initial_delay_ms(), 500.0);
    assert_float(always.max_delay_ms(), 10_000.0);
    assert_float(always.jitter_ratio(), 0.1);

    let invalid = [
        json!({"mode": "normal", "maxRetries": -1}),
        json!({"mode": "normal", "maxRetries": 1.5}),
        json!({"mode": "normal", "maxRetries": 9_007_199_254_740_992_u64}),
        json!({"mode": "always", "backoff": {"initialDelayMs": 0}}),
        json!({"mode": "normal", "backoff": {"maxDelayMs": "infinite"}}),
        json!({"mode": "normal", "backoff": {"initialDelayMs": 2_147_483_648_u64}}),
        json!({"mode": "always", "backoff": {"maxDelayMs": 2_147_483_648_u64}}),
        json!({"mode": "normal", "backoff": {"initialDelayMs": 20, "maxDelayMs": 10}}),
        json!({"mode": "always", "backoff": {"jitterRatio": 1.1}}),
        json!({"mode": "normal", "retryableCodes": []}),
        json!({"mode": "normal", "retryableCodes": ["SERVER", "SERVER"]}),
        json!({"mode": "normal", "retryableCodes": [""]}),
        json!({"mode": "normal", "retryableCodes": [429]}),
        json!({"mode": "normal", "maxRetires": 1}),
        json!({"mode": "always", "maxRetries": 1}),
        json!({"mode": "always", "backoff": {"initialDelay": 1}}),
        json!({"mode": "sometimes"}),
    ];
    for policy in invalid {
        assert!(
            resolve_retry_policy(Some(&policy), "provider.retryPolicy").is_err(),
            "accepted {policy}"
        );
    }
}

#[test]
fn unknown_extensible_values_survive_wire_round_trips() {
    let block_wire = json!({"type": "plugin-block", "answer": 42, "nested": {"x": true}});
    let block: ContentBlock = serde_json::from_value(block_wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(block).unwrap(), block_wire);

    let finish_wire = json!({"kind": "provider-policy", "category": "safety"});
    let finish: FinishReason = serde_json::from_value(finish_wire.clone()).unwrap();
    assert_eq!(finish.kind(), "provider-policy");
    assert_eq!(serde_json::to_value(finish).unwrap(), finish_wire);

    let forged_block = ContentBlock::Unknown {
        block_type: "plugin-block".to_owned(),
        fields: json!({"type": "forged", "answer": 42})
            .as_object()
            .unwrap()
            .clone(),
    };
    assert_eq!(
        serde_json::to_value(forged_block).unwrap(),
        json!({"type": "plugin-block", "answer": 42})
    );
    let forged_finish = FinishReason::Unknown {
        kind: "provider-policy".to_owned(),
        fields: json!({"kind": "forged", "category": "safety"})
            .as_object()
            .unwrap()
            .clone(),
    };
    assert_eq!(
        serde_json::to_value(forged_finish).unwrap(),
        json!({"kind": "provider-policy", "category": "safety"})
    );

    let mut source = MessageSource::plugin("test");
    source
        .fields
        .insert("kind".to_owned(), json!("forged-source"));
    assert_eq!(
        serde_json::to_value(source).unwrap(),
        json!({"kind": "plugin", "plugin": "test"})
    );
}
