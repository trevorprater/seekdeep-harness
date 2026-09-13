//! Streaming Assistant block accumulator with block-level immutability.

use std::rc::Rc;

use seekdeep_lossless_json::{JsonString, JsonValue as Value};

/// Client projection of one Assistant content block.
#[derive(Clone, Debug, PartialEq)]
pub enum AssistantBlock {
    /// Visible text.
    Text {
        /// Complete accumulated text.
        text: JsonString,
    },
    /// Visible reasoning.
    Reasoning {
        /// Complete accumulated reasoning text.
        text: JsonString,
    },
    /// Image attachment reference.
    Image {
        /// Durable image attachment reference.
        attachment: Value,
    },
    /// Model Tool call.
    ToolCall {
        /// First non-empty streamed Tool call identity.
        call_id: JsonString,
        /// Latest supplied Tool name.
        name: JsonString,
        /// Concatenated raw argument JSON.
        args_raw: JsonString,
    },
    /// Merge-extensible unknown content block.
    Other {
        /// Original merge-extensible content block.
        block: Value,
    },
}

/// Current partial Assistant projection.
#[derive(Clone, Debug, PartialEq)]
pub struct PartialAssistant {
    /// Owning Turn.
    pub turn: i64,
    /// Owning Step.
    pub step: i64,
    /// Dense render-order blocks.
    pub blocks: Rc<Vec<Rc<AssistantBlock>>>,
}

/// Stream chunk variants relevant to partial projection.
#[derive(Clone, Debug, PartialEq)]
pub enum PartialChunk {
    /// Starts or resets one block lane.
    BlockStart {
        /// Sparse wire block index.
        index: usize,
        /// Wire block type.
        block_type: String,
    },
    /// Appended text delta.
    TextDelta {
        /// Sparse wire block index.
        index: usize,
        /// Appended text.
        text: JsonString,
    },
    /// Appended reasoning delta.
    ReasoningDelta {
        /// Sparse wire block index.
        index: usize,
        /// Appended reasoning text.
        text: JsonString,
    },
    /// Appended Tool call delta.
    ToolCallDelta {
        /// Sparse wire block index.
        index: usize,
        /// Candidate Tool call identity.
        id: JsonString,
        /// Optional late Tool name.
        name: Option<JsonString>,
        /// Appended argument JSON.
        arguments_delta: JsonString,
    },
    /// Final materialized block replacement.
    BlockEnd {
        /// Sparse wire block index.
        index: usize,
        /// Complete core content block.
        block: Value,
    },
    /// Usage, finish, or a future non-visible chunk.
    Other {
        /// Merge-extensible discriminant.
        chunk_type: String,
    },
}

/// Whether one wire chunk discriminant may change visible partial blocks.
#[must_use]
pub fn is_visible_assistant_chunk(chunk_type: &str) -> bool {
    matches!(
        chunk_type,
        "block-start" | "text-delta" | "reasoning-delta" | "tool-call-delta" | "block-end"
    )
}

/// Assistant stream accumulator.
pub struct PartialAccumulator {
    turn: i64,
    step: i64,
    blocks: Vec<Option<Rc<AssistantBlock>>>,
    changed: bool,
    snapshot: Rc<PartialAssistant>,
}

impl PartialAccumulator {
    /// Creates an accumulator from an optional materialized history prefix.
    #[must_use]
    pub fn new(turn: i64, step: i64, initial: Vec<Rc<AssistantBlock>>) -> Self {
        let blocks = Rc::new(initial.clone());
        Self {
            turn,
            step,
            blocks: initial.into_iter().map(Some).collect(),
            changed: true,
            snapshot: Rc::new(PartialAssistant { turn, step, blocks }),
        }
    }

    /// Folds one chunk and reports whether visible projection changed.
    pub fn push(&mut self, chunk: &PartialChunk) -> bool {
        match chunk {
            PartialChunk::BlockStart { index, block_type } => {
                self.set(*index, Rc::new(empty_assistant_block(block_type)));
            }
            PartialChunk::TextDelta { index, text } => {
                let prior = self.get(*index);
                let prefix = match prior.as_deref() {
                    Some(AssistantBlock::Text { text }) => text.clone(),
                    _ => JsonString::default(),
                };
                self.set(
                    *index,
                    Rc::new(AssistantBlock::Text {
                        text: JsonString::concat(&[&prefix, text]),
                    }),
                );
            }
            PartialChunk::ReasoningDelta { index, text } => {
                let prior = self.get(*index);
                let prefix = match prior.as_deref() {
                    Some(AssistantBlock::Reasoning { text }) => text.clone(),
                    _ => JsonString::default(),
                };
                self.set(
                    *index,
                    Rc::new(AssistantBlock::Reasoning {
                        text: JsonString::concat(&[&prefix, text]),
                    }),
                );
            }
            PartialChunk::ToolCallDelta {
                index,
                id,
                name,
                arguments_delta,
            } => {
                let prior = self.get(*index);
                let (call_id, prior_name, args_raw) = match prior.as_deref() {
                    Some(AssistantBlock::ToolCall {
                        call_id,
                        name,
                        args_raw,
                    }) => (call_id.clone(), name.clone(), args_raw.clone()),
                    _ => (
                        JsonString::default(),
                        JsonString::default(),
                        JsonString::default(),
                    ),
                };
                self.set(
                    *index,
                    Rc::new(AssistantBlock::ToolCall {
                        call_id: if call_id.is_empty() {
                            id.clone()
                        } else {
                            call_id
                        },
                        name: name.clone().unwrap_or(prior_name),
                        args_raw: JsonString::concat(&[&args_raw, arguments_delta]),
                    }),
                );
            }
            PartialChunk::BlockEnd { index, block } => {
                self.set(*index, Rc::new(to_assistant_block(block)));
            }
            PartialChunk::Other { .. } => return false,
        }
        true
    }

    /// Reference-stable current partial snapshot.
    #[must_use]
    pub fn partial(&mut self) -> Rc<PartialAssistant> {
        if self.changed {
            self.snapshot = Rc::new(PartialAssistant {
                turn: self.turn,
                step: self.step,
                blocks: Rc::new(self.blocks.iter().flatten().cloned().collect()),
            });
            self.changed = false;
        }
        self.snapshot.clone()
    }

    fn get(&self, index: usize) -> Option<Rc<AssistantBlock>> {
        self.blocks.get(index).and_then(Clone::clone)
    }

    fn set(&mut self, index: usize, value: Rc<AssistantBlock>) {
        if self.blocks.len() <= index {
            self.blocks.resize(index + 1, None);
        }
        self.blocks[index] = Some(value);
        self.changed = true;
    }
}

/// Empty projection for one streamed block kind.
#[must_use]
pub fn empty_assistant_block(block_type: &str) -> AssistantBlock {
    match block_type {
        "text" => AssistantBlock::Text {
            text: JsonString::default(),
        },
        "reasoning" => AssistantBlock::Reasoning {
            text: JsonString::default(),
        },
        "tool-call" => AssistantBlock::ToolCall {
            call_id: JsonString::default(),
            name: JsonString::default(),
            args_raw: JsonString::default(),
        },
        _ => AssistantBlock::Other {
            block: serde_json::Value::Null.into(),
        },
    }
}

/// Classifies one complete provider-neutral content block for Client rendering.
#[must_use]
pub fn to_assistant_block(block: &Value) -> AssistantBlock {
    match block.get_value("type").and_then(Value::as_str) {
        Some("text") => AssistantBlock::Text {
            text: block
                .get_value("text")
                .and_then(|value| value.deserialize::<JsonString>().ok())
                .unwrap_or_default(),
        },
        Some("reasoning") => AssistantBlock::Reasoning {
            text: block
                .get_value("text")
                .and_then(|value| value.deserialize::<JsonString>().ok())
                .unwrap_or_default(),
        },
        Some("image") => AssistantBlock::Image {
            attachment: block
                .get_value("attachment")
                .cloned()
                .unwrap_or_else(|| serde_json::Value::Null.into()),
        },
        Some("tool-call") => AssistantBlock::ToolCall {
            call_id: block
                .get_value("id")
                .and_then(|value| value.deserialize::<JsonString>().ok())
                .unwrap_or_default(),
            name: block
                .get_value("name")
                .and_then(|value| value.deserialize::<JsonString>().ok())
                .unwrap_or_default(),
            args_raw: block
                .get_value("arguments")
                .and_then(|value| value.deserialize::<JsonString>().ok())
                .unwrap_or_default(),
        },
        Some(_) | None => AssistantBlock::Other {
            block: block.clone(),
        },
    }
}

/// Classifies complete content blocks in source order.
#[must_use]
pub fn to_assistant_blocks(content: &[Value]) -> Vec<AssistantBlock> {
    content.iter().map(to_assistant_block).collect()
}
