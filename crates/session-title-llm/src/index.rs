//! Shared route, framing, timeout, assembly, and validation policy for
//! model-backed session-title providers.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use seekdeep_cordis::Context;
use seekdeep_core::session::AppendOptions;
use seekdeep_llm::{
    AbortSignal, BlockAssembler, ContentBlock, FinishReason, GenerateOptions, LLM, LlmError,
    LlmRequestPurpose, Message, MessageSource, ModelId, ProviderId, UserMessage,
};
use seekdeep_schemastery::Schema;
use seekdeep_session_title::{
    SESSION_TITLE, SessionTitleAutomaticMode, SessionTitleModelProvenance, SessionTitleProvider,
    SessionTitleProviderId, SessionTitleProviderRequest, SessionTitleProviderResult,
    SessionTitleUserMessage, normalize_session_title,
};
use seekdeep_util::timeout::{MAX_TIMER_DELAY_MS, TimeoutReason, deadline};
use serde::{Deserialize, Serialize};

/// Capability-owned timeout reason code for auxiliary title requests.
pub const SESSION_TITLE_TIMEOUT_CODE: &str = "SESSION_TITLE_TIMEOUT";

/// Required deployment policy for one model-backed title plugin.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionTitleLlmConfig {
    /// Target word count for non-CJK titles.
    pub target_words: u64,
    /// Target character count for Chinese, Japanese, or Korean titles.
    pub target_cjk_characters: u64,
    /// Maximum UTF-8 bytes in the final JSON-framed user prompt.
    pub max_input_bytes: u64,
    /// Auxiliary generation output-token cap.
    pub max_output_tokens: u64,
    /// End-to-end auxiliary request deadline in milliseconds.
    pub timeout_ms: u64,
    /// Optional explicit provider route; must be paired with model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Optional explicit model id; must be paired with provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// The source-compatible admission schema for `SessionTitleLlmConfig`.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn config_schema() -> Schema {
    Schema::object([
        (
            "targetWords",
            Schema::number().step(1.0).min(1.0).required(),
        ),
        (
            "targetCjkCharacters",
            Schema::number().step(1.0).min(1.0).required(),
        ),
        (
            "maxInputBytes",
            Schema::number().step(1.0).min(1.0).required(),
        ),
        (
            "maxOutputTokens",
            Schema::number().step(1.0).min(1.0).required(),
        ),
        (
            "timeoutMs",
            Schema::number()
                .step(1.0)
                .min(1.0)
                .max(MAX_TIMER_DELAY_MS)
                .required(),
        ),
        ("provider", Schema::string()),
        ("model", Schema::string()),
    ])
}

/// Validates and detaches required model-provider configuration.
///
/// # Errors
///
/// Returns an invalid-config failure for unknown, non-positive, oversized, or
/// unpaired fields.
pub fn resolve_session_title_llm_config(
    config: &SessionTitleLlmConfig,
) -> anyhow::Result<SessionTitleLlmConfig> {
    for (name, value) in [
        ("targetWords", config.target_words),
        ("targetCjkCharacters", config.target_cjk_characters),
        ("maxInputBytes", config.max_input_bytes),
        ("maxOutputTokens", config.max_output_tokens),
        ("timeoutMs", config.timeout_ms),
    ] {
        if value == 0 {
            anyhow::bail!("session-title-llm: {name} must be a positive integer");
        }
    }
    if config.timeout_ms > MAX_TIMER_DELAY_MS as u64 {
        anyhow::bail!("session-title-llm: timeoutMs must not exceed {MAX_TIMER_DELAY_MS}");
    }
    let has_provider = config.provider.is_some();
    let has_model = config.model.is_some();
    if has_provider != has_model {
        anyhow::bail!("session-title-llm: provider and model must be supplied together");
    }
    if has_provider
        && (config.provider.as_deref().is_none_or(str::is_empty)
            || config.model.as_deref().is_none_or(str::is_empty))
    {
        anyhow::bail!("session-title-llm: provider and model overrides must be non-empty strings");
    }
    Ok(config.clone())
}

/// Selects the provider-owned message subset from one fixed service revision.
pub type SessionTitleLlmMessageSelector = Arc<
    dyn Fn(&[SessionTitleUserMessage]) -> anyhow::Result<Vec<SessionTitleUserMessage>>
        + Send
        + Sync,
>;

struct LlmTitleProvider {
    id: SessionTitleProviderId,
    automatic: SessionTitleAutomaticMode,
    select_messages: SessionTitleLlmMessageSelector,
    context: Context,
    config: SessionTitleLlmConfig,
}

#[async_trait]
impl SessionTitleProvider for LlmTitleProvider {
    fn id(&self) -> SessionTitleProviderId {
        self.id.clone()
    }

    fn automatic(&self) -> SessionTitleAutomaticMode {
        self.automatic
    }

    async fn generate(
        &self,
        request: SessionTitleProviderRequest,
    ) -> anyhow::Result<SessionTitleProviderResult> {
        let selected = (self.select_messages)(&request.messages)?;
        generate_session_title_with_llm(&self.context, &self.config, &request, &selected, &self.id)
            .await
    }
}

/// Registers one model-backed provider through the shared configuration and call policy.
///
/// # Errors
///
/// Returns missing-service, invalid-config, or duplicate-provider failures.
pub fn register_session_title_llm_provider(
    ctx: &Context,
    config: &SessionTitleLlmConfig,
    id: &str,
    automatic: SessionTitleAutomaticMode,
    select_messages: SessionTitleLlmMessageSelector,
) -> anyhow::Result<()> {
    let resolved = resolve_session_title_llm_config(config)?;
    let title_provider = SessionTitleProviderId::new(id);
    let service = ctx
        .get(SESSION_TITLE)
        .ok_or_else(|| anyhow::anyhow!("session-title-llm requires sessionTitle"))?;
    service.register(Arc::new(LlmTitleProvider {
        id: title_provider,
        automatic,
        select_messages,
        context: ctx.clone(),
        config: resolved,
    }))?;
    Ok(())
}

fn resolve_route(
    config: &SessionTitleLlmConfig,
    request: &SessionTitleProviderRequest,
) -> anyhow::Result<SessionTitleModelProvenance> {
    if let (Some(provider), Some(model)) = (&config.provider, &config.model) {
        return Ok(SessionTitleModelProvenance {
            provider: provider.clone(),
            model: model.clone(),
        });
    }
    request.route.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "session-title-llm: no logged request route is available; configure provider and model together"
        )
    })
}

fn system_prompt(config: &SessionTitleLlmConfig) -> String {
    [
        "Create a concise title for an AI coding-assistant session from the supplied human messages.".to_owned(),
        "Return only the title on one line, **in plain text of natural language**, with no quotes, prefix, explanation, Markdown, XML, or terminal control codes. No code is allowed.".to_owned(),
        "Use the language of the messages.".to_owned(),
        format!(
            "Aim for about {} words in non-CJK languages or {} CJK characters.",
            config.target_words, config.target_cjk_characters
        ),
    ]
    .join("
")
}

fn frame_messages(messages: &[SessionTitleUserMessage]) -> String {
    format!(
        "Generate the session title from this JSON array of human messages:
{}",
        serde_json::to_string(messages).expect("messages serialize")
    )
}

fn abort_error(signal: &AbortSignal) -> anyhow::Error {
    if let Some(timeout) = signal.typed_reason::<TimeoutReason>() {
        return anyhow::Error::new((*timeout).clone());
    }
    if let Some(reason) = signal
        .reason()
        .and_then(|value| value.as_str().map(str::to_owned))
    {
        return anyhow::Error::msg(reason);
    }
    anyhow::Error::msg("aborted")
}

fn finish_error(finish: &FinishReason) -> Option<anyhow::Error> {
    match finish {
        FinishReason::Stop => None,
        FinishReason::Error { failure } | FinishReason::Aborted { failure } => {
            Some(anyhow::Error::from(LlmError::simple(
                failure.message.clone(),
                failure.code.clone(),
            )))
        }
        FinishReason::MaxTokens => Some(anyhow::anyhow!(
            "session-title-llm: title output reached maxOutputTokens"
        )),
        FinishReason::ToolCalls => Some(anyhow::anyhow!(
            "session-title-llm: title model unexpectedly requested a tool"
        )),
        FinishReason::Unknown { kind, .. } => Some(anyhow::anyhow!(
            "session-title-llm: unsupported finish reason \"{kind}\""
        )),
    }
}

/// Generates one title through the shared auxiliary LLM call.
///
/// # Errors
///
/// Returns cancellation, empty-selection, oversize-input, route, stream,
/// assembly, or empty-title failures.
pub async fn generate_session_title_with_llm(
    ctx: &Context,
    config: &SessionTitleLlmConfig,
    request: &SessionTitleProviderRequest,
    selected_messages: &[SessionTitleUserMessage],
    title_provider: &SessionTitleProviderId,
) -> anyhow::Result<SessionTitleProviderResult> {
    if request.signal.is_aborted() {
        return Err(abort_error(&request.signal));
    }
    if selected_messages.is_empty() {
        anyhow::bail!("session-title-llm: at least one source message is required");
    }
    let framed_input = frame_messages(selected_messages);
    let input_bytes = framed_input.len();
    if input_bytes > config.max_input_bytes as usize {
        anyhow::bail!(
            "session-title-llm: input is {input_bytes} bytes, exceeding maxInputBytes {}",
            config.max_input_bytes
        );
    }
    let route = resolve_route(config, request)?;
    let system = system_prompt(config);
    let messages: Vec<Message> = vec![
        UserMessage::new(
            vec![ContentBlock::Text { text: framed_input }],
            MessageSource::plugin("seekdeep-session-title-llm"),
        )
        .into_message(),
    ];

    let mut call_deadline = deadline(
        Some(&request.signal),
        config.timeout_ms as f64,
        SESSION_TITLE_TIMEOUT_CODE,
    )?;
    let mut options = GenerateOptions::new(
        ProviderId::new(route.provider.clone()),
        ModelId::new(route.model.clone()),
        messages.clone(),
    );
    options.system = Some(system.clone());
    options.max_tokens = Some(config.max_output_tokens);
    options.session_id = Some(request.session.id().clone());
    options.purpose = Some(LlmRequestPurpose::SessionTitle);
    options.signal = Some(call_deadline.signal.clone());

    request.session.append(
        "session/title-llm-request",
        serde_json::json!({
            "titleProvider": title_provider.as_str(),
            "messageSeqs": selected_messages.iter().map(|m| m.seq).collect::<Vec<_>>(),
            "route": route,
            "system": system,
            "messages": messages,
            "maxTokens": config.max_output_tokens,
        }),
        AppendOptions::default(),
    )?;

    if call_deadline.signal.is_aborted() {
        return Err(abort_error(&call_deadline.signal));
    }
    let llm = ctx
        .get(LLM)
        .ok_or_else(|| anyhow::anyhow!("session-title-llm requires llm"))?;
    let mut assembler = BlockAssembler::new();
    let mut stream = llm.stream(options);
    while let Some(chunk) = stream.next().await {
        if call_deadline.signal.is_aborted() {
            return Err(abort_error(&call_deadline.signal));
        }
        assembler.push(chunk?);
    }
    if call_deadline.signal.is_aborted() {
        return Err(abort_error(&call_deadline.signal));
    }
    if let Some(error) = finish_error(&assembler.finish()) {
        call_deadline.dispose();
        return Err(error);
    }
    let blocks = assembler.blocks()?;
    call_deadline.dispose();
    if blocks
        .iter()
        .any(|block| matches!(block, ContentBlock::ToolCall { .. }))
    {
        anyhow::bail!("session-title-llm: title output must contain text only");
    }
    let text = blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ");
    let title = normalize_session_title(&text, usize::MAX);
    if title.is_empty() {
        anyhow::bail!("session-title-llm: title model produced no text");
    }
    Ok(SessionTitleProviderResult {
        title,
        message_seqs: selected_messages.iter().map(|m| m.seq).collect(),
        model: Some(route),
    })
}
