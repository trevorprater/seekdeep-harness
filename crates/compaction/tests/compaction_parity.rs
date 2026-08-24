//! Behavioral mirror of the `CompactionEngine` seam source suite.

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use seekdeep_compaction::{
    CompactionId, CompactionResult, ShadowedRange, compact_checkpoint_source,
    is_compact_checkpoint_source,
    service::{
        COMPACTION, CompactionAgentContext, CompactionEngine, CompactionRoutingOptions,
        CompactionService, CompactionTrigger, ManualCompactAgentContext,
    },
};
use seekdeep_cordis::{Context, Fiber};
use seekdeep_core::session::{AppendOptions, Session, SessionId, SurfaceOp};
use seekdeep_llm::{AbortSignal, ContentBlock, Message, MessageSource};
use serde_json::json;

#[derive(Default)]
struct StubEngine {
    last_signal: Mutex<Option<AbortSignal>>,
}

impl StubEngine {
    fn agent(session: Arc<Session>, model: Option<&str>) -> CompactionAgentContext {
        CompactionAgentContext {
            session,
            options: CompactionRoutingOptions {
                model: model.map(str::to_owned),
                ..CompactionRoutingOptions::default()
            },
        }
    }

    fn record(&self, signal: Option<&AbortSignal>) {
        *self.last_signal.lock() = signal.cloned();
    }
}

#[async_trait]
impl CompactionEngine for StubEngine {
    async fn compact_if_needed(
        &self,
        _agent: &CompactionAgentContext,
        _trigger: CompactionTrigger,
        signal: &AbortSignal,
    ) -> anyhow::Result<Option<CompactionResult>> {
        self.record(Some(signal));
        Ok(None)
    }

    async fn compact_now(
        &self,
        _agent: &ManualCompactAgentContext,
        signal: &AbortSignal,
        _source_command_id: Option<&seekdeep_commands::CommandId>,
    ) -> anyhow::Result<Option<CompactionResult>> {
        self.record(Some(signal));
        Ok(None)
    }

    async fn compact_region(
        &self,
        start: u64,
        end: u64,
        agent: &CompactionAgentContext,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<CompactionResult> {
        self.record(signal);
        let surface = agent.session.surface_nodes();
        let start_index = surface
            .iter()
            .position(|seq| *seq == start)
            .ok_or_else(|| anyhow::anyhow!("stub compact range is invalid"))?;
        let end_index = surface
            .iter()
            .position(|seq| *seq == end)
            .filter(|end_index| *end_index >= start_index)
            .ok_or_else(|| anyhow::anyhow!("stub compact range is invalid"))?;
        let shadowed_seqs = surface[start_index..=end_index].to_vec();
        let compaction_id = CompactionId::new("stub-compaction");
        let summary = vec![ContentBlock::Text {
            text: "stub".to_owned(),
        }];
        let start_event = agent.session.append(
            "compaction/start",
            json!({"compactionId": compaction_id, "turn": 0}),
            AppendOptions::default(),
        )?;
        let summary_event = agent.session.append(
            "compaction/summary",
            json!({
                "compactionId": compaction_id,
                "summary": summary,
                "shadowedRange": {"start": start, "end": end},
                "shadowedSeqs": shadowed_seqs,
                "shadowedTokenCount": 0,
                "provider": "mock",
                "model": "stub"
            }),
            AppendOptions::default(),
        )?;
        let checkpoint = Message::user(
            summary.clone(),
            compact_checkpoint_source(&compaction_id, None),
        );
        agent.session.append(
            "user/message",
            serde_json::to_value(checkpoint)?,
            AppendOptions {
                surface_op: Some(SurfaceOp::replace(start, end)),
                source_event_seqs: Some(
                    [start_event.seq, summary_event.seq]
                        .into_iter()
                        .chain(shadowed_seqs.iter().copied())
                        .collect(),
                ),
                ..AppendOptions::default()
            },
        )?;
        let end_event = agent.session.append(
            "compaction/end",
            json!({"compactionId": compaction_id, "turn": 0}),
            AppendOptions::default(),
        )?;
        Ok(CompactionResult {
            compaction_id,
            source_command_id: None,
            start_seq: start_event.seq,
            summary_seq: summary_event.seq,
            end_seq: end_event.seq,
            summary,
            shadowed_range: ShadowedRange { start, end },
            shadowed_seqs,
            shadowed_token_count: 0,
        })
    }
}

#[tokio::test]
async fn service_registration_contract_methods_and_disposal_are_exact() {
    let context = Context::new();
    let fiber = Fiber::active_child("compaction-stub");
    let child = context.with_fiber(fiber.clone());
    let engine = Arc::new(StubEngine::default());
    CompactionService::new(engine.clone())
        .provide(&child)
        .unwrap();
    let service = context.get(COMPACTION).unwrap();
    let session = Session::create(&SessionId::new("compaction-seam"), None, None).unwrap();
    let agent = StubEngine::agent(session.clone(), None);
    let signal = AbortSignal::default();
    assert_eq!(
        service
            .compact_if_needed(&agent, CompactionTrigger::Pressure, &signal)
            .await
            .unwrap(),
        None
    );
    let manual = ManualCompactAgentContext {
        session,
        options: CompactionRoutingOptions::default(),
        run_maintenance: Arc::new(|task| task(AbortSignal::default())),
    };
    assert_eq!(
        service.compact_now(&manual, &signal, None).await.unwrap(),
        None
    );
    signal.abort();
    assert!(engine.last_signal.lock().as_ref().unwrap().is_aborted());
    fiber.dispose().await.unwrap();
    assert!(context.get(COMPACTION).is_none());
}

#[tokio::test]
async fn region_records_log_only_lifecycle_checkpoint_and_signal_provenance() {
    let engine = StubEngine::default();
    let session = Session::create(&SessionId::new("compaction-region"), None, None).unwrap();
    let original = Message::user(
        vec![ContentBlock::Text {
            text: "original".to_owned(),
        }],
        MessageSource::user(),
    );
    let original = session
        .append(
            "user/message",
            serde_json::to_value(original).unwrap(),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .unwrap();
    let signal = AbortSignal::default();
    let result = engine
        .compact_region(
            original.seq,
            original.seq,
            &StubEngine::agent(session.clone(), Some("m")),
            Some(&signal),
        )
        .await
        .unwrap();
    signal.abort();
    assert!(engine.last_signal.lock().as_ref().unwrap().is_aborted());
    assert_eq!(
        result.summary,
        [ContentBlock::Text {
            text: "stub".into()
        }]
    );
    assert!(result.start_seq < result.summary_seq && result.summary_seq < result.end_seq);
    assert_eq!(result.shadowed_range, ShadowedRange { start: 0, end: 0 });
    assert_eq!(result.shadowed_seqs, [0]);
    let events = session.events();
    assert!(
        events
            .iter()
            .filter(|event| event.event_type.starts_with("compaction/"))
            .all(|event| event.surface_op.is_none())
    );
    let source = events
        .iter()
        .find(|event| event.event_type == "user/message" && event.seq != original.seq)
        .and_then(|event| serde_json::from_value::<Message>(event.data.clone()).ok())
        .map(|message| message.source().clone())
        .unwrap();
    assert!(is_compact_checkpoint_source(&source));
    assert_eq!(
        source,
        compact_checkpoint_source(&result.compaction_id, None)
    );
    assert!(!is_compact_checkpoint_source(&MessageSource::plugin(
        "other"
    )));
    assert!(!is_compact_checkpoint_source(&MessageSource::user()));
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type.starts_with("compaction/"))
            .map(|event| event.event_type.as_str())
            .collect::<Vec<_>>(),
        ["compaction/start", "compaction/summary", "compaction/end"]
    );
}
