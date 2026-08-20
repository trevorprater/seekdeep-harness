//! Log-backed session title service: deterministic fallback, provider
//! contract, and automatic generation.

use std::{
    collections::HashMap,
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use parking_lot::Mutex;
use seekdeep_cordis::{
    Context, EventOptions, EventReply, FiberState, Plugin, ServiceKey, fiber::EffectHandle,
};
use seekdeep_core::session::{AppendOptions, Session, SessionEvent};
use seekdeep_core::session_store::SESSIONS;
use seekdeep_llm::{AbortSignal, GenerateOptions, LLM, is_agent_loop_request};
use seekdeep_session_projection::{
    ProjectionDefinition, ProjectionTransition, SESSION_PROJECTIONS,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Notify;

use crate::model::{
    Config, SessionTitleAutomaticMode, SessionTitleModelProvenance, SessionTitleProviderId,
    SessionTitleSnapshot, SessionTitleSource, SessionTitleUserMessage, fold_session_title,
};
use crate::normalize::{fallback_session_title, normalize_session_title};

/// Typed Cordis slot for the session-title service.
pub const SESSION_TITLE: ServiceKey<SessionTitleService> = ServiceKey::new("sessionTitle");

/// Services required by the session-title service.
pub const INJECT: &[&str] = &["sessions"];

/// Rejection of an explicit user title whose text normalizes to empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionTitleInvalidError;

impl std::fmt::Display for SessionTitleInvalidError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("session title must contain visible characters")
    }
}

impl std::error::Error for SessionTitleInvalidError {}

/// Immutable input supplied to one title-provider call.
#[derive(Clone, Debug)]
pub struct SessionTitleProviderRequest {
    /// Live session being titled.
    pub session: Arc<Session>,
    /// All eligible human messages through this generation revision.
    pub messages: Vec<SessionTitleUserMessage>,
    /// Exact current logged main-request route, when one has been recorded.
    pub route: Option<SessionTitleModelProvenance>,
    /// Cancellation for supersession, disposal, or the explicit caller.
    pub signal: AbortSignal,
}

/// Provider output before service-owned normalization and log acceptance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTitleProviderResult {
    /// Proposed title text.
    pub title: String,
    /// Exact seqs from the request messages used by this result.
    pub message_seqs: Vec<u64>,
    /// Auxiliary LLM route, when generation used a model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<SessionTitleModelProvenance>,
}

/// One optional asynchronous title implementation registered with the service.
#[async_trait]
pub trait SessionTitleProvider: Send + Sync {
    /// Stable id of the provider recorded with the title.
    fn id(&self) -> SessionTitleProviderId;
    /// When new human prompts start automatic generation.
    fn automatic(&self) -> SessionTitleAutomaticMode;
    /// Produce one title revision.
    ///
    /// # Errors
    ///
    /// Returns the provider's own generation failure.
    async fn generate(
        &self,
        request: SessionTitleProviderRequest,
    ) -> anyhow::Result<SessionTitleProviderResult>;
}

/// Collects human text-bearing user messages in log order.
#[must_use]
pub fn collect_session_title_messages(
    events: &[SessionEvent],
    through_seq: Option<u64>,
) -> Vec<SessionTitleUserMessage> {
    let mut messages = Vec::new();
    for event in events {
        if through_seq.is_some_and(|seq| event.seq > seq) {
            break;
        }
        if event.event_type != "user/message" {
            continue;
        }
        let source_kind = event
            .data
            .get("source")
            .and_then(|source| source.get("kind"))
            .and_then(Value::as_str);
        if source_kind != Some("user") {
            continue;
        }
        let Some(content) = event.data.get("content").and_then(Value::as_array) else {
            continue;
        };
        let text = content
            .iter()
            .filter_map(|block| {
                if block.get("type").and_then(Value::as_str) == Some("text") {
                    block.get("text").and_then(Value::as_str)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join(
                "
",
            );
        if normalize_session_title(&text, usize::MAX).is_empty() {
            continue;
        }
        messages.push(SessionTitleUserMessage {
            seq: event.seq,
            text,
        });
    }
    messages
}

/// Tracks in-flight work for lifecycle teardown.
struct WorkTracker {
    count: AtomicUsize,
    notify: Notify,
}

impl WorkTracker {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            count: AtomicUsize::new(0),
            notify: Notify::new(),
        })
    }

    fn guard(self: &Arc<Self>) -> WorkGuard {
        self.count.fetch_add(1, Ordering::AcqRel);
        WorkGuard {
            tracker: Arc::clone(self),
        }
    }

    async fn drain(&self) {
        loop {
            if self.count.load(Ordering::Acquire) == 0 {
                return;
            }
            self.notify.notified().await;
        }
    }
}

struct WorkGuard {
    tracker: Arc<WorkTracker>,
}

impl Drop for WorkGuard {
    fn drop(&mut self) {
        self.tracker.count.fetch_sub(1, Ordering::AcqRel);
        self.tracker.notify.notify_one();
    }
}

/// One exact provider registration generation.
struct ProviderRegistration {
    provider: Arc<dyn SessionTitleProvider>,
    active: Arc<WorkTracker>,
    closing: std::sync::atomic::AtomicBool,
}

impl ProviderRegistration {
    fn new(provider: Arc<dyn SessionTitleProvider>) -> Arc<Self> {
        Arc::new(Self {
            provider,
            active: WorkTracker::new(),
            closing: std::sync::atomic::AtomicBool::new(false),
        })
    }
}

/// Automatic work waiting for the matching main-request header.
#[derive(Clone)]
struct PendingAutomaticWork {
    registration: Arc<ProviderRegistration>,
    revision: u64,
    through_seq: u64,
}

/// Provider call currently allowed to commit for one session.
struct ActiveProviderWork {
    pending: PendingAutomaticWork,
    signal: AbortSignal,
}

/// Mutable concurrency state scoped to one live session.
struct SessionTitleWorkState {
    revision: u64,
    fallback: Option<AbortSignal>,
    pending: Option<PendingAutomaticWork>,
    active: Option<ActiveProviderWork>,
}

impl SessionTitleWorkState {
    fn new() -> Self {
        Self {
            revision: 0,
            fallback: None,
            pending: None,
            active: None,
        }
    }
}

/// Log-backed title fold plus asynchronous fallback generation.
pub struct SessionTitleService {
    context: Context,
    config: Config,
    lifetime: AbortSignal,
    registration: Mutex<Option<Arc<ProviderRegistration>>>,
    work: Mutex<HashMap<usize, Arc<Mutex<SessionTitleWorkState>>>>,
    in_flight: Arc<WorkTracker>,
}

impl SessionTitleService {
    /// Constructs the service and registers its listeners and projection.
    ///
    /// # Errors
    ///
    /// Returns config-validation, listener, or projection failures.
    pub fn new(context: &Context, config: Config) -> anyhow::Result<Arc<Self>> {
        validate_config(&config)?;
        let service = Arc::new(Self {
            context: context.clone(),
            config,
            lifetime: AbortSignal::default(),
            registration: Mutex::new(None),
            work: Mutex::new(HashMap::new()),
            in_flight: WorkTracker::new(),
        });
        service.register_listeners(context)?;
        Self::register_projection(context)?;
        service.register_stream_middleware(context)?;
        service.register_lifecycle(context)?;
        Ok(service)
    }

    /// Publishes this exact service on the session-title slot.
    ///
    /// # Errors
    ///
    /// Returns duplicate-service or inactive-owner failures.
    pub fn provide(
        self: &Arc<Self>,
        context: &Context,
    ) -> Result<EffectHandle, seekdeep_cordis::CordisError> {
        context.provide(SESSION_TITLE, self.clone())
    }

    /// Builds and publishes the session-title service.
    ///
    /// # Errors
    ///
    /// Returns config-validation, listener, projection, or provide failures.
    pub fn install(context: &Context, config: Config) -> anyhow::Result<Arc<Self>> {
        let service = Self::new(context, config)?;
        service.provide(context)?;
        Ok(service)
    }

    /// Reads the latest folded title from one live or replayed session.
    #[must_use]
    pub fn get(&self, session: &Arc<Session>) -> Option<SessionTitleSnapshot> {
        fold_session_title(&session.events())
    }

    /// Accepts an explicit user title, pinning the title against automatic generation.
    ///
    /// # Errors
    ///
    /// Returns an invalid-title, inactive-service, or non-live-session failure.
    pub fn rename(
        &self,
        session: &Arc<Session>,
        title: &str,
    ) -> anyhow::Result<SessionTitleSnapshot> {
        self.assert_service_active()?;
        self.assert_live(session)?;
        let normalized = normalize_session_title(title, self.max_title_bytes());
        if normalized.is_empty() {
            return Err(SessionTitleInvalidError.into());
        }
        let state = self.state_for(session);
        Self::supersede(&state, "user rename superseded automatic title generation");
        session.append(
            "session/title",
            json!({
                "title": normalized,
                "messageSeqs": [],
                "source": {"kind": "user"},
            }),
            AppendOptions::default(),
        )?;
        self.get(session)
            .ok_or_else(|| anyhow::anyhow!("renamed title failed to fold"))
    }

    /// Explicitly retries the registered provider, or materializes the fallback.
    ///
    /// # Errors
    ///
    /// Returns an inactive-service, non-live-session, or generation failure.
    pub async fn refresh(
        &self,
        session: &Arc<Session>,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<Option<SessionTitleSnapshot>> {
        Self::ensure_not_aborted(signal.as_ref())?;
        self.assert_service_active()?;
        self.assert_live(session)?;
        let registration = self.registration.lock().clone();
        let messages = collect_session_title_messages(&session.events(), None);
        let Some(latest) = messages.last() else {
            let fallback = self.ensure_fallback(session).await?;
            Self::ensure_not_aborted(signal.as_ref())?;
            return Ok(fallback);
        };
        let Some(registration) = registration.filter(|r| !r.closing.load(Ordering::Acquire)) else {
            let current = self.get(session);
            if let Some(first) = messages.first()
                && current
                    .as_ref()
                    .is_some_and(|c| matches!(c.event.source, SessionTitleSource::User))
            {
                self.append_fallback(session, first);
                Self::ensure_not_aborted(signal.as_ref())?;
                return Ok(self.get(session));
            }
            let fallback = self.ensure_fallback(session).await?;
            Self::ensure_not_aborted(signal.as_ref())?;
            return Ok(fallback);
        };
        let state = self.state_for(session);
        let revision =
            Self::supersede(&state, "explicit title refresh superseded older generation");
        let work = self.activate(
            &state,
            PendingAutomaticWork {
                registration: Arc::clone(&registration),
                revision,
                through_seq: latest.seq,
            },
            signal.as_ref(),
        );
        let route = session
            .request_header()
            .map(|header| SessionTitleModelProvenance {
                provider: header.config.provider.as_str().to_owned(),
                model: header.config.model.as_str().to_owned(),
            });
        self.start_provider(session, work, route).await
    }

    /// Registers the sole optional title provider.
    ///
    /// # Errors
    ///
    /// Returns an invalid-provider or duplicate-provider failure.
    pub fn register(
        self: &Arc<Self>,
        provider: Arc<dyn SessionTitleProvider>,
    ) -> anyhow::Result<EffectHandle> {
        Self::validate_provider(&provider)?;
        let mut slot = self.registration.lock();
        if let Some(existing) = slot.as_ref() {
            anyhow::bail!(
                "session-title provider \"{}\" is already registered",
                existing.provider.id()
            );
        }
        let registration = ProviderRegistration::new(provider);
        *slot = Some(Arc::clone(&registration));
        drop(slot);

        let service = Arc::clone(self);
        Ok(EffectHandle::new("sessionTitle.register()", move || {
            let service = Arc::clone(&service);
            let registration = Arc::clone(&registration);
            Box::pin(async move {
                registration.closing.store(true, Ordering::Release);
                for state in service.work.lock().values() {
                    let mut state = state.lock();
                    if state
                        .pending
                        .as_ref()
                        .is_some_and(|p| Arc::ptr_eq(&p.registration, &registration))
                    {
                        state.pending = None;
                    }
                    if let Some(active) = state.active.as_ref()
                        && Arc::ptr_eq(&active.pending.registration, &registration)
                    {
                        active.signal.abort_with_reason(json!({
                            "message": format!("session-title provider \"{}\" was disposed", registration.provider.id())
                        }));
                    }
                }
                registration.active.drain().await;
                let mut slot = service.registration.lock();
                if slot.as_ref().is_some_and(|r| Arc::ptr_eq(r, &registration)) {
                    *slot = None;
                }
                Ok(())
            })
        }))
    }

    fn register_lifecycle(self: &Arc<Self>, context: &Context) -> anyhow::Result<()> {
        let weak = Arc::downgrade(self);
        let effect = EffectHandle::new("sessionTitle lifecycle", move || {
            let weak = weak.clone();
            Box::pin(async move {
                if let Some(service) = weak.upgrade() {
                    service
                        .lifetime
                        .abort_with_reason(json!("session-title service disposed"));
                    if let Some(registration) = service.registration.lock().clone() {
                        registration.closing.store(true, Ordering::Release);
                    }
                    *service.registration.lock() = None;
                    for state in service.work.lock().values() {
                        let mut state = state.lock();
                        state.pending = None;
                        if let Some(active) = &state.active {
                            active
                                .signal
                                .abort_with_reason(json!("session-title service disposed"));
                        }
                    }
                    service.in_flight.drain().await;
                    service.work.lock().clear();
                }
                Ok(())
            })
        });
        context.own(effect)?;
        Ok(())
    }

    fn register_listeners(self: &Arc<Self>, context: &Context) -> anyhow::Result<()> {
        let service = Arc::clone(self);
        context.events().on_sync(
            context,
            "session/event",
            move |_, args| {
                let session = args
                    .get::<Session>(0)
                    .ok_or_else(|| anyhow::anyhow!("session/event lacks its session"))?;
                let event = args
                    .get::<SessionEvent>(1)
                    .ok_or_else(|| anyhow::anyhow!("session/event lacks its event"))?;
                let service = service.clone();
                match event.event_type.as_str() {
                    "user/message" => service.on_user_message(&session, event.as_ref()),
                    "request/header" => service.on_request_header(&session, event.as_ref()),
                    _ => {}
                }
                Ok(EventReply::Undefined)
            },
            global_events(),
        )?;

        let service = Arc::clone(self);
        context.events().on_sync(
            context,
            "session/disposed",
            move |_, args| {
                let session = args
                    .get::<Session>(0)
                    .ok_or_else(|| anyhow::anyhow!("session/disposed lacks its session"))?;
                let key = session_key(&session);
                let state = service.work.lock().remove(&key);
                if let Some(state) = state
                    && let Some(active) = &state.lock().active
                {
                    active
                        .signal
                        .abort_with_reason(json!("session disposed during title generation"));
                }
                Ok(EventReply::Undefined)
            },
            global_events(),
        )?;
        Ok(())
    }

    fn register_projection(context: &Context) -> anyhow::Result<()> {
        let Some(registry) = context.get(SESSION_PROJECTIONS) else {
            return Ok(());
        };
        let definition = ProjectionDefinition::new(
            "title",
            1,
            || Ok(Value::Null),
            |state: &Value, event: &SessionEvent| {
                if event.event_type == "session/title" {
                    let title = event.data.get("title").cloned().unwrap_or(Value::Null);
                    if title == *state {
                        Ok(ProjectionTransition::Unchanged)
                    } else {
                        Ok(ProjectionTransition::Changed(title))
                    }
                } else {
                    Ok(ProjectionTransition::Unchanged)
                }
            },
            |state: &Value| Ok(state.clone()),
        );
        registry.register(context, definition)?;
        Ok(())
    }

    fn register_stream_middleware(self: &Arc<Self>, context: &Context) -> anyhow::Result<()> {
        let Some(llm) = context.get(LLM) else {
            return Ok(());
        };
        let service = Arc::clone(self);
        llm.register_stream_middleware(
            context,
            Arc::new(move |options: GenerateOptions, next| {
                service.on_main_request(&options);
                next(options)
            }),
            true,
        )?;
        Ok(())
    }

    fn on_user_message(self: &Arc<Self>, session: &Arc<Session>, event: &SessionEvent) {
        if !self.service_active() {
            return;
        }
        let source_kind = event
            .data
            .get("source")
            .and_then(|source| source.get("kind"))
            .and_then(Value::as_str);
        if source_kind != Some("user")
            || collect_session_title_messages(std::slice::from_ref(event), None).is_empty()
        {
            return;
        }
        if self
            .get(session)
            .is_some_and(|c| matches!(c.event.source, SessionTitleSource::User))
        {
            return;
        }
        let registration = self.registration.lock().clone();
        if let Some(registration) = registration
            && !registration.closing.load(Ordering::Acquire)
        {
            let messages = collect_session_title_messages(&session.events(), Some(event.seq));
            let should_schedule = registration.provider.automatic()
                == SessionTitleAutomaticMode::AllPrompts
                || (session.header().parent_session.is_none()
                    && messages.len() == 1
                    && self.get(session).is_none());
            if should_schedule {
                let state = self.state_for(session);
                let revision =
                    Self::supersede(&state, "newer user message superseded title generation");
                state.lock().pending = Some(PendingAutomaticWork {
                    registration: Arc::clone(&registration),
                    revision,
                    through_seq: event.seq,
                });
            }
        }
        let service = Arc::clone(self);
        let session = Arc::clone(session);
        let inner = Arc::clone(&service);
        service.defer(async move {
            if let Err(error) = inner.ensure_fallback(&session).await
                && inner.service_active()
            {
                tracing::warn!(session = %session.id(), %error, "fallback title update failed");
            }
        });
    }

    fn on_request_header(self: &Arc<Self>, session: &Arc<Session>, event: &SessionEvent) {
        if !self.service_active() {
            return;
        }
        let key = session_key(session);
        let Some(state) = self.work.lock().get(&key).cloned() else {
            return;
        };
        let Some(pending) = state.lock().pending.clone() else {
            return;
        };
        if pending.through_seq >= event.seq {
            return;
        }
        let Some(header) = event.data.get("header").and_then(|h| h.get("config")) else {
            return;
        };
        let provider = header
            .get("provider")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let model = header
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        self.start_pending(
            session,
            &state,
            pending,
            SessionTitleModelProvenance { provider, model },
        );
    }

    fn on_main_request(self: &Arc<Self>, options: &GenerateOptions) {
        if !self.service_active() || options.session_id.is_none() || !is_agent_loop_request(options)
        {
            return;
        }
        let session_id = options.session_id.as_ref().expect("checked above");
        let Some(session) = self
            .context
            .get(SESSIONS)
            .and_then(|store| store.get(session_id))
        else {
            return;
        };
        let key = session_key(&session);
        let Some(state) = self.work.lock().get(&key).cloned() else {
            return;
        };
        let Some(pending) = state.lock().pending.clone() else {
            return;
        };
        let events = session.events();
        let boundary = events
            .iter()
            .rev()
            .find(|e| e.event_type == "step/start" || e.event_type == "step/end");
        let route = session
            .request_header()
            .map(|h| SessionTitleModelProvenance {
                provider: h.config.provider.as_str().to_owned(),
                model: h.config.model.as_str().to_owned(),
            });
        let boundary_ok =
            boundary.is_some_and(|b| b.event_type == "step/start" && b.seq > pending.through_seq);
        if !boundary_ok
            || route
                .as_ref()
                .is_none_or(|r| r.provider != options.provider.as_str())
            || route
                .as_ref()
                .is_none_or(|r| r.model != options.model.as_str())
        {
            return;
        }
        self.start_pending(
            &session,
            &state,
            pending,
            SessionTitleModelProvenance {
                provider: options.provider.as_str().to_owned(),
                model: options.model.as_str().to_owned(),
            },
        );
    }

    fn start_pending(
        self: &Arc<Self>,
        session: &Arc<Session>,
        state: &Arc<Mutex<SessionTitleWorkState>>,
        pending: PendingAutomaticWork,
        route: SessionTitleModelProvenance,
    ) {
        state.lock().pending = None;
        let service = Arc::clone(self);
        let session = Arc::clone(session);
        let state = Arc::clone(state);
        let inner = Arc::clone(&service);
        service.defer(async move {
            let current_registration = inner.registration.lock().clone();
            if current_registration.as_ref().is_none_or(|r| !Arc::ptr_eq(r, &pending.registration))
                || pending.registration.closing.load(Ordering::Acquire)
                || inner.work.lock().get(&session_key(&session)).is_none_or(|s| !Arc::ptr_eq(s, &state))
                || state.lock().revision != pending.revision
            {
                return;
            }
            let work = inner.activate(&state, pending, None);
            if let Err(error) = inner.start_provider(&session, work, Some(route)).await
                && inner.service_active()
            {
                tracing::warn!(session = %session.id(), %error, "automatic title generation failed");
            }
        });
    }

    async fn start_provider(
        &self,
        session: &Arc<Session>,
        work: ActiveProviderWork,
        route: Option<SessionTitleModelProvenance>,
    ) -> anyhow::Result<Option<SessionTitleSnapshot>> {
        let registration = Arc::clone(&work.pending.registration);
        let registration_guard = registration.active.guard();
        let in_flight_guard = self.in_flight.guard();
        let result = self.run_provider(session, &work, route).await;
        drop(registration_guard);
        drop(in_flight_guard);
        result
    }

    async fn run_provider(
        &self,
        session: &Arc<Session>,
        work: &ActiveProviderWork,
        route: Option<SessionTitleModelProvenance>,
    ) -> anyhow::Result<Option<SessionTitleSnapshot>> {
        let result = async {
            self.assert_current(session, work)?;
            self.ensure_fallback(session).await?;
            self.assert_current(session, work)?;
            let messages =
                collect_session_title_messages(&session.events(), Some(work.pending.through_seq));
            let result = work
                .pending
                .registration
                .provider
                .generate(SessionTitleProviderRequest {
                    session: Arc::clone(session),
                    messages: messages.clone(),
                    route: route.clone(),
                    signal: work.signal.clone(),
                })
                .await?;
            self.assert_current(session, work)?;
            let accepted = self.validate_result(&result, &messages)?;
            let mut source = json!({
                "kind": "provider",
                "provider": work.pending.registration.provider.id().as_str(),
            });
            if let Some(model) = &accepted.model {
                source["model"] = json!(model);
            }
            session.append(
                "session/title",
                json!({
                    "title": accepted.title,
                    "messageSeqs": accepted.message_seqs,
                    "source": source,
                }),
                AppendOptions::default(),
            )?;
            Ok(self.get(session))
        }
        .await;
        let key = session_key(session);
        if let Some(state) = self.work.lock().get(&key) {
            let mut state = state.lock();
            if state.active.as_ref().is_some_and(|a| {
                a.pending.through_seq == work.pending.through_seq
                    && a.pending.revision == work.pending.revision
                    && Arc::ptr_eq(&a.pending.registration, &work.pending.registration)
            }) {
                state.active = None;
            }
        }
        result
    }

    fn validate_result(
        &self,
        result: &SessionTitleProviderResult,
        messages: &[SessionTitleUserMessage],
    ) -> anyhow::Result<SessionTitleProviderResult> {
        let title = normalize_session_title(&result.title, self.max_title_bytes());
        if title.is_empty() {
            anyhow::bail!("session-title provider returned an empty title");
        }
        if result.message_seqs.is_empty() {
            anyhow::bail!("session-title provider must identify at least one source message seq");
        }
        let order: std::collections::HashMap<u64, usize> = messages
            .iter()
            .enumerate()
            .map(|(index, message)| (message.seq, index))
            .collect();
        let mut previous = None;
        let mut message_seqs = Vec::with_capacity(result.message_seqs.len());
        for seq in &result.message_seqs {
            let Some(index) = order.get(seq) else {
                anyhow::bail!(
                    "session-title provider messageSeqs must be unique, ordered seqs from the request"
                );
            };
            if previous.is_some_and(|prev| *index <= prev) {
                anyhow::bail!(
                    "session-title provider messageSeqs must be unique, ordered seqs from the request"
                );
            }
            message_seqs.push(*seq);
            previous = Some(*index);
        }
        Ok(SessionTitleProviderResult {
            title,
            message_seqs,
            model: result.model.clone(),
        })
    }

    fn assert_current(
        &self,
        session: &Arc<Session>,
        work: &ActiveProviderWork,
    ) -> anyhow::Result<()> {
        self.assert_service_active()?;
        if work.signal.is_aborted() {
            anyhow::bail!(
                work.signal
                    .reason()
                    .and_then(|r| r.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "aborted".to_owned())
            );
        }
        let state = self.work.lock().get(&session_key(session)).cloned();
        let current = self.registration.lock().clone();
        let active_matches = state.as_ref().is_some_and(|s| {
            s.lock().active.as_ref().is_some_and(|a| {
                a.pending.revision == work.pending.revision
                    && a.pending.through_seq == work.pending.through_seq
                    && Arc::ptr_eq(&a.pending.registration, &work.pending.registration)
            })
        });
        let registration_matches = current
            .as_ref()
            .is_some_and(|r| Arc::ptr_eq(r, &work.pending.registration));
        let live = self
            .context
            .get(SESSIONS)
            .and_then(|store| store.get(session.id()))
            .is_some_and(|live| Arc::ptr_eq(&live, session));
        if !registration_matches || !active_matches || !live {
            anyhow::bail!("session title generation state changed without cancellation");
        }
        Ok(())
    }

    fn activate(
        &self,
        state: &Arc<Mutex<SessionTitleWorkState>>,
        pending: PendingAutomaticWork,
        upstream: Option<&AbortSignal>,
    ) -> ActiveProviderWork {
        let controller = AbortSignal::default();
        let fused = AbortSignal::fuse(&controller, &self.lifetime);
        let signal = upstream.map_or_else(
            || fused.clone(),
            |upstream| AbortSignal::fuse(&fused, upstream),
        );
        let work = ActiveProviderWork { pending, signal };
        state.lock().active = Some(ActiveProviderWork {
            pending: work.pending.clone(),
            signal: work.signal.clone(),
        });
        work
    }

    fn supersede(state: &Arc<Mutex<SessionTitleWorkState>>, reason: &str) -> u64 {
        let mut state = state.lock();
        if let Some(active) = &state.active {
            active
                .signal
                .abort_with_reason(Value::String(reason.to_owned()));
        }
        state.pending = None;
        state.revision += 1;
        state.revision
    }

    fn state_for(&self, session: &Arc<Session>) -> Arc<Mutex<SessionTitleWorkState>> {
        let key = session_key(session);
        let mut work = self.work.lock();
        work.entry(key)
            .or_insert_with(|| Arc::new(Mutex::new(SessionTitleWorkState::new())))
            .clone()
    }

    fn defer(&self, task: impl Future<Output = ()> + Send + 'static) {
        let guard = self.in_flight.guard();
        tokio::spawn(async move {
            let _guard = guard;
            task.await;
        });
    }

    fn append_fallback(&self, session: &Arc<Session>, first: &SessionTitleUserMessage) {
        let title = fallback_session_title(
            &first.text,
            self.fallback_max_words(),
            self.fallback_max_bytes(),
        );
        if title.is_empty() {
            return;
        }
        let _ = session.append(
            "session/title",
            json!({
                "title": title,
                "messageSeqs": [first.seq],
                "source": {"kind": "fallback"},
            }),
            AppendOptions::default(),
        );
    }

    async fn ensure_fallback(
        &self,
        session: &Arc<Session>,
    ) -> anyhow::Result<Option<SessionTitleSnapshot>> {
        self.assert_service_active()?;
        if let Some(current) = self.get(session) {
            return Ok(Some(current));
        }
        let messages = collect_session_title_messages(&session.events(), None);
        let Some(first) = messages.first() else {
            return Ok(None);
        };
        let title = fallback_session_title(
            &first.text,
            self.fallback_max_words(),
            self.fallback_max_bytes(),
        );
        if title.is_empty() {
            return Ok(None);
        }
        let state = self.state_for(session);
        if state.lock().fallback.is_some() {
            return Ok(self.get(session));
        }
        let controller = AbortSignal::default();
        state.lock().fallback = Some(controller.clone());
        let result = async {
            self.assert_service_active()?;
            self.assert_live(session)?;
            if let Some(accepted) = self.get(session) {
                return Ok(Some(accepted));
            }
            session.append(
                "session/title",
                json!({
                    "title": title,
                    "messageSeqs": [first.seq],
                    "source": {"kind": "fallback"},
                }),
                AppendOptions::default(),
            )?;
            Ok(self.get(session))
        }
        .await;
        if let Some(state) = self.work.lock().get(&session_key(session)) {
            state.lock().fallback = None;
        }
        result
    }

    fn service_active(&self) -> bool {
        !self.lifetime.is_aborted() && matches!(self.context.fiber().state(), FiberState::Active)
    }

    fn assert_service_active(&self) -> anyhow::Result<()> {
        if !self.service_active() {
            anyhow::bail!("session-title service disposed");
        }
        Ok(())
    }

    fn ensure_not_aborted(signal: Option<&AbortSignal>) -> anyhow::Result<()> {
        if let Some(signal) = signal
            && signal.is_aborted()
        {
            let reason = signal
                .reason()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_else(|| "aborted".to_owned());
            anyhow::bail!("{reason}");
        }
        Ok(())
    }

    fn assert_live(&self, session: &Arc<Session>) -> anyhow::Result<()> {
        let live = self
            .context
            .get(SESSIONS)
            .and_then(|store| store.get(session.id()));
        if !live.as_ref().is_some_and(|live| Arc::ptr_eq(live, session)) {
            anyhow::bail!("session \"{}\" is not live in this store", session.id());
        }
        Ok(())
    }

    fn validate_provider(provider: &Arc<dyn SessionTitleProvider>) -> anyhow::Result<()> {
        if provider.id().as_str().is_empty() {
            anyhow::bail!("session-title provider id must be a non-empty string");
        }
        Ok(())
    }

    fn max_title_bytes(&self) -> usize {
        usize::try_from(self.config.max_title_bytes).unwrap_or(usize::MAX)
    }

    fn fallback_max_words(&self) -> usize {
        usize::try_from(self.config.fallback_max_words).unwrap_or(usize::MAX)
    }

    fn fallback_max_bytes(&self) -> usize {
        usize::try_from(self.config.fallback_max_bytes).unwrap_or(usize::MAX)
    }
}

fn validate_config(config: &Config) -> anyhow::Result<()> {
    for (name, value) in [
        ("fallbackMaxWords", config.fallback_max_words),
        ("fallbackMaxBytes", config.fallback_max_bytes),
        ("maxTitleBytes", config.max_title_bytes),
    ] {
        if value < 1 {
            anyhow::bail!("session-title: {name} must be a positive integer");
        }
    }
    if config.fallback_max_bytes > config.max_title_bytes {
        anyhow::bail!("session-title: fallbackMaxBytes must not exceed maxTitleBytes");
    }
    Ok(())
}

fn session_key(session: &Arc<Session>) -> usize {
    Arc::as_ptr(session) as usize
}

fn global_events() -> EventOptions {
    EventOptions {
        global: true,
        ..EventOptions::default()
    }
}

/// Builds the loader-compatible session-title plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(crate::NAME, INJECT.iter().copied(), |context, config| {
        Box::pin(async move {
            let config: Config = serde_json::from_value(config)?;
            SessionTitleService::install(&context, config)?;
            Ok(())
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_rejects_zero_and_oversized_fallback() {
        assert!(
            validate_config(&Config {
                fallback_max_words: 0,
                fallback_max_bytes: 40,
                max_title_bytes: 80,
            })
            .is_err()
        );
        assert!(
            validate_config(&Config {
                fallback_max_words: 5,
                fallback_max_bytes: 100,
                max_title_bytes: 80,
            })
            .is_err()
        );
        assert!(
            validate_config(&Config {
                fallback_max_words: 5,
                fallback_max_bytes: 40,
                max_title_bytes: 80,
            })
            .is_ok()
        );
    }

    #[test]
    fn collects_eligible_user_messages() {
        use seekdeep_core::session::SessionEvent;
        use serde_json::json;

        let user_message = |seq: u64, text: &str| SessionEvent {
            event_type: "user/message".to_owned(),
            seq,
            time: i64::try_from(seq).expect("seq"),
            data: json!({
                "source": {"kind": "user"},
                "content": [{"type": "text", "text": text}],
            }),
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        };
        let events = vec![user_message(0, "hello"), user_message(1, "world")];
        let messages = collect_session_title_messages(&events, None);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].seq, 0);
        assert_eq!(messages[0].text, "hello");
        let bounded = collect_session_title_messages(&events, Some(0));
        assert_eq!(bounded.len(), 1);
    }
}
