//! Incremental canonical stream-to-message assembly.

use std::collections::HashMap;

use serde_json::Value;

use crate::{
    brand::CallId,
    message::{Message, MessageRole, MessageSource},
    types::{ContentBlock, FinishReason, StreamChunk, TokenUsage},
};

#[derive(Clone, Debug)]
struct PartialBlock {
    block_type: String,
    text: String,
    tool_call_id: Option<CallId>,
    tool_call_name: Option<String>,
    tool_call_arguments: String,
    block: Option<ContentBlock>,
}

impl PartialBlock {
    fn new(block_type: impl Into<String>) -> Self {
        Self {
            block_type: block_type.into(),
            text: String::new(),
            tool_call_id: None,
            tool_call_name: None,
            tool_call_arguments: String::new(),
            block: None,
        }
    }
}

/// Incrementally assembles raw chunks into complete blocks and a message.
#[derive(Clone, Debug, Default)]
pub struct BlockAssembler {
    partials: HashMap<u64, PartialBlock>,
    order: Vec<u64>,
    usage: Option<TokenUsage>,
    finish: Option<FinishReason>,
    replay_state: Option<Value>,
}

impl BlockAssembler {
    /// Creates empty assembly state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Accepts one raw chunk in stream order.
    pub fn push(&mut self, chunk: StreamChunk) {
        match chunk {
            StreamChunk::BlockStart { index, block_type } => {
                if !self.partials.contains_key(&index) {
                    self.order.push(index);
                    self.partials.insert(index, PartialBlock::new(block_type));
                }
            }
            StreamChunk::TextDelta { index, text } => {
                let partial = self.ensure(index, "text");
                if partial.block.is_none() {
                    partial.text.push_str(&text);
                }
            }
            StreamChunk::ReasoningDelta { index, text } => {
                let partial = self.ensure(index, "reasoning");
                if partial.block.is_none() {
                    partial.text.push_str(&text);
                }
            }
            StreamChunk::ToolCallDelta {
                index,
                id,
                name,
                arguments_delta,
            } => {
                let partial = self.ensure(index, "tool-call");
                if partial.block.is_none() {
                    partial.tool_call_id = Some(id);
                    if name.is_some() {
                        partial.tool_call_name = name;
                    }
                    partial.tool_call_arguments.push_str(&arguments_delta);
                }
            }
            StreamChunk::BlockEnd { index, block } => {
                let block_type = block.block_type().to_owned();
                let partial = self.ensure(index, block_type);
                if partial.block.is_none() {
                    partial.block = Some(block);
                }
            }
            StreamChunk::Usage { usage } => self.usage = Some(usage),
            StreamChunk::Finish {
                reason,
                replay_state,
            } => {
                self.finish = Some(reason);
                self.replay_state = replay_state;
            }
        }
    }

    fn ensure(&mut self, index: u64, block_type: impl Into<String>) -> &mut PartialBlock {
        if !self.partials.contains_key(&index) {
            self.order.push(index);
            self.partials.insert(index, PartialBlock::new(block_type));
        }
        self.partials
            .get_mut(&index)
            .expect("an ensured block index is present")
    }

    /// Assembles blocks in first-seen index order.
    ///
    /// Tool calls are omitted after a max-token finish because truncated calls cannot execute safely.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown incomplete block type.
    pub fn blocks(&self) -> anyhow::Result<Vec<ContentBlock>> {
        let mut blocks = Vec::with_capacity(self.order.len());
        for index in &self.order {
            let partial = self.partials.get(index).ok_or_else(|| {
                anyhow::anyhow!("BlockAssembler invariant violated: no partial for index {index}")
            })?;
            blocks.push(assemble(partial, *index)?);
        }
        if self.finish() == FinishReason::MaxTokens {
            blocks.retain(|block| !matches!(block, ContentBlock::ToolCall { .. }));
        }
        Ok(blocks)
    }

    /// Latest token accounting chunk.
    #[must_use]
    pub fn usage(&self) -> Option<&TokenUsage> {
        self.usage.as_ref()
    }

    /// Terminal finish, defaulting to stop when the stream ended without one.
    #[must_use]
    pub fn finish(&self) -> FinishReason {
        self.finish.clone().unwrap_or(FinishReason::Stop)
    }

    /// Adapter-private replay state from the terminal finish.
    #[must_use]
    pub fn replay_state(&self) -> Option<&Value> {
        self.replay_state.as_ref()
    }

    /// Builds an assistant-role message over the current blocks.
    ///
    /// # Errors
    ///
    /// Returns the same unknown-incomplete-block error as [`Self::blocks`].
    pub fn message(&self, source: Option<MessageSource>) -> anyhow::Result<Message> {
        Ok(Message::new(
            MessageRole::Assistant,
            self.blocks()?,
            source.unwrap_or_else(|| MessageSource::plugin("seekdeep-llm/assembler")),
        ))
    }
}

fn assemble(partial: &PartialBlock, index: u64) -> anyhow::Result<ContentBlock> {
    if let Some(block) = &partial.block {
        return Ok(block.clone());
    }
    match partial.block_type.as_str() {
        "text" => Ok(ContentBlock::Text {
            text: partial.text.clone(),
        }),
        "reasoning" => Ok(ContentBlock::Reasoning {
            text: partial.text.clone(),
        }),
        "tool-call" => Ok(ContentBlock::ToolCall {
            id: partial
                .tool_call_id
                .clone()
                .unwrap_or_else(|| CallId::new(format!("call-{index}"))),
            name: partial.tool_call_name.clone().unwrap_or_default(),
            arguments: partial.tool_call_arguments.clone(),
        }),
        block_type => Err(anyhow::anyhow!(
            "cannot assemble incomplete block of type \"{block_type}\""
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assembles_interleaved_deltas_in_first_seen_order() {
        let mut assembler = BlockAssembler::new();
        for chunk in [
            StreamChunk::BlockStart {
                index: 0,
                block_type: "reasoning".to_owned(),
            },
            StreamChunk::ReasoningDelta {
                index: 0,
                text: "thinking…".to_owned(),
            },
            StreamChunk::TextDelta {
                index: 1,
                text: "Hello".to_owned(),
            },
            StreamChunk::TextDelta {
                index: 1,
                text: " world".to_owned(),
            },
            StreamChunk::ToolCallDelta {
                index: 2,
                id: CallId::new("call-1"),
                name: Some("echo".to_owned()),
                arguments_delta: "{\"text\":".to_owned(),
            },
            StreamChunk::ToolCallDelta {
                index: 2,
                id: CallId::new("call-1"),
                name: None,
                arguments_delta: "\"hi\"}".to_owned(),
            },
        ] {
            assembler.push(chunk);
        }
        assert_eq!(
            assembler.blocks().expect("known blocks"),
            [
                ContentBlock::Reasoning {
                    text: "thinking…".to_owned()
                },
                ContentBlock::Text {
                    text: "Hello world".to_owned()
                },
                ContentBlock::ToolCall {
                    id: CallId::new("call-1"),
                    name: "echo".to_owned(),
                    arguments: "{\"text\":\"hi\"}".to_owned()
                }
            ]
        );
    }

    #[test]
    fn first_close_wins_and_stragglers_are_ignored() {
        let mut assembler = BlockAssembler::new();
        assembler.push(StreamChunk::BlockEnd {
            index: 0,
            block: ContentBlock::Reasoning {
                text: "first".to_owned(),
            },
        });
        assembler.push(StreamChunk::BlockEnd {
            index: 0,
            block: ContentBlock::Text {
                text: "second".to_owned(),
            },
        });
        assembler.push(StreamChunk::ReasoningDelta {
            index: 0,
            text: "straggler".to_owned(),
        });
        assert_eq!(
            assembler.blocks().expect("closed block"),
            [ContentBlock::Reasoning {
                text: "first".to_owned()
            }]
        );
    }

    #[test]
    fn max_tokens_drops_incomplete_tool_calls() {
        let mut assembler = BlockAssembler::new();
        assembler.push(StreamChunk::BlockStart {
            index: 0,
            block_type: "tool-call".to_owned(),
        });
        assembler.push(StreamChunk::Finish {
            reason: FinishReason::MaxTokens,
            replay_state: None,
        });
        assert!(assembler.blocks().expect("known block").is_empty());
    }
}
