//! Harness request-history conversion into pi-ai context values.

use std::collections::HashMap;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use seekdeep_attachment::AttachmentStore;
use seekdeep_llm::{
    CallId, ContentBlock, GenerateOptions, LlmError, Message, MessageRole, ToolSchema,
    content_has_image,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::replay::{PiAssistantBlock, PiAssistantMessage, to_pi_assistant};

/// Native pi-ai text or inline image content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PiUserContentBlock {
    /// Text content.
    #[serde(rename = "text")]
    Text {
        /// Exact text.
        text: String,
    },
    /// Base64-encoded image bytes.
    #[serde(rename = "image")]
    Image {
        /// Unpadded or padded standard base64 as emitted by Node `Buffer`.
        data: String,
        /// Verified MIME type.
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
}

/// Native user content is compact text until an image requires block form.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PiUserContent {
    /// Text-only content.
    Text(String),
    /// Mixed text and image content.
    Blocks(Vec<PiUserContentBlock>),
}

impl PiUserContent {
    fn is_empty(&self) -> bool {
        match self {
            Self::Text(text) => text.is_empty(),
            Self::Blocks(blocks) => blocks.is_empty(),
        }
    }
}

/// Closed native user role.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PiUserRole {
    /// Human or folded system input.
    #[serde(rename = "user")]
    User,
}

/// One native pi-ai user message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PiUserMessage {
    /// Role discriminator.
    pub role: PiUserRole,
    /// Text or mixed content.
    pub content: PiUserContent,
    /// Historical timestamp.
    pub timestamp: u64,
}

/// Closed native tool-result role.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PiToolResultRole {
    /// Tool output.
    #[serde(rename = "toolResult")]
    ToolResult,
}

/// One native pi-ai tool-result message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiToolResultMessage {
    /// Role discriminator.
    pub role: PiToolResultRole,
    /// Correlated provider call identity.
    pub tool_call_id: CallId,
    /// Recovered preceding tool name or `unknown`.
    pub tool_name: String,
    /// Tool output blocks.
    pub content: Vec<PiUserContentBlock>,
    /// Whether tool execution failed.
    pub is_error: bool,
    /// Historical timestamp.
    pub timestamp: u64,
}

/// One native pi-ai history item.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PiMessage {
    /// User or folded system input.
    User(PiUserMessage),
    /// Native assistant history.
    Assistant(PiAssistantMessage),
    /// Tool output.
    ToolResult(PiToolResultMessage),
}

/// Native pi-ai tool declaration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PiTool {
    /// Tool name.
    pub name: String,
    /// Model-facing purpose.
    pub description: String,
    /// Structurally compatible JSON Schema.
    pub parameters: Map<String, Value>,
}

/// Native pi-ai request context.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiContext {
    /// Single request-level system prompt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// Ordered history.
    pub messages: Vec<PiMessage>,
    /// Tools, omitted when absent or empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<PiTool>>,
}

/// Converts text-only Harness history synchronously.
///
/// # Errors
///
/// Returns `UNSUPPORTED_CONTENT` if any nested image requires the durable
/// attachment service, or propagates assistant replay validation errors.
pub fn to_pi_context(options: &GenerateOptions) -> Result<PiContext, LlmError> {
    let mut tool_names = HashMap::<CallId, String>::new();
    let mut messages = Vec::new();
    for message in &options.messages {
        if content_has_image(message.content()) {
            return Err(LlmError::simple(
                "pi-ai image conversion requires the durable attachment service",
                "UNSUPPORTED_CONTENT",
            ));
        }
        match message.role() {
            MessageRole::System => messages.push(user_message(flatten_text(message))),
            MessageRole::Assistant => {
                let assistant = to_pi_assistant(message)?;
                remember_tool_names(&assistant, &mut tool_names);
                messages.push(PiMessage::Assistant(assistant));
            }
            MessageRole::User => {
                let text = flatten_text(message);
                let results = top_level_results(message.content());
                if !text.is_empty() || results.is_empty() {
                    messages.push(user_message(text));
                }
                for result in results {
                    messages.push(PiMessage::ToolResult(text_tool_result(result, &tool_names)));
                }
            }
        }
    }
    Ok(context_envelope(options, messages))
}

/// Converts Harness history while resolving durable image references.
///
/// # Errors
///
/// Returns `UNSUPPORTED_CONTENT` for an in-history system image, propagates
/// replay validation errors, and preserves attachment backend failures.
pub async fn to_pi_context_with_images(
    options: &GenerateOptions,
    attachments: &AttachmentStore,
) -> anyhow::Result<PiContext> {
    let mut tool_names = HashMap::<CallId, String>::new();
    let mut messages = Vec::new();
    for message in &options.messages {
        match message.role() {
            MessageRole::System => {
                if content_has_image(message.content()) {
                    return Err(LlmError::simple(
                        "pi-ai cannot represent an image in an in-history system message",
                        "UNSUPPORTED_CONTENT",
                    )
                    .into());
                }
                messages.push(user_message(flatten_text(message)));
            }
            MessageRole::Assistant => {
                let assistant = to_pi_assistant(message)?;
                remember_tool_names(&assistant, &mut tool_names);
                messages.push(PiMessage::Assistant(assistant));
            }
            MessageRole::User => {
                let regular = message
                    .content()
                    .iter()
                    .filter(|block| !matches!(block, ContentBlock::ToolResult { .. }));
                let content = user_content(regular, attachments).await?;
                let results = top_level_results(message.content());
                if !content.is_empty() || results.is_empty() {
                    messages.push(PiMessage::User(PiUserMessage {
                        role: PiUserRole::User,
                        content,
                        timestamp: 0,
                    }));
                }
                for result in results {
                    messages.push(PiMessage::ToolResult(
                        image_tool_result(result, &tool_names, attachments).await?,
                    ));
                }
            }
        }
    }
    Ok(context_envelope(options, messages))
}

fn context_envelope(options: &GenerateOptions, messages: Vec<PiMessage>) -> PiContext {
    let tools = options.tools.as_ref().and_then(|tools| {
        (!tools.is_empty()).then(|| tools.iter().map(tool_of).collect::<Vec<_>>())
    });
    PiContext {
        system_prompt: options.system.clone(),
        messages,
        tools,
    }
}

fn tool_of(tool: &ToolSchema) -> PiTool {
    PiTool {
        name: tool.name.clone(),
        description: tool.description.clone(),
        parameters: tool.parameters.clone(),
    }
}

fn flatten_text(message: &Message) -> String {
    message
        .content()
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn tool_result_text(blocks: &[ContentBlock]) -> String {
    let mut output = String::new();
    for block in blocks {
        match block {
            ContentBlock::Text { text } => output.push_str(text),
            ContentBlock::ToolResult { content, .. } => output.push_str(&tool_result_text(content)),
            ContentBlock::Reasoning { .. }
            | ContentBlock::Image { .. }
            | ContentBlock::ToolCall { .. }
            | ContentBlock::Unknown { .. } => {}
        }
    }
    output
}

fn top_level_results(content: &[ContentBlock]) -> Vec<&ContentBlock> {
    content
        .iter()
        .filter(|block| matches!(block, ContentBlock::ToolResult { .. }))
        .collect()
}

fn user_message(text: String) -> PiMessage {
    PiMessage::User(PiUserMessage {
        role: PiUserRole::User,
        content: PiUserContent::Text(text),
        timestamp: 0,
    })
}

fn remember_tool_names(assistant: &PiAssistantMessage, names: &mut HashMap<CallId, String>) {
    for block in &assistant.content {
        if let PiAssistantBlock::ToolCall { id, name, .. } = block {
            names.insert(id.clone(), name.clone());
        }
    }
}

fn text_tool_result(
    block: &ContentBlock,
    tool_names: &HashMap<CallId, String>,
) -> PiToolResultMessage {
    let ContentBlock::ToolResult {
        tool_call_id,
        content,
        is_error,
    } = block
    else {
        unreachable!("caller filters tool results")
    };
    let text = tool_result_text(content);
    PiToolResultMessage {
        role: PiToolResultRole::ToolResult,
        tool_call_id: tool_call_id.clone(),
        tool_name: tool_names
            .get(tool_call_id)
            .cloned()
            .unwrap_or_else(|| "unknown".to_owned()),
        content: vec![PiUserContentBlock::Text {
            text: if text.is_empty() {
                "(no output)".to_owned()
            } else {
                text
            },
        }],
        is_error: is_error.unwrap_or(false),
        timestamp: 0,
    }
}

async fn image_tool_result(
    block: &ContentBlock,
    tool_names: &HashMap<CallId, String>,
    attachments: &AttachmentStore,
) -> anyhow::Result<PiToolResultMessage> {
    let ContentBlock::ToolResult {
        tool_call_id,
        content,
        is_error,
    } = block
    else {
        unreachable!("caller filters tool results")
    };
    let resolved = user_content(content.iter(), attachments).await?;
    let content = match resolved {
        PiUserContent::Text(text) => vec![PiUserContentBlock::Text {
            text: if text.is_empty() {
                "(no output)".to_owned()
            } else {
                text
            },
        }],
        PiUserContent::Blocks(content) => content,
    };
    Ok(PiToolResultMessage {
        role: PiToolResultRole::ToolResult,
        tool_call_id: tool_call_id.clone(),
        tool_name: tool_names
            .get(tool_call_id)
            .cloned()
            .unwrap_or_else(|| "unknown".to_owned()),
        content,
        is_error: is_error.unwrap_or(false),
        timestamp: 0,
    })
}

async fn user_content<'a>(
    blocks: impl IntoIterator<Item = &'a ContentBlock>,
    attachments: &AttachmentStore,
) -> anyhow::Result<PiUserContent> {
    let mut pending = blocks.into_iter().collect::<Vec<_>>();
    pending.reverse();
    let mut content = Vec::new();
    let mut has_image = false;
    while let Some(block) = pending.pop() {
        match block {
            ContentBlock::Text { text } if !text.is_empty() => {
                content.push(PiUserContentBlock::Text { text: text.clone() });
            }
            ContentBlock::Image { attachment } => {
                let stored = attachments.read_image(attachment, None).await?;
                has_image = true;
                content.push(PiUserContentBlock::Image {
                    data: STANDARD.encode(stored.data),
                    mime_type: stored.reference.media_type.to_string(),
                });
            }
            ContentBlock::ToolResult {
                content: nested, ..
            } => {
                pending.extend(nested.iter().rev());
            }
            ContentBlock::Text { .. }
            | ContentBlock::Reasoning { .. }
            | ContentBlock::ToolCall { .. }
            | ContentBlock::Unknown { .. } => {}
        }
    }
    if has_image {
        Ok(PiUserContent::Blocks(content))
    } else {
        Ok(PiUserContent::Text(
            content
                .into_iter()
                .map(|block| match block {
                    PiUserContentBlock::Text { text } => text,
                    PiUserContentBlock::Image { .. } => {
                        unreachable!("has_image is false")
                    }
                })
                .collect(),
        ))
    }
}
