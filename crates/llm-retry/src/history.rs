//! Durable request-route and retry-chain lookup.

use seekdeep_core::session::SessionEvent;
use seekdeep_llm::ProviderId;

use crate::{brand::RetryPolicyKey, types::LlmRetryEventData};

/// Finds the provider in force for one currently open step.
#[must_use]
pub(crate) fn provider_for_open_step(
    events: &[SessionEvent],
    turn: u64,
    step: u64,
) -> Option<ProviderId> {
    let step_start = events.iter().rposition(|event| {
        event.event_type == "step/start"
            && event.data.get("turn").and_then(serde_json::Value::as_u64) == Some(turn)
            && event.data.get("step").and_then(serde_json::Value::as_u64) == Some(step)
    })?;
    if events[step_start + 1..]
        .iter()
        .any(|event| matches!(event.event_type.as_str(), "step/end" | "turn/end"))
    {
        return None;
    }
    events.iter().rev().find_map(|event| {
        (event.event_type == "request/header")
            .then(|| {
                event
                    .data
                    .pointer("/header/config/provider")
                    .and_then(serde_json::Value::as_str)
                    .filter(|provider| !provider.is_empty())
                    .map(ProviderId::new)
            })
            .flatten()
    })
}

/// Finds the latest retry in one exact open-step provider-policy chain.
#[must_use]
pub(crate) fn prior_policy_retry(
    events: &[SessionEvent],
    turn: u64,
    step: u64,
    provider: &ProviderId,
    policy_key: &RetryPolicyKey,
) -> Option<LlmRetryEventData> {
    events.iter().rev().find_map(|event| {
        if event.event_type != "llm/retry" {
            return None;
        }
        let retry = serde_json::from_value::<LlmRetryEventData>(event.data.clone()).ok()?;
        (retry.turn() == turn
            && retry.step() == step
            && retry.provider() == provider
            && retry.policy_key() == policy_key)
            .then_some(retry)
    })
}

#[cfg(test)]
mod tests {
    use seekdeep_core::session::{AppendOptions, Session, SessionId};
    use serde_json::json;

    use super::*;

    #[test]
    fn route_lookup_requires_the_exact_open_step_and_latest_header() {
        assert_eq!(provider_for_open_step(&[], 1, 1), None);
        let no_route = Session::create(&SessionId::new("no-route"), None, None).unwrap();
        no_route
            .append(
                "step/start",
                json!({"turn":1,"step":1}),
                AppendOptions::default(),
            )
            .unwrap();
        assert_eq!(provider_for_open_step(&no_route.events(), 1, 1), None);
        let session = Session::create(&SessionId::new("route"), None, None).unwrap();
        for (event_type, data) in [
            ("turn/start", json!({"turn":1})),
            ("step/start", json!({"turn":1,"step":1})),
            (
                "request/header",
                json!({"header":{"config":{"provider":"mock","model":"mock"}},"reason":"initial"}),
            ),
        ] {
            session
                .append(event_type, data, AppendOptions::default())
                .unwrap();
        }
        assert_eq!(
            provider_for_open_step(&session.events(), 1, 1)
                .unwrap()
                .as_str(),
            "mock"
        );
        session
            .append(
                "step/end",
                json!({"turn":1,"step":1}),
                AppendOptions::default(),
            )
            .unwrap();
        assert_eq!(provider_for_open_step(&session.events(), 1, 1), None);
    }
}
