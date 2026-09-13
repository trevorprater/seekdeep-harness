//! Portable Chat-node and conversation view currencies.

use std::rc::Rc;

use seekdeep_client_runtime::{AssistantBlock, ToolCallBlock};
use serde_json::Value;

/// Tool call identity carried across selection and inspect boundaries.
#[repr(transparent)]
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConversationCallId(String);

impl ConversationCallId {
    /// Brands one exact call id.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the wire string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Details linkage target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionTarget {
    /// Turn sequence.
    pub turn_seq: u64,
    /// Optional Step sequence.
    pub step_seq: Option<u64>,
    /// Optional Tool call.
    pub call_id: Option<ConversationCallId>,
    /// Optional Tool name.
    pub tool_name: Option<String>,
}

/// One registered conversation view tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationViewTab {
    /// Slot entry id.
    pub id: String,
    /// Resolved label or id fallback.
    pub label: String,
}

/// Persisted per-session Chat store state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConversationChatStoreState {
    /// Details selection linkage.
    pub selection: Option<SelectionTarget>,
    /// Persisted composer draft.
    pub draft: String,
    /// Selected view id; absent falls back to Chat.
    pub view: Option<String>,
    /// One-shot inspect handoff.
    pub inspect: Option<ConversationCallId>,
}

/// Extensible Chat renderer kind.
#[repr(transparent)]
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChatNodeKind(String);

impl ChatNodeKind {
    /// Brands one renderer dispatch key.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the renderer key.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Chat node visibility.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChatNodeVisibility {
    /// Included in ordered render flow.
    #[default]
    Visible,
    /// Retained for legacy/data readers but omitted from render order.
    Hidden,
}

/// Final Chat node with a merge-extensible payload.
#[derive(Clone, Debug, PartialEq)]
pub struct ChatNode {
    /// Engine-owned stable key.
    pub key: String,
    /// Renderer dispatch kind.
    pub kind: ChatNodeKind,
    /// Definition context id.
    pub id: String,
    /// Sortable render anchor.
    pub anchor_seq: f64,
    /// Runtime location wire value.
    pub location: Value,
    /// Render visibility.
    pub visibility: ChatNodeVisibility,
    /// Renderer-owned payload.
    pub data: Value,
}

/// Assistant row lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssistantChatStatus {
    /// Streaming.
    Running,
    /// Durable final Assistant.
    Settled,
    /// Stream ended without a final Assistant.
    Interrupted,
}

/// Assistant row payload shared by streaming and final states.
#[derive(Clone, Debug, PartialEq)]
pub struct AssistantChatData {
    /// Lifecycle state.
    pub status: AssistantChatStatus,
    /// Owning Turn.
    pub turn: i64,
    /// Owning Step.
    pub step: i64,
    /// Visible Assistant blocks.
    pub blocks: Rc<Vec<Rc<AssistantBlock>>>,
    /// Start/final epoch milliseconds.
    pub time: i64,
    /// Merge-extensible usage payload.
    pub usage: Option<Value>,
    /// Durable final presentation node.
    pub final_node: Option<Value>,
}

/// Root Tool row payload.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolChatData {
    /// Root lifecycle with recursive subcalls.
    pub root: Rc<ToolCallBlock>,
}

/// Manual command and correlated compaction transaction.
#[derive(Clone, Debug, PartialEq)]
pub struct ManualCompactionChatData {
    /// Command node wire value.
    pub command: Value,
    /// Correlated compaction summary.
    pub compaction: Option<Value>,
}

/// One durable model-retry chain.
#[derive(Clone, Debug, PartialEq)]
pub struct RetryChatData {
    /// Stable attempt history.
    pub attempts: Rc<Vec<Value>>,
    /// Current attempt.
    pub current: Value,
}

/// Turn-local footer payload.
#[derive(Clone, Debug, PartialEq)]
pub struct TurnTailChatData {
    /// Owning Turn.
    pub turn: i64,
    /// Footer sequence.
    pub seq: u64,
    /// Footer epoch milliseconds.
    pub time: i64,
    /// Last finalized content-bearing Assistant.
    pub closing: Option<AssistantChatData>,
    /// Later non-rendered evidence disables branch.
    pub branch_unavailable: bool,
    /// Time to first token in milliseconds.
    pub ttft_ms: Option<f64>,
    /// Decode throughput.
    pub tokens_per_second: Option<f64>,
}

/// Whether a Tool root has settled.
#[must_use]
pub const fn is_settled_tool(block: &ToolCallBlock) -> bool {
    matches!(block, ToolCallBlock::Settled(_))
}

/// Whether a Tool root is still running.
#[must_use]
pub const fn is_running_tool(block: &ToolCallBlock) -> bool {
    matches!(block, ToolCallBlock::Running(_))
}
