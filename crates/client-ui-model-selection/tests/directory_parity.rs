//! Shared model directory lifecycle, selection, reset, and stale-settlement parity.

use std::{cell::RefCell, collections::VecDeque, rc::Rc};

use futures::{
    FutureExt as _, channel::oneshot, executor::LocalPool, future::LocalBoxFuture,
    task::LocalSpawnExt as _,
};
use seekdeep_client_ui_model_selection::{
    ModelDirectory, ModelDirectoryFailure, ModelDirectoryStatus, ModelDirectoryTransport,
    ModelEntry, ModelId, ModelProviderGroup, ModelProviderId, ModelSelection, SessionModels,
};
use seekdeep_identity::SessionId;

enum Plan<T> {
    Ready(Result<T, ModelDirectoryFailure>),
    Deferred(oneshot::Receiver<Result<T, ModelDirectoryFailure>>),
}

impl<T: 'static> Plan<T> {
    fn future(self) -> LocalBoxFuture<'static, Result<T, ModelDirectoryFailure>> {
        match self {
            Self::Ready(value) => futures::future::ready(value).boxed_local(),
            Self::Deferred(receiver) => async move {
                receiver.await.unwrap_or_else(|_| {
                    Err(ModelDirectoryFailure {
                        code: "internal".to_owned(),
                        message: "test response dropped".to_owned(),
                    })
                })
            }
            .boxed_local(),
        }
    }
}

#[derive(Default)]
struct Transport {
    models: RefCell<VecDeque<Plan<SessionModels>>>,
    selects: RefCell<VecDeque<Plan<ModelSelection>>>,
    model_calls: RefCell<Vec<SessionId>>,
    select_calls: RefCell<Vec<(SessionId, ModelSelection)>>,
}

impl ModelDirectoryTransport for Transport {
    fn models(
        &self,
        session_id: SessionId,
    ) -> LocalBoxFuture<'static, Result<SessionModels, ModelDirectoryFailure>> {
        self.model_calls.borrow_mut().push(session_id);
        self.models.borrow_mut().pop_front().unwrap().future()
    }

    fn select_model(
        &self,
        session_id: SessionId,
        selection: ModelSelection,
    ) -> LocalBoxFuture<'static, Result<ModelSelection, ModelDirectoryFailure>> {
        self.select_calls.borrow_mut().push((session_id, selection));
        self.selects.borrow_mut().pop_front().unwrap().future()
    }
}

fn provider(value: &str) -> ModelProviderId {
    ModelProviderId::new(value)
}

fn model(value: &str) -> ModelId {
    ModelId::new(value)
}

fn selection(model_id: &str) -> ModelSelection {
    ModelSelection {
        provider: provider("deepseek-official"),
        model: model(model_id),
        reasoning_effort: None,
    }
}

fn models(current: &str, routable: bool) -> SessionModels {
    SessionModels {
        current: selection(current),
        routable,
        groups: vec![ModelProviderGroup {
            id: provider("deepseek-official"),
            name: "DeepSeek".to_owned(),
            models: vec![
                ModelEntry {
                    id: model("deepseek-v4-flash"),
                    name: "Flash".to_owned(),
                    description: None,
                    reasoning: None,
                },
                ModelEntry {
                    id: model("deepseek-v4-pro"),
                    name: "Pro".to_owned(),
                    description: None,
                    reasoning: None,
                },
            ],
        }],
        failures: Vec::new(),
    }
}

fn failure(code: &str, message: &str) -> ModelDirectoryFailure {
    ModelDirectoryFailure {
        code: code.to_owned(),
        message: message.to_owned(),
    }
}

#[test]
fn load_and_select_publish_one_shared_snapshot_and_exact_requests() {
    let transport = Rc::new(Transport::default());
    transport
        .models
        .borrow_mut()
        .push_back(Plan::Ready(Ok(models("deepseek-v4-flash", true))));
    transport
        .selects
        .borrow_mut()
        .push_back(Plan::Ready(Ok(selection("deepseek-v4-pro"))));
    let directory = ModelDirectory::new(transport.clone(), SessionId::new("s1"), Rc::new(|| true));
    let notifications = Rc::new(RefCell::new(0));
    let notified = notifications.clone();
    let _subscription = directory.subscribe(Rc::new(move || *notified.borrow_mut() += 1));
    let loaded = futures::executor::block_on(directory.load()).unwrap();
    assert_eq!(loaded.current.model.as_str(), "deepseek-v4-flash");
    assert_eq!(directory.snapshot().status, ModelDirectoryStatus::Ready);
    assert_eq!(directory.snapshot().routable, Some(true));
    futures::executor::block_on(directory.select(selection("deepseek-v4-pro"))).unwrap();
    assert_eq!(
        directory
            .snapshot()
            .current
            .as_ref()
            .unwrap()
            .model
            .as_str(),
        "deepseek-v4-pro"
    );
    assert_eq!(
        transport.model_calls.borrow().as_slice(),
        [SessionId::new("s1")]
    );
    assert_eq!(
        transport.select_calls.borrow().as_slice(),
        [(SessionId::new("s1"), selection("deepseek-v4-pro"))]
    );
    assert_eq!(*notifications.borrow(), 4);
}

#[test]
fn failures_preserve_last_good_data_and_unavailable_sessions_never_call_transport() {
    let transport = Rc::new(Transport::default());
    transport.models.borrow_mut().extend([
        Plan::Ready(Ok(models("deepseek-v4-flash", true))),
        Plan::Ready(Err(failure("internal", "offline"))),
    ]);
    transport
        .selects
        .borrow_mut()
        .push_back(Plan::Ready(Err(failure("model-unavailable", "images"))));
    let directory = ModelDirectory::new(transport.clone(), SessionId::new("s1"), Rc::new(|| true));
    futures::executor::block_on(directory.load()).unwrap();
    let error = futures::executor::block_on(directory.load()).unwrap_err();
    assert_eq!(error, "session.models failed: internal: offline");
    assert_eq!(directory.snapshot().groups.len(), 1);
    assert_eq!(
        directory.snapshot().error.as_deref(),
        Some("internal: offline")
    );
    let error =
        futures::executor::block_on(directory.select(selection("deepseek-v4-pro"))).unwrap_err();
    assert_eq!(
        error,
        "session.selectModel failed: model-unavailable: images"
    );
    assert_eq!(
        directory
            .snapshot()
            .current
            .as_ref()
            .unwrap()
            .model
            .as_str(),
        "deepseek-v4-flash"
    );

    let unavailable_transport = Rc::new(Transport::default());
    let unavailable = ModelDirectory::new(
        unavailable_transport.clone(),
        SessionId::new("child"),
        Rc::new(|| false),
    );
    assert!(
        futures::executor::block_on(unavailable.load())
            .unwrap_err()
            .contains("addressed subagent")
    );
    assert!(
        futures::executor::block_on(unavailable.select(selection("deepseek-v4-pro")))
            .unwrap_err()
            .contains("addressed subagent")
    );
    assert!(unavailable_transport.model_calls.borrow().is_empty());
    assert!(unavailable_transport.select_calls.borrow().is_empty());
}

#[test]
fn stale_disposed_and_reconnect_generations_preserve_exact_publication_rules() {
    let transport = Rc::new(Transport::default());
    let (stale_sender, stale_receiver) = oneshot::channel();
    transport.models.borrow_mut().extend([
        Plan::Deferred(stale_receiver),
        Plan::Ready(Ok(models("deepseek-v4-pro", false))),
        Plan::Ready(Ok(models("deepseek-v4-flash", true))),
    ]);
    let directory = ModelDirectory::new(transport.clone(), SessionId::new("s1"), Rc::new(|| true));
    let mut pool = LocalPool::new();
    let first = directory.load();
    pool.spawner().spawn_local(first.map(|_| ())).unwrap();
    pool.run_until_stalled();
    pool.run_until(directory.load()).unwrap();
    assert_eq!(directory.snapshot().routable, Some(false));
    assert!(
        stale_sender
            .send(Ok(models("deepseek-v4-flash", true)))
            .is_ok()
    );
    pool.run_until_stalled();
    assert_eq!(
        directory
            .snapshot()
            .current
            .as_ref()
            .unwrap()
            .model
            .as_str(),
        "deepseek-v4-pro"
    );

    let reset = directory.reset_connected().unwrap();
    assert_eq!(directory.snapshot().current, None);
    assert_eq!(directory.snapshot().status, ModelDirectoryStatus::Loading);
    pool.run_until(reset).unwrap();
    assert_eq!(
        directory
            .snapshot()
            .current
            .as_ref()
            .unwrap()
            .model
            .as_str(),
        "deepseek-v4-flash"
    );

    let (late_sender, late_receiver) = oneshot::channel();
    transport
        .models
        .borrow_mut()
        .push_back(Plan::Deferred(late_receiver));
    pool.spawner()
        .spawn_local(directory.load().map(|_| ()))
        .unwrap();
    pool.run_until_stalled();
    directory.dispose();
    assert!(
        late_sender
            .send(Ok(models("deepseek-v4-pro", true)))
            .is_ok()
    );
    pool.run_until_stalled();
    assert_eq!(directory.snapshot().status, ModelDirectoryStatus::Loading);
    assert!(directory.reset_connected().is_none());
}
