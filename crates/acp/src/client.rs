//! ACP client role over one JSON-RPC line transport.

use std::sync::{Arc, Weak};

use futures::future::BoxFuture;
use parking_lot::Mutex;
use seekdeep_sdk_protocol::{BoxedJsonRpcInput, BoxedJsonRpcOutput, JsonRpcLineTransport};
use serde_json::{Map, Value, json};

use crate::types::{
    AcpSessionId, AcpSessionUpdate, AcpStopReason, PROTOCOL_VERSION, PermissionPolicy,
    agent_methods, client_methods,
};

/// Observer for server-to-client session updates.
pub type AcpUpdateObserver = Arc<dyn Fn(&AcpSessionUpdate) + Send + Sync>;

/// Caller-supplied asynchronous policy for one ACP permission request.
pub type AcpPermissionHandler =
    Arc<dyn Fn(Map<String, Value>) -> BoxFuture<'static, anyhow::Result<Value>> + Send + Sync>;

/// Baseline ACP client with automatic permission policy.
pub struct AcpClient {
    transport: Arc<JsonRpcLineTransport>,
    permission: AcpPermissionHandler,
    observer: Mutex<Option<AcpUpdateObserver>>,
}

impl AcpClient {
    /// Creates a client over erased caller-owned streams.
    #[must_use]
    pub fn from_boxed(
        input: BoxedJsonRpcInput,
        output: BoxedJsonRpcOutput,
        permission: PermissionPolicy,
    ) -> Arc<Self> {
        let transport = JsonRpcLineTransport::from_boxed(input, output);
        Self::new(&transport, permission)
    }

    /// Creates and wires one client role; call [`Self::start`] after handlers are installed.
    #[must_use]
    pub fn new(transport: &Arc<JsonRpcLineTransport>, permission: PermissionPolicy) -> Arc<Self> {
        Self::new_with_permission_handler(transport, permission_handler(permission))
    }

    /// Creates a client with a caller-owned asynchronous permission policy.
    #[must_use]
    pub fn new_with_permission_handler(
        transport: &Arc<JsonRpcLineTransport>,
        permission: AcpPermissionHandler,
    ) -> Arc<Self> {
        let client = Arc::new(Self {
            transport: Arc::clone(transport),
            permission,
            observer: Mutex::new(None),
        });
        let weak: Weak<Self> = Arc::downgrade(&client);
        transport.on_notification(Arc::new(move |method, params| {
            if method != client_methods::SESSION_UPDATE {
                return;
            }
            if let Some(client) = weak.upgrade() {
                client.observe_update(&params);
            }
        }));
        let weak = Arc::downgrade(&client);
        transport.on_request(Arc::new(move |method, params| {
            let weak = weak.clone();
            Box::pin(async move {
                let Some(client) = weak.upgrade() else {
                    anyhow::bail!("ACP client is closed");
                };
                client.handle_request(&method, params).await
            })
        }));
        client
    }

    /// Starts consuming transport frames.
    pub fn start(&self) {
        self.transport.start();
    }

    /// Installs or replaces the session-update observer.
    pub fn on_update(&self, observer: AcpUpdateObserver) {
        *self.observer.lock() = Some(observer);
    }

    /// Negotiates the pinned version and baseline capabilities.
    ///
    /// # Errors
    ///
    /// Returns transport or remote protocol failures.
    pub async fn initialize(&self) -> anyhow::Result<Value> {
        self.transport
            .request(
                agent_methods::INITIALIZE,
                Map::from_iter([
                    ("protocolVersion".to_owned(), Value::from(PROTOCOL_VERSION)),
                    ("clientCapabilities".to_owned(), json!({})),
                ]),
                None,
            )
            .await
    }

    /// Creates one fresh remote session.
    ///
    /// # Errors
    ///
    /// Returns transport, remote validation, or missing-session-id failures.
    pub async fn new_session(&self, cwd: &str) -> anyhow::Result<AcpSessionId> {
        self.new_session_with_additional_directories(cwd, None)
            .await
    }

    /// Creates one remote Session with an explicitly widened workspace scope.
    ///
    /// # Errors
    ///
    /// Returns transport, remote validation, or missing-session-id failures.
    pub async fn new_session_with_additional_directories(
        &self,
        cwd: &str,
        additional_directories: Option<&[String]>,
    ) -> anyhow::Result<AcpSessionId> {
        let mut params = Map::from_iter([
            ("cwd".to_owned(), Value::String(cwd.to_owned())),
            ("mcpServers".to_owned(), json!([])),
        ]);
        if let Some(additional_directories) = additional_directories {
            params.insert(
                "additionalDirectories".to_owned(),
                Value::Array(
                    additional_directories
                        .iter()
                        .cloned()
                        .map(Value::String)
                        .collect(),
                ),
            );
        }
        let value = self
            .transport
            .request(agent_methods::SESSION_NEW, params, None)
            .await?;
        value
            .get("sessionId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(AcpSessionId::new)
            .ok_or_else(|| anyhow::anyhow!("ACP child published without a session id"))
    }

    /// Runs one prompt and returns its terminal reason.
    ///
    /// # Errors
    ///
    /// Returns transport, remote prompt, or malformed-response failures.
    pub async fn prompt(
        &self,
        session_id: &AcpSessionId,
        prompt: Vec<Value>,
    ) -> anyhow::Result<AcpStopReason> {
        let value = self
            .transport
            .request(
                agent_methods::SESSION_PROMPT,
                Map::from_iter([
                    (
                        "sessionId".to_owned(),
                        Value::String(session_id.as_str().to_owned()),
                    ),
                    ("prompt".to_owned(), Value::Array(prompt)),
                ]),
                None,
            )
            .await?;
        value
            .get("stopReason")
            .and_then(Value::as_str)
            .map(AcpStopReason::parse)
            .ok_or_else(|| anyhow::anyhow!("ACP prompt response omitted stopReason"))
    }

    /// Sends best-effort session cancellation.
    ///
    /// # Errors
    ///
    /// Returns an output serialization or transport failure.
    pub async fn cancel(&self, session_id: &AcpSessionId) -> anyhow::Result<()> {
        self.transport
            .notify(
                agent_methods::SESSION_CANCEL,
                Some(Map::from_iter([(
                    "sessionId".to_owned(),
                    Value::String(session_id.as_str().to_owned()),
                )])),
            )
            .await
    }

    /// Closes the local protocol role.
    pub fn close(&self) {
        self.transport.close();
    }

    /// Delivers EOF to the remote protocol reader.
    ///
    /// # Errors
    ///
    /// Returns the underlying output shutdown failure.
    pub async fn shutdown_output(&self) -> anyhow::Result<()> {
        self.transport.shutdown_output().await
    }

    fn observe_update(&self, params: &Map<String, Value>) {
        let Some(session_id) = params.get("sessionId").and_then(Value::as_str) else {
            return;
        };
        let update = params.get("update").cloned().unwrap_or(Value::Null);
        if let Some(observer) = self.observer.lock().clone() {
            observer(&AcpSessionUpdate {
                session_id: AcpSessionId::new(session_id),
                update,
            });
        }
    }

    async fn handle_request(
        &self,
        method: &str,
        params: Map<String, Value>,
    ) -> anyhow::Result<Value> {
        if method != client_methods::SESSION_REQUEST_PERMISSION {
            anyhow::bail!("method not found: {method}");
        }
        (self.permission)(params).await
    }
}

fn permission_handler(permission: PermissionPolicy) -> AcpPermissionHandler {
    Arc::new(move |params| {
        Box::pin(async move {
            let options = params
                .get("options")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let selected = match permission {
                PermissionPolicy::Reject => None,
                PermissionPolicy::Allow => options.iter().find(|option| {
                    matches!(
                        option.get("kind").and_then(Value::as_str),
                        Some("allow_once" | "allow_always")
                    )
                }),
            };
            Ok(selected
                .and_then(|option| option.get("optionId").and_then(Value::as_str))
                .map_or_else(
                    || json!({"outcome":{"outcome":"cancelled"}}),
                    |option| json!({"outcome":{"outcome":"selected","optionId":option}}),
                ))
        })
    })
}
