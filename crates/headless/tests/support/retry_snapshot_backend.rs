//! Source retry fixture adapter with request-identity checking.

use std::sync::Arc;

use async_trait::async_trait;
use futures::stream;
use parking_lot::Mutex;
use seekdeep_cordis::{Plugin, ServiceKey, fiber::EffectHandle};
use seekdeep_llm::{
    AdapterStream, ContentBlock, FinishReason, GenerateOptions, LLM, LlmAdapter, LlmError,
    ResolvedRetryPolicy, StreamChunk, TokenUsage, resolve_retry_policy,
};
use serde_json::json;

pub(super) const PROBE: ServiceKey<RetrySnapshotAdapter> = ServiceKey::new("headlessRetryProbe");

pub(super) struct RetrySnapshotAdapter {
    requests: Mutex<Vec<String>>,
    policy: ResolvedRetryPolicy,
}

impl RetrySnapshotAdapter {
    pub(super) fn assert_consumed(&self) -> anyhow::Result<()> {
        let requests = self.requests.lock().len();
        anyhow::ensure!(
            requests == 2,
            "retry fixture consumed {requests} requests, expected 2"
        );
        Ok(())
    }
}

#[async_trait]
impl LlmAdapter for RetrySnapshotAdapter {
    fn provider_retry_policy(&self, _provider: &str) -> Option<ResolvedRetryPolicy> {
        Some(self.policy.clone())
    }

    fn stream(&self, options: GenerateOptions) -> AdapterStream {
        let messages = serde_json::to_string(&options.messages).unwrap();
        let mut requests = self.requests.lock();
        requests.push(messages.clone());
        if requests.len() == 1 {
            return AdapterStream::new(stream::iter([Err(LlmError::new(
                "snapshot transient failure",
                "RATE_LIMIT",
                Some(429),
                None,
                None,
            )
            .unwrap()
            .into())]));
        }
        if requests.len() == 2 && requests[0] != messages {
            return AdapterStream::new(stream::iter([Err(anyhow::anyhow!(
                "retry snapshot changed the model-visible messages"
            ))]));
        }
        AdapterStream::new(stream::iter(
            [
                StreamChunk::BlockStart {
                    index: 0,
                    block_type: "text".to_owned(),
                },
                StreamChunk::TextDelta {
                    index: 0,
                    text: "RETRY_OK".to_owned(),
                },
                StreamChunk::BlockEnd {
                    index: 0,
                    block: ContentBlock::Text {
                        text: "RETRY_OK".to_owned(),
                    },
                },
                StreamChunk::Usage {
                    usage: TokenUsage {
                        input_tokens: 4,
                        output_tokens: 2,
                        cache_read_tokens: None,
                        cache_write_tokens: None,
                        reasoning_tokens: None,
                    },
                },
                StreamChunk::Finish {
                    reason: FinishReason::Stop,
                    replay_state: None,
                },
            ]
            .into_iter()
            .map(Ok),
        ))
    }
}

pub(super) fn plugin() -> Plugin {
    Plugin::new("retry-snapshot-backend", ["llm"], |context, _| {
        Box::pin(async move {
            let adapter = Arc::new(RetrySnapshotAdapter {
                requests: Mutex::new(Vec::new()),
                policy: resolve_retry_policy(
                    Some(&json!({
                        "mode":"normal", "maxRetries":1, "retryableCodes":["RATE_LIMIT"],
                        "backoff":{"initialDelayMs":1,"maxDelayMs":1,"jitterRatio":0}
                    })),
                    "retry-snapshot-backend.retryPolicy",
                )?,
            });
            let registration = Arc::new(
                context
                    .get(LLM)
                    .unwrap()
                    .register_adapter(&["deepseek-official".to_owned()], adapter.clone())?,
            );
            context.provide(PROBE, adapter)?;
            context.own(EffectHandle::new("retry fixture adapter", move || {
                Box::pin(async move { registration.dispose().await })
            }))?;
            Ok(())
        })
    })
}
