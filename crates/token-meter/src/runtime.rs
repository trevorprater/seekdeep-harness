//! Replay owner for one fixed estimator and isolated per-session folds.

use std::{
    collections::{HashMap, HashSet},
    ops::Deref,
    sync::{Arc, Weak},
};

use parking_lot::Mutex;
use seekdeep_cordis::{
    Context, EventOptions, EventReply, Plugin, ServiceKey,
    fiber::{DisposeFuture, EffectHandle, Fiber},
};
use seekdeep_core::{
    request_header::{EpochHeader, canonical_header, header_equals},
    session::{Session, SessionEvent, is_surface_event},
};
use seekdeep_llm::{BlockAssembler, Message, StreamChunk, TokenUsage};
use seekdeep_session_projection::SESSION_PROJECTIONS;
use serde_json::{Value, json};

use crate::{
    breakdown_projection::context_breakdown_definition,
    estimate::{ROLE_OVERHEAD, estimate_content, estimate_header, estimate_message},
    surface_fold::{fold_surface_tokens, signed_difference},
    types::{TokenMeasurement, TokenMeasurementBaseline, TokenMeterConfig, TokenSurfaceNode},
    usage_projection::{context_pressure_definition, token_usage_definition},
};

/// Plugin name.
pub const NAME: &str = "token-meter";
/// Typed singleton service slot.
pub const TOKEN_METER: ServiceKey<TokenMeter> = ServiceKey::new("tokenMeter");

#[derive(Clone, Debug, PartialEq)]
struct MeasurementAnchor {
    header: Option<EpochHeader>,
    surface_tokens: u64,
    baseline: TokenMeasurementBaseline,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StepStart {
    turn: u64,
    step: u64,
    surface_tokens: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct ReplayState {
    consumed_events: u64,
    header: Option<EpochHeader>,
    surface: Vec<TokenSurfaceNode>,
    surface_tokens: u64,
    step_start: Option<StepStart>,
    anchor: Option<MeasurementAnchor>,
}

#[derive(Debug)]
struct CachedState {
    session: Weak<Session>,
    state: ReplayState,
}

#[derive(Debug)]
struct ProjectionBinding {
    registry: Option<usize>,
    handles: Vec<EffectHandle>,
}

impl ProjectionBinding {
    fn new() -> Self {
        Self {
            registry: None,
            handles: Vec::new(),
        }
    }
}

/// Replay-aware token measurement service.
pub struct TokenMeter {
    states: Mutex<HashMap<usize, CachedState>>,
}

impl std::fmt::Debug for TokenMeter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TokenMeter")
            .field("observed_sessions", &self.states.lock().len())
            .finish_non_exhaustive()
    }
}

impl TokenMeter {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            states: Mutex::new(HashMap::new()),
        })
    }

    /// Measures current request pressure and current positional surface.
    ///
    /// # Errors
    ///
    /// Returns the first malformed unread event or arithmetic failure without
    /// advancing the cached replay cursor past that event.
    pub fn measure(
        &self,
        session: &Arc<Session>,
        request_header: Option<EpochHeader>,
    ) -> anyhow::Result<TokenMeasurement> {
        let mut states = self.states.lock();
        let state = sync_state(&mut states, session)?;
        let header = request_header
            .map(canonical_header)
            .or_else(|| state.header.clone());
        let (baseline, surface_delta_tokens) = match state.anchor.as_ref() {
            Some(anchor) if optional_header_equals(anchor.header.as_ref(), header.as_ref()) => (
                anchor.baseline.clone(),
                signed_difference(state.surface_tokens, anchor.surface_tokens)?,
            ),
            _ if header.is_none() && state.surface_tokens == 0 => {
                (TokenMeasurementBaseline::None { tokens: 0 }, 0)
            }
            _ => (
                TokenMeasurementBaseline::Estimated {
                    tokens: estimate_header(header.as_ref())
                        .checked_add(state.surface_tokens)
                        .ok_or_else(|| {
                            anyhow::anyhow!("token meter estimated baseline overflowed")
                        })?,
                },
                0,
            ),
        };
        let total = i128::from(baseline.tokens()) + i128::from(surface_delta_tokens);
        let total_tokens = if total <= 0 {
            0
        } else {
            u64::try_from(total).map_err(|_| anyhow::anyhow!("token meter total overflowed"))?
        };
        Ok(TokenMeasurement {
            log_revision: state.consumed_events,
            baseline,
            surface_delta_tokens,
            total_tokens,
            surface_tokens: state.surface_tokens,
            nodes: state.surface.clone(),
        })
    }

    /// Prices one model-visible message with the service's fixed heuristic.
    #[must_use]
    pub fn estimate_message(&self, message: &Message) -> u64 {
        estimate_message(message)
    }

    fn sync_existing(&self, session: &Arc<Session>) -> anyhow::Result<()> {
        let key = session_key(session);
        let mut states = self.states.lock();
        let exists = states.get(&key).is_some_and(|cached| {
            cached
                .session
                .upgrade()
                .is_some_and(|cached| Arc::ptr_eq(&cached, session))
        });
        if exists {
            sync_state(&mut states, session)?;
        }
        Ok(())
    }
}

/// Lifecycle-owned direct installation.
pub struct TokenMeterInstallation {
    service: Arc<TokenMeter>,
    effect: EffectHandle,
}

impl std::fmt::Debug for TokenMeterInstallation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TokenMeterInstallation")
            .field("service", &self.service)
            .finish_non_exhaustive()
    }
}

impl Deref for TokenMeterInstallation {
    type Target = Arc<TokenMeter>;

    fn deref(&self) -> &Self::Target {
        &self.service
    }
}

impl TokenMeterInstallation {
    /// Disposes the service, listener, and optional projection registrations.
    ///
    /// # Errors
    ///
    /// Returns aggregate lifecycle cleanup failures.
    pub async fn dispose(&self) -> anyhow::Result<()> {
        self.effect.dispose().await
    }
}

/// Installs the service under one child lifecycle.
///
/// # Errors
///
/// Returns invalid configuration, duplicate service, projection, or ownership failures.
pub fn install(
    context: &Context,
    config: TokenMeterConfig,
) -> anyhow::Result<TokenMeterInstallation> {
    let fiber = Fiber::active_child(NAME);
    let child = context.with_fiber(fiber.clone());
    let service = match install_scoped(&child, config) {
        Ok(service) => service,
        Err(error) => {
            return match futures::executor::block_on(fiber.dispose()) {
                Ok(()) => Err(error),
                Err(cleanup) => Err(anyhow::anyhow!(
                    "{error:#}: token-meter rollback failed: {cleanup:#}"
                )),
            };
        }
    };
    let cleanup = fiber.clone();
    let effect = EffectHandle::new(NAME, move || -> DisposeFuture {
        Box::pin(async move { cleanup.dispose().await })
    });
    match context.own(effect) {
        Ok(effect) => Ok(TokenMeterInstallation { service, effect }),
        Err(error) => match futures::executor::block_on(fiber.dispose()) {
            Ok(()) => Err(error.into()),
            Err(cleanup) => Err(anyhow::anyhow!(
                "{error}: token-meter ownership rollback failed: {cleanup:#}"
            )),
        },
    }
}

/// Builds the loader-facing plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, std::iter::empty::<&str>(), |context, config| {
        Box::pin(async move {
            validate_config_value(&config)?;
            install_scoped(&context, TokenMeterConfig::default())?;
            Ok(())
        })
    })
    .with_config_validator(|value| {
        validate_config_value(value)?;
        Ok(json!({}))
    })
}

fn install_scoped(context: &Context, _config: TokenMeterConfig) -> anyhow::Result<Arc<TokenMeter>> {
    let service = TokenMeter::new();
    context.provide(TOKEN_METER, service.clone())?;
    let weak = Arc::downgrade(&service);
    context.events().on_sync(
        context,
        "session/event",
        move |_, args| {
            let Some(service) = weak.upgrade() else {
                return Ok(EventReply::Undefined);
            };
            let session = args
                .get::<Session>(0)
                .ok_or_else(|| anyhow::anyhow!("session/event lacks a session"))?;
            service.sync_existing(&session)?;
            Ok(EventReply::Undefined)
        },
        EventOptions::default(),
    )?;

    let binding = Arc::new(Mutex::new(ProjectionBinding::new()));
    reconcile_projections(context, &binding)?;
    let watched_context = context.clone();
    let watched_binding = binding;
    context.on_service_change(move || {
        if let Err(error) = reconcile_projections(&watched_context, &watched_binding) {
            tracing::error!(%error, "token meter: projection dependency reconciliation failed");
        }
    })?;
    Ok(service)
}

fn reconcile_projections(
    context: &Context,
    binding: &Arc<Mutex<ProjectionBinding>>,
) -> anyhow::Result<()> {
    let registry = context.get(SESSION_PROJECTIONS);
    let identity = registry
        .as_ref()
        .map(|registry| Arc::as_ptr(registry) as usize);
    let mut binding = binding.lock();
    if binding.registry == identity {
        return Ok(());
    }
    for handle in binding.handles.drain(..).rev() {
        futures::executor::block_on(handle.dispose())?;
    }
    binding.registry = None;
    let Some(registry) = registry else {
        return Ok(());
    };
    let mut handles = Vec::new();
    for definition in [
        token_usage_definition(),
        context_pressure_definition(),
        context_breakdown_definition(),
    ] {
        match registry.register(context, definition) {
            Ok(handle) => handles.push(handle),
            Err(error) => {
                for handle in handles.drain(..).rev() {
                    let _ = futures::executor::block_on(handle.dispose());
                }
                return Err(error);
            }
        }
    }
    binding.registry = identity;
    binding.handles = handles;
    Ok(())
}

fn validate_config_value(value: &Value) -> anyhow::Result<()> {
    let config = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("TokenMeterConfig: expected an object"))?;
    if let Some(key) = config.keys().next() {
        anyhow::bail!("TokenMeterConfig: unknown key {key:?} (no settings are supported)");
    }
    Ok(())
}

fn sync_state<'a>(
    states: &'a mut HashMap<usize, CachedState>,
    session: &Arc<Session>,
) -> anyhow::Result<&'a ReplayState> {
    let key = session_key(session);
    let stale = states.get(&key).is_some_and(|cached| {
        cached
            .session
            .upgrade()
            .is_none_or(|cached| !Arc::ptr_eq(&cached, session))
    });
    if stale {
        states.remove(&key);
    }
    let cached = states.entry(key).or_insert_with(|| CachedState {
        session: Arc::downgrade(session),
        state: ReplayState::default(),
    });
    let events = session.events();
    while usize::try_from(cached.state.consumed_events)
        .ok()
        .is_some_and(|index| index < events.len())
    {
        let index = usize::try_from(cached.state.consumed_events)
            .map_err(|_| anyhow::anyhow!("token meter replay cursor exceeds usize"))?;
        let event = &events[index];
        let mut next = cached.state.clone();
        fold_event(&events, &mut next, event)?;
        next.consumed_events = next
            .consumed_events
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("token meter replay cursor overflowed"))?;
        cached.state = next;
    }
    Ok(&cached.state)
}

fn fold_event(
    events: &[SessionEvent],
    state: &mut ReplayState,
    event: &SessionEvent,
) -> anyhow::Result<()> {
    let mut next_header = state.header.clone();
    let mut next_step_start = state.step_start.clone();
    let mut next_anchor = state.anchor.clone();
    match event.event_type.as_str() {
        "request/header" => {
            let header: EpochHeader = serde_json::from_value(
                event
                    .data
                    .get("header")
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("request/header lacks header"))?,
            )?;
            next_header = Some(canonical_header(header));
        }
        "step/start" => {
            if let Some(open) = &state.step_start {
                anyhow::bail!(
                    "token meter: step/start at seq {} arrived before turn {}/step {} ended",
                    event.seq,
                    open.turn,
                    open.step
                );
            }
            next_step_start = Some(StepStart {
                turn: coordinate(event, "turn")?,
                step: coordinate(event, "step")?,
                surface_tokens: state.surface_tokens,
            });
        }
        "step/end" => {
            let turn = coordinate(event, "turn")?;
            let step = coordinate(event, "step")?;
            anyhow::ensure!(
                state
                    .step_start
                    .as_ref()
                    .is_some_and(|open| open.turn == turn && open.step == step),
                "token meter: step/end at seq {} has no matching step/start event",
                event.seq
            );
            next_step_start = None;
        }
        _ => {}
    }

    let surface = is_surface_event(event)
        .then(|| fold_surface_tokens(&state.surface, event))
        .transpose()?;
    if event.event_type == "assistant/message" {
        let event_tokens = surface.as_ref().map_or(0, |surface| surface.tokens);
        next_anchor = Some(assistant_anchor(
            events,
            state,
            event,
            next_header.as_ref(),
            event_tokens,
        )?);
    }

    state.header = next_header;
    state.step_start = next_step_start;
    if let Some(surface) = surface {
        state.surface = surface.nodes;
        state.surface_tokens = apply_signed(state.surface_tokens, surface.delta_tokens)?;
    }
    state.anchor = next_anchor;
    Ok(())
}

fn assistant_anchor(
    events: &[SessionEvent],
    state: &ReplayState,
    event: &SessionEvent,
    header: Option<&EpochHeader>,
    event_tokens: u64,
) -> anyhow::Result<MeasurementAnchor> {
    let turn = coordinate(event, "turn")?;
    let step = coordinate(event, "step")?;
    let step_start = state
        .step_start
        .as_ref()
        .filter(|open| open.turn == turn && open.step == step)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "token meter: assistant/message at seq {} has no matching step/start event",
                event.seq
            )
        })?;
    let usage = event
        .data
        .get("usage")
        .map(|usage| serde_json::from_value::<TokenUsage>(usage.clone()))
        .transpose()?;
    if let (Some(usage), Some(header)) = (usage, header) {
        let provider_assistant_tokens = estimate_provider_assistant(events, event, event_tokens)?;
        let anchor_surface_tokens = step_start
            .surface_tokens
            .checked_add(provider_assistant_tokens)
            .ok_or_else(|| anyhow::anyhow!("token meter anchor surface overflowed"))?;
        let provider_tokens = usage_tokens(&usage)?;
        let estimated_anchor_tokens = estimate_header(Some(header))
            .checked_add(anchor_surface_tokens)
            .ok_or_else(|| anyhow::anyhow!("token meter anchor estimate overflowed"))?;
        let baseline = if provider_tokens >= estimated_anchor_tokens {
            TokenMeasurementBaseline::Usage {
                tokens: provider_tokens,
                usage,
            }
        } else {
            TokenMeasurementBaseline::Estimated {
                tokens: estimated_anchor_tokens,
            }
        };
        Ok(MeasurementAnchor {
            header: Some(header.clone()),
            surface_tokens: anchor_surface_tokens,
            baseline,
        })
    } else {
        let anchor_surface_tokens = step_start
            .surface_tokens
            .checked_add(event_tokens)
            .ok_or_else(|| anyhow::anyhow!("token meter anchor surface overflowed"))?;
        Ok(MeasurementAnchor {
            header: header.cloned(),
            surface_tokens: anchor_surface_tokens,
            baseline: TokenMeasurementBaseline::Estimated {
                tokens: estimate_header(header)
                    .checked_add(anchor_surface_tokens)
                    .ok_or_else(|| anyhow::anyhow!("token meter anchor estimate overflowed"))?,
            },
        })
    }
}

fn estimate_provider_assistant(
    events: &[SessionEvent],
    event: &SessionEvent,
    durable_event_tokens: u64,
) -> anyhow::Result<u64> {
    let Some(source_seqs) = event.source_event_seqs.as_ref() else {
        return Ok(durable_event_tokens);
    };
    let turn = coordinate(event, "turn")?;
    let step = coordinate(event, "step")?;
    let mut assembler = BlockAssembler::new();
    let mut seen = HashSet::new();
    for seq in source_seqs {
        anyhow::ensure!(
            *seq < event.seq,
            "token meter: assistant/message at seq {} source seq {} is not earlier",
            event.seq,
            seq
        );
        anyhow::ensure!(
            seen.insert(*seq),
            "token meter: assistant/message at seq {} repeats source seq {}",
            event.seq,
            seq
        );
        let index = usize::try_from(*seq)
            .map_err(|_| anyhow::anyhow!("token meter source seq exceeds usize"))?;
        let source = events.get(index).ok_or_else(|| {
            anyhow::anyhow!(
                "token meter: assistant/message at seq {} source seq {} is not assistant/chunk",
                event.seq,
                seq
            )
        })?;
        anyhow::ensure!(
            source.event_type == "assistant/chunk",
            "token meter: assistant/message at seq {} source seq {} is not assistant/chunk",
            event.seq,
            seq
        );
        anyhow::ensure!(
            coordinate(source, "turn")? == turn && coordinate(source, "step")? == step,
            "token meter: assistant/message at seq {} source seq {} belongs to another step",
            event.seq,
            seq
        );
        let chunk: StreamChunk = serde_json::from_value(
            source
                .data
                .get("chunk")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("assistant/chunk lacks chunk"))?,
        )?;
        assembler.push(chunk);
    }
    let content = assembler.blocks()?;
    if content.is_empty() {
        Ok(0)
    } else {
        estimate_content(&content)
            .checked_add(ROLE_OVERHEAD)
            .ok_or_else(|| anyhow::anyhow!("token meter provider assistant estimate overflowed"))
    }
}

fn usage_tokens(usage: &TokenUsage) -> anyhow::Result<u64> {
    usage
        .input_tokens
        .checked_add(usage.cache_read_tokens.unwrap_or(0))
        .and_then(|value| value.checked_add(usage.cache_write_tokens.unwrap_or(0)))
        .and_then(|value| value.checked_add(usage.output_tokens))
        .ok_or_else(|| anyhow::anyhow!("token meter provider usage sum overflowed"))
}

fn coordinate(event: &SessionEvent, field: &str) -> anyhow::Result<u64> {
    event
        .data
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "token meter: {} at seq {} has no valid {field}",
                event.event_type,
                event.seq
            )
        })
}

fn apply_signed(total: u64, delta: i64) -> anyhow::Result<u64> {
    let next = i128::from(total) + i128::from(delta);
    anyhow::ensure!(next >= 0, "token meter surface total became negative");
    u64::try_from(next).map_err(|_| anyhow::anyhow!("token meter surface total overflowed"))
}

fn optional_header_equals(left: Option<&EpochHeader>, right: Option<&EpochHeader>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => header_equals(left, right),
        (None, None) => true,
        _ => false,
    }
}

fn session_key(session: &Arc<Session>) -> usize {
    Arc::as_ptr(session) as usize
}

#[cfg(test)]
mod tests {
    use seekdeep_core::session::SurfaceOp;
    use seekdeep_llm::{
        ContentBlock, LlmCallConfig, Message, MessageRole, MessageSource, ModelId, ProviderId,
    };

    use super::*;

    fn raw(seq: u64, event_type: &str, data: Value, surface: Option<SurfaceOp>) -> SessionEvent {
        SessionEvent {
            event_type: event_type.to_owned(),
            seq,
            time: 0,
            data,
            source_event_seqs: None,
            surface_op: surface,
            ignorable: None,
        }
    }

    fn header() -> EpochHeader {
        EpochHeader {
            config: LlmCallConfig {
                provider: ProviderId::new("mock"),
                model: ModelId::new("model"),
                reasoning_effort: None,
                temperature: None,
                max_tokens: None,
                stop: None,
            },
            adapter_defaults: None,
            system: None,
            tools: None,
        }
    }

    fn assistant(seq: u64, sources: Vec<u64>) -> SessionEvent {
        let mut event = raw(
            seq,
            "assistant/message",
            json!({
                "turn":1,"step":1,
                "message":Message::new(
                    MessageRole::Assistant,
                    Vec::<ContentBlock>::new(),
                    MessageSource::model("mock","model")
                ),
                "usage":{"inputTokens":1,"outputTokens":0}
            }),
            Some(SurfaceOp::append()),
        );
        event.source_event_seqs = Some(sources);
        event
    }

    fn prefix(source: SessionEvent, assistant: SessionEvent) -> Vec<SessionEvent> {
        vec![
            raw(0, "step/start", json!({"turn":1,"step":1}), None),
            raw(
                1,
                "request/header",
                json!({"header":header(),"reason":"initial"}),
                None,
            ),
            source,
            assistant,
        ]
    }

    fn fold_until_last(events: &[SessionEvent]) -> ReplayState {
        let mut state = ReplayState::default();
        for event in &events[..events.len() - 1] {
            fold_event(events, &mut state, event).unwrap();
            state.consumed_events += 1;
        }
        state
    }

    #[test]
    fn invalid_source_references_fail_without_partial_state_mutation() {
        let user = raw(
            2,
            "user/message",
            serde_json::to_value(seekdeep_llm::UserMessage::new(
                vec![ContentBlock::Text {
                    text: "x".to_owned(),
                }],
                MessageSource::user(),
            ))
            .unwrap(),
            Some(SurfaceOp::append()),
        );
        let cases = [
            prefix(user, assistant(3, vec![2])),
            prefix(
                raw(
                    2,
                    "assistant/chunk",
                    json!({"turn":1,"step":2,"chunk":{"type":"finish","reason":{"kind":"stop"}}}),
                    None,
                ),
                assistant(3, vec![2]),
            ),
            prefix(
                raw(
                    2,
                    "assistant/chunk",
                    json!({"turn":1,"step":1,"chunk":{"type":"finish","reason":{"kind":"stop"}}}),
                    None,
                ),
                assistant(3, vec![2, 2]),
            ),
            prefix(
                raw(
                    2,
                    "assistant/chunk",
                    json!({"turn":1,"step":1,"chunk":{"type":"finish","reason":{"kind":"stop"}}}),
                    None,
                ),
                assistant(3, vec![99]),
            ),
        ];
        let expected = [
            "is not assistant/chunk",
            "belongs to another step",
            "repeats source seq",
            "is not earlier",
        ];
        for (events, expected) in cases.into_iter().zip(expected) {
            let mut state = fold_until_last(&events);
            let before = state.clone();
            for _ in 0..2 {
                let error = fold_event(&events, &mut state, events.last().unwrap()).unwrap_err();
                assert!(error.to_string().contains(expected));
                assert_eq!(state, before);
            }
        }
    }
}
