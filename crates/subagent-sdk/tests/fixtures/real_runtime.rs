//! Complete keyless Harness child served over the production SDK protocol.

use std::sync::Arc;

use async_trait::async_trait;
use futures::stream;
use seekdeep_agent_loop::{AgentLoop, AgentLoopServices, DEFAULT_MAX_PARALLEL_TOOL_CALLS};
use seekdeep_cordis::Context;
use seekdeep_llm::{AdapterStream, FinishReason, GenerateOptions, LlmAdapter, StreamChunk};
use seekdeep_session_persistence::SESSION_PERSISTENCE;
use seekdeep_session_persistence_jsonl::{JsonlCompression, JsonlConfig};

#[derive(Debug)]
struct ChildAnswerAdapter {
    answer: String,
}

#[async_trait]
impl LlmAdapter for ChildAnswerAdapter {
    fn stream(&self, _options: GenerateOptions) -> AdapterStream {
        AdapterStream::new(stream::iter([
            Ok(StreamChunk::TextDelta {
                index: 0,
                text: self.answer.clone(),
            }),
            Ok(StreamChunk::Finish {
                reason: FinishReason::Stop,
                replay_state: None,
            }),
        ]))
    }
}

fn main() -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run())
}

async fn run() -> anyhow::Result<()> {
    let context = Context::new();
    let dependencies = seekdeep_agent_loop_testkit::mount_agent_loop_test_dependencies(
        &context,
        seekdeep_agent_loop_testkit::AgentLoopTestDependenciesOptions::default(),
    )?;
    let cwd = std::fs::canonicalize(std::env::current_dir()?)?;
    dependencies.llm.register_adapter(
        &["fixture-provider".to_owned()],
        Arc::new(ChildAnswerAdapter {
            answer: format!("child cwd: {}", cwd.display()),
        }),
    )?;
    let persistence = seekdeep_session_persistence_jsonl::install(
        &context,
        JsonlConfig {
            root: cwd.join(".child-sessions"),
            pack_chunks: false,
            compression: JsonlCompression::None,
            write_batch_max_delay_ms: 1,
            prepared_session_cache_size: 5,
        },
    )?;
    persistence.await_settled().await?;
    let loop_ = AgentLoop::new(
        context.clone(),
        Arc::clone(&dependencies.sessions),
        (*dependencies.agents).clone(),
        AgentLoopServices {
            llm: Arc::clone(&dependencies.llm),
            system_prompt: Arc::clone(&dependencies.system_prompt),
            tools: Arc::clone(&dependencies.tools),
            max_parallel_tool_calls: DEFAULT_MAX_PARALLEL_TOOL_CALLS,
        },
    )?;
    let persistence_service = context
        .get(SESSION_PERSISTENCE)
        .ok_or_else(|| anyhow::anyhow!("child persistence failed to activate"))?;
    loop_.set_persistence(persistence_service.persistence())?;
    dependencies.agents.set_factory(Arc::new(loop_))?;
    seekdeep_sdk_server::apply(&context, seekdeep_sdk_server::Config::default())?;
    std::future::pending::<()>().await;
    Ok(())
}
