//! Pure plan summary derivation for the todo tool row.

use seekdeep_lossless_json::{JsonString, JsonValue};

/// Counts and parallel-active summary fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanSummary {
    /// Completed item count.
    pub done: usize,
    /// Total item count.
    pub total: usize,
    /// First usable active item name, preserved verbatim.
    pub active_content: Option<JsonString>,
    /// Other active items beyond the named first item.
    pub active_extra: usize,
}

/// Derives counts and active summary from untrusted parsed todo objects.
#[must_use]
pub fn plan_summary(todos: &[JsonValue]) -> PlanSummary {
    let active = todos
        .iter()
        .filter(|todo| todo.get_value("status").and_then(JsonValue::as_str) == Some("in_progress"))
        .collect::<Vec<_>>();
    let active_content = active
        .first()
        .and_then(|todo| todo.get_value("content"))
        .and_then(crate::model::json_string)
        .filter(|content| !content.trim().is_empty());
    PlanSummary {
        done: todos
            .iter()
            .filter(|todo| {
                todo.get_value("status").and_then(JsonValue::as_str) == Some("completed")
            })
            .count(),
        total: todos.len(),
        active_extra: active_content.as_ref().map_or(0, |_| active.len() - 1),
        active_content,
    }
}
