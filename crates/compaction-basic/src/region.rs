//! Surface retention selection and the shared log-recorded compaction
//! transaction for automatic open-turn and manual idle-session compaction.

use std::sync::Arc;

use futures::future::BoxFuture;
use seekdeep_commands::CommandId;
use seekdeep_compaction::{
    CompactionId, CompactionResult, ShadowedRange, compact_checkpoint_source,
    service::{ManualCompactionError, ManualCompactionErrorCode},
    tool_pairing::{tool_pairing_balanced_after, tool_pairing_balanced_before},
};
use seekdeep_core::session::{
    AppendOptions, Session, SessionEvent, SurfaceOp, derive_event_message,
};
use seekdeep_llm::{AbortSignal, UserMessage, error_chain};
use seekdeep_token_meter::{TokenMeasurement, TokenMeter};
use serde_json::{Value, json};

use crate::summarizer::{SummarizationInput, SummaryResult, Target, frame_summary};

/// Dynamically dispatched summarizer hook used by the region transaction.
pub type RegionSummarize = Arc<
    dyn Fn(
            SummarizationInput,
            Arc<Session>,
            Option<Target>,
            Option<AbortSignal>,
        ) -> BoxFuture<'static, anyhow::Result<SummaryResult>>
        + Send
        + Sync,
>;

/// Injectable append boundary for deterministic commit-failure tests.
pub type CompactionAppend = Arc<
    dyn Fn(
            &Arc<Session>,
            &str,
            Value,
            AppendOptions,
        ) -> Result<SessionEvent, seekdeep_core::session::SessionError>
        + Send
        + Sync,
>;

/// Injectable token-measurement boundary for stability testing.
pub type RegionMeasure = Arc<
    dyn Fn(&Arc<Session>) -> anyhow::Result<seekdeep_token_meter::TokenMeasurement> + Send + Sync,
>;

/// Effective pricing and summarization dependencies for one region transaction.
#[derive(Clone)]
pub struct RegionDependencies {
    /// Conversation meter for pressure, retention, and convergence pricing.
    pub meter: Arc<TokenMeter>,
    /// Dynamically dispatched summarizer.
    pub summarize: RegionSummarize,
    /// Optional measurement carrier; production calls the concrete meter.
    pub measure: Option<RegionMeasure>,
}

impl RegionDependencies {
    fn measure(
        &self,
        session: &Arc<Session>,
    ) -> anyhow::Result<seekdeep_token_meter::TokenMeasurement> {
        self.measure.as_ref().map_or_else(
            || self.meter.measure(session, None),
            |measure| measure(session),
        )
    }
}

/// One validated inclusive span of current surface positions.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SurfaceSelection {
    start: u64,
    end: u64,
    start_idx: usize,
    end_idx: usize,
    shadowed_seqs: Vec<u64>,
}

/// A selection with its priced snapshot and the replay input built from it.
#[derive(Clone)]
struct PreparedCompaction {
    selection: SurfaceSelection,
    measurement: TokenMeasurement,
    selected_nodes: Vec<seekdeep_token_meter::TokenSurfaceNode>,
    shadowed_token_count: u64,
    input: SummarizationInput,
}

/// A prepared selection combined with the summarized checkpoint framing.
#[derive(Clone)]
struct SummarizedCompaction {
    prepared: PreparedCompaction,
    result: SummaryResult,
    checkpoint_message: UserMessage,
}

/// Bracket owner, stability rule, and optional durability checkpoint.
pub struct CompactionTransactionOptions {
    /// `None` writes a standalone bracket; `Some` derives the open turn.
    pub owner: Option<u64>,
    /// Surface relationship that must survive asynchronous summarization.
    pub stability: Stability,
    /// Optional durability checkpoint after a successfully closed bracket.
    pub flush: Option<BoxFuture<'static, anyhow::Result<()>>>,
    /// Manual command that initiated this transaction, when present.
    pub source_command_id: Option<CommandId>,
    /// Optional deterministic append boundary; production uses `Session::append`.
    pub append: Option<CompactionAppend>,
}

/// Surface relationship that must survive asynchronous summarization.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Stability {
    /// The entire surface must remain unchanged.
    WholeSurface,
    /// Only the selected span must remain stable.
    SelectedSpan,
}

/// Latest open-turn, unmatched-marker, and seed-boundary state.
#[derive(Clone, Debug, Default)]
struct CompactionEntryState {
    open_turn: Option<u64>,
    unmatched_compaction_start: Option<SessionEvent>,
    latest_end_seed_seq: Option<u64>,
}

/// Rejects a summary whose replacement boundaries are no longer the ones it
/// was built from.
#[derive(Clone, Debug, thiserror::Error)]
#[error("{0}")]
struct SurfaceChangedError(String);

/// Resolves the next head-anchored range while retaining a priced recent tail
/// and never splitting an assistant tool-call/result pair.
///
/// # Errors
///
/// Returns a mismatch failure when the token meter and session surfaces
/// disagree, or a pairing-balance failure.
pub fn select_compactable_range(
    session: &Arc<Session>,
    measurement: &TokenMeasurement,
    retain_tokens: u64,
) -> anyhow::Result<Option<ShadowedRange>> {
    let priced_nodes = &measurement.nodes;
    if priced_nodes.is_empty() {
        return Ok(None);
    }
    let surface_nodes = session.surface_nodes();
    if surface_nodes.len() != priced_nodes.len()
        || surface_nodes
            .iter()
            .zip(priced_nodes)
            .any(|(seq, node)| *seq != node.seq)
    {
        anyhow::bail!("compaction: token-meter surface does not match the current session surface");
    }

    let mut accumulated = 0_u64;
    let mut keep_from_idx = priced_nodes.len();
    for index in (0..priced_nodes.len()).rev() {
        accumulated += priced_nodes[index].tokens;
        keep_from_idx = index;
        if accumulated >= retain_tokens {
            break;
        }
    }
    if keep_from_idx == 0 {
        return Ok(None);
    }
    while keep_from_idx > 0 {
        if tool_pairing_balanced_before(session, surface_nodes[keep_from_idx])? {
            break;
        }
        keep_from_idx -= 1;
    }
    if keep_from_idx == 0 {
        return Ok(None);
    }
    let first = surface_nodes[0];
    let cutoff = surface_nodes[keep_from_idx - 1];
    Ok(Some(ShadowedRange {
        start: first,
        end: cutoff,
    }))
}

/// Runs the single compaction transaction over one selected positional span.
///
/// # Errors
///
/// Returns selection, lock, summarization, stability, commit, or persistence
/// failures, classified per stage for the manual caller.
#[allow(clippy::too_many_lines)]
pub async fn compact_surface_region(
    dependencies: &RegionDependencies,
    session: &Arc<Session>,
    start: u64,
    end: u64,
    fallback: Option<Target>,
    options: CompactionTransactionOptions,
    signal: Option<AbortSignal>,
) -> anyhow::Result<CompactionResult> {
    let CompactionTransactionOptions {
        owner: owner_option,
        stability,
        flush,
        source_command_id,
        append,
    } = options;

    if owner_option.is_none()
        && let Some(signal) = signal.as_ref()
        && signal.is_aborted()
    {
        return Err(crate::index::compaction_abort_error(signal));
    }
    let selection = validate_surface_region(session, start, end)?;
    let entry_state = inspect_compaction_entry_state(&session.events());
    assert_compaction_inactive(
        entry_state.unmatched_compaction_start.as_ref(),
        entry_state.latest_end_seed_seq,
        "compaction",
    )?;

    let owner = if owner_option.is_none() {
        if entry_state.open_turn.is_some() {
            return Err(ManualCompactionError::new(
                ManualCompactionErrorCode::Busy,
                "manual compaction: the session already has an open turn",
            )
            .into());
        }
        None
    } else {
        let turn = entry_state.open_turn.ok_or_else(|| {
            anyhow::anyhow!(
                "compactRegion: no open turn — automatic compaction events must be enclosed in a turn"
            )
        })?;
        Some(turn)
    };

    let compaction_id = CompactionId::new(uuid::Uuid::new_v4().to_string());
    let start_event = append_event(
        append.as_ref(),
        session,
        "compaction/start",
        lifecycle_value(&compaction_id, source_command_id.as_ref(), owner),
        AppendOptions::default(),
    )?;

    let mut failure: Option<(anyhow::Error, Stage)> = None;
    let mut flush_failure: Option<anyhow::Error> = None;
    let mut result: Option<CompactionResult> = None;
    let mut closed = false;
    let mut closing = false;
    let mut stage = Stage::Summary;

    let try_outcome: Result<(), anyhow::Error> = async {
        let prepared = prepare_compaction(dependencies, session, &selection)?;
        let summarized = summarize_compaction(
            dependencies,
            &prepared,
            session,
            fallback.clone(),
            &compaction_id,
            source_command_id.as_ref(),
            signal.clone(),
        )
        .await?;
        if owner_option.is_none()
            && let Some(signal) = signal.as_ref()
            && signal.is_aborted()
        {
            return Err(crate::index::compaction_abort_error(signal));
        }
        match stability {
            Stability::WholeSurface => {
                assert_whole_surface_unchanged(dependencies, session, &summarized.prepared)?;
            }
            Stability::SelectedSpan => {
                assert_selected_span_stable(dependencies, session, &summarized.prepared)?;
            }
        }
        stage = Stage::Commit;
        let pending = commit_compaction_body(append.as_ref(), session, &start_event, &summarized)?;
        closing = true;
        let end_event = append_event(
            append.as_ref(),
            session,
            "compaction/end",
            lifecycle_value(&compaction_id, source_command_id.as_ref(), owner),
            AppendOptions::default(),
        )?;
        closed = true;
        result = Some(complete_compaction(&pending, &end_event));
        Ok(())
    }
    .await;

    if let Err(error) = try_outcome {
        let stage_kind = if closing { Stage::Commit } else { stage };
        failure = Some((error, stage_kind));
        if !closing {
            let mut close_lifecycle =
                lifecycle_value(&compaction_id, source_command_id.as_ref(), owner);
            if let Some((error, _)) = &failure {
                close_lifecycle["error"] = json!(error_chain(error.as_ref()));
            }
            match append_event(
                append.as_ref(),
                session,
                "compaction/end",
                close_lifecycle,
                AppendOptions::default(),
            ) {
                Ok(_) => closed = true,
                Err(close_error) => {
                    failure = Some((close_error.into(), Stage::Commit));
                }
            }
        }
    }

    if closed
        && let Some(flush) = flush
        && let Err(error) = flush.await
    {
        flush_failure = Some(error);
    }

    if owner_option.is_none()
        && let Some(signal) = signal.as_ref()
        && signal.is_aborted()
    {
        return Err(crate::index::compaction_abort_error(signal));
    }
    if let Some((error, stage_kind)) = failure {
        if owner_option.is_none() {
            return Err(throw_manual_failure(stage_kind, error));
        }
        return Err(error);
    }
    if let Some(error) = flush_failure {
        return Err(ManualCompactionError::new(
            ManualCompactionErrorCode::Persistence,
            "manual compaction durability checkpoint failed",
        )
        .with_cause(error)
        .into());
    }
    result.ok_or_else(|| anyhow::anyhow!("compaction committed without a result"))
}

/// Rechecks the durable compaction lock after an asynchronous policy decision.
///
/// # Errors
///
/// Returns a busy failure when an unmatched marker is still active.
pub fn assert_no_active_compaction(session: &Arc<Session>, stage: &str) -> anyhow::Result<()> {
    let entry_state = inspect_compaction_entry_state(&session.events());
    assert_compaction_inactive(
        entry_state.unmatched_compaction_start.as_ref(),
        entry_state.latest_end_seed_seq,
        stage,
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Summary,
    Commit,
}

fn throw_manual_failure(stage: Stage, error: anyhow::Error) -> anyhow::Error {
    match stage {
        Stage::Commit => ManualCompactionError::new(
            ManualCompactionErrorCode::Commit,
            "manual compaction did not commit cleanly",
        )
        .with_cause(error)
        .into(),
        Stage::Summary => {
            if error.downcast_ref::<SurfaceChangedError>().is_some() {
                ManualCompactionError::new(
                    ManualCompactionErrorCode::Changed,
                    "the compacted history changed during manual compaction",
                )
                .with_cause(error)
                .into()
            } else {
                ManualCompactionError::new(
                    ManualCompactionErrorCode::Summary,
                    "manual compaction could not produce a smaller summary",
                )
                .with_cause(error)
                .into()
            }
        }
    }
}

fn assert_compaction_inactive(
    unmatched_compaction_start: Option<&SessionEvent>,
    latest_end_seed_seq: Option<u64>,
    stage: &str,
) -> anyhow::Result<()> {
    let Some(unmatched) = unmatched_compaction_start else {
        return Ok(());
    };
    if latest_end_seed_seq.is_some_and(|seq| seq > unmatched.seq) {
        return Ok(());
    }
    Err(ManualCompactionError::new(
        ManualCompactionErrorCode::Busy,
        format!(
            "{stage}: compaction already in progress; the session compaction lock is already active"
        ),
    )
    .into())
}

fn validate_surface_region(
    session: &Arc<Session>,
    start: u64,
    end: u64,
) -> anyhow::Result<SurfaceSelection> {
    let nodes = session.surface_nodes();
    let start_idx = nodes.iter().position(|seq| *seq == start);
    let end_idx = nodes.iter().position(|seq| *seq == end);
    let Some(start_idx) = start_idx else {
        anyhow::bail!("compactRegion: start seq {start} not found in surface");
    };
    let Some(end_idx) = end_idx else {
        anyhow::bail!("compactRegion: end seq {end} not found in surface");
    };
    if start_idx > end_idx {
        anyhow::bail!(
            "compactRegion: start seq {start} (position {start_idx}) is after end seq {end} (position {end_idx}) on the surface"
        );
    }
    if !tool_pairing_balanced_before(session, nodes[start_idx])? {
        anyhow::bail!(
            "compactRegion: start seq {start} is not a balanced boundary (would split a step's tool-call/result pair)"
        );
    }
    if !tool_pairing_balanced_after(session, nodes[end_idx])? {
        anyhow::bail!(
            "compactRegion: end seq {end} is not a balanced boundary (would split a step, or the step is still open)"
        );
    }
    Ok(SurfaceSelection {
        start,
        end,
        start_idx,
        end_idx,
        shadowed_seqs: nodes[start_idx..=end_idx].to_vec(),
    })
}

fn prepare_compaction(
    dependencies: &RegionDependencies,
    session: &Arc<Session>,
    selection: &SurfaceSelection,
) -> anyhow::Result<PreparedCompaction> {
    let measurement = dependencies.measure(session)?;
    let selected_nodes = measurement.nodes[selection.start_idx..=selection.end_idx].to_vec();
    if selected_nodes.len() != selection.shadowed_seqs.len()
        || selected_nodes
            .iter()
            .zip(&selection.shadowed_seqs)
            .any(|(node, seq)| node.seq != *seq)
    {
        return Err(SurfaceChangedError(
            "compaction: selected surface changed before summarization began".to_owned(),
        )
        .into());
    }
    let shadowed_token_count = selected_nodes.iter().map(|node| node.tokens).sum();
    Ok(PreparedCompaction {
        selection: selection.clone(),
        measurement,
        selected_nodes,
        shadowed_token_count,
        input: build_summarization_input(session, &selection.shadowed_seqs),
    })
}

async fn summarize_compaction(
    dependencies: &RegionDependencies,
    prepared: &PreparedCompaction,
    session: &Arc<Session>,
    fallback: Option<Target>,
    compaction_id: &CompactionId,
    source_command_id: Option<&CommandId>,
    signal: Option<AbortSignal>,
) -> anyhow::Result<SummarizedCompaction> {
    let result =
        (dependencies.summarize)(prepared.input.clone(), session.clone(), fallback, signal).await?;
    let checkpoint_message = UserMessage::new(
        frame_summary(&result.summary),
        compact_checkpoint_source(compaction_id, source_command_id),
    );
    let framed_summary_token_count = dependencies.meter.estimate_message(&checkpoint_message);
    if framed_summary_token_count >= prepared.shadowed_token_count {
        anyhow::bail!(
            "summary is not smaller than the shadowed content ({framed_summary_token_count} estimated framed tokens >= {})",
            prepared.shadowed_token_count
        );
    }
    Ok(SummarizedCompaction {
        prepared: prepared.clone(),
        result,
        checkpoint_message,
    })
}

fn assert_whole_surface_unchanged(
    dependencies: &RegionDependencies,
    session: &Arc<Session>,
    prepared: &PreparedCompaction,
) -> anyhow::Result<()> {
    let current = dependencies.measure(session)?;
    if current.nodes != prepared.measurement.nodes {
        return Err(SurfaceChangedError(
            "compaction: session surface changed during summarization".to_owned(),
        )
        .into());
    }
    Ok(())
}

fn assert_selected_span_stable(
    dependencies: &RegionDependencies,
    session: &Arc<Session>,
    prepared: &PreparedCompaction,
) -> anyhow::Result<()> {
    let current =
        validate_surface_region(session, prepared.selection.start, prepared.selection.end)
            .map_err(|error| {
                SurfaceChangedError(format!(
                    "compaction: the selected span is no longer a valid replacement target: {error}"
                ))
            })?;
    if current.shadowed_seqs != prepared.selection.shadowed_seqs {
        return Err(SurfaceChangedError(
            "compaction: the selected span changed during summarization".to_owned(),
        )
        .into());
    }
    let measured =
        dependencies.measure(session)?.nodes[current.start_idx..=current.end_idx].to_vec();
    if measured != prepared.selected_nodes {
        return Err(SurfaceChangedError(
            "compaction: the selected span was rewritten during summarization".to_owned(),
        )
        .into());
    }
    Ok(())
}

fn commit_compaction_body(
    append: Option<&CompactionAppend>,
    session: &Arc<Session>,
    start_event: &SessionEvent,
    summarized: &SummarizedCompaction,
) -> anyhow::Result<CompactionResult> {
    let prepared = &summarized.prepared;
    let result = &summarized.result;
    let mut summary_data = json!({
        "compactionId": start_event.data["compactionId"],
    });
    if let Some(source_command_id) = start_event.data.get("sourceCommandId") {
        summary_data["sourceCommandId"] = source_command_id.clone();
    }
    summary_data["summary"] = serde_json::to_value(&result.summary)?;
    if result.llm_stream_call {
        summary_data["rawOutput"] = serde_json::to_value(&result.raw_output)?;
        summary_data["llmStreamCall"] = json!(true);
    } else if !result.raw_output.is_empty() {
        summary_data["rawOutput"] = serde_json::to_value(&result.raw_output)?;
    }
    summary_data["shadowedRange"] =
        json!({"start": prepared.selection.start, "end": prepared.selection.end});
    summary_data["shadowedSeqs"] = serde_json::to_value(&prepared.selection.shadowed_seqs)?;
    summary_data["shadowedTokenCount"] = json!(prepared.shadowed_token_count);
    summary_data["provider"] = serde_json::to_value(&result.provider)?;
    summary_data["model"] = serde_json::to_value(&result.model)?;
    if let Some(max_tokens) = result.max_tokens {
        summary_data["maxTokens"] = json!(max_tokens);
    }
    if let Some(usage) = &result.usage {
        summary_data["usage"] = serde_json::to_value(usage)?;
    }
    let summary_event = append_event(
        append,
        session,
        "compaction/summary",
        summary_data,
        AppendOptions::default(),
    )?;

    let checkpoint_data = serde_json::to_value(&summarized.checkpoint_message)?;
    append_event(
        append,
        session,
        "user/message",
        checkpoint_data,
        AppendOptions {
            surface_op: Some(SurfaceOp::replace(
                prepared.selection.start,
                prepared.selection.end,
            )),
            source_event_seqs: Some(
                std::iter::once(start_event.seq)
                    .chain(std::iter::once(summary_event.seq))
                    .chain(prepared.selection.shadowed_seqs.iter().copied())
                    .collect(),
            ),
            ..AppendOptions::default()
        },
    )?;

    let result_value = CompactionResult {
        compaction_id: CompactionId::new(
            start_event.data["compactionId"]
                .as_str()
                .unwrap_or_default(),
        ),
        source_command_id: start_event
            .data
            .get("sourceCommandId")
            .and_then(Value::as_str)
            .map(CommandId::new),
        start_seq: start_event.seq,
        summary_seq: summary_event.seq,
        end_seq: 0,
        summary: result.summary.clone(),
        shadowed_range: ShadowedRange {
            start: prepared.selection.start,
            end: prepared.selection.end,
        },
        shadowed_seqs: prepared.selection.shadowed_seqs.clone(),
        shadowed_token_count: prepared.shadowed_token_count,
    };
    Ok(result_value)
}

fn append_event(
    append: Option<&CompactionAppend>,
    session: &Arc<Session>,
    event_type: &str,
    data: Value,
    options: AppendOptions,
) -> Result<SessionEvent, seekdeep_core::session::SessionError> {
    match append {
        Some(append) => append(session, event_type, data, options),
        None => session.append(event_type, data, options),
    }
}

fn complete_compaction(pending: &CompactionResult, end_event: &SessionEvent) -> CompactionResult {
    let mut result = pending.clone();
    result.end_seq = end_event.seq;
    result
}

fn build_summarization_input(session: &Arc<Session>, shadowed_seqs: &[u64]) -> SummarizationInput {
    let header = session.request_header();
    let events = session.events();
    let region_messages = shadowed_seqs
        .iter()
        .filter_map(|seq| {
            let index = usize::try_from(*seq).ok()?;
            derive_event_message(events.get(index)?)
        })
        .collect();
    SummarizationInput {
        system: header.as_ref().and_then(|header| header.system.clone()),
        tools: header.as_ref().and_then(|header| header.tools.clone()),
        messages: region_messages,
    }
}

fn inspect_compaction_entry_state(events: &[SessionEvent]) -> CompactionEntryState {
    let mut state = CompactionEntryState::default();
    let mut open_turn_known = false;
    let mut compaction_known = false;
    for event in events.iter().rev() {
        if state.latest_end_seed_seq.is_none() && event.event_type == "session/end-seed" {
            state.latest_end_seed_seq = Some(event.seq);
        }
        if !compaction_known {
            if event.event_type == "compaction/start" {
                state.unmatched_compaction_start = Some(event.clone());
                compaction_known = true;
            } else if event.event_type == "compaction/end" {
                compaction_known = true;
            }
        }
        if !open_turn_known {
            if event.event_type == "turn/start" {
                state.open_turn = event.data.get("turn").and_then(Value::as_u64);
                open_turn_known = true;
            } else if event.event_type == "turn/end" {
                open_turn_known = true;
            }
        }
        if open_turn_known && compaction_known && state.latest_end_seed_seq.is_some() {
            break;
        }
    }
    state
}

fn lifecycle_value(
    compaction_id: &CompactionId,
    source_command_id: Option<&CommandId>,
    turn: Option<u64>,
) -> Value {
    let mut value = json!({"compactionId": compaction_id.as_str()});
    if let Some(source_command_id) = source_command_id {
        value["sourceCommandId"] = json!(source_command_id.as_str());
    }
    if let Some(turn) = turn {
        value["turn"] = json!(turn);
    }
    value
}
