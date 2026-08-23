//! Parent-side `send_message`, `interrupt_agent`, and `list_agents` tools.

use std::sync::Arc;

use seekdeep_agent::{AGENTS, AgentRegistry, AgentStatus};
use seekdeep_cordis::{Context, Plugin, fiber::EffectHandle};
use seekdeep_llm::{ContentBlock, MessageSource};
use seekdeep_subagent::{
    SUBAGENTS, SubagentFollowupOptions, SubagentInterruptAuthority, SubagentListEntry,
    SubagentListMode, SubagentRuntime,
};
use seekdeep_tools::{DefineToolOptions, DefineToolOutput, TOOLS, ToolDefinition, define_tool};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Control plugin name.
pub const NAME: &str = "tool-subagent-control";
/// List plugin name.
pub const LIST_NAME: &str = "tool-subagent-list-agents";
/// Control dependencies.
pub const INJECT: &[&str] = &["tools", "subagents"];
/// List dependencies.
pub const LIST_INJECT: &[&str] = &["tools", "subagents", "agents"];

#[derive(Debug, Deserialize)]
struct SendArgs {
    subagent_id: String,
    message: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct SendValue {
    message_id: seekdeep_llm::MessageId,
}

#[derive(Debug, Deserialize)]
struct InterruptArgs {
    agent_id: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
struct InterruptValue {
    accepted: bool,
}

/// List scope.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ListScope {
    /// Direct children only.
    #[default]
    Children,
    /// Complete descendant tree.
    Descendants,
}

#[derive(Debug, Deserialize)]
struct ListArgs {
    #[serde(default)]
    scope: ListScope,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum ListEntry {
    Child {
        id: seekdeep_core::session::SessionId,
        label: String,
        status: ChildStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<seekdeep_core::session::SessionId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        depth: Option<u64>,
    },
    Diagnostic {
        id: seekdeep_core::session::SessionId,
        reason: seekdeep_subagent::SubagentDiagnosticReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<seekdeep_core::session::SessionId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        depth: Option<u64>,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum ChildStatus {
    Running,
    Idle,
    Ready,
}

/// Registers `send_message` and `interrupt_agent` globally.
///
/// # Errors
///
/// Returns missing-service, schema, or duplicate-tool failures.
pub fn install_control(context: &Context) -> anyhow::Result<Vec<EffectHandle>> {
    let tools = context
        .get(TOOLS)
        .ok_or_else(|| anyhow::anyhow!("tool-subagent-control requires tools"))?;
    let subagents = context
        .get(SUBAGENTS)
        .ok_or_else(|| anyhow::anyhow!("tool-subagent-control requires subagents"))?;
    let send = send_definition(subagents.clone())?;
    let interrupt = interrupt_definition(subagents)?;
    let send_effect = tools.register(context, send)?;
    match tools.register(context, interrupt) {
        Ok(interrupt_effect) => Ok(vec![send_effect, interrupt_effect]),
        Err(error) => {
            let rollback = futures::executor::block_on(send_effect.dispose());
            if let Err(rollback) = rollback {
                return Err(anyhow::anyhow!(
                    "{error:#}: send_message rollback failed: {rollback:#}"
                ));
            }
            Err(error)
        }
    }
}

fn send_definition(subagents: Arc<SubagentRuntime>) -> anyhow::Result<ToolDefinition> {
    define_tool(DefineToolOptions::new(
        "send_message",
        "Send a message to a background subagent by its subagent id, continuing the same conversation. It becomes the subagent's next turn: if it is still working, the message waits until its current turn finishes, so it cannot redirect work already underway. This call returns no answer from the subagent — only confirmation that the message was delivered — so use it to give it more work. A failure means the message was NOT delivered.",
        json!({
            "subagent_id": {
                "type": "string",
                "required": true,
                "description": "The subagent id returned when the background subagent was started."
            },
            "message": {
                "type": "string",
                "required": true,
                "description": "The message to deliver to the subagent."
            }
        }),
        DefineToolOutput::new(
            json!({
                "type": "object", "additionalProperties": false,
                "properties": { "messageId": { "type": "string", "required": true } }
            }),
            Arc::new(|args: &SendArgs, _value: &SendValue| {
                Ok(vec![ContentBlock::Text {
                    text: format!(
                        "message queued as the next turn for subagent {}",
                        args.subagent_id
                    ),
                }])
            }),
        ),
        Arc::new(move |args: SendArgs, run| {
            let subagents = subagents.clone();
            Box::pin(async move {
                let parent = run.execution().agent.clone().ok_or_else(|| {
                    anyhow::anyhow!(
                        "send_message requires a calling agent (exec.agent was undefined)"
                    )
                })?;
                let source = coordinator_source(parent.id());
                let message_id = subagents
                    .followup(
                        &parent,
                        &seekdeep_core::session::SessionId::new(args.subagent_id),
                        vec![ContentBlock::Text { text: args.message }],
                        SubagentFollowupOptions {
                            source,
                            signal: run.signal(),
                        },
                    )
                    .await?;
                Ok(SendValue { message_id })
            })
        }),
    ))
}

fn coordinator_source(sender: &seekdeep_core::session::SessionId) -> MessageSource {
    let mut fields = serde_json::Map::new();
    fields.insert(
        "form".to_owned(),
        serde_json::Value::String("relay".to_owned()),
    );
    fields.insert(
        "senderSessionId".to_owned(),
        serde_json::to_value(sender).unwrap_or(serde_json::Value::Null),
    );
    MessageSource {
        kind: "coordinator".to_owned(),
        fields,
    }
}

fn interrupt_definition(subagents: Arc<SubagentRuntime>) -> anyhow::Result<ToolDefinition> {
    define_tool(DefineToolOptions::new(
        "interrupt_agent",
        "Request cancellation of a background agent's current turn by its agent id. The target may be your direct child or a deeper agent created under you. Only the current turn stops: messages already queued for the agent stay parked until a later send_message, agents it started keep running, and the agent itself stays available for follow-ups. This call returns as soon as the stop request is accepted, so the target may keep running briefly; interrupting an agent that already finished is an accepted no-op.",
        json!({
            "agent_id": {
                "type": "string",
                "required": true,
                "description": "The agent id of the running agent to interrupt."
            }
        }),
        DefineToolOutput::new(
            json!({
                "type": "object", "additionalProperties": false,
                "properties": { "accepted": { "type": "boolean", "required": true } }
            }),
            Arc::new(|args: &InterruptArgs, _value: &InterruptValue| {
                Ok(vec![ContentBlock::Text {
                    text: format!("interrupt requested for agent {}", args.agent_id),
                }])
            }),
        ),
        Arc::new(move |args: InterruptArgs, run| {
            let subagents = subagents.clone();
            Box::pin(async move {
                let caller = run.execution().agent.clone().ok_or_else(|| {
                    anyhow::anyhow!(
                        "interrupt_agent requires a calling agent (exec.agent was undefined)"
                    )
                })?;
                subagents.interrupt(
                    seekdeep_core::session::SessionId::new(args.agent_id),
                    SubagentInterruptAuthority::Ancestor { agent: caller },
                )?;
                Ok(InterruptValue { accepted: true })
            })
        }),
    ))
}

/// Registers `list_agents` globally.
///
/// # Errors
///
/// Returns missing-service, schema, or duplicate-tool failures.
pub fn install_list_agents(context: &Context) -> anyhow::Result<EffectHandle> {
    let tools = context
        .get(TOOLS)
        .ok_or_else(|| anyhow::anyhow!("tool-subagent-list-agents requires tools"))?;
    let subagents = context
        .get(SUBAGENTS)
        .ok_or_else(|| anyhow::anyhow!("tool-subagent-list-agents requires subagents"))?;
    let agents = context
        .get(AGENTS)
        .ok_or_else(|| anyhow::anyhow!("tool-subagent-list-agents requires agents"))?;
    let definition = define_tool(DefineToolOptions::new(
        "list_agents",
        "List your continuable background subagents by durable id and label. Use it to recall which ones you started, not to poll for completion — you are told when one finishes. Status comes from the live registry: running means the agent is working right now, idle means it is loaded but between turns (it may be waiting on agents it started), and ready means it exists only in storage — resumable, not terminal, and not a result waiting to be collected; a `send_message` starts a new turn on the same conversation, and a direct child remains a `send_message` candidate in every status. The snapshot is not a delivery promise — `send_message` performs the authoritative check and may still fail. Children that could not be read are reported as diagnostics instead of being silently dropped. Scope `descendants` walks the whole tree below you in stable pre-order, annotating each entry with its durable direct-parent session id and depth. You may use `send_message` only for depth-1 entries; deeper entries are candidates for `interrupt_agent` only.",
        json!({
            "scope": {
                "type": "string",
                "enum": ["children", "descendants"],
                "description": "children (default) lists direct children only; descendants walks the complete tree below you."
            }
        }),
        DefineToolOutput::new(
            list_output_schema(),
            Arc::new(|args: &ListArgs, entries: &Vec<ListEntry>| {
                let text = if entries.is_empty() {
                    "(no subagents)".to_owned()
                } else {
                    entries
                        .iter()
                        .map(|entry| render_entry(entry, args.scope))
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                Ok(vec![ContentBlock::Text { text }])
            }),
        ),
        Arc::new(move |args: ListArgs, run| {
            let subagents = subagents.clone();
            let agents = agents.clone();
            Box::pin(async move {
                let parent = run.execution().agent.clone().ok_or_else(|| {
                    anyhow::anyhow!(
                        "list_agents requires a calling agent (exec.agent was undefined)"
                    )
                })?;
                match args.scope {
                    ListScope::Children => Ok(subagents
                        .list_children(parent.id(), Some(run.signal()))
                        .await?
                        .into_iter()
                        .filter_map(|entry| project_entry(&agents, entry, None))
                        .collect()),
                    ListScope::Descendants => Ok(subagents
                        .list_descendants(parent.id(), Some(run.signal()))
                        .await?
                        .into_iter()
                        .filter_map(|position| {
                            let at = Some((position.parent_id.clone(), position.depth));
                            project_entry(&agents, position.entry, at)
                        })
                        .collect()),
                }
            })
        }),
    ))?;
    tools.register(context, definition)
}

fn list_output_schema() -> serde_json::Value {
    json!({
        "type": "array",
        "items": {
            "oneOf": [
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "kind": { "type": "string", "required": true, "enum": ["child"] },
                        "id": { "type": "string", "required": true },
                        "label": { "type": "string", "required": true },
                        "status": {
                            "type": "string",
                            "required": true,
                            "enum": ["running", "idle", "ready"]
                        },
                        "parent": { "type": "string" },
                        "depth": { "type": "number" }
                    }
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "kind": {
                            "type": "string",
                            "required": true,
                            "enum": ["diagnostic"]
                        },
                        "id": { "type": "string", "required": true },
                        "reason": {
                            "type": "string",
                            "required": true,
                            "enum": ["corrupt", "unsupported", "unavailable"]
                        },
                        "parent": { "type": "string" },
                        "depth": { "type": "number" }
                    }
                }
            ]
        }
    })
}

fn project_entry(
    agents: &AgentRegistry,
    entry: SubagentListEntry,
    position: Option<(seekdeep_core::session::SessionId, u64)>,
) -> Option<ListEntry> {
    let (parent, depth) =
        position.map_or((None, None), |(parent, depth)| (Some(parent), Some(depth)));
    match entry {
        SubagentListEntry::Diagnostic { id, reason } => Some(ListEntry::Diagnostic {
            id,
            reason,
            parent,
            depth,
        }),
        SubagentListEntry::Child {
            id: _,
            mode: SubagentListMode::OneShot { .. },
            ..
        } => None,
        SubagentListEntry::Child {
            id,
            mode: SubagentListMode::Continuable { label },
            ..
        } => Some(ListEntry::Child {
            status: status_of(agents, &id),
            id,
            label,
            parent,
            depth,
        }),
    }
}

fn status_of(agents: &AgentRegistry, id: &seekdeep_core::session::SessionId) -> ChildStatus {
    match agents.get(id) {
        None => ChildStatus::Ready,
        Some(agent) if agent.status() == AgentStatus::Running => ChildStatus::Running,
        Some(_) => ChildStatus::Idle,
    }
}

fn render_entry(entry: &ListEntry, scope: ListScope) -> String {
    let position = match (scope, entry) {
        (
            ListScope::Descendants,
            ListEntry::Child { parent, depth, .. } | ListEntry::Diagnostic { parent, depth, .. },
        ) => {
            format!(
                " parent={} depth={}",
                parent.as_ref().map(ToString::to_string).unwrap_or_default(),
                depth.unwrap_or_default()
            )
        }
        (ListScope::Children, _) => String::new(),
    };
    match entry {
        ListEntry::Child {
            id, label, status, ..
        } => format!(
            "{id} [{}]{position} — {label}",
            match status {
                ChildStatus::Running => "running",
                ChildStatus::Idle => "idle",
                ChildStatus::Ready => "ready",
            }
        ),
        ListEntry::Diagnostic { id, reason, .. } => {
            format!(
                "{id} [diagnostic: {}]{position}",
                match reason {
                    seekdeep_subagent::SubagentDiagnosticReason::Corrupt => "corrupt",
                    seekdeep_subagent::SubagentDiagnosticReason::Unsupported => "unsupported",
                    seekdeep_subagent::SubagentDiagnosticReason::Unavailable => "unavailable",
                }
            )
        }
    }
}

/// Loader plugin for delivery and interrupt controls.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, _| {
        Box::pin(async move {
            install_control(&context)?;
            Ok(())
        })
    })
}

/// Loader plugin for discovery.
#[must_use]
pub fn list_plugin() -> Plugin {
    Plugin::new(LIST_NAME, LIST_INJECT.iter().copied(), |context, _| {
        Box::pin(async move {
            install_list_agents(&context)?;
            Ok(())
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinator_attribution_has_only_the_wire_fields_owned_by_the_source() {
        assert_eq!(
            serde_json::to_value(coordinator_source(&seekdeep_core::session::SessionId::new(
                "parent"
            )))
            .unwrap(),
            json!({
                "kind": "coordinator",
                "form": "relay",
                "senderSessionId": "parent"
            })
        );
    }
}
