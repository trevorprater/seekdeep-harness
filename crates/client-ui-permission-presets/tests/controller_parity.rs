//! Permission Settings controller lifecycle, concurrency, and failure parity.

use std::{cell::RefCell, collections::VecDeque, rc::Rc};

use futures::{
    FutureExt as _, channel::oneshot, executor::LocalPool, future::LocalBoxFuture,
    task::LocalSpawnExt as _,
};
use seekdeep_client_ui_permission_presets::{
    PermissionNamespaceView, PermissionPresetSettingsController, PermissionSettingsDescription,
    PermissionSettingsMutation, PermissionSettingsStatus, PermissionSettingsTransport,
};
use serde_json::json;

enum Plan<T> {
    Ready(Result<T, String>),
    Deferred(oneshot::Receiver<Result<T, String>>),
}

impl<T: 'static> Plan<T> {
    fn future(self) -> LocalBoxFuture<'static, Result<T, String>> {
        match self {
            Self::Ready(value) => futures::future::ready(value).boxed_local(),
            Self::Deferred(receiver) => async move {
                receiver
                    .await
                    .unwrap_or_else(|_| Err("test response dropped".to_owned()))
            }
            .boxed_local(),
        }
    }
}

#[derive(Default)]
struct Transport {
    descriptions: RefCell<VecDeque<Plan<PermissionSettingsDescription>>>,
    mutations: RefCell<VecDeque<Plan<PermissionNamespaceView>>>,
    requests: RefCell<Vec<PermissionSettingsMutation>>,
    describes: RefCell<usize>,
}

impl PermissionSettingsTransport for Transport {
    fn describe(&self) -> LocalBoxFuture<'static, Result<PermissionSettingsDescription, String>> {
        *self.describes.borrow_mut() += 1;
        self.descriptions.borrow_mut().pop_front().unwrap().future()
    }

    fn mutate(
        &self,
        request: PermissionSettingsMutation,
    ) -> LocalBoxFuture<'static, Result<PermissionNamespaceView, String>> {
        self.requests.borrow_mut().push(request);
        self.mutations.borrow_mut().pop_front().unwrap().future()
    }
}

fn schema() -> serde_json::Value {
    json!({
        "uid": 5,
        "refs": {
            "1": {"type":"const","value":"read-only"},
            "2": {"type":"const","value":"workspace-write"},
            "3": {"type":"const","value":"danger-full-access"},
            "4": {"type":"union","list":[1,2,3]},
            "5": {"type":"object","dict":{"defaultPreset":4}}
        }
    })
}

fn view(value: &str, revision: u64) -> PermissionNamespaceView {
    PermissionNamespaceView {
        namespace: "permission".to_owned(),
        schema: schema(),
        value: json!({"defaultPreset":value}),
        revision,
    }
}

fn description(
    writable: bool,
    namespaces: Vec<PermissionNamespaceView>,
) -> PermissionSettingsDescription {
    PermissionSettingsDescription {
        writable,
        namespaces,
    }
}

#[test]
fn load_select_and_read_only_noop_preserve_revision_and_notifications() {
    let transport = Rc::new(Transport::default());
    transport
        .descriptions
        .borrow_mut()
        .push_back(Plan::Ready(Ok(description(
            true,
            vec![view("read-only", 4)],
        ))));
    transport
        .mutations
        .borrow_mut()
        .push_back(Plan::Ready(Ok(view("workspace-write", 5))));
    let controller = PermissionPresetSettingsController::new(transport.clone());
    let notifications = Rc::new(RefCell::new(0));
    let notified = notifications.clone();
    let _subscription = controller.subscribe(Rc::new(move || *notified.borrow_mut() += 1));
    futures::executor::block_on(controller.load());
    assert_eq!(
        controller.snapshot().status,
        PermissionSettingsStatus::Ready
    );
    assert!(controller.snapshot().writable);
    assert_eq!(controller.snapshot().current_value, "read-only");
    assert_eq!(controller.snapshot().revision, 4);
    futures::executor::block_on(controller.select("workspace-write".to_owned()));
    assert_eq!(
        transport.requests.borrow().as_slice(),
        [PermissionSettingsMutation {
            preset: "workspace-write".to_owned(),
            expected_revision: 4,
        }]
    );
    assert_eq!(controller.snapshot().current_value, "workspace-write");
    assert_eq!(controller.snapshot().revision, 5);
    assert_eq!(*notifications.borrow(), 4);

    let read_only = Rc::new(Transport::default());
    read_only
        .descriptions
        .borrow_mut()
        .push_back(Plan::Ready(Ok(description(
            false,
            vec![view("read-only", 2)],
        ))));
    let controller = PermissionPresetSettingsController::new(read_only.clone());
    futures::executor::block_on(controller.load());
    futures::executor::block_on(controller.select("workspace-write".to_owned()));
    assert!(read_only.requests.borrow().is_empty());
}

#[test]
fn unavailable_and_failures_are_contained_without_erasing_previous_fields() {
    let transport = Rc::new(Transport::default());
    transport.descriptions.borrow_mut().extend([
        Plan::Ready(Ok(description(true, vec![view("read-only", 7)]))),
        Plan::Ready(Err("offline".to_owned())),
        Plan::Ready(Ok(description(true, Vec::new()))),
    ]);
    transport
        .mutations
        .borrow_mut()
        .push_back(Plan::Ready(Err("stale".to_owned())));
    let controller = PermissionPresetSettingsController::new(transport);
    futures::executor::block_on(controller.load());
    futures::executor::block_on(controller.select("workspace-write".to_owned()));
    assert_eq!(
        controller.snapshot().status,
        PermissionSettingsStatus::Error
    );
    assert_eq!(controller.snapshot().error.as_deref(), Some("stale"));
    assert_eq!(controller.snapshot().current_value, "read-only");
    futures::executor::block_on(controller.load());
    assert_eq!(controller.snapshot().error.as_deref(), Some("offline"));
    assert_eq!(controller.snapshot().revision, 7);
    futures::executor::block_on(controller.load());
    assert_eq!(
        controller.snapshot().status,
        PermissionSettingsStatus::Unavailable
    );
    assert!(!controller.snapshot().writable);
    assert!(controller.snapshot().current_value.is_empty());
    assert!(controller.snapshot().options.is_empty());
    assert_eq!(controller.snapshot().revision, 7);
}

#[test]
fn latest_generation_disposal_and_loaded_refresh_own_publication() {
    let transport = Rc::new(Transport::default());
    let (first_sender, first_receiver) = oneshot::channel();
    transport.descriptions.borrow_mut().extend([
        Plan::Deferred(first_receiver),
        Plan::Ready(Ok(description(false, vec![view("read-only", 2)]))),
    ]);
    let controller = PermissionPresetSettingsController::new(transport.clone());
    assert!(controller.refresh_if_loaded().is_none());
    let mut pool = LocalPool::new();
    pool.spawner().spawn_local(controller.load()).unwrap();
    pool.spawner().spawn_local(controller.load()).unwrap();
    pool.run_until_stalled();
    assert_eq!(controller.snapshot().revision, 2);
    assert!(!controller.snapshot().writable);
    assert!(
        first_sender
            .send(Ok(description(true, vec![view("workspace-write", 1)])))
            .is_ok()
    );
    pool.run_until_stalled();
    assert_eq!(controller.snapshot().current_value, "read-only");
    assert_eq!(controller.snapshot().revision, 2);

    transport
        .descriptions
        .borrow_mut()
        .push_back(Plan::Ready(Ok(description(
            true,
            vec![view("read-only", 3)],
        ))));
    let refresh = controller.refresh_if_loaded().unwrap();
    pool.run_until(refresh);
    assert_eq!(*transport.describes.borrow(), 3);
    assert_eq!(controller.snapshot().revision, 3);

    let (late_sender, late_receiver) = oneshot::channel();
    transport
        .descriptions
        .borrow_mut()
        .push_back(Plan::Deferred(late_receiver));
    pool.spawner().spawn_local(controller.load()).unwrap();
    pool.run_until_stalled();
    controller.dispose();
    assert!(
        late_sender
            .send(Ok(description(true, vec![view("workspace-write", 4)])))
            .is_ok()
    );
    pool.run_until_stalled();
    assert_eq!(
        controller.snapshot().status,
        PermissionSettingsStatus::Loading
    );
    assert_eq!(controller.snapshot().revision, 3);
}
