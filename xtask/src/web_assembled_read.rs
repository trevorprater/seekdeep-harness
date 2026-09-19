//! Real filesystem output embedded into recorded browser history.

use std::path::Path;

use seekdeep_cordis::Context;
use seekdeep_llm::{AbortSignal, CallId};
use seekdeep_lossless_json::{JsonNumber, JsonString, JsonValue};
use seekdeep_tools::{
    ToolExecutionInput, ToolExecutionResult, ToolPresentationMode, ToolRuntimeConfig,
};

pub(super) async fn seed(fixture: &str, home: &Path, workspace: &Path) -> anyhow::Result<String> {
    let mut lines = vec![format!("{}😀tail", "a".repeat(31))];
    lines.extend((2..=20).map(|number| format!("const line{number} = {number};")));
    std::fs::write(workspace.join("nav-a.md"), lines.join("\n"))?;
    let context = Context::new();
    let seeded = seed_with_context(&context, fixture, home, workspace).await;
    context.root_fiber().dispose().await?;
    seeded
}

async fn seed_with_context(
    context: &Context,
    fixture: &str,
    home: &Path,
    workspace: &Path,
) -> anyhow::Result<String> {
    let prompt = seekdeep_system_prompt::install(
        context,
        seekdeep_system_prompt::SystemPromptConfig::default(),
    )?;
    let tools = seekdeep_tools::install(
        context,
        &prompt,
        ToolRuntimeConfig {
            mode: ToolPresentationMode::Native,
            ..Default::default()
        },
    )?;
    seekdeep_fs_local::LocalFileSystem::install(
        context,
        seekdeep_fs_local::Config {
            cwd: Some(workspace.to_string_lossy().into_owned()),
            ..Default::default()
        },
    )?;
    seekdeep_tool_fs::apply_read_tool(
        context,
        &seekdeep_tool_fs::Config {
            read_max_line_length: Some(JsonNumber::new(32.0)),
            read_stream_min_size: Some(JsonNumber::new(1.0)),
            ..Default::default()
        }
        .resolved()?,
    )?;
    let arguments = serde_json::json!({"file_path":"nav-a.md"});
    let result = tools
        .execute(ToolExecutionInput::new(
            CallId::new("read-utf16"),
            "read",
            arguments,
            AbortSignal::default(),
        ))
        .await;
    anyhow::ensure!(
        !result.is_error(),
        "assembled read failed: {:?}",
        result.error()
    );
    let value: seekdeep_tool_fs::read::ReadOutcome = result
        .json_value()
        .ok_or_else(|| anyhow::anyhow!("assembled read has no canonical output"))?
        .deserialize()?;
    let mut expected = JsonString::from("a".repeat(31));
    expected.push_utf16(&[0xd83d]);
    expected.push_str("... (line truncated to 32 chars)");
    anyhow::ensure!(
        value
            .lines
            .first()
            .is_some_and(|line| line.text == expected),
        "assembled read did not preserve its split UTF-16 prefix"
    );
    let raw = JsonString::join(
        &value
            .lines
            .iter()
            .map(|line| line.text.clone())
            .collect::<Vec<_>>(),
        "\n",
    );
    std::fs::write(
        home.join("read-expected.json"),
        JsonValue::object([
            ("lines", JsonValue::from_serialize(&value.lines)?),
            ("raw", raw.into()),
        ])
        .as_raw(),
    )?;
    replace_read_result(fixture, &result)
}

fn replace_read_result(fixture: &str, result: &ToolExecutionResult) -> anyhow::Result<String> {
    let mut records = Vec::new();
    let mut selected: Option<CallId> = None;
    let mut replaced = false;
    for record in fixture.lines() {
        let mut event = JsonValue::parse(record.to_owned())?;
        let event_type = event.get_value("type").and_then(JsonValue::as_str);
        if event_type == Some("tool/call")
            && selected.is_none()
            && event
                .get_value("data")
                .and_then(|data| data.get_value("name"))
                .and_then(JsonValue::as_str)
                == Some("read")
        {
            let data = event.get("data").expect("tool call has data");
            selected = Some(
                data.get("callId")
                    .ok_or_else(|| anyhow::anyhow!("recorded read has no call ID"))?
                    .deserialize()?,
            );
            records.push(record.to_owned());
        } else if event_type == Some("tool/result")
            && selected.as_ref().is_some_and(|selected| {
                event
                    .get_value("data")
                    .and_then(|data| data.get_value("callId"))
                    .and_then(JsonValue::as_str)
                    == Some(selected.as_str())
            })
        {
            let mut data = event.get("data").expect("tool result has data").to_owned();
            data.insert("content", JsonValue::from_serialize(result.content())?)?;
            data.insert(
                "meta",
                result
                    .meta()
                    .ok_or_else(|| anyhow::anyhow!("assembled read has no metadata"))?
                    .clone(),
            )?;
            event.insert("data", data)?;
            records.push(event.as_raw().to_owned());
            replaced = true;
        } else {
            records.push(record.to_owned());
        }
    }
    anyhow::ensure!(
        selected.is_some() && replaced,
        "source fixture has no completed read"
    );
    Ok(records.join("\n") + "\n")
}
