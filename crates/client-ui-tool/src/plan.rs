//! Pure plan summary derivation for the todo tool row.

use serde_json::Value;

/// Counts and parallel-active summary fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanSummary {
    /// Completed item count.
    pub done: usize,
    /// Total item count.
    pub total: usize,
    /// First usable active item name, preserved verbatim.
    pub active_content: Option<String>,
    /// Other active items beyond the named first item.
    pub active_extra: usize,
}

/// Derives counts and active summary from untrusted parsed todo objects.
#[must_use]
pub fn plan_summary(todos: &[Value]) -> PlanSummary {
    let active = todos
        .iter()
        .filter(|todo| todo.get("status").and_then(Value::as_str) == Some("in_progress"))
        .collect::<Vec<_>>();
    let active_content = active
        .first()
        .and_then(|todo| todo.get("content"))
        .and_then(Value::as_str)
        .filter(|content| !content.trim().is_empty())
        .map(ToOwned::to_owned);
    PlanSummary {
        done: todos
            .iter()
            .filter(|todo| todo.get("status").and_then(Value::as_str) == Some("completed"))
            .count(),
        total: todos.len(),
        active_extra: active_content.as_ref().map_or(0, |_| active.len() - 1),
        active_content,
    }
}
