//! Native replay-state projection and reconstruction parity tests.

use seekdeep_llm::{
    CallId, ContentBlock, Message, MessageRole, MessageSource, ModelId, ProviderId,
};
use seekdeep_llm_pi_ai::replay::{
    PiApi, PiAssistantBlock, PiAssistantMessage, PiAssistantRole, PiCost, PiResponseId,
    PiStopReason, PiUsage, to_pi_assistant, to_pi_replay_state,
};
use serde_json::{Map, Value, json};

fn usage() -> PiUsage {
    PiUsage {
        input: 0,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        total_tokens: 0,
        cost: PiCost::default(),
    }
}

fn native(content: Vec<PiAssistantBlock>) -> PiAssistantMessage {
    PiAssistantMessage {
        role: PiAssistantRole::Assistant,
        content,
        api: PiApi::new("openai-responses"),
        provider: ProviderId::new("openai"),
        model: ModelId::new("gpt-5"),
        response_model: Some(ModelId::new("gpt-5-2026-01-01")),
        response_id: Some(PiResponseId::new("resp_123")),
        usage: usage(),
        stop_reason: PiStopReason::ToolUse,
        error_message: None,
        timestamp: 42,
    }
}

fn assistant(content: Vec<ContentBlock>, source: MessageSource) -> Message {
    Message::new(MessageRole::Assistant, content, source)
}

fn replay_source(provider: &str, model: &str, replay_state: Value) -> MessageSource {
    let mut source = MessageSource::model(provider, model);
    source.fields.insert("replayState".to_owned(), replay_state);
    source
}

fn valid_replay() -> Value {
    json!({
        "kind": "pi-ai",
        "version": 1,
        "api": "openai-completions",
        "provider": "deepseek",
        "model": "deepseek-v4-flash",
        "stopReason": "stop",
        "blocks": [{"type": "text"}],
    })
}

#[test]
fn projects_only_native_metadata_with_exact_wire_shape() {
    let message = native(vec![
        PiAssistantBlock::Thinking {
            thinking: "private".to_owned(),
            thinking_signature: Some("think-sig".to_owned()),
            redacted: Some(true),
        },
        PiAssistantBlock::Text {
            text: "calling".to_owned(),
            text_signature: Some("text-sig".to_owned()),
        },
        PiAssistantBlock::ToolCall {
            id: CallId::new("c1"),
            name: "f".to_owned(),
            arguments: Map::from_iter([("a".to_owned(), json!(1))]),
            thought_signature: Some("tool-sig".to_owned()),
        },
    ]);
    assert_eq!(
        serde_json::to_value(to_pi_replay_state(&message)).unwrap(),
        json!({
            "kind": "pi-ai",
            "version": 1,
            "api": "openai-responses",
            "provider": "openai",
            "model": "gpt-5",
            "responseModel": "gpt-5-2026-01-01",
            "responseId": "resp_123",
            "stopReason": "toolUse",
            "blocks": [
                {"type": "reasoning", "thinkingSignature": "think-sig", "redacted": true},
                {"type": "text", "textSignature": "text-sig"},
                {"type": "tool-call", "thoughtSignature": "tool-sig"},
            ],
        })
    );
}

#[test]
fn foreign_history_preserves_source_skips_extensions_and_tolerates_bad_arguments() {
    let message = assistant(
        vec![
            ContentBlock::Unknown {
                block_type: "chart".to_owned(),
                fields: Map::new(),
            },
            ContentBlock::Reasoning {
                text: "hmm".to_owned(),
            },
            ContentBlock::Text {
                text: "calling".to_owned(),
            },
            ContentBlock::ToolCall {
                id: CallId::new("c1"),
                name: "f".to_owned(),
                arguments: "[1,2]".to_owned(),
            },
        ],
        MessageSource::model("deepseek", "old-model"),
    );
    let converted = to_pi_assistant(&message).unwrap();
    assert_eq!(converted.api.as_str(), "seekdeep-foreign");
    assert_eq!(converted.provider.as_str(), "deepseek");
    assert_eq!(converted.model.as_str(), "old-model");
    assert_eq!(converted.stop_reason, PiStopReason::ToolUse);
    assert_eq!(converted.timestamp, 0);
    assert_eq!(converted.usage, PiUsage::default());
    assert!(matches!(
        &converted.content[2],
        PiAssistantBlock::ToolCall { arguments, .. } if arguments.is_empty()
    ));
}

#[test]
fn foreign_history_rejects_structured_assistant_images() {
    let image: ContentBlock = serde_json::from_value(json!({
        "type": "image",
        "attachment": {
            "attachmentId": format!("sha256:{}", "a".repeat(64)),
            "mediaType": "image/png",
            "bytes": 1,
            "width": 1,
            "height": 1
        }
    }))
    .unwrap();
    let error =
        to_pi_assistant(&assistant(vec![image], MessageSource::plugin("test"))).unwrap_err();
    assert_eq!(error.code(), "UNSUPPORTED_CONTENT");
    assert_eq!(
        error.message(),
        "pi-ai chat history cannot represent structured assistant image output"
    );
}

#[test]
fn recombines_durable_content_with_validated_native_metadata() {
    let native = native(vec![
        PiAssistantBlock::Thinking {
            thinking: "ignored".to_owned(),
            thinking_signature: Some("think-sig".to_owned()),
            redacted: Some(true),
        },
        PiAssistantBlock::Text {
            text: "ignored".to_owned(),
            text_signature: Some("text-sig".to_owned()),
        },
        PiAssistantBlock::ToolCall {
            id: CallId::new("ignored"),
            name: "ignored".to_owned(),
            arguments: Map::new(),
            thought_signature: Some("tool-sig".to_owned()),
        },
    ]);
    let replay = serde_json::to_value(to_pi_replay_state(&native)).unwrap();
    let durable = assistant(
        vec![
            ContentBlock::Reasoning {
                text: "private".to_owned(),
            },
            ContentBlock::Text {
                text: "calling".to_owned(),
            },
            ContentBlock::ToolCall {
                id: CallId::new("c1"),
                name: "f".to_owned(),
                arguments: "{\"a\":1}".to_owned(),
            },
        ],
        replay_source("openai", "gpt-5", replay),
    );
    let converted = to_pi_assistant(&durable).unwrap();
    assert_eq!(converted.api.as_str(), "openai-responses");
    assert_eq!(converted.response_id.unwrap().as_str(), "resp_123");
    assert_eq!(
        serde_json::to_value(converted.content).unwrap(),
        json!([
            {"type":"thinking","thinking":"private","thinkingSignature":"think-sig","redacted":true},
            {"type":"text","text":"calling","textSignature":"text-sig"},
            {"type":"toolCall","id":"c1","name":"f","arguments":{"a":1},"thoughtSignature":"tool-sig"}
        ])
    );
}

#[test]
fn rejects_source_and_durable_content_mismatches() {
    for (replay, expected) in [
        (
            json!({"kind":"pi-ai","version":1,"api":"a","provider":"other","model":"m","stopReason":"stop","blocks":[{"type":"text"}]}),
            "provider does not match assistant source",
        ),
        (
            json!({"kind":"pi-ai","version":1,"api":"a","provider":"p","model":"other","stopReason":"stop","blocks":[{"type":"text"}]}),
            "model does not match assistant source",
        ),
        (
            json!({"kind":"pi-ai","version":1,"api":"a","provider":"p","model":"m","stopReason":"stop","blocks":[]}),
            "block count does not match assistant content",
        ),
        (
            json!({"kind":"pi-ai","version":1,"api":"a","provider":"p","model":"m","stopReason":"stop","blocks":[{"type":"reasoning"}]}),
            "block 0 does not match assistant content",
        ),
    ] {
        let message = assistant(
            vec![ContentBlock::Text {
                text: "done".to_owned(),
            }],
            replay_source("p", "m", replay),
        );
        let error = to_pi_assistant(&message).unwrap_err();
        assert_eq!(error.code(), "INVALID_REPLAY_STATE");
        assert!(error.message().contains(expected), "{}", error.message());
    }
}

#[test]
fn rejects_every_malformed_replay_shape_with_stable_diagnostics() {
    let valid = valid_replay();
    let cases = [
        (json!(1), "expected an object"),
        (Value::Null, "expected an object"),
        (json!([]), "expected an object"),
        (merge(&valid, "kind", json!("other")), "unknown state kind"),
        (merge(&valid, "version", json!(2)), "unsupported version 2"),
        (without(&valid, "version"), "unsupported version undefined"),
        (
            merge(&valid, "api", json!(1)),
            "api must be a non-empty string",
        ),
        (
            merge(&valid, "provider", json!("")),
            "provider must be a non-empty string",
        ),
        (without(&valid, "model"), "model must be a non-empty string"),
        (
            merge(&valid, "stopReason", json!("pause")),
            "unknown stopReason",
        ),
        (
            merge(&valid, "responseModel", json!(1)),
            "responseModel must be a string",
        ),
        (
            merge(&valid, "responseId", json!(1)),
            "responseId must be a string",
        ),
        (
            merge(&valid, "blocks", json!("text")),
            "blocks must be an array",
        ),
        (
            merge(&valid, "blocks", json!([1])),
            "block 0 must be an object",
        ),
        (
            merge(&valid, "blocks", json!([null])),
            "block 0 must be an object",
        ),
        (
            merge(&valid, "blocks", json!([[]])),
            "block 0 must be an object",
        ),
        (
            merge(&valid, "blocks", json!([{"type":"audio"}])),
            "block 0 has an unknown type",
        ),
        (
            merge(&valid, "blocks", json!([{"type":"text","textSignature":1}])),
            "block 0 textSignature must be a string",
        ),
        (
            merge(
                &valid,
                "blocks",
                json!([{"type":"reasoning","redacted":"yes"}]),
            ),
            "block 0 redacted must be boolean",
        ),
    ];
    for (replay, expected) in cases {
        let message = assistant(
            vec![ContentBlock::Text {
                text: "done".to_owned(),
            }],
            replay_source("deepseek", "deepseek-v4-flash", replay),
        );
        let error = to_pi_assistant(&message).unwrap_err();
        assert_eq!(error.code(), "INVALID_REPLAY_STATE");
        assert!(error.message().contains(expected), "{}", error.message());
    }
}

fn merge(value: &Value, key: &str, replacement: Value) -> Value {
    let mut object = value.as_object().unwrap().clone();
    object.insert(key.to_owned(), replacement);
    Value::Object(object)
}

fn without(value: &Value, key: &str) -> Value {
    let mut object = value.as_object().unwrap().clone();
    object.remove(key);
    Value::Object(object)
}
