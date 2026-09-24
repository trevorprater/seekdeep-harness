//! Declaration injection, renderer installation, Store axes, and event-bridge parity.

use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
};

use seekdeep_client_runtime::*;
use seekdeep_client_ui_slots::*;
use serde_json::json;

type Registry = ClientSlotRegistry<String, String, String, String, String>;

#[derive(Default)]
struct ManualMicrotasks {
    queue: RefCell<VecDeque<Box<dyn FnOnce()>>>,
}

impl SlotMicrotaskScheduler for ManualMicrotasks {
    fn queue(&self, callback: Box<dyn FnOnce()>) {
        self.queue.borrow_mut().push_back(callback);
    }
}

impl ManualMicrotasks {
    fn flush_all(&self) {
        loop {
            let callback = self.queue.borrow_mut().pop_front();
            let Some(callback) = callback else {
                return;
            };
            callback();
        }
    }
}

#[derive(Default)]
struct Reporter {
    errors: RefCell<Vec<ClientSlotError>>,
}

impl SlotInjectionFailureReporter for Reporter {
    fn report_later(&self, error: ClientSlotError) {
        self.errors.borrow_mut().push(error);
    }
}

#[derive(Clone)]
struct JsonFace(serde_json::Value);

impl RuntimeStandardFace for JsonFace {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn snapshot(&self) -> serde_json::Value {
        self.0.clone()
    }
}

struct Locale(u64);

impl RuntimeLocaleFace for Locale {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn revision(&self) -> u64 {
        self.0
    }
}

#[derive(Default)]
struct FakeInstance {
    clear_count: Cell<usize>,
}

impl SlotStoreInstance for FakeInstance {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn snapshot(&self) -> serde_json::Value {
        serde_json::Value::Null
    }

    fn subscribe(&self, _listener: Rc<dyn Fn()>) -> Box<dyn Fn()> {
        Box::new(|| {})
    }

    fn clear_persisted(&self) {
        self.clear_count.set(self.clear_count.get() + 1);
    }
}

#[derive(Default)]
struct FakeHandle {
    keys: RefCell<Vec<Option<String>>>,
    instances: RefCell<Vec<Rc<FakeInstance>>>,
}

impl SlotStoreFactory for FakeHandle {
    fn create(&self, scope_key: Option<&str>) -> Rc<dyn SlotStoreInstance> {
        self.keys.borrow_mut().push(scope_key.map(str::to_owned));
        let instance = Rc::new(FakeInstance::default());
        self.instances.borrow_mut().push(instance.clone());
        instance
    }
}

struct Renderer {
    calls: Cell<usize>,
}

impl ClientRootRenderer<String, String, String, String, String> for Renderer {
    fn render_root(&self, host: &Registry, owner: String) -> String {
        self.calls.set(self.calls.get() + 1);
        assert_eq!(
            host.sessions().unwrap().snapshot(),
            json!({"ids": [], "current": null})
        );
        assert_eq!(
            host.workspaces().unwrap().snapshot(),
            json!({"items": [], "phase": "ready"})
        );
        format!("tree:{owner}")
    }
}

fn bench() -> (Rc<Registry>, Rc<ManualMicrotasks>, Rc<RefCell<Vec<String>>>) {
    let scheduler = Rc::new(ManualMicrotasks::default());
    let changed = Rc::new(RefCell::new(Vec::new()));
    let observed = changed.clone();
    let registry = ClientSlotRegistry::new(
        scheduler.clone(),
        Rc::new(move |key| observed.borrow_mut().push(key.to_string())),
    );
    (registry, scheduler, changed)
}

fn child_spec(kind: SlotKind, scope: SlotScope) -> SlotSpec<String> {
    SlotSpec::new(kind, scope)
}

fn frame_options() -> SlotRegistrationOptions<String> {
    let mut options = SlotRegistrationOptions::new("root");
    options.children.insert(
        SlotName::new("t.host"),
        child_spec(SlotKind::Single, SlotScope::Root),
    );
    options.children.insert(
        SlotName::new("t.rows"),
        child_spec(SlotKind::List, SlotScope::Root),
    );
    options.children.insert(
        SlotName::new("t.panel"),
        child_spec(SlotKind::Single, SlotScope::Session),
    );
    options
}

fn mount_frame(registry: &Rc<Registry>) -> RuntimeDisposer {
    registry
        .register(frame_options(), "frame".to_owned(), None)
        .unwrap()
}

fn mount_host_frame(registry: &Rc<Registry>) -> RuntimeDisposer {
    let mut options = SlotRegistrationOptions::new("root");
    options.children.insert(
        SlotName::new("t.host"),
        child_spec(SlotKind::Single, SlotScope::Root),
    );
    registry
        .register(options, "host-frame".to_owned(), None)
        .unwrap()
}

fn register_host(registry: &Rc<Registry>, component: &str) -> RuntimeDisposer {
    registry
        .register(
            SlotRegistrationOptions::new("t.host"),
            component.to_owned(),
            None,
        )
        .unwrap()
}

#[test]
fn registration_delegates_validation_and_changed_events_commit_per_mutation() {
    let (registry, _, changed) = bench();
    assert_eq!(
        registry.core().spec(&SlotName::new("root")).unwrap().kind,
        SlotKind::Single
    );
    assert!(
        registry
            .register(
                SlotRegistrationOptions::new("t.host"),
                "early".to_owned(),
                None,
            )
            .unwrap_err()
            .to_string()
            .contains("not declared")
    );
    mount_frame(&registry);
    let mut row = SlotRegistrationOptions::new("t.rows");
    row.id = Some("a".to_owned());
    registry.register(row, "row".to_owned(), None).unwrap();
    assert_eq!(
        changed.borrow().as_slice(),
        ["root", "t.host", "t.rows", "t.panel", "t.rows"]
    );
}

#[test]
fn injection_activates_immediately_ignores_entries_and_stops_cleanly() {
    let (registry, scheduler, _) = bench();
    mount_frame(&registry);
    scheduler.flush_all();
    let setups = Rc::new(Cell::new(0));
    let observed = setups.clone();
    let setup_registry = registry.clone();
    let reporter = Rc::new(Reporter::default());
    let injection = registry
        .inject(
            SlotName::new("t.rows"),
            Rc::new(move |batch| {
                observed.set(observed.get() + 1);
                let mut options = SlotRegistrationOptions::new("t.rows");
                options.id = Some("injected".to_owned());
                batch.push(setup_registry.register(options, "injected".to_owned(), None)?);
                Ok(())
            }),
            reporter,
        )
        .unwrap();
    assert_eq!(setups.get(), 1);
    let mut ordinary = SlotRegistrationOptions::new("t.rows");
    ordinary.id = Some("ordinary".to_owned());
    registry
        .register(ordinary, "ordinary".to_owned(), None)
        .unwrap();
    scheduler.flush_all();
    assert_eq!(setups.get(), 1);
    injection.dispose();
    assert_eq!(
        registry
            .core()
            .entries(&SlotName::new("t.rows"))
            .iter()
            .map(|entry| entry.options.id.as_deref().unwrap())
            .collect::<Vec<_>>(),
        ["ordinary"]
    );
}

#[test]
fn injection_waits_cleans_on_collapse_and_reactivates_on_redeclaration() {
    let (registry, _, _) = bench();
    let setups = Rc::new(Cell::new(0));
    let cleanups = Rc::new(Cell::new(0));
    let setup_registry = registry.clone();
    let observed_setups = setups.clone();
    let observed_cleanups = cleanups.clone();
    let injection = registry
        .inject(
            SlotName::new("t.host"),
            Rc::new(move |batch| {
                observed_setups.set(observed_setups.get() + 1);
                let registration = register_host(&setup_registry, "injected");
                let cleanup_count = observed_cleanups.clone();
                batch.push(RuntimeDisposer::new(move || {
                    registration.dispose();
                    cleanup_count.set(cleanup_count.get() + 1);
                }));
                Ok(())
            }),
            Rc::new(Reporter::default()),
        )
        .unwrap();
    assert_eq!(setups.get(), 0);
    let frame = mount_frame(&registry);
    assert_eq!(setups.get(), 1);
    frame.dispose();
    assert_eq!(cleanups.get(), 1);
    assert!(registry.core().entries(&SlotName::new("t.host")).is_empty());
    mount_frame(&registry);
    assert_eq!(setups.get(), 2);
    injection.dispose();
}

#[test]
fn same_tick_collapse_redeclaration_and_nested_cleanup_keep_latest_activation() {
    let (registry, _, _) = bench();
    let first_frame = mount_host_frame(&registry);
    let setups = Rc::new(Cell::new(0));
    let replaced = Rc::new(Cell::new(false));
    let replacement = Rc::new(RefCell::new(None::<RuntimeDisposer>));
    let setup_registry = registry.clone();
    let setup_count = setups.clone();
    let replaced_flag = replaced.clone();
    let replacement_handle = replacement.clone();
    let _injection = registry
        .inject(
            SlotName::new("t.host"),
            Rc::new(move |batch| {
                setup_count.set(setup_count.get() + 1);
                let registry = setup_registry.clone();
                let replaced = replaced_flag.clone();
                let replacement = replacement_handle.clone();
                batch.push(RuntimeDisposer::new(move || {
                    if !replaced.replace(true) {
                        *replacement.borrow_mut() = Some(mount_host_frame(&registry));
                    }
                }));
                Ok(())
            }),
            Rc::new(Reporter::default()),
        )
        .unwrap();
    first_frame.dispose();
    assert_eq!(setups.get(), 2);
    assert!(registry.core().spec(&SlotName::new("t.host")).is_some());
    replacement.borrow().as_ref().unwrap().dispose();
}

#[test]
fn transactional_setup_rolls_back_and_delayed_failure_does_not_block_later_listener() {
    let (registry, _, _) = bench();
    let reporter = Rc::new(Reporter::default());
    let failed_registry = registry.clone();
    let _failed = registry
        .inject(
            SlotName::new("t.host"),
            Rc::new(move |batch| {
                batch.push(RuntimeDisposer::new({
                    let registry = failed_registry.clone();
                    move || {
                        let _ = &registry;
                    }
                }));
                Err(ClientSlotError::new("null"))
            }),
            reporter.clone(),
        )
        .unwrap();
    let later_calls = Rc::new(Cell::new(0));
    let observed = later_calls.clone();
    let _later = registry
        .inject(
            SlotName::new("t.host"),
            Rc::new(move |_batch| {
                observed.set(observed.get() + 1);
                Ok(())
            }),
            Rc::new(Reporter::default()),
        )
        .unwrap();
    mount_frame(&registry);
    assert_eq!(reporter.errors.borrow().len(), 1);
    assert_eq!(reporter.errors.borrow()[0].to_string(), "null");
    assert_eq!(later_calls.get(), 1);
}

#[test]
fn stopped_waiting_controller_never_resurrects() {
    let (registry, _, _) = bench();
    let calls = Rc::new(Cell::new(0));
    let observed = calls.clone();
    let injection = registry
        .inject(
            SlotName::new("t.host"),
            Rc::new(move |_batch| {
                observed.set(observed.get() + 1);
                Ok(())
            }),
            Rc::new(Reporter::default()),
        )
        .unwrap();
    injection.dispose();
    mount_frame(&registry);
    assert_eq!(calls.get(), 0);
}

#[test]
fn renderer_install_guards_boot_order_and_exposes_live_standard_faces() {
    let (registry, _, _) = bench();
    let key = SlotName::new("root");
    assert!(
        registry
            .render_slot(&key, "owner".to_owned())
            .unwrap_err()
            .to_string()
            .contains("renderer not installed")
    );
    let renderer = Rc::new(Renderer {
        calls: Cell::new(0),
    });
    let install = registry.install_renderer(renderer.clone()).unwrap();
    assert!(registry.install_renderer(renderer.clone()).is_err());
    assert!(
        registry
            .render_slot(&SlotName::new("other"), "owner".to_owned())
            .unwrap_err()
            .to_string()
            .contains("only renders 'root'")
    );
    assert!(
        registry
            .render_slot(&key, "owner".to_owned())
            .unwrap_err()
            .to_string()
            .contains("no registration")
    );
    mount_frame(&registry);
    assert!(
        registry
            .render_slot(&key, "owner".to_owned())
            .unwrap_err()
            .to_string()
            .contains("sessions service mounted")
    );
    let _sessions = registry.install_sessions(Rc::new(JsonFace(json!({
        "ids": [], "current": null
    }))));
    assert!(
        registry
            .render_slot(&key, "owner".to_owned())
            .unwrap_err()
            .to_string()
            .contains("workspaces service mounted")
    );
    let _workspaces = registry.install_workspaces(Rc::new(JsonFace(json!({
        "items": [], "phase": "ready"
    }))));
    assert_eq!(
        registry.render_slot(&key, "owner".to_owned()).unwrap(),
        "tree:owner"
    );
    assert_eq!(renderer.calls.get(), 1);
    install.dispose();
    assert!(registry.render_slot(&key, "owner".to_owned()).is_err());
}

#[test]
fn locale_install_is_boot_once_and_renderer_reads_the_live_face() {
    let (registry, _, _) = bench();
    let first = registry.install_locale(Rc::new(Locale(1))).unwrap();
    assert_eq!(registry.locale().unwrap().revision(), 1);
    assert!(registry.install_locale(Rc::new(Locale(2))).is_err());
    first.dispose();
    let _second = registry.install_locale(Rc::new(Locale(2))).unwrap();
    assert_eq!(registry.locale().unwrap().revision(), 2);
}

#[test]
fn root_store_instances_share_by_handle_while_factories_mint_per_entry() {
    let (registry, _, _) = bench();
    mount_frame(&registry);
    let shared = Rc::new(FakeHandle::default());
    let first = registry
        .register(
            SlotRegistrationOptions::new("t.host"),
            "host".to_owned(),
            Some(RuntimeStoreDeclaration::Shared(shared.clone())),
        )
        .unwrap();
    let mut row = SlotRegistrationOptions::new("t.rows");
    row.id = Some("a".to_owned());
    registry
        .register(
            row,
            "row".to_owned(),
            Some(RuntimeStoreDeclaration::Shared(shared.clone())),
        )
        .unwrap();
    let host_entry = registry.core().entries(&SlotName::new("t.host"))[0].clone();
    let row_entry = registry.core().entries(&SlotName::new("t.rows"))[0].clone();
    let a = registry.store_of(&host_entry, None).unwrap().unwrap();
    let b = registry.store_of(&row_entry, None).unwrap().unwrap();
    assert!(Rc::ptr_eq(&a, &b));
    assert_eq!(shared.keys.borrow().as_slice(), [None]);
    first.dispose();
    assert!(registry.store_of(&host_entry, None).is_ok());

    let minted = Rc::new(Cell::new(0));
    let factory = {
        let minted = minted.clone();
        Rc::new(move || {
            minted.set(minted.get() + 1);
            Rc::new(FakeHandle::default()) as Rc<dyn SlotStoreFactory>
        })
    };
    let mut exclusive = SlotRegistrationOptions::new("t.rows");
    exclusive.id = Some("exclusive".to_owned());
    registry
        .register(
            exclusive,
            "exclusive".to_owned(),
            Some(RuntimeStoreDeclaration::Factory(factory)),
        )
        .unwrap();
    assert_eq!(minted.get(), 1);
}

#[test]
fn session_store_instances_cache_per_id_and_prune_clears_materialized_and_absent_sessions() {
    let (registry, _, _) = bench();
    mount_frame(&registry);
    let handle = Rc::new(FakeHandle::default());
    registry
        .register(
            SlotRegistrationOptions::new("t.panel"),
            "panel".to_owned(),
            Some(RuntimeStoreDeclaration::Shared(handle.clone())),
        )
        .unwrap();
    let entry = registry.core().entries(&SlotName::new("t.panel"))[0].clone();
    let s1 = registry.store_of(&entry, Some("s1")).unwrap().unwrap();
    let s2 = registry.store_of(&entry, Some("s2")).unwrap().unwrap();
    assert!(!Rc::ptr_eq(&s1, &s2));
    assert!(Rc::ptr_eq(
        &s1,
        &registry.store_of(&entry, Some("s1")).unwrap().unwrap()
    ));
    assert!(registry.store_of(&entry, None).is_err());
    registry.prune_store_scope("s1");
    assert_eq!(handle.instances.borrow()[0].clear_count.get(), 1);
    assert!(!Rc::ptr_eq(
        &s1,
        &registry.store_of(&entry, Some("s1")).unwrap().unwrap()
    ));
    let before = handle.instances.borrow().len();
    registry.prune_store_scope("never");
    assert_eq!(handle.instances.borrow().len(), before + 1);
    assert_eq!(
        handle.instances.borrow().last().unwrap().clear_count.get(),
        1
    );
}

#[test]
fn declaration_collapse_releases_injected_store_axis_and_stale_resolution_fails() {
    let (registry, _, _) = bench();
    let frame = mount_frame(&registry);
    let handle = Rc::new(FakeHandle::default());
    let setup_registry = registry.clone();
    let setup_handle = handle.clone();
    let _injection = registry
        .inject(
            SlotName::new("t.host"),
            Rc::new(move |batch| {
                batch.push(setup_registry.register(
                    SlotRegistrationOptions::new("t.host"),
                    "store".to_owned(),
                    Some(RuntimeStoreDeclaration::Shared(setup_handle.clone())),
                )?);
                Ok(())
            }),
            Rc::new(Reporter::default()),
        )
        .unwrap();
    let old = registry.core().entries(&SlotName::new("t.host"))[0].clone();
    registry.store_of(&old, None).unwrap();
    frame.dispose();
    assert!(registry.store_of(&old, None).is_err());
}
