//! Streaming Assistant block accumulator with block-level immutability.

use std::rc::Rc;

use serde_json::Value;

/// Client projection of one Assistant content block.
#[derive(Clone, Debug, PartialEq)]
pub enum AssistantBlock {
    /// Visible text.
    Text {
        /// Complete accumulated text.
        text: String,
    },
    /// Visible reasoning.
    Reasoning {
        /// Complete accumulated reasoning text.
        text: String,
    },
    /// Image attachment reference.
    Image {
        /// Durable image attachment reference.
        attachment: Value,
    },
    /// Model Tool call.
    ToolCall {
        /// First non-empty streamed Tool call identity.
        call_id: String,
        /// Latest supplied Tool name.
        name: String,
        /// Concatenated raw argument JSON.
        args_raw: String,
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
        text: String,
    },
    /// Appended reasoning delta.
    ReasoningDelta {
        /// Sparse wire block index.
        index: usize,
        /// Appended reasoning text.
        text: String,
    },
    /// Appended Tool call delta.
    ToolCallDelta {
        /// Sparse wire block index.
        index: usize,
        /// Candidate Tool call identity.
        id: String,
        /// Optional late Tool name.
        name: Option<String>,
        /// Appended argument JSON.
        arguments_delta: String,
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
                    Some(AssistantBlock::Text { text }) => text.as_str(),
                    _ => "",
                };
                self.set(
                    *index,
                    Rc::new(AssistantBlock::Text {
                        text: format!("{prefix}{text}"),
                    }),
                );
            }
            PartialChunk::ReasoningDelta { index, text } => {
                let prior = self.get(*index);
                let prefix = match prior.as_deref() {
                    Some(AssistantBlock::Reasoning { text }) => text.as_str(),
                    _ => "",
                };
                self.set(
                    *index,
                    Rc::new(AssistantBlock::Reasoning {
                        text: format!("{prefix}{text}"),
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
                    }) => (call_id.as_str(), name.as_str(), args_raw.as_str()),
                    _ => ("", "", ""),
                };
                self.set(
                    *index,
                    Rc::new(AssistantBlock::ToolCall {
                        call_id: if call_id.is_empty() {
                            id.clone()
                        } else {
                            call_id.to_owned()
                        },
                        name: name.as_deref().unwrap_or(prior_name).to_owned(),
                        args_raw: format!("{args_raw}{arguments_delta}"),
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
            text: String::new(),
        },
        "reasoning" => AssistantBlock::Reasoning {
            text: String::new(),
        },
        "tool-call" => AssistantBlock::ToolCall {
            call_id: String::new(),
            name: String::new(),
            args_raw: String::new(),
        },
        _ => AssistantBlock::Other { block: Value::Null },
    }
}

fn to_assistant_block(block: &Value) -> AssistantBlock {
    match block.get("type").and_then(Value::as_str) {
        Some("text") => AssistantBlock::Text {
            text: block
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        },
        Some("reasoning") => AssistantBlock::Reasoning {
            text: block
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        },
        Some("image") => AssistantBlock::Image {
            attachment: block.get("attachment").cloned().unwrap_or(Value::Null),
        },
        Some("tool-call") => AssistantBlock::ToolCall {
            call_id: block
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            name: block
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            args_raw: block
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        },
        Some(_) | None => AssistantBlock::Other {
            block: block.clone(),
        },
    }
}
