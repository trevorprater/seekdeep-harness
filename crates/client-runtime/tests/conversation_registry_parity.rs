//! Conversation Definition/View registration, fallback, disposal, and rebuild batching parity.

use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
};

use seekdeep_client_runtime::*;

fn event(kind: &str) -> ConversationNodeDefinition<()> {
    ConversationNodeDefinition {
        kind: kind.to_owned(),
        target: Some("chat".to_owned()),
        has_view_builder: true,
        payload: (),
    }
}

fn view(target: &str) -> ConversationViewDefinition<()> {
    ConversationViewDefinition {
        target: target.to_owned(),
        payload: (),
    }
}

#[test]
fn event_registration_rejects_duplicates_keeps_stable_order_and_disposes_once() {
    let registry = ConversationEventRegistry::new();
    let first = registry.register(event("message")).unwrap();
    registry.register(event("tool")).unwrap();
    let entries = registry.entries();
    assert!(Rc::ptr_eq(&entries, &registry.entries()));
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.kind.as_str())
            .collect::<Vec<_>>(),
        ["message", "tool"]
    );
    assert!(registry.register(event("message")).is_err());
    first.dispose();
    first.dispose();
    assert_eq!(registry.entries().len(), 1);
    assert_eq!(registry.entries()[0].kind, "tool");
}

#[test]
fn fallback_is_unique_targeted_and_idempotently_disposed() {
    let registry = ConversationEventRegistry::new();
    let fallback = registry.register_fallback(event("unknown")).unwrap();
    assert_eq!(registry.fallback().unwrap().kind, "unknown");
    assert!(registry.register_fallback(event("other")).is_err());
    fallback.dispose();
    fallback.dispose();
    assert!(registry.fallback().is_none());

    let state_only = ConversationNodeDefinition {
        kind: "state-only".to_owned(),
        target: None,
        has_view_builder: false,
        payload: (),
    };
    assert!(
        registry
            .register_fallback(state_only)
            .unwrap_err()
            .to_string()
            .contains("must declare a target")
    );
}

#[test]
fn target_and_builder_must_be_declared_together() {
    let registry = ConversationEventRegistry::new();
    for (target, builder) in [(Some("chat".to_owned()), false), (None, true)] {
        let error = registry
            .register(ConversationNodeDefinition {
                kind: "drift".to_owned(),
                target,
                has_view_builder: builder,
                payload: (),
            })
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("target and buildViewNode together")
        );
    }
}

#[test]
fn view_targets_are_unique_and_dispose_once() {
    let registry = ConversationViewRegistry::new();
    let dispose = registry.register(view("chat")).unwrap();
    assert_eq!(registry.entries()[0].target, "chat");
    assert!(registry.register(view("chat")).is_err());
    dispose.dispose();
    dispose.dispose();
    assert!(registry.entries().is_empty());
}

#[test]
fn subscriptions_invalidate_synchronously_for_entries_fallback_and_views() {
    let events = ConversationEventRegistry::new();
    let views = ConversationViewRegistry::new();
    let event_calls = Rc::new(Cell::new(0));
    let view_calls = Rc::new(Cell::new(0));
    let observed_events = event_calls.clone();
    let observed_views = view_calls.clone();
    let _events = events.subscribe(Rc::new(move || {
        observed_events.set(observed_events.get() + 1);
    }));
    let _views = views.subscribe(Rc::new(move || {
        observed_views.set(observed_views.get() + 1);
    }));
    let event_registration = events.register(event("message")).unwrap();
    let fallback = events.register_fallback(event("unknown")).unwrap();
    let view = views.register(view("chat")).unwrap();
    assert_eq!(event_calls.get(), 2);
    assert_eq!(view_calls.get(), 1);
    event_registration.dispose();
    fallback.dispose();
    view.dispose();
    assert_eq!(event_calls.get(), 4);
    assert_eq!(view_calls.get(), 2);
}

#[derive(Default)]
struct ManualMicrotasks {
    queue: RefCell<VecDeque<Box<dyn FnOnce()>>>,
}

impl ConversationRegistryScheduler for ManualMicrotasks {
    fn queue(&self, callback: Box<dyn FnOnce()>) {
        self.queue.borrow_mut().push_back(callback);
    }
}

#[test]
fn event_and_view_changes_coalesce_into_one_resident_rebuild() {
    let events = ConversationEventRegistry::new();
    let views = ConversationViewRegistry::new();
    let scheduler = Rc::new(ManualMicrotasks::default());
    let rebuilds = Rc::new(Cell::new(0));
    let observed = rebuilds.clone();
    let scheduler_face: Rc<dyn ConversationRegistryScheduler> = scheduler.clone();
    let rebuild: Rc<dyn Fn()> = Rc::new(move || observed.set(observed.get() + 1));
    let _coordinator =
        ConversationRegistryCoordinator::new(&events, &views, &scheduler_face, &rebuild);
    events.register(event("message")).unwrap();
    views.register(view("chat")).unwrap();
    assert_eq!(rebuilds.get(), 0);
    scheduler.queue.borrow_mut().pop_front().unwrap()();
    assert_eq!(rebuilds.get(), 1);
}
