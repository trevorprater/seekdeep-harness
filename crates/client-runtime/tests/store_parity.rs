//! Snapshot update, batching, persistence, actions, and instance-identity parity.

use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, HashMap, VecDeque},
    rc::Rc,
    sync::Arc,
};

use seekdeep_client_runtime::*;
use seekdeep_client_ui_slots::SlotStoreInstance;
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Default)]
struct ManualFrames {
    queue: RefCell<VecDeque<Box<dyn FnOnce()>>>,
}

impl StoreFlushScheduler for ManualFrames {
    fn queue(&self, callback: Box<dyn FnOnce()>) {
        self.queue.borrow_mut().push_back(callback);
    }
}

impl ManualFrames {
    fn flush_one(&self) -> bool {
        let Some(callback) = self.queue.borrow_mut().pop_front() else {
            return false;
        };
        callback();
        true
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Branch {
    n: i64,
}

#[derive(Clone, Debug, PartialEq)]
struct State {
    a: Arc<Branch>,
    b: Arc<Vec<String>>,
}

fn initial() -> State {
    State {
        a: Arc::new(Branch { n: 1 }),
        b: Arc::new(vec!["x".to_owned()]),
    }
}

fn logger() -> StoreLogger {
    Rc::new(|_| {})
}

#[test]
fn update_replaces_snapshot_and_preserves_untouched_branch_identity() {
    let scheduler = Rc::new(ManualFrames::default());
    let store = SnapshotStore::new(initial(), StoreFlushMode::Sync, scheduler, None, logger());
    let before = store.snapshot();
    store.update(|draft| Arc::make_mut(&mut draft.a).n = 2);
    let after = store.snapshot();
    assert!(!Rc::ptr_eq(&before, &after));
    assert_eq!(after.a.n, 2);
    assert!(Arc::ptr_eq(&before.b, &after.b));
}

#[test]
fn sync_mode_notifies_each_update_immediately() {
    let store = SnapshotStore::new(
        initial(),
        StoreFlushMode::Sync,
        Rc::new(ManualFrames::default()),
        None,
        logger(),
    );
    let seen = Rc::new(RefCell::new(Vec::new()));
    let observed = seen.clone();
    let observed_store = store.clone();
    let _subscription = store.subscribe(Rc::new(move || {
        observed.borrow_mut().push(observed_store.snapshot().a.n);
    }));
    store.update(|draft| Arc::make_mut(&mut draft.a).n = 2);
    store.update(|draft| Arc::make_mut(&mut draft.a).n = 3);
    assert_eq!(seen.borrow().as_slice(), [2, 3]);
}

#[test]
fn frame_mode_coalesces_and_unsubscribe_before_flush_suppresses_delivery() {
    let scheduler = Rc::new(ManualFrames::default());
    let store = SnapshotStore::new(
        initial(),
        StoreFlushMode::Frame,
        scheduler.clone(),
        None,
        logger(),
    );
    let calls = Rc::new(Cell::new(0));
    let observed = calls.clone();
    let subscription = store.subscribe(Rc::new(move || observed.set(observed.get() + 1)));
    store.update(|draft| Arc::make_mut(&mut draft.a).n = 2);
    store.update(|draft| Arc::make_mut(&mut draft.a).n = 3);
    store.update(|draft| Arc::make_mut(&mut draft.b).push("y".to_owned()));
    assert_eq!(calls.get(), 0);
    assert!(scheduler.flush_one());
    assert_eq!(calls.get(), 1);
    store.update(|draft| Arc::make_mut(&mut draft.a).n = 4);
    subscription.dispose();
    assert!(scheduler.flush_one());
    assert_eq!(calls.get(), 1);
}

#[derive(Clone)]
struct MemoryPersistence<T> {
    name: String,
    values: Rc<RefCell<HashMap<String, T>>>,
    removals: Rc<RefCell<Vec<String>>>,
    fail: bool,
}

impl<T: Clone> StorePersistence<T> for MemoryPersistence<T> {
    fn read(&self) -> Result<Option<T>, String> {
        if self.fail {
            Err("read failed".to_owned())
        } else {
            Ok(self.values.borrow().get(&self.name).cloned())
        }
    }

    fn write(&self, value: &T) -> Result<(), String> {
        if self.fail {
            Err("write failed".to_owned())
        } else {
            self.values
                .borrow_mut()
                .insert(self.name.clone(), value.clone());
            Ok(())
        }
    }

    fn remove(&self) -> Result<(), String> {
        if self.fail {
            Err("remove failed".to_owned())
        } else {
            self.values.borrow_mut().remove(&self.name);
            self.removals.borrow_mut().push(self.name.clone());
            Ok(())
        }
    }
}

#[test]
fn whole_primitive_persistence_rehydrates_without_spreading_and_failures_are_nonfatal() {
    let values = Rc::new(RefCell::new(HashMap::new()));
    let removals = Rc::new(RefCell::new(Vec::new()));
    let persistence = Rc::new(MemoryPersistence {
        name: "draft".to_owned(),
        values: values.clone(),
        removals: removals.clone(),
        fail: false,
    });
    let store = SnapshotStore::new(
        String::new(),
        StoreFlushMode::Sync,
        Rc::new(ManualFrames::default()),
        Some(("draft".to_owned(), persistence.clone())),
        logger(),
    );
    store.set("hello".to_owned());
    let revived = SnapshotStore::new(
        String::new(),
        StoreFlushMode::Sync,
        Rc::new(ManualFrames::default()),
        Some(("draft".to_owned(), persistence)),
        logger(),
    );
    assert_eq!(revived.snapshot().as_str(), "hello");

    let logs = Rc::new(RefCell::new(Vec::new()));
    let observed = logs.clone();
    let broken = Rc::new(MemoryPersistence {
        name: "broken".to_owned(),
        values,
        removals,
        fail: true,
    });
    let store = SnapshotStore::new(
        "fallback".to_owned(),
        StoreFlushMode::Sync,
        Rc::new(ManualFrames::default()),
        Some(("broken".to_owned(), broken)),
        Rc::new(move |message| observed.borrow_mut().push(message)),
    );
    assert_eq!(store.snapshot().as_str(), "fallback");
    store.set("next".to_owned());
    store.clear_persisted();
    assert_eq!(logs.borrow().len(), 3);
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct DraftState {
    selection: Option<String>,
    draft: String,
}

fn declaration() -> StoreDeclaration<DraftState> {
    let mut actions: BTreeMap<String, StoreAction<DraftState>> = BTreeMap::new();
    actions.insert(
        "select".to_owned(),
        Rc::new(|draft, args| {
            draft.selection = Some(
                args.first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| "select needs a string".to_owned())?
                    .to_owned(),
            );
            Ok(())
        }),
    );
    actions.insert(
        "setDraft".to_owned(),
        Rc::new(|draft, args| {
            args.first()
                .and_then(Value::as_str)
                .ok_or_else(|| "setDraft needs a string".to_owned())?
                .clone_into(&mut draft.draft);
            Ok(())
        }),
    );
    actions.insert(
        "clearDraft".to_owned(),
        Rc::new(|draft, _| {
            draft.draft.clear();
            Ok(())
        }),
    );
    StoreDeclaration {
        init: Rc::new(|| DraftState {
            selection: None,
            draft: String::new(),
        }),
        persist: None,
        actions,
    }
}

fn environment<T>() -> StoreEnvironment<T> {
    StoreEnvironment {
        scheduler: Rc::new(ManualFrames::default()),
        persistence: None,
        logger: logger(),
    }
}

#[test]
fn declarative_handle_bakes_actions_and_creates_independent_instances() {
    let handle = EngineStoreHandle::new(declaration(), environment());
    let first = handle.create_typed(None);
    let second = handle.create_typed(None);
    first.invoke("setDraft", &[json!("hello")]).unwrap();
    first.invoke("select", &[json!("m1")]).unwrap();
    assert_eq!(
        first.store.snapshot().as_ref(),
        &DraftState {
            selection: Some("m1".to_owned()),
            draft: "hello".to_owned(),
        }
    );
    assert_eq!(second.store.snapshot().draft, "");
    first.invoke("clearDraft", &[]).unwrap();
    assert_eq!(first.store.snapshot().draft, "");
    assert!(first.invoke("setDraft", &[json!(1)]).is_err());
    assert_eq!(first.store.snapshot().draft, "");
}

#[test]
fn declarative_persist_keys_suffix_by_scope_and_clear_exact_key() {
    let values = Rc::new(RefCell::new(HashMap::<String, DraftState>::new()));
    let removals = Rc::new(RefCell::new(Vec::new()));
    let persistence_values = values.clone();
    let persistence_removals = removals.clone();
    let factory: StorePersistenceFactory<DraftState> = Rc::new(move |name| {
        Rc::new(MemoryPersistence {
            name: name.to_owned(),
            values: persistence_values.clone(),
            removals: persistence_removals.clone(),
            fail: false,
        })
    });
    let mut declaration = declaration();
    declaration.persist = Some("chat".to_owned());
    let handle = EngineStoreHandle::new(
        declaration,
        StoreEnvironment {
            scheduler: Rc::new(ManualFrames::default()),
            persistence: Some(factory),
            logger: logger(),
        },
    );
    handle
        .create_typed(Some("s1"))
        .invoke("setDraft", &[json!("one")])
        .unwrap();
    handle
        .create_typed(Some("s2"))
        .invoke("setDraft", &[json!("two")])
        .unwrap();
    handle
        .create_typed(None)
        .invoke("setDraft", &[json!("root")])
        .unwrap();
    assert_eq!(values.borrow()["chat.s1"].draft, "one");
    assert_eq!(values.borrow()["chat.s2"].draft, "two");
    assert_eq!(values.borrow()["chat"].draft, "root");
    let revived = handle.create_typed(Some("s1"));
    assert_eq!(revived.store.snapshot().draft, "one");
    revived.clear_persisted();
    assert!(!values.borrow().contains_key("chat.s1"));
    assert!(values.borrow().contains_key("chat.s2"));
    assert_eq!(removals.borrow().as_slice(), ["chat.s1"]);
}
