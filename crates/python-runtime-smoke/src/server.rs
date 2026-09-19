//! Owned loopback HTTP/SSE model; shutdown joins the listener and every connection.

use std::sync::{Arc, Mutex};

use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper::{Request, Response, body::Incoming, server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use serde_json::Value;
use tokio::{
    net::TcpListener,
    sync::oneshot,
    task::{JoinHandle, JoinSet},
};

#[derive(Default)]
struct Observations {
    requests: Vec<Value>,
    errors: Vec<String>,
}

pub(crate) struct MockModel {
    pub(crate) url: String,
    observations: Arc<Mutex<Observations>>,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<anyhow::Result<()>>>,
}

impl MockModel {
    pub(crate) async fn start() -> anyhow::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let url = format!("http://{}", listener.local_addr()?);
        let observations = Arc::new(Mutex::new(Observations::default()));
        let state = Arc::clone(&observations);
        let (shutdown, mut stop) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    _ = &mut stop => break,
                    Some(_) = connections.join_next(), if !connections.is_empty() => {},
                    accepted = listener.accept() => {
                        let (socket, _) = accepted?;
                        let state = Arc::clone(&state);
                        connections.spawn(async move {
                            let service = service_fn(move |request| respond(request, Arc::clone(&state)));
                            let _ = http1::Builder::new().keep_alive(false)
                                .serve_connection(TokioIo::new(socket), service).await;
                        });
                    }
                }
            }
            connections.abort_all();
            while connections.join_next().await.is_some() {}
            Ok(())
        });
        Ok(Self {
            url,
            observations,
            shutdown: Some(shutdown),
            task: Some(task),
        })
    }

    pub(crate) async fn close(mut self, verify_requests: bool) -> anyhow::Result<()> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.task.take() {
            task.await??;
        }
        let state = self.observations.lock().expect("model observations lock");
        anyhow::ensure!(
            state.errors.is_empty(),
            "mock model failed: {}",
            state.errors.join("; ")
        );
        if verify_requests {
            anyhow::ensure!(
                !state.requests.is_empty(),
                "mock model endpoint received no requests"
            );
        }
        Ok(())
    }
}

impl Drop for MockModel {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

async fn respond(
    request: Request<Incoming>,
    state: Arc<Mutex<Observations>>,
) -> Result<Response<Full<Bytes>>, std::io::Error> {
    if request.method() != hyper::Method::POST {
        return Ok(Response::builder()
            .status(501)
            .body(Full::new(Bytes::from(format!(
                "Unsupported method ('{}')",
                request.method()
            ))))
            .expect("fixed method error response"));
    }
    let bytes = request
        .into_body()
        .collect()
        .await
        .map_err(std::io::Error::other)?
        .to_bytes();
    let request: Value = serde_json::from_slice(&bytes).map_err(std::io::Error::other)?;
    state
        .lock()
        .expect("model observations lock")
        .requests
        .push(request.clone());
    let chunks = crate::model::completion_chunks(&request);
    let mut body = String::new();
    match chunks {
        Ok(chunks) => {
            for chunk in chunks {
                body.push_str("data: ");
                body.push_str(&crate::json::dumps(&chunk, false, true));
                body.push_str("\n\n");
            }
            body.push_str("data: [DONE]\n\n");
        }
        Err(error) => state
            .lock()
            .expect("model observations lock")
            .errors
            .push(error.to_string()),
    }
    Ok(Response::builder()
        .header("content-type", "text/event-stream")
        .body(Full::new(Bytes::from(body)))
        .expect("fixed HTTP response"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    async fn request(url: &str, method: &str, body: &str) -> String {
        let mut socket = tokio::net::TcpStream::connect(url.strip_prefix("http://").unwrap())
            .await
            .unwrap();
        let message = format!(
            "{method} /chat/completions HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(message.as_bytes()).await.unwrap();
        let mut bytes = Vec::new();
        socket.read_to_end(&mut bytes).await.unwrap();
        String::from_utf8(bytes).unwrap()
    }

    #[tokio::test]
    async fn sse_response_and_shutdown_are_real_socket_operations() {
        let model = MockModel::start().await.unwrap();
        let resources = Arc::downgrade(&model.observations);
        let address = model.url.strip_prefix("http://").unwrap().to_owned();
        let response = request(
            &model.url,
            "POST",
            r#"{"messages":[{"role":"user","content":"hello"}]}"#,
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 200"));
        assert!(response.contains("content-type: text/event-stream"));
        let body = response.split_once("\r\n\r\n").unwrap().1;
        assert_eq!(body.matches("data: ").count(), 4);
        assert!(body.contains("runtime smoke ok"));
        assert!(body.ends_with("data: [DONE]\n\n"));
        model.close(true).await.unwrap();
        assert!(resources.upgrade().is_none());
        assert!(tokio::net::TcpStream::connect(address).await.is_err());
    }

    #[tokio::test]
    async fn method_and_model_failures_cannot_be_reported_as_success() {
        let model = MockModel::start().await.unwrap();
        assert!(
            request(&model.url, "GET", "")
                .await
                .starts_with("HTTP/1.1 501")
        );
        assert!(
            model
                .close(true)
                .await
                .unwrap_err()
                .to_string()
                .contains("received no requests")
        );
        let model = MockModel::start().await.unwrap();
        let response = request(&model.url, "POST", "{}").await;
        assert!(response.starts_with("HTTP/1.1 200"));
        assert!(!response.contains("[DONE]"));
        assert!(
            model
                .close(true)
                .await
                .unwrap_err()
                .to_string()
                .contains("model request has no messages")
        );
        let model = MockModel::start().await.unwrap();
        assert!(request(&model.url, "POST", "{malformed").await.is_empty());
        assert!(
            model
                .close(true)
                .await
                .unwrap_err()
                .to_string()
                .contains("received no requests")
        );
    }
}
