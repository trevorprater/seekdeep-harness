//! Real-socket SPA fallback, MIME, traversal, tap, and lifecycle contracts.

use std::sync::Arc;

use seekdeep_cordis::Context;
use seekdeep_host_frontend_static::{FrontendStaticConfig, install, invariant::register_invariant};
use seekdeep_host_webserver::{ListenHost, WebServer, WebServerConfig, response};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
};

async fn raw_request(port: u16, method: &str, path: &str) -> anyhow::Result<Vec<u8>> {
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
    Ok(response)
}

fn status(response: &[u8]) -> u16 {
    std::str::from_utf8(response)
        .expect("HTTP response")
        .split_ascii_whitespace()
        .nth(1)
        .expect("status")
        .parse()
        .expect("numeric status")
}

fn headers(response: &[u8]) -> &str {
    let boundary = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("header boundary");
    std::str::from_utf8(&response[..boundary]).expect("headers")
}

fn body(response: &[u8]) -> &[u8] {
    let boundary = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("header boundary");
    &response[boundary + 4..]
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn serves_assets_spa_fallback_taps_traversal_and_method_gate() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let dist = temporary.path().join("dist");
    std::fs::create_dir(&dist).expect("dist");
    let index = dist.join("index.html");
    std::fs::write(&index, "<head></head><body>shell</body>").expect("index");
    std::fs::write(dist.join("app.js"), "export {}").expect("app");
    std::fs::write(dist.join("app.wasm"), b"\0asm").expect("wasm");
    std::fs::write(dist.join("blob.bin"), "BLOB").expect("blob");
    std::fs::write(dist.join("manifest.webmanifest"), "{}").expect("manifest");

    let context = Context::new();
    let server = WebServer::install(
        &context,
        WebServerConfig {
            host: ListenHost::Loopback,
            port: 0,
        },
    )
    .await
    .expect("server");
    let frontend = install(&context, FrontendStaticConfig { dist_index: index }).expect("frontend");

    let javascript = raw_request(server.port(), "GET", "/app.js")
        .await
        .expect("javascript");
    assert_eq!(status(&javascript), 200);
    assert!(headers(&javascript).contains("content-type: text/javascript; charset=utf-8"));
    assert_eq!(body(&javascript), b"export {}");

    let wasm = raw_request(server.port(), "GET", "/app.wasm")
        .await
        .expect("wasm");
    assert!(headers(&wasm).contains("content-type: application/wasm"));
    assert_eq!(body(&wasm), b"\0asm");

    let manifest = raw_request(server.port(), "GET", "/manifest.webmanifest")
        .await
        .expect("manifest");
    assert!(headers(&manifest).contains("content-type: application/manifest+json"));
    assert_eq!(body(&manifest), b"{}");
    std::fs::write(dist.join("app.js"), "export const rebuilt = true").expect("rebuild");
    assert_eq!(
        body(
            &raw_request(server.port(), "GET", "/app.js")
                .await
                .expect("rebuilt")
        ),
        b"export const rebuilt = true"
    );

    let blob = raw_request(server.port(), "GET", "/blob.bin")
        .await
        .expect("blob");
    assert!(headers(&blob).contains("content-type: application/octet-stream"));
    assert_eq!(body(&blob), b"BLOB");

    let tap = server.tap_index(Arc::new(|html| {
        html.replace("<head>", "<head><script>window.__T__=1</script>")
    }));
    for path in ["/", "/index.html", "/no/such/route"] {
        let response = raw_request(server.port(), "GET", path).await.expect("SPA");
        assert_eq!(status(&response), 200);
        let body = std::str::from_utf8(body(&response)).expect("body");
        assert!(body.contains("__T__"));
        assert!(body.contains("shell"));
    }
    tap.dispose();
    assert!(
        !std::str::from_utf8(body(
            &raw_request(server.port(), "GET", "/")
                .await
                .expect("untapped")
        ))
        .expect("body")
        .contains("__T__")
    );

    assert_eq!(
        status(
            &raw_request(server.port(), "GET", "/..%2f..%2fetc%2fpasswd")
                .await
                .expect("traversal")
        ),
        403
    );
    assert_eq!(
        status(
            &raw_request(server.port(), "POST", "/nowhere")
                .await
                .expect("method")
        ),
        405
    );

    frontend.dispose().await.expect("release fallback");
    assert_eq!(
        status(
            &raw_request(server.port(), "GET", "/no/such/route")
                .await
                .expect("unclaimed")
        ),
        404
    );
    assert!(
        server
            .register_fallback(Arc::new(|_| {
                Box::pin(async { Ok(response(hyper::StatusCode::OK, "replacement")) })
            }))
            .is_ok()
    );
    context.fiber().dispose().await.expect("dispose server");
}

#[tokio::test]
async fn invalid_percent_encoding_is_a_bad_request_and_head_has_no_wire_body() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let index = temporary.path().join("index.html");
    std::fs::write(&index, "shell").expect("index");
    let context = Context::new();
    let server = WebServer::install(
        &context,
        WebServerConfig {
            host: ListenHost::Loopback,
            port: 0,
        },
    )
    .await
    .expect("server");
    install(&context, FrontendStaticConfig { dist_index: index }).expect("frontend");
    assert_eq!(
        status(
            &raw_request(server.port(), "GET", "/%FF")
                .await
                .expect("invalid UTF-8")
        ),
        400
    );
    let head = raw_request(server.port(), "HEAD", "/").await.expect("head");
    assert_eq!(status(&head), 200);
    assert!(body(&head).is_empty());
    context.fiber().dispose().await.expect("dispose");
}

#[tokio::test]
async fn explained_empty_invariant_reserves_and_releases_identity() {
    let context = Context::new();
    let registry = Arc::new(
        seekdeep_invariants::InvariantRegistry::new(
            &context,
            &seekdeep_invariants::InvariantConfig::default(),
        )
        .expect("registry"),
    );
    let registration = register_invariant(&registry).expect("register");
    registration.await_ready().await.expect("ready");
    assert!(register_invariant(&registry).is_err());
    registration.dispose().await.expect("dispose");
    register_invariant(&registry)
        .expect("replacement")
        .await_ready()
        .await
        .expect("ready replacement");
}
