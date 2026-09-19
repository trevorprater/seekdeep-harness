//! Notification batching, laziness, freshness, and synchronous echo parity.

use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
};

use seekdeep_client_runtime::*;

#[derive(Default)]
struct ManualScheduler {
    frames: RefCell<VecDeque<Box<dyn FnOnce()>>>,
    microtasks: RefCell<VecDeque<Box<dyn FnOnce()>>>,
    frame_available: Cell<bool>,
}

impl NotifierScheduler for ManualScheduler {
    fn has_animation_frame(&self) -> bool {
        self.frame_available.get()
    }

    fn queue_microtask(&self, callback: Box<dyn FnOnce()>) {
        self.microtasks.borrow_mut().push_back(callback);
    }

    fn queue_animation_frame(&self, callback: Box<dyn FnOnce()>) {
        self.frames.borrow_mut().push_back(callback);
    }
}

impl ManualScheduler {
    fn microtask(&self) {
        self.microtasks.borrow_mut().pop_front().unwrap()();
    }

    fn frame(&self) {
        self.frames.borrow_mut().pop_front().unwrap()();
    }
}

#[test]
fn mark_dirty_coalesces_and_rebuilds_before_notifying() {
    let scheduler = Rc::new(ManualScheduler::default());
    let order = Rc::new(RefCell::new(Vec::new()));
    let rebuilt = order.clone();
    let notifier = Notifier::new(
        Rc::new(move || rebuilt.borrow_mut().push("rebuild")),
        scheduler.clone(),
    );
    let notification_order = order.clone();
    let _subscription = notifier.subscribe(Rc::new(move || {
        notification_order.borrow_mut().push("notify");
    }));
    notifier.mark_dirty();
    notifier.mark_dirty();
    notifier.mark_dirty();
    assert!(order.borrow().is_empty());
    scheduler.microtask();
    assert_eq!(order.borrow().as_slice(), ["rebuild", "notify"]);
}

#[test]
fn no_listener_stays_lazy_and_pull_does_not_consume_pending_notification() {
    let scheduler = Rc::new(ManualScheduler::default());
    let rebuilds = Rc::new(Cell::new(0));
    let observed = rebuilds.clone();
    let notifier = Notifier::new(
        Rc::new(move || observed.set(observed.get() + 1)),
        scheduler.clone(),
    );
    notifier.mark_dirty();
    notifier.ensure_fresh();
    assert_eq!(rebuilds.get(), 1);
    notifier.ensure_fresh();
    assert_eq!(rebuilds.get(), 1);
    let notifications = Rc::new(Cell::new(0));
    let observed = notifications.clone();
    let _subscription = notifier.subscribe(Rc::new(move || observed.set(observed.get() + 1)));
    scheduler.microtask();
    assert_eq!(rebuilds.get(), 1);
    assert_eq!(notifications.get(), 1);
}

#[test]
fn notify_now_is_synchronous_lazy_without_listeners_and_invalidates_older_schedule() {
    let scheduler = Rc::new(ManualScheduler::default());
    let order = Rc::new(RefCell::new(Vec::new()));
    let rebuilt = order.clone();
    let notifier = Notifier::new(
        Rc::new(move || rebuilt.borrow_mut().push("rebuild")),
        scheduler.clone(),
    );
    notifier.notify_now();
    assert!(order.borrow().is_empty());
    notifier.ensure_fresh();
    assert_eq!(order.borrow().as_slice(), ["rebuild"]);
    let notification_order = order.clone();
    let _subscription = notifier.subscribe(Rc::new(move || {
        notification_order.borrow_mut().push("notify");
    }));
    notifier.mark_dirty();
    notifier.notify_now();
    assert_eq!(order.borrow().as_slice(), ["rebuild", "rebuild", "notify"]);
    scheduler.microtask();
    assert_eq!(order.borrow().len(), 3);
}

#[test]
fn frame_changes_coalesce_structural_microtask_supersedes_frame_and_fallback_works() {
    let scheduler = Rc::new(ManualScheduler::default());
    scheduler.frame_available.set(true);
    let notifications = Rc::new(Cell::new(0));
    let notifier = Notifier::new(Rc::new(|| {}), scheduler.clone());
    let observed = notifications.clone();
    let _subscription = notifier.subscribe(Rc::new(move || observed.set(observed.get() + 1)));
    notifier.mark_frame_dirty();
    notifier.mark_frame_dirty();
    assert_eq!(scheduler.frames.borrow().len(), 1);
    notifier.mark_dirty();
    scheduler.microtask();
    assert_eq!(notifications.get(), 1);
    scheduler.frame();
    assert_eq!(notifications.get(), 1);

    scheduler.frame_available.set(false);
    notifier.mark_frame_dirty();
    notifier.mark_frame_dirty();
    scheduler.microtask();
    assert_eq!(notifications.get(), 2);
}

#[test]
fn unsubscribe_stops_future_delivery() {
    let scheduler = Rc::new(ManualScheduler::default());
    let calls = Rc::new(Cell::new(0));
    let notifier = Notifier::new(Rc::new(|| {}), scheduler);
    let observed = calls.clone();
    let subscription = notifier.subscribe(Rc::new(move || observed.set(observed.get() + 1)));
    notifier.notify_now();
    subscription.dispose();
    notifier.notify_now();
    assert_eq!(calls.get(), 1);
}
