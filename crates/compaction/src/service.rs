//! Compaction service-definition vocabulary: triggers, classified manual
//! failures, the agent context backends consume, and the service seat.

use std::{ops::Deref, sync::Arc};

use async_trait::async_trait;
use futures::future::BoxFuture;
use seekdeep_commands::CommandId;
use seekdeep_cordis::{Context, ServiceKey, fiber::EffectHandle};
use seekdeep_core::session::Session;
use seekdeep_llm::AbortSignal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::CompactionResult;

/// Why automatic policy is asking a backend to consider compaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CompactionTrigger {
    /// Normal pressure.
    Pressure,
    /// Provider-confirmed context overflow.
    ContextOverflow,
}

/// Expected failure classes for an explicit idle-session compaction request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ManualCompactionErrorCode {
    /// Another compaction owns the session lock.
    Busy,
    /// The agent or request was cancelled.
    Cancelled,
    /// The selected span changed under the summary.
    Changed,
    /// Summarization or shrink failed.
    Summary,
    /// The commit stage failed.
    Commit,
    /// Persistence failed.
    Persistence,
}

/// Expected manual-compaction failure suitable for a direct human-command result.
#[derive(Clone, Debug, Error)]
#[error("{message}")]
pub struct ManualCompactionError {
    /// Stable failure class.
    pub code: ManualCompactionErrorCode,
    /// Backend diagnostic retained as the error message.
    pub message: String,
}

impl ManualCompactionError {
    /// Creates one classified compaction failure.
    #[must_use]
    pub fn new(code: ManualCompactionErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Routing options guiding a backend's summarization call.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionRoutingOptions {
    /// Provider route override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Model override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Minimal agent context compaction needs without depending on the agent package.
#[derive(Clone, Debug)]
pub struct CompactionAgentContext {
    /// Session whose surface is compacted.
    pub session: Arc<Session>,
    /// Routing options for the summary call.
    pub options: CompactionRoutingOptions,
}

/// A non-turn maintenance task that produces one compaction result.
pub type MaintenanceTask = Box<
    dyn FnOnce(AbortSignal) -> BoxFuture<'static, anyhow::Result<Option<CompactionResult>>> + Send,
>;

/// Idle-only maintenance runner; a busy agent rejects with an immediate error future.
pub type MaintenanceRunner = Arc<
    dyn Fn(MaintenanceTask) -> BoxFuture<'static, anyhow::Result<Option<CompactionResult>>>
        + Send
        + Sync,
>;

/// Idle-agent context for an explicit compaction request.
#[derive(Clone)]
pub struct ManualCompactAgentContext {
    /// Session whose durable history is compacted.
    pub session: Arc<Session>,
    /// Routing options for the summary call.
    pub options: CompactionRoutingOptions,
    /// Runs the compaction task only while the agent is idle.
    pub run_maintenance: MaintenanceRunner,
}

/// Abstract compaction service; backends own trigger policy, retention, and summarization.
#[async_trait]
pub trait CompactionEngine: Send + Sync + 'static {
    /// Considers automatic compaction for one explicit trigger.
    async fn compact_if_needed(
        &self,
        agent: &CompactionAgentContext,
        trigger: CompactionTrigger,
        signal: &AbortSignal,
    ) -> anyhow::Result<Option<CompactionResult>>;

    /// Explicitly compacts useful history even below automatic pressure thresholds.
    async fn compact_now(
        &self,
        agent: &ManualCompactAgentContext,
        signal: &AbortSignal,
        source_command_id: Option<&CommandId>,
    ) -> anyhow::Result<Option<CompactionResult>>;

    /// Forcibly compacts an inclusive surface span into one summary node.
    async fn compact_region(
        &self,
        start: u64,
        end: u64,
        agent: &CompactionAgentContext,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<CompactionResult>;
}

/// Typed Cordis seat corresponding to `ctx.compaction`.
pub const COMPACTION: ServiceKey<CompactionService> = ServiceKey::new("compaction");

/// Dynamically dispatched exact backend occupying the compaction service seat.
#[derive(Clone)]
pub struct CompactionService(Arc<dyn CompactionEngine>);

impl std::fmt::Debug for CompactionService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("CompactionService")
            .field(&"dyn CompactionEngine")
            .finish()
    }
}

impl CompactionService {
    /// Wraps one concrete engine.
    #[must_use]
    pub fn new(engine: Arc<dyn CompactionEngine>) -> Arc<Self> {
        Arc::new(Self(engine))
    }

    /// Returns the object-safe engine.
    #[must_use]
    pub fn engine(&self) -> Arc<dyn CompactionEngine> {
        self.0.clone()
    }

    /// Publishes this backend on the source-compatible Cordis seat.
    ///
    /// # Errors
    ///
    /// Returns inactive-fiber or duplicate-service failures.
    pub fn provide(self: &Arc<Self>, context: &Context) -> anyhow::Result<EffectHandle> {
        Ok(context.provide(COMPACTION, self.clone())?)
    }
}

impl Deref for CompactionService {
    type Target = dyn CompactionEngine;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_and_error_code_round_trip() {
        assert_eq!(
            serde_json::to_string(&CompactionTrigger::ContextOverflow).expect("trigger"),
            "\"context-overflow\""
        );
        assert_eq!(
            serde_json::to_string(&ManualCompactionErrorCode::Persistence).expect("code"),
            "\"persistence\""
        );
    }

    #[test]
    fn manual_compaction_error_carries_code_and_message() {
        let error = ManualCompactionError::new(ManualCompactionErrorCode::Busy, "already running");
        assert_eq!(error.code, ManualCompactionErrorCode::Busy);
        assert_eq!(error.message, "already running");
        assert_eq!(error.to_string(), "already running");
    }

    #[derive(Debug)]
    struct MockEngine;

    #[async_trait]
    impl CompactionEngine for MockEngine {
        async fn compact_if_needed(
            &self,
            _agent: &CompactionAgentContext,
            _trigger: CompactionTrigger,
            _signal: &AbortSignal,
        ) -> anyhow::Result<Option<CompactionResult>> {
            Ok(None)
        }

        async fn compact_now(
            &self,
            _agent: &ManualCompactAgentContext,
            _signal: &AbortSignal,
            _source_command_id: Option<&CommandId>,
        ) -> anyhow::Result<Option<CompactionResult>> {
            Ok(None)
        }

        async fn compact_region(
            &self,
            _start: u64,
            _end: u64,
            _agent: &CompactionAgentContext,
            _signal: Option<&AbortSignal>,
        ) -> anyhow::Result<CompactionResult> {
            anyhow::bail!("unreachable")
        }
    }

    #[tokio::test]
    async fn service_seat_round_trips_the_engine() {
        let context = Context::new();
        let service = CompactionService::new(Arc::new(MockEngine));
        service.provide(&context).expect("provide");
        let seat = context.get(COMPACTION).expect("seat");
        let agent = CompactionAgentContext {
            session: seekdeep_core::session::Session::create(
                &seekdeep_llm::SessionId::new("s"),
                None,
                None,
            )
            .expect("session"),
            options: CompactionRoutingOptions::default(),
        };
        assert_eq!(
            seat.compact_if_needed(&agent, CompactionTrigger::Pressure, &AbortSignal::default())
                .await
                .expect("compact"),
            None
        );
    }
}
