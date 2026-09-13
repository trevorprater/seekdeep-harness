//! Host/Client inspection directory, validation, routing, cancellation, and disposal parity.

use std::sync::Arc;

use parking_lot::Mutex;
use seekdeep_cordis::{Context, EventOptions, EventReply};
use seekdeep_cordis_host_runner::{
    CORDIS_INSPECT, CordisInspectFailureReason, CordisInspectMethodManifest, CordisInspectPlatform,
    CordisInspectProviderManifest, CordisInspectQueryRequest, CordisInspectQueryResolution,
    CordisInspectQueryResolved, CordisInspectRegistryService, CordisInspectResolveAck,
    DynamicCordisRunner, HostCordisInspectProviderRegistration,
};
use seekdeep_llm::{AbortSignal, SessionId};
use serde_json::{Value, json};

fn method(name: &str, input_schema: Value, output_schema: Value) -> CordisInspectMethodManifest {
    CordisInspectMethodManifest {
        name: name.to_owned(),
        description: format!("query {name}"),
        input_schema,
        output_schema,
    }
}

fn manifest(id: &str, methods: Vec<CordisInspectMethodManifest>) -> CordisInspectProviderManifest {
    CordisInspectProviderManifest {
        id: id.to_owned(),
        description: format!("inspect {id}"),
        methods,
    }
}

fn host_registration(id: &str) -> HostCordisInspectProviderRegistration {
    HostCordisInspectProviderRegistration {
        manifest: manifest(
            id,
            vec![method("read", json!({"type": "object"}), json!({}))],
        ),
        query: Arc::new(|_, input, _| Box::pin(async move { Ok(input.unwrap_or(Value::Null)) })),
    }
}

#[tokio::test]
async fn provider_directories_are_ordered_validated_atomic_and_lifecycle_owned() {
    let context = Context::new();
    let registry = CordisInspectRegistryService::new(context.clone());
    let first = registry
        .register(&context, host_registration("alpha"))
        .unwrap();
    registry
        .register(&context, host_registration("beta"))
        .unwrap();
    assert!(
        registry
            .register(&context, host_registration("alpha"))
            .unwrap_err()
            .to_string()
            .contains("already registered")
    );

    let client = manifest(
        "browser",
        vec![method("visible", json!({}), json!({"type": "boolean"}))],
    );
    registry
        .sync_client_manifest(std::slice::from_ref(&client))
        .unwrap();
    assert_eq!(
        registry
            .list()
            .iter()
            .map(|provider| (provider.platform, provider.id.as_str()))
            .collect::<Vec<_>>(),
        [
            (CordisInspectPlatform::Host, "alpha"),
            (CordisInspectPlatform::Host, "beta"),
            (CordisInspectPlatform::Client, "browser"),
        ]
    );

    let invalid = manifest(
        "broken",
        vec![method("bad", json!({"type": "future"}), json!({}))],
    );
    assert!(registry.sync_client_manifest(&[invalid]).is_err());
    assert_eq!(registry.list().last().unwrap().id, "browser");

    first.dispose().await.unwrap();
    assert_eq!(
        registry
            .list()
            .iter()
            .map(|provider| provider.id.as_str())
            .collect::<Vec<_>>(),
        ["beta", "browser"]
    );
    context.fiber().dispose().await.unwrap();
    assert!(
        registry
            .list()
            .iter()
            .all(|row| row.platform != CordisInspectPlatform::Host)
    );
}

#[tokio::test]
async fn host_queries_validate_both_sides_and_observe_cancellation_after_provider_work() {
    let context = Context::new();
    let registry = CordisInspectRegistryService::new(context.clone());
    registry
        .register(
            &context,
            HostCordisInspectProviderRegistration {
                manifest: manifest(
                    "clock",
                    vec![
                        method(
                            "read",
                            json!({
                                "type": "object",
                                "properties": {"value": {"type": "number"}},
                                "required": ["value"],
                            }),
                            json!({"type": "number"}),
                        ),
                        method("bad", json!({}), json!({"type": "number"})),
                        method("abort", json!({}), json!({"type": "number"})),
                    ],
                ),
                query: Arc::new(|method, input, context| {
                    Box::pin(async move {
                        match method.as_str() {
                            "read" => Ok(input.unwrap()["value"].clone()),
                            "bad" => Ok(json!("wrong")),
                            "abort" => {
                                context.signal.abort();
                                Ok(json!(1))
                            }
                            unknown => anyhow::bail!("unexpected method {unknown}"),
                        }
                    })
                }),
            },
        )
        .unwrap();
    let session = SessionId::new("session-a");

    assert_eq!(
        registry
            .query(
                CordisInspectPlatform::Host,
                "clock",
                "read",
                Some(json!({"value": 7})),
                &session,
                AbortSignal::default(),
            )
            .await
            .unwrap(),
        json!(7)
    );
    let invalid_input = registry
        .query(
            CordisInspectPlatform::Host,
            "clock",
            "read",
            Some(json!({})),
            &session,
            AbortSignal::default(),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(invalid_input.contains("Host Cordis inspect clock.read rejected input"));
    let invalid_output = registry
        .query(
            CordisInspectPlatform::Host,
            "clock",
            "bad",
            None,
            &session,
            AbortSignal::default(),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(invalid_output.contains("Host Cordis inspect clock.bad returned invalid output"));
    assert_eq!(
        registry
            .query(
                CordisInspectPlatform::Host,
                "clock",
                "abort",
                None,
                &session,
                AbortSignal::default(),
            )
            .await
            .unwrap_err()
            .to_string(),
        "This operation was aborted"
    );
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn client_queries_are_session_owned_schema_checked_and_first_valid_answer_wins() {
    let context = Context::new();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    assert!(Arc::ptr_eq(
        context.get(CORDIS_INSPECT).as_ref().unwrap(),
        runner.inspect_registry().unwrap()
    ));
    runner
        .sync_inspect_manifest(&[manifest(
            "browser",
            vec![method("count", json!({}), json!({"type": "number"}))],
        )])
        .unwrap();
    let session = SessionId::new("session-a");
    let wrong = SessionId::new("session-b");
    let acknowledgements = Arc::new(Mutex::new(Vec::new()));
    let acks = acknowledgements.clone();
    let router = runner.clone();
    let owner = session.clone();
    context
        .events()
        .on_sync(
            &context,
            "cordis/inspect-query",
            move |_, args| {
                let request = args.get::<CordisInspectQueryRequest>(0).unwrap();
                acks.lock().push(router.resolve_inspect_query(
                    &wrong,
                    &request.request_id,
                    CordisInspectQueryResolution::Success { data: json!(1) },
                ));
                acks.lock().push(router.resolve_inspect_query(
                    &owner,
                    &request.request_id,
                    CordisInspectQueryResolution::Failure {
                        reason: CordisInspectFailureReason::ProviderError,
                        message: "failed".to_owned(),
                    },
                ));
                acks.lock().push(router.resolve_inspect_query(
                    &owner,
                    &request.request_id,
                    CordisInspectQueryResolution::Success {
                        data: json!("wrong"),
                    },
                ));
                acks.lock().push(router.resolve_inspect_query(
                    &owner,
                    &request.request_id,
                    CordisInspectQueryResolution::Success { data: json!(42) },
                ));
                acks.lock().push(router.resolve_inspect_query(
                    &owner,
                    &request.request_id,
                    CordisInspectQueryResolution::Success { data: json!(43) },
                ));
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();

    let value = runner
        .inspect_registry()
        .unwrap()
        .query(
            CordisInspectPlatform::Client,
            "browser",
            "count",
            None,
            &session,
            AbortSignal::default(),
        )
        .await
        .unwrap();
    assert_eq!(value, json!(42));
    assert_eq!(
        *acknowledgements.lock(),
        [
            CordisInspectResolveAck { accepted: false },
            CordisInspectResolveAck { accepted: false },
            CordisInspectResolveAck { accepted: false },
            CordisInspectResolveAck { accepted: true },
            CordisInspectResolveAck { accepted: false },
        ]
    );
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn client_query_cancellation_retires_the_request_and_rejects_late_answers() {
    let context = Context::new();
    let registry = CordisInspectRegistryService::new(context.clone());
    registry
        .sync_client_manifest(&[manifest(
            "browser",
            vec![method("count", json!({}), json!({"type": "number"}))],
        )])
        .unwrap();
    let signal = AbortSignal::default();
    let cancel = signal.clone();
    let request = Arc::new(Mutex::new(None::<CordisInspectQueryRequest>));
    let observed = request.clone();
    context
        .events()
        .on_sync(
            &context,
            "cordis/inspect-query",
            move |_, args| {
                *observed.lock() = args.get::<CordisInspectQueryRequest>(0).as_deref().cloned();
                cancel.abort();
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    let resolved = Arc::new(Mutex::new(Vec::new()));
    let resolved_events = resolved.clone();
    context
        .events()
        .on_sync(
            &context,
            "cordis/inspect-query-resolved",
            move |_, args| {
                resolved_events.lock().push(
                    args.get::<CordisInspectQueryResolved>(0)
                        .unwrap()
                        .request_id
                        .clone(),
                );
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    let session = SessionId::new("session-a");

    let error = registry
        .query(
            CordisInspectPlatform::Client,
            "browser",
            "count",
            None,
            &session,
            signal,
        )
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(
        error,
        "browser.count: Client inspect query browser.count was cancelled"
    );
    let request = request.lock().clone().unwrap();
    assert_eq!(
        resolved.lock().as_slice(),
        std::slice::from_ref(&request.request_id)
    );
    assert_eq!(
        registry.resolve_client_query(
            &session,
            &request.request_id,
            CordisInspectQueryResolution::Success { data: json!(1) },
        ),
        CordisInspectResolveAck { accepted: false }
    );
    context.fiber().dispose().await.unwrap();
}
