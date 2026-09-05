//! Recursive Code Dispatch pairing with cycle and depth safety.

use std::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};

use serde_json::Value;

use crate::OptionalJson;

/// Fixed wire-safety ceiling for every recursive Tool call consumer.
pub const MAX_TOOL_CALL_TREE_DEPTH: usize = 256;

/// Running Tool call projection.
#[derive(Clone, Debug, PartialEq)]
pub struct RunningToolCall {
    /// Stable call identity.
    pub call_id: String,
    /// Tool name.
    pub name: String,
    /// Raw JSON argument text.
    pub args_raw: String,
    /// Owning Turn; child dispatches use zero.
    pub turn: i64,
    /// Owning Step; child dispatches use zero.
    pub step: i64,
    /// Start epoch milliseconds.
    pub time: i64,
    /// Host-computed Tool call render intent; absent means generic JSON.
    pub call_view: Option<Value>,
    /// Recursively projected child calls.
    pub sub_calls: Rc<Vec<Rc<ToolCallBlock>>>,
}

/// In-window Tool call head paired with a result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCallHead {
    /// Tool name.
    pub name: String,
    /// Raw JSON argument text.
    pub args_raw: String,
}

/// Structured Tool execution failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolResultError {
    /// Error name.
    pub name: String,
    /// Stable error code.
    pub code: String,
}

/// Settled Tool result projection.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolResultNode {
    /// Settlement log sequence.
    pub seq: i64,
    /// Settlement epoch milliseconds.
    pub time: i64,
    /// Stable call identity.
    pub call_id: String,
    /// In-window call head, absent when window truncation removed it.
    pub call: Option<ToolCallHead>,
    /// Paired start time when the start is in-window.
    pub call_time: Option<i64>,
    /// Result content blocks.
    pub content: Vec<Value>,
    /// Tool outcome flag.
    pub is_error: bool,
    /// Structured Tool failure when supplied.
    pub error: Option<ToolResultError>,
    /// Merge-extensible metadata, preserving absent versus explicit null.
    pub meta: OptionalJson,
    /// Host-computed call render intent; absent means generic JSON.
    pub call_view: Option<Value>,
    /// Host-computed result render intent; absent means generic JSON.
    pub result_view: Option<Value>,
    /// Recursively projected child calls.
    pub sub_calls: Rc<Vec<Rc<ToolCallBlock>>>,
}

/// Recursive Tool call block.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolCallBlock {
    /// Active call.
    Running(RunningToolCall),
    /// Settled result.
    Settled(Box<ToolResultNode>),
}

impl ToolCallBlock {
    /// Stable call identity.
    #[must_use]
    pub fn call_id(&self) -> &str {
        match self {
            Self::Running(call) => &call.call_id,
            Self::Settled(result) => &result.call_id,
        }
    }

    /// Child call projection.
    #[must_use]
    pub fn sub_calls(&self) -> &Rc<Vec<Rc<Self>>> {
        match self {
            Self::Running(call) => &call.sub_calls,
            Self::Settled(result) => &result.sub_calls,
        }
    }

    fn with_sub_calls(&self, sub_calls: Rc<Vec<Rc<Self>>>) -> Self {
        match self {
            Self::Running(call) => Self::Running(RunningToolCall {
                sub_calls,
                ..call.clone()
            }),
            Self::Settled(result) => Self::Settled(Box::new(ToolResultNode {
                sub_calls,
                ..result.as_ref().clone()
            })),
        }
    }
}

/// Conversation node relevant to Tool child projection.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolTreeConversationNode {
    /// Settled Tool root.
    ToolResult(Rc<ToolCallBlock>),
    /// Any unrelated node retained by identity.
    Other(Value),
}

/// Code Dispatch lifecycle event.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolDispatchEvent {
    /// Child call began.
    Start {
        /// Event epoch milliseconds.
        time: i64,
        /// Parent call identity.
        parent_call_id: String,
        /// Child call identity.
        sub_call_id: String,
        /// Child Tool name.
        name: String,
        /// Structured call arguments.
        arguments: Value,
    },
    /// Child call settled.
    Settle {
        /// Event log sequence.
        seq: i64,
        /// Event epoch milliseconds.
        time: i64,
        /// Parent call identity.
        parent_call_id: String,
        /// Child call identity.
        sub_call_id: String,
        /// Child Tool name.
        name: String,
        /// Structured call arguments.
        arguments: Value,
        /// Result content blocks.
        content: Vec<Value>,
        /// Tool outcome flag.
        is_error: bool,
    },
    /// Unrelated event.
    Other,
}

struct ProjectedBlock {
    source: Rc<ToolCallBlock>,
    children: Rc<Vec<Rc<ToolCallBlock>>>,
    value: Rc<ToolCallBlock>,
}

struct NodeCache {
    source: Rc<Vec<Rc<ToolTreeConversationNode>>>,
    revision: u64,
    value: Rc<Vec<Rc<ToolTreeConversationNode>>>,
}

struct RunningCache {
    source: Rc<Vec<Rc<ToolCallBlock>>>,
    revision: u64,
    value: Rc<Vec<Rc<ToolCallBlock>>>,
}

/// Child-call index and recursive structural-sharing projector.
#[derive(Default)]
pub struct ToolCallTree {
    children_by_parent: BTreeMap<String, Rc<Vec<Rc<ToolCallBlock>>>>,
    depth_by_call: BTreeMap<String, usize>,
    projected_by_call: BTreeMap<String, ProjectedBlock>,
    revision: u64,
    nodes_cache: Option<NodeCache>,
    running_cache: Option<RunningCache>,
}

impl ToolCallTree {
    /// Clears every event-derived child edge before replay.
    pub fn reset(&mut self) {
        self.children_by_parent.clear();
        self.depth_by_call.clear();
        self.projected_by_call.clear();
        self.revision = self.revision.wrapping_add(1);
    }

    /// Folds one Code Dispatch event, consuming malformed rejected edges without hiding other events.
    pub fn apply(&mut self, event: &ToolDispatchEvent) -> bool {
        match event {
            ToolDispatchEvent::Start {
                time,
                parent_call_id,
                sub_call_id,
                name,
                arguments,
            } => {
                if !self.accept_edge(parent_call_id, sub_call_id) {
                    return true;
                }
                let running = Rc::new(ToolCallBlock::Running(RunningToolCall {
                    call_id: sub_call_id.clone(),
                    name: name.clone(),
                    args_raw: json_text(arguments),
                    turn: 0,
                    step: 0,
                    time: *time,
                    call_view: None,
                    sub_calls: Rc::new(Vec::new()),
                }));
                let mut siblings = self
                    .children_by_parent
                    .get(parent_call_id)
                    .map_or_else(Vec::new, |siblings| siblings.as_ref().clone());
                siblings.push(running);
                self.children_by_parent
                    .insert(parent_call_id.clone(), Rc::new(siblings));
                self.revision = self.revision.wrapping_add(1);
                true
            }
            ToolDispatchEvent::Settle {
                seq,
                time,
                parent_call_id,
                sub_call_id,
                name,
                arguments,
                content,
                is_error,
            } => {
                let mut siblings = self
                    .children_by_parent
                    .get(parent_call_id)
                    .map_or_else(Vec::new, |siblings| siblings.as_ref().clone());
                let at = siblings
                    .iter()
                    .position(|call| call.call_id() == sub_call_id);
                if at.is_none() && !self.accept_edge(parent_call_id, sub_call_id) {
                    return true;
                }
                let call_time = at.and_then(|index| match siblings[index].as_ref() {
                    ToolCallBlock::Running(call) => Some(call.time),
                    ToolCallBlock::Settled(_) => None,
                });
                let settled = Rc::new(ToolCallBlock::Settled(Box::new(ToolResultNode {
                    seq: *seq,
                    time: *time,
                    call_id: sub_call_id.clone(),
                    call: Some(ToolCallHead {
                        name: name.clone(),
                        args_raw: json_text(arguments),
                    }),
                    call_time,
                    content: content.clone(),
                    is_error: *is_error,
                    error: None,
                    meta: OptionalJson::Absent,
                    call_view: None,
                    result_view: None,
                    sub_calls: Rc::new(Vec::new()),
                })));
                if let Some(index) = at {
                    siblings[index] = settled;
                } else {
                    siblings.push(settled);
                }
                self.children_by_parent
                    .insert(parent_call_id.clone(), Rc::new(siblings));
                self.revision = self.revision.wrapping_add(1);
                true
            }
            ToolDispatchEvent::Other => false,
        }
    }

    /// Recursively attaches children to settled roots with cache-stable identity.
    #[must_use]
    pub fn project_nodes(
        &mut self,
        nodes: Rc<Vec<Rc<ToolTreeConversationNode>>>,
    ) -> Rc<Vec<Rc<ToolTreeConversationNode>>> {
        if let Some(cache) = &self.nodes_cache
            && Rc::ptr_eq(&cache.source, &nodes)
            && cache.revision == self.revision
        {
            return cache.value.clone();
        }
        let projected = nodes
            .iter()
            .map(|node| match node.as_ref() {
                ToolTreeConversationNode::ToolResult(block) => {
                    let projected = self.project_block(block.clone());
                    if Rc::ptr_eq(block, &projected) {
                        node.clone()
                    } else {
                        Rc::new(ToolTreeConversationNode::ToolResult(projected))
                    }
                }
                ToolTreeConversationNode::Other(_) => node.clone(),
            })
            .collect::<Vec<_>>();
        let value = if same_references(&nodes, &projected) {
            nodes.clone()
        } else {
            Rc::new(projected)
        };
        self.nodes_cache = Some(NodeCache {
            source: nodes,
            revision: self.revision,
            value: value.clone(),
        });
        value
    }

    /// Recursively attaches children to running root calls with cache-stable identity.
    #[must_use]
    pub fn project_running_calls(
        &mut self,
        calls: Rc<Vec<Rc<ToolCallBlock>>>,
    ) -> Rc<Vec<Rc<ToolCallBlock>>> {
        if let Some(cache) = &self.running_cache
            && Rc::ptr_eq(&cache.source, &calls)
            && cache.revision == self.revision
        {
            return cache.value.clone();
        }
        let projected = calls
            .iter()
            .map(|call| self.project_block(call.clone()))
            .collect::<Vec<_>>();
        let value = if same_references(&calls, &projected) {
            calls.clone()
        } else {
            Rc::new(projected)
        };
        self.running_cache = Some(RunningCache {
            source: calls,
            revision: self.revision,
            value: value.clone(),
        });
        value
    }

    fn project_block(&mut self, block: Rc<ToolCallBlock>) -> Rc<ToolCallBlock> {
        let children = self
            .children_by_parent
            .get(block.call_id())
            .cloned()
            .unwrap_or_else(|| block.sub_calls().clone());
        let projected = children
            .iter()
            .map(|child| self.project_block(child.clone()))
            .collect::<Vec<_>>();
        let child_value = if same_references(&children, &projected) {
            children
        } else {
            Rc::new(projected)
        };
        if let Some(cached) = self.projected_by_call.get(block.call_id())
            && Rc::ptr_eq(&cached.source, &block)
            && same_references(&cached.children, &child_value)
        {
            return cached.value.clone();
        }
        let value = if Rc::ptr_eq(block.sub_calls(), &child_value) {
            block.clone()
        } else {
            Rc::new(block.with_sub_calls(child_value.clone()))
        };
        self.projected_by_call.insert(
            block.call_id().to_owned(),
            ProjectedBlock {
                source: block,
                children: child_value,
                value: value.clone(),
            },
        );
        value
    }

    fn accept_edge(&mut self, parent: &str, child: &str) -> bool {
        if self.would_create_cycle(parent, child) {
            return false;
        }
        let mut pending = vec![(
            child.to_owned(),
            self.depth_by_call.get(parent).copied().unwrap_or(1) + 1,
        )];
        let mut updates = BTreeMap::new();
        let mut index = 0;
        while index < pending.len() {
            let (call_id, depth) = pending[index].clone();
            index += 1;
            let known = updates
                .get(&call_id)
                .copied()
                .or_else(|| self.depth_by_call.get(&call_id).copied())
                .unwrap_or(1);
            if depth <= known {
                continue;
            }
            if depth > MAX_TOOL_CALL_TREE_DEPTH {
                return false;
            }
            updates.insert(call_id.clone(), depth);
            if let Some(children) = self.children_by_parent.get(&call_id) {
                pending.extend(
                    children
                        .iter()
                        .map(|child| (child.call_id().to_owned(), depth + 1)),
                );
            }
        }
        self.depth_by_call.extend(updates);
        true
    }

    fn would_create_cycle(&self, parent: &str, child: &str) -> bool {
        if parent == child {
            return true;
        }
        let mut pending = vec![child.to_owned()];
        let mut visited = BTreeSet::from([child.to_owned()]);
        let mut index = 0;
        while index < pending.len() {
            let call_id = pending[index].clone();
            index += 1;
            if let Some(children) = self.children_by_parent.get(&call_id) {
                for child in children.iter() {
                    if child.call_id() == parent {
                        return true;
                    }
                    if visited.insert(child.call_id().to_owned()) {
                        pending.push(child.call_id().to_owned());
                    }
                }
            }
        }
        false
    }
}

fn same_references<T>(left: &Rc<Vec<Rc<T>>>, right: &[Rc<T>]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| Rc::ptr_eq(left, right))
}

fn json_text(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_owned())
}
