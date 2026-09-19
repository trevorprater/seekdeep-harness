//! Skill tool-row model and Rust/WASM UI semantics.

mod catalog;
#[cfg(target_arch = "wasm32")]
mod wasm;

pub use catalog::*;
#[cfg(target_arch = "wasm32")]
pub use wasm::*;

/// Compiled dedicated skill-row stylesheet.
pub const SKILL_ROW_STYLES: &str = include_str!("../data/skill-row.css");

use seekdeep_client_ui_tool::{ToolCallBlock, result_text};
use seekdeep_lossless_json::{JsonString, JsonValue};

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
    pub name: JsonString,
    /// Flattened durable output.
    pub output: Option<JsonString>,
    /// First output line on execution failure.
    pub error_summary: Option<JsonString>,
    /// Current lifecycle.
    pub state: SkillRowState,
}

fn first_line(text: &JsonString) -> JsonString {
    let units = text.utf16_units();
    let end = units
        .iter()
        .position(|unit| *unit == u16::from(b'\n'))
        .unwrap_or(units.len());
    JsonString::from_utf16(&units[..end])
}

fn skill_name(args_raw: &JsonString, call_id: &str) -> JsonString {
    if let Ok(arguments) = JsonValue::parse_text(args_raw)
        && let Some(name) = arguments
            .get_value("name")
            .and_then(|value| value.deserialize::<JsonString>().ok())
            .filter(|name| !name.is_empty())
    {
        return first_line(&name);
    }
    if args_raw.is_empty() {
        call_id.into()
    } else {
        first_line(args_raw)
    }
}

/// Derives row state and copy solely from the durable call slice.
#[must_use]
pub fn skill_row_model(block: &ToolCallBlock) -> SkillRowModel {
    let (args_raw, state) = match block {
        ToolCallBlock::Running { args_raw, .. } => (args_raw.clone(), SkillRowState::Running),
        ToolCallBlock::Settled {
            call,
            error: Some(error),
            ..
        } if error.code == "interrupted" => (
            call.as_ref()
                .map(|call| call.args_raw.clone())
                .unwrap_or_default(),
            SkillRowState::Stopped,
        ),
        ToolCallBlock::Settled {
            call,
            is_error: true,
            ..
        } => (
            call.as_ref()
                .map(|call| call.args_raw.clone())
                .unwrap_or_default(),
            SkillRowState::Error,
        ),
        ToolCallBlock::Settled { call, .. } => (
            call.as_ref()
                .map(|call| call.args_raw.clone())
                .unwrap_or_default(),
            SkillRowState::Ok,
        ),
    };
    let output = block
        .settled()
        .then(|| result_text(block))
        .filter(|output| !output.is_empty());
    SkillRowModel {
        name: skill_name(&args_raw, block.call_id()),
        error_summary: (state == SkillRowState::Error)
            .then(|| output.as_ref().map(first_line))
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
