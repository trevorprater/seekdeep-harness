use std::sync::Arc;

use async_trait::async_trait;
use futures::TryStreamExt as _;
use seekdeep_attachment::{
    AttachmentBackend, AttachmentId, AttachmentStore, ImageAttachmentLimits, ImageAttachmentRef,
    ImageMediaType, SaveImageAttachment, StoredImageAttachment,
};
use seekdeep_llm::{
    AbortSignal, CallId, ContentBlock, JsonString, Message, MessageRole, MessageSource, StreamChunk,
};
use seekdeep_llm_pi_ai::{
    context::{PiContext, to_pi_context, to_pi_context_with_images},
    replay::{
        PiAssistantBlock, PiAssistantMessage, PiStopReason, to_pi_assistant, to_pi_replay_state,
    },
    stream::{PiAssistantEvent, to_stream_chunks},
};
use seekdeep_lossless_json::JsonValue;
use serde_json::json;

use super::support::{samples, source_call, tool_request};

#[test]
fn exact_tool_context_and_raw_enum_roundtrip_match_the_source() {
    for text in samples() {
        let request = tool_request("openai", "gpt-4.1", text.clone());
        let context = to_pi_context(&request).unwrap();
        let raw = JsonValue::from_serialize(&context).unwrap();
        let expected = source_call(&JsonValue::object([
            ("op", json!("context").into()),
            (
                "messages",
                JsonValue::from_serialize(&request.messages).unwrap(),
            ),
        ]));
        assert_eq!(raw, expected);
        assert_eq!(raw.deserialize::<PiContext>().unwrap(), context);
        let expected_text = if text.is_empty() {
            "(no output)".into()
        } else {
            text
        };
        assert_eq!(
            raw.pointer("/messages/0/content/0/text")
                .unwrap()
                .to_utf16()
                .unwrap(),
            expected_text.utf16_units()
        );
    }
}

#[test]
fn concatenation_replay_and_message_roles_keep_code_units_until_provider_conversion() {
    let mut request = tool_request("openai", "gpt-4.1", JsonString::default());
    request.messages = vec![
        Message::new(
            MessageRole::User,
            vec![
                ContentBlock::text_utf16(&[0xd800]),
                ContentBlock::text_utf16(&[0xdc00]),
            ],
            MessageSource::user(),
        ),
        Message::new(
            MessageRole::Assistant,
            vec![
                ContentBlock::text_utf16(&[0xd800]),
                ContentBlock::text("visible"),
            ],
            MessageSource::model("openai", "gpt-4.1"),
        ),
        Message::tool_result(
            &CallId::new("nested"),
            vec![
                ContentBlock::text_utf16(&[0xd800]),
                ContentBlock::ToolResult {
                    tool_call_id: CallId::new("inner"),
                    content: vec![ContentBlock::text_utf16(&[0xdc00, 0xdfff])],
                    is_error: None,
                },
            ],
            false,
        ),
    ];
    let context = to_pi_context(&request).unwrap();
    let raw = JsonValue::from_serialize(&context).unwrap();
    let expected = source_call(&JsonValue::object([
        ("op", json!("context").into()),
        (
            "messages",
            JsonValue::from_serialize(&request.messages).unwrap(),
        ),
    ]));
    assert_eq!(context, expected.deserialize::<PiContext>().unwrap());
    assert_eq!(raw.deserialize::<PiContext>().unwrap(), context);
    assert_eq!(
        raw.pointer("/messages/0/content")
            .unwrap()
            .to_utf16()
            .unwrap(),
        [0xd800, 0xdc00]
    );
    assert_eq!(
        raw.pointer("/messages/2/content/0/text")
            .unwrap()
            .to_utf16()
            .unwrap(),
        [0xd800, 0xdc00, 0xdfff]
    );

    let native = to_pi_assistant(&request.messages[1]).unwrap();
    let mut source = MessageSource::model("openai", "gpt-4.1");
    source.fields.insert(
        "replayState".to_owned(),
        serde_json::to_value(to_pi_replay_state(&native)).unwrap(),
    );
    let durable = Message::new(
        MessageRole::Assistant,
        request.messages[1].content().to_vec(),
        source,
    );
    assert_eq!(to_pi_assistant(&durable).unwrap(), native);
}

struct ImageBackend(ImageAttachmentLimits);

#[async_trait]
impl AttachmentBackend for ImageBackend {
    fn image_limits(&self) -> &ImageAttachmentLimits {
        &self.0
    }
    async fn validate_image(&self, _: &SaveImageAttachment) -> anyhow::Result<()> {
        Ok(())
    }
    async fn save_image(&self, _: SaveImageAttachment) -> anyhow::Result<ImageAttachmentRef> {
        unreachable!("read-only fixture")
    }
    async fn read_image(
        &self,
        reference: &ImageAttachmentRef,
        _: Option<AbortSignal>,
    ) -> anyhow::Result<StoredImageAttachment> {
        Ok(StoredImageAttachment {
            reference: reference.clone(),
            data: vec![1],
        })
    }
}

#[tokio::test]
async fn nested_text_compacts_before_it_joins_parent_image_blocks() {
    let attachments = AttachmentStore::new(Arc::new(ImageBackend(ImageAttachmentLimits {
        max_image_bytes: 10,
        max_images_per_message: 10,
        max_message_image_bytes: 100,
        max_image_pixels: 100,
        media_types: vec![ImageMediaType::Png],
    })));
    let image = ContentBlock::Image {
        attachment: ImageAttachmentRef {
            attachment_id: AttachmentId::new(format!("sha256:{}", "a".repeat(64))),
            media_type: ImageMediaType::Png,
            bytes: 1,
            width: 1,
            height: 1,
            name: None,
        },
    };
    let nested = ContentBlock::ToolResult {
        tool_call_id: CallId::new("nested"),
        content: vec![
            ContentBlock::text_utf16(&[0xd800]),
            ContentBlock::text_utf16(&[0xdc00]),
        ],
        is_error: None,
    };
    let mut request = tool_request("openai", "gpt-4.1", JsonString::default());
    request.messages = vec![Message::tool_result(
        &CallId::new("outer"),
        vec![nested, image],
        false,
    )];
    let context = to_pi_context_with_images(&request, &attachments)
        .await
        .unwrap();
    let actual = JsonValue::from_serialize(&context).unwrap();
    let expected = source_call(&JsonValue::object([
        ("op", json!("context").into()),
        ("images", json!(true).into()),
        (
            "messages",
            JsonValue::from_serialize(&request.messages).unwrap(),
        ),
    ]));
    assert_eq!(actual, expected);
    assert_eq!(actual.deserialize::<PiContext>().unwrap(), context);
    assert_eq!(
        actual
            .pointer("/messages/0/content")
            .unwrap()
            .array_items()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        actual
            .pointer("/messages/0/content/0/text")
            .unwrap()
            .to_utf16()
            .unwrap(),
        [0xd800, 0xdc00]
    );
}

fn assistant() -> PiAssistantMessage {
    to_pi_assistant(&Message::new(
        MessageRole::Assistant,
        vec![ContentBlock::text_utf16(&[0xd800])],
        MessageSource::model("openai", "gpt-4.1"),
    ))
    .unwrap()
}

#[tokio::test]
async fn every_native_event_keeps_raw_partial_text_and_completed_harness_blocks() {
    let partial = assistant();
    for (kind, fields, message_field) in [
        ("start", json!({}), "partial"),
        ("text_start", json!({"contentIndex":0}), "partial"),
        (
            "text_delta",
            json!({"contentIndex":0,"delta":"ordinary"}),
            "partial",
        ),
        ("text_end", json!({"contentIndex":0}), "partial"),
        ("thinking_start", json!({"contentIndex":1}), "partial"),
        (
            "thinking_delta",
            json!({"contentIndex":1,"delta":"reasoning"}),
            "partial",
        ),
        (
            "thinking_end",
            json!({"contentIndex":1,"content":"reasoning"}),
            "partial",
        ),
        ("toolcall_start", json!({"contentIndex":2}), "partial"),
        (
            "toolcall_delta",
            json!({"contentIndex":2,"delta":"{}"}),
            "partial",
        ),
        (
            "toolcall_end",
            json!({"contentIndex":2,"toolCall":{"id":"call","name":"run_code","arguments":{}}}),
            "partial",
        ),
        ("done", json!({"reason":"stop"}), "message"),
        ("error", json!({"reason":"error"}), "error"),
    ] {
        let mut raw = JsonValue::from(fields);
        if kind == "text_end" {
            raw.insert("content", JsonString::from_utf16(&[0xd800]).into())
                .unwrap();
        }
        let mut message = partial.clone();
        if kind == "error" {
            message.stop_reason = PiStopReason::Error;
            message.error_message = Some("fixture failure".into());
        }
        raw.insert(message_field, JsonValue::from_serialize(&message).unwrap())
            .unwrap();
        raw.insert("type", json!(kind).into()).unwrap();
        let event = raw.deserialize::<PiAssistantEvent>().unwrap();
        assert_eq!(JsonValue::from_serialize(&event).unwrap(), raw);
    }
    let stream = futures::stream::iter(vec![
        Ok(PiAssistantEvent::TextEnd {
            content_index: 0,
            content: JsonString::from_utf16(&[0xd800]),
            partial: partial.clone(),
        }),
        Ok(PiAssistantEvent::Done {
            reason: PiStopReason::Stop,
            message: partial,
        }),
    ]);
    let chunks = to_stream_chunks(stream, None)
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
    assert!(
        matches!(&chunks[0], StreamChunk::BlockEnd { block: ContentBlock::Text { text }, .. } if text.utf16_units() == [0xd800])
    );
    let content = assistant().content;
    assert!(matches!(&content[0], PiAssistantBlock::Text { text, .. } if text.as_str().is_none()));
}
