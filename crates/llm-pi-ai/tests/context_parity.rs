//! Native request-context conversion parity tests.

use std::sync::Arc;

use async_trait::async_trait;
use seekdeep_attachment::{
    AttachmentBackend, AttachmentId, ImageAttachmentLimits, ImageAttachmentRef, ImageMediaType,
    SaveImageAttachment, StoredImageAttachment,
};
use seekdeep_llm::{
    AbortSignal, CallId, ContentBlock, GenerateOptions, LlmError, Message, MessageRole,
    MessageSource, ModelId, ProviderId, ToolSchema,
};
use seekdeep_llm_pi_ai::context::{to_pi_context, to_pi_context_with_images};
use serde_json::{Map, json};

struct BytesBackend {
    limits: ImageAttachmentLimits,
}

#[async_trait]
impl AttachmentBackend for BytesBackend {
    fn image_limits(&self) -> &ImageAttachmentLimits {
        &self.limits
    }

    async fn validate_image(&self, _input: &SaveImageAttachment) -> anyhow::Result<()> {
        Ok(())
    }

    async fn save_image(&self, _input: SaveImageAttachment) -> anyhow::Result<ImageAttachmentRef> {
        unreachable!("context conversion only reads images")
    }

    async fn read_image(
        &self,
        reference: &ImageAttachmentRef,
        _signal: Option<AbortSignal>,
    ) -> anyhow::Result<StoredImageAttachment> {
        Ok(StoredImageAttachment {
            reference: reference.clone(),
            data: vec![1],
        })
    }
}

fn store() -> seekdeep_attachment::AttachmentStore {
    seekdeep_attachment::AttachmentStore::new(Arc::new(BytesBackend {
        limits: ImageAttachmentLimits {
            max_image_bytes: 10,
            max_images_per_message: 10,
            max_message_image_bytes: 100,
            max_image_pixels: 100,
            media_types: vec![ImageMediaType::Png],
        },
    }))
}

fn image() -> ContentBlock {
    ContentBlock::Image {
        attachment: ImageAttachmentRef {
            attachment_id: AttachmentId::new(format!("sha256:{}", "a".repeat(64))),
            media_type: ImageMediaType::Png,
            bytes: 1,
            width: 1,
            height: 1,
            name: None,
        },
    }
}

fn user(content: Vec<ContentBlock>) -> Message {
    Message::new(MessageRole::User, content, MessageSource::plugin("test"))
}

fn history(role: MessageRole, content: Vec<ContentBlock>) -> Message {
    Message::new(role, content, MessageSource::plugin("test"))
}

fn request(messages: Vec<Message>) -> GenerateOptions {
    let mut options =
        GenerateOptions::new(ProviderId::new("openai"), ModelId::new("gpt-4.1"), messages);
    options.system = Some("system prompt".to_owned());
    options.tools = Some(vec![ToolSchema {
        name: "lookup".to_owned(),
        description: "look up".to_owned(),
        parameters: Map::from_iter([("type".to_owned(), json!("object"))]),
    }]);
    options
}

#[test]
fn omits_absent_and_empty_request_level_fields() {
    let base = GenerateOptions::new(ProviderId::new("openai"), ModelId::new("gpt-4.1"), vec![]);
    assert_eq!(
        serde_json::to_value(to_pi_context(&base).unwrap()).unwrap(),
        json!({"messages": []})
    );
    let mut empty_tools = base;
    empty_tools.tools = Some(vec![]);
    assert_eq!(
        serde_json::to_value(to_pi_context(&empty_tools).unwrap()).unwrap(),
        json!({"messages": []})
    );
}

#[test]
fn converts_complete_text_history_and_recovers_tool_names() {
    let call_id = CallId::new("call-1");
    let context = to_pi_context(&request(vec![
        history(
            MessageRole::System,
            vec![ContentBlock::Text {
                text: "history system".to_owned(),
            }],
        ),
        history(
            MessageRole::Assistant,
            vec![ContentBlock::ToolCall {
                id: call_id.clone(),
                name: "lookup".to_owned(),
                arguments: "{}".to_owned(),
            }],
        ),
        user(vec![
            ContentBlock::Text {
                text: "after tool".to_owned(),
            },
            ContentBlock::ToolResult {
                tool_call_id: call_id,
                content: vec![ContentBlock::ToolResult {
                    tool_call_id: CallId::new("nested"),
                    content: vec![ContentBlock::Text {
                        text: String::new(),
                    }],
                    is_error: None,
                }],
                is_error: None,
            },
        ]),
    ]))
    .unwrap();
    assert_eq!(
        serde_json::to_value(context).unwrap(),
        json!({
            "systemPrompt": "system prompt",
            "tools": [{"name":"lookup","description":"look up","parameters":{"type":"object"}}],
            "messages": [
                {"role":"user","content":"history system","timestamp":0},
                {
                    "role":"assistant","content":[{"type":"toolCall","id":"call-1","name":"lookup","arguments":{}}],
                    "api":"seekdeep-foreign","provider":"seekdeep-foreign","model":"seekdeep-foreign",
                    "usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0.0,"output":0.0,"cacheRead":0.0,"cacheWrite":0.0,"total":0.0}},
                    "stopReason":"toolUse","timestamp":0
                },
                {"role":"user","content":"after tool","timestamp":0},
                {"role":"toolResult","toolCallId":"call-1","toolName":"lookup","content":[{"type":"text","text":"(no output)"}],"isError":false,"timestamp":0}
            ]
        })
    );
}

#[test]
fn text_only_path_rejects_images_at_any_nested_depth() {
    let options = request(vec![user(vec![ContentBlock::ToolResult {
        tool_call_id: CallId::new("c"),
        content: vec![image()],
        is_error: None,
    }])]);
    let error = to_pi_context(&options).unwrap_err();
    assert_eq!(error.code(), "UNSUPPORTED_CONTENT");
    assert!(error.message().contains("durable attachment service"));
}

#[tokio::test]
async fn resolves_images_flattens_nested_results_and_preserves_fallbacks() {
    let known = CallId::new("known-call");
    let options = request(vec![
        user(vec![]),
        history(
            MessageRole::Assistant,
            vec![ContentBlock::ToolCall {
                id: known.clone(),
                name: "lookup".to_owned(),
                arguments: "{}".to_owned(),
            }],
        ),
        user(vec![
            image(),
            ContentBlock::Text {
                text: "caption".to_owned(),
            },
            ContentBlock::Reasoning {
                text: "ignored".to_owned(),
            },
        ]),
        user(vec![ContentBlock::ToolResult {
            tool_call_id: known,
            content: vec![],
            is_error: None,
        }]),
        user(vec![ContentBlock::ToolResult {
            tool_call_id: CallId::new("missing-call"),
            content: vec![
                ContentBlock::ToolResult {
                    tool_call_id: CallId::new("nested"),
                    content: vec![ContentBlock::Text {
                        text: "before".to_owned(),
                    }],
                    is_error: None,
                },
                image(),
            ],
            is_error: Some(true),
        }]),
    ]);
    let context = to_pi_context_with_images(&options, &store()).await.unwrap();
    let value = serde_json::to_value(context).unwrap();
    assert_eq!(
        value["messages"][0],
        json!({"role":"user","content":"","timestamp":0})
    );
    assert_eq!(
        value["messages"][2],
        json!({
            "role":"user",
            "content":[{"type":"image","data":"AQ==","mimeType":"image/png"},{"type":"text","text":"caption"}],
            "timestamp":0
        })
    );
    assert_eq!(
        value["messages"][3],
        json!({"role":"toolResult","toolCallId":"known-call","toolName":"lookup","content":[{"type":"text","text":"(no output)"}],"isError":false,"timestamp":0})
    );
    assert_eq!(
        value["messages"][4],
        json!({"role":"toolResult","toolCallId":"missing-call","toolName":"unknown","content":[{"type":"text","text":"before"},{"type":"image","data":"AQ==","mimeType":"image/png"}],"isError":true,"timestamp":0})
    );
}

#[tokio::test]
async fn image_path_rejects_in_history_system_images_and_assistant_images() {
    let system = request(vec![history(MessageRole::System, vec![image()])]);
    let error = to_pi_context_with_images(&system, &store())
        .await
        .unwrap_err();
    let llm = error.downcast_ref::<LlmError>().unwrap();
    assert_eq!(llm.code(), "UNSUPPORTED_CONTENT");
    assert!(llm.message().contains("in-history system message"));

    let assistant = request(vec![history(MessageRole::Assistant, vec![image()])]);
    let error = to_pi_context_with_images(&assistant, &store())
        .await
        .unwrap_err();
    let llm = error.downcast_ref::<LlmError>().unwrap();
    assert_eq!(llm.code(), "UNSUPPORTED_CONTENT");
    assert!(llm.message().contains("assistant image output"));
}
