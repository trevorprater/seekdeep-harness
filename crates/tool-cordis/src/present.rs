//! Pure replay-safe render intents for Cordis tools.

use seekdeep_tools::{GenericCallView, ToolCallKind, ToolCallView};
use serde_json::{Value, json};

fn generic(title: String, kind: ToolCallKind, raw_input: Option<Value>) -> ToolCallView {
    ToolCallView::Generic(GenericCallView {
        title,
        kind: Some(kind),
        raw_input,
        content: None,
        locations: None,
    })
}

/// Lists inspect providers.
#[must_use]
pub fn inspect_list_call() -> ToolCallView {
    generic(
        "List Cordis Inspect Providers".to_owned(),
        ToolCallKind::Read,
        None,
    )
}

/// Presents one provider query.
#[must_use]
pub fn inspect_query_call(platform: &str, provider: &str, method: &str) -> ToolCallView {
    generic(
        format!("Query Cordis {platform} {provider}.{method}"),
        ToolCallKind::Read,
        None,
    )
}

/// Presents layered self-inspection.
#[must_use]
pub fn inspect_self_call(plugin_id: Option<&str>, package_id: Option<&str>) -> ToolCallView {
    let target = plugin_id.map_or_else(
        || "dynamic Cordis Plugins".to_owned(),
        |plugin| {
            package_id.map_or_else(
                || plugin.to_owned(),
                |package| format!("{plugin}/{package}"),
            )
        },
    );
    generic(format!("Inspect {target}"), ToolCallKind::Read, None)
}

/// Presents an immutable package definition.
#[must_use]
pub fn define_call(target: &str, name: &str, purpose: &str, code: &Value) -> ToolCallView {
    generic(
        format!("Register Cordis Plugin \"{name}\" for {target}: {purpose}"),
        ToolCallKind::Execute,
        Some(code.clone()),
    )
}

/// Presents one exact package activation.
#[must_use]
pub fn run_call(plugin_id: &str, package_id: &str, update: bool) -> ToolCallView {
    generic(
        format!(
            "{} Cordis Plugin {plugin_id} · {package_id}",
            if update { "Update" } else { "Run" }
        ),
        ToolCallKind::Execute,
        None,
    )
}

/// Presents a temporary stop.
#[must_use]
pub fn stop_call(plugin_id: &str) -> ToolCallView {
    generic(
        format!("Stop Cordis Plugin {plugin_id}"),
        ToolCallKind::Execute,
        None,
    )
}

/// Presents permanent removal.
#[must_use]
pub fn undefine_call(plugin_id: &str) -> ToolCallView {
    generic(
        format!("Remove Cordis Plugin {plugin_id}"),
        ToolCallKind::Delete,
        None,
    )
}

/// New-plugin target spelling used by definition cards.
#[must_use]
pub fn new_target(prefix: &str) -> Value {
    json!(format!("new {prefix}-*"))
}
