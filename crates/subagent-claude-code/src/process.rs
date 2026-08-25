//! Projection of the Claude stream protocol onto the shared subprocess owner.

use std::{collections::BTreeMap, path::Path};

use seekdeep_llm::AbortSignal;
use seekdeep_subprocess::{
    SubprocessEnvironment, SubprocessOutputMode, SubprocessSpawnSpec, SubprocessStdinMode,
    SubprocessStdio,
};
use serde_json::{Value, json};

/// Environment name carrying a quoted Windows batch executable through `cmd.exe`.
pub const WINDOWS_BATCH_EXECUTABLE_ENV: &str = "SEEKDEEP_CLAUDE_CODE_EXECUTABLE";

/// Fixed one-shot flags corresponding to the pinned official Agent SDK query.
pub const CLAUDE_STREAM_ARGS: &[&str] = &[
    "--output-format",
    "stream-json",
    "--verbose",
    "--input-format",
    "stream-json",
    "--disallowedTools",
    "AskUserQuestion",
    "--no-session-persistence",
];

/// Produces the exact one-message streaming-input envelope.
#[must_use]
pub fn prompt_frame(prompt: &str) -> Value {
    json!({
        "type":"user",
        "session_id":"",
        "message":{"role":"user","content":[{"type":"text","text":prompt}]},
        "parent_tool_use_id":null
    })
}

/// Builds the managed Claude CLI spawn request.
///
/// # Errors
///
/// Returns when the SDK boundary omitted its workspace.
pub fn claude_spawn_spec(
    executable: &str,
    cwd: &str,
    configured_env: &BTreeMap<String, String>,
    grace_ms: f64,
    signal: AbortSignal,
    platform: &str,
) -> anyhow::Result<SubprocessSpawnSpec> {
    anyhow::ensure!(
        !cwd.is_empty(),
        "subagent-claude-code: SDK spawn request omitted its workspace"
    );
    let extension = Path::new(executable)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    let batch = matches!(platform, "win32" | "windows")
        && matches!(extension.as_deref(), Some("cmd" | "bat"));
    let mut env = configured_env
        .iter()
        .map(|(key, value)| (key.clone(), Some(value.clone())))
        .collect::<SubprocessEnvironment>();
    env.entry("CLAUDE_CODE_ENTRYPOINT".to_owned())
        .or_insert_with(|| Some("sdk-ts".to_owned()));
    env.entry("CLAUDE_AGENT_SDK_VERSION".to_owned())
        .or_insert_with(|| Some("0.3.220".to_owned()));
    env.insert("NODE_OPTIONS".to_owned(), None);
    if !configured_env.contains_key("DEBUG_CLAUDE_AGENT_SDK") {
        env.insert("DEBUG".to_owned(), None);
    }
    let argv = if batch {
        env.insert(
            WINDOWS_BATCH_EXECUTABLE_ENV.to_owned(),
            Some(format!("\"{executable}\"")),
        );
        [
            "cmd.exe".to_owned(),
            "/d".to_owned(),
            "/v:off".to_owned(),
            "/s".to_owned(),
            "/c".to_owned(),
            format!("%{WINDOWS_BATCH_EXECUTABLE_ENV}%"),
        ]
        .into_iter()
        .chain(
            CLAUDE_STREAM_ARGS
                .iter()
                .map(|argument| (*argument).to_owned()),
        )
        .collect()
    } else {
        std::iter::once(executable.to_owned())
            .chain(
                CLAUDE_STREAM_ARGS
                    .iter()
                    .map(|argument| (*argument).to_owned()),
            )
            .collect()
    };
    Ok(SubprocessSpawnSpec {
        argv,
        cwd: cwd.into(),
        stdio: SubprocessStdio {
            stdin: SubprocessStdinMode::Pipe,
            stdout: SubprocessOutputMode::Pipe,
            stderr: SubprocessOutputMode::Inherit,
        },
        grace_ms,
        signal: Some(signal),
        env: Some(env),
    })
}
