//! Package-owned durable retry-event invariants.

use std::sync::Arc;

use seekdeep_cordis::{Context, DispatchMode, EventArgs, EventOptions, EventReply};
use seekdeep_core::{
    session::{Session, SessionEvent},
    session_store::SESSIONS,
};
use seekdeep_invariants::{
    InvariantFailure, InvariantInstaller, InvariantRegistration, InvariantRegistry,
};
use seekdeep_llm::MAX_TIMER_DELAY_MS;
use serde_json::Value;

use crate::history::provider_for_open_step;

const PACKAGE_NAME: &str = "@deepseek-ai/seekdeep-llm-retry";
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Registers complete validation for loaded and newly appended retry events.
///
/// # Errors
///
/// Returns ordinary invariant-registry or installer failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(
        PACKAGE_NAME,
        InvariantInstaller::new(["sessions"], |context, failure| async move {
            install(&context, &failure)
        }),
    )
}

fn install(context: &Context, failure: &InvariantFailure) -> anyhow::Result<()> {
    let sessions = context
        .get(SESSIONS)
        .ok_or_else(|| anyhow::anyhow!("llm-retry invariant requires sessions"))?;
    for session in sessions.list() {
        validate_session(&session).map_err(|error| failure.fail(error.to_string()))?;
    }
    let created_failure = failure.clone();
    context.events().on_sync(
        context,
        "session/created",
        move |_, args| {
            let session = required_session(&args, "session/created")?;
            validate_session(&session).map_err(|error| created_failure.fail(error.to_string()))?;
            Ok(EventReply::Undefined)
        },
        global(),
    )?;
    let append_failure = failure.clone();
    context.events().on_sync(
        context,
        "internal/dispatch",
        move |_, args| {
            validate_internal(&args).map_err(|error| append_failure.fail(error.to_string()))?;
            Ok(EventReply::Undefined)
        },
        global(),
    )?;
    Ok(())
}

fn global() -> EventOptions {
    EventOptions {
        global: true,
        ..EventOptions::default()
    }
}

fn validate_internal(args: &EventArgs) -> anyhow::Result<()> {
    args.get::<DispatchMode>(0)
        .ok_or_else(|| anyhow::anyhow!("internal/dispatch lacks a dispatch mode"))?;
    let name = args
        .get::<String>(1)
        .ok_or_else(|| anyhow::anyhow!("internal/dispatch lacks an event name"))?;
    if name.as_str() != "session/event" {
        return Ok(());
    }
    let carried = args
        .get::<EventArgs>(2)
        .ok_or_else(|| anyhow::anyhow!("internal/dispatch lacks event arguments"))?;
    let session = required_session(&carried, "session/event")?;
    let event = carried
        .get::<SessionEvent>(1)
        .ok_or_else(|| anyhow::anyhow!("session/event lacks its event"))?;
    validate_event(&session.events(), &event)
}

fn required_session(args: &EventArgs, event: &str) -> anyhow::Result<Arc<Session>> {
    args.get::<Arc<Session>>(0)
        .map(|nested| (*nested).clone())
        .or_else(|| args.get::<Session>(0))
        .ok_or_else(|| anyhow::anyhow!("{event} lacks its session"))
}

/// Validates all retry transitions already present in one session.
///
/// # Errors
///
/// Returns the first source-compatible invariant diagnostic.
pub fn validate_session(session: &Session) -> anyhow::Result<()> {
    let events = session.events();
    for (index, event) in events.iter().enumerate() {
        validate_event(&events[..index], event)?;
    }
    Ok(())
}

/// Validates one candidate retry event against its prior history.
///
/// # Errors
///
/// Returns a source-compatible invariant diagnostic.
pub fn validate_event(history: &[SessionEvent], event: &SessionEvent) -> anyhow::Result<()> {
    match event.event_type.as_str() {
        "llm/retry" => validate_retry(history, event),
        "llm/retry-started" => validate_started(history, event),
        _ => Ok(()),
    }
}

fn validate_failure(value: Option<&Value>) -> anyhow::Result<()> {
    let failure = value
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow::anyhow!("llm/retry failure must be an object"))?;
    required_nonempty_string(failure.get("message"), "llm/retry failure.message")?;
    required_nonempty_string(failure.get("code"), "llm/retry failure.code")?;
    if let Some(status) = failure.get("status") {
        let valid = status
            .as_f64()
            .is_some_and(|status| status.fract() == 0.0 && (100.0..=599.0).contains(&status));
        anyhow::ensure!(
            valid,
            "llm/retry failure.status must be an integer from 100 through 599 when present"
        );
    }
    if let Some(delay) = failure.get("providerRetryAfterMs") {
        anyhow::ensure!(
            delay
                .as_f64()
                .is_some_and(|delay| delay.is_finite() && delay > 0.0),
            "llm/retry failure.providerRetryAfterMs must be a positive finite number when present"
        );
    }
    if let Some(request_id) = failure.get("requestId") {
        required_nonempty_string(Some(request_id), "llm/retry failure.requestId")?;
    }
    Ok(())
}

fn validate_retry(history: &[SessionEvent], event: &SessionEvent) -> anyhow::Result<()> {
    let data = event
        .data
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("llm/retry data must be an object"))?;
    let retry_id = required_nonempty_string(data.get("retryId"), "llm/retry retryId")?;
    validate_failure(data.get("failure"))?;
    let retry = positive_safe_integer(data.get("retry"), "llm/retry retry")?;
    let provider = required_nonempty_string(data.get("provider"), "llm/retry provider")?;
    let policy_key = required_nonempty_string(data.get("policyKey"), "llm/retry policyKey")?;
    validate_mode_and_delay(data, retry)?;
    let turn = required_u64(data.get("turn"), "llm/retry turn")?;
    let step = required_u64(data.get("step"), "llm/retry step")?;
    validate_open_location(history, turn, step, provider)?;
    validate_chain(history, turn, step, provider, policy_key, retry_id, retry)
}

fn validate_mode_and_delay(
    data: &serde_json::Map<String, Value>,
    retry: u64,
) -> anyhow::Result<()> {
    match data.get("mode").and_then(Value::as_str) {
        Some("normal") => {
            let maximum = data
                .get("maxRetries")
                .and_then(Value::as_u64)
                .filter(|maximum| (1..=MAX_SAFE_INTEGER).contains(maximum));
            anyhow::ensure!(
                maximum.is_some_and(|maximum| retry <= maximum),
                "llm/retry retry {retry} must not exceed a positive safe maxRetries {}",
                display_value(data.get("maxRetries"))
            );
        }
        Some("always") => anyhow::ensure!(
            !data.contains_key("maxRetries"),
            "llm/retry always mode must omit maxRetries"
        ),
        mode => anyhow::bail!(
            "llm/retry mode must be normal or always, got {}",
            mode.map_or_else(|| display_value(data.get("mode")), ToOwned::to_owned)
        ),
    }
    let delay = data.get("delayMs").and_then(Value::as_f64);
    anyhow::ensure!(
        delay.is_some_and(|delay| {
            delay.is_finite() && (0.0..=MAX_TIMER_DELAY_MS).contains(&delay)
        }),
        "llm/retry delayMs must be a finite number within 0..{}",
        2_147_483_647_u64
    );
    Ok(())
}

fn validate_open_location(
    history: &[SessionEvent],
    turn: u64,
    step: u64,
    provider: &str,
) -> anyhow::Result<()> {
    let turn_boundary = history
        .iter()
        .rev()
        .find(|prior| matches!(prior.event_type.as_str(), "turn/start" | "turn/end"));
    anyhow::ensure!(
        turn_boundary.is_some_and(|boundary| boundary.event_type == "turn/start"),
        "llm/retry must be appended inside an open turn"
    );
    let open_turn = turn_boundary
        .and_then(|boundary| boundary.data.get("turn"))
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow::anyhow!("open turn has no valid turn number"))?;
    anyhow::ensure!(
        turn == open_turn,
        "llm/retry names turn {turn}, but the open turn is {open_turn}"
    );

    let step_boundary = history
        .iter()
        .rev()
        .find(|prior| matches!(prior.event_type.as_str(), "step/start" | "step/end"));
    anyhow::ensure!(
        step_boundary.is_some_and(|boundary| boundary.event_type == "step/start"),
        "llm/retry must be appended inside an open step"
    );
    let boundary = step_boundary
        .ok_or_else(|| anyhow::anyhow!("llm/retry must be appended inside an open step"))?;
    let open_step_turn = required_u64(boundary.data.get("turn"), "open step turn")?;
    let open_step = required_u64(boundary.data.get("step"), "open step number")?;
    anyhow::ensure!(
        turn == open_step_turn && step == open_step,
        "llm/retry names turn {turn}/step {step}, but the open step is {open_step_turn}/{open_step}"
    );
    let routed = provider_for_open_step(history, turn, step);
    anyhow::ensure!(
        routed.as_ref().map(seekdeep_llm::ProviderId::as_str) == Some(provider),
        "llm/retry provider {provider} does not match the failed request provider {}",
        routed
            .as_ref()
            .map_or("undefined", seekdeep_llm::ProviderId::as_str)
    );
    Ok(())
}

fn validate_chain(
    history: &[SessionEvent],
    turn: u64,
    step: u64,
    provider: &str,
    policy_key: &str,
    retry_id: &str,
    retry: u64,
) -> anyhow::Result<()> {
    let prior_policy = history.iter().rev().find(|prior| {
        prior.event_type == "llm/retry"
            && prior.data.get("turn").and_then(Value::as_u64) == Some(turn)
            && prior.data.get("step").and_then(Value::as_u64) == Some(step)
            && prior.data.get("provider").and_then(Value::as_str) == Some(provider)
            && prior.data.get("policyKey").and_then(Value::as_str) == Some(policy_key)
    });
    let expected = prior_policy
        .and_then(|prior| prior.data.get("retry"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
        + 1;
    anyhow::ensure!(
        retry == expected,
        "llm/retry retry {retry} must equal provider policy retry {expected}"
    );
    if let Some(prior) = prior_policy {
        anyhow::ensure!(
            prior.data.get("retryId").and_then(Value::as_str) == Some(retry_id),
            "llm/retry must preserve retryId across one provider-policy chain"
        );
    } else {
        anyhow::ensure!(
            !history.iter().any(|prior| {
                matches!(prior.event_type.as_str(), "llm/retry" | "llm/retry-started")
                    && prior.data.get("retryId").and_then(Value::as_str) == Some(retry_id)
            }),
            "llm/retry retryId {retry_id:?} is already owned by another chain"
        );
    }
    Ok(())
}

fn validate_started(history: &[SessionEvent], event: &SessionEvent) -> anyhow::Result<()> {
    let data = event
        .data
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("llm/retry-started data must be an object"))?;
    let retry_id = required_nonempty_string(data.get("retryId"), "llm/retry-started retryId")?;
    let retry = positive_safe_integer(data.get("retry"), "llm/retry-started retry")?;
    let turn = required_u64(data.get("turn"), "llm/retry-started turn")?;
    let step = required_u64(data.get("step"), "llm/retry-started step")?;
    let scheduled = history.iter().rev().find(|prior| {
        prior.event_type == "llm/retry"
            && prior.data.get("retryId").and_then(Value::as_str) == Some(retry_id)
            && prior.data.get("retry").and_then(Value::as_u64) == Some(retry)
    });
    let scheduled = scheduled
        .ok_or_else(|| anyhow::anyhow!("llm/retry-started pairs no prior scheduled attempt"))?;
    anyhow::ensure!(
        scheduled.data.get("turn").and_then(Value::as_u64) == Some(turn)
            && scheduled.data.get("step").and_then(Value::as_u64) == Some(step),
        "llm/retry-started turn/step must match its scheduled attempt"
    );
    anyhow::ensure!(
        !history.iter().any(|prior| {
            prior.event_type == "llm/retry-started"
                && prior.data.get("retryId").and_then(Value::as_str) == Some(retry_id)
                && prior.data.get("retry").and_then(Value::as_u64) == Some(retry)
        }),
        "llm/retry-started repeats one scheduled attempt"
    );
    Ok(())
}

fn required_nonempty_string<'a>(value: Option<&'a Value>, path: &str) -> anyhow::Result<&'a str> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("{path} must be a non-empty string"))
}

fn required_u64(value: Option<&Value>, path: &str) -> anyhow::Result<u64> {
    value
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow::anyhow!("{path} must be a non-negative integer"))
}

fn positive_safe_integer(value: Option<&Value>, path: &str) -> anyhow::Result<u64> {
    value
        .and_then(Value::as_u64)
        .filter(|value| (1..=MAX_SAFE_INTEGER).contains(value))
        .ok_or_else(|| anyhow::anyhow!("{path} must be a positive safe integer"))
}

fn display_value(value: Option<&Value>) -> String {
    value.map_or_else(|| "undefined".to_owned(), Value::to_string)
}
