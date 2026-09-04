//! Compares exact snapshot data while admitting source-observed worker scheduling.

use serde_json::Value;

/// Moves only workflow start observations across their own child's notifications.
///
/// The source child executes independently while `ChildStarted` and `AgentStart` make
/// a worker round trip. No parent event or another child's notification may move.
pub(crate) fn canonical_workflow_starts(mut result: Value) -> Result<Value, String> {
    let notifications = result["notifications"]
        .as_array_mut()
        .ok_or("snapshot result has no notifications array")?;
    let mut index = 0;
    while index < notifications.len() {
        let marker = &notifications[index];
        if marker["method"] != "session.event"
            || marker["payload"]["event"]["type"] != "tool-workflow/agent-start"
        {
            index += 1;
            continue;
        }
        let parent = marker["payload"]["sessionId"]
            .as_str()
            .ok_or("workflow start has no parent session")?
            .to_owned();
        let child = marker["payload"]["event"]["data"]["childId"]
            .as_str()
            .ok_or("workflow start has no child session")?
            .to_owned();
        if child == parent {
            return Err("workflow child aliases its parent".to_owned());
        }
        let created = notifications[..index]
            .iter()
            .rposition(|notification| {
                notification["method"] == "subagent.started"
                    && notification["payload"]["parentSessionId"] == parent
                    && notification["payload"]["childSessionId"] == child
            })
            .ok_or("workflow start precedes its child's publication")?;
        if notifications[created + 1..index]
            .iter()
            .any(|notification| !belongs_only_to_child(notification, &parent, &child))
        {
            return Err("workflow start crossed an unrelated or parent notification".to_owned());
        }
        let marker = notifications.remove(index);
        notifications.insert(created + 1, marker);
        index += 1;
    }
    Ok(result)
}

fn belongs_only_to_child(notification: &Value, parent: &str, child: &str) -> bool {
    match notification["method"].as_str() {
        Some("session.event" | "session.status") => notification["payload"]["sessionId"] == child,
        Some("subagent.finished") => {
            notification["payload"]["parentSessionId"] == parent
                && notification["payload"]["childSessionId"] == child
        }
        _ => false,
    }
}
