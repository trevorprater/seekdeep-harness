//! Behavioral mirror of Connection's dedicated/shared Host channels and browser caller.

use std::{collections::HashMap, sync::Arc};

use futures::future::BoxFuture;
use parking_lot::Mutex;
use seekdeep_client_connection::{
    ClientConnection, ClientRequest, ConnectionRpcAuthority, HostConnectionService, HttpMethod,
    HttpRequest, HttpResponse, HttpTransport, HttpTransportFuture, RpcError, RpcHandler,
    RpcHandlerFuture, RpcId, RpcResult, ServerResponse, WebConnectionRpc, endpoint_from_path,
    result_of, transport_error, validate_rpc_target,
};
use seekdeep_cordis::Context;
use seekdeep_llm::AbortSignal;
use serde_json::{Value, json};

fn request(method: HttpMethod, path: &str, host: &str, body: impl serde::Serialize) -> HttpRequest {
    let mut request = HttpRequest::new(method, path);
    request.headers.insert("host".to_owned(), host.to_owned());
    request
        .headers
        .insert("content-type".to_owned(), "application/json".to_owned());
    request.body = serde_json::to_vec(&body).unwrap();
    request
}

fn handler(
    callback: impl Fn(String, Value, AbortSignal) -> anyhow::Result<RpcResult<Value>>
    + Send
    + Sync
    + 'static,
) -> RpcHandler {
    let callback = Arc::new(callback);
    Arc::new(move |endpoint, payload, signal| {
        let callback = callback.clone();
        Box::pin(async move { callback(endpoint, payload, signal) }) as RpcHandlerFuture
    })
}

fn success(value: Value) -> RpcResult<Value> {
    RpcResult::Success { value: Some(value) }
}

fn envelope(id: &str, method: &str, payload: Value) -> Value {
    serde_json::to_value(ClientRequest::new(RpcId::new(id), method, payload)).unwrap()
}

fn response_json(response: &HttpResponse) -> Value {
    serde_json::from_slice(&response.body).unwrap()
}

#[test]
fn endpoint_and_channel_grammar_matches_the_source() {
    assert_eq!(
        endpoint_from_path("/rpc", "/rpc/goals/create"),
        Some("goals/create".to_owned())
    );
    for path in [
        "/outside/goals/create",
        "/rpc/goals//create",
        "/rpc/./create",
        "/rpc/../create",
        "/rpc/goals/%2F",
    ] {
        assert_eq!(endpoint_from_path("/rpc", path), None, "{path}");
    }
    assert!(validate_rpc_target("/api", "goals/create").is_ok());
    for (channel, endpoint) in [
        ("api", "goals/create"),
        ("/api/extra", "goals/create"),
        ("/api", "goals//create"),
        ("/api", "../create"),
    ] {
        assert!(validate_rpc_target(channel, endpoint).is_err());
    }
}

#[tokio::test]
async fn dedicated_channel_dispatches_exact_envelope_and_withdraws() {
    let context = Context::new();
    let connection = HostConnectionService::new(Vec::new()).unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let calls_for_handler = calls.clone();
    let remove = connection
        .handle(
            &context,
            "/rpc",
            handler(move |endpoint, payload, _| {
                calls_for_handler.lock().push(json!({
                    "endpoint": endpoint,
                    "payload": payload,
                }));
                Ok(success(json!({ "accepted": true })))
            }),
            ConnectionRpcAuthority::TrustedHost,
        )
        .unwrap();
    let response = connection
        .dispatch(
            "/rpc",
            request(
                HttpMethod::Post,
                "/rpc/goals/create",
                "127.0.0.1:3080",
                envelope(
                    "rpc-dedicated",
                    "goals/create",
                    json!({ "args": { "agentId": "agent-1" } }),
                ),
            ),
        )
        .await;
    assert_eq!(response.status, 200);
    assert_eq!(
        response_json(&response),
        json!({
            "type": "server-response",
            "rpcId": "rpc-dedicated",
            "result": { "ok": true, "value": { "accepted": true } },
        })
    );
    assert_eq!(calls.lock().len(), 1);
    assert!(
        connection
            .handle(
                &context,
                "/rpc",
                handler(|_, _, _| Ok(success(Value::Null))),
                ConnectionRpcAuthority::TrustedHost,
            )
            .is_err()
    );
    remove.dispose().await.unwrap();
    assert_eq!(
        connection
            .dispatch(
                "/rpc",
                HttpRequest::new(HttpMethod::Post, "/rpc/goals/create")
            )
            .await
            .status,
        404
    );
}

#[tokio::test]
async fn shared_claim_precedes_fallback_and_loopback_policy_is_local() {
    let context = Context::new();
    let connection = HostConnectionService::new(vec!["harness.example".to_owned()]).unwrap();
    let remove = connection
        .intercept(
            &context,
            "/api",
            Arc::new(|endpoint| endpoint == "goals/create"),
            handler(|_, _, _| Ok(success(json!({ "claimed": true })))),
            ConnectionRpcAuthority::TrustedHost,
        )
        .unwrap();
    assert!(
        connection
            .intercept(
                &context,
                "/api",
                Arc::new(|_| true),
                handler(|_, _, _| Ok(success(Value::Null))),
                ConnectionRpcAuthority::TrustedHost,
            )
            .is_err()
    );
    let response = connection
        .dispatch_shared(
            "/api",
            request(
                HttpMethod::Post,
                "/api/goals/create",
                "harness.example",
                envelope("shared", "goals/create", json!({})),
            ),
            |_| async { HttpResponse::text(418, "fallback") },
        )
        .await;
    assert_eq!(response.status, 200);
    let fallback = connection
        .dispatch_shared(
            "/api",
            HttpRequest::new(HttpMethod::Get, "/api/session.list"),
            |_| async { HttpResponse::text(418, "fallback") },
        )
        .await;
    assert_eq!(fallback.status, 418);
    remove.dispose().await.unwrap();

    let remove = connection
        .intercept(
            &context,
            "/api",
            Arc::new(|endpoint| endpoint == "goals/create"),
            handler(|_, _, _| Ok(success(Value::Null))),
            ConnectionRpcAuthority::Loopback,
        )
        .unwrap();
    let denied = connection
        .dispatch_shared(
            "/api",
            request(
                HttpMethod::Post,
                "/api/goals/create",
                "harness.example",
                envelope("loopback", "goals/create", json!({})),
            ),
            |_| async { HttpResponse::text(418, "fallback") },
        )
        .await;
    assert_eq!(denied, HttpResponse::text(403, "forbidden"));
    remove.dispose().await.unwrap();
}

fn generic_connection() -> (Context, Arc<HostConnectionService>) {
    let context = Context::new();
    let connection = HostConnectionService::new(vec!["harness.example".to_owned()]).unwrap();
    connection
        .handle(
            &context,
            "/rpc",
            handler(|endpoint, _, _| {
                if endpoint == "fail" {
                    anyhow::bail!("handler broke");
                }
                Ok(RpcResult::Success { value: None })
            }),
            ConnectionRpcAuthority::TrustedHost,
        )
        .unwrap();
    (context, connection)
}

#[tokio::test]
async fn generic_rpc_checks_trust_method_and_correlation() {
    let (_context, connection) = generic_connection();
    let denied = connection
        .dispatch(
            "/rpc",
            request(
                HttpMethod::Post,
                "/rpc/goals/create",
                "other.example",
                json!({}),
            ),
        )
        .await;
    assert_eq!(denied.status, 403);

    let mismatch = connection
        .dispatch(
            "/rpc",
            request(
                HttpMethod::Post,
                "/rpc/goals/create",
                "harness.example",
                envelope("rpc-bad", "other", json!({})),
            ),
        )
        .await;
    assert_eq!(
        response_json(&mismatch)["result"]["error"]["code"],
        "bad-request"
    );
    assert_eq!(response_json(&mismatch)["rpcId"], "rpc-bad");
}

#[tokio::test]
async fn generic_rpc_checks_media_json_shape_and_handler_failure() {
    let (_context, connection) = generic_connection();
    let get = connection
        .dispatch(
            "/rpc",
            request(
                HttpMethod::Get,
                "/rpc/goals/create",
                "harness.example",
                json!({}),
            ),
        )
        .await;
    assert_eq!(get.status, 404);
    let mut wrong_media = request(
        HttpMethod::Post,
        "/rpc/goals/create",
        "harness.example",
        json!({}),
    );
    wrong_media
        .headers
        .insert("content-type".to_owned(), "text/plain".to_owned());
    assert_eq!(connection.dispatch("/rpc", wrong_media).await.status, 415);
    let mut malformed = request(
        HttpMethod::Post,
        "/rpc/goals/create",
        "harness.example",
        json!({}),
    );
    malformed.body = b"{".to_vec();
    assert_eq!(connection.dispatch("/rpc", malformed).await.status, 400);

    for (body, expected) in [
        (json!({ "rpcId": "retained-id" }), "retained-id"),
        (json!({ "rpcId": 42 }), "invalid-request"),
        (Value::Null, "invalid-request"),
    ] {
        let response = connection
            .dispatch(
                "/rpc",
                request(
                    HttpMethod::Post,
                    "/rpc/goals/create",
                    "harness.example",
                    body,
                ),
            )
            .await;
        let value = response_json(&response);
        assert_eq!(value["rpcId"], expected);
        assert_eq!(value["result"]["error"]["code"], "bad-request");
    }

    let failed = connection
        .dispatch(
            "/rpc",
            request(
                HttpMethod::Post,
                "/rpc/fail",
                "harness.example",
                envelope("rpc-fail", "fail", json!({})),
            ),
        )
        .await;
    assert_eq!(failed.status, 500);
    assert_eq!(
        String::from_utf8(failed.body).unwrap(),
        "handler failure: handler broke"
    );
}

#[test]
fn wire_result_distinguishes_absence_null_and_failure() {
    let absent = ServerResponse::<Value>::new(RpcId::new("a"), RpcResult::Success { value: None });
    let explicit_null = ServerResponse::new(
        RpcId::new("n"),
        RpcResult::Success {
            value: Some(Value::Null),
        },
    );
    assert_eq!(
        serde_json::to_value(absent).unwrap(),
        json!({ "type": "server-response", "rpcId": "a", "result": { "ok": true } })
    );
    assert_eq!(
        serde_json::to_value(explicit_null).unwrap()["result"]["value"],
        Value::Null
    );
    let failure: RpcResult<Value> = RpcResult::Failure {
        error: RpcError {
            code: "future-code".to_owned(),
            message: "retained".to_owned(),
            details: serde_json::Map::new(),
        },
    };
    assert_eq!(
        serde_json::from_value::<RpcResult<Value>>(serde_json::to_value(&failure).unwrap())
            .unwrap(),
        failure
    );
}

#[derive(Default)]
struct RecordingTransport {
    requests: Mutex<Vec<HttpRequest>>,
    response: Mutex<Option<anyhow::Result<HttpResponse>>>,
}

struct EchoTransport(Mutex<Vec<HttpRequest>>);

impl HttpTransport for EchoTransport {
    fn fetch(&self, request: HttpRequest) -> HttpTransportFuture {
        let message: ClientRequest = serde_json::from_slice(&request.body).unwrap();
        self.0.lock().push(request);
        Box::pin(async move {
            Ok(response_for(
                &message.rpc_id,
                success(json!({ "accepted": true })),
            ))
        })
    }
}

impl HttpTransport for RecordingTransport {
    fn fetch(&self, request: HttpRequest) -> HttpTransportFuture {
        self.requests.lock().push(request);
        let response = self.response.lock().take().unwrap();
        Box::pin(async move { response })
    }
}

fn response_for(id: &RpcId, result: RpcResult<Value>) -> HttpResponse {
    let body = serde_json::to_vec(&ServerResponse::new(id.clone(), result)).unwrap();
    HttpResponse {
        status: 200,
        headers: HashMap::new(),
        body,
        body_stream: None,
    }
}

#[tokio::test]
async fn web_caller_mints_validates_and_correlates_without_secure_context() {
    let transport = Arc::new(RecordingTransport::default());
    // Capture the minted id by generating a response after inspecting the request.
    let echo = Arc::new(EchoTransport(Mutex::new(Vec::new())));
    let caller = WebConnectionRpc::new(echo.clone());
    assert_eq!(
        caller
            .call(
                "/rpc",
                "goals/create",
                json!({ "args": {} }),
                AbortSignal::default(),
            )
            .await
            .unwrap(),
        success(json!({ "accepted": true }))
    );
    {
        let recorded = echo.0.lock();
        assert_eq!(recorded[0].path, "/rpc/goals/create");
        assert_eq!(recorded[0].method, HttpMethod::Post);
    }

    assert!(
        caller
            .call("rpc", "goals/create", json!({}), AbortSignal::default())
            .await
            .unwrap_err()
            .to_string()
            .contains("invalid RPC target")
    );
    drop(transport);
}

#[tokio::test]
async fn web_caller_rejects_http_failure_malformed_envelope_and_id_mismatch() {
    for response in [
        HttpResponse::text(503, "down"),
        HttpResponse::text(200, "not-json"),
        response_for(
            &RpcId::new("wrong"),
            RpcResult::Success {
                value: Some(Value::Null),
            },
        ),
    ] {
        let transport = Arc::new(RecordingTransport {
            requests: Mutex::new(Vec::new()),
            response: Mutex::new(Some(Ok(response))),
        });
        let caller = WebConnectionRpc::new(transport);
        assert!(
            caller
                .call("/rpc", "read", json!({}), AbortSignal::default())
                .await
                .is_err()
        );
    }
}

#[allow(dead_code)]
fn _assert_future_is_send(_: BoxFuture<'static, anyhow::Result<RpcResult<Value>>>) {}

#[test]
fn contract_helpers_unwrap_results_and_fold_transport_errors() {
    assert_eq!(
        result_of(ServerResponse::new(
            RpcId::new("helper"),
            RpcResult::Success { value: Some(7) },
        )),
        RpcResult::Success { value: Some(7) }
    );
    assert_eq!(
        transport_error::<Value>(&"线断了"),
        RpcResult::Failure {
            error: RpcError {
                code: "internal".to_owned(),
                message: "线断了".to_owned(),
                details: serde_json::Map::new(),
            },
        }
    );
}
