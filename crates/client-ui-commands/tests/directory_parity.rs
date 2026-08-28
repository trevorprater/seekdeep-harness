//! Command directory cache, epochs, invalidation, and strong-wait parity.

use std::{cell::RefCell, collections::VecDeque, rc::Rc};

use futures::{
    FutureExt as _,
    channel::oneshot,
    executor::{LocalPool, LocalSpawner},
    future::LocalBoxFuture,
    task::LocalSpawnExt as _,
};
use seekdeep_client_ui_commands::{
    CommandDirectory, CommandDirectoryAbort, CommandDirectorySpawner, CommandDirectoryStatus,
    CommandDirectoryTransport,
};
use seekdeep_commands::{CommandDescriptor, CommandInputDescriptor};
use seekdeep_identity::SessionId;

enum Plan {
    Ready(Result<Vec<CommandDescriptor>, String>),
    Deferred(oneshot::Receiver<Result<Vec<CommandDescriptor>, String>>),
}

#[derive(Default)]
struct Transport {
    plans: RefCell<VecDeque<Plan>>,
    calls: RefCell<Vec<SessionId>>,
}

impl CommandDirectoryTransport for Transport {
    fn fetch(
        &self,
        session_id: SessionId,
    ) -> LocalBoxFuture<'static, Result<Vec<CommandDescriptor>, String>> {
        self.calls.borrow_mut().push(session_id);
        match self.plans.borrow_mut().pop_front().unwrap() {
            Plan::Ready(value) => futures::future::ready(value).boxed_local(),
            Plan::Deferred(receiver) => async move {
                receiver
                    .await
                    .unwrap_or_else(|_| Err("test pull dropped".to_owned()))
            }
            .boxed_local(),
        }
    }
}

struct Spawner(LocalSpawner);

impl CommandDirectorySpawner for Spawner {
    fn spawn(&self, task: LocalBoxFuture<'static, ()>) {
        self.0.spawn_local(task).unwrap();
    }
}

#[derive(Default)]
struct AbortState {
    aborted: bool,
    reason: String,
    listeners: Vec<oneshot::Sender<()>>,
}

#[derive(Default)]
struct Abort {
    state: Rc<RefCell<AbortState>>,
}

impl Abort {
    fn abort(&self, reason: &str) {
        let listeners = {
            let mut state = self.state.borrow_mut();
            state.aborted = true;
            reason.clone_into(&mut state.reason);
            std::mem::take(&mut state.listeners)
        };
        for listener in listeners {
            let _ = listener.send(());
        }
    }
}

impl CommandDirectoryAbort for Abort {
    fn aborted(&self) -> bool {
        self.state.borrow().aborted
    }

    fn reason(&self) -> String {
        let state = self.state.borrow();
        if state.reason.is_empty() {
            "command directory wait aborted".to_owned()
        } else {
            state.reason.clone()
        }
    }

    fn cancelled(&self) -> LocalBoxFuture<'static, ()> {
        let (sender, receiver) = oneshot::channel();
        if self.aborted() {
            let _ = sender.send(());
        } else {
            self.state.borrow_mut().listeners.push(sender);
        }
        receiver.map(|_| ()).boxed_local()
    }
}

fn command(name: &str, input: bool) -> CommandDescriptor {
    CommandDescriptor {
        name: name.to_owned(),
        description: format!("{name} description"),
        input: input.then(|| CommandInputDescriptor {
            hint: format!("{name} hint"),
        }),
    }
}

fn make_directory(pool: &LocalPool) -> (Rc<Transport>, Rc<CommandDirectory>) {
    let transport = Rc::new(Transport::default());
    let directory = CommandDirectory::new(transport.clone(), Rc::new(Spawner(pool.spawner())));
    (transport, directory)
}

#[test]
fn status_lookup_failure_isolation_and_epoch_guards_match_the_source() {
    let mut pool = LocalPool::new();
    let (transport, directory) = make_directory(&pool);
    let one = SessionId::new("s1");
    let two = SessionId::new("s2");
    assert_eq!(directory.status(&one), CommandDirectoryStatus::Cold);
    assert_eq!(directory.resolve(&one, "goal"), None);
    transport
        .plans
        .borrow_mut()
        .push_back(Plan::Ready(Ok(vec![command("goal", true)])));
    pool.run_until(directory.refresh(one.clone()));
    assert_eq!(directory.status(&one), CommandDirectoryStatus::Ready);
    assert_eq!(directory.resolve(&one, "goal").unwrap().name, "goal");
    assert_eq!(directory.status(&two), CommandDirectoryStatus::Cold);

    transport
        .plans
        .borrow_mut()
        .push_back(Plan::Ready(Err("offline".to_owned())));
    pool.run_until(directory.refresh(one.clone()));
    assert_eq!(directory.status(&one), CommandDirectoryStatus::Failed);
    assert_eq!(directory.resolve(&one, "goal"), None);

    let (old_sender, old_receiver) = oneshot::channel();
    transport.plans.borrow_mut().extend([
        Plan::Deferred(old_receiver),
        Plan::Ready(Ok(vec![command("new", false)])),
    ]);
    let old = directory.refresh(one.clone());
    pool.spawner().spawn_local(old).unwrap();
    pool.run_until_stalled();
    pool.run_until(directory.refresh(one.clone()));
    assert!(old_sender.send(Ok(vec![command("old", false)])).is_ok());
    pool.run_until_stalled();
    assert!(directory.resolve(&one, "new").is_some());
    assert!(directory.resolve(&one, "old").is_none());
}

#[test]
fn warm_soft_invalidate_and_hard_reset_touch_only_existing_keys() {
    let mut pool = LocalPool::new();
    let (transport, directory) = make_directory(&pool);
    let one = SessionId::new("s1");
    let two = SessionId::new("s2");
    directory.invalidate_all();
    assert!(transport.calls.borrow().is_empty());
    transport.plans.borrow_mut().extend([
        Plan::Ready(Ok(vec![command("one", false)])),
        Plan::Ready(Ok(vec![command("two", false)])),
    ]);
    directory.warm(one.clone());
    directory.warm(one.clone());
    directory.warm(two.clone());
    pool.run_until_stalled();
    assert_eq!(transport.calls.borrow().len(), 2);
    assert_eq!(directory.status(&one), CommandDirectoryStatus::Ready);

    transport.plans.borrow_mut().extend([
        Plan::Ready(Ok(vec![command("one-new", false)])),
        Plan::Ready(Ok(vec![command("two-new", false)])),
    ]);
    directory.invalidate_all();
    assert!(directory.resolve(&one, "one").is_some());
    pool.run_until_stalled();
    assert!(directory.resolve(&one, "one-new").is_some());

    transport.plans.borrow_mut().extend([
        Plan::Ready(Ok(vec![command("one-reset", false)])),
        Plan::Ready(Ok(vec![command("two-reset", false)])),
    ]);
    directory.reset_connected();
    assert_eq!(directory.resolve(&one, "one-new"), None);
    assert_eq!(directory.status(&one), CommandDirectoryStatus::Pending);
    pool.run_until_stalled();
    assert!(directory.resolve(&one, "one-reset").is_some());
}

#[test]
fn ensure_ready_joins_retries_spans_supersession_and_aborts() {
    let mut pool = LocalPool::new();
    let (transport, directory) = make_directory(&pool);
    let session = SessionId::new("s1");
    let (sender, receiver) = oneshot::channel();
    transport
        .plans
        .borrow_mut()
        .push_back(Plan::Deferred(receiver));
    let first_abort = Rc::new(Abort::default());
    let second_abort = Rc::new(Abort::default());
    let first = directory.ensure_ready(session.clone(), first_abort);
    let second = directory.ensure_ready(session.clone(), second_abort);
    let first_result = Rc::new(RefCell::new(None));
    let second_result = Rc::new(RefCell::new(None));
    let first_out = first_result.clone();
    let second_out = second_result.clone();
    pool.spawner()
        .spawn_local(async move { *first_out.borrow_mut() = Some(first.await) })
        .unwrap();
    pool.spawner()
        .spawn_local(async move { *second_out.borrow_mut() = Some(second.await) })
        .unwrap();
    pool.run_until_stalled();
    assert_eq!(transport.calls.borrow().len(), 1);
    assert!(sender.send(Ok(vec![command("goal", false)])).is_ok());
    pool.run_until_stalled();
    assert_eq!(
        first_result
            .borrow()
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        second_result
            .borrow()
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .len(),
        1
    );

    let mut failure_pool = LocalPool::new();
    let (failure_transport, failure_directory) = make_directory(&failure_pool);
    failure_transport
        .plans
        .borrow_mut()
        .push_back(Plan::Ready(Err("bad catalog".to_owned())));
    let error = failure_pool
        .run_until(
            failure_directory.ensure_ready(SessionId::new("failed"), Rc::new(Abort::default())),
        )
        .unwrap_err();
    assert!(error.contains("bad catalog"));

    let mut abort_pool = LocalPool::new();
    let (abort_transport, abort_directory) = make_directory(&abort_pool);
    let (retry_sender, retry_receiver) = oneshot::channel();
    abort_transport
        .plans
        .borrow_mut()
        .push_back(Plan::Deferred(retry_receiver));
    let abort = Rc::new(Abort::default());
    let waiting = abort_directory.ensure_ready(SessionId::new("abort"), abort.clone());
    let result = Rc::new(RefCell::new(None));
    let output = result.clone();
    abort_pool
        .spawner()
        .spawn_local(async move { *output.borrow_mut() = Some(waiting.await) })
        .unwrap();
    abort_pool.run_until_stalled();
    abort.abort("stop");
    abort_pool.run_until_stalled();
    assert_eq!(
        result.borrow().as_ref().unwrap().as_ref().unwrap_err(),
        "stop"
    );
    assert!(retry_sender.send(Ok(vec![command("late", false)])).is_ok());
    abort_pool.run_until_stalled();

    let mut already_pool = LocalPool::new();
    let (already_transport, already_directory) = make_directory(&already_pool);
    already_transport
        .plans
        .borrow_mut()
        .push_back(Plan::Ready(Ok(vec![command("unused", false)])));
    let already = Rc::new(Abort::default());
    already.abort("already");
    let error = already_pool
        .run_until(already_directory.ensure_ready(SessionId::new("cold"), already))
        .unwrap_err();
    assert_eq!(error, "already");
}
