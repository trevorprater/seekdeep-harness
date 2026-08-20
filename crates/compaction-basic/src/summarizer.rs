//! Default one-shot summarization and durable checkpoint framing.

use std::sync::Arc;

use futures::StreamExt;
use seekdeep_cordis::Context;
use seekdeep_core::session::Session;
use seekdeep_llm::{
    AbortSignal, BlockAssembler, ContentBlock, FinishReason, GenerateOptions, LLM, LlmError,
    LlmRequestPurpose, Message, MessageSource, ModelId, ProviderId, TokenUsage, ToolSchema,
    UserMessage, content_has_image,
};

/// Resolved summarization route and output cap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SummaryConfig {
    /// Summary provider route.
    pub summarization_provider: String,
    /// Summary model id.
    pub summarization_model: String,
    /// Output cap.
    pub max_tokens: u64,
}

const SUMMARY_OPEN_TAG: &str = "<compacted-summary>";
const SUMMARY_CLOSE_TAG: &str = "</compacted-summary>";

const COMPACTION_INSTRUCTION: &str = "You are now acting as a compaction engine for this AI coding assistant. Condense the conversation ABOVE into a structured checkpoint that lets another model resume the work with no loss of essential context.\n\nOutput EXACTLY the Markdown structure below: keep every section, in order. Use terse bullets, not prose paragraphs. Write \"(none)\" for an empty section — never drop a section.\n\n## Primary Request and Intent\n- [the user's original and evolving goals; quote verbatim where the exact wording matters]\n\n## Key Technical Concepts\n- [technologies, frameworks, patterns, and conventions in play]\n\n## Files and Code\n- [exact path: why it matters, key changes or snippets]\n\n## Errors and Fixes\n- [error: how it was resolved, plus any related user feedback]\n\n## Pending Jobs\n- [explicitly requested work not yet completed]\n\n## Current Work\n- [precisely what was in progress at this checkpoint]\n\n## Next Step\n- [the single next action, directly in line with the most recent request, or \"(none)\"]\n\n## Critical Context\n- [decisions and their rationale, constraints, user preferences, open questions, data needed to continue]\n\nRules:\n- Write concise English engineering prose. Preserve exact file paths, commands, error strings, identifiers, numeric values, function signatures, and syntax fragments.\n- Capture user feedback and explicit instructions faithfully, especially corrections.\n- Do NOT mention this summarization request or that the context was compacted.\n- Output only the checkpoint text: do not call any tool or take any other action.\n- If the conversation already contains a <compacted-summary> block, it is a PRIOR checkpoint. Do not copy it forward verbatim: preserve still-true facts, drop stale ones, and merge newer information into a single consolidated summary under the same structure.";

const CHECKPOINT_PREAMBLE: &str = "This is an automatically generated checkpoint condensing an earlier span of the conversation to free up context. Treat the captured context as established background and build on it without restating it. Continue the task directly from the messages that follow, without acknowledging this checkpoint.";

/// The replayed conversation surface the summarizer condenses.
#[derive(Clone, Debug)]
pub struct SummarizationInput {
    /// The conversation's own system prompt, reused for prefix-cache alignment.
    pub system: Option<String>,
    /// The conversation's tool schemas, reused for prefix-cache alignment.
    pub tools: Option<Vec<ToolSchema>>,
    /// The shadowed region, in surface order.
    pub messages: Vec<Message>,
}

/// Safe summary content plus the exact auxiliary call envelope recorded with it.
#[derive(Clone, Debug)]
pub struct SummaryResult {
    /// Safe text-only model output.
    pub summary: Vec<ContentBlock>,
    /// Complete provider output before the text-only summary projection.
    pub raw_output: Vec<ContentBlock>,
    /// Identifies exactly one call through the LLM seam.
    pub llm_stream_call: bool,
    /// Resolved provider route.
    pub provider: String,
    /// Resolved model id.
    pub model: String,
    /// Output cap.
    pub max_tokens: Option<u64>,
    /// Provider-reported usage.
    pub usage: Option<TokenUsage>,
}

/// One resolved summarization route.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// Provider route.
    pub provider: String,
    /// Model id.
    pub model: String,
}

/// Runs the default cache-reusing `ctx.llm.stream()` summarization call.
///
/// # Errors
///
/// Returns a missing-service, no-route, stream, assembly, or empty-summary
/// failure.
pub async fn summarize_with_llm(
    ctx: &Context,
    config: &SummaryConfig,
    input: &SummarizationInput,
    session: &Arc<Session>,
    fallback: Option<Target>,
    signal: Option<AbortSignal>,
) -> anyhow::Result<SummaryResult> {
    let latest = session.request_header().map(|header| Target {
        provider: header.config.provider.as_str().to_owned(),
        model: header.config.model.as_str().to_owned(),
    });
    let configured = if config.summarization_provider.is_empty() {
        None
    } else {
        Some(Target {
            provider: config.summarization_provider.clone(),
            model: config.summarization_model.clone(),
        })
    };
    let target = configured.or(latest).or(fallback).ok_or_else(|| {
        anyhow::anyhow!(
            "no provider/model available for summarization: set both BasicCompactionConfig summarization fields, route one request, or set both AgentOptions fields"
        )
    })?;

    let mut assembler = BlockAssembler::new();
    let mut messages = input.messages.clone();
    messages.push(
        UserMessage::new(
            vec![ContentBlock::Text {
                text: COMPACTION_INSTRUCTION.to_owned(),
            }],
            MessageSource::plugin("seekdeep-compaction-basic"),
        )
        .into_message(),
    );

    let provider = ProviderId::new(target.provider.clone());
    let model = ModelId::new(target.model.clone());
    let mut options = GenerateOptions::new(provider.clone(), model.clone(), messages);
    options.system = input.system.clone();
    options.tools = input.tools.clone();
    options.max_tokens = Some(config.max_tokens);
    options.session_id = Some(session.id().clone());
    options.purpose = Some(LlmRequestPurpose::Compaction);
    options.signal = signal;

    let llm = ctx
        .get(LLM)
        .ok_or_else(|| anyhow::anyhow!("compaction summarizer requires llm"))?;
    let mut stream = llm.stream(options);
    while let Some(chunk) = stream.next().await {
        assembler.push(chunk?);
    }

    if let Some(error) = finish_error(&assembler.finish()) {
        return Err(error);
    }
    let raw_output = assembler.blocks()?;
    let summary = summary_text(&raw_output)?;
    if !summary
        .iter()
        .any(|block| matches!(block, ContentBlock::Text { text } if !text.trim().is_empty()))
    {
        anyhow::bail!("summarization produced no text summary content");
    }
    Ok(SummaryResult {
        summary,
        raw_output,
        llm_stream_call: true,
        provider: provider.as_str().to_owned(),
        model: model.as_str().to_owned(),
        max_tokens: Some(config.max_tokens),
        usage: assembler.usage().cloned(),
    })
}

/// Wraps raw summary blocks in the durable checkpoint framing.
#[must_use]
pub fn frame_summary(summary: &[ContentBlock]) -> Vec<ContentBlock> {
    let mut blocks = vec![ContentBlock::Text {
        text: format!("{CHECKPOINT_PREAMBLE}\n\n{SUMMARY_OPEN_TAG}"),
    }];
    blocks.extend(summary.iter().cloned());
    blocks.push(ContentBlock::Text {
        text: SUMMARY_CLOSE_TAG.to_owned(),
    });
    blocks
}

fn finish_error(finish: &FinishReason) -> Option<anyhow::Error> {
    match finish {
        FinishReason::Error { failure } | FinishReason::Aborted { failure } => {
            Some(anyhow::Error::from(LlmError::simple(
                failure.message.clone(),
                failure.code.clone(),
            )))
        }
        FinishReason::MaxTokens => Some(anyhow::Error::from(LlmError::simple(
            "summarization truncated at the token cap (incomplete checkpoint)",
            "MAX_TOKENS",
        ))),
        _ => None,
    }
}

fn summary_text(blocks: &[ContentBlock]) -> anyhow::Result<Vec<ContentBlock>> {
    if content_has_image(blocks) {
        return Err(anyhow::Error::from(LlmError::simple(
            "compaction summary cannot contain image output",
            "UNSUPPORTED_CONTENT",
        )));
    }
    Ok(blocks
        .iter()
        .filter(|block| matches!(block, ContentBlock::Text { .. }))
        .cloned()
        .collect())
}
