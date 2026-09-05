//! Incremental validation for the LLM stream grammar.

use std::{collections::HashMap, sync::Arc};

use async_stream::stream;
use futures::StreamExt;
use seekdeep_cordis::{EventOptions, EventReply};
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};

use crate::{FinishReason, LLM, LlmStream, LlmStreamMiddleware, StreamChunk};

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const PACKAGE_NAME: &str = "@deepseek-ai/seekdeep-llm";

/// Stable invariant companion name.
pub const INVARIANT_NAME: &str = "llm-invariant";

/// Registers the package-owned stream and topology invariant companion.
///
/// The installer waits for the LLM service, then owns both checks in its child
/// fiber so disabling or disposing the companion removes them together.
///
/// # Errors
///
/// Returns ordinary invariant-registry reservation or lifecycle failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(
        PACKAGE_NAME,
        InvariantInstaller::new(["llm"], |context, failure| async move {
            let runtime = context
                .get(LLM)
                .ok_or_else(|| anyhow::anyhow!("llm invariant activated without llm service"))?;

            let stream_failure = failure.clone();
            let middleware: LlmStreamMiddleware = Arc::new(move |options, next| {
                validate_stream_with(next(options), stream_failure.clone())
            });
            runtime.register_stream_middleware(&context, middleware, true)?;

            let listener_context = context.clone();
            context.events().on_sync(
                &context,
                "llm/adapters-updated",
                move |_, _| {
                    let Some(runtime) = listener_context.get(LLM) else {
                        return Ok(EventReply::Undefined);
                    };
                    for provider in runtime.list_providers() {
                        if runtime.provider_retry_policy(&provider.id).is_err() {
                            return Err(failure
                                .fail(format!(
                                    "llm/adapters-updated fired while provider \"{}\" has no readable registration",
                                    provider.id
                                ))
                                .into());
                        }
                    }
                    Ok(EventReply::Undefined)
                },
                EventOptions {
                    global: true,
                    ..EventOptions::default()
                },
            )?;
            Ok(())
        }),
    )
}

/// Wraps one provider stream and rejects malformed chunk sequences as consumed.
#[must_use]
pub fn validate_stream(source: LlmStream) -> LlmStream {
    validate_stream_inner(source, None)
}

fn validate_stream_with(
    source: LlmStream,
    failure: seekdeep_invariants::InvariantFailure,
) -> LlmStream {
    validate_stream_inner(source, Some(failure))
}

fn validate_stream_inner(
    source: LlmStream,
    failure: Option<seekdeep_invariants::InvariantFailure>,
) -> LlmStream {
    source.wrap(move |mut source| {
        Box::pin(stream! {
            let mut open = HashMap::<u64, String>::new();
            let mut usage_seen = false;
            let mut finished = false;
            while let Some(item) = source.next().await {
                let chunk = match item {
                    Ok(chunk) => chunk,
                    Err(error) => {
                        yield Err(error);
                        return;
                    }
                };
                if finished {
                    let message = format!(
                        "LLM stream emitted {} after terminal finish", chunk_type(&chunk)
                    );
                    yield Err(invariant_error(failure.as_ref(), message));
                    return;
                }
                if let Err(error) = validate_chunk(&chunk, &mut open, &mut usage_seen, &mut finished) {
                    yield Err(invariant_error(failure.as_ref(), error.to_string()));
                    return;
                }
                yield Ok(chunk);
            }
            if !finished {
                yield Err(invariant_error(
                    failure.as_ref(),
                    "LLM stream ended without a terminal finish chunk",
                ));
            }
        })
    })
}

fn invariant_error(
    failure: Option<&seekdeep_invariants::InvariantFailure>,
    message: impl Into<String>,
) -> anyhow::Error {
    let message = message.into();
    match failure {
        Some(failure) => failure.fail(message).into(),
        None => anyhow::anyhow!(message),
    }
}

fn validate_chunk(
    chunk: &StreamChunk,
    open: &mut HashMap<u64, String>,
    usage_seen: &mut bool,
    finished: &mut bool,
) -> anyhow::Result<()> {
    match chunk {
        StreamChunk::BlockStart { index, block_type } => {
            validate_index(*index)?;
            anyhow::ensure!(
                !open.contains_key(index),
                "LLM stream repeated block-start index {index}"
            );
            open.insert(*index, block_type.clone());
        }
        StreamChunk::TextDelta { index, .. } => validate_delta(open, *index, "text")?,
        StreamChunk::ReasoningDelta { index, .. } => {
            validate_delta(open, *index, "reasoning")?;
        }
        StreamChunk::ToolCallDelta { index, .. } => {
            validate_delta(open, *index, "tool-call")?;
        }
        StreamChunk::BlockEnd { index, block } => {
            validate_index(*index)?;
            let expected = open.get(index).ok_or_else(|| {
                anyhow::anyhow!("LLM stream block-end index {index} has no open block")
            })?;
            anyhow::ensure!(
                block.block_type() == expected,
                "LLM stream block-end index {index} closes {}, expected {expected}",
                block.block_type()
            );
            open.remove(index);
        }
        StreamChunk::Usage { .. } => {
            anyhow::ensure!(!*usage_seen, "LLM stream emitted usage more than once");
            *usage_seen = true;
        }
        StreamChunk::Finish { reason, .. } => {
            let failure_finish = matches!(
                reason,
                FinishReason::Error { .. } | FinishReason::Aborted { .. }
            );
            anyhow::ensure!(
                open.is_empty() || failure_finish,
                "LLM stream finished with {} open block(s)",
                open.len()
            );
            *finished = true;
        }
    }
    Ok(())
}

fn validate_index(index: u64) -> anyhow::Result<()> {
    anyhow::ensure!(
        index <= MAX_SAFE_INTEGER,
        "LLM stream block index must be a non-negative safe integer, got {index}"
    );
    Ok(())
}

fn validate_delta(open: &HashMap<u64, String>, index: u64, expected: &str) -> anyhow::Result<()> {
    validate_index(index)?;
    let actual = open.get(&index).map_or("undefined", String::as_str);
    anyhow::ensure!(
        actual == expected,
        "{expected} delta at index {index} requires an open {expected} block, got {actual}"
    );
    Ok(())
}

fn chunk_type(chunk: &StreamChunk) -> &'static str {
    match chunk {
        StreamChunk::BlockStart { .. } => "block-start",
        StreamChunk::TextDelta { .. } => "text-delta",
        StreamChunk::ReasoningDelta { .. } => "reasoning-delta",
        StreamChunk::ToolCallDelta { .. } => "tool-call-delta",
        StreamChunk::BlockEnd { .. } => "block-end",
        StreamChunk::Usage { .. } => "usage",
        StreamChunk::Finish { .. } => "finish",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use futures::{StreamExt, stream};

    use super::*;
    use crate::ContentBlock;

    fn source(chunks: Vec<StreamChunk>) -> LlmStream {
        LlmStream::new(stream::iter(chunks.into_iter().map(Ok)))
    }

    #[tokio::test]
    async fn accepts_complete_interleaved_grammar() {
        let chunks = vec![
            StreamChunk::BlockStart {
                index: 0,
                block_type: "text".to_owned(),
            },
            StreamChunk::TextDelta {
                index: 0,
                text: "a".to_owned(),
            },
            StreamChunk::BlockEnd {
                index: 0,
                block: ContentBlock::Text {
                    text: "a".to_owned(),
                },
            },
            StreamChunk::Finish {
                reason: FinishReason::Stop,
                replay_state: None,
            },
        ];
        let output = validate_stream(source(chunks.clone()))
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<anyhow::Result<Vec<_>>>()
            .expect("valid");
        assert_eq!(output, chunks);
    }

    #[tokio::test]
    async fn rejects_missing_finish_and_mismatched_delta() {
        let missing = validate_stream(source(Vec::new()))
            .collect::<Vec<_>>()
            .await;
        assert!(
            missing[0]
                .as_ref()
                .is_err_and(|error| error.to_string().contains("without a terminal finish"))
        );
        let mismatch = validate_stream(source(vec![StreamChunk::TextDelta {
            index: 0,
            text: "x".to_owned(),
        }]))
        .collect::<Vec<_>>()
        .await;
        assert!(
            mismatch[0]
                .as_ref()
                .is_err_and(|error| error.to_string().contains("requires an open text block"))
        );
    }

    async fn first_error(chunks: Vec<StreamChunk>) -> String {
        let mut validated = validate_stream(source(chunks));
        while let Some(item) = validated.next().await {
            if let Err(error) = item {
                return error.to_string();
            }
        }
        panic!("expected invariant failure")
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn rejects_the_complete_source_malformed_grammar_matrix() {
        let finish = StreamChunk::Finish {
            reason: FinishReason::Stop,
            replay_state: None,
        };
        let cases = vec![
            (
                vec![
                    StreamChunk::BlockStart {
                        index: MAX_SAFE_INTEGER + 1,
                        block_type: "text".to_owned(),
                    },
                    finish.clone(),
                ],
                "non-negative safe integer",
            ),
            (
                vec![
                    StreamChunk::BlockStart {
                        index: 0,
                        block_type: "text".to_owned(),
                    },
                    StreamChunk::BlockStart {
                        index: 0,
                        block_type: "text".to_owned(),
                    },
                ],
                "repeated block-start",
            ),
            (
                vec![StreamChunk::TextDelta {
                    index: 0,
                    text: "x".to_owned(),
                }],
                "requires an open text block",
            ),
            (
                vec![
                    StreamChunk::BlockStart {
                        index: 0,
                        block_type: "reasoning".to_owned(),
                    },
                    StreamChunk::TextDelta {
                        index: 0,
                        text: "x".to_owned(),
                    },
                ],
                "got reasoning",
            ),
            (
                vec![StreamChunk::BlockEnd {
                    index: 0,
                    block: ContentBlock::Text {
                        text: String::new(),
                    },
                }],
                "has no open block",
            ),
            (
                vec![
                    StreamChunk::BlockStart {
                        index: 0,
                        block_type: "text".to_owned(),
                    },
                    StreamChunk::BlockEnd {
                        index: 0,
                        block: ContentBlock::Reasoning {
                            text: String::new(),
                        },
                    },
                ],
                "closes reasoning, expected text",
            ),
            (
                vec![
                    StreamChunk::Usage {
                        usage: crate::TokenUsage {
                            input_tokens: 0,
                            output_tokens: 0,
                            cache_read_tokens: None,
                            cache_write_tokens: None,
                            reasoning_tokens: None,
                        },
                    },
                    StreamChunk::Usage {
                        usage: crate::TokenUsage {
                            input_tokens: 0,
                            output_tokens: 0,
                            cache_read_tokens: None,
                            cache_write_tokens: None,
                            reasoning_tokens: None,
                        },
                    },
                ],
                "usage more than once",
            ),
            (
                vec![
                    StreamChunk::BlockStart {
                        index: 0,
                        block_type: "text".to_owned(),
                    },
                    finish.clone(),
                ],
                "finished with 1 open block",
            ),
            (
                vec![
                    finish,
                    StreamChunk::Usage {
                        usage: crate::TokenUsage {
                            input_tokens: 0,
                            output_tokens: 0,
                            cache_read_tokens: None,
                            cache_write_tokens: None,
                            reasoning_tokens: None,
                        },
                    },
                ],
                "usage after terminal finish",
            ),
            (Vec::new(), "ended without a terminal finish"),
        ];
        for (chunks, expected) in cases {
            assert!(first_error(chunks).await.contains(expected), "{expected}");
        }
    }

    #[tokio::test]
    async fn provider_failure_is_forwarded_without_a_synthetic_missing_finish() {
        let mut validated = validate_stream(LlmStream::new(stream::iter([Err(anyhow::anyhow!(
            "provider failed"
        ))])));
        assert_eq!(
            validated
                .next()
                .await
                .expect("error item")
                .expect_err("provider error")
                .to_string(),
            "provider failed"
        );
        assert!(validated.next().await.is_none());
    }

    #[derive(Debug)]
    struct EmptyAdapter;

    #[async_trait::async_trait]
    impl crate::LlmAdapter for EmptyAdapter {
        fn stream(&self, _options: crate::GenerateOptions) -> crate::AdapterStream {
            crate::AdapterStream::new(stream::empty())
        }
    }

    #[tokio::test]
    async fn registry_companion_activation_and_disposal_own_the_stream_check() {
        let context = seekdeep_cordis::Context::new();
        let invariants =
            InvariantRegistry::install(&context, &seekdeep_invariants::InvariantConfig::default())
                .expect("registry");
        let companion = register_invariant(&invariants).expect("companion");
        let runtime = crate::LlmRuntime::install(&context).expect("runtime");
        companion.await_ready().await.expect("activate");
        runtime
            .register_adapter(&["empty".to_owned()], Arc::new(EmptyAdapter))
            .expect("adapter");
        let options = crate::GenerateOptions::new(
            crate::ProviderId::new("empty"),
            crate::ModelId::new("m"),
            Vec::new(),
        );

        let error = runtime
            .stream(options.clone())
            .next()
            .await
            .expect("invariant item")
            .expect_err("missing finish");
        assert!(
            error
                .downcast_ref::<seekdeep_invariants::InvariantError>()
                .is_some_and(|error| error.code == "INVARIANT")
        );

        companion.dispose().await.expect("dispose companion");
        assert!(runtime.stream(options).next().await.is_none());
    }
}
