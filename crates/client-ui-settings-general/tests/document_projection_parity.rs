//! Pinned-source document-action state and settings-ledger projection parity.

#![cfg(not(target_arch = "wasm32"))]

use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
};

use futures::{FutureExt, channel::oneshot, future::LocalBoxFuture, pin_mut, poll};
use seekdeep_client_ui_settings_general::*;

#[derive(Default)]
struct Transport {
    describes: RefCell<
        VecDeque<LocalBoxFuture<'static, SettingsDocumentCall<SettingsDocumentDescription>>>,
    >,
    opens: RefCell<VecDeque<LocalBoxFuture<'static, SettingsDocumentCall<()>>>>,
    describe_calls: Cell<usize>,
    open_calls: Cell<usize>,
}

impl Transport {
    fn describe(&self, result: SettingsDocumentCall<SettingsDocumentDescription>) {
        self.describes
            .borrow_mut()
            .push_back(futures::future::ready(result).boxed_local());
    }

    fn describe_future(
        &self,
        future: LocalBoxFuture<'static, SettingsDocumentCall<SettingsDocumentDescription>>,
    ) {
        self.describes.borrow_mut().push_back(future);
    }

    fn open(&self, result: SettingsDocumentCall<()>) {
        self.opens
            .borrow_mut()
            .push_back(futures::future::ready(result).boxed_local());
    }

    fn open_future(&self, future: LocalBoxFuture<'static, SettingsDocumentCall<()>>) {
        self.opens.borrow_mut().push_back(future);
    }
}

impl SettingsDocumentTransport for Transport {
    fn describe(
        &self,
    ) -> LocalBoxFuture<'static, SettingsDocumentCall<SettingsDocumentDescription>> {
        self.describe_calls.set(self.describe_calls.get() + 1);
        self.describes.borrow_mut().pop_front().unwrap_or_else(|| {
            futures::future::ready(SettingsDocumentCall::Failed(
                "unexpected settings.describe call".to_owned(),
            ))
            .boxed_local()
        })
    }

    fn open_document(&self) -> LocalBoxFuture<'static, SettingsDocumentCall<()>> {
        self.open_calls.set(self.open_calls.get() + 1);
        self.opens.borrow_mut().pop_front().unwrap_or_else(|| {
            futures::future::ready(SettingsDocumentCall::Failed(
                "unexpected settings.openDocument call".to_owned(),
            ))
            .boxed_local()
        })
    }
}

fn store(transport: &Rc<Transport>) -> Rc<SettingsDocumentStore> {
    let transport: Rc<dyn SettingsDocumentTransport> = transport.clone();
    SettingsDocumentStore::new(transport)
}

fn available(has_document: bool) -> SettingsDocumentCall<SettingsDocumentDescription> {
    SettingsDocumentCall::Success(SettingsDocumentDescription { has_document })
}

#[tokio::test]
async fn loads_provider_metadata_and_opens_the_host_owned_document() {
    let transport = Rc::new(Transport::default());
    transport.describe(available(true));
    transport.open(SettingsDocumentCall::Success(()));
    let controller = store(&transport);
    controller.load().await.unwrap();
    assert_eq!(
        controller.snapshot().as_ref(),
        &SettingsDocumentState {
            status: SettingsDocumentStatus::Ready,
            opening: false,
            error: None,
        }
    );
    controller.open().await.unwrap();
    assert_eq!(transport.open_calls.get(), 1);
}

#[tokio::test]
async fn absent_rejected_and_failed_metadata_are_unavailable_and_never_open() {
    let transport = Rc::new(Transport::default());
    transport.describe(available(false));
    let absent = store(&transport);
    absent.load().await.unwrap();
    absent.open().await.unwrap();
    assert_eq!(
        absent.snapshot().status,
        SettingsDocumentStatus::Unavailable
    );
    assert_eq!(transport.open_calls.get(), 0);

    let transport = Rc::new(Transport::default());
    transport.describe(SettingsDocumentCall::Failed("offline".to_owned()));
    let failed = store(&transport);
    failed.load().await.unwrap();
    assert_eq!(
        failed.snapshot().status,
        SettingsDocumentStatus::Unavailable
    );
    assert_eq!(failed.snapshot().error.as_deref(), Some("offline"));

    let transport = Rc::new(Transport::default());
    transport.describe(SettingsDocumentCall::Rejected("provider failed".to_owned()));
    let rejected = store(&transport);
    rejected.load().await.unwrap();
    assert_eq!(
        rejected.snapshot().error.as_deref(),
        Some("provider failed")
    );
}

#[tokio::test]
async fn concurrent_open_gestures_collapse_and_failure_restores_the_ready_action() {
    let transport = Rc::new(Transport::default());
    transport.describe(available(true));
    let (sender, receiver) = oneshot::channel();
    transport.open_future(
        async move {
            receiver
                .await
                .unwrap_or_else(|_| SettingsDocumentCall::Failed("open sender dropped".to_owned()))
        }
        .boxed_local(),
    );
    let controller = store(&transport);
    controller.load().await.unwrap();
    let first = controller.open();
    pin_mut!(first);
    assert!(poll!(&mut first).is_pending());
    let second = controller.open();
    assert_eq!(transport.open_calls.get(), 1);
    sender
        .send(SettingsDocumentCall::Rejected(
            "no default editor".to_owned(),
        ))
        .unwrap();
    let (first, second) = futures::join!(first, second);
    first.unwrap();
    second.unwrap();
    let state = controller.snapshot();
    assert_eq!(state.status, SettingsDocumentStatus::Ready);
    assert!(!state.opening);
    assert_eq!(state.error.as_deref(), Some("no default editor"));
}

#[tokio::test]
async fn stale_metadata_completions_are_ignored_and_non_error_failures_are_preserved() {
    let transport = Rc::new(Transport::default());
    let (sender, receiver) = oneshot::channel();
    transport.describe_future(
        async move {
            receiver.await.unwrap_or_else(|_| {
                SettingsDocumentCall::Failed("describe sender dropped".to_owned())
            })
        }
        .boxed_local(),
    );
    transport.describe(available(true));
    transport.open(SettingsDocumentCall::Failed(
        "native unavailable".to_owned(),
    ));
    let controller = store(&transport);
    let stale = controller.load();
    pin_mut!(stale);
    assert!(poll!(&mut stale).is_pending());
    controller.load().await.unwrap();
    sender.send(available(false)).unwrap();
    stale.await.unwrap();
    assert_eq!(controller.snapshot().status, SettingsDocumentStatus::Ready);
    controller.open().await.unwrap();
    assert_eq!(
        controller.snapshot().error.as_deref(),
        Some("native unavailable")
    );

    let transport = Rc::new(Transport::default());
    let (sender, receiver) = oneshot::channel();
    transport.describe_future(
        async move {
            receiver.await.unwrap_or_else(|_| {
                SettingsDocumentCall::Failed("describe sender dropped".to_owned())
            })
        }
        .boxed_local(),
    );
    transport.describe(available(true));
    let controller = store(&transport);
    let stale = controller.load();
    pin_mut!(stale);
    assert!(poll!(&mut stale).is_pending());
    controller.load().await.unwrap();
    sender
        .send(SettingsDocumentCall::Failed("stale offline".to_owned()))
        .unwrap();
    stale.await.unwrap();
    assert_eq!(controller.snapshot().status, SettingsDocumentStatus::Ready);
    assert!(controller.snapshot().error.is_none());
}

struct CapturingSpawner {
    tasks: RefCell<Vec<LocalBoxFuture<'static, ()>>>,
}

impl SettingsDocumentTaskSpawner for CapturingSpawner {
    fn spawn(&self, task: LocalBoxFuture<'static, ()>) {
        self.tasks.borrow_mut().push(task);
    }
}

#[tokio::test]
async fn reconnect_refresh_stays_lazy_until_metadata_was_requested() {
    let transport = Rc::new(Transport::default());
    transport.describe(available(true));
    transport.describe(available(false));
    let controller = store(&transport);
    let spawner = CapturingSpawner {
        tasks: RefCell::new(Vec::new()),
    };
    refresh_document_if_loaded(Some(&controller), &spawner);
    assert!(spawner.tasks.borrow().is_empty());
    controller.load().await.unwrap();
    refresh_document_if_loaded(Some(&controller), &spawner);
    assert_eq!(spawner.tasks.borrow().len(), 1);
    let refresh = spawner.tasks.borrow_mut().pop().unwrap();
    refresh.await;
    assert_eq!(
        controller.snapshot().status,
        SettingsDocumentStatus::Unavailable
    );
}

#[test]
fn section_projection_defaults_sorts_and_preserves_identity_until_slot_or_locale_moves() {
    let projection = SettingsLedgerProjection::default();
    let rows = projection.sections(
        1,
        1,
        [
            SettingsSectionEntry {
                id: Some("z".into()),
                order: Some(20.0),
                label: Some("Z".into()),
            },
            SettingsSectionEntry {
                id: Some("general".into()),
                order: None,
                label: Some("General".into()),
            },
            SettingsSectionEntry {
                id: None,
                order: None,
                label: None,
            },
        ],
    );
    assert_eq!(
        rows.as_slice(),
        [
            SettingsSectionRow {
                id: "general".into(),
                order: 0.0,
                label: "General".into(),
            },
            SettingsSectionRow {
                id: String::new(),
                order: 0.0,
                label: String::new(),
            },
            SettingsSectionRow {
                id: "z".into(),
                order: 20.0,
                label: "Z".into(),
            },
        ]
    );
    assert!(Rc::ptr_eq(
        &rows,
        &projection.sections(1, 1, std::iter::empty())
    ));
    assert!(!Rc::ptr_eq(
        &rows,
        &projection.sections(1, 2, std::iter::empty())
    ));
}

#[test]
fn onboarding_projection_keeps_stable_tie_order_and_version_identity() {
    let projection = SettingsLedgerProjection::default();
    let steps = projection.onboarding(
        4,
        [
            SettingsOnboardingEntry {
                id: Some("credential".into()),
                order: Some(0.0),
            },
            SettingsOnboardingEntry {
                id: Some("welcome".into()),
                order: Some(-100.0),
            },
            SettingsOnboardingEntry {
                id: Some("default-order".into()),
                order: None,
            },
        ],
    );
    assert_eq!(
        steps
            .iter()
            .map(|step| step.id.as_str())
            .collect::<Vec<_>>(),
        ["welcome", "credential", "default-order"]
    );
    assert!(Rc::ptr_eq(
        &steps,
        &projection.onboarding(4, std::iter::empty())
    ));
    assert!(!Rc::ptr_eq(
        &steps,
        &projection.onboarding(5, std::iter::empty())
    ));
}

#[test]
fn shipped_dictionaries_have_exact_balanced_keys_and_copy() {
    assert_eq!(
        SETTINGS_ZH.iter().map(|(key, _)| *key).collect::<Vec<_>>(),
        SETTINGS_EN.iter().map(|(key, _)| *key).collect::<Vec<_>>()
    );
    assert_eq!(SETTINGS_EN[3], ("openDocument", "Open configuration file"));
    assert_eq!(SETTINGS_ZH[5], ("general.nav", "通用设置"));
}
