//! High-level reusable runtime and per-session activity API.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use parking_lot::Mutex;
use path_clean::PathClean as _;
use seekdeep_core::session::{SessionEvent, SessionId};
use seekdeep_llm::ContentBlock;
use seekdeep_sdk_protocol::InitializeParams;
use serde_json::Value;
use tokio::sync::Notify;

use crate::{
    HarnessClient, HarnessNotification, RunResult, SdkProtocolError, TransportClosedError,
    types::DeepSeekHarnessOptions,
};

static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

#[derive(Default)]
struct StartSlot {
    result: Mutex<Option<Result<(), SharedStartError>>>,
    notify: Notify,
}

#[derive(Clone, Debug)]
enum SharedStartError {
    Transport(TransportClosedError),
    Response(seekdeep_sdk_protocol::JsonRpcResponseError),
    Protocol(SdkProtocolError),
    Other(String),
}

impl SharedStartError {
    fn capture(error: &anyhow::Error) -> Self {
        if let Some(error) = error.downcast_ref::<TransportClosedError>() {
            Self::Transport(error.clone())
        } else if let Some(error) =
            error.downcast_ref::<seekdeep_sdk_protocol::JsonRpcResponseError>()
        {
            Self::Response(error.clone())
        } else if let Some(error) = error.downcast_ref::<SdkProtocolError>() {
            Self::Protocol(error.clone())
        } else {
            Self::Other(error.to_string())
        }
    }

    fn to_error(&self) -> anyhow::Error {
        match self {
            Self::Transport(error) => anyhow::Error::new(error.clone()),
            Self::Response(error) => anyhow::Error::new(error.clone()),
            Self::Protocol(error) => anyhow::Error::new(error.clone()),
            Self::Other(error) => anyhow::Error::msg(error.clone()),
        }
    }
}

struct HarnessState {
    client: Arc<HarnessClient>,
    start: Option<Arc<StartSlot>>,
}

/// Reusable high-level SDK owning one runtime subprocess.
pub struct DeepSeekHarness {
    options: DeepSeekHarnessOptions,
    cwd: String,
    provider: String,
    model: String,
    state: Mutex<HarnessState>,
    closed: AtomicBool,
}

impl DeepSeekHarness {
    /// Constructs a lazy high-level runtime client.
    ///
    /// # Errors
    ///
    /// Returns current-directory resolution failures.
    pub fn new(options: DeepSeekHarnessOptions) -> anyhow::Result<Arc<Self>> {
        let cwd = options
            .cwd
            .clone()
            .or_else(|| options.launch.cwd.clone())
            .map_or(std::env::current_dir()?, Into::into);
        let cwd = if cwd.is_absolute() {
            cwd
        } else {
            std::env::current_dir()?.join(cwd)
        }
        .clean()
        .to_string_lossy()
        .into_owned();
        Ok(Arc::new(Self {
            provider: options
                .provider
                .clone()
                .unwrap_or_else(|| "deepseek-official".to_owned()),
            model: options
                .model
                .clone()
                .unwrap_or_else(|| "deepseek-v4-flash".to_owned()),
            state: Mutex::new(HarnessState {
                client: HarnessClient::new(options.launch.clone()),
                start: None,
            }),
            cwd,
            options,
            closed: AtomicBool::new(false),
        }))
    }

    /// Current low-level client; do not cache it across a failed start.
    #[must_use]
    pub fn client(&self) -> Arc<HarnessClient> {
        Arc::clone(&self.state.lock().client)
    }

    /// Starts and initializes once; a failed handshake reaps and resets the client.
    ///
    /// # Errors
    ///
    /// Returns terminal-close, spawn, request, or handshake failures.
    pub async fn start(self: &Arc<Self>) -> anyhow::Result<()> {
        if self.closed.load(Ordering::Acquire) {
            return Err(anyhow::Error::new(crate::TransportClosedError {
                message: "SeekDeep Harness is closed".to_owned(),
            }));
        }
        let (slot, creator, client) = {
            let mut state = self.state.lock();
            if let Some(slot) = &state.start {
                (Arc::clone(slot), false, Arc::clone(&state.client))
            } else {
                let slot = Arc::new(StartSlot::default());
                state.start = Some(Arc::clone(&slot));
                (slot, true, Arc::clone(&state.client))
            }
        };
        if creator {
            let result = async {
                client.start().await?;
                client
                    .initialize(InitializeParams {
                        cwd: self.cwd.clone(),
                        provider: seekdeep_llm::ProviderId::new(self.provider.clone()),
                        model: seekdeep_llm::ModelId::new(self.model.clone()),
                        max_tokens: self.options.max_tokens,
                    })
                    .await?;
                Ok::<(), anyhow::Error>(())
            }
            .await;
            if result.is_err() {
                let _ = client.close().await;
                let mut state = self.state.lock();
                state.start = None;
                if !self.closed.load(Ordering::Acquire) {
                    state.client = HarnessClient::new(self.options.launch.clone());
                }
            }
            *slot.result.lock() = Some(result.as_ref().map_err(SharedStartError::capture).copied());
            slot.notify.notify_waiters();
        }
        loop {
            let notified = slot.notify.notified();
            if let Some(result) = slot.result.lock().clone() {
                return result.map_err(|error| error.to_error());
            }
            notified.await;
        }
    }

    /// Opens a session handle; omitted identity mints a process-local id.
    #[must_use]
    pub fn session(self: &Arc<Self>, id: Option<SessionId>) -> HarnessSession {
        HarnessSession {
            harness: Arc::clone(self),
            id: id.unwrap_or_else(|| {
                SessionId::new(format!(
                    "session-{:016x}",
                    NEXT_SESSION.fetch_add(1, Ordering::AcqRel)
                ))
            }),
        }
    }

    /// Runs one prompt on a named or fresh session.
    ///
    /// # Errors
    ///
    /// Returns start, request, notification, or protocol failures.
    pub async fn run(
        self: &Arc<Self>,
        input: impl Into<RunInput>,
        options: RunOptions,
    ) -> anyhow::Result<RunResult> {
        self.session(options.session_id.clone())
            .run(input, options)
            .await
    }

    /// Shuts down and reaps the runtime. Idempotent and terminal.
    ///
    /// # Errors
    ///
    /// Returns process teardown failures.
    pub async fn close(self: &Arc<Self>) -> anyhow::Result<()> {
        self.closed.store(true, Ordering::Release);
        self.client().close().await
    }
}

/// Prompt text or verbatim content blocks.
pub enum RunInput {
    /// One text block.
    Text(String),
    /// Verbatim blocks.
    Blocks(Vec<ContentBlock>),
}

impl From<String> for RunInput {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<&str> for RunInput {
    fn from(value: &str) -> Self {
        Self::Text(value.to_owned())
    }
}

impl From<Vec<ContentBlock>> for RunInput {
    fn from(value: Vec<ContentBlock>) -> Self {
        Self::Blocks(value)
    }
}

/// Per-run target and notification observer.
#[derive(Clone, Default)]
pub struct RunOptions {
    /// Explicit session id.
    pub session_id: Option<SessionId>,
    /// Per-notification observer.
    pub on_notification: Option<NotificationObserver>,
}

/// Observer invoked for notifications in one owned run interval.
pub type NotificationObserver = Arc<dyn Fn(&HarnessNotification) + Send + Sync>;

/// One SDK session handle.
pub struct HarnessSession {
    /// Owning harness.
    pub harness: Arc<DeepSeekHarness>,
    /// Stable wire session id.
    pub id: SessionId,
}

impl HarnessSession {
    /// Queues a prompt and observes activity through the next idle state.
    ///
    /// # Errors
    ///
    /// Returns start, prompt, notification, or malformed-event failures.
    pub async fn run(
        &self,
        input: impl Into<RunInput>,
        options: RunOptions,
    ) -> anyhow::Result<RunResult> {
        self.harness.start().await?;
        let client = self.harness.client();
        let subscription = client.subscribe_session_tree(&self.id);
        let content = normalize_input(input.into());
        let message_id = client.prompt(self.id.clone(), content).await?;
        let mut events = Vec::new();
        let mut notifications = Vec::new();
        let mut received = false;
        loop {
            let notification = subscription.next().await?;
            if !received {
                if notification.method != "session.event"
                    || notification.params.get("sessionId").and_then(Value::as_str)
                        != Some(self.id.as_str())
                    || !is_inbox_receipt(
                        notification.params.get("event").unwrap_or(&Value::Null),
                        message_id.as_str(),
                    )
                {
                    continue;
                }
                received = true;
            }
            if notification.method == "session.event"
                && notification.params.get("sessionId").and_then(Value::as_str)
                    == Some(self.id.as_str())
            {
                let event = validated_session_event(
                    notification.params.get("event").unwrap_or(&Value::Null),
                )?;
                events.push(event);
            }
            if let Some(observer) = &options.on_notification {
                observer(&notification);
            }
            let idle = notification.method == "session.status"
                && notification.params.get("sessionId").and_then(Value::as_str)
                    == Some(self.id.as_str())
                && notification.params.get("status") == Some(&Value::String("idle".to_owned()));
            notifications.push(notification);
            if idle {
                break;
            }
        }
        subscription.close();
        Ok(RunResult {
            session_id: self.id.clone(),
            final_response: final_response(&events),
            events,
            notifications,
        })
    }
}

fn normalize_input(input: RunInput) -> Vec<ContentBlock> {
    match input {
        RunInput::Text(text) => vec![ContentBlock::Text { text }],
        RunInput::Blocks(blocks) => blocks,
    }
}

fn final_response(events: &[SessionEvent]) -> String {
    events
        .iter()
        .rev()
        .find(|event| event.event_type == "assistant/message")
        .and_then(|event| event.data.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|block| {
            (block.get("type")?.as_str()? == "text")
                .then(|| block.get("text")?.as_str().map(str::to_owned))?
        })
        .collect()
}

fn is_inbox_receipt(value: &Value, message_id: &str) -> bool {
    value.get("type").and_then(Value::as_str) == Some("agent/inbox/spliced")
        && value
            .get("data")
            .and_then(|data| data.get("inserted"))
            .and_then(Value::as_array)
            .is_some_and(|messages| {
                messages
                    .iter()
                    .any(|message| message.get("id").and_then(Value::as_str) == Some(message_id))
            })
}

fn validated_session_event(value: &Value) -> anyhow::Result<SessionEvent> {
    let valid_envelope = value
        .as_object()
        .and_then(|event| event.get("type"))
        .is_some_and(Value::is_string);
    if !valid_envelope {
        return Err(anyhow::Error::new(crate::SdkProtocolError {
            message: format!("session.event carried no event envelope: {value}"),
        }));
    }
    if value.get("type").and_then(Value::as_str) == Some("assistant/message") {
        let content = value
            .get("data")
            .and_then(Value::as_object)
            .and_then(|data| data.get("message"))
            .and_then(Value::as_object)
            .and_then(|message| message.get("content"))
            .and_then(Value::as_array);
        let valid_content = content.is_some_and(|content| {
            content.iter().all(|block| {
                block
                    .as_object()
                    .and_then(|block| block.get("type"))
                    .is_some_and(Value::is_string)
            })
        });
        if !valid_content {
            return Err(anyhow::Error::new(crate::SdkProtocolError {
                message: format!("assistant/message event carried malformed content: {value}"),
            }));
        }
    }
    serde_json::from_value(value.clone()).map_err(|_| {
        anyhow::Error::new(crate::SdkProtocolError {
            message: format!("session.event carried no event envelope: {value}"),
        })
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn input_and_final_response_helpers_preserve_exact_rules() {
        assert_eq!(
            normalize_input("hello".into()),
            [ContentBlock::Text {
                text: "hello".to_owned()
            }]
        );
        let blocks = vec![ContentBlock::Text { text: "x".into() }];
        assert_eq!(normalize_input(blocks.clone().into()), blocks);
        let events = vec![
            SessionEvent {
                event_type: "assistant/message".to_owned(),
                seq: 1,
                time: 1,
                data: json!({"message":{"content":[{"type":"text","text":"first"}]}}),
                source_event_seqs: None,
                surface_op: None,
                ignorable: None,
            },
            SessionEvent {
                event_type: "assistant/message".to_owned(),
                seq: 2,
                time: 2,
                data: json!({"message":{"content":[{"type":"reasoning","text":"hidden"},{"type":"text","text":"last"}]}}),
                source_event_seqs: None,
                surface_op: None,
                ignorable: None,
            },
        ];
        assert_eq!(final_response(&events), "last");
    }
}
