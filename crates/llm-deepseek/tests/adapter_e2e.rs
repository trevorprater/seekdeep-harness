//! Credential-gated live mirror of the source adapter E2E suite.
//!
//! These tests are ignored even when `DEEPSEEK_API_KEY` is present. Run them
//! deliberately with:
//! `cargo test -p seekdeep-llm-deepseek --test adapter_e2e -- --ignored`.

use std::{collections::BTreeMap, sync::Arc};

use futures::TryStreamExt as _;
use seekdeep_cordis::Context;
use seekdeep_credentials::{CREDENTIALS, credential_ref};
use seekdeep_credentials_local::{LocalCredentialConfig, install as install_credentials};
use seekdeep_llm::{
    BlockAssembler, CallId, ContentBlock, FinishReason, GenerateOptions, LlmRuntime, Message,
    MessageRole, MessageSource, ModelId, ProviderId, ReasoningEffortId, StreamChunk, TokenUsage,
    ToolSchema,
};
use seekdeep_llm_deepseek::{DeepSeekConfig, ReasoningEffort, install as install_deepseek};
use seekdeep_util::launch_environment::{
    LaunchEnvironmentLayerInput, LaunchEnvironmentSource, SEEKDEEP_LAUNCH_ENVIRONMENT,
    create_launch_environment_snapshot,
};
use serde_json::{Value, json};

const FLASH: &str = "deepseek-v4-flash";
const PRO: &str = "deepseek-v4-pro";

struct AssembledResult {
    message: Message,
    finish: FinishReason,
    usage: Option<TokenUsage>,
}

struct LiveHarness {
    context: Context,
    runtime: Arc<LlmRuntime>,
    _home: tempfile::TempDir,
}

impl LiveHarness {
    async fn open(
        config: DeepSeekConfig,
        key: &str,
        key_in_document: bool,
    ) -> anyhow::Result<Self> {
        let home = tempfile::tempdir()?;
        let environment_key = if key_in_document { "" } else { key };
        let snapshot = Arc::new(create_launch_environment_snapshot(&[
            LaunchEnvironmentLayerInput {
                source: LaunchEnvironmentSource::Process,
                path: None,
                values: BTreeMap::from([
                    (
                        "SEEKDEEP_HOME".to_owned(),
                        home.path().to_string_lossy().into_owned(),
                    ),
                    ("DEEPSEEK_API_KEY".to_owned(), environment_key.to_owned()),
                ]),
            },
        ]));
        let context = Context::new();
        context.provide(SEEKDEEP_LAUNCH_ENVIRONMENT, snapshot)?;
        let runtime = LlmRuntime::install(&context)?;
        if key_in_document {
            let credentials = install_credentials(
                &context,
                LocalCredentialConfig {
                    path: Some(home.path().join(".credentials.yaml")),
                    seekdeep_home: None,
                    watch: false,
                    debounce_ms: 0.0,
                },
            )?;
            credentials.await_settled().await?;
            context
                .get(CREDENTIALS)
                .expect("credentials service is installed")
                .set(&credential_ref("DEEPSEEK_API_KEY")?, key)
                .await?;
        }
        let provider = install_deepseek(&context, config)?;
        provider.await_settled().await?;
        Ok(Self {
            context,
            runtime,
            _home: home,
        })
    }

    async fn close(self) -> anyhow::Result<()> {
        self.context.fiber().dispose().await
    }

    async fn assemble(&self, options: GenerateOptions) -> anyhow::Result<AssembledResult> {
        let chunks = self.runtime.stream(options).try_collect::<Vec<_>>().await?;
        let mut assembler = BlockAssembler::new();
        for chunk in chunks {
            assembler.push(chunk);
        }
        Ok(AssembledResult {
            message: assembler.message(Some(MessageSource::plugin("deepseek-live-e2e")))?,
            finish: assembler.finish(),
            usage: assembler.usage().cloned(),
        })
    }
}

fn live_key() -> Option<String> {
    std::env::var("DEEPSEEK_API_KEY")
        .ok()
        .filter(|key| !key.is_empty())
}

fn ask(text: &str) -> Vec<Message> {
    vec![Message::user(
        vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        MessageSource::plugin("test"),
    )]
}

fn request(model: &str, messages: Vec<Message>, max_tokens: u64) -> GenerateOptions {
    let mut request = GenerateOptions::new(
        ProviderId::new("deepseek-official"),
        ModelId::new(model),
        messages,
    );
    request.max_tokens = Some(max_tokens);
    request
}

fn text_of(result: &AssembledResult) -> String {
    result
        .message
        .content()
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn has_reasoning(result: &AssembledResult) -> bool {
    result
        .message
        .content()
        .iter()
        .any(|block| matches!(block, ContentBlock::Reasoning { .. }))
}

fn weather_tool() -> ToolSchema {
    ToolSchema {
        name: "get_weather".to_owned(),
        description: "Get the current weather for a city.".to_owned(),
        parameters: serde_json::Map::from_iter([
            ("type".to_owned(), json!("object")),
            (
                "properties".to_owned(),
                json!({"city":{"type":"string","description":"City name"}}),
            ),
            ("required".to_owned(), json!(["city"])),
        ]),
    }
}

#[tokio::test]
#[ignore = "contacts the real DeepSeek API and consumes account quota"]
async fn real_request_resolves_the_key_only_from_credentials_local() -> anyhow::Result<()> {
    let Some(key) = live_key() else {
        return Ok(());
    };
    let harness = LiveHarness::open(DeepSeekConfig::default(), &key, true).await?;
    let result = harness
        .assemble(request(FLASH, ask("Reply with exactly the word: pong"), 50))
        .await?;
    assert_eq!(result.finish, FinishReason::Stop);
    assert!(text_of(&result).to_lowercase().contains("pong"));
    harness.close().await
}

#[tokio::test]
#[ignore = "contacts the real DeepSeek API and consumes account quota"]
async fn flash_switches_dynamically_from_off_to_high() -> anyhow::Result<()> {
    let Some(key) = live_key() else {
        return Ok(());
    };
    let harness = LiveHarness::open(
        DeepSeekConfig {
            reasoning_effort: Some(ReasoningEffort::Off),
            ..DeepSeekConfig::default()
        },
        &key,
        false,
    )
    .await?;
    let without = harness
        .assemble(request(FLASH, ask("Reply with exactly the word: pong"), 50))
        .await?;
    assert_eq!(without.finish, FinishReason::Stop);
    assert!(!has_reasoning(&without));
    let usage = without.usage.as_ref().expect("provider reports usage");
    assert!(usage.input_tokens > 0 && usage.output_tokens > 0);

    let mut with_request = request(
        FLASH,
        ask("Which is larger, 9.11 or 9.8? Answer with just the number."),
        2_000,
    );
    with_request.reasoning_effort = Some(ReasoningEffortId::new("high"));
    let with = harness.assemble(with_request).await?;
    assert_eq!(with.finish, FinishReason::Stop);
    assert!(has_reasoning(&with));
    assert!(text_of(&with).contains("9.8"));
    assert!(
        with.usage
            .is_some_and(|usage| usage.reasoning_tokens > Some(0))
    );
    harness.close().await
}

async fn pro_tool_round_trip(effort: &str) -> anyhow::Result<()> {
    let Some(key) = live_key() else {
        return Ok(());
    };
    let harness = LiveHarness::open(
        DeepSeekConfig {
            thinking: Some(seekdeep_llm_deepseek::types::ThinkingMode::Enabled),
            ..DeepSeekConfig::default()
        },
        &key,
        false,
    )
    .await?;
    let question = "What is the weather in Paris right now? Use the get_weather tool.";
    let mut first_request = request(PRO, ask(question), 2_000);
    first_request.reasoning_effort = Some(ReasoningEffortId::new(effort));
    first_request.tools = Some(vec![weather_tool()]);
    let first = harness.assemble(first_request).await?;
    assert_eq!(first.finish, FinishReason::ToolCalls);
    let (id, name, arguments) = first
        .message
        .content()
        .iter()
        .find_map(|block| match block {
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
            } => Some((id.clone(), name.clone(), arguments.clone())),
            _ => None,
        })
        .expect("model emitted a tool call");
    assert_eq!(name, "get_weather");
    let parsed: Value = serde_json::from_str(&arguments)?;
    assert!(
        parsed["city"]
            .as_str()
            .is_some_and(|city| city.to_lowercase().contains("paris"))
    );

    let mut history = ask(question);
    history.push(Message::new(
        MessageRole::Assistant,
        first.message.content().to_vec(),
        MessageSource::plugin("test"),
    ));
    history.push(Message::tool_result(
        &CallId::new(id.as_str()),
        vec![ContentBlock::Text {
            text: "Sunny, 22°C".to_owned(),
        }],
        false,
    ));
    let mut second_request = request(PRO, history, 2_000);
    second_request.reasoning_effort = Some(ReasoningEffortId::new(effort));
    second_request.tools = Some(vec![weather_tool()]);
    let second = harness.assemble(second_request).await?;
    assert_eq!(second.finish, FinishReason::Stop);
    let answer = text_of(&second).to_lowercase();
    assert!(answer.contains("sunny") || answer.contains("22"));
    harness.close().await
}

#[tokio::test]
#[ignore = "contacts the real DeepSeek API and consumes account quota"]
async fn pro_high_tool_round_trip_preserves_reasoning_passback() -> anyhow::Result<()> {
    pro_tool_round_trip("high").await
}

#[tokio::test]
#[ignore = "contacts the real DeepSeek API and consumes account quota"]
async fn pro_max_tool_round_trip_preserves_reasoning_passback() -> anyhow::Result<()> {
    pro_tool_round_trip("max").await
}

#[tokio::test]
#[ignore = "contacts the real DeepSeek API and consumes account quota"]
async fn pro_disabled_thinking_has_no_reasoning_blocks() -> anyhow::Result<()> {
    let Some(key) = live_key() else {
        return Ok(());
    };
    let harness = LiveHarness::open(
        DeepSeekConfig {
            thinking: Some(seekdeep_llm_deepseek::types::ThinkingMode::Disabled),
            ..DeepSeekConfig::default()
        },
        &key,
        false,
    )
    .await?;
    let result = harness
        .assemble(request(PRO, ask("Reply with exactly the word: pong"), 50))
        .await?;
    assert_eq!(result.finish, FinishReason::Stop);
    assert!(!has_reasoning(&result));
    harness.close().await
}

#[tokio::test]
#[ignore = "contacts the real DeepSeek API and consumes account quota"]
async fn raw_chunks_keep_usage_before_the_single_finish() -> anyhow::Result<()> {
    let Some(key) = live_key() else {
        return Ok(());
    };
    let harness = LiveHarness::open(
        DeepSeekConfig {
            thinking: Some(seekdeep_llm_deepseek::types::ThinkingMode::Disabled),
            ..DeepSeekConfig::default()
        },
        &key,
        false,
    )
    .await?;
    let chunks = harness
        .runtime
        .stream(request(FLASH, ask("Count from 1 to 5, digits only."), 50))
        .try_collect::<Vec<_>>()
        .await?;
    assert!(matches!(
        chunks.first(),
        Some(StreamChunk::BlockStart { .. })
    ));
    assert!(matches!(chunks.last(), Some(StreamChunk::Finish { .. })));
    assert_eq!(
        chunks
            .iter()
            .filter(|chunk| matches!(chunk, StreamChunk::Finish { .. }))
            .count(),
        1
    );
    let usage = chunks
        .iter()
        .position(|chunk| matches!(chunk, StreamChunk::Usage { .. }))
        .expect("provider emitted usage");
    let finish = chunks
        .iter()
        .position(|chunk| matches!(chunk, StreamChunk::Finish { .. }))
        .expect("provider emitted finish");
    assert!(usage < finish);
    harness.close().await
}
