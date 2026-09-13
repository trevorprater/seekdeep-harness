//! Host-computed projection store sequence, identity, and publication parity.

use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
};

use indexmap::IndexMap;
use seekdeep_client_runtime::{NotifierScheduler, ProjectionValueStore, ProjectionsBaseline};
use serde_json::{Value, json};

#[derive(Default)]
struct ManualScheduler {
    microtasks: RefCell<VecDeque<Box<dyn FnOnce()>>>,
}

impl NotifierScheduler for ManualScheduler {
    fn has_animation_frame(&self) -> bool {
        false
    }

    fn queue_microtask(&self, callback: Box<dyn FnOnce()>) {
        self.microtasks.borrow_mut().push_back(callback);
    }

    fn queue_animation_frame(&self, callback: Box<dyn FnOnce()>) {
        self.queue_microtask(callback);
    }
}

impl ManualScheduler {
    fn flush(&self) {
        while let Some(callback) = self.microtasks.borrow_mut().pop_front() {
            callback();
        }
    }
}

fn store() -> (ProjectionValueStore<Value>, Rc<ManualScheduler>) {
    let scheduler = Rc::new(ManualScheduler::default());
    (ProjectionValueStore::new(scheduler.clone()), scheduler)
}

fn value(value: Value) -> Rc<Value> {
    Rc::new(value)
}

#[test]
fn absent_keys_read_none_and_faces_are_identity_stable() {
    let (store, _) = store();
    assert!(store.get("test/marks").is_none());
    let face = store.face_of("test/marks");
    assert!(face.snapshot().is_none());
    assert!(Rc::ptr_eq(&face, &store.face_of("test/marks")));
}

#[test]
fn strictly_higher_sequences_win_and_keep_the_exact_value_identity() {
    let (store, _) = store();
    let first = value(json!({"marks":["a"]}));
    let latest = value(json!({"marks":["a","b"]}));
    store.apply("test/marks", first, 5);
    store.apply("test/marks", latest.clone(), 9);
    store.apply("test/marks", value(json!({"marks":["stale"]})), 5);
    store.apply("test/marks", value(json!({"marks":["equal"]})), 9);
    assert!(Rc::ptr_eq(&store.get("test/marks").unwrap(), &latest));
    assert!(Rc::ptr_eq(
        &store.face_of("test/marks").snapshot().unwrap(),
        &latest
    ));
}

#[test]
fn stale_baselines_neither_overwrite_nor_clear_and_fresh_baselines_do_both() {
    let (store, _) = store();
    let pushed = value(json!({"marks":["frame-20"]}));
    store.apply("test/marks", pushed.clone(), 20);
    store.seed(&ProjectionsBaseline {
        as_of_seq: 10,
        values: IndexMap::from([(
            "test/marks".to_owned(),
            value(json!({"marks":["baseline-10"]})),
        )]),
    });
    assert!(Rc::ptr_eq(&store.get("test/marks").unwrap(), &pushed));
    store.seed(&ProjectionsBaseline {
        as_of_seq: 15,
        values: IndexMap::new(),
    });
    assert!(Rc::ptr_eq(&store.get("test/marks").unwrap(), &pushed));

    let fresh = value(json!({"marks":["baseline-30"]}));
    store.seed(&ProjectionsBaseline {
        as_of_seq: 30,
        values: IndexMap::from([("test/marks".to_owned(), fresh.clone())]),
    });
    assert!(Rc::ptr_eq(&store.get("test/marks").unwrap(), &fresh));
    store.seed(&ProjectionsBaseline {
        as_of_seq: 40,
        values: IndexMap::new(),
    });
    assert!(store.get("test/marks").is_none());
}

#[test]
fn generation_truncation_keeps_durable_rows_and_drops_phantom_rows() {
    let (store, _) = store();
    store.apply("test/marks", value(json!({"marks":["durable"]})), 5);
    store.apply("other", value(json!("phantom")), 50);
    store.truncate(10);
    assert_eq!(
        store.get("test/marks").as_deref(),
        Some(&json!({"marks":["durable"]}))
    );
    assert!(store.get("other").is_none());
}

#[test]
fn key_and_any_notifications_batch_and_dropped_applications_stay_silent() {
    let (store, scheduler) = store();
    let key_ticks = Rc::new(Cell::new(0));
    let any_ticks = Rc::new(Cell::new(0));
    let observed = key_ticks.clone();
    let _key_subscription = store
        .face_of("test/marks")
        .subscribe(Rc::new(move || observed.set(observed.get() + 1)));
    let observed = any_ticks.clone();
    let _any_subscription = store.subscribe_any(Rc::new(move || observed.set(observed.get() + 1)));

    store.apply("test/marks", value(json!({"marks":["a"]})), 5);
    store.apply("test/marks", value(json!({"marks":["b"]})), 6);
    scheduler.flush();
    assert_eq!(key_ticks.get(), 1);
    assert_eq!(any_ticks.get(), 1);

    store.apply("test/marks", value(json!({"marks":["replay"]})), 3);
    scheduler.flush();
    assert_eq!(key_ticks.get(), 1);
    assert_eq!(any_ticks.get(), 1);
}

#[test]
fn key_channels_publish_only_their_own_row_changes() {
    let (store, scheduler) = store();
    let marks_ticks = Rc::new(Cell::new(0));
    let title_ticks = Rc::new(Cell::new(0));
    let observed = marks_ticks.clone();
    let _marks = store
        .face_of("test/marks")
        .subscribe(Rc::new(move || observed.set(observed.get() + 1)));
    let observed = title_ticks.clone();
    let _title = store
        .face_of("title")
        .subscribe(Rc::new(move || observed.set(observed.get() + 1)));
    store.apply("title", value(json!("Projected title")), 4);
    scheduler.flush();
    assert_eq!(marks_ticks.get(), 0);
    assert_eq!(title_ticks.get(), 1);
}

#[test]
fn aggregate_values_are_ordered_and_reference_stable_until_a_row_changes() {
    let (store, _) = store();
    let empty = store.values();
    assert!(Rc::ptr_eq(&empty, &store.values()));
    store.apply("test/marks", value(json!({"marks":["a"]})), 1);
    store.apply("title", value(json!("Title")), 2);
    let populated = store.values();
    assert!(!Rc::ptr_eq(&empty, &populated));
    assert!(Rc::ptr_eq(&populated, &store.values()));
    assert_eq!(
        populated.keys().map(String::as_str).collect::<Vec<_>>(),
        ["test/marks", "title"]
    );
}
