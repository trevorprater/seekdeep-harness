//! Behavioral mirror of the durable `llm/retry` invariant suite.

use seekdeep_core::{
    session::{AppendOptions, Session, SessionId},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use seekdeep_llm::MAX_TIMER_DELAY_MS;
use seekdeep_llm_retry::invariant::register_invariant;
use serde_json::{Value, json};

fn open_step(id: &str) -> std::sync::Arc<Session> {
    let session = Session::create(&SessionId::new(id), None, None).unwrap();
    open_boundary(&session, 1, 1);
    session
}

fn open_boundary(session: &Session, turn: u64, step: u64) {
    session
        .append("turn/start", json!({"turn":turn}), AppendOptions::default())
        .unwrap();
    session
        .append(
            "step/start",
            json!({"turn":turn,"step":step}),
            AppendOptions::default(),
        )
        .unwrap();
    session
        .append(
            "request/header",
            json!({
                "header":{"config":{"provider":"mock","model":"mock"}},
                "reason":"initial"
            }),
            AppendOptions::default(),
        )
        .unwrap();
}

fn failure() -> Value {
    json!({"message":"provider busy","code":"RATE_LIMIT","status":429})
}

fn normal() -> Value {
    json!({
        "retryId":"normal-retry-chain",
        "provider":"mock",
        "mode":"normal",
        "policyKey":"normal-policy",
        "retry":1,
        "maxRetries":2,
        "delayMs":1,
        "failure":failure()
    })
}

fn always() -> Value {
    json!({
        "retryId":"always-retry-chain",
        "provider":"mock",
        "mode":"always",
        "policyKey":"always-policy",
        "retry":1,
        "delayMs":1,
        "failure":failure()
    })
}

fn append_retry(session: &Session, mut data: Value, turn: u64, step: u64) {
    data["turn"] = json!(turn);
    data["step"] = json!(step);
    session
        .append("llm/retry", data, AppendOptions::default())
        .unwrap();
}

fn invalid(data: Value) -> String {
    let session = open_step("invalid");
    append_retry(&session, data, 1, 1);
    seekdeep_llm_retry::invariant::validate_session(&session)
        .unwrap_err()
        .to_string()
}

#[test]
fn accepts_successive_bounded_and_unbounded_records_in_open_steps() {
    let bounded = open_step("bounded");
    append_retry(&bounded, normal(), 1, 1);
    let mut second = normal();
    second["retry"] = json!(2);
    second["delayMs"] = json!(0);
    append_retry(&bounded, second, 1, 1);
    seekdeep_llm_retry::invariant::validate_session(&bounded).unwrap();

    let unbounded = open_step("always");
    append_retry(&unbounded, always(), 1, 1);
    seekdeep_llm_retry::invariant::validate_session(&unbounded).unwrap();
}

#[test]
fn validates_every_failure_field_without_losing_optional_facts() {
    let complete = open_step("complete-failure");
    let mut data = always();
    data["failure"] = json!({
        "message":"provider busy",
        "code":"RATE_LIMIT",
        "status":429,
        "providerRetryAfterMs":25,
        "requestId":"request-1"
    });
    append_retry(&complete, data, 1, 1);
    seekdeep_llm_retry::invariant::validate_session(&complete).unwrap();

    let cases = [
        ("failure must be an object", Value::Null),
        ("failure.message", json!({"message":1,"code":"RATE_LIMIT"})),
        ("failure.message", json!({"message":"","code":"RATE_LIMIT"})),
        ("failure.code", json!({"message":"failed","code":1})),
        ("failure.code", json!({"message":"failed","code":""})),
        (
            "failure.status",
            json!({"message":"failed","code":"RATE_LIMIT","status":429.5}),
        ),
        (
            "failure.status",
            json!({"message":"failed","code":"RATE_LIMIT","status":99}),
        ),
        (
            "failure.status",
            json!({"message":"failed","code":"RATE_LIMIT","status":600}),
        ),
        (
            "failure.providerRetryAfterMs",
            json!({"message":"failed","code":"RATE_LIMIT","providerRetryAfterMs":"25"}),
        ),
        (
            "failure.providerRetryAfterMs",
            json!({"message":"failed","code":"RATE_LIMIT","providerRetryAfterMs":0}),
        ),
        (
            "failure.requestId",
            json!({"message":"failed","code":"RATE_LIMIT","requestId":1}),
        ),
        (
            "failure.requestId",
            json!({"message":"failed","code":"RATE_LIMIT","requestId":""}),
        ),
    ];
    for (expected, bad_failure) in cases {
        let mut data = always();
        data["failure"] = bad_failure;
        assert!(invalid(data).contains(expected));
    }
}

#[test]
fn rejects_invalid_identity_numbers_modes_and_delays() {
    let cases = [
        ("retryId must be a non-empty string", "retryId", json!("")),
        ("retry must be a positive safe integer", "retry", json!(0)),
        ("retry must be a positive safe integer", "retry", json!(1.5)),
        ("positive safe maxRetries", "maxRetries", json!(0)),
        ("positive safe maxRetries", "maxRetries", json!(1.5)),
        ("must not exceed", "retry", json!(3)),
        ("mode must be normal or always", "mode", json!("sometimes")),
        ("provider must be a non-empty string", "provider", json!("")),
        (
            "policyKey must be a non-empty string",
            "policyKey",
            json!(""),
        ),
        ("delayMs", "delayMs", json!(-1)),
        ("delayMs", "delayMs", json!(MAX_TIMER_DELAY_MS + 1.0)),
        ("delayMs", "delayMs", json!("1")),
    ];
    for (expected, field, value) in cases {
        let mut data = normal();
        data[field] = value;
        assert!(invalid(data).contains(expected), "case {field}");
    }
    let mut always_with_maximum = always();
    always_with_maximum["maxRetries"] = json!(2);
    assert!(invalid(always_with_maximum).contains("always mode must omit maxRetries"));
}

#[test]
fn binds_records_to_open_turn_step_and_request_provider() {
    let no_turn = Session::create(&SessionId::new("no-turn"), None, None).unwrap();
    append_retry(&no_turn, normal(), 1, 1);
    assert!(
        seekdeep_llm_retry::invariant::validate_session(&no_turn)
            .unwrap_err()
            .to_string()
            .contains("inside an open turn")
    );

    let wrong_turn = open_step("wrong-turn");
    append_retry(&wrong_turn, normal(), 2, 1);
    assert!(
        seekdeep_llm_retry::invariant::validate_session(&wrong_turn)
            .unwrap_err()
            .to_string()
            .contains("open turn is 1")
    );

    let closed_step = open_step("closed-step");
    closed_step
        .append(
            "step/end",
            json!({"turn":1,"step":1}),
            AppendOptions::default(),
        )
        .unwrap();
    append_retry(&closed_step, normal(), 1, 1);
    assert!(
        seekdeep_llm_retry::invariant::validate_session(&closed_step)
            .unwrap_err()
            .to_string()
            .contains("inside an open step")
    );

    let wrong_step = open_step("wrong-step");
    append_retry(&wrong_step, normal(), 1, 2);
    assert!(
        seekdeep_llm_retry::invariant::validate_session(&wrong_step)
            .unwrap_err()
            .to_string()
            .contains("open step is 1/1")
    );

    let no_step = Session::create(&SessionId::new("no-step"), None, None).unwrap();
    no_step
        .append("turn/start", json!({"turn":1}), AppendOptions::default())
        .unwrap();
    append_retry(&no_step, normal(), 1, 1);
    assert!(
        seekdeep_llm_retry::invariant::validate_session(&no_step)
            .unwrap_err()
            .to_string()
            .contains("inside an open step")
    );

    let closed_turn = open_step("closed-turn");
    closed_turn
        .append(
            "step/end",
            json!({"turn":1,"step":1}),
            AppendOptions::default(),
        )
        .unwrap();
    closed_turn
        .append(
            "turn/end",
            json!({"turn":1,"reason":{"kind":"aborted","reason":{"kind":"user"}}}),
            AppendOptions::default(),
        )
        .unwrap();
    append_retry(&closed_turn, normal(), 1, 1);
    assert!(
        seekdeep_llm_retry::invariant::validate_session(&closed_turn)
            .unwrap_err()
            .to_string()
            .contains("inside an open turn")
    );

    let wrong_provider = open_step("wrong-provider");
    let mut data = always();
    data["provider"] = json!("other");
    append_retry(&wrong_provider, data, 1, 1);
    assert!(
        seekdeep_llm_retry::invariant::validate_session(&wrong_provider)
            .unwrap_err()
            .to_string()
            .contains("does not match the failed request provider mock")
    );
}

#[test]
fn numbering_and_chain_identity_are_provider_policy_and_step_scoped() {
    let session = open_step("chain");
    append_retry(&session, normal(), 1, 1);
    let mut skipped = always();
    skipped["retry"] = json!(2);
    append_retry(&session, skipped, 1, 1);
    assert!(
        seekdeep_llm_retry::invariant::validate_session(&session)
            .unwrap_err()
            .to_string()
            .contains("provider policy retry 1")
    );

    let changed = open_step("changed-id");
    append_retry(&changed, normal(), 1, 1);
    let mut second = normal();
    second["retry"] = json!(2);
    second["retryId"] = json!("different-chain");
    append_retry(&changed, second, 1, 1);
    assert!(
        seekdeep_llm_retry::invariant::validate_session(&changed)
            .unwrap_err()
            .to_string()
            .contains("preserve retryId")
    );

    let reused = open_step("reused-id");
    append_retry(&reused, normal(), 1, 1);
    let mut other_policy = always();
    other_policy["retryId"] = json!("normal-retry-chain");
    append_retry(&reused, other_policy, 1, 1);
    assert!(
        seekdeep_llm_retry::invariant::validate_session(&reused)
            .unwrap_err()
            .to_string()
            .contains("already owned by another chain")
    );

    let reset = open_step("reset-step");
    append_retry(&reset, normal(), 1, 1);
    reset
        .append(
            "step/end",
            json!({"turn":1,"step":1}),
            AppendOptions::default(),
        )
        .unwrap();
    reset
        .append(
            "step/start",
            json!({"turn":1,"step":2}),
            AppendOptions::default(),
        )
        .unwrap();
    let mut next_step = normal();
    next_step["retryId"] = json!("step-two-chain");
    append_retry(&reset, next_step, 1, 2);
    seekdeep_llm_retry::invariant::validate_session(&reset).unwrap();
}

#[test]
fn retry_started_requires_one_matching_unique_schedule() {
    let empty = open_step("started-empty");
    empty
        .append(
            "llm/retry-started",
            json!({"retryId":"","turn":1,"step":1,"retry":1}),
            AppendOptions::default(),
        )
        .unwrap();
    assert!(
        seekdeep_llm_retry::invariant::validate_session(&empty)
            .unwrap_err()
            .to_string()
            .contains("retryId must be a non-empty string")
    );

    let missing = open_step("started-missing");
    missing
        .append(
            "llm/retry-started",
            json!({"retryId":"missing","turn":1,"step":1,"retry":1}),
            AppendOptions::default(),
        )
        .unwrap();
    assert!(
        seekdeep_llm_retry::invariant::validate_session(&missing)
            .unwrap_err()
            .to_string()
            .contains("pairs no prior scheduled attempt")
    );

    let mismatch = open_step("started-mismatch");
    append_retry(&mismatch, normal(), 1, 1);
    mismatch
        .append(
            "llm/retry-started",
            json!({"retryId":"normal-retry-chain","turn":2,"step":1,"retry":1}),
            AppendOptions::default(),
        )
        .unwrap();
    assert!(
        seekdeep_llm_retry::invariant::validate_session(&mismatch)
            .unwrap_err()
            .to_string()
            .contains("turn/step must match")
    );

    let repeated = open_step("started-repeat");
    append_retry(&repeated, normal(), 1, 1);
    for _ in 0..2 {
        repeated
            .append(
                "llm/retry-started",
                json!({"retryId":"normal-retry-chain","turn":1,"step":1,"retry":1}),
                AppendOptions::default(),
            )
            .unwrap();
    }
    assert!(
        seekdeep_llm_retry::invariant::validate_session(&repeated)
            .unwrap_err()
            .to_string()
            .contains("repeats one scheduled attempt")
    );
}

#[test]
fn accepts_fresh_retry_chains_after_incomplete_predecessor_boundaries() {
    let missing_end = Session::create(&SessionId::new("missing-end"), None, None).unwrap();
    missing_end
        .append("turn/start", json!({"turn":1}), AppendOptions::default())
        .unwrap();
    open_boundary(&missing_end, 2, 1);
    append_retry(&missing_end, normal(), 2, 1);
    seekdeep_llm_retry::invariant::validate_session(&missing_end).unwrap();

    let non_failure_end = Session::create(&SessionId::new("non-failure-end"), None, None).unwrap();
    non_failure_end
        .append(
            "turn/end",
            json!({"turn":1,"reason":{"kind":"completed"}}),
            AppendOptions::default(),
        )
        .unwrap();
    open_boundary(&non_failure_end, 2, 1);
    append_retry(&non_failure_end, normal(), 2, 1);
    seekdeep_llm_retry::invariant::validate_session(&non_failure_end).unwrap();

    let missing_start = Session::create(&SessionId::new("missing-start"), None, None).unwrap();
    missing_start
        .append(
            "turn/end",
            json!({"turn":1,"reason":{"kind":"error","error":failure()}}),
            AppendOptions::default(),
        )
        .unwrap();
    open_boundary(&missing_start, 2, 1);
    append_retry(&missing_start, normal(), 2, 1);
    seekdeep_llm_retry::invariant::validate_session(&missing_start).unwrap();
}

#[tokio::test]
async fn installed_invariant_rejects_live_append_and_invalid_late_history() {
    let context = seekdeep_cordis::Context::new();
    let sessions = SessionStore::install(&context).unwrap();
    let registry = InvariantRegistry::install(&context, &InvariantConfig::default()).unwrap();
    let registration = register_invariant(&registry).unwrap();
    registration.await_ready().await.unwrap();
    assert!(registry.is_registered("@deepseek-ai/seekdeep-llm-retry"));
    let session = sessions
        .create(
            &context,
            Some(SessionId::new("live-invariant")),
            CreateSessionOptions::default(),
        )
        .unwrap();
    open_boundary(&session, 1, 1);
    let mut bad = normal();
    bad["provider"] = json!("other");
    bad["turn"] = json!(1);
    bad["step"] = json!(1);
    let error = session
        .append("llm/retry", bad, AppendOptions::default())
        .unwrap_err();
    assert!(error.to_string().contains("failed request provider mock"));
    registration.dispose().await.unwrap();
    assert!(!registry.is_registered("@deepseek-ai/seekdeep-llm-retry"));
    context.fiber().dispose().await.unwrap();

    let late_context = seekdeep_cordis::Context::new();
    let late_sessions = SessionStore::install(&late_context).unwrap();
    let late = late_sessions
        .create(
            &late_context,
            Some(SessionId::new("late-invariant")),
            CreateSessionOptions::default(),
        )
        .unwrap();
    late.append(
        "step/start",
        json!({"turn":1,"step":1}),
        AppendOptions::default(),
    )
    .unwrap();
    append_retry(&late, normal(), 1, 1);
    let late_registry =
        InvariantRegistry::install(&late_context, &InvariantConfig::default()).unwrap();
    let late_registration = register_invariant(&late_registry).unwrap();
    let error = late_registration.await_ready().await.unwrap_err();
    assert!(error.to_string().contains("inside an open turn"));
    late_context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn late_registration_accepts_a_scheduled_and_started_attempt() {
    let context = seekdeep_cordis::Context::new();
    let sessions = SessionStore::install(&context).unwrap();
    let session = sessions
        .create(
            &context,
            Some(SessionId::new("late-started")),
            CreateSessionOptions::default(),
        )
        .unwrap();
    open_boundary(&session, 1, 1);
    append_retry(&session, normal(), 1, 1);
    session
        .append(
            "llm/retry-started",
            json!({"retryId":"normal-retry-chain","turn":1,"step":1,"retry":1}),
            AppendOptions::default(),
        )
        .unwrap();
    let registry = InvariantRegistry::install(&context, &InvariantConfig::default()).unwrap();
    let registration = register_invariant(&registry).unwrap();
    registration.await_ready().await.unwrap();
    registration.dispose().await.unwrap();
    context.fiber().dispose().await.unwrap();
}

#[test]
fn typed_payload_round_trips_as_the_browser_safe_event_shape() {
    let data: seekdeep_llm_retry::LlmRetryEventData = serde_json::from_value(json!({
        "retryId":"chain",
        "turn":1,
        "step":2,
        "provider":"mock",
        "mode":"always",
        "policyKey":"policy",
        "retry":3,
        "delayMs":750,
        "failure":{"message":"busy","code":"RATE_LIMIT","status":429}
    }))
    .unwrap();
    assert_eq!(
        serde_json::to_value(data).unwrap(),
        json!({
            "retryId":"chain",
            "turn":1,
            "step":2,
            "provider":"mock",
            "mode":"always",
            "policyKey":"policy",
            "retry":3,
            "delayMs":750,
            "failure":{"message":"busy","code":"RATE_LIMIT","status":429}
        })
    );
}
