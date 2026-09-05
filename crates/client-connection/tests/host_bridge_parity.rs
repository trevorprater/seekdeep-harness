//! Real-socket parity for Connection's Host `/api` route and bounded bridge.

use std::sync::Arc;

use parking_lot::Mutex;
use seekdeep_client_connection::{
    ConnectionHostConfig, ConnectionRpcAuthority, HOST_CONNECTION, HttpResponse, RpcHandler,
    RpcHandlerFuture, RpcResult, install_host,
};
use seekdeep_cordis::{Context, Fiber};
use seekdeep_host_webserver::{ListenHost, WebServer, WebServerConfig};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
    sync::oneshot,
};

fn handler(
    callback: impl Fn(String, Value, seekdeep_llm::AbortSignal) -> RpcHandlerFuture
    + Send
    + Sync
    + 'static,
) -> RpcHandler {
    Arc::new(callback)
}

async fn raw(port: u16, request: &str) -> anyhow::Result<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await?;
    stream.write_all(request.as_bytes()).await?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await?;
    Ok(String::from_utf8(response)?)
}

fn request(_port: u16, method: &str, path: &str, host: &str, body: &str) -> String {
    format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

fn status(response: &str) -> u16 {
    response
        .split_ascii_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap()
}

fn body(response: &str) -> &str {
    response.split_once("\r\n\r\n").unwrap().1
}

async fn server(context: &Context) -> Arc<WebServer> {
    WebServer::install(
        context,
        WebServerConfig {
            host: ListenHost::Loopback,
            port: 0,
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn trust_privilege_and_event_upgrade_boundaries_run_over_real_http() {
    let context = Context::new();
    let webserver = server(&context).await;
    install_host(
        &context,
        ConnectionHostConfig {
            trusted_hosts: vec!["harness.example".to_owned()],
            ..ConnectionHostConfig::default()
        },
        None,
    )
    .unwrap();
    for (host, expected) in [
        ("other.example", 403),
        ("127.0.0.1:3080", 404),
        ("harness.example", 404),
    ] {
        let response = raw(
            webserver.port(),
            &request(webserver.port(), "GET", "/api/session.list", host, ""),
        )
        .await
        .unwrap();
        assert_eq!(status(&response), expected, "{host}: {response}");
    }
    for method in [
        "settings.describe",
        "credentials.describe",
        "host.openPath",
        "llm.discoverModels",
        "agentPreset.read",
    ] {
        let response = raw(
            webserver.port(),
            &request(
                webserver.port(),
                "GET",
                &format!("/api/{method}"),
                "harness.example",
                "",
            ),
        )
        .await
        .unwrap();
        assert_eq!(status(&response), 403, "{method}");
    }
    assert_eq!(
        status(
            &raw(
                webserver.port(),
                &request(
                    webserver.port(),
                    "GET",
                    "/api/llm.models",
                    "harness.example",
                    "",
                ),
            )
            .await
            .unwrap()
        ),
        404
    );
    for path in ["/api/events.mux", "/api/events.host"] {
        let response = raw(
            webserver.port(),
            &request(webserver.port(), "GET", path, "127.0.0.1:3080", ""),
        )
        .await
        .unwrap();
        assert_eq!(status(&response), 426);
        assert!(response.to_ascii_lowercase().contains("upgrade: websocket"));
        assert_eq!(body(&response), "upgrade required");
    }
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn claimed_gateway_shape_round_trips_and_unclaimed_falls_back() {
    let context = Context::new();
    let webserver = server(&context).await;
    let connection = install_host(&context, ConnectionHostConfig::default(), None).unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    connection
        .intercept(
            &context,
            "/api",
            Arc::new(|endpoint| endpoint == "goals/create"),
            handler({
                let calls = calls.clone();
                move |endpoint, payload, _signal| {
                    calls.lock().push((endpoint, payload));
                    Box::pin(async {
                        Ok(RpcResult::Success {
                            value: Some(json!({ "accepted": true })),
                        })
                    })
                }
            }),
            ConnectionRpcAuthority::TrustedHost,
        )
        .unwrap();
    let envelope = json!({
        "type": "client-request",
        "rpcId": "real-http",
        "method": "goals/create",
        "payload": { "args": { "agentId": "a1" } },
    })
    .to_string();
    let response = raw(
        webserver.port(),
        &request(
            webserver.port(),
            "POST",
            "/api/goals/create",
            "127.0.0.1:3080",
            &envelope,
        ),
    )
    .await
    .unwrap();
    assert_eq!(status(&response), 200);
    assert_eq!(
        serde_json::from_str::<Value>(body(&response)).unwrap(),
        json!({
            "type": "server-response",
            "rpcId": "real-http",
            "result": { "ok": true, "value": { "accepted": true } },
        })
    );
    assert_eq!(calls.lock().len(), 1);
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn declared_and_streamed_oversize_bodies_answer_413_and_close() {
    let context = Context::new();
    let webserver = server(&context).await;
    install_host(
        &context,
        ConnectionHostConfig {
            max_request_body_bytes: 5,
            ..ConnectionHostConfig::default()
        },
        None,
    )
    .unwrap();
    let declared = raw(
        webserver.port(),
        &format!(
            "POST /api/read HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n",
            webserver.port()
        ),
    )
    .await
    .unwrap();
    assert_eq!(status(&declared), 413);
    assert!(declared.to_ascii_lowercase().contains("connection: close"));
    let streamed = raw(
        webserver.port(),
        &format!(
            "POST /api/read HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n6\r\n123456\r\n0\r\n\r\n",
            webserver.port()
        ),
    )
    .await
    .unwrap();
    assert_eq!(status(&streamed), 413);
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn client_disconnect_aborts_a_pending_claimed_request() {
    let context = Context::new();
    let webserver = server(&context).await;
    let connection = install_host(&context, ConnectionHostConfig::default(), None).unwrap();
    let (started_tx, started_rx) = oneshot::channel();
    let started_tx = Arc::new(Mutex::new(Some(started_tx)));
    let carrier_signal = Arc::new(Mutex::new(None));
    let observed_signal = carrier_signal.clone();
    connection
        .intercept(
            &context,
            "/api",
            Arc::new(|endpoint| endpoint == "host/pick"),
            handler(move |_endpoint, _payload, signal| {
                let started_tx = started_tx.clone();
                let observed_signal = observed_signal.clone();
                Box::pin(async move {
                    *observed_signal.lock() = Some(signal);
                    if let Some(started) = started_tx.lock().take() {
                        let _ = started.send(());
                    }
                    std::future::pending().await
                })
            }),
            ConnectionRpcAuthority::TrustedHost,
        )
        .unwrap();
    let envelope = json!({
        "type": "client-request",
        "rpcId": "disconnect",
        "method": "host/pick",
        "payload": {},
    })
    .to_string();
    let mut stream = TcpStream::connect(("127.0.0.1", webserver.port()))
        .await
        .unwrap();
    stream
        .write_all(
            request(
                webserver.port(),
                "POST",
                "/api/host/pick",
                "127.0.0.1:3080",
                &envelope,
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    started_rx.await.unwrap();
    drop(stream);
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if carrier_signal
                .lock()
                .as_ref()
                .is_some_and(seekdeep_llm::AbortSignal::is_aborted)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("disconnect must abort the handler's carrier signal");
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn capacity_failure_precedes_service_and_route_publication() {
    let context = Context::new();
    let webserver = server(&context).await;
    let error = install_host(
        &context,
        ConnectionHostConfig {
            max_request_body_bytes: 1024,
            max_message_image_bytes: Some(20 * 1024 * 1024),
            ..ConnectionHostConfig::default()
        },
        Some(Arc::new(|_| {
            Box::pin(async { HttpResponse::text(200, "fallback") })
        })),
    )
    .unwrap_err();
    assert!(error.to_string().contains("aggregate image limit"));
    assert!(context.get(HOST_CONNECTION).is_none());
    let response = raw(
        webserver.port(),
        &request(webserver.port(), "GET", "/api/read", "127.0.0.1:3080", ""),
    )
    .await
    .unwrap();
    assert_eq!(status(&response), 404);
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn connection_route_and_service_leave_with_their_calling_fiber() {
    let context = Context::new();
    let webserver = server(&context).await;
    let fiber = Fiber::active_child("connection");
    let child = context.with_fiber(fiber.clone());
    install_host(&child, ConnectionHostConfig::default(), None).unwrap();
    assert_eq!(
        status(
            &raw(
                webserver.port(),
                &request(webserver.port(), "GET", "/api/read", "127.0.0.1:3080", "",),
            )
            .await
            .unwrap()
        ),
        404
    );
    fiber.dispose().await.unwrap();
    assert!(context.get(HOST_CONNECTION).is_none());
    assert_eq!(
        status(
            &raw(
                webserver.port(),
                &request(webserver.port(), "GET", "/api/read", "127.0.0.1:3080", "",),
            )
            .await
            .unwrap()
        ),
        404
    );
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn dedicated_rpc_channel_mounts_a_real_prefix_route_and_withdraws_atomically() {
    let context = Context::new();
    let webserver = server(&context).await;
    let connection = install_host(
        &context,
        ConnectionHostConfig {
            trusted_hosts: vec!["harness.example".to_owned()],
            ..ConnectionHostConfig::default()
        },
        None,
    )
    .unwrap();
    let registration = connection
        .handle(
            &context,
            "/rpc",
            handler(|endpoint, payload, _signal| {
                Box::pin(async move {
                    Ok(RpcResult::Success {
                        value: Some(json!({ "endpoint": endpoint, "payload": payload })),
                    })
                })
            }),
            ConnectionRpcAuthority::TrustedHost,
        )
        .unwrap();
    let envelope = json!({
        "type": "client-request",
        "rpcId": "dedicated",
        "method": "goals/create",
        "payload": { "agentId": "a" },
    })
    .to_string();
    let response = raw(
        webserver.port(),
        &request(
            webserver.port(),
            "POST",
            "/rpc/goals/create",
            "harness.example",
            &envelope,
        ),
    )
    .await
    .unwrap();
    assert_eq!(status(&response), 200);
    assert_eq!(
        serde_json::from_str::<Value>(body(&response)).unwrap(),
        json!({
            "type": "server-response",
            "rpcId": "dedicated",
            "result": {
                "ok": true,
                "value": {
                    "endpoint": "goals/create",
                    "payload": { "agentId": "a" },
                },
            },
        })
    );
    let duplicate = connection
        .handle(
            &context,
            "/rpc",
            handler(|_, _, _| Box::pin(async { Ok(RpcResult::Success { value: None }) })),
            ConnectionRpcAuthority::TrustedHost,
        )
        .unwrap_err();
    assert!(duplicate.to_string().contains("duplicate"));
    registration.dispose().await.unwrap();
    let withdrawn = raw(
        webserver.port(),
        &request(
            webserver.port(),
            "POST",
            "/rpc/goals/create",
            "harness.example",
            &envelope,
        ),
    )
    .await
    .unwrap();
    assert_eq!(status(&withdrawn), 404);
    context.fiber().dispose().await.unwrap();
}
