//! Slot declaration, registration, shadowing, lifecycle, and notification parity.

use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
};

use seekdeep_client_ui_slots::*;

type Core = SlotCore<String, String, String>;

#[derive(Default)]
struct ManualMicrotasks {
    queued: RefCell<VecDeque<Box<dyn FnOnce()>>>,
}

impl SlotMicrotaskScheduler for ManualMicrotasks {
    fn queue(&self, callback: Box<dyn FnOnce()>) {
        self.queued.borrow_mut().push_back(callback);
    }
}

impl ManualMicrotasks {
    fn flush_one(&self) -> bool {
        let Some(callback) = self.queued.borrow_mut().pop_front() else {
            return false;
        };
        callback();
        true
    }

    fn flush_all(&self) {
        while self.flush_one() {}
    }
}

fn harness() -> (Rc<Core>, Rc<ManualMicrotasks>) {
    let scheduler = Rc::new(ManualMicrotasks::default());
    (SlotCore::new(scheduler.clone()), scheduler)
}

fn spec(kind: SlotKind, scope: SlotScope) -> SlotSpec<String> {
    SlotSpec::new(kind, scope)
}

fn mount_frame(core: &Rc<Core>) -> SlotRegistration<String, String, String> {
    let mut options = SlotRegistrationOptions::new("root");
    options.children = [
        (
            SlotName::new("test.single"),
            spec(SlotKind::Single, SlotScope::Root),
        ),
        (
            SlotName::new("test.session"),
            spec(SlotKind::Single, SlotScope::Session),
        ),
        (
            SlotName::new("test.list"),
            spec(SlotKind::List, SlotScope::Root),
        ),
        (
            SlotName::new("test.keyed"),
            spec(SlotKind::Keyed, SlotScope::Session),
        ),
        (
            SlotName::new("test.chain"),
            spec(SlotKind::Chain, SlotScope::Session),
        ),
    ]
    .into_iter()
    .collect();
    core.register(options, "frame".to_owned()).unwrap()
}

fn register(
    core: &Rc<Core>,
    name: &str,
    payload: &str,
) -> SlotRegistration<String, String, String> {
    core.register(SlotRegistrationOptions::new(name), payload.to_owned())
        .unwrap()
}

#[test]
fn root_is_the_only_apriori_single_root_declaration() {
    let (core, _) = harness();
    assert_eq!(
        core.spec(&SlotName::new("root")),
        Some(spec(SlotKind::Single, SlotScope::Root))
    );
    assert_eq!(core.spec(&SlotName::new("test.single")), None);
    assert_eq!(core.declaration_epoch(&SlotName::new("root")), 1);
}

#[test]
fn undeclared_target_fails_and_root_rejects_a_same_priority_second_frame() {
    let (core, _) = harness();
    let error = core
        .register(SlotRegistrationOptions::new("test.single"), "x".to_owned())
        .unwrap_err();
    assert!(error.to_string().contains("not declared"));
    mount_frame(&core);
    let error = core
        .register(SlotRegistrationOptions::new("root"), "other".to_owned())
        .unwrap_err();
    assert!(error.to_string().contains("already has a registration"));
}

#[test]
fn children_commit_specs_and_duplicate_declaration_names_the_first_owner() {
    let (core, _) = harness();
    mount_frame(&core);
    assert_eq!(
        core.spec(&SlotName::new("test.session")),
        Some(spec(SlotKind::Single, SlotScope::Session))
    );
    let mut child = SlotRegistrationOptions::new("test.single");
    child.children.insert(
        SlotName::new("test.grandchild"),
        spec(SlotKind::Single, SlotScope::Root),
    );
    core.register(child, "child".to_owned()).unwrap();
    let mut imposter = SlotRegistrationOptions::new("test.session");
    imposter.registrant = Some("imposter".to_owned());
    imposter.children.insert(
        SlotName::new("test.grandchild"),
        spec(SlotKind::Single, SlotScope::Root),
    );
    let error = core.register(imposter, "bad".to_owned()).unwrap_err();
    assert!(error.to_string().contains("already declared"));
    assert!(error.to_string().contains("test.single"));
}

#[test]
fn declaration_cascade_removes_descendants_and_stale_disposers_are_noops() {
    let (core, _) = harness();
    let frame = mount_frame(&core);
    let mut child = SlotRegistrationOptions::new("test.single");
    child.children.insert(
        SlotName::new("test.grandchild"),
        spec(SlotKind::Single, SlotScope::Root),
    );
    let child = core.register(child, "child".to_owned()).unwrap();
    register(&core, "test.grandchild", "leaf");
    frame.dispose();
    assert_eq!(core.spec(&SlotName::new("test.single")), None);
    assert_eq!(core.spec(&SlotName::new("test.grandchild")), None);
    assert!(core.entries(&SlotName::new("test.single")).is_empty());
    assert!(core.entries(&SlotName::new("test.grandchild")).is_empty());
    child.dispose();
    assert!(
        core.register(
            SlotRegistrationOptions::new("test.single"),
            "late".to_owned(),
        )
        .is_err()
    );
}

#[test]
fn registration_disposal_is_idempotent_and_is_live_tracks_membership() {
    let (core, _) = harness();
    mount_frame(&core);
    let registration = register(&core, "test.single", "entry");
    let entry = core.entries(&SlotName::new("test.single"))[0].clone();
    assert!(core.is_live(&entry));
    registration.dispose();
    registration.dispose();
    assert!(!core.is_live(&entry));
}

#[test]
fn keyed_requires_a_key_rejects_exact_cell_priority_and_accepts_another_cell() {
    let (core, _) = harness();
    mount_frame(&core);
    let missing = core
        .register(
            SlotRegistrationOptions::new("test.keyed"),
            "missing".to_owned(),
        )
        .unwrap_err();
    assert!(missing.to_string().contains("requires options.key"));
    let mut first = SlotRegistrationOptions::new("test.keyed");
    first.key = Some("a".to_owned());
    core.register(first.clone(), "a".to_owned()).unwrap();
    assert!(core.register(first, "duplicate".to_owned()).is_err());
    let mut second = SlotRegistrationOptions::new("test.keyed");
    second.key = Some("b".to_owned());
    core.register(second, "b".to_owned()).unwrap();
}

#[test]
fn list_requires_unique_cell_priority_and_sorts_by_priority_order_then_sequence() {
    let (core, _) = harness();
    mount_frame(&core);
    let missing = core
        .register(
            SlotRegistrationOptions::new("test.list"),
            "missing".to_owned(),
        )
        .unwrap_err();
    assert!(missing.to_string().contains("requires options.id"));
    for (id, order) in [("c", Some(10.0)), ("a", None), ("b", None)] {
        let mut options = SlotRegistrationOptions::new("test.list");
        options.id = Some(id.to_owned());
        options.order = order;
        core.register(options, id.to_owned()).unwrap();
    }
    assert_eq!(
        core.entries(&SlotName::new("test.list"))
            .iter()
            .map(|entry| entry.options.id.as_deref().unwrap())
            .collect::<Vec<_>>(),
        ["a", "b", "c"]
    );
}

#[test]
fn chain_requires_selector_and_sorts_priorities_stably() {
    let (core, _) = harness();
    mount_frame(&core);
    assert!(
        core.register(
            SlotRegistrationOptions::new("test.chain"),
            "missing".to_owned(),
        )
        .unwrap_err()
        .to_string()
        .contains("requires options.select")
    );
    for (name, priority) in [
        ("late", Some(10.0)),
        ("default-a", None),
        ("default-b", None),
        ("first", Some(-1.0)),
    ] {
        let mut options = SlotRegistrationOptions::new("test.chain");
        options.has_selector = true;
        options.priority = priority;
        options.registrant = Some(name.to_owned());
        core.register(options, name.to_owned()).unwrap();
    }
    assert_eq!(
        core.entries(&SlotName::new("test.chain"))
            .iter()
            .map(|entry| entry.options.registrant.as_deref().unwrap())
            .collect::<Vec<_>>(),
        ["first", "default-a", "default-b", "late"]
    );
}

#[test]
fn shadowing_accepts_distinct_priorities_and_abdication_promotes_next_survivor() {
    let (core, scheduler) = harness();
    mount_frame(&core);
    let low = register(&core, "test.single", "low");
    let mut high_options = SlotRegistrationOptions::new("test.single");
    high_options.priority = Some(10.0);
    core.register(high_options, "high".to_owned()).unwrap();
    let raw = core.entries(&SlotName::new("test.single"));
    assert_eq!(raw.len(), 2);
    assert_eq!(
        core.entries_of_slot(&SlotName::new("test.single"))[0].payload,
        "low"
    );
    let first = raw[0].clone();
    core.report_entry_error(
        &SlotName::new("test.single"),
        &first,
        &"crash".to_owned(),
        true,
    );
    assert_eq!(
        core.entries_of_slot(&SlotName::new("test.single"))[0].payload,
        "high"
    );
    scheduler.flush_all();
    low.dispose();
}

#[test]
fn shared_store_handles_pin_one_scope_release_on_full_unmount_and_factories_do_not_pin() {
    let (core, _) = harness();
    mount_frame(&core);
    let handle = StoreHandleId::new(7);
    let mut first = SlotRegistrationOptions::new("test.list");
    first.id = Some("x".to_owned());
    first.store = Some(SlotStoreDeclaration::Shared(handle));
    let first = core.register(first, "x".to_owned()).unwrap();
    let mut second = SlotRegistrationOptions::new("test.list");
    second.id = Some("y".to_owned());
    second.store = Some(SlotStoreDeclaration::Shared(handle));
    let second = core.register(second, "y".to_owned()).unwrap();
    let mut wrong = SlotRegistrationOptions::new("test.session");
    wrong.store = Some(SlotStoreDeclaration::Shared(handle));
    assert!(core.register(wrong.clone(), "bad".to_owned()).is_err());
    first.dispose();
    assert!(
        core.register(wrong.clone(), "still-bad".to_owned())
            .is_err()
    );
    second.dispose();
    core.register(wrong, "rebound".to_owned()).unwrap();

    let mut factory_root = SlotRegistrationOptions::new("test.single");
    factory_root.store = Some(SlotStoreDeclaration::Factory);
    core.register(factory_root, "factory-root".to_owned())
        .unwrap();
}

#[test]
fn cascade_releases_shared_store_scope_pins() {
    let (core, _) = harness();
    let frame = mount_frame(&core);
    let handle = StoreHandleId::new(9);
    let mut session = SlotRegistrationOptions::new("test.session");
    session.store = Some(SlotStoreDeclaration::Shared(handle));
    core.register(session, "session".to_owned()).unwrap();
    frame.dispose();
    mount_frame(&core);
    let mut root = SlotRegistrationOptions::new("test.single");
    root.store = Some(SlotStoreDeclaration::Shared(handle));
    core.register(root, "root".to_owned()).unwrap();
}

#[test]
fn epochs_and_versions_are_separate_and_monotonic_across_redeclaration() {
    let (core, _) = harness();
    let key = SlotName::new("test.list");
    assert_eq!(core.declaration_epoch(&key), 0);
    assert_eq!(core.version(&key), 0);
    let frame = mount_frame(&core);
    let declared = core.declaration_epoch(&key);
    let version = core.version(&key);
    let mut options = SlotRegistrationOptions::new("test.list");
    options.id = Some("a".to_owned());
    let entry = core.register(options, "a".to_owned()).unwrap();
    entry.dispose();
    assert_eq!(core.declaration_epoch(&key), declared);
    assert_eq!(core.version(&key), version + 2);
    frame.dispose();
    let collapsed = core.declaration_epoch(&key);
    mount_frame(&core);
    assert!(core.declaration_epoch(&key) > collapsed);
}

#[test]
fn raw_entry_snapshot_reference_is_stable_between_mutations() {
    let (core, _) = harness();
    mount_frame(&core);
    let key = SlotName::new("test.list");
    let untouched_a = core.entries(&SlotName::new("never"));
    let untouched_b = core.entries(&SlotName::new("other"));
    assert!(Rc::ptr_eq(&untouched_a, &untouched_b));
    let mut first = SlotRegistrationOptions::new("test.list");
    first.id = Some("a".to_owned());
    core.register(first, "a".to_owned()).unwrap();
    let snapshot = core.entries(&key);
    assert!(Rc::ptr_eq(&snapshot, &core.entries(&key)));
    let mut second = SlotRegistrationOptions::new("test.list");
    second.id = Some("b".to_owned());
    core.register(second, "b".to_owned()).unwrap();
    assert!(!Rc::ptr_eq(&snapshot, &core.entries(&key)));
}

#[test]
fn versions_bump_synchronously_and_notifications_batch_per_microtask() {
    let (core, scheduler) = harness();
    mount_frame(&core);
    scheduler.flush_all();
    let key = SlotName::new("test.list");
    let calls = Rc::new(Cell::new(0));
    let observed = calls.clone();
    let _subscription = core.subscribe(
        key.clone(),
        Rc::new(move || observed.set(observed.get() + 1)),
    );
    let before = core.version(&key);
    for id in ["a", "b"] {
        let mut options = SlotRegistrationOptions::new("test.list");
        options.id = Some(id.to_owned());
        core.register(options, id.to_owned()).unwrap();
    }
    assert_eq!(core.version(&key), before + 2);
    assert_eq!(calls.get(), 0);
    assert!(scheduler.flush_one());
    assert_eq!(calls.get(), 1);
}

#[test]
fn declaration_subscribers_are_synchronous_exclude_entries_and_commit_siblings_first() {
    let (core, _) = harness();
    let calls = Rc::new(Cell::new(0));
    let duplicate = Rc::new(RefCell::new(None::<String>));
    let observed_calls = calls.clone();
    let observed_duplicate = duplicate.clone();
    let listener_core = core.clone();
    let subscription = core.subscribe_declaration(
        SlotName::new("test.single"),
        Rc::new(move || {
            observed_calls.set(observed_calls.get() + 1);
            let mut list = SlotRegistrationOptions::new("test.list");
            list.id = Some("from-listener".to_owned());
            listener_core.register(list, "listener".to_owned()).unwrap();
            let mut duplicate_options = SlotRegistrationOptions::new("test.single");
            duplicate_options.children.insert(
                SlotName::new("test.list"),
                spec(SlotKind::List, SlotScope::Root),
            );
            *observed_duplicate.borrow_mut() = listener_core
                .register(duplicate_options, "duplicate".to_owned())
                .err()
                .map(|error| error.to_string());
        }),
    );
    let frame = mount_frame(&core);
    assert_eq!(calls.get(), 1);
    assert_eq!(core.entries(&SlotName::new("test.list")).len(), 1);
    assert!(
        duplicate
            .borrow()
            .as_deref()
            .unwrap()
            .contains("already declared")
    );
    subscription.dispose();
    frame.dispose();
    assert_eq!(calls.get(), 1);
}

#[test]
fn touched_key_delivery_is_isolated_and_unsubscribe_stops_it() {
    let (core, scheduler) = harness();
    mount_frame(&core);
    scheduler.flush_all();
    let single_calls = Rc::new(Cell::new(0));
    let list_calls = Rc::new(Cell::new(0));
    let single_observed = single_calls.clone();
    let list_observed = list_calls.clone();
    let _single = core.subscribe(
        SlotName::new("test.single"),
        Rc::new(move || single_observed.set(single_observed.get() + 1)),
    );
    let list = core.subscribe(
        SlotName::new("test.list"),
        Rc::new(move || list_observed.set(list_observed.get() + 1)),
    );
    register(&core, "test.single", "single");
    scheduler.flush_all();
    assert_eq!(single_calls.get(), 1);
    assert_eq!(list_calls.get(), 0);
    list.dispose();
    let mut options = SlotRegistrationOptions::new("test.list");
    options.id = Some("a".to_owned());
    core.register(options, "a".to_owned()).unwrap();
    scheduler.flush_all();
    assert_eq!(list_calls.get(), 0);
}

#[test]
fn mutation_during_flush_reschedules_without_losing_delivery() {
    let (core, scheduler) = harness();
    mount_frame(&core);
    scheduler.flush_all();
    let key = SlotName::new("test.list");
    let seen = Rc::new(RefCell::new(Vec::new()));
    let reentered = Rc::new(Cell::new(false));
    let seen_listener = seen.clone();
    let reentered_listener = reentered.clone();
    let listener_core = core.clone();
    let listener_key = key.clone();
    let _subscription = core.subscribe(
        key.clone(),
        Rc::new(move || {
            seen_listener
                .borrow_mut()
                .push(listener_core.version(&listener_key));
            if !reentered_listener.replace(true) {
                let mut options = SlotRegistrationOptions::new("test.list");
                options.id = Some("reentrant".to_owned());
                listener_core
                    .register(options, "reentrant".to_owned())
                    .unwrap();
            }
        }),
    );
    let mut options = SlotRegistrationOptions::new("test.list");
    options.id = Some("a".to_owned());
    core.register(options, "a".to_owned()).unwrap();
    assert!(scheduler.flush_one());
    assert_eq!(seen.borrow().len(), 1);
    assert!(scheduler.flush_one());
    assert_eq!(seen.borrow().len(), 2);
}

#[test]
fn mutation_observers_fire_synchronously_per_touched_key() {
    let (core, _) = harness();
    let keys = Rc::new(RefCell::new(Vec::new()));
    let observed = keys.clone();
    let subscription = core.on_mutate(Rc::new(move |key| {
        observed.borrow_mut().push(key.to_string());
    }));
    mount_frame(&core);
    assert_eq!(
        keys.borrow().as_slice(),
        [
            "root",
            "test.single",
            "test.session",
            "test.list",
            "test.keyed",
            "test.chain"
        ]
    );
    subscription.dispose();
    register(&core, "test.single", "entry");
    assert_eq!(keys.borrow().len(), 6);
}

#[test]
fn entry_failure_observer_preserves_error_and_abdication_is_one_shot() {
    let (core, scheduler) = harness();
    mount_frame(&core);
    register(&core, "test.single", "entry");
    let entry = core.entries(&SlotName::new("test.single"))[0].clone();
    let errors = Rc::new(RefCell::new(Vec::new()));
    let observed = errors.clone();
    let _subscription = core.on_entry_error(Rc::new(move |key, entry, error, abdicated| {
        observed.borrow_mut().push((
            key.to_string(),
            entry.payload.clone(),
            error.clone(),
            abdicated,
        ));
    }));
    core.report_entry_error(
        &SlotName::new("test.single"),
        &entry,
        &"boom".to_owned(),
        true,
    );
    core.report_entry_error(
        &SlotName::new("test.single"),
        &entry,
        &"again".to_owned(),
        true,
    );
    scheduler.flush_all();
    assert_eq!(errors.borrow().len(), 1);
    assert_eq!(errors.borrow()[0].2, "boom");
    assert!(errors.borrow()[0].3);
}

#[test]
fn live_snapshot_preserves_tree_order_occupants_and_active_shadowing() {
    let (core, _) = harness();
    mount_frame(&core);
    let mut low = SlotRegistrationOptions::new("test.list");
    low.id = Some("cell".to_owned());
    low.registrant = Some("low".to_owned());
    core.register(low, "low".to_owned()).unwrap();
    let mut high = SlotRegistrationOptions::new("test.list");
    high.id = Some("cell".to_owned());
    high.priority = Some(5.0);
    high.registrant = Some("high".to_owned());
    core.register(high, "high".to_owned()).unwrap();
    let root = core.snapshot(Some(&SlotName::new("root")));
    assert_eq!(root.len(), 1);
    let list = root[0]
        .children
        .iter()
        .find(|child| child.name.as_str() == "test.list")
        .unwrap();
    assert_eq!(list.occupants.len(), 2);
    assert!(list.occupants[0].active);
    assert!(!list.occupants[1].active);
    assert_eq!(list.declared_by.as_deref(), Some("an entry in \"root\""));
}

#[test]
fn parent_declared_inject_and_dynamic_untouched_key_behavior_are_preserved() {
    let (core, _) = harness();
    let untouched_a = core.entries(&SlotName::new("dynamic.a"));
    let untouched_b = core.entries(&SlotName::new("dynamic.b"));
    assert!(Rc::ptr_eq(&untouched_a, &untouched_b));
    assert_eq!(core.version(&SlotName::new("dynamic.a")), 0);
    let mut root = SlotRegistrationOptions::new("root");
    root.children.insert(
        SlotName::new("surface.injected"),
        SlotSpec {
            kind: SlotKind::Single,
            scope: SlotScope::Root,
            inject: Some("shared".to_owned()),
        },
    );
    core.register(root, "root".to_owned()).unwrap();
    assert_eq!(
        core.spec(&SlotName::new("surface.injected"))
            .unwrap()
            .inject
            .as_deref(),
        Some("shared")
    );
}

#[test]
fn typed_builders_pin_kind_scope_and_mandatory_registration_fields() {
    let keyed = TypedSlot::<KeyedSlot>::new("typed.keyed", SlotScope::Session);
    let list = TypedSlot::<ListSlot>::new("typed.list", SlotScope::Root);
    let chain = TypedSlot::<ChainSlot>::new("typed.chain", SlotScope::Session);
    let frame = TypedSlot::<SingleSlot>::new("typed.frame", SlotScope::Root);
    let options = frame
        .registration::<String>()
        .child(&keyed, keyed.spec(None))
        .child(&list, list.spec(None))
        .child(&chain, chain.spec(None))
        .into_options();
    assert_eq!(
        options.children[&SlotName::new("typed.keyed")].kind,
        SlotKind::Keyed
    );
    assert_eq!(
        keyed
            .registration::<String>("bash")
            .into_options()
            .key
            .as_deref(),
        Some("bash")
    );
    assert_eq!(
        list.registration::<String>("nav")
            .list_order(10.0)
            .into_options()
            .id
            .as_deref(),
        Some("nav")
    );
    let (chain_options, selector) = chain
        .registration::<String, _>(|value: &str| (!value.is_empty()).then_some(value.len()))
        .priority(1.0)
        .into_parts();
    assert!(chain_options.has_selector);
    assert_eq!(selector("item"), Some(4));
}
