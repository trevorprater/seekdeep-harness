//! The fixture's projection folds reproduce the values the source fixture captured
//! for the seeded history, and advance live through the goal command.

#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use futures::StreamExt as _;
use seekdeep_abort::AbortSignal;
use seekdeep_client_connection::{FixtureApi, FixtureOptions, RpcId, RpcResult, StreamApi as _};
use serde_json::{Value, json};

async fn call(api: &FixtureApi, method: &str, payload: Value) -> Value {
    let response = api
        .unary(
            method,
            RpcId::new(format!("test-{method}")),
            payload,
            AbortSignal::default(),
        )
        .await
        .unwrap();
    match response.result {
        RpcResult::Success { value } => value.unwrap_or(Value::Null),
        RpcResult::Failure { error } => panic!("{}: {}", error.code, error.message),
    }
}

#[tokio::test]
async fn folded_history_projections_match_the_captured_seed_values() {
    let seed: Value = serde_json::from_str(include_str!("../data/fixture-seed.json")).unwrap();
    let expected = seed
        .pointer("/history/projections/values")
        .cloned()
        .unwrap();
    let api = FixtureApi::new(FixtureOptions::default());
    let history = call(&api, "session.history", json!({"sessionId":"fx-alpha"})).await;
    let actual = history.pointer("/projections/values").cloned().unwrap();
    for (key, value) in expected.as_object().unwrap() {
        assert_eq!(actual.get(key), Some(value), "projection {key} differs");
    }
    assert_eq!(
        actual.as_object().unwrap().len(),
        expected.as_object().unwrap().len()
    );
}

#[tokio::test]
async fn goal_command_appends_a_durable_change_and_pushes_the_goal_projection() {
    let api: Arc<FixtureApi> = FixtureApi::new(FixtureOptions::default());
    let signal = AbortSignal::default();
    let mut mux = api.mux(signal.clone(), Arc::new(|| {}));
    // Drain the initial frames so only the command's frames remain.
    let create = call(&api, "session.create", json!({})).await;
    let session = create
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap()
        .to_owned();
    let executed = call(
        &api,
        "commands/execute",
        json!({"args":{"agentId":session,"line":"/goal guard rapid clear clicks"}}),
    )
    .await;
    assert_eq!(
        executed.pointer("/result/text").and_then(Value::as_str),
        Some("Goal created: guard rapid clear clicks")
    );
    let mut goal_projection = None;
    while let Some(Ok(frame)) = mux.next().await {
        if frame.payload.get("type").and_then(Value::as_str) == Some("session/projection")
            && frame.payload.get("key").and_then(Value::as_str) == Some("goal")
            && frame.payload.get("sessionId").and_then(Value::as_str) == Some(session.as_str())
        {
            goal_projection = Some(frame.payload["value"].clone());
            break;
        }
    }
    let projection = goal_projection.expect("goal projection frame");
    assert_eq!(
        projection
            .pointer("/goal/objective")
            .and_then(Value::as_str),
        Some("guard rapid clear clicks")
    );
    assert_eq!(
        projection.pointer("/goal/phase").and_then(Value::as_str),
        Some("active")
    );
    let again = call(
        &api,
        "commands/execute",
        json!({"args":{"agentId":session,"line":"/goal another"}}),
    )
    .await;
    assert_eq!(
        again.pointer("/result/text").and_then(Value::as_str),
        Some("A goal already exists (guard rapid clear clicks). Clear it first.")
    );
    signal.abort();
}
