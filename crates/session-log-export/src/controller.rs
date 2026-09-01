//! Browser download state and one-flight-per-Session lifecycle.

use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::{Rc, Weak},
    task::Poll,
};

use futures::{
    FutureExt as _,
    future::{LocalBoxFuture, Shared, join_all},
};
use seekdeep_identity::SessionId;
use serde::{Deserialize, Serialize};
use url::Url;

/// Download phases presented by the shared modal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionLogDownloadStatus {
    /// HEAD preflight in progress.
    Downloading,
    /// Browser save started.
    Success,
    /// Preflight or transport failed.
    Error,
}

/// One Session's current download-dialog state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLogDownloadEntry {
    /// Whether the shared modal is visible.
    pub open: bool,
    /// Current phase.
    pub status: SessionLogDownloadStatus,
    /// User-visible transport detail on failure.
    pub error: Option<String>,
}

/// Download states keyed by Session id.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLogDownloadState {
    /// Per-session state.
    pub by_session: BTreeMap<String, SessionLogDownloadEntry>,
}

/// HEAD preflight request.
#[derive(Clone, Debug)]
pub struct DownloadRequest {
    /// Fully resolved same-origin URL.
    pub url: String,
    /// Per-download abort signal.
    pub signal: DownloadAbortSignal,
}

/// HEAD preflight response facts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadResponse {
    /// HTTP status.
    pub status: u16,
    /// Optional response text; a read failure is treated as no detail.
    pub detail: Result<String, String>,
}

impl DownloadResponse {
    fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// Browser/fixture HEAD carrier.
pub type DownloadFetcher =
    Rc<dyn Fn(DownloadRequest) -> LocalBoxFuture<'static, Result<DownloadResponse, String>>>;
/// Browser/fixture save operation.
pub type DownloadSaver = Rc<dyn Fn(&str, &str) -> Result<(), String>>;
/// Shared completion returned to concurrent gestures.
pub type DownloadFuture = Shared<LocalBoxFuture<'static, ()>>;

struct ActiveDownload {
    abort: DownloadAbortSignal,
    done: DownloadFuture,
}

/// Owns one in-flight browser download per Session and publishes modal state.
pub struct SessionLogDownloadController {
    state: RefCell<SessionLogDownloadState>,
    active: RefCell<BTreeMap<SessionId, ActiveDownload>>,
    disposed: Cell<bool>,
    listeners: RefCell<BTreeMap<u64, Rc<dyn Fn()>>>,
    next_listener_id: Cell<u64>,
    fetcher: DownloadFetcher,
    save: DownloadSaver,
    host_base: String,
}

/// One reversible snapshot listener registration.
pub struct SessionLogDownloadSubscription {
    controller: Weak<SessionLogDownloadController>,
    listener_id: u64,
    disposed: Cell<bool>,
}

impl SessionLogDownloadSubscription {
    /// Removes the listener once.
    pub fn dispose(&self) {
        if self.disposed.replace(true) {
            return;
        }
        if let Some(controller) = self.controller.upgrade() {
            controller.listeners.borrow_mut().remove(&self.listener_id);
        }
    }
}

impl Drop for SessionLogDownloadSubscription {
    fn drop(&mut self) {
        self.dispose();
    }
}

#[derive(Default)]
struct DownloadAbortState {
    aborted: Cell<bool>,
    wakers: RefCell<Vec<std::task::Waker>>,
}

/// Target-portable browser-download cancellation signal.
#[derive(Clone, Default)]
pub struct DownloadAbortSignal(Rc<DownloadAbortState>);

impl std::fmt::Debug for DownloadAbortSignal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DownloadAbortSignal")
            .field("aborted", &self.is_aborted())
            .finish()
    }
}

impl DownloadAbortSignal {
    /// Requests cancellation once and wakes every waiter.
    pub fn abort(&self) {
        if self.0.aborted.replace(true) {
            return;
        }
        for waker in self.0.wakers.borrow_mut().drain(..) {
            waker.wake();
        }
    }

    /// Whether cancellation was requested.
    #[must_use]
    pub fn is_aborted(&self) -> bool {
        self.0.aborted.get()
    }

    /// Resolves after cancellation.
    pub async fn cancelled(&self) {
        futures::future::poll_fn(|context| {
            if self.is_aborted() {
                Poll::Ready(())
            } else {
                self.0.wakers.borrow_mut().push(context.waker().clone());
                Poll::Pending
            }
        })
        .await;
    }
}

impl std::fmt::Debug for SessionLogDownloadController {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionLogDownloadController")
            .field("active", &self.active.borrow().len())
            .field("disposed", &self.disposed.get())
            .field("host_base", &self.host_base)
            .finish_non_exhaustive()
    }
}

impl SessionLogDownloadController {
    /// Creates a controller over injected browser operations.
    #[must_use]
    pub fn new(fetcher: DownloadFetcher, save: DownloadSaver, origin: Option<&str>) -> Rc<Self> {
        Rc::new(Self {
            state: RefCell::new(SessionLogDownloadState::default()),
            active: RefCell::new(BTreeMap::new()),
            disposed: Cell::new(false),
            listeners: RefCell::new(BTreeMap::new()),
            next_listener_id: Cell::new(1),
            fetcher,
            save,
            host_base: host_base(origin),
        })
    }

    /// Returns a detached current state snapshot.
    #[must_use]
    pub fn state(&self) -> SessionLogDownloadState {
        self.state.borrow().clone()
    }

    /// Replaces state, mirroring an external snapshot-store clear.
    pub fn set_state(&self, state: SessionLogDownloadState) {
        *self.state.borrow_mut() = state;
        self.notify();
    }

    /// Subscribes to every immutable state replacement.
    #[must_use]
    pub fn subscribe(self: &Rc<Self>, listener: Rc<dyn Fn()>) -> SessionLogDownloadSubscription {
        let listener_id = self.next_listener_id.get();
        self.next_listener_id.set(listener_id.saturating_add(1));
        self.listeners.borrow_mut().insert(listener_id, listener);
        SessionLogDownloadSubscription {
            controller: Rc::downgrade(self),
            listener_id,
            disposed: Cell::new(false),
        }
    }

    /// Downloads one Session tree; concurrent gestures share one operation.
    pub fn download(self: &Rc<Self>, session_id: SessionId) -> DownloadFuture {
        if let Some(active) = self.active.borrow().get(&session_id) {
            return active.done.clone();
        }
        if self.disposed.get() {
            return futures::future::ready(()).boxed_local().shared();
        }
        self.publish(
            &session_id,
            SessionLogDownloadEntry {
                open: true,
                status: SessionLogDownloadStatus::Downloading,
                error: None,
            },
        );
        let abort = DownloadAbortSignal::default();
        let request = self.request(&session_id, abort.clone());
        let fetch = (self.fetcher)(request.clone());
        let weak = Rc::downgrade(self);
        let owned_id = session_id.clone();
        let run_abort = abort.clone();
        let done = async move {
            if let Some(controller) = weak.upgrade() {
                controller
                    .run(&owned_id, &request.url, &run_abort, fetch)
                    .await;
                controller.active.borrow_mut().remove(&owned_id);
            }
        }
        .boxed_local()
        .shared();
        self.active.borrow_mut().insert(
            session_id,
            ActiveDownload {
                abort,
                done: done.clone(),
            },
        );
        done
    }

    /// Closes one Session's dialog without cancelling its active download.
    pub fn dismiss(&self, session_id: &SessionId) {
        let current = self
            .state
            .borrow()
            .by_session
            .get(session_id.as_str())
            .cloned();
        if let Some(mut current) = current.filter(|entry| entry.open) {
            current.open = false;
            self.publish(session_id, current);
        }
    }

    /// Aborts active fetches and reaches quiescence.
    pub async fn dispose(&self) {
        self.disposed.set(true);
        let active = self
            .active
            .borrow()
            .values()
            .map(|active| {
                active.abort.abort();
                active.done.clone()
            })
            .collect::<Vec<_>>();
        join_all(active).await;
    }

    async fn run(
        &self,
        session_id: &SessionId,
        url: &str,
        signal: &DownloadAbortSignal,
        fetch: LocalBoxFuture<'static, Result<DownloadResponse, String>>,
    ) {
        let result = fetch.await.and_then(|response| {
            if response.ok() {
                Ok(())
            } else {
                let detail = response.detail.unwrap_or_default();
                Err(format!(
                    "Export failed: HTTP {}{}",
                    response.status,
                    if detail.is_empty() {
                        String::new()
                    } else {
                        format!(" {detail}")
                    }
                ))
            }
        });
        if signal.is_aborted() {
            return;
        }
        let open = self
            .state
            .borrow()
            .by_session
            .get(session_id.as_str())
            .is_none_or(|entry| entry.open);
        let result = result.and_then(|()| (self.save)(url, &session_log_zip_filename(session_id)));
        match result {
            Ok(()) => {
                self.publish(
                    session_id,
                    SessionLogDownloadEntry {
                        open,
                        status: SessionLogDownloadStatus::Success,
                        error: None,
                    },
                );
            }
            Err(error) => self.publish(
                session_id,
                SessionLogDownloadEntry {
                    open,
                    status: SessionLogDownloadStatus::Error,
                    error: Some(error),
                },
            ),
        }
    }

    fn request(&self, session_id: &SessionId, signal: DownloadAbortSignal) -> DownloadRequest {
        let mut url = Url::parse(&self.host_base)
            .expect("validated host base")
            .join("/api/session.export")
            .expect("static export path");
        url.query_pairs_mut()
            .append_pair("sessionId", session_id.as_str())
            .append_pair("includeDescendants", "true");
        DownloadRequest {
            url: url.into(),
            signal,
        }
    }

    fn publish(&self, session_id: &SessionId, entry: SessionLogDownloadEntry) {
        self.state
            .borrow_mut()
            .by_session
            .insert(session_id.as_str().to_owned(), entry);
        self.notify();
    }

    fn notify(&self) {
        let listeners = self
            .listeners
            .borrow()
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for listener in listeners {
            listener();
        }
    }
}

/// Collapses an untrusted Session id into the host-owned filename convention.
#[must_use]
pub fn session_log_zip_filename(session_id: &SessionId) -> String {
    let safe = session_id
        .as_str()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("seekdeep-session-{safe}.zip")
}

/// Resolves the browser Host base with the null-origin fallback.
#[must_use]
pub fn host_base(origin: Option<&str>) -> String {
    match origin {
        Some(origin) if origin != "null" => origin.to_owned(),
        _ => "http://seekdeep.internal".to_owned(),
    }
}

#[cfg(target_arch = "wasm32")]
mod browser {
    use futures::FutureExt as _;
    use js_sys::JsString;
    use wasm_bindgen::{JsCast as _, JsValue};
    use wasm_bindgen_futures::JsFuture;
    use web_sys::{AbortController, HtmlAnchorElement, Request, RequestInit, Response};

    use super::*;

    impl SessionLogDownloadController {
        /// Creates the production browser controller over `fetch` and an anchor save.
        ///
        /// # Errors
        ///
        /// Returns when browser globals or the abort controller are unavailable.
        pub fn browser_default() -> Result<Rc<Self>, JsValue> {
            let window =
                web_sys::window().ok_or_else(|| JsValue::from_str("window unavailable"))?;
            let origin = window.location().origin()?;
            let fetch_window = window.clone();
            let fetcher: DownloadFetcher = Rc::new(move |request| {
                let window = fetch_window.clone();
                async move {
                    let web_abort = AbortController::new().map_err(message)?;
                    let bridge = web_abort.clone();
                    let signal = request.signal.clone();
                    let bridge_done = DownloadAbortSignal::default();
                    let wait_done = bridge_done.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        let cancelled = signal.cancelled().fuse();
                        let completed = wait_done.cancelled().fuse();
                        futures::pin_mut!(cancelled, completed);
                        futures::select_biased! {
                            () = cancelled => bridge.abort(),
                            () = completed => {},
                        }
                    });
                    let result = async {
                        let init = RequestInit::new();
                        init.set_method("HEAD");
                        init.set_signal(Some(&web_abort.signal()));
                        let web_request =
                            Request::new_with_str_and_init(&request.url, &init).map_err(message)?;
                        let response = JsFuture::from(window.fetch_with_request(&web_request))
                            .await
                            .map_err(message)?
                            .dyn_into::<Response>()
                            .map_err(message)?;
                        let detail = match response.text() {
                            Ok(text) => JsFuture::from(text)
                                .await
                                .map_err(message)
                                .and_then(|value| value.as_string().ok_or_else(String::new)),
                            Err(error) => Err(message(error)),
                        };
                        Ok(DownloadResponse {
                            status: response.status(),
                            detail,
                        })
                    };
                    let result = result.await;
                    bridge_done.abort();
                    result
                }
                .boxed_local()
            });
            let save_window = window;
            let save: DownloadSaver = Rc::new(move |url, filename| {
                if let Ok(anchor) = save_window
                    .document()
                    .and_then(|document| {
                        document
                            .create_element("a")
                            .ok()?
                            .dyn_into::<HtmlAnchorElement>()
                            .ok()
                    })
                    .ok_or(())
                {
                    anchor.set_href(url);
                    anchor.set_download(filename);
                    anchor.click();
                }
                Ok(())
            });
            Ok(Self::new(fetcher, save, Some(&origin)))
        }
    }

    fn message(value: JsValue) -> String {
        match value.dyn_into::<JsString>() {
            Ok(value) => value.as_string().unwrap_or_default(),
            Err(value) => format!("{value:?}"),
        }
    }
}

/// Weak controller reference used by browser plugin disposal bridges.
pub type WeakSessionLogDownloadController = Weak<SessionLogDownloadController>;
