//! Behavioral mirror of provider-routed retry scheduling.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use seekdeep_agent::{
    Agent, AgentEvents, AgentOptions, AgentRegistry, Inbox, NoopInboxNotifications,
    RequestErrorAction,
};
use seekdeep_agent_loop::AgentRequestErrorEvent;
use seekdeep_cordis::{Context, EventOptions};
use seekdeep_core::session::{AppendOptions, Session, SessionId};
use seekdeep_llm::{
    AbortSignal, LlmFailure, ProviderId, ResolvedRetryPolicy, resolve_retry_policy,
};
use seekdeep_llm_retry::{RetryConfig, RetryId, RetryInternals, install_with_internals};
use seekdeep_scope::ScopeKey;
use serde_json::{Value, json};

struct Harness {
    context: Context,
    agent: Arc<Agent>,
    plugin: Arc<seekdeep_cordis::PluginFiber>,
}

impl Harness {
    async fn new(random: f64) -> Self {
        let context = Context::new();
        let agents = Arc::new(AgentRegistry::new(context.clone()));
        agents.provide(&context).unwrap();
        let session = Session::create(&SessionId::new("retry-runtime"), None, None).unwrap();
        for (event_type, data) in [
            ("turn/start", json!({"turn":1})),
            ("step/start", json!({"turn":1,"step":1})),
            (
                "request/header",
                json!({"header":{"config":{"provider":"mock","model":"model"}},"reason":"initial"}),
            ),
        ] {
            session
                .append(event_type, data, AppendOptions::default())
                .unwrap();
        }
        let inbox =
            Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
        let agent = Arc::new(Agent::new(
            SessionId::new("retry-runtime"),
            AgentOptions::default(),
            session,
            inbox,
            context.clone(),
            ScopeKey::new(),
        ));
        let minted = Arc::new(AtomicUsize::new(0));
        let minted_for_ids = minted.clone();
        let plugin = install_with_internals(
            &context,
            RetryConfig::default(),
            RetryInternals::new(
                move || random,
                move || {
                    let index = minted_for_ids.fetch_add(1, Ordering::AcqRel);
                    if index == 0 {
                        RetryId::new("deterministic-retry-chain")
                    } else {
                        RetryId::new(format!("deterministic-retry-chain-{}", index + 1))
                    }
                },
            ),
        )
        .unwrap();
        plugin.await_settled().await.unwrap();
        Self {
            context,
            agent,
            plugin,
        }
    }

    async fn dispatch(
        &self,
        policy: Option<ResolvedRetryPolicy>,
        failure: LlmFailure,
        signal: AbortSignal,
        downstream: anyhow::Result<RequestErrorAction>,
        calls: Arc<AtomicUsize>,
    ) -> anyhow::Result<RequestErrorAction> {
        self.dispatch_for("mock", policy, failure, signal, downstream, calls)
            .await
    }

    async fn dispatch_for(
        &self,
        provider: &str,
        policy: Option<ResolvedRetryPolicy>,
        failure: LlmFailure,
        signal: AbortSignal,
        downstream: anyhow::Result<RequestErrorAction>,
        calls: Arc<AtomicUsize>,
    ) -> anyhow::Result<RequestErrorAction> {
        AgentEvents::new(self.context.clone(), self.agent.clone())
            .waterfall(
                "agent/request-error",
                AgentRequestErrorEvent {
                    turn: 1,
                    step: 1,
                    provider: ProviderId::new(provider),
                    failure,
                    retry_policy: policy,
                    signal,
                },
                move || async move {
                    calls.fetch_add(1, Ordering::AcqRel);
                    downstream
                },
            )
            .await
    }

    fn retry_events(&self) -> Vec<Value> {
        self.agent
            .session()
            .events()
            .into_iter()
            .filter(|event| event.event_type == "llm/retry")
            .map(|event| event.data)
            .collect()
    }

    async fn close(self) {
        self.plugin.dispose().await.unwrap();
        self.context.fiber().dispose().await.unwrap();
    }
}

fn normal(max_retries: u64, codes: &[&str], max_delay_ms: f64) -> ResolvedRetryPolicy {
    resolve_retry_policy(
        Some(&json!({
            "mode":"normal",
            "maxRetries":max_retries,
            "retryableCodes":codes,
            "backoff":{
                "initialDelayMs":1,
                "maxDelayMs":max_delay_ms,
                "jitterRatio":1
            }
        })),
        "retryPolicy",
    )
    .unwrap()
}

fn always(max_delay_ms: f64) -> ResolvedRetryPolicy {
    resolve_retry_policy(
        Some(&json!({
            "mode":"always",
            "backoff":{
                "initialDelayMs":1,
                "maxDelayMs":max_delay_ms,
                "jitterRatio":1
            }
        })),
        "retryPolicy",
    )
    .unwrap()
}

fn failure(code: &str, retry_after: Option<f64>) -> LlmFailure {
    LlmFailure {
        message: "provider busy".to_owned(),
        code: code.to_owned(),
        status: Some(429),
        provider_retry_after_ms: retry_after,
        request_id: None,
    }
}

#[tokio::test]
async fn records_schedule_then_started_before_retrying_without_opening_a_new_step() {
    let harness = Harness::new(0.0).await;
    let downstream = Arc::new(AtomicUsize::new(0));
    let action = harness
        .dispatch(
            Some(normal(2, &["RATE_LIMIT", "SERVER"], 10_000.0)),
            failure("RATE_LIMIT", None),
            AbortSignal::default(),
            Ok(RequestErrorAction::Terminal),
            downstream.clone(),
        )
        .await
        .unwrap();
    assert_eq!(action, RequestErrorAction::Retry);
    assert_eq!(downstream.load(Ordering::Acquire), 0);
    let events = harness.agent.session().events();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "step/start")
            .count(),
        1
    );
    let retry = events
        .iter()
        .find(|event| event.event_type == "llm/retry")
        .unwrap();
    assert_eq!(
        retry.data,
        json!({
            "retryId":"deterministic-retry-chain",
            "turn":1,
            "step":1,
            "provider":"mock",
            "mode":"normal",
            "policyKey":"[\"normal\",2,[\"RATE_LIMIT\",\"SERVER\"],1,10000,1]",
            "retry":1,
            "maxRetries":2,
            "delayMs":0,
            "failure":{"message":"provider busy","code":"RATE_LIMIT","status":429}
        })
    );
    let started = events
        .iter()
        .find(|event| event.event_type == "llm/retry-started")
        .unwrap();
    assert_eq!(
        started.data,
        json!({"retryId":"deterministic-retry-chain","turn":1,"step":1,"retry":1})
    );
    harness.close().await;
}

#[tokio::test]
async fn finite_budget_and_code_membership_delegate_exactly_once() {
    let harness = Harness::new(0.0).await;
    let calls = Arc::new(AtomicUsize::new(0));
    for expected in [RequestErrorAction::Retry, RequestErrorAction::Retry] {
        assert_eq!(
            harness
                .dispatch(
                    Some(normal(2, &["RATE_LIMIT"], 1.0)),
                    failure("RATE_LIMIT", None),
                    AbortSignal::default(),
                    Ok(RequestErrorAction::Terminal),
                    calls.clone(),
                )
                .await
                .unwrap(),
            expected
        );
    }
    assert_eq!(harness.retry_events().len(), 2);
    assert_eq!(
        harness
            .dispatch(
                Some(normal(2, &["RATE_LIMIT"], 1.0)),
                failure("RATE_LIMIT", None),
                AbortSignal::default(),
                Ok(RequestErrorAction::Terminal),
                calls.clone(),
            )
            .await
            .unwrap(),
        RequestErrorAction::Terminal
    );
    assert_eq!(
        harness
            .dispatch(
                Some(normal(2, &["RATE_LIMIT"], 1.0)),
                failure("AUTH", None),
                AbortSignal::default(),
                Ok(RequestErrorAction::Terminal),
                calls.clone(),
            )
            .await
            .unwrap(),
        RequestErrorAction::Terminal
    );
    assert_eq!(calls.load(Ordering::Acquire), 2);
    harness.close().await;
}

#[tokio::test]
async fn provider_and_complete_policy_identity_scope_retry_budgets_and_chain_ids() {
    let harness = Harness::new(0.0).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let one_retry = normal(1, &["SERVER"], 1.0);
    for provider in ["mock", "other"] {
        assert_eq!(
            harness
                .dispatch_for(
                    provider,
                    Some(one_retry.clone()),
                    failure("SERVER", None),
                    AbortSignal::default(),
                    Ok(RequestErrorAction::Terminal),
                    calls.clone(),
                )
                .await
                .unwrap(),
            RequestErrorAction::Retry
        );
    }
    assert_eq!(
        harness
            .dispatch_for(
                "mock",
                Some(one_retry),
                failure("SERVER", None),
                AbortSignal::default(),
                Ok(RequestErrorAction::Terminal),
                calls.clone(),
            )
            .await
            .unwrap(),
        RequestErrorAction::Terminal
    );

    let changed_policy = normal(2, &["SERVER"], 2.0);
    assert_eq!(
        harness
            .dispatch_for(
                "mock",
                Some(changed_policy),
                failure("SERVER", None),
                AbortSignal::default(),
                Ok(RequestErrorAction::Terminal),
                calls.clone(),
            )
            .await
            .unwrap(),
        RequestErrorAction::Retry
    );
    let events = harness.retry_events();
    assert_eq!(events.len(), 3);
    assert_eq!(
        events
            .iter()
            .map(|event| (
                event["provider"].as_str().unwrap(),
                event["retry"].as_u64().unwrap(),
            ))
            .collect::<Vec<_>>(),
        vec![("mock", 1), ("other", 1), ("mock", 1)]
    );
    assert_ne!(events[0]["retryId"], events[1]["retryId"]);
    assert_ne!(events[0]["retryId"], events[2]["retryId"]);
    assert_ne!(events[0]["policyKey"], events[2]["policyKey"]);
    assert_eq!(calls.load(Ordering::Acquire), 1);
    harness.close().await;
}

#[tokio::test]
async fn always_mode_remains_unbounded_with_capped_exponential_jitter() {
    let harness = Harness::new(1.0).await;
    let policy = resolve_retry_policy(
        Some(&json!({
            "mode":"always",
            "backoff":{"initialDelayMs":1,"maxDelayMs":4,"jitterRatio":0.1}
        })),
        "retryPolicy",
    )
    .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    for _ in 0..4 {
        assert_eq!(
            harness
                .dispatch(
                    Some(policy.clone()),
                    failure("AUTH", None),
                    AbortSignal::default(),
                    Ok(RequestErrorAction::Terminal),
                    calls.clone(),
                )
                .await
                .unwrap(),
            RequestErrorAction::Retry
        );
    }
    let events = harness.retry_events();
    assert_eq!(events.len(), 4);
    let delays = events
        .iter()
        .map(|event| event["delayMs"].as_f64().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(delays, vec![1.1, 2.2, 4.0, 4.0]);
    assert!(events.iter().all(|event| event.get("maxRetries").is_none()));
    assert_eq!(calls.load(Ordering::Acquire), 4);
    harness.close().await;
}

#[tokio::test]
async fn retry_after_is_exact_when_bounded_and_delegates_normal_over_cap() {
    let harness = Harness::new(0.5).await;
    let calls = Arc::new(AtomicUsize::new(0));
    assert_eq!(
        harness
            .dispatch(
                Some(normal(2, &["RATE_LIMIT"], 100.0)),
                failure("RATE_LIMIT", Some(75.0)),
                AbortSignal::default(),
                Ok(RequestErrorAction::Terminal),
                calls.clone(),
            )
            .await
            .unwrap(),
        RequestErrorAction::Retry
    );
    assert_eq!(harness.retry_events()[0]["delayMs"], 75.0);
    assert_eq!(
        harness
            .dispatch(
                Some(normal(2, &["RATE_LIMIT"], 100.0)),
                failure("RATE_LIMIT", Some(101.0)),
                AbortSignal::default(),
                Ok(RequestErrorAction::Terminal),
                calls.clone(),
            )
            .await
            .unwrap(),
        RequestErrorAction::Terminal
    );
    assert_eq!(calls.load(Ordering::Acquire), 1);
    harness.close().await;
}

#[tokio::test]
async fn always_mode_prefers_downstream_retry_and_falls_back_after_downstream_error() {
    let harness = Harness::new(0.0).await;
    let calls = Arc::new(AtomicUsize::new(0));
    assert_eq!(
        harness
            .dispatch(
                Some(always(10.0)),
                failure("AUTH", None),
                AbortSignal::default(),
                Ok(RequestErrorAction::Retry),
                calls.clone(),
            )
            .await
            .unwrap(),
        RequestErrorAction::Retry
    );
    assert!(harness.retry_events().is_empty());
    assert_eq!(
        harness
            .dispatch(
                Some(always(10.0)),
                failure("AUTH", Some(100.0)),
                AbortSignal::default(),
                Err(anyhow::anyhow!("specialized recovery failed")),
                calls.clone(),
            )
            .await
            .unwrap(),
        RequestErrorAction::Retry
    );
    assert_eq!(harness.retry_events().len(), 1);
    assert_eq!(harness.retry_events()[0]["delayMs"], 0.0);
    assert_eq!(calls.load(Ordering::Acquire), 2);
    harness.close().await;
}

#[tokio::test]
async fn missing_policy_and_preaborted_always_policy_fail_closed() {
    let harness = Harness::new(0.0).await;
    let calls = Arc::new(AtomicUsize::new(0));
    assert_eq!(
        harness
            .dispatch(
                None,
                failure("RATE_LIMIT", None),
                AbortSignal::default(),
                Ok(RequestErrorAction::Terminal),
                calls.clone(),
            )
            .await
            .unwrap(),
        RequestErrorAction::Terminal
    );
    let signal = AbortSignal::default();
    signal.abort();
    assert_eq!(
        harness
            .dispatch(
                Some(normal(1, &["RATE_LIMIT"], 1.0)),
                failure("RATE_LIMIT", None),
                signal.clone(),
                Ok(RequestErrorAction::Retry),
                calls.clone(),
            )
            .await
            .unwrap(),
        RequestErrorAction::Terminal
    );
    assert_eq!(
        harness
            .dispatch(
                Some(always(1.0)),
                failure("AUTH", None),
                signal,
                Ok(RequestErrorAction::Retry),
                calls.clone(),
            )
            .await
            .unwrap(),
        RequestErrorAction::Terminal
    );
    assert_eq!(calls.load(Ordering::Acquire), 1);
    assert!(harness.retry_events().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn turn_signal_cancellation_during_backoff_wins_without_starting_the_retry() {
    let harness = Harness::new(0.5).await;
    let policy = resolve_retry_policy(
        Some(&json!({
            "mode":"normal",
            "maxRetries":1,
            "retryableCodes":["RATE_LIMIT"],
            "backoff":{"initialDelayMs":60000,"maxDelayMs":60000,"jitterRatio":0}
        })),
        "retryPolicy",
    )
    .unwrap();
    let signal = AbortSignal::default();
    let signal_for_task = signal.clone();
    let events = AgentEvents::new(harness.context.clone(), harness.agent.clone());
    let task = tokio::spawn(async move {
        events
            .waterfall(
                "agent/request-error",
                AgentRequestErrorEvent {
                    turn: 1,
                    step: 1,
                    provider: ProviderId::new("mock"),
                    failure: failure("RATE_LIMIT", None),
                    retry_policy: Some(policy),
                    signal: signal_for_task,
                },
                || async { Ok(RequestErrorAction::Terminal) },
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if !harness.retry_events().is_empty() {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    signal.abort();
    assert_eq!(task.await.unwrap().unwrap(), RequestErrorAction::Terminal);
    assert!(
        !harness
            .agent
            .session()
            .events()
            .iter()
            .any(|event| event.event_type == "llm/retry-started")
    );
    harness.close().await;
}

#[tokio::test]
async fn disposal_aborts_and_drains_an_active_backoff_without_starting_retry() {
    let harness = Harness::new(0.5).await;
    let policy = resolve_retry_policy(
        Some(&json!({
            "mode":"normal",
            "maxRetries":1,
            "retryableCodes":["RATE_LIMIT"],
            "backoff":{"initialDelayMs":60000,"maxDelayMs":60000,"jitterRatio":0}
        })),
        "retryPolicy",
    )
    .unwrap();
    let events = AgentEvents::new(harness.context.clone(), harness.agent.clone());
    let task = tokio::spawn(async move {
        events
            .waterfall(
                "agent/request-error",
                AgentRequestErrorEvent {
                    turn: 1,
                    step: 1,
                    provider: ProviderId::new("mock"),
                    failure: failure("RATE_LIMIT", None),
                    retry_policy: Some(policy),
                    signal: AbortSignal::default(),
                },
                || async { Ok(RequestErrorAction::Terminal) },
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if harness
                .agent
                .session()
                .events()
                .iter()
                .any(|event| event.event_type == "llm/retry")
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    harness.plugin.dispose().await.unwrap();
    assert_eq!(task.await.unwrap().unwrap(), RequestErrorAction::Terminal);
    assert!(
        !harness
            .agent
            .session()
            .events()
            .iter()
            .any(|event| event.event_type == "llm/retry-started")
    );
    harness.close().await;
}

#[tokio::test]
async fn disposal_waits_for_delegated_recovery_to_reach_quiescence() {
    let harness = Harness::new(0.0).await;
    let entered = Arc::new(tokio::sync::Semaphore::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let events = AgentEvents::new(harness.context.clone(), harness.agent.clone());
    let entered_task = entered.clone();
    let release_task = release.clone();
    let task = tokio::spawn(async move {
        events
            .waterfall(
                "agent/request-error",
                AgentRequestErrorEvent {
                    turn: 1,
                    step: 1,
                    provider: ProviderId::new("mock"),
                    failure: failure("AUTH", None),
                    retry_policy: Some(always(1.0)),
                    signal: AbortSignal::default(),
                },
                move || async move {
                    entered_task.add_permits(1);
                    release_task.acquire().await.unwrap().forget();
                    Ok(RequestErrorAction::Terminal)
                },
            )
            .await
    });
    entered.acquire().await.unwrap().forget();
    let plugin = harness.plugin.clone();
    let mut disposal = tokio::spawn(async move { plugin.dispose().await });
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut disposal)
            .await
            .is_err()
    );
    release.add_permits(1);
    disposal.await.unwrap().unwrap();
    assert_eq!(task.await.unwrap().unwrap(), RequestErrorAction::Terminal);
    assert!(harness.retry_events().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn a_callback_captured_before_disposal_fails_closed_without_entering_downstream() {
    let context = Context::new();
    let agents = Arc::new(AgentRegistry::new(context.clone()));
    agents.provide(&context).unwrap();
    let captured_next = Arc::new(parking_lot::Mutex::new(None));
    let entered = Arc::new(tokio::sync::Semaphore::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let slot = captured_next.clone();
    let entered_listener = entered.clone();
    let release_listener = release.clone();
    context
        .events()
        .on_waterfall(
            &context,
            "agent/request-error",
            move |_, _, next| {
                *slot.lock() = Some(next);
                entered_listener.add_permits(1);
                let slot = slot.clone();
                let release = release_listener.clone();
                Box::pin(async move {
                    release.acquire().await.unwrap().forget();
                    let next = { slot.lock().take().expect("captured continuation") };
                    next.run().await
                })
            },
            EventOptions::default(),
        )
        .unwrap();
    let plugin = install_with_internals(
        &context,
        RetryConfig::default(),
        RetryInternals::new(|| 0.0, || RetryId::new("unused")),
    )
    .unwrap();
    plugin.await_settled().await.unwrap();
    let downstream_calls = Arc::new(AtomicUsize::new(0));
    let calls = downstream_calls.clone();
    context
        .events()
        .on_waterfall(
            &context,
            "agent/request-error",
            move |_, _, next| {
                calls.fetch_add(1, Ordering::AcqRel);
                next.run()
            },
            EventOptions::default(),
        )
        .unwrap();

    let session = Session::create(&SessionId::new("captured-disposal"), None, None).unwrap();
    let inbox = Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
    let agent = Arc::new(Agent::new(
        SessionId::new("captured-disposal"),
        AgentOptions::default(),
        session,
        inbox,
        context.clone(),
        ScopeKey::new(),
    ));
    let events = AgentEvents::new(context.clone(), agent);
    let task = tokio::spawn(async move {
        events
            .waterfall(
                "agent/request-error",
                AgentRequestErrorEvent {
                    turn: 1,
                    step: 1,
                    provider: ProviderId::new("mock"),
                    failure: failure("AUTH", None),
                    retry_policy: Some(always(1.0)),
                    signal: AbortSignal::default(),
                },
                || async { Ok(RequestErrorAction::Terminal) },
            )
            .await
    });
    entered.acquire().await.unwrap().forget();
    plugin.dispose().await.unwrap();
    release.add_permits(1);
    assert_eq!(task.await.unwrap().unwrap(), RequestErrorAction::Terminal);
    assert_eq!(downstream_calls.load(Ordering::Acquire), 0);
    context.fiber().dispose().await.unwrap();
}
