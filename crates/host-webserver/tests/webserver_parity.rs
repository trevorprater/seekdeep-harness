//! Real-socket behavioral mirror of the Host Webserver composition specs.

use std::sync::Arc;

use bytes::Bytes;
use seekdeep_cordis::Context;
use seekdeep_host_webserver::{
    ListenHost, WebHandler, WebHandlerFuture, WebRoute, WebRouteKind, WebServer, WebServerConfig,
    WebUpgradeRoute, response, switching_protocols,
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
};

fn handler(
    callback: impl Fn(
        seekdeep_host_webserver::WebRequest,
    ) -> anyhow::Result<seekdeep_host_webserver::WebResponse>
    + Send
    + Sync
    + 'static,
) -> WebHandler {
    let callback = Arc::new(callback);
    Arc::new(move |request| {
        let callback = callback.clone();
        Box::pin(async move { callback(request) }) as WebHandlerFuture
    })
}

fn text(value: &'static str) -> WebHandler {
    handler(move |_| {
        Ok(response(
            hyper::StatusCode::OK,
            Bytes::from_static(value.as_bytes()),
        ))
    })
}

async fn raw_request(port: u16, method: &str, path: &str) -> anyhow::Result<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await?;
    stream
        .write_all(
            format!(
                "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
            )
            .as_bytes(),
        )
        .await?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await?;
    Ok(String::from_utf8(response)?)
}

fn status(response: &str) -> u16 {
    response
        .split_ascii_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap()
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn serves_route_precedence_index_taps_fallback_and_disposal() {
    let context = Context::new();
    let server = WebServer::install(
        &context,
        WebServerConfig {
            host: ListenHost::Loopback,
            port: 0,
        },
    )
    .await
    .unwrap();
    assert!(server.port() > 0);
    assert_eq!(server.host(), ListenHost::Loopback);

    server
        .register(WebRoute {
            kind: WebRouteKind::Exact,
            path: "/probe".to_owned(),
            handler: text("EXACT"),
        })
        .unwrap();
    server
        .register(WebRoute {
            kind: WebRouteKind::Prefix,
            path: "/api".to_owned(),
            handler: text("API"),
        })
        .unwrap();
    server
        .register(WebRoute {
            kind: WebRouteKind::Prefix,
            path: "/api/deep".to_owned(),
            handler: text("DEEP"),
        })
        .unwrap();
    for (path, expected) in [
        ("/probe", "EXACT"),
        ("/api", "API"),
        ("/api/anything", "API"),
        ("/api/deep/leaf", "DEEP"),
    ] {
        let received = raw_request(server.port(), "GET", path).await.unwrap();
        assert_eq!(status(&received), 200);
        assert!(received.ends_with(expected), "{path}: {received}");
    }
    assert!(
        raw_request(server.port(), "POST", "/api/anything")
            .await
            .unwrap()
            .ends_with("API")
    );
    assert_eq!(
        status(
            &raw_request(server.port(), "GET", "/unclaimed")
                .await
                .unwrap()
        ),
        404
    );

    let tap = server.tap_index(Arc::new(|html| {
        html.replace("<head>", "<head><script>window.__T__=1</script>")
    }));
    let fallback_server = server.clone();
    let fallback = server
        .register_fallback(handler(move |request| {
            anyhow::ensure!(request.uri().path() != "/%zz", "malformed escape");
            Ok(response(
                hyper::StatusCode::OK,
                fallback_server.apply_index_taps("<head></head><body>shell</body>"),
            ))
        }))
        .unwrap();
    assert!(
        server
            .register_fallback(text("OTHER"))
            .unwrap_err()
            .to_string()
            .contains("fallback already registered")
    );
    assert!(
        raw_request(server.port(), "GET", "/unclaimed")
            .await
            .unwrap()
            .contains("__T__")
    );
    tap.dispose();
    let untapped = raw_request(server.port(), "GET", "/unclaimed")
        .await
        .unwrap();
    assert!(!untapped.contains("__T__"));
    assert!(untapped.contains("shell"));
    assert_eq!(
        status(&raw_request(server.port(), "GET", "/%zz").await.unwrap()),
        400
    );
    assert_eq!(
        status(&raw_request(server.port(), "GET", "/probe").await.unwrap()),
        200
    );

    assert!(
        server
            .register(WebRoute {
                kind: WebRouteKind::Exact,
                path: "/probe".to_owned(),
                handler: text("DUPLICATE"),
            })
            .is_err()
    );
    let once = server
        .register(WebRoute {
            kind: WebRouteKind::Exact,
            path: "/once".to_owned(),
            handler: text("ONCE"),
        })
        .unwrap();
    assert!(
        raw_request(server.port(), "GET", "/once")
            .await
            .unwrap()
            .ends_with("ONCE")
    );
    once.dispose();
    assert!(
        raw_request(server.port(), "GET", "/once")
            .await
            .unwrap()
            .ends_with("<head></head><body>shell</body>")
    );
    assert!(
        server
            .register(WebRoute {
                kind: WebRouteKind::Exact,
                path: "/once".to_owned(),
                handler: text("REPLACEMENT"),
            })
            .is_ok()
    );
    fallback.dispose();
    assert_eq!(
        status(&raw_request(server.port(), "GET", "/another").await.unwrap()),
        404
    );
    assert!(server.register_fallback(text("RESTORED")).is_ok());
    context.fiber().dispose().await.unwrap();
    assert!(raw_request(server.port(), "GET", "/probe").await.is_err());
}

#[tokio::test]
async fn upgrade_routes_are_exact_query_agnostic_and_disposable() {
    let context = Context::new();
    let server = WebServer::install(
        &context,
        WebServerConfig {
            host: ListenHost::Loopback,
            port: 0,
        },
    )
    .await
    .unwrap();
    let registration = server
        .register_upgrade(WebUpgradeRoute {
            path: "/events".to_owned(),
            handler: handler(|_| switching_protocols("seekdeep-test")),
        })
        .unwrap();
    assert!(
        server
            .register_upgrade(WebUpgradeRoute {
                path: "/events".to_owned(),
                handler: text("duplicate"),
            })
            .is_err()
    );
    let mut stream = TcpStream::connect(("127.0.0.1", server.port()))
        .await
        .unwrap();
    stream
        .write_all(
            format!(
                "GET /events?stream=mux HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: Upgrade\r\nUpgrade: seekdeep-test\r\n\r\n",
                server.port()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut buffer = [0_u8; 256];
    let read = stream.read(&mut buffer).await.unwrap();
    let response = String::from_utf8_lossy(&buffer[..read]);
    assert!(response.contains("101 Switching Protocols"), "{response}");
    registration.dispose();
    assert!(
        server
            .register_upgrade(WebUpgradeRoute {
                path: "/events".to_owned(),
                handler: text("replacement"),
            })
            .is_ok()
    );
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn taken_port_fails_before_service_publication() {
    let first_context = Context::new();
    let first = WebServer::install(
        &first_context,
        WebServerConfig {
            host: ListenHost::Loopback,
            port: 0,
        },
    )
    .await
    .unwrap();
    let second_context = Context::new();
    let error = WebServer::install(
        &second_context,
        WebServerConfig {
            host: ListenHost::Loopback,
            port: first.port(),
        },
    )
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains("in use")
            || error.to_string().contains("Address already in use")
    );
    assert!(
        second_context
            .get(seekdeep_host_webserver::WEB_SERVER)
            .is_none()
    );
    first_context.fiber().dispose().await.unwrap();
}
