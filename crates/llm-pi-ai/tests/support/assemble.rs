//! Production-path stream assembly shared by package integration tests.

use std::sync::Arc;

use futures::TryStreamExt as _;
use seekdeep_llm::{
    BlockAssembler, FinishReason, GenerateOptions, LlmRuntime, Message, MessageSource, TokenUsage,
};

/// Complete provider-neutral result assembled from the raw streaming seam.
#[derive(Debug)]
pub(crate) struct AssembledResult {
    /// Assistant message with model attribution and replay metadata.
    pub(crate) message: Message,
    /// Last cumulative token accounting, when emitted.
    pub(crate) usage: Option<TokenUsage>,
    /// Terminal finish reason.
    pub(crate) finish: FinishReason,
}

/// Drives the real `LlmRuntime::stream` path through `BlockAssembler`.
pub(crate) async fn assemble(
    runtime: &Arc<LlmRuntime>,
    options: GenerateOptions,
) -> anyhow::Result<AssembledResult> {
    let provider = options.provider.clone();
    let model = options.model.clone();
    let chunks = runtime.stream(options).try_collect::<Vec<_>>().await?;
    let mut assembler = BlockAssembler::new();
    for chunk in chunks {
        assembler.push(chunk);
    }
    let mut source = MessageSource::model(provider.as_str(), model.as_str());
    if let Some(replay_state) = assembler.replay_state() {
        source
            .fields
            .insert("replayState".to_owned(), replay_state.clone());
    }
    Ok(AssembledResult {
        message: assembler.message(Some(source))?,
        usage: assembler.usage().cloned(),
        finish: assembler.finish(),
    })
}
