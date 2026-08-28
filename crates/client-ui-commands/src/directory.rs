//! Session-keyed command directory with epoch-guarded publication and abortable strong waits.

use std::{cell::RefCell, rc::Rc};

use futures::{
    FutureExt as _,
    channel::oneshot,
    future::{Either, LocalBoxFuture, select},
};
use indexmap::IndexMap;
use seekdeep_commands_contract::CommandDescriptor;
use seekdeep_identity::SessionId;

/// One cache key's load lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandDirectoryStatus {
    /// Never pulled.
    Cold,
    /// Pull active with no servable snapshot.
    Pending,
    /// Servable snapshot, including during soft refresh.
    Ready,
    /// Latest winning pull failed and dropped its snapshot.
    Failed,
}

/// Injected command-list transport.
pub trait CommandDirectoryTransport {
    /// Fetches one Session's Host catalog.
    fn fetch(
        &self,
        session_id: SessionId,
    ) -> LocalBoxFuture<'static, Result<Vec<CommandDescriptor>, String>>;
}

/// Fire-and-forget task owner used by warm and invalidation paths.
pub trait CommandDirectorySpawner {
    /// Owns one local task until settlement.
    fn spawn(&self, task: LocalBoxFuture<'static, ()>);
}

/// Attempt-scoped abort signal for strong waits.
pub trait CommandDirectoryAbort {
    /// Whether the attempt is already aborted.
    fn aborted(&self) -> bool;
    /// Normalized abort diagnostic.
    fn reason(&self) -> String;
    /// Resolves when the attempt aborts.
    fn cancelled(&self) -> LocalBoxFuture<'static, ()>;
}

struct Entry {
    state: CommandDirectoryStatus,
    commands: Vec<CommandDescriptor>,
    epoch: u64,
    last_error: Option<String>,
    waiters: IndexMap<u64, oneshot::Sender<()>>,
    next_waiter: u64,
}

impl Default for Entry {
    fn default() -> Self {
        Self {
            state: CommandDirectoryStatus::Cold,
            commands: Vec::new(),
            epoch: 0,
            last_error: None,
            waiters: IndexMap::new(),
            next_waiter: 0,
        }
    }
}

/// Session-keyed command cache.
pub struct CommandDirectory {
    transport: Rc<dyn CommandDirectoryTransport>,
    spawner: Rc<dyn CommandDirectorySpawner>,
    entries: RefCell<IndexMap<SessionId, Entry>>,
}

impl CommandDirectory {
    /// Creates an untouched directory.
    #[must_use]
    pub fn new(
        transport: Rc<dyn CommandDirectoryTransport>,
        spawner: Rc<dyn CommandDirectorySpawner>,
    ) -> Rc<Self> {
        Rc::new(Self {
            transport,
            spawner,
            entries: RefCell::new(IndexMap::new()),
        })
    }

    /// Returns one Session's current status, defaulting to cold.
    #[must_use]
    pub fn status(&self, session_id: &SessionId) -> CommandDirectoryStatus {
        self.entries
            .borrow()
            .get(session_id)
            .map_or(CommandDirectoryStatus::Cold, |entry| entry.state)
    }

    /// Resolves one exact command only from a ready snapshot.
    #[must_use]
    pub fn resolve(&self, session_id: &SessionId, name: &str) -> Option<CommandDescriptor> {
        let entries = self.entries.borrow();
        let entry = entries.get(session_id)?;
        (entry.state == CommandDirectoryStatus::Ready)
            .then(|| {
                entry
                    .commands
                    .iter()
                    .find(|command| command.name == name)
                    .cloned()
            })
            .flatten()
    }

    /// Soft-refreshes every touched key while ready snapshots keep serving.
    pub fn invalidate_all(self: &Rc<Self>) {
        let keys = self.entries.borrow().keys().cloned().collect::<Vec<_>>();
        for key in keys {
            self.spawn_refresh(key);
        }
    }

    /// Hard-drops and prewarms every touched key after reconnect.
    pub fn reset_connected(self: &Rc<Self>) {
        let keys = {
            let mut entries = self.entries.borrow_mut();
            for entry in entries.values_mut() {
                entry.state = CommandDirectoryStatus::Cold;
                entry.commands.clear();
            }
            entries.keys().cloned().collect::<Vec<_>>()
        };
        for key in keys {
            self.spawn_refresh(key);
        }
    }

    /// Prewarms cold and failed keys without duplicating pending or ready work.
    pub fn warm(self: &Rc<Self>, session_id: SessionId) {
        let state = self.entry_state(&session_id);
        if matches!(
            state,
            CommandDirectoryStatus::Cold | CommandDirectoryStatus::Failed
        ) {
            self.spawn_refresh(session_id);
        }
    }

    /// Starts one epoch-owned pull.
    ///
    /// # Panics
    ///
    /// Panics after exhausting every `u64` epoch rather than allowing stale publication.
    #[must_use]
    pub fn refresh(self: &Rc<Self>, session_id: SessionId) -> LocalBoxFuture<'static, ()> {
        let epoch = {
            let mut entries = self.entries.borrow_mut();
            let entry = entries.entry(session_id.clone()).or_default();
            entry.epoch = entry
                .epoch
                .checked_add(1)
                .expect("command directory epoch exhausted");
            if entry.state != CommandDirectoryStatus::Ready {
                entry.state = CommandDirectoryStatus::Pending;
            }
            entry.epoch
        };
        let directory = self.clone();
        async move {
            let result = directory.transport.fetch(session_id.clone()).await;
            let waiters = {
                let mut entries = directory.entries.borrow_mut();
                let Some(entry) = entries.get_mut(&session_id) else {
                    return;
                };
                if entry.epoch != epoch {
                    return;
                }
                match result {
                    Ok(commands) => {
                        entry.commands = commands;
                        entry.state = CommandDirectoryStatus::Ready;
                        entry.last_error = None;
                    }
                    Err(error) => {
                        entry.commands.clear();
                        entry.state = CommandDirectoryStatus::Failed;
                        entry.last_error = Some(error);
                    }
                }
                std::mem::take(&mut entry.waiters)
            };
            for (_, waiter) in waiters {
                let _ = waiter.send(());
            }
        }
        .boxed_local()
    }

    /// Strong-waits until one Session has a ready snapshot.
    #[must_use]
    pub fn ensure_ready(
        self: &Rc<Self>,
        session_id: SessionId,
        signal: Rc<dyn CommandDirectoryAbort>,
    ) -> LocalBoxFuture<'static, Result<Vec<CommandDescriptor>, String>> {
        let directory = self.clone();
        async move {
            loop {
                match directory.status(&session_id) {
                    CommandDirectoryStatus::Ready => {
                        return Ok(directory
                            .entries
                            .borrow()
                            .get(&session_id)
                            .map_or_else(Vec::new, |entry| entry.commands.clone()));
                    }
                    CommandDirectoryStatus::Pending => {
                        directory
                            .wait_for_settlement(&session_id, signal.clone())
                            .await?;
                    }
                    CommandDirectoryStatus::Cold | CommandDirectoryStatus::Failed => {
                        directory.spawn_refresh(session_id.clone());
                        directory
                            .wait_for_settlement(&session_id, signal.clone())
                            .await?;
                    }
                }
                if directory.status(&session_id) == CommandDirectoryStatus::Failed {
                    let error = directory
                        .entries
                        .borrow()
                        .get(&session_id)
                        .and_then(|entry| entry.last_error.clone())
                        .unwrap_or_else(|| "unknown command directory failure".to_owned());
                    return Err(format!("command directory warmup failed: {error}"));
                }
            }
        }
        .boxed_local()
    }

    fn entry_state(&self, session_id: &SessionId) -> CommandDirectoryStatus {
        let mut entries = self.entries.borrow_mut();
        entries.entry(session_id.clone()).or_default().state
    }

    fn spawn_refresh(self: &Rc<Self>, session_id: SessionId) {
        let refresh = self.refresh(session_id);
        self.spawner.spawn(refresh);
    }

    async fn wait_for_settlement(
        &self,
        session_id: &SessionId,
        signal: Rc<dyn CommandDirectoryAbort>,
    ) -> Result<(), String> {
        if signal.aborted() {
            return Err(signal.reason());
        }
        let (sender, receiver) = oneshot::channel();
        let waiter_id = {
            let mut entries = self.entries.borrow_mut();
            let entry = entries.entry(session_id.clone()).or_default();
            entry.next_waiter = entry
                .next_waiter
                .checked_add(1)
                .expect("command directory waiter id exhausted");
            let id = entry.next_waiter;
            entry.waiters.insert(id, sender);
            id
        };
        let settlement = receiver.map(|_| ()).boxed_local();
        let cancellation = signal.cancelled();
        match select(settlement, cancellation).await {
            Either::Left(_) => Ok(()),
            Either::Right(_) => {
                if let Some(entry) = self.entries.borrow_mut().get_mut(session_id) {
                    entry.waiters.shift_remove(&waiter_id);
                }
                Err(signal.reason())
            }
        }
    }
}
