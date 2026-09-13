//! Lifecycle-owned native access to the catalog's Chokidar boundary.

use crate::{LoaderError, node_plugin::NodeRealm};
use parking_lot::{Condvar, Mutex};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// Exact events emitted by the source-compatible file watcher.
#[derive(Clone, Debug, PartialEq)]
pub enum HostWatchEvent {
    /// The initial scan completed after all preceding initial file events.
    Ready,
    /// A file appeared, including an admitted initial scan event.
    Add(PathBuf),
    /// An existing file changed after configured stabilization.
    Change(PathBuf),
    /// A file was removed.
    Unlink(PathBuf),
    /// A watcher failed during or after startup.
    Error(Value),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub(super) struct WatchId(pub(super) u64);

#[derive(Default)]
struct Ready {
    outcome: Mutex<Option<Result<(), Value>>>,
    blocking: Condvar,
    notified: tokio::sync::Notify,
}

impl Ready {
    fn complete(&self, result: Result<(), Value>) {
        let mut outcome = self.outcome.lock();
        if outcome.is_none() {
            *outcome = Some(result);
        }
        drop(outcome);
        self.blocking.notify_all();
        self.notified.notify_waiters();
    }
}

pub(super) struct WatchRoute {
    sender: tokio::sync::mpsc::UnboundedSender<HostWatchEvent>,
    ready: Arc<Ready>,
}
pub(super) type WatchRoutes = Arc<Mutex<HashMap<WatchId, WatchRoute>>>;

/// One watcher, its readiness barrier, and its exact event stream.
pub struct HostFileWatcher {
    realm: Arc<NodeRealm>,
    id: WatchId,
    ready: Arc<Ready>,
    receiver: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<HostWatchEvent>>,
    closed: AtomicBool,
    closing: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for HostFileWatcher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HostFileWatcher")
            .field("id", &self.id)
            .field("closed", &self.closed.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl HostFileWatcher {
    pub(super) fn open(
        realm: &Arc<NodeRealm>,
        roots: &[PathBuf],
        options: &Value,
        ignored: Option<&(PathBuf, Vec<String>)>,
    ) -> Result<Arc<Self>, LoaderError> {
        let id = WatchId(realm.next_watch.fetch_add(1, Ordering::Relaxed));
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let ready = Arc::new(Ready::default());
        realm.watches.lock().insert(
            id,
            WatchRoute {
                sender,
                ready: ready.clone(),
            },
        );
        let watcher = Arc::new(Self {
            realm: realm.clone(),
            id,
            ready,
            receiver: tokio::sync::Mutex::new(receiver),
            closed: AtomicBool::new(false),
            closing: tokio::sync::Mutex::new(()),
        });
        let ignored = ignored.map(|(base, patterns)| json!({"base":base,"patterns":patterns}));
        if let Err(error) = realm.request(
            json!({"action":"watch","watch":id,"roots":roots,"options":options,"ignored":ignored}),
        ) {
            realm.watches.lock().remove(&id);
            watcher.closed.store(true, Ordering::Release);
            return Err(error);
        }
        Ok(watcher)
    }

    /// Waits until the initial scan reaches readiness.
    ///
    /// # Errors
    ///
    /// Returns the original startup failure or closure before readiness.
    pub async fn ready(&self) -> Result<(), LoaderError> {
        loop {
            let notified = self.ready.notified.notified();
            if let Some(outcome) = self.ready.outcome.lock().clone() {
                return outcome.map_err(crate::node_plugin::load_error);
            }
            notified.await;
        }
    }

    /// Waits for readiness from a synchronous service constructor.
    ///
    /// # Errors
    ///
    /// Returns the original startup failure or closure before readiness.
    pub fn ready_blocking(&self) -> Result<(), LoaderError> {
        let mut outcome = self.ready.outcome.lock();
        loop {
            if let Some(outcome) = outcome.clone() {
                return outcome.map_err(crate::node_plugin::load_error);
            }
            self.ready.blocking.wait(&mut outcome);
        }
    }

    /// Receives the next admitted file or watcher-error event.
    pub async fn next_event(&self) -> Option<HostWatchEvent> {
        self.receiver.lock().await.recv().await
    }

    /// Closes the native watcher and joins its close operation.
    ///
    /// # Errors
    ///
    /// Returns an unexpected watcher-close or realm-transport failure.
    pub async fn close(&self) -> Result<(), LoaderError> {
        let _closing = self.closing.lock().await;
        if self.closed.load(Ordering::Acquire) {
            return Ok(());
        }
        let outcome = self
            .realm
            .request_async(json!({"action":"closeWatch","watch":self.id}))
            .await;
        self.closed.store(true, Ordering::Release);
        self.realm.watches.lock().remove(&self.id);
        self.ready.complete(Err(
            json!({"message":"Host file watcher closed before readiness"}),
        ));
        outcome.map(|_| ())
    }
}

impl Drop for HostFileWatcher {
    fn drop(&mut self) {
        if !self.closed.swap(true, Ordering::AcqRel) {
            let _ = self
                .realm
                .write(&json!({"action":"closeWatch","watch":self.id}));
        }
        self.realm.watches.lock().remove(&self.id);
        self.ready.complete(Err(
            json!({"message":"Host file watcher closed before readiness"}),
        ));
    }
}

pub(super) fn route(routes: &WatchRoutes, message: &Value) {
    let Some(id) = message["watch"].as_u64() else {
        return;
    };
    let routes = routes.lock();
    let Some(route) = routes.get(&WatchId(id)) else {
        return;
    };
    let path = PathBuf::from(message["path"].as_str().unwrap_or_default());
    let event = match message["type"].as_str().unwrap_or_default() {
        "ready" => {
            route.ready.complete(Ok(()));
            HostWatchEvent::Ready
        }
        "add" => HostWatchEvent::Add(path),
        "change" => HostWatchEvent::Change(path),
        "unlink" => HostWatchEvent::Unlink(path),
        "error" => {
            let error = message["error"].clone();
            route.ready.complete(Err(error.clone()));
            HostWatchEvent::Error(error)
        }
        _ => return,
    };
    let _ = route.sender.send(event);
}

pub(super) fn closed(routes: &WatchRoutes) {
    for (_, route) in routes.lock().drain() {
        let error = json!({"message":"Node plugin realm closed"});
        route.ready.complete(Err(error.clone()));
        let _ = route.sender.send(HostWatchEvent::Error(error));
    }
}
