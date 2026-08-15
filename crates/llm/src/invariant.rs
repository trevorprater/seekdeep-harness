//! Incremental validation for the LLM stream grammar.

use std::collections::HashMap;

use async_stream::stream;
use futures::StreamExt;

use crate::{FinishReason, LlmStream, StreamChunk};

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Wraps one provider stream and rejects malformed chunk sequences as consumed.
#[must_use]
pub fn validate_stream(mut source: LlmStream) -> LlmStream {
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
                yield Err(anyhow::anyhow!(
                    "LLM stream emitted {} after terminal finish",
                    chunk_type(&chunk)
                ));
                return;
            }
            if let Err(error) = validate_chunk(&chunk, &mut open, &mut usage_seen, &mut finished) {
                yield Err(error);
                return;
            }
            yield Ok(chunk);
        }
        if !finished {
            yield Err(anyhow::anyhow!("LLM stream ended without a terminal finish chunk"));
        }
    })
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
    use futures::{StreamExt, stream};

    use super::*;
    use crate::ContentBlock;

    fn source(chunks: Vec<StreamChunk>) -> LlmStream {
        Box::pin(stream::iter(chunks.into_iter().map(Ok)))
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
}
