//! Source-equivalent scripted text, tools, and orchestration model responses.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::{
    constants::{
        CODE_PROMPT, CODE_WORKER_TEXT, EXPECTED_TEXT, MINIMAL_BASH_COMMAND,
        MINIMAL_EDITOR_PATH_PREFIX, MINIMAL_PROMPT, MINIMAL_SYSTEM_PROMPT, MINIMAL_TEXT,
        SNAPSHOT_DIRECT_CHILD_PROMPT, SNAPSHOT_FINAL_TEXT, SNAPSHOT_PLUGIN_CODE, SNAPSHOT_PROMPT,
        SNAPSHOT_WORKFLOW_CHILD_PROMPT, SNAPSHOT_WORKFLOW_SCRIPT, WORKFLOW_PROMPT,
        WORKFLOW_WORKER_TEXT,
    },
    json::dumps,
};

/// Selects the next SSE JSON chunks from the complete `OpenAI` request history.
///
/// # Errors
/// Rejects malformed history, missing advertised tools, and incorrect prior tool results.
pub fn completion_chunks(body: &Value) -> anyhow::Result<Vec<Value>> {
    let (messages, latest) = body["messages"]
        .as_array()
        .and_then(|messages| messages.last().map(|latest| (messages, latest)))
        .ok_or_else(|| anyhow::anyhow!("model request has no messages: {body}"))?;
    anyhow::ensure!(
        latest.is_object(),
        "model request has an invalid latest message: {body}"
    );
    if latest["role"] == "tool" {
        return tool_followup(body, messages, latest);
    }
    let prompts = messages
        .iter()
        .rev()
        .filter(|message| message["role"] == "user")
        .map(|message| message_text(&message["content"]))
        .collect::<Vec<_>>();
    if prompts.iter().any(|prompt| {
        prompt.starts_with(&format!("{MINIMAL_PROMPT}\n{MINIMAL_EDITOR_PATH_PREFIX}"))
    }) {
        let names = advertised_tool_names(body)?;
        anyhow::ensure!(
            names == BTreeSet::from(["bash", "str_replace_editor"]),
            "minimal agent smoke advertised unexpected tools: {names:?}"
        );
        let system = messages
            .iter()
            .filter(|message| message["role"] == "system")
            .map(|message| message_text(&message["content"]))
            .collect::<Vec<_>>();
        anyhow::ensure!(
            system == [MINIMAL_SYSTEM_PROMPT],
            "minimal agent smoke assembled unexpected system prompts: {system:?}"
        );
        return Ok(tool_chunks(
            "minimal-bash-1",
            "bash",
            &json!({"command": MINIMAL_BASH_COMMAND}),
        ));
    }
    let fallback = message_text(&latest["content"]);
    let prompt = prompts
        .iter()
        .find(|prompt| {
            [
                SNAPSHOT_DIRECT_CHILD_PROMPT,
                SNAPSHOT_WORKFLOW_CHILD_PROMPT,
                SNAPSHOT_PROMPT,
                CODE_PROMPT,
                WORKFLOW_PROMPT,
            ]
            .contains(&prompt.as_str())
        })
        .map_or(fallback.as_str(), String::as_str);
    Ok(match prompt {
        SNAPSHOT_DIRECT_CHILD_PROMPT => text_chunks("DIRECT_CHILD_OK"),
        SNAPSHOT_WORKFLOW_CHILD_PROMPT => text_chunks("WORKFLOW_CHILD_OK"),
        SNAPSHOT_PROMPT => {
            require_tool(body, "cordis_define")?;
            tool_chunks(
                "advanced-define",
                "cordis_define",
                &json!({
                    "plugin":{"kind":"new","idPrefix":"snap"}, "name":"Snapshot Double",
                    "purpose":"Expose a deterministic doubling tool for executable snapshot verification.",
                    "code":{"host":SNAPSHOT_PLUGIN_CODE}
                }),
            )
        }
        CODE_PROMPT => {
            require_tool(body, "run_code")?;
            tool_chunks(
                "call-code-worker",
                "run_code",
                &json!({"code":"return 6 * 7","description":"Compute the smoke value"}),
            )
        }
        WORKFLOW_PROMPT => {
            require_tool(body, "workflow")?;
            tool_chunks(
                "call-workflow-worker",
                "workflow",
                &json!({"script":"return 6 * 7","meta":{"name":"pkg-worker-smoke","description":"exercise the packaged workflow worker"}}),
            )
        }
        _ => text_chunks(EXPECTED_TEXT),
    })
}

fn tool_followup(body: &Value, messages: &[Value], latest: &Value) -> anyhow::Result<Vec<Value>> {
    let (call_id, tool) = latest_tool_call(messages)?;
    let text = message_text(&latest["content"]);
    if call_id.starts_with("minimal-") {
        return minimal_followup(body, call_id, tool, &text);
    }
    if call_id.starts_with("advanced-") {
        return advanced_followup(body, call_id, tool, &text);
    }
    anyhow::ensure!(
        text.contains("42"),
        "{tool} worker returned no expected value: {latest}"
    );
    Ok(match tool {
        "run_code" => text_chunks(CODE_WORKER_TEXT),
        "workflow" => text_chunks(WORKFLOW_WORKER_TEXT),
        _ => anyhow::bail!("unexpected tool follow-up: {tool}"),
    })
}

fn minimal_followup(body: &Value, id: &str, tool: &str, text: &str) -> anyhow::Result<Vec<Value>> {
    Ok(match (id, tool) {
        ("minimal-bash-1", "bash") => {
            anyhow::ensure!(
                text.contains("COUNT=1"),
                "first persistent bash call lost its output: {text}"
            );
            tool_chunks(
                "minimal-bash-2",
                "bash",
                &json!({"command":MINIMAL_BASH_COMMAND}),
            )
        }
        ("minimal-bash-2", "bash") => {
            anyhow::ensure!(
                text.contains("COUNT=2 CWD=/tmp"),
                "persistent bash did not retain state: {text}"
            );
            let messages = body["messages"].as_array().ok_or_else(|| {
                anyhow::anyhow!("persistent editor smoke request has no messages")
            })?;
            let path = messages
                .iter()
                .filter(|message| message["role"] == "user")
                .map(|message| message_text(&message["content"]))
                .find_map(|text| {
                    text.split_once(MINIMAL_EDITOR_PATH_PREFIX)
                        .map(|(_, path)| {
                            path.trim_matches(|character: char| {
                                character.is_whitespace()
                                    || ('\u{1c}'..='\u{1f}').contains(&character)
                            })
                            .to_owned()
                        })
                })
                .ok_or_else(|| {
                    anyhow::anyhow!("persistent editor smoke prompt has no editor path")
                })?;
            tool_chunks(
                "minimal-editor",
                "str_replace_editor",
                &json!({"command":"create","path":path,"file_text":"created by packaged editor\n"}),
            )
        }
        ("minimal-editor", "str_replace_editor") => {
            anyhow::ensure!(
                text.contains("New file created successfully"),
                "packaged editor did not create its file: {text}"
            );
            text_chunks(MINIMAL_TEXT)
        }
        _ => anyhow::bail!("unexpected minimal-agent follow-up: {id} {tool}: {text}"),
    })
}

fn advanced_followup(body: &Value, id: &str, tool: &str, text: &str) -> anyhow::Result<Vec<Value>> {
    Ok(match (id, tool) {
        ("advanced-define", "cordis_define") => {
            anyhow::ensure!(
                text.contains("Defined snap-1/pkg-1 (Snapshot Double)"),
                "cordis_define returned no dynamic Package ids: {text}"
            );
            anyhow::ensure!(
                !advertised_tool_names(body)?.contains("snapshot_double"),
                "snapshot_double was advertised before cordis_run"
            );
            require_tool(body, "cordis_run")?;
            tool_chunks(
                "advanced-run",
                "cordis_run",
                &json!({"pluginId":"snap-1","packageId":"pkg-1","mode":"run"}),
            )
        }
        ("advanced-run", "cordis_run") => {
            anyhow::ensure!(
                text.contains("snap-1/pkg-1 is running (run-1)"),
                "cordis_run returned no running Package ids: {text}"
            );
            require_tool(body, "run_code")?;
            require_tool(body, "snapshot_double")?;
            tool_chunks(
                "advanced-code",
                "run_code",
                &json!({"code":"return await tools.snapshot_double({ value: 21 })","description":"Run the temporary Plugin tool"}),
            )
        }
        ("advanced-code", "run_code") => {
            anyhow::ensure!(
                text.contains("42"),
                "run_code returned no dynamic-tool value: {text}"
            );
            require_tool(body, "subagent")?;
            tool_chunks(
                "advanced-direct-child",
                "subagent",
                &json!({"description":"Check direct child","prompt":SNAPSHOT_DIRECT_CHILD_PROMPT}),
            )
        }
        ("advanced-direct-child", "subagent") => {
            anyhow::ensure!(
                text.contains("DIRECT_CHILD_OK"),
                "subagent returned no expected child value: {text}"
            );
            require_tool(body, "workflow")?;
            tool_chunks(
                "advanced-workflow",
                "workflow",
                &json!({"script":SNAPSHOT_WORKFLOW_SCRIPT,"meta":{"name":"advanced-exe-snapshot","description":"exercise one packaged workflow child"}}),
            )
        }
        ("advanced-workflow", "workflow") => {
            anyhow::ensure!(
                text.contains("WORKFLOW_CHILD_OK"),
                "workflow returned no expected child value: {text}"
            );
            require_tool(body, "cordis_undefine")?;
            tool_chunks(
                "advanced-undefine",
                "cordis_undefine",
                &json!({"pluginId":"snap-1"}),
            )
        }
        ("advanced-undefine", "cordis_undefine") => {
            anyhow::ensure!(
                text.contains("Removed dynamic Plugin snap-1 and all of its Packages."),
                "cordis_undefine returned no removal result: {text}"
            );
            anyhow::ensure!(
                !advertised_tool_names(body)?.contains("snapshot_double"),
                "snapshot_double remained advertised after cordis_undefine"
            );
            text_chunks(SNAPSHOT_FINAL_TEXT)
        }
        _ => anyhow::bail!("unexpected advanced tool follow-up: {id} {tool}: {text}"),
    })
}

fn text_chunks(text: &str) -> Vec<Value> {
    vec![
        role_chunk(),
        json!({"choices":[{"delta":{"content":text}}]}),
        json!({"choices":[{"delta":{"content":""},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":3}}),
    ]
}

fn tool_chunks(id: &str, name: &str, arguments: &Value) -> Vec<Value> {
    vec![
        role_chunk(),
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":id,"type":"function","function":{"name":name,"arguments":dumps(arguments, false, true)}}]}}]}),
        json!({"choices":[{"delta":{"content":""},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":3,"completion_tokens":3}}),
    ]
}

fn role_chunk() -> Value {
    json!({"choices":[{"delta":{"role":"assistant","content":null,"reasoning_content":""}}]})
}

fn latest_tool_call(messages: &[Value]) -> anyhow::Result<(&str, &str)> {
    for message in messages[..messages.len() - 1].iter().rev() {
        if let Some(calls) = message["tool_calls"].as_array() {
            for call in calls.iter().rev() {
                if let (Some(id), Some(name)) =
                    (call["id"].as_str(), call["function"]["name"].as_str())
                {
                    return Ok((id, name));
                }
            }
        }
    }
    anyhow::bail!("tool result has no preceding assistant tool call: {messages:?}")
}

pub(crate) fn message_text(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_owned();
    }
    content
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|block| block["text"].as_str())
                .collect()
        })
        .unwrap_or_default()
}

fn advertised_tool_names(body: &Value) -> anyhow::Result<BTreeSet<&str>> {
    let tools = body["tools"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("model request advertised no tools: {body}"))?;
    Ok(tools
        .iter()
        .filter_map(|tool| tool["function"]["name"].as_str())
        .collect())
}

fn require_tool(body: &Value, name: &str) -> anyhow::Result<()> {
    let names = advertised_tool_names(body)?;
    anyhow::ensure!(
        names.contains(name),
        "model request did not advertise {name}: {names:?}"
    );
    Ok(())
}
