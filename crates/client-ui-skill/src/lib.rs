//! Skill tool-row model and Rust/WASM UI semantics.

mod catalog;

pub use catalog::*;

use seekdeep_client_ui_tool::{ToolCallBlock, result_text};
use serde_json::Value;

/// Stable Host plugin identity.
pub const NAME: &str = "client-ui-skill";
/// Dictionary namespace.
pub const SKILL_NS: &str = "skill";
/// Key, Simplified Chinese, and English values in source order.
pub const SKILL_LOCALES: [(&str, &str, &str); 5] = [
    ("row.running", "正在加载 skill", "Loading skill"),
    ("row.failed", "skill 加载失败", "Skill load failed"),
    ("row.stopped", "skill 加载已中止", "Skill load stopped"),
    ("row.instructions", "说明", "Instructions"),
    ("menu.userOnly", "仅用户", "user-only"),
];

/// Dedicated row lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkillRowState {
    /// Call has not settled.
    Running,
    /// Call settled successfully.
    Ok,
    /// Tool execution failed.
    Error,
    /// Lifecycle was interrupted.
    Stopped,
}

/// Compact replay-stable skill row model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillRowModel {
    /// Skill name or durable fallback.
    pub name: String,
    /// Flattened durable output.
    pub output: Option<String>,
    /// First output line on execution failure.
    pub error_summary: Option<String>,
    /// Current lifecycle.
    pub state: SkillRowState,
}

fn first_line(text: &str) -> &str {
    text.split_once('\n').map_or(text, |(first, _)| first)
}

fn skill_name(args_raw: &str, call_id: &str) -> String {
    if let Ok(Value::Object(arguments)) = serde_json::from_str(args_raw)
        && let Some(name) = arguments
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
    {
        return first_line(name).to_owned();
    }
    if args_raw.is_empty() {
        call_id.to_owned()
    } else {
        first_line(args_raw).to_owned()
    }
}

/// Derives row state and copy solely from the durable call slice.
#[must_use]
pub fn skill_row_model(block: &ToolCallBlock) -> SkillRowModel {
    let (args_raw, state) = match block {
        ToolCallBlock::Running { args_raw, .. } => (args_raw.as_str(), SkillRowState::Running),
        ToolCallBlock::Settled {
            call,
            error: Some(error),
            ..
        } if error.code == "interrupted" => (
            call.as_ref().map_or("", |call| call.args_raw.as_str()),
            SkillRowState::Stopped,
        ),
        ToolCallBlock::Settled {
            call,
            is_error: true,
            ..
        } => (
            call.as_ref().map_or("", |call| call.args_raw.as_str()),
            SkillRowState::Error,
        ),
        ToolCallBlock::Settled { call, .. } => (
            call.as_ref().map_or("", |call| call.args_raw.as_str()),
            SkillRowState::Ok,
        ),
    };
    let output = block
        .settled()
        .then(|| result_text(block))
        .filter(|output| !output.is_empty());
    SkillRowModel {
        name: skill_name(args_raw, block.call_id()),
        error_summary: (state == SkillRowState::Error)
            .then(|| output.as_deref().map(first_line).map(ToOwned::to_owned))
            .flatten(),
        output,
        state,
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
