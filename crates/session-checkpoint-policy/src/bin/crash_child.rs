//! Deliberately SIGKILL-targeted child for semantic checkpoint recovery tests.

use std::{path::PathBuf, sync::Arc};

use futures::{StreamExt, stream};
use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{AppendOptions, Session, SessionId, SurfaceOp},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_llm::{
    AbortSignal, AdapterStream, CallId, ContentBlock, GenerateOptions, LlmAdapter, LlmRuntime,
    Message,
};
use seekdeep_scope::ScopeKey;
use seekdeep_session_checkpoint_policy::install;
use seekdeep_session_persistence_jsonl::{JsonlCompression, JsonlConfig, JsonlSessionPersistence};
use seekdeep_tools::{
    ToolDefinition, ToolExecutionInput, ToolOutputDefinition, ToolRuntime, ToolRuntimeConfig,
    assert_supported_json_schema,
};
use serde_json::{Map, Value, json};

const SESSION_ID: &str = "semantic-checkpoint-crash";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Request,
    Tool,
}

struct CrashAdapter {
    marker: PathBuf,
}

impl LlmAdapter for CrashAdapter {
    fn stream(&self, _options: GenerateOptions) -> AdapterStream {
        let outcome = std::fs::write(&self.marker, "request-dispatched")
            .map(|()| seekdeep_llm::StreamChunk::Finish {
                reason: seekdeep_llm::FinishReason::Stop,
                replay_state: None,
            })
            .map_err(anyhow::Error::from);
        if outcome.is_err() {
            return AdapterStream::new(stream::iter([outcome]));
        }
        AdapterStream::new(stream::pending())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut arguments = std::env::args_os().skip(1);
    let mode = match arguments
        .next()
        .and_then(|value| value.into_string().ok())
        .as_deref()
    {
        Some("request") => Mode::Request,
        Some("tool") => Mode::Tool,
        _ => anyhow::bail!("usage: crash child <request|tool> <root> <marker>"),
    };
    let root = arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("missing persistence root"))?;
    let marker = arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("missing marker path"))?;

    let context = Context::new();
    let sessions = SessionStore::install(&context)?;
    let llm = LlmRuntime::install(&context)?;
    let tools = ToolRuntime::new(context.clone(), ToolRuntimeConfig::default())?;
    let mut config = JsonlConfig::new(root);
    config.compression = JsonlCompression::None;
    let _persistence = JsonlSessionPersistence::new(sessions.clone(), config)?;
    let _policy = install(&context, &llm, &sessions, &tools).await?;
    let session = sessions.create(
        &context,
        Some(SessionId::new(SESSION_ID)),
        CreateSessionOptions::default(),
    )?;
    session.append("turn/start", json!({ "turn": 1 }), AppendOptions::default())?;
    session.append(
        "step/start",
        json!({ "turn": 1, "step": 1 }),
        AppendOptions::default(),
    )?;

    match mode {
        Mode::Request => run_request(&llm, &session, marker).await?,
        Mode::Tool => run_tool(&context, &tools, session, marker).await?,
    }
    Ok(())
}

async fn run_request(
    llm: &Arc<LlmRuntime>,
    session: &Arc<Session>,
    marker: PathBuf,
) -> anyhow::Result<()> {
    session.append(
        "checkpoint/request-ready",
        json!({ "turn": 1, "step": 1, "complete": true }),
        AppendOptions {
            ignorable: true,
            ..AppendOptions::default()
        },
    )?;
    llm.register_adapter(&["crash".to_owned()], Arc::new(CrashAdapter { marker }))?;
    let mut output = llm.stream(generate_options(Some(SESSION_ID)));
    while let Some(chunk) = output.next().await {
        chunk?;
    }
    Ok(())
}

async fn run_tool(
    context: &Context,
    tools: &Arc<ToolRuntime>,
    session: Arc<Session>,
    marker: PathBuf,
) -> anyhow::Result<()> {
    let call_id = CallId::new("crash-call");
    let message = Message::assistant(
        vec![ContentBlock::ToolCall {
            id: call_id.clone(),
            name: "crash_tool".to_owned(),
            arguments: "{}".to_owned(),
        }],
        "crash",
        "crash",
    );
    session.append(
        "assistant/message",
        json!({ "turn": 1, "step": 1, "message": message }),
        AppendOptions {
            surface_op: Some(SurfaceOp::append()),
            source_event_seqs: Some(Vec::new()),
            ..AppendOptions::default()
        },
    )?;
    session.append(
        "tool/call",
        json!({
            "turn": 1,
            "step": 1,
            "callId": call_id,
            "name": "crash_tool",
            "arguments": "{}",
        }),
        AppendOptions::default(),
    )?;
    tools.register(context, crash_tool(marker))?;
    let _result = tools
        .execute(
            ToolExecutionInput::new(call_id, "crash_tool", json!({}), AbortSignal::default())
                .with_agent_scope(ScopeKey::new())
                .with_agent_session(session),
        )
        .await;
    Ok(())
}

fn crash_tool(marker: PathBuf) -> ToolDefinition {
    ToolDefinition::new(
        "crash_tool",
        "records an external effect and never returns",
        Map::new(),
        ToolOutputDefinition::new(
            Arc::new(
                assert_supported_json_schema(json!({ "type": "null" })).expect("constant schema"),
            ),
            Arc::new(|_, _| Ok(Vec::new())),
        ),
        Arc::new(move |_, _| {
            let marker = marker.clone();
            Box::pin(async move {
                tokio::fs::write(marker, "tool-side-effect").await?;
                std::future::pending::<()>().await;
                Ok(Value::Null)
            })
        }),
    )
}

fn generate_options(session_id: Option<&str>) -> GenerateOptions {
    let mut options = GenerateOptions::new(
        seekdeep_llm::ProviderId::new("crash"),
        seekdeep_llm::ModelId::new("crash"),
        Vec::new(),
    );
    options.session_id = session_id.map(seekdeep_llm::SessionId::new);
    options
}
