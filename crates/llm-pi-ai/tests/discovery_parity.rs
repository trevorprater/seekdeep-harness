//! Catalog and bounded endpoint discovery parity tests.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use seekdeep_llm::{AbortSignal, LlmError, LlmModelDiscoveryRequest, ProviderId, user_agent};
use seekdeep_llm_pi_ai::{
    catalog::builtin_catalog,
    discovery::{MAX_RESPONSE_BYTES, StoredApiKeyResolver, discover_models},
};
use serde_json::json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::oneshot,
};

struct KeyResolver {
    calls: AtomicUsize,
    key: Option<String>,
}

#[async_trait]
impl StoredApiKeyResolver for KeyResolver {
    async fn resolve(&self) -> anyhow::Result<Option<String>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.key.clone())
    }
}

struct TestResponse {
    status: u16,
    chunks: Vec<Vec<u8>>,
    declared_length: Option<usize>,
    hold_open: bool,
}

async fn server(response: TestResponse) -> (String, oneshot::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (request_tx, request_rx) = oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        loop {
            let mut buffer = [0_u8; 1024];
            let read = socket.read(&mut buffer).await.unwrap();
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let _ = request_tx.send(String::from_utf8_lossy(&request).into_owned());
        let reason = if response.status == 200 {
            "OK"
        } else {
            "Error"
        };
        if let Some(length) = response.declared_length {
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 {} {reason}\r\ncontent-type: application/json\r\ncontent-length: {length}\r\nconnection: close\r\n\r\n",
                        response.status
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            for chunk in response.chunks {
                socket.write_all(&chunk).await.unwrap();
            }
        } else {
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 {} {reason}\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n",
                        response.status
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            for chunk in response.chunks {
                socket
                    .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                    .await
                    .unwrap();
                socket.write_all(&chunk).await.unwrap();
                socket.write_all(b"\r\n").await.unwrap();
            }
            if !response.hold_open {
                socket.write_all(b"0\r\n\r\n").await.unwrap();
            }
        }
        if response.hold_open {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        }
    });
    (format!("http://{address}"), request_rx)
}

fn request(base_url: Option<String>) -> LlmModelDiscoveryRequest {
    LlmModelDiscoveryRequest {
        base_url,
        ..LlmModelDiscoveryRequest::default()
    }
}

fn llm_error(error: &anyhow::Error) -> &LlmError {
    error.downcast_ref::<LlmError>().unwrap()
}

#[tokio::test]
async fn catalog_routes_short_circuit_endpoint_and_stored_credentials() {
    let resolver = KeyResolver {
        calls: AtomicUsize::new(0),
        key: Some("stored".to_owned()),
    };
    let request = LlmModelDiscoveryRequest {
        provider: Some(ProviderId::new("deepseek")),
        base_url: Some("http://127.0.0.1:9".to_owned()),
        ..LlmModelDiscoveryRequest::default()
    };
    let models = discover_models(
        &reqwest::Client::new(),
        builtin_catalog(),
        &request,
        Some(&resolver),
    )
    .await
    .unwrap();
    assert_eq!(models.len(), 2);
    assert!(models.iter().all(|model| {
        model.context_window.unwrap_or_default() > 0 && model.max_tokens.unwrap_or_default() > 0
    }));
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn missing_endpoint_and_unlistable_protocol_have_stable_failures() {
    let missing = LlmModelDiscoveryRequest {
        provider: Some(ProviderId::new("acme-gateway")),
        ..LlmModelDiscoveryRequest::default()
    };
    let error = discover_models(&reqwest::Client::new(), builtin_catalog(), &missing, None)
        .await
        .unwrap_err();
    assert_eq!(llm_error(&error).code(), "DISCOVERY_FAILED");
    assert!(error.to_string().contains("set a baseURL"));

    let mut unsupported = request(Some("https://gateway.example/v1".to_owned()));
    unsupported.api = Some("anthropic-messages".to_owned());
    let error = discover_models(
        &reqwest::Client::new(),
        builtin_catalog(),
        &unsupported,
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(llm_error(&error).code(), "DISCOVERY_UNSUPPORTED");
}

#[tokio::test]
async fn reads_listing_preserves_path_capacities_headers_and_key_precedence() {
    let body = serde_json::to_vec(&json!({
        "data": [
            {"id":"acme-large","display_name":"Acme Large","context_length":65536,"max_output_tokens":4096},
            {"id":"acme-small"}
        ]
    }))
    .unwrap();
    let (url, captured) = server(TestResponse {
        status: 200,
        declared_length: Some(body.len()),
        chunks: vec![body],
        hold_open: false,
    })
    .await;
    let resolver = KeyResolver {
        calls: AtomicUsize::new(0),
        key: Some("stored".to_owned()),
    };
    let mut request = request(Some(format!("{url}/openai/v1/")));
    request.api_key = Some("  typed-key  ".to_owned());
    let models = discover_models(
        &reqwest::Client::new(),
        builtin_catalog(),
        &request,
        Some(&resolver),
    )
    .await
    .unwrap();
    assert_eq!(
        serde_json::to_value(models).unwrap(),
        json!([
            {"id":"acme-large","name":"Acme Large","contextWindow":65536,"maxTokens":4096},
            {"id":"acme-small"}
        ])
    );
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 0);
    let request = captured.await.unwrap().to_ascii_lowercase();
    assert!(request.starts_with("get /openai/v1/models http/1.1"));
    assert!(request.contains("authorization: bearer typed-key"));
    assert!(request.contains(&format!("user-agent: {}", user_agent()).to_ascii_lowercase()));
}

#[tokio::test]
async fn stored_key_is_lazy_and_malformed_rows_are_skipped() {
    let body = br#"{"data":[{"id":"good"},{"id":""},{"name":"missing"},null,{"id":"good"},{"id":"zero","context_length":0,"max_tokens":-1}]}"#.to_vec();
    let (url, captured) = server(TestResponse {
        status: 200,
        declared_length: Some(body.len()),
        chunks: vec![body],
        hold_open: false,
    })
    .await;
    let resolver = KeyResolver {
        calls: AtomicUsize::new(0),
        key: Some("stored-key".to_owned()),
    };
    let request = LlmModelDiscoveryRequest {
        provider: Some(ProviderId::new("acme")),
        base_url: Some(url),
        ..LlmModelDiscoveryRequest::default()
    };
    let models = discover_models(
        &reqwest::Client::new(),
        builtin_catalog(),
        &request,
        Some(&resolver),
    )
    .await
    .unwrap();
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        vec!["good", "good", "zero"]
    );
    assert!(
        captured
            .await
            .unwrap()
            .to_ascii_lowercase()
            .contains("authorization: bearer stored-key")
    );
}

#[tokio::test]
async fn status_json_shape_and_probe_key_failures_are_classified() {
    for status in [401, 403, 500] {
        let body = b"{}".to_vec();
        let (url, _captured) = server(TestResponse {
            status,
            declared_length: Some(body.len()),
            chunks: vec![body],
            hold_open: false,
        })
        .await;
        let error = discover_models(
            &reqwest::Client::new(),
            builtin_catalog(),
            &request(Some(url)),
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(llm_error(&error).code(), "DISCOVERY_FAILED");
        assert_eq!(
            error.to_string().contains("check the API key"),
            status != 500
        );
    }

    for body in [b"not json".to_vec(), br#"{"models":[]}"#.to_vec()] {
        let (url, _captured) = server(TestResponse {
            status: 200,
            declared_length: Some(body.len()),
            chunks: vec![body],
            hold_open: false,
        })
        .await;
        assert!(
            discover_models(
                &reqwest::Client::new(),
                builtin_catalog(),
                &request(Some(url)),
                None,
            )
            .await
            .is_err()
        );
    }

    for key in ["", "sk-😀"] {
        let mut invalid = request(Some("https://acme.test".to_owned()));
        invalid.api_key = Some(key.to_owned());
        let error = discover_models(&reqwest::Client::new(), builtin_catalog(), &invalid, None)
            .await
            .unwrap_err();
        assert_eq!(llm_error(&error).code(), "INVALID_CREDENTIAL");
    }
}

#[tokio::test]
async fn declared_and_streamed_oversize_are_refused() {
    let (url, _captured) = server(TestResponse {
        status: 200,
        declared_length: Some(MAX_RESPONSE_BYTES + 1),
        chunks: vec![],
        hold_open: false,
    })
    .await;
    let error = discover_models(
        &reqwest::Client::new(),
        builtin_catalog(),
        &request(Some(url)),
        None,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("more than 4194304 bytes"));

    let (url, _captured) = server(TestResponse {
        status: 200,
        declared_length: None,
        chunks: vec![vec![b'x'; MAX_RESPONSE_BYTES], vec![b'x']],
        hold_open: false,
    })
    .await;
    let error = discover_models(
        &reqwest::Client::new(),
        builtin_catalog(),
        &request(Some(url)),
        None,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("more than 4194304 bytes"));
}

#[tokio::test]
async fn cancellation_before_request_and_during_body_is_aborted() {
    let signal = AbortSignal::default();
    signal.abort();
    let mut preaborted = request(Some("http://127.0.0.1:9".to_owned()));
    preaborted.signal = Some(signal);
    let error = discover_models(
        &reqwest::Client::new(),
        builtin_catalog(),
        &preaborted,
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(llm_error(&error).code(), "ABORTED");

    let (url, captured) = server(TestResponse {
        status: 200,
        declared_length: None,
        chunks: vec![],
        hold_open: true,
    })
    .await;
    let signal = AbortSignal::default();
    let mut reading = request(Some(url));
    reading.signal = Some(signal.clone());
    let task = tokio::spawn(async move {
        discover_models(&reqwest::Client::new(), builtin_catalog(), &reading, None).await
    });
    let _ = captured.await.unwrap();
    signal.abort();
    let error = task.await.unwrap().unwrap_err();
    assert_eq!(llm_error(&error).code(), "ABORTED");
}

#[test]
fn key_resolver_is_send_sync() {
    fn assert_bounds<T: Send + Sync>() {}
    assert_bounds::<Arc<KeyResolver>>();
}
