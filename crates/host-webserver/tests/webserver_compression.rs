//! Response compression: which complete bodies the Host webserver encodes, and which it leaves
//! alone.

use std::{io::Read as _, sync::Arc};

use bytes::Bytes;
use flate2::read::GzDecoder;
use http_body_util::{BodyExt as _, StreamBody};
use hyper::{StatusCode, body::Frame, header};
use seekdeep_cordis::Context;
use seekdeep_host_webserver::{
    ListenHost, WebHandler, WebHandlerFuture, WebResponse, WebRoute, WebRouteKind, WebServer,
    WebServerConfig, response,
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
};

fn typed(content_type: &'static str, body: String) -> WebHandler {
    let body = Arc::new(body);
    Arc::new(move |_request| {
        let body = body.clone();
        Box::pin(async move {
            let mut response = response(StatusCode::OK, Bytes::from(body.as_bytes().to_vec()));
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static(content_type),
            );
            Ok(response) as anyhow::Result<WebResponse>
        }) as WebHandlerFuture
    })
}

/// An event stream: the body has no exact length, so it must never be buffered for encoding.
fn event_stream() -> WebHandler {
    Arc::new(move |_request| {
        Box::pin(async move {
            let frames = vec![
                Ok::<_, std::io::Error>(Frame::data(Bytes::from_static(b"data: first\n\n"))),
                Ok(Frame::data(Bytes::from_static(b"data: second\n\n"))),
            ];
            let mut response = response(StatusCode::OK, Bytes::new());
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("text/event-stream"),
            );
            *response.body_mut() = StreamBody::new(futures::stream::iter(frames)).boxed_unsync();
            Ok(response) as anyhow::Result<WebResponse>
        }) as WebHandlerFuture
    })
}

async fn request(port: u16, path: &str, accept_encoding: Option<&str>) -> anyhow::Result<Vec<u8>> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await?;
    let encoding =
        accept_encoding.map_or_else(String::new, |value| format!("Accept-Encoding: {value}\r\n"));
    stream
        .write_all(
            format!(
                "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n{encoding}Content-Length: 0\r\n\r\n"
            )
            .as_bytes(),
        )
        .await?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await?;
    Ok(response)
}

fn split(response: &[u8]) -> (String, &[u8]) {
    let boundary = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("response has a header boundary");
    (
        String::from_utf8_lossy(&response[..boundary]).to_ascii_lowercase(),
        &response[boundary + 4..],
    )
}

fn gunzip(body: &[u8]) -> String {
    let mut decoded = String::new();
    GzDecoder::new(body)
        .read_to_string(&mut decoded)
        .expect("gzip body decodes");
    decoded
}

async fn install(routes: Vec<(&str, WebHandler)>) -> anyhow::Result<Arc<WebServer>> {
    let context = Context::new();
    let server = WebServer::install(
        &context,
        WebServerConfig {
            host: ListenHost::Loopback,
            port: 0,
        },
    )
    .await?;
    for (path, handler) in routes {
        server.register(WebRoute {
            kind: WebRouteKind::Exact,
            path: path.to_owned(),
            handler,
        })?;
    }
    Ok(server)
}

#[tokio::test]
async fn compresses_complete_compressible_bodies_and_leaves_the_rest_alone() {
    let script = "const value = 1;\n".repeat(200);
    let tiny = "const value = 1;\n".to_owned();
    assert!(script.len() > 1024 && tiny.len() < 1024);

    let server = install(vec![
        (
            "/bundle.js",
            typed("text/javascript; charset=utf-8", script.clone()),
        ),
        ("/module.wasm", typed("application/wasm", script.clone())),
        ("/payload.json", typed("application/json", script.clone())),
        ("/icon.png", typed("image/png", script.clone())),
        (
            "/tiny.js",
            typed("text/javascript; charset=utf-8", tiny.clone()),
        ),
    ])
    .await
    .unwrap();

    for path in ["/bundle.js", "/module.wasm", "/payload.json"] {
        let response = request(server.port(), path, Some("gzip, deflate, br"))
            .await
            .unwrap();
        let (headers, body) = split(&response);
        assert!(
            headers.contains("content-encoding: gzip"),
            "{path}: {headers}"
        );
        assert!(
            headers.contains("vary: accept-encoding"),
            "{path}: {headers}"
        );
        assert_eq!(gunzip(body), script, "{path} round-trips");
    }

    for (path, expected) in [("/icon.png", script.as_str()), ("/tiny.js", tiny.as_str())] {
        let response = request(server.port(), path, Some("gzip")).await.unwrap();
        let (headers, body) = split(&response);
        assert!(!headers.contains("content-encoding"), "{path}: {headers}");
        assert_eq!(body, expected.as_bytes(), "{path} is verbatim");
    }

    for encoding in [
        None,
        Some("identity"),
        Some("gzip;q=0"),
        Some("br, deflate"),
    ] {
        let response = request(server.port(), "/bundle.js", encoding)
            .await
            .unwrap();
        let (headers, body) = split(&response);
        assert!(
            !headers.contains("content-encoding"),
            "{encoding:?}: {headers}"
        );
        assert_eq!(body, script.as_bytes(), "{encoding:?} is verbatim");
    }
}

#[tokio::test]
async fn an_event_stream_still_flows_frame_by_frame() {
    let server = install(vec![("/events", event_stream())]).await.unwrap();
    let response = request(server.port(), "/events", Some("gzip"))
        .await
        .unwrap();
    let (headers, body) = split(&response);
    assert!(
        !headers.contains("content-encoding"),
        "a stream is never buffered for encoding: {headers}"
    );
    // The frames arrive as their own chunks, so the stream was not collected before sending.
    assert!(headers.contains("transfer-encoding: chunked"), "{headers}");
    let body = String::from_utf8_lossy(body);
    assert!(body.contains("data: first\n\n"), "{body:?}");
    assert!(body.contains("data: second\n\n"), "{body:?}");
}
