//! Assistant blocks a batched Definition fold is still growing.
//!
//! A message streams thousands of chunks, and publishing each chunk into the Definition state
//! re-encodes the block's whole accumulated text — for streamed text, reasoning, and Tool call
//! arguments alike. A fold keeps the block it has not finished growing here, appends each delta to
//! its exact code units, and publishes every pending block once when the fold ends.

use seekdeep_lossless_json::{JsonString, JsonValue as Value};

use crate::AssistantBlock;

/// The kind of Assistant block a fold is still growing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PendingBlockKind {
    /// Visible text.
    #[default]
    Text,
    /// Visible reasoning.
    Reasoning,
    /// Model Tool call arguments.
    ToolCall,
}

impl PendingBlockKind {
    /// The block's `kind` discriminator.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Reasoning => "reasoning",
            Self::ToolCall => "tool-call",
        }
    }

    /// Whether a block of this kind holds visible Assistant content.
    ///
    /// A Tool call is never visible content, and text or reasoning is visible from its first
    /// non-whitespace code unit. The same rule decides visibility for a published block.
    #[must_use]
    pub fn is_visible(self, text: &JsonString) -> bool {
        match self {
            Self::ToolCall => false,
            Self::Text | Self::Reasoning => !text.trim().is_empty(),
        }
    }
}

/// One Assistant block a fold has updated without republishing the state's JSON.
#[derive(Clone, Debug, Default)]
pub struct PendingBlock {
    kind: PendingBlockKind,
    text: JsonString,
    call_id: JsonString,
    name: JsonString,
}

impl PendingBlock {
    /// An empty block of `kind`, whose accumulated text is empty.
    #[must_use]
    pub fn new(kind: PendingBlockKind) -> Self {
        Self {
            kind,
            ..Self::default()
        }
    }

    /// The same block carrying an already accumulated text.
    #[must_use]
    pub fn with_text(mut self, text: JsonString) -> Self {
        self.text = text;
        self
    }

    /// The same Tool call block carrying its streamed identity.
    #[must_use]
    pub fn with_identity(mut self, call_id: JsonString, name: JsonString) -> Self {
        self.call_id = call_id;
        self.name = name;
        self
    }

    /// This block's kind.
    #[must_use]
    pub fn kind(&self) -> PendingBlockKind {
        self.kind
    }

    /// The accumulated text: text, reasoning, or Tool call arguments.
    #[must_use]
    pub fn text(&self) -> &JsonString {
        &self.text
    }

    /// The Tool call identity accumulated so far.
    #[must_use]
    pub fn call_id(&self) -> &JsonString {
        &self.call_id
    }

    /// The Tool name accumulated so far.
    #[must_use]
    pub fn name(&self) -> &JsonString {
        &self.name
    }

    /// Appends a streamed delta's exact code units.
    pub fn push(&mut self, delta: &JsonString) {
        self.text.push_utf16(delta.utf16_units());
    }

    /// Whether this block holds visible Assistant content.
    #[must_use]
    pub fn is_visible(&self) -> bool {
        self.kind.is_visible(&self.text)
    }

    /// The block as the published projection of a streamed Assistant block.
    #[must_use]
    pub fn as_assistant_block(&self) -> AssistantBlock {
        match self.kind {
            PendingBlockKind::Text => AssistantBlock::Text {
                text: self.text.clone(),
            },
            PendingBlockKind::Reasoning => AssistantBlock::Reasoning {
                text: self.text.clone(),
            },
            PendingBlockKind::ToolCall => AssistantBlock::ToolCall {
                call_id: self.call_id.clone(),
                name: self.name.clone(),
                args_raw: self.text.clone(),
            },
        }
    }
}

/// The Assistant blocks one fold has updated without republishing the state's JSON.
#[derive(Default)]
pub struct PendingBlocks {
    entries: Vec<(usize, PendingBlock)>,
}

impl PendingBlocks {
    fn position(&self, index: usize) -> Option<usize> {
        self.entries.iter().position(|(at, _)| *at == index)
    }

    /// The pending block at `index`, when this fold already owns it.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&PendingBlock> {
        self.position(index).map(|at| &self.entries[at].1)
    }

    /// Whether the fold is still growing any block.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether any block still being grown holds visible content.
    #[must_use]
    pub fn has_visible_content(&self) -> bool {
        self.entries.iter().any(|(_, block)| block.is_visible())
    }

    /// Forgets every pending block, for a state whose blocks were replaced wholesale.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Forgets the pending block at `index`, for a block that was replaced wholesale.
    pub fn remove(&mut self, index: usize) {
        if let Some(at) = self.position(index) {
            self.entries.remove(at);
        }
    }

    /// The block at `index`, loading it through `load` unless the fold already grows it.
    ///
    /// A block of another kind is a different block at the same index, so it is replaced by
    /// whatever `load` reports for the requested kind.
    pub fn load(
        &mut self,
        index: usize,
        kind: PendingBlockKind,
        load: impl FnOnce() -> PendingBlock,
    ) -> &mut PendingBlock {
        let at = match self.position(index) {
            Some(at) if self.entries[at].1.kind == kind => at,
            at => {
                let block = load();
                if let Some(at) = at {
                    self.entries[at].1 = block;
                    at
                } else {
                    self.entries.push((index, block));
                    self.entries.len() - 1
                }
            }
        };
        &mut self.entries[at].1
    }

    /// Publishes every pending block into `blocks` through `value`.
    pub fn publish(
        &self,
        blocks: &mut Vec<Option<Value>>,
        mut value: impl FnMut(&PendingBlock) -> Value,
    ) {
        for (index, block) in &self.entries {
            if blocks.len() <= *index {
                blocks.resize(*index + 1, None);
            }
            blocks[*index] = Some(value(block));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &str) -> JsonString {
        JsonString::from(value)
    }

    fn published(block: &PendingBlock) -> Value {
        match block.kind() {
            PendingBlockKind::ToolCall => Value::object([
                ("kind", text("tool-call").into()),
                ("callId", block.call_id().clone().into()),
                ("name", block.name().clone().into()),
                ("argsRaw", block.text().clone().into()),
            ]),
            kind => Value::object([
                ("kind", text(kind.as_str()).into()),
                ("text", block.text().clone().into()),
            ]),
        }
    }

    #[test]
    fn a_grown_block_publishes_what_appending_each_delta_publishes() {
        let mut pending = PendingBlocks::default();
        for delta in ["# ", "title", " text"] {
            pending
                .load(2, PendingBlockKind::Text, || {
                    PendingBlock::new(PendingBlockKind::Text)
                })
                .push(&text(delta));
        }
        let mut blocks = Vec::new();
        pending.publish(&mut blocks, published);
        assert_eq!(blocks.len(), 3, "the published block keeps its index");
        let block = blocks[2].clone().expect("published block");
        assert_eq!(
            block.get_value("text").and_then(Value::as_str),
            Some("# title text")
        );
        assert_eq!(
            block.get_value("kind").and_then(Value::as_str),
            Some("text")
        );
    }

    #[test]
    fn a_loaded_block_keeps_the_text_the_fold_inherited() {
        let mut pending = PendingBlocks::default();
        let entry = pending.load(0, PendingBlockKind::Reasoning, || {
            PendingBlock::new(PendingBlockKind::Reasoning).with_text(text("already "))
        });
        assert_eq!(entry.text().as_str(), Some("already "));
        entry.push(&text("streamed"));
        assert_eq!(
            pending.get(0).expect("pending block").text().as_str(),
            Some("already streamed")
        );
    }

    #[test]
    fn a_block_of_another_kind_is_replaced_by_the_loaded_one() {
        let mut pending = PendingBlocks::default();
        let entry = pending.load(1, PendingBlockKind::Text, || {
            PendingBlock::new(PendingBlockKind::Text).with_text(text("text"))
        });
        entry.push(&text(" more"));
        let entry = pending.load(1, PendingBlockKind::ToolCall, || {
            PendingBlock::new(PendingBlockKind::ToolCall)
                .with_identity(text("call-1"), text("read"))
        });
        assert_eq!(entry.kind(), PendingBlockKind::ToolCall);
        assert_eq!(entry.call_id().as_str(), Some("call-1"));
        assert_eq!(entry.name().as_str(), Some("read"));
        assert!(entry.text().is_empty(), "the replaced block starts empty");
    }

    #[test]
    fn a_tool_call_block_is_never_visible_and_whitespace_text_is_not_visible() {
        let mut pending = PendingBlocks::default();
        pending
            .load(0, PendingBlockKind::ToolCall, || {
                PendingBlock::new(PendingBlockKind::ToolCall)
            })
            .push(&text("{\"path\":\"a\"}"));
        assert!(!pending.has_visible_content());
        pending
            .load(1, PendingBlockKind::Text, || {
                PendingBlock::new(PendingBlockKind::Text)
            })
            .push(&text(" \n\t"));
        assert!(!pending.has_visible_content());
        pending
            .load(1, PendingBlockKind::Reasoning, || {
                PendingBlock::new(PendingBlockKind::Reasoning)
            })
            .push(&text("why"));
        assert!(pending.has_visible_content());
    }

    #[test]
    fn a_replaced_block_is_forgotten_and_publishes_nothing() {
        let mut pending = PendingBlocks::default();
        pending
            .load(0, PendingBlockKind::Text, || {
                PendingBlock::new(PendingBlockKind::Text)
            })
            .push(&text("streamed"));
        pending.remove(0);
        pending.clear();
        assert!(pending.is_empty());
        let mut blocks = vec![Some(published(&PendingBlock::new(PendingBlockKind::Text)))];
        pending.publish(&mut blocks, published);
        assert_eq!(blocks.len(), 1, "an emptied fold publishes nothing");
    }
}
