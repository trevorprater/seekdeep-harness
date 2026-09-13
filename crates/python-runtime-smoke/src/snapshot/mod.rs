//! SDK result and JSONL snapshots with only source-defined volatile fields removed.

use std::{cmp::Reverse, collections::BTreeSet, path::Path};

use serde_json::{Value, json};

use crate::{
    constants::{SNAPSHOT_FINAL_TEXT, SNAPSHOT_SESSION_ID},
    json::{compact, dumps},
};

mod scheduling;
pub use scheduling::canonical_workflow_starts;

const FILENAMES: [&str; 4] = [
    "result.json",
    "session.jsonl",
    "session.1.jsonl",
    "session.2.jsonl",
];

/// Renders the complete advanced result and all three logs in source file order.
///
/// # Errors
/// Rejects absent or unsuccessful children, unexpected log identities, and missing workflow events.
pub fn build_snapshot_files(
    result: &Value,
    logs: &Value,
    cwd: &Path,
) -> anyhow::Result<Vec<(String, String)>> {
    anyhow::ensure!(
        result["final_response"] == SNAPSHOT_FINAL_TEXT,
        "advanced snapshot final response differs: {}",
        result["final_response"]
    );
    let notifications = result["notifications"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("advanced snapshot has no notifications"))?;
    for method in ["subagent.started", "subagent.finished"] {
        anyhow::ensure!(
            notifications
                .iter()
                .filter(|item| item["method"] == method)
                .count()
                == 2,
            "advanced snapshot emitted unexpected subagent lifecycle: {notifications:?}"
        );
    }
    let events = result["events"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("advanced snapshot has no events"))?;
    anyhow::ensure!(
        events
            .iter()
            .any(|event| event["type"] == "tool/code-dispatch"),
        "advanced snapshot emitted no tool/code-dispatch event"
    );
    let children = child_ids(notifications)?;
    let logs = logs
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("advanced snapshot logs are not an object"))?;
    let actual_ids = logs.keys().map(String::as_str).collect::<BTreeSet<_>>();
    let expected_ids = BTreeSet::from([SNAPSHOT_SESSION_ID, children[0], children[1]]);
    anyhow::ensure!(
        actual_ids == expected_ids,
        "advanced snapshot expected parent plus two child logs: {actual_ids:?}"
    );
    for (child, expected) in children
        .iter()
        .zip(["DIRECT_CHILD_OK", "WORKFLOW_CHILD_OK"])
    {
        anyhow::ensure!(
            render_jsonl(&logs[*child])?.contains(expected),
            "advanced child log has no expected result: {expected}"
        );
    }
    let run_ids = events
        .iter()
        .filter(|event| {
            event["type"]
                .as_str()
                .is_some_and(|kind| kind.starts_with("tool-workflow/"))
        })
        .filter_map(|event| event["data"]["runId"].as_str())
        .collect::<BTreeSet<_>>();
    let run_id = run_ids
        .first()
        .filter(|_| run_ids.len() == 1)
        .ok_or_else(|| {
            anyhow::anyhow!("advanced snapshot expected one workflow run id: {run_ids:?}")
        })?;
    let mut replacements = vec![
        (cwd.to_string_lossy().into_owned(), "{{cwd}}".to_owned()),
        (SNAPSHOT_SESSION_ID.to_owned(), "{{parent}}".to_owned()),
        ((*run_id).to_owned(), "{{workflow-run}}".to_owned()),
    ];
    for (index, child) in children.iter().enumerate() {
        let agent = agent_id(notifications, child)?;
        replacements.push(((*child).to_owned(), format!("{{{{child-{}}}}}", index + 1)));
        replacements.push((agent.to_owned(), format!("{{{{agent-{}}}}}", index + 1)));
    }
    replacements.sort_by_key(|(actual, _)| Reverse(actual.chars().count()));
    let value = json!({"session_id":result["session_id"],"final_response":result["final_response"],"events":result["events"],"notifications":result["notifications"],"session_root":result["session_root"]});
    let mut files = vec![
        (
            FILENAMES[0].to_owned(),
            format!(
                "{}\n",
                dumps(&normalize_snapshot_value(value, &replacements), true, false)
            ),
        ),
        (
            FILENAMES[1].to_owned(),
            render_jsonl(&normalize_snapshot_value(
                logs[SNAPSHOT_SESSION_ID].clone(),
                &replacements,
            ))?,
        ),
    ];
    for (index, child) in children.iter().enumerate() {
        files.push((
            FILENAMES[index + 2].to_owned(),
            render_jsonl(&normalize_snapshot_value(
                logs[*child].clone(),
                &replacements,
            ))?,
        ));
    }
    Ok(files)
}

fn child_ids(notifications: &[Value]) -> anyhow::Result<Vec<&str>> {
    let mut children = Vec::new();
    for notification in notifications {
        if notification["method"] == "subagent.started"
            && notification["payload"]["parentSessionId"] == SNAPSHOT_SESSION_ID
            && let Some(child) = notification["payload"]["childSessionId"].as_str()
            && !children.contains(&child)
        {
            children.push(child);
        }
    }
    anyhow::ensure!(
        children.len() == 2,
        "advanced snapshot expected two child session ids: {children:?}"
    );
    Ok(children)
}

fn agent_id<'a>(notifications: &'a [Value], child: &str) -> anyhow::Result<&'a str> {
    for notification in notifications {
        if notification["method"] != "subagent.finished"
            || notification["payload"]["childSessionId"] != child
        {
            continue;
        }
        let payload = &notification["payload"];
        anyhow::ensure!(
            payload["provider"] == "spawn" && payload["status"] == "ok",
            "advanced child did not finish successfully: {payload}"
        );
        if let Some(agent) = payload["agentId"].as_str() {
            return Ok(agent);
        }
    }
    anyhow::bail!("advanced snapshot has no finished agent for child {child}")
}

/// Applies ordered string replacements and the source's exact header/time scrubbing.
pub fn normalize_snapshot_value(mut value: Value, replacements: &[(String, String)]) -> Value {
    match &mut value {
        Value::String(text) => {
            for (actual, token) in replacements {
                *text = text.replace(actual, token);
            }
        }
        Value::Array(items) => {
            for item in items {
                *item = normalize_snapshot_value(item.take(), replacements);
            }
        }
        Value::Object(fields) => {
            for item in fields.values_mut() {
                *item = normalize_snapshot_value(item.take(), replacements);
            }
            if fields.get("type") == Some(&json!("session")) && fields.contains_key("createdAt") {
                fields.insert("createdAt".to_owned(), json!(0));
            }
            if fields.contains_key("seq") && fields.contains_key("time") {
                fields.insert("time".to_owned(), json!(0));
            }
            if fields.get("id").is_some_and(Value::is_string)
                && fields
                    .get("role")
                    .and_then(Value::as_str)
                    .is_some_and(|role| matches!(role, "assistant" | "user"))
            {
                fields.insert("id".to_owned(), json!("{{messageId}}"));
            }
            if fields.get("type") == Some(&json!("request/header"))
                && let Some(header) = fields
                    .get_mut("data")
                    .and_then(|data| data.get_mut("header"))
                    .and_then(Value::as_object_mut)
            {
                if header.contains_key("system") {
                    header.insert("system".to_owned(), json!("{{system}}"));
                }
                if let Some(tools) = header.get_mut("tools").and_then(Value::as_array_mut) {
                    for tool in tools {
                        *tool = if tool.is_object() {
                            tool["name"].clone()
                        } else {
                            json!("{{tools}}")
                        };
                    }
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
    value
}

pub(crate) fn render_jsonl(records: &Value) -> anyhow::Result<String> {
    let records = records
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("session records are not an array"))?;
    let mut output = String::new();
    for record in records {
        output.push_str(&compact(record));
        output.push('\n');
    }
    Ok(output)
}

/// Writes explicitly requested snapshots, then verifies the exact file set and data.
///
/// # Errors
/// Reports filesystem failures and the first mismatching file; only demonstrated
/// child/worker start scheduling may differ in the SDK result.
pub fn compare_snapshot_files(
    directory: &Path,
    files: &[(String, String)],
    update: bool,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        files.iter().map(|(name, _)| name.as_str()).eq(FILENAMES),
        "advanced snapshot file set drifted"
    );
    if update {
        std::fs::create_dir_all(directory)?;
        for (name, content) in files {
            std::fs::write(directory.join(name), content)?;
        }
        println!(
            "smoke-python-runtime: updated snapshots in {}",
            directory.display()
        );
    }
    let existing = if directory.is_dir() {
        std::fs::read_dir(directory)?
            .filter_map(|entry| match entry {
                Ok(entry) if entry.path().is_file() => {
                    Some(Ok(entry.file_name().to_string_lossy().into_owned()))
                }
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            })
            .collect::<Result<BTreeSet<_>, _>>()?
    } else {
        BTreeSet::new()
    };
    let expected = FILENAMES
        .map(str::to_owned)
        .into_iter()
        .collect::<BTreeSet<_>>();
    anyhow::ensure!(
        existing == expected,
        "advanced snapshot files differ: missing={:?}, unexpected={:?}",
        expected.difference(&existing).collect::<Vec<_>>(),
        existing.difference(&expected).collect::<Vec<_>>()
    );
    for (name, actual) in files {
        let expected = std::fs::read_to_string(directory.join(name))?;
        if *actual == expected {
            continue;
        }
        if name == "result.json" {
            let left = canonical_workflow_starts(serde_json::from_str(&expected)?)
                .map_err(anyhow::Error::msg)?;
            let right = canonical_workflow_starts(serde_json::from_str(actual)?)
                .map_err(anyhow::Error::msg)?;
            if serde_json::to_string(&left)? == serde_json::to_string(&right)? {
                continue;
            }
        }
        let line = expected
            .lines()
            .zip(actual.lines())
            .position(|(left, right)| left != right)
            .unwrap_or_else(|| expected.lines().count().min(actual.lines().count()));
        anyhow::bail!(
            "advanced executable snapshot mismatch in {name}; rerun with --update-snapshots after reviewing the behavior\nline {}\nexpected: {}\nactual: {}",
            line + 1,
            expected.lines().nth(line).unwrap_or("<EOF>"),
            actual.lines().nth(line).unwrap_or("<EOF>")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_update_is_explicit_and_comparison_retains_bytes_and_file_set() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("advanced");
        let files = FILENAMES.map(|name| {
            (
                name.to_owned(),
                if name == "result.json" {
                    "{\"notifications\":[]}\n".to_owned()
                } else {
                    "{\"type\":\"session\"}\n".to_owned()
                },
            )
        });
        assert!(compare_snapshot_files(&root, &files, false).is_err());
        assert!(!root.exists());
        compare_snapshot_files(&root, &files, true).unwrap();
        compare_snapshot_files(&root, &files, false).unwrap();
        let mut wrong = files.clone();
        wrong[2].1.push('\n');
        assert!(
            compare_snapshot_files(&root, &wrong, false)
                .unwrap_err()
                .to_string()
                .contains("session.1.jsonl")
        );
        assert!(
            compare_snapshot_files(&root, &[], true)
                .unwrap_err()
                .to_string()
                .contains("file set drifted")
        );
        std::fs::write(root.join("unexpected.json"), "{}").unwrap();
        assert!(
            compare_snapshot_files(&root, &files, false)
                .unwrap_err()
                .to_string()
                .contains("unexpected.json")
        );
    }
}
