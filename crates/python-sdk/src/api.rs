//! Lazy high-level harness and receipt-to-idle turn ownership.

use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use parking_lot::Mutex;
use seekdeep_identity::{MessageId, SessionId};
use serde_json::{Map, Value};

use crate::{
    Client, HarnessConfig, HarnessOptions, Host, IdSource, NotificationObserver, RequestOptions,
    Result, RunResult, final_response, finish_reason, is_inbox_receipt, normalize_input,
    runtime::resolve_path,
};

/// Validates the high-level client's initialization model.
pub type InitializeValidator = Arc<dyn Fn(Map<String, Value>) -> Result<()> + Send + Sync>;
/// Validates and extracts the prompt response's messageId.
pub type PromptValidator = Arc<dyn Fn(Map<String, Value>) -> Result<MessageId> + Send + Sync>;

/// Reusable synchronous harness with a separately owned low-level client.
pub struct Harness {
    config: Mutex<HarnessOptions>,
    client: Arc<Client>,
    cwd: String,
    ids: Arc<dyn IdSource>,
    initialized: AtomicBool,
    startup: Mutex<()>,
    initialize_validator: InitializeValidator,
    prompt_validator: PromptValidator,
}

impl Harness {
    /// Resolves workspace paths and captures launch overrides without spawning.
    ///
    /// # Errors
    /// Propagates workspace/runtime path-resolution errors.
    pub fn new(
        config: HarnessOptions,
        host: Host,
        ids: Arc<dyn IdSource>,
        initialize_validator: InitializeValidator,
        prompt_validator: PromptValidator,
    ) -> Result<Arc<Self>> {
        let current = (host.cwd)()?;
        let cwd = config
            .cwd
            .as_deref()
            .filter(|cwd| !cwd.is_empty())
            .map_or_else(
                || Ok(current.clone()),
                |cwd| resolve_path(Path::new(cwd), &current),
            )?;
        let runtime_cwd = config.runtime_cwd.as_deref().map_or_else(
            || Ok(cwd.clone()),
            |cwd| resolve_path(Path::new(cwd), &current),
        )?;
        let cwd = cwd.to_string_lossy().into_owned();
        let mut environment = config.env.clone();
        for (name, value) in [
            ("SEEKDEEP_SESSION_ROOT", &config.session_root),
            ("SEEKDEEP_CORDIS_CONFIG", &config.cordis),
            ("DEEPSEEK_BASE_URL", &config.base_url),
            ("DEEPSEEK_API_KEY", &config.api_key),
        ] {
            if let Some(value) = value {
                environment.insert(name.to_owned(), value.clone());
            }
        }
        environment.insert("SEEKDEEP_CWD".to_owned(), cwd.clone());
        let client = Client::new(
            HarnessConfig {
                runtime_bin: config.runtime_bin.clone(),
                bridge_bin: None,
                launch_args_override: config.launch_args_override.clone(),
                cwd: Some(runtime_cwd.to_string_lossy().into_owned()),
                env: Some(environment),
                request_timeout_seconds: config.request_timeout_seconds,
                shutdown_timeout_seconds: config.shutdown_timeout_seconds,
            },
            host,
            Arc::clone(&ids),
        );
        Ok(Arc::new(Self {
            config: Mutex::new(config),
            client,
            cwd,
            ids,
            initialized: AtomicBool::new(false),
            startup: Mutex::new(()),
            initialize_validator,
            prompt_validator,
        }))
    }

    /// Current high-level options; changing them does not rebuild captured launch configuration.
    pub fn config(&self) -> HarnessOptions {
        self.config.lock().clone()
    }

    /// Updates options read during initialization and result construction.
    pub fn set_config(&self, config: HarnessOptions) {
        *self.config.lock() = config;
    }

    /// The same low-level client for this harness's complete lifetime.
    pub fn client(&self) -> &Arc<Client> {
        &self.client
    }

    /// The normalized workspace path sent during initialization.
    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    /// Starts and initializes at most once between closes.
    ///
    /// # Errors
    /// Propagates startup and initialization failures; failed initialization reaps the child.
    pub fn start(&self) -> Result<()> {
        let _startup = self.startup.lock();
        if self.initialized.load(Ordering::Acquire) {
            return Ok(());
        }
        self.client.start()?;
        let config = self.config();
        self.client.initialize(
            &self.cwd,
            &config.provider,
            &config.model,
            config.max_tokens,
            |response| (self.initialize_validator)(response),
        )?;
        self.initialized.store(true, Ordering::Release);
        Ok(())
    }

    /// Closes the client and permits a later lazy restart.
    ///
    /// # Errors
    /// Propagates low-level close failures without claiming initialization was reset.
    pub fn close(&self) -> Result<()> {
        let _startup = self.startup.lock();
        self.client.close()?;
        self.initialized.store(false, Ordering::Release);
        Ok(())
    }

    /// Initializes the harness and chooses an explicit or generated session identity.
    ///
    /// # Errors
    /// Propagates startup failures.
    pub fn start_session(&self, session: Option<SessionId>) -> Result<SessionId> {
        self.start()?;
        Ok(session
            .filter(|id| !id.as_str().is_empty())
            .unwrap_or_else(|| {
                SessionId::new(format!("session-{}", self.ids.next_uuid().simple()))
            }))
    }

    /// Runs through a lazily started session.
    ///
    /// # Errors
    /// Propagates startup, request, observer, transport, and malformed turn-end failures.
    pub fn run(
        &self,
        input: Value,
        session: Option<SessionId>,
        observer: Option<&NotificationObserver>,
    ) -> Result<RunResult> {
        let session = self.start_session(session)?;
        self.run_session(&session, input, observer)
    }

    /// Runs an already selected session without implicitly restarting a closed harness.
    ///
    /// # Errors
    /// Propagates request, observer, transport, and malformed turn-end failures.
    pub fn run_session(
        &self,
        session: &SessionId,
        input: Value,
        observer: Option<&NotificationObserver>,
    ) -> Result<RunResult> {
        let subscription = self.client.subscribe_session(session.clone());
        let result = (|| {
            let message = self.client.session_prompt_with(
                session,
                normalize_input(input),
                RequestOptions {
                    notification_subscription: Some(Arc::clone(&subscription)),
                    ..RequestOptions::default()
                },
                |response| (self.prompt_validator)(response),
            )?;
            let mut received = false;
            let mut notifications = Vec::new();
            let mut events = Vec::new();
            loop {
                let notification = subscription.next()?;
                if !received {
                    if !is_inbox_receipt(&notification, session, &message) {
                        continue;
                    }
                    received = true;
                }
                notifications.push(notification.clone());
                if let Some(observer) = &observer {
                    observer(&notification)?;
                }
                let root = notification
                    .payload
                    .get("sessionId")
                    .and_then(Value::as_str)
                    == Some(session.as_str());
                if notification.method == "session.event"
                    && root
                    && let Some(event) =
                        notification.payload.get("event").and_then(Value::as_object)
                {
                    events.push(event.clone());
                }
                if notification.method == "session.status"
                    && root
                    && notification.payload.get("status").and_then(Value::as_str) == Some("idle")
                {
                    break;
                }
            }
            Ok(RunResult {
                session_id: session.clone(),
                final_response: final_response(&events),
                finish_reason: finish_reason(&events)?,
                events,
                notifications,
                session_root: self.config().session_root,
            })
        })();
        subscription.close();
        result
    }
}
