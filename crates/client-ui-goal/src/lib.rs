//! Goal command projection and Rust/WASM UI semantics.

use std::rc::Rc;

mod bar;
#[cfg(target_arch = "wasm32")]
mod wasm;

pub use bar::*;
#[cfg(target_arch = "wasm32")]
pub use wasm::*;

use seekdeep_client_runtime::{
    AssemblerNodeDefinition, ChatConversationViewMetadata, ConversationAssemblerError,
    ConversationLocation, ConversationLocationEvent, ConversationMatchResult,
    ConversationMatchRole, ConversationNodeContext, ConversationViewNode, ConversationVisibility,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Stable Host plugin identity.
pub const NAME: &str = "client-ui-goal";
/// Locale namespace.
pub const GOAL_NS: &str = "goal";
/// Compiled goal-dock stylesheet.
pub const GOAL_BAR_STYLES: &str = include_str!("../data/goal-bar.css");
/// Compiled command-input bubble stylesheet.
pub const GOAL_COMMAND_STYLES: &str = include_str!("../data/command-input.css");
/// Key, Simplified Chinese, and English values in source order.
pub const GOAL_LOCALES: [(&str, &str, &str); 11] = [
    ("phase.active", "进行中的目标", "Ongoing Goal"),
    ("phase.paused", "已暂停的目标", "Paused Goal"),
    ("phase.blocked", "受阻的目标", "Blocked Goal"),
    ("objective.aria", "目标内容", "Goal objective"),
    ("commandInput.aria", "命令输入", "Command input"),
    ("action.save", "保存目标", "Save goal"),
    ("action.cancel", "取消编辑", "Cancel edit"),
    ("action.pause", "暂停目标", "Pause goal"),
    ("action.resume", "恢复目标", "Resume goal"),
    ("action.edit", "编辑目标", "Edit goal"),
    ("action.clear", "清除目标", "Clear goal"),
];

/// Branded Goal command lifecycle identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GoalCommandId(String);

impl GoalCommandId {
    /// Brands one exact wire identity.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Exact wire string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Goal-owned human command input data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalCommandInputData {
    /// Command lifecycle identity.
    pub command_id: GoalCommandId,
    /// Visible `/goal` line.
    pub text: String,
    /// Durable event time.
    pub time: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoalCommandInputState {
    command_id: GoalCommandId,
    text: String,
    time: i64,
    seq: u64,
}

fn is_js_whitespace(character: char) -> bool {
    character.is_whitespace() || character == '\u{feff}'
}

fn trim_end_js(value: &str) -> &str {
    value.trim_end_matches(is_js_whitespace)
}

/// Derives visible command text from one structured command/run event.
#[must_use]
pub fn goal_command_text(name: &str, args: Option<&str>) -> String {
    format!("/{name}{}", trim_end_js(args.unwrap_or_default()))
}

fn event_string(
    event: &ConversationLocationEvent,
    key: &str,
) -> Result<String, ConversationAssemblerError> {
    event
        .data
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            ConversationAssemblerError::new(format!("{} requires string {key}", event.event_type))
        })
}

fn encode<T: Serialize>(value: &T) -> Result<Rc<Value>, ConversationAssemblerError> {
    serde_json::to_value(value).map(Rc::new).map_err(|error| {
        ConversationAssemblerError::new(format!("goal command serialization failed: {error}"))
    })
}

fn state_of(
    context: &ConversationNodeContext,
) -> Result<GoalCommandInputState, ConversationAssemblerError> {
    let state = context.state.as_deref().ok_or_else(|| {
        ConversationAssemblerError::new("goal-command-input requires initialized state")
    })?;
    serde_json::from_value(state.clone()).map_err(|error| {
        ConversationAssemblerError::new(format!("invalid goal-command-input state: {error}"))
    })
}

fn anchor_seq(seq: u64) -> f64 {
    assert!(seq <= 9_007_199_254_740_991);
    #[allow(clippy::cast_precision_loss)]
    {
        seq as f64 - 0.1
    }
}

/// Builds the Goal-owned command input Conversation definition.
#[must_use]
pub fn goal_command_input_definition() -> AssemblerNodeDefinition {
    AssemblerNodeDefinition {
        kind: "goal-command-input".to_owned(),
        target: Some("chat".to_owned()),
        match_event: Rc::new(|event| {
            if event.event_type != "command/run"
                || event.data.get("name").and_then(Value::as_str) != Some("goal")
            {
                return Ok(None);
            }
            Ok(Some(ConversationMatchResult {
                id: event_string(event, "commandId")?,
                role: ConversationMatchRole::Start,
            }))
        }),
        start: Rc::new(|_context, accepted, _reader| {
            if accepted.event.event_type != "command/run" {
                return Err(ConversationAssemblerError::new(
                    "goal-command-input start requires command/run",
                ));
            }
            let command_id = GoalCommandId::new(event_string(&accepted.event, "commandId")?);
            let name = event_string(&accepted.event, "name")?;
            let args = accepted.event.data.get("args").and_then(Value::as_str);
            encode(&GoalCommandInputState {
                command_id,
                text: goal_command_text(&name, args),
                time: accepted.event.time,
                seq: accepted.event.seq,
            })
            .map(Some)
        }),
        update: Rc::new(|context, _accepted| Ok(context.state.clone())),
        publication: None,
        build_location_data: None,
        build_view_node: Some(Rc::new(|context| {
            let state = state_of(context)?;
            let location = context
                .start
                .as_ref()
                .map_or(ConversationLocation::Unresolved, |start| {
                    start.location.clone()
                });
            let data = GoalCommandInputData {
                command_id: state.command_id,
                text: state.text,
                time: state.time,
            };
            Ok(Some(Rc::new(ConversationViewNode {
                key: context.key.clone(),
                kind: "command-input".to_owned(),
                id: context.id.clone(),
                target: "chat".to_owned(),
                data: encode(&data)?,
                placement: None,
                chat: Some(ChatConversationViewMetadata {
                    anchor_seq: anchor_seq(state.seq),
                    location,
                    visibility: ConversationVisibility::Visible,
                }),
            })))
        })),
    }
}

/// Builds the no-op Host half of this pure Client plugin.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn host_plugin() -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new(NAME, std::iter::empty::<String>(), |_, _| {
        Box::pin(async { Ok(()) })
    })
}
