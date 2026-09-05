//! Provider-routed retry policy on the agent request-error waterfall.

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use parking_lot::Mutex;
use seekdeep_agent::{AgentEvent, RequestErrorAction};
use seekdeep_agent_loop::AgentRequestErrorEvent;
use seekdeep_cordis::{
    Context, EventOptions, EventReply, Plugin,
    events::Next,
    fiber::{DisposeFuture, EffectHandle},
};
use seekdeep_core::session::AppendOptions;
use seekdeep_llm::{AbortSignal, ResolvedRetryPolicy, RetryPolicyMode};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    brand::{RetryId, RetryPolicyKey},
    history::prior_policy_retry,
    types::{LlmRetryEventData, LlmRetryStartedEventData},
};

/// Cordis plugin name.
pub const NAME: &str = "llm-retry";
const INJECT: &[&str] = &["agents"];

/// The executor has no configuration; providers own retry policies.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetryConfig {}

/// Process-local deterministic seams.
#[derive(Clone)]
pub struct RetryInternals {
    random: Arc<dyn Fn() -> f64 + Send + Sync + 'static>,
    random_retry_id: Arc<dyn Fn() -> RetryId + Send + Sync + 'static>,
}

impl std::fmt::Debug for RetryInternals {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RetryInternals")
            .field("random", &"<function>")
            .field("random_retry_id", &"<function>")
            .finish()
    }
}

impl Default for RetryInternals {
    fn default() -> Self {
        Self {
            random: Arc::new(|| {
                let bytes = uuid::Uuid::new_v4().into_bytes();
                let sample = u32::from_be_bytes(bytes[..4].try_into().unwrap_or_default());
                f64::from(sample) / f64::from(u32::MAX)
            }),
            random_retry_id: Arc::new(|| RetryId::new(uuid::Uuid::new_v4().to_string())),
        }
    }
}

impl RetryInternals {
    /// Creates deterministic timing and identity seams for tests or embedding.
    #[must_use]
    pub fn new(
        random: impl Fn() -> f64 + Send + Sync + 'static,
        random_retry_id: impl Fn() -> RetryId + Send + Sync + 'static,
    ) -> Self {
        Self {
            random: Arc::new(random),
            random_retry_id: Arc::new(random_retry_id),
        }
    }
}

#[derive(Debug)]
struct RuntimeState {
    lifetime: AbortSignal,
    active: AtomicUsize,
    idle: tokio::sync::Notify,
    internals: RetryInternals,
    warnings: Mutex<Vec<String>>,
}

impl RuntimeState {
    fn enter(self: &Arc<Self>) -> ActiveGuard {
        self.active.fetch_add(1, Ordering::AcqRel);
        ActiveGuard(self.clone())
    }

    async fn drain(&self) {
        loop {
            let notified = self.idle.notified();
            if self.active.load(Ordering::Acquire) == 0 {
                return;
            }
            notified.await;
        }
    }
}

struct ActiveGuard(Arc<RuntimeState>);

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        if self.0.active.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.0.idle.notify_waiters();
        }
    }
}

/// Computes bounded exponential backoff with symmetric jitter.
#[must_use]
fn local_delay(policy: &ResolvedRetryPolicy, retry: u64, random: f64) -> f64 {
    let exponent = retry.saturating_sub(1).min(1_024);
    let exponential = (policy.initial_delay_ms()
        * 2_f64.powi(i32::try_from(exponent).unwrap_or(1_024)))
    .min(policy.max_delay_ms());
    let jitter = 1.0 - policy.jitter_ratio() + 2.0 * policy.jitter_ratio() * random;
    (exponential * jitter).min(policy.max_delay_ms())
}

/// Returns the canonical source-compatible resolved-policy identity.
#[must_use]
fn retry_policy_key(policy: &ResolvedRetryPolicy) -> RetryPolicyKey {
    let number = |value: f64| ryu_js::Buffer::new().format(value).to_owned();
    let raw = match policy.mode() {
        RetryPolicyMode::Normal => {
            let mut codes = policy.retryable_codes().unwrap_or_default().to_vec();
            codes.sort();
            format!(
                "[\"normal\",{}, {},{},{},{}]",
                policy.max_retries().unwrap_or_default(),
                serde_json::to_string(&codes).unwrap_or_else(|_| "[]".to_owned()),
                number(policy.initial_delay_ms()),
                number(policy.max_delay_ms()),
                number(policy.jitter_ratio()),
            )
            .replace(", ", ",")
        }
        RetryPolicyMode::Always => format!(
            "[\"always\",{},{},{}]",
            number(policy.initial_delay_ms()),
            number(policy.max_delay_ms()),
            number(policy.jitter_ratio()),
        ),
    };
    RetryPolicyKey::new(raw)
}

/// Builds the default runtime plugin.
#[must_use]
pub fn plugin() -> Plugin {
    plugin_with_internals(RetryInternals::default())
}

/// Builds a runtime plugin over deterministic process-local seams.
#[must_use]
pub fn plugin_with_internals(internals: RetryInternals) -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, config| {
        let internals = internals.clone();
        Box::pin(async move {
            validate_config_value(&config)?;
            install_listener(&context, internals)?;
            Ok(())
        })
    })
    .with_config_validator(|value| {
        validate_config_value(value)?;
        Ok(json!({}))
    })
}

fn validate_config_value(value: &serde_json::Value) -> anyhow::Result<()> {
    let config = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("llm-retry: config must be an object"))?;
    let Some(key) = config.keys().next() else {
        return Ok(());
    };
    if key == "retryPolicy" {
        anyhow::bail!("llm-retry: retryPolicy belongs under each provider configuration");
    }
    anyhow::bail!("llm-retry: unknown key {key:?}")
}

/// Installs the default plugin as one lifecycle-owned fiber.
///
/// # Errors
///
/// Returns configuration serialization or inactive-context failures.
pub fn install(
    context: &Context,
    config: RetryConfig,
) -> anyhow::Result<Arc<seekdeep_cordis::PluginFiber>> {
    Ok(context.plugin(plugin(), serde_json::to_value(config)?)?)
}

/// Installs deterministic runtime seams as one lifecycle-owned fiber.
///
/// # Errors
///
/// Returns configuration serialization or inactive-context failures.
pub fn install_with_internals(
    context: &Context,
    config: RetryConfig,
    internals: RetryInternals,
) -> anyhow::Result<Arc<seekdeep_cordis::PluginFiber>> {
    Ok(context.plugin(
        plugin_with_internals(internals),
        serde_json::to_value(config)?,
    )?)
}

fn install_listener(context: &Context, internals: RetryInternals) -> anyhow::Result<()> {
    let state = Arc::new(RuntimeState {
        lifetime: AbortSignal::default(),
        active: AtomicUsize::new(0),
        idle: tokio::sync::Notify::new(),
        internals,
        warnings: Mutex::new(Vec::new()),
    });
    let listener_state = state.clone();
    let listener = context.events().on_waterfall(
        context,
        "agent/request-error",
        move |_, args, next| {
            if listener_state.lifetime.is_aborted() {
                return Box::pin(async {
                    Ok(EventReply::Value(Arc::new(RequestErrorAction::Terminal)))
                });
            }
            let state = listener_state.clone();
            Box::pin(async move {
                let _active = state.enter();
                let event = args
                    .get::<AgentEvent<AgentRequestErrorEvent>>(0)
                    .ok_or_else(|| anyhow::anyhow!("agent/request-error lacks its agent event"))?;
                recover(&state, &event, next).await
            })
        },
        EventOptions::default(),
    )?;
    let cleanup_state = state;
    let cleanup_listener = listener;
    context.own(EffectHandle::new(
        "llm-retry: abort and drain active recovery",
        move || -> DisposeFuture {
            Box::pin(async move {
                cleanup_listener.dispose().await?;
                cleanup_state.lifetime.abort_with_reason(json!({
                    "name": "Error",
                    "message": "llm-retry plugin disposed"
                }));
                cleanup_state.drain().await;
                Ok(())
            })
        },
    ))?;
    Ok(())
}

async fn recover(
    state: &Arc<RuntimeState>,
    event: &AgentEvent<AgentRequestErrorEvent>,
    next: Next,
) -> anyhow::Result<EventReply> {
    let mut downstream_next = Some(next);
    let Some(policy) = event.payload.retry_policy.as_ref() else {
        return downstream_next
            .expect("downstream is available")
            .run()
            .await;
    };
    if policy.mode() == RetryPolicyMode::Always {
        if event.payload.signal.is_aborted() || state.lifetime.is_aborted() {
            return Ok(terminal_reply());
        }
        let fused = AbortSignal::fuse(&event.payload.signal, &state.lifetime);
        let downstream = downstream_next
            .take()
            .expect("always policy delegates exactly once")
            .run()
            .await;
        if fused.is_aborted() {
            return Ok(terminal_reply());
        }
        match downstream {
            Ok(reply)
                if reply.downcast::<RequestErrorAction>().as_deref()
                    == Some(&RequestErrorAction::Retry) =>
            {
                return Ok(reply);
            }
            Ok(_) => {}
            Err(error) => {
                let warning = format!(
                    "llm-retry: provider \"{}\" always policy ignored a downstream recovery failure: {error:#}",
                    event.payload.provider
                );
                tracing::warn!("{warning}");
                state.warnings.lock().push(warning);
            }
        }
    } else if !policy
        .retryable_codes()
        .expect("normal policy has retryable codes")
        .iter()
        .any(|code| code == &event.payload.failure.code)
    {
        return downstream_next
            .expect("normal delegation is available")
            .run()
            .await;
    }

    let policy_key = retry_policy_key(policy);
    let prior = prior_policy_retry(
        &event.agent.session().events(),
        event.payload.turn,
        event.payload.step,
        &event.payload.provider,
        &policy_key,
    );
    let previous_retry = prior.as_ref().map_or(0, LlmRetryEventData::retry);
    if policy.mode() == RetryPolicyMode::Normal
        && previous_retry >= policy.max_retries().expect("normal policy has a budget")
    {
        return downstream_next
            .expect("normal delegation is available")
            .run()
            .await;
    }
    let retry = previous_retry + 1;
    let retry_id = prior.map_or_else(
        || (state.internals.random_retry_id)(),
        |prior| prior.retry_id().clone(),
    );
    let local = || local_delay(policy, retry, (state.internals.random)());
    let delay_ms = match event.payload.failure.provider_retry_after_ms {
        Some(provider_delay)
            if provider_delay.is_finite()
                && provider_delay > 0.0
                && provider_delay <= policy.max_delay_ms() =>
        {
            provider_delay
        }
        Some(provider_delay)
            if provider_delay.is_finite()
                && provider_delay > policy.max_delay_ms()
                && policy.mode() == RetryPolicyMode::Normal =>
        {
            return downstream_next
                .expect("normal delegation is available")
                .run()
                .await;
        }
        _ => local(),
    };
    backoff(state, event, policy, policy_key, retry, retry_id, delay_ms).await
}

async fn backoff(
    state: &Arc<RuntimeState>,
    event: &AgentEvent<AgentRequestErrorEvent>,
    policy: &ResolvedRetryPolicy,
    policy_key: RetryPolicyKey,
    retry: u64,
    retry_id: RetryId,
    delay_ms: f64,
) -> anyhow::Result<EventReply> {
    let fused = AbortSignal::fuse(&event.payload.signal, &state.lifetime);
    if fused.is_aborted() {
        return Ok(terminal_reply());
    }
    let data = match policy.mode() {
        RetryPolicyMode::Normal => LlmRetryEventData::Normal {
            retry_id: retry_id.clone(),
            turn: event.payload.turn,
            step: event.payload.step,
            provider: event.payload.provider.clone(),
            policy_key,
            retry,
            max_retries: policy.max_retries().expect("normal policy has a budget"),
            delay_ms,
            failure: event.payload.failure.clone(),
        },
        RetryPolicyMode::Always => LlmRetryEventData::Always {
            retry_id: retry_id.clone(),
            turn: event.payload.turn,
            step: event.payload.step,
            provider: event.payload.provider.clone(),
            policy_key,
            retry,
            delay_ms,
            failure: event.payload.failure.clone(),
        },
    };
    event.agent.session().append(
        "llm/retry",
        serde_json::to_value(data)?,
        AppendOptions::default(),
    )?;
    let duration = Duration::try_from_secs_f64(delay_ms / 1_000.0)
        .map_err(|_| anyhow::anyhow!("llm-retry produced an invalid delay {delay_ms}"))?;
    tokio::select! {
        biased;
        () = fused.cancelled() => Ok(terminal_reply()),
        () = tokio::time::sleep(duration) => {
            event.agent.session().append(
                "llm/retry-started",
                serde_json::to_value(LlmRetryStartedEventData {
                    retry_id,
                    turn: event.payload.turn,
                    step: event.payload.step,
                    retry,
                })?,
                AppendOptions::default(),
            )?;
            Ok(EventReply::Value(Arc::new(RequestErrorAction::Retry)))
        }
    }
}

fn terminal_reply() -> EventReply {
    EventReply::Value(Arc::new(RequestErrorAction::Terminal))
}

#[cfg(test)]
mod tests {
    use seekdeep_llm::resolve_retry_policy;

    use super::*;

    #[test]
    fn policy_key_matches_javascript_json_stringification() {
        let normal = resolve_retry_policy(
            Some(&json!({
                "mode":"normal",
                "maxRetries":2,
                "retryableCodes":["SERVER","RATE_LIMIT"],
                "backoff":{"initialDelayMs":500,"maxDelayMs":10000,"jitterRatio":0}
            })),
            "retryPolicy",
        )
        .unwrap();
        assert_eq!(
            retry_policy_key(&normal).as_str(),
            "[\"normal\",2,[\"RATE_LIMIT\",\"SERVER\"],500,10000,0]"
        );
        assert!((local_delay(&normal, 1, 0.0) - 500.0).abs() < f64::EPSILON);
        assert!((local_delay(&normal, 2, 0.5) - 1_000.0).abs() < f64::EPSILON);
        assert!((local_delay(&normal, 20, 1.0) - 10_000.0).abs() < f64::EPSILON);

        let jittered = resolve_retry_policy(
            Some(&json!({
                "mode":"normal",
                "maxRetries":2,
                "retryableCodes":["SERVER"],
                "backoff":{"initialDelayMs":500,"maxDelayMs":10000,"jitterRatio":0.1}
            })),
            "retryPolicy",
        )
        .unwrap();
        assert!((local_delay(&jittered, 1, 0.0) - 450.0).abs() < f64::EPSILON);
        assert!((local_delay(&jittered, 2, 1.0) - 1_100.0).abs() < f64::EPSILON);
        assert!((local_delay(&jittered, 20, 1.0) - 10_000.0).abs() < f64::EPSILON);
    }

    #[test]
    fn executor_config_rejects_provider_policy_and_unknown_keys_precisely() {
        assert!(validate_config_value(&json!({})).is_ok());
        assert_eq!(
            validate_config_value(&json!({"retryPolicy":{}}))
                .unwrap_err()
                .to_string(),
            "llm-retry: retryPolicy belongs under each provider configuration"
        );
        assert_eq!(
            validate_config_value(&json!({"mystery":true}))
                .unwrap_err()
                .to_string(),
            "llm-retry: unknown key \"mystery\""
        );
    }
}
