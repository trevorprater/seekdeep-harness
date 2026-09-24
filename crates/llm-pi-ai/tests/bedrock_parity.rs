//! Bedrock Converse Stream event translation parity tests.

use futures::{TryStreamExt, stream};
use seekdeep_llm::StreamChunk;
use seekdeep_llm_pi_ai::{
    bedrock::{BedrockEvent, translate_bedrock_events},
    catalog::builtin_catalog,
    stream::to_stream_chunks,
};
use serde_json::json;

fn model() -> seekdeep_llm_pi_ai::catalog::PiModel {
    builtin_catalog().provider("amazon-bedrock").unwrap().models[0].clone()
}

async fn chunks(events: Vec<BedrockEvent>) -> Vec<StreamChunk> {
    let model = model();
    to_stream_chunks(
        translate_bedrock_events(
            stream::iter(events.into_iter().map(Ok::<_, anyhow::Error>)),
            &model,
        ),
        Some(model.context_window),
    )
    .try_collect()
    .await
    .unwrap()
}

#[tokio::test]
async fn translates_text_usage_and_natural_stop() {
    let chunks = chunks(vec![
        BedrockEvent::MessageStart,
        BedrockEvent::Text {
            wire_index: 0,
            text: "hello".to_owned(),
        },
        BedrockEvent::BlockStop { wire_index: 0 },
        BedrockEvent::MessageStop {
            reason: "end_turn".to_owned(),
        },
        BedrockEvent::Metadata {
            input: 3,
            output: 1,
            cache_read: 2,
            cache_write: 4,
            total: 10,
        },
    ])
    .await;
    assert_eq!(
        serde_json::to_value(chunks).unwrap(),
        json!([
            {"type":"block-start","index":0,"blockType":"text"},
            {"type":"text-delta","index":0,"text":"hello"},
            {"type":"block-end","index":0,"block":{"type":"text","text":"hello"}},
            {"type":"usage","usage":{"inputTokens":3,"outputTokens":1,"cacheReadTokens":2,"cacheWriteTokens":4}},
            {"type":"finish","reason":{"kind":"stop"},"replayState":{"kind":"pi-ai","version":1,"api":"bedrock-converse-stream","provider":"amazon-bedrock","model":model().id,"stopReason":"stop","blocks":[{"type":"text"}]}}
        ])
    );
}

#[tokio::test]
async fn translates_reasoning_signature_and_fragmented_tool_use() {
    let chunks = chunks(vec![
        BedrockEvent::MessageStart,
        BedrockEvent::ReasoningText {
            wire_index: 0,
            text: "think".to_owned(),
        },
        BedrockEvent::ReasoningSignature {
            wire_index: 0,
            signature: "sig".to_owned(),
        },
        BedrockEvent::BlockStop { wire_index: 0 },
        BedrockEvent::ToolStart {
            wire_index: 1,
            id: "call_1".to_owned(),
            name: "lookup".to_owned(),
        },
        BedrockEvent::ToolInput {
            wire_index: 1,
            input: "{\"a\"".to_owned(),
        },
        BedrockEvent::ToolInput {
            wire_index: 1,
            input: ":1}".to_owned(),
        },
        BedrockEvent::BlockStop { wire_index: 1 },
        BedrockEvent::MessageStop {
            reason: "tool_use".to_owned(),
        },
    ])
    .await;
    assert!(chunks.iter().any(|chunk| matches!(
        chunk,
        StreamChunk::BlockEnd {
            block: seekdeep_llm::ContentBlock::ToolCall { id, name, arguments },
            ..
        } if id.as_str() == "call_1" && name == "lookup" && arguments == "{\"a\":1}"
    )));
    let finish = serde_json::to_value(chunks.last().unwrap()).unwrap();
    assert_eq!(finish["reason"]["kind"], json!("tool-calls"));
    assert_eq!(
        finish["replayState"]["blocks"][0]["thinkingSignature"],
        json!("sig")
    );
}

#[tokio::test]
async fn context_overflow_stop_reason_is_normalized() {
    let chunks = chunks(vec![
        BedrockEvent::MessageStart,
        BedrockEvent::MessageStop {
            reason: "model_context_window_exceeded".to_owned(),
        },
    ])
    .await;
    let finish = serde_json::to_value(chunks.last().unwrap()).unwrap();
    assert_eq!(
        finish["reason"]["failure"]["code"],
        json!("CONTEXT_WINDOW_EXCEEDED")
    );
}
