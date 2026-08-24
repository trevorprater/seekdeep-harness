//! Real OTLP/HTTP JSON wire coverage for the native Rust pipeline.

use std::{
    collections::{BTreeMap, HashMap},
    io::Read as _,
    sync::Arc,
};

use flate2::read::GzDecoder;
use http_body_util::{BodyExt as _, Full};
use hyper::{Request, Response, body::Incoming, service::service_fn};
use hyper_util::rt::TokioIo;
use seekdeep_anonymous_user_id::{ANONYMOUS_USER_ID_FILE_NAME, AnonymousUserIdOptions};
use seekdeep_cordis::{Context, Fiber};
use seekdeep_core::{
    session::{AppendOptions, SessionId},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_session_telemetry_otel::{
    NativeOtelLogPipelineFactory, OpenTelemetrySessionBackend, OtelLogFactoryService,
    OtelLogPipelineFactory as _, OtelLogRecord, OtelPipelineOptions, OtelResource,
    OtelTelemetryConfig,
};
use serde_json::{Map, Value, json};

struct Capture {
    headers: hyper::HeaderMap,
    body: Value,
}

async fn collector() -> (String, tokio::sync::oneshot::Receiver<Capture>) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind collector");
    let address = listener.local_addr().expect("collector address");
    let (send, receive) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept collector request");
        let send = Arc::new(parking_lot::Mutex::new(Some(send)));
        hyper::server::conn::http1::Builder::new()
            .serve_connection(
                TokioIo::new(stream),
                service_fn(move |request| {
                    let send = send.clone();
                    async move { capture(request, &send).await }
                }),
            )
            .await
            .expect("serve collector request");
    });
    (format!("http://{address}/v1/logs"), receive)
}

async fn capture(
    request: Request<Incoming>,
    send: &parking_lot::Mutex<Option<tokio::sync::oneshot::Sender<Capture>>>,
) -> Result<Response<Full<hyper::body::Bytes>>, hyper::Error> {
    let headers = request.headers().clone();
    let mut bytes = request.into_body().collect().await?.to_bytes().to_vec();
    if headers
        .get(hyper::header::CONTENT_ENCODING)
        .is_some_and(|value| value == "gzip")
    {
        let mut decoded = Vec::new();
        GzDecoder::new(bytes.as_slice())
            .read_to_end(&mut decoded)
            .expect("decode gzip request");
        bytes = decoded;
    }
    let body = serde_json::from_slice(&bytes).expect("OTLP JSON request");
    if let Some(send) = send.lock().take() {
        let _ = send.send(Capture { headers, body });
    }
    Ok(Response::new(Full::new(hyper::body::Bytes::from_static(
        b"{}",
    ))))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pipeline_emits_otlp_json_with_resource_scopes_headers_and_gzip() {
    let (endpoint, captured) = collector().await;
    let factory = NativeOtelLogPipelineFactory::default();
    let pipeline = factory
        .create(OtelPipelineOptions {
            exporter: json!({
                "url": endpoint,
                "headers": {"authorization": "Bearer native-test"},
                "compression": "gzip",
                "timeoutMillis": 5_000
            }),
            processor: Some(json!({
                "maxQueueSize": 32,
                "maxExportBatchSize": 16,
                "scheduledDelayMillis": 60_000,
                "exportTimeoutMillis": 5_000
            })),
            resource: OtelResource {
                attributes: BTreeMap::from([
                    ("service.name".to_owned(), "seekdeep-harness".to_owned()),
                    ("service.version".to_owned(), "0.1.0-rc.5".to_owned()),
                    ("user.id".to_owned(), "native-test-user".to_owned()),
                ]),
            },
        })
        .expect("native pipeline");
    pipeline.emit(OtelLogRecord {
        scope: "@seekdeep-ai/seekdeep-session-telemetry-otel".to_owned(),
        scope_version: "0.1.0-rc.5".to_owned(),
        timestamp: 1_700_000_000_123,
        severity_number: 13,
        severity_text: "WARN",
        attributes: Map::from_iter([
            ("session.id".to_owned(), json!("wire")),
            ("event.type".to_owned(), json!("turn/end")),
            ("event.seq".to_owned(), json!(7)),
        ]),
        body: json!({"reason": {"kind": "error"}}),
    });
    pipeline.emit(OtelLogRecord {
        scope: "@seekdeep-ai/seekdeep-session-telemetry-otel/ops".to_owned(),
        scope_version: "0.1.0-rc.5".to_owned(),
        timestamp: 1_700_000_000_124,
        severity_number: 9,
        severity_text: "INFO",
        attributes: Map::from_iter([("telemetry.op".to_owned(), json!("shutdown"))]),
        body: json!({"op": "shutdown"}),
    });
    pipeline.shutdown().await.expect("pipeline shutdown");

    let capture = tokio::time::timeout(std::time::Duration::from_secs(5), captured)
        .await
        .expect("collector deadline")
        .expect("collector capture");
    assert_eq!(capture.headers["authorization"], "Bearer native-test");
    assert_eq!(capture.headers["content-encoding"], "gzip");
    assert_eq!(capture.headers["content-type"], "application/json");
    let resource_logs = capture.body["resourceLogs"]
        .as_array()
        .expect("resource logs");
    let resource_attributes = resource_logs[0]["resource"]["attributes"]
        .as_array()
        .expect("resource attributes");
    assert!(resource_attributes.iter().any(|attribute| {
        attribute["key"] == "service.name"
            && attribute["value"]["stringValue"] == "seekdeep-harness"
    }));
    assert!(resource_attributes.iter().any(|attribute| {
        attribute["key"] == "user.id" && attribute["value"]["stringValue"] == "native-test-user"
    }));
    let scope_logs = resource_logs[0]["scopeLogs"]
        .as_array()
        .expect("scope logs");
    let scopes = scope_logs
        .iter()
        .map(|logs| logs["scope"]["name"].as_str().expect("scope name"))
        .collect::<Vec<_>>();
    assert!(scopes.contains(&"@seekdeep-ai/seekdeep-session-telemetry-otel"));
    assert!(scopes.contains(&"@seekdeep-ai/seekdeep-session-telemetry-otel/ops"));
    let ledger = scope_logs
        .iter()
        .find(|logs| logs["scope"]["name"] == "@seekdeep-ai/seekdeep-session-telemetry-otel")
        .expect("ledger scope");
    let record = &ledger["logRecords"][0];
    assert_eq!(record["timeUnixNano"], "1700000000123000000");
    assert_eq!(record["observedTimeUnixNano"], "1700000000123000000");
    assert_eq!(record["severityNumber"], 13);
    assert_eq!(record["severityText"], "WARN");
    assert_eq!(record["body"]["kvlistValue"]["values"][0]["key"], "reason");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backend_coordinator_reaches_the_native_otlp_wire_and_drains_on_disposal() {
    let (endpoint, captured) = collector().await;
    let home = tempfile::tempdir().expect("temporary home");
    let factory = Arc::new(NativeOtelLogPipelineFactory::new(AnonymousUserIdOptions {
        env: Some(HashMap::from([(
            std::ffi::OsString::from("SEEKDEEP_HOME"),
            home.path().as_os_str().to_owned(),
        )])),
        random_uuid: Some(Arc::new(|| {
            "00000000-0000-4000-8000-000000000654".to_owned()
        })),
    }));
    let root = Context::new();
    let sessions = SessionStore::install(&root).expect("session store");
    OtelLogFactoryService::new(factory)
        .provide(&root)
        .expect("native factory service");
    let fiber = Fiber::active_child("native-otel-backend");
    let context = root.with_fiber(fiber.clone());
    OpenTelemetrySessionBackend::install(
        &context,
        OtelTelemetryConfig {
            mode: Some("FULL".to_owned()),
            exporter: Some(json!({
                "url": endpoint,
                "headers": {"x-native-composition": "yes"}
            })),
            processor: Some(json!({"scheduledDelayMillis": 60_000})),
            shutdown_timeout_millis: Some(5_000.0),
        },
    )
    .expect("backend");
    let session = sessions
        .create(
            &root,
            Some(SessionId::new("native-wire")),
            CreateSessionOptions::default(),
        )
        .expect("session");
    session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .expect("turn start");
    session
        .append(
            "turn/end",
            json!({"turn": 1, "reason": {"kind": "error", "error": {"message": "boom"}}}),
            AppendOptions::default(),
        )
        .expect("turn end");
    fiber.dispose().await.expect("dispose backend");

    let capture = tokio::time::timeout(std::time::Duration::from_secs(5), captured)
        .await
        .expect("collector deadline")
        .expect("collector capture");
    assert_eq!(capture.headers["x-native-composition"], "yes");
    let scopes = capture.body["resourceLogs"][0]["scopeLogs"]
        .as_array()
        .expect("scope logs");
    let records = scopes
        .iter()
        .flat_map(|scope| scope["logRecords"].as_array().expect("log records").iter())
        .collect::<Vec<_>>();
    let event_types = records
        .iter()
        .flat_map(|record| {
            record["attributes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|attribute| attribute["key"] == "event.type")
                .filter_map(|attribute| attribute["value"]["stringValue"].as_str())
        })
        .collect::<Vec<_>>();
    assert!(event_types.contains(&"turn/start"));
    assert!(event_types.contains(&"turn/end"));
    assert!(records.iter().any(|record| {
        record["attributes"].as_array().is_some_and(|attributes| {
            attributes.iter().any(|attribute| {
                attribute["key"] == "telemetry.op"
                    && attribute["value"]["stringValue"] == "shutdown"
            })
        })
    }));
    assert!(records.iter().any(|record| {
        record["severityNumber"] == 17
            && record["attributes"].as_array().is_some_and(|attributes| {
                attributes.iter().any(|attribute| {
                    attribute["key"] == "event.type"
                        && attribute["value"]["stringValue"] == "turn/end"
                })
            })
    }));
    root.root_fiber().dispose().await.expect("dispose root");
}

#[test]
fn native_factory_uses_the_shared_harness_home_identity_contract() {
    let home = tempfile::tempdir().expect("temporary home");
    let factory = NativeOtelLogPipelineFactory::new(AnonymousUserIdOptions {
        env: Some(HashMap::from([(
            std::ffi::OsString::from("SEEKDEEP_HOME"),
            home.path().as_os_str().to_owned(),
        )])),
        random_uuid: Some(Arc::new(|| {
            "00000000-0000-4000-8000-000000000321".to_owned()
        })),
    });
    assert_eq!(
        factory.anonymous_user_id().expect("anonymous id"),
        "00000000-0000-4000-8000-000000000321"
    );
    assert_eq!(
        std::fs::read_to_string(home.path().join(ANONYMOUS_USER_ID_FILE_NAME))
            .expect("identity file"),
        "00000000-0000-4000-8000-000000000321\n"
    );
}
