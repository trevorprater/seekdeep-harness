//! Popup-select binding, filtering, confirmation, settlement, and race parity.

use std::{
    any::Any,
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
};

use futures::{
    FutureExt as _,
    channel::oneshot,
    executor::{LocalPool, LocalSpawner},
    future::LocalBoxFuture,
    task::LocalSpawnExt as _,
};
use seekdeep_client_ui_commands::{
    PopupAbortFactory, PopupAbortHandle, PopupBusinessSpec, PopupSelectController, PopupSelectDeps,
    PopupStatus, PopupTaskSpawner, PopupTokenSegment, SelectConfirmation, SelectOption,
};
use seekdeep_client_ui_input_trigger::TokenSpan;
use serde_json::{Value, json};

#[derive(Default)]
struct AbortHandle {
    aborted: Cell<bool>,
}

impl PopupAbortHandle for AbortHandle {
    fn abort(&self) {
        self.aborted.set(true);
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(Default)]
struct AbortFactory {
    signals: RefCell<Vec<Rc<AbortHandle>>>,
}

impl PopupAbortFactory for AbortFactory {
    fn create(&self) -> Rc<dyn PopupAbortHandle> {
        let signal = Rc::new(AbortHandle::default());
        self.signals.borrow_mut().push(signal.clone());
        signal
    }
}

enum OptionPlan {
    Ready(Result<Vec<SelectOption>, String>),
    Deferred(oneshot::Receiver<Result<Vec<SelectOption>, String>>),
}

enum SelectPlan {
    Ready(Result<(), String>),
    Deferred(oneshot::Receiver<Result<(), String>>),
}

#[derive(Default)]
struct Spec {
    options: RefCell<VecDeque<OptionPlan>>,
    selects: RefCell<VecDeque<SelectPlan>>,
    option_calls: RefCell<Vec<Value>>,
    select_calls: RefCell<Vec<(String, Value)>>,
}

impl PopupBusinessSpec for Spec {
    fn options(
        &self,
        context: Value,
        _signal: Rc<dyn PopupAbortHandle>,
    ) -> LocalBoxFuture<'static, Result<Vec<SelectOption>, String>> {
        self.option_calls.borrow_mut().push(context);
        match self.options.borrow_mut().pop_front().unwrap() {
            OptionPlan::Ready(value) => futures::future::ready(value).boxed_local(),
            OptionPlan::Deferred(receiver) => async move {
                receiver
                    .await
                    .unwrap_or_else(|_| Err("options dropped".to_owned()))
            }
            .boxed_local(),
        }
    }

    fn on_select(
        &self,
        option: SelectOption,
        context: Value,
    ) -> LocalBoxFuture<'static, Result<(), String>> {
        self.select_calls.borrow_mut().push((option.id, context));
        match self.selects.borrow_mut().pop_front().unwrap() {
            SelectPlan::Ready(value) => futures::future::ready(value).boxed_local(),
            SelectPlan::Deferred(receiver) => async move {
                receiver
                    .await
                    .unwrap_or_else(|_| Err("selection dropped".to_owned()))
            }
            .boxed_local(),
        }
    }
}

#[derive(Default)]
struct Deps {
    consumed: RefCell<Vec<PopupTokenSegment>>,
    focused: Cell<usize>,
    consume_result: Cell<bool>,
}

impl PopupSelectDeps for Deps {
    fn consume(&self, segment: &PopupTokenSegment) -> bool {
        self.consumed.borrow_mut().push(segment.clone());
        self.consume_result.get()
    }

    fn focus_composer(&self) {
        self.focused.set(self.focused.get() + 1);
    }
}

struct Spawner(LocalSpawner);

impl PopupTaskSpawner for Spawner {
    fn spawn(&self, task: LocalBoxFuture<'static, ()>) {
        self.0.spawn_local(task).unwrap();
    }
}

fn option(id: &str, detail: Option<&str>) -> SelectOption {
    SelectOption {
        id: id.to_owned(),
        label: id.to_uppercase(),
        detail: detail.map(ToOwned::to_owned),
        active: None,
        confirmation: None,
    }
}

fn gated(id: &str) -> SelectOption {
    SelectOption {
        confirmation: Some(SelectConfirmation {
            title: "Confirm".to_owned(),
            description: "Risk".to_owned(),
            acknowledge_label: "Acknowledge".to_owned(),
            cancel_label: "Cancel".to_owned(),
            confirm_label: "Enable".to_owned(),
        }),
        ..option(id, None)
    }
}

fn controller(pool: &LocalPool) -> (Rc<Deps>, Rc<AbortFactory>, Rc<PopupSelectController>) {
    let deps = Rc::new(Deps::default());
    deps.consume_result.set(true);
    let aborts = Rc::new(AbortFactory::default());
    let controller = PopupSelectController::new(
        deps.clone(),
        aborts.clone(),
        Rc::new(Spawner(pool.spawner())),
    );
    (deps, aborts, controller)
}

fn segment(revision: u64) -> PopupTokenSegment {
    PopupTokenSegment::Menu(TokenSpan {
        start: 0,
        end: 4,
        draft_rev: revision,
    })
}

#[test]
fn open_ready_search_movement_failure_retry_and_dismiss_match_the_source() {
    let mut pool = LocalPool::new();
    let (deps, aborts, controller) = controller(&pool);
    let spec = Rc::new(Spec::default());
    spec.options.borrow_mut().extend([
        OptionPlan::Ready(Ok(vec![
            option("dark", None),
            option("light", Some("bright")),
            option("sepia", Some("warm")),
        ])),
        OptionPlan::Ready(Err("offline".to_owned())),
        OptionPlan::Ready(Ok(vec![option("retry", None)])),
    ]);
    controller.open(
        "theme".to_owned(),
        spec.clone(),
        json!({"session":"s1"}),
        segment(1),
    );
    assert_eq!(controller.snapshot().status, PopupStatus::Pending);
    pool.run_until_stalled();
    assert_eq!(controller.snapshot().status, PopupStatus::Ready);
    assert_eq!(controller.snapshot().options.len(), 3);
    controller.set_search(" WARM ".to_owned());
    assert_eq!(controller.snapshot().active, 0);
    controller.move_highlight(1);
    assert_eq!(controller.snapshot().active, 0);
    controller.set_search(String::new());
    controller.move_highlight(-1);
    assert_eq!(controller.snapshot().active, 2);
    controller.highlight(1);
    assert_eq!(controller.snapshot().active, 1);

    controller.open(
        "theme".to_owned(),
        spec.clone(),
        json!({"session":"s2"}),
        segment(2),
    );
    assert!(aborts.signals.borrow()[0].aborted.get());
    pool.run_until_stalled();
    assert_eq!(controller.snapshot().status, PopupStatus::Failed);
    assert_eq!(controller.snapshot().error.as_deref(), Some("offline"));
    controller.set_search("kept".to_owned());
    controller.retry();
    assert_eq!(controller.snapshot().status, PopupStatus::Pending);
    pool.run_until_stalled();
    assert_eq!(controller.snapshot().options[0].id, "retry");
    assert_eq!(controller.snapshot().search, "kept");
    controller.dismiss(true);
    assert!(!controller.snapshot().open);
    assert!(aborts.signals.borrow()[1].aborted.get());
    assert_eq!(deps.focused.get(), 1);
    controller.dismiss(true);
    assert_eq!(deps.focused.get(), 1);
}

#[test]
fn confirmation_success_failure_and_benign_consume_miss_match_the_source() {
    let mut pool = LocalPool::new();
    let (deps, _aborts, controller) = controller(&pool);
    let spec = Rc::new(Spec::default());
    spec.options
        .borrow_mut()
        .push_back(OptionPlan::Ready(Ok(vec![
            gated("full"),
            option("safe", None),
        ])));
    spec.selects.borrow_mut().extend([
        SelectPlan::Ready(Ok(())),
        SelectPlan::Ready(Err("selection failed".to_owned())),
        SelectPlan::Ready(Ok(())),
    ]);
    controller.open(
        "permission".to_owned(),
        spec,
        json!({"session":"s1"}),
        segment(7),
    );
    pool.run_until_stalled();
    pool.run_until(controller.select(0));
    assert_eq!(
        controller.snapshot().confirming.as_ref().unwrap().id,
        "full"
    );
    assert!(!controller.snapshot().acknowledged);
    controller.acknowledge(true);
    pool.run_until(controller.confirm());
    assert!(!controller.snapshot().open);
    assert_eq!(deps.consumed.borrow().as_slice(), [segment(7)]);
    assert_eq!(deps.focused.get(), 1);

    let spec = Rc::new(Spec::default());
    spec.options
        .borrow_mut()
        .push_back(OptionPlan::Ready(Ok(vec![option("safe", None)])));
    spec.selects
        .borrow_mut()
        .push_back(SelectPlan::Ready(Err("selection failed".to_owned())));
    controller.open(
        "theme".to_owned(),
        spec,
        json!({"session":"s2"}),
        segment(8),
    );
    pool.run_until_stalled();
    controller.set_search("safe".to_owned());
    pool.run_until(controller.select(0));
    assert!(controller.snapshot().open);
    assert!(!controller.snapshot().submitting);
    assert_eq!(controller.snapshot().search, "safe");
    assert_eq!(
        controller.snapshot().error.as_deref(),
        Some("selection failed")
    );
    assert_eq!(deps.consumed.borrow().len(), 1);

    let spec = Rc::new(Spec::default());
    spec.options
        .borrow_mut()
        .push_back(OptionPlan::Ready(Ok(vec![option("safe", None)])));
    spec.selects
        .borrow_mut()
        .push_back(SelectPlan::Ready(Ok(())));
    deps.consume_result.set(false);
    controller.open(
        "theme".to_owned(),
        spec,
        json!({"session":"s3"}),
        segment(9),
    );
    pool.run_until_stalled();
    pool.run_until(controller.select(0));
    assert!(!controller.snapshot().open);
    assert_eq!(deps.consumed.borrow().len(), 2);
    assert_eq!(deps.focused.get(), 2);
}

#[test]
fn binding_identity_revokes_late_loads_and_selections() {
    let mut pool = LocalPool::new();
    let (deps, aborts, controller) = controller(&pool);
    let old = Rc::new(Spec::default());
    let (old_options_sender, old_options_receiver) = oneshot::channel();
    old.options
        .borrow_mut()
        .push_back(OptionPlan::Deferred(old_options_receiver));
    controller.open("old".to_owned(), old, json!({"id":"old"}), segment(1));
    pool.run_until_stalled();
    let new = Rc::new(Spec::default());
    new.options
        .borrow_mut()
        .push_back(OptionPlan::Ready(Ok(vec![option("new", None)])));
    controller.open("new".to_owned(), new, json!({"id":"new"}), segment(2));
    assert!(aborts.signals.borrow()[0].aborted.get());
    assert!(
        old_options_sender
            .send(Ok(vec![option("old", None)]))
            .is_ok()
    );
    pool.run_until_stalled();
    assert_eq!(controller.snapshot().command.as_deref(), Some("new"));
    assert_eq!(controller.snapshot().options[0].id, "new");

    let selecting = Rc::new(Spec::default());
    selecting
        .options
        .borrow_mut()
        .push_back(OptionPlan::Ready(Ok(vec![option("pick", None)])));
    let (select_sender, select_receiver) = oneshot::channel();
    selecting
        .selects
        .borrow_mut()
        .push_back(SelectPlan::Deferred(select_receiver));
    controller.open("pick".to_owned(), selecting, json!({}), segment(3));
    pool.run_until_stalled();
    let select = controller.select(0);
    pool.spawner().spawn_local(select).unwrap();
    pool.run_until_stalled();
    controller.dismiss(false);
    assert!(select_sender.send(Ok(())).is_ok());
    pool.run_until_stalled();
    assert!(deps.consumed.borrow().is_empty());
    assert_eq!(deps.focused.get(), 0);
    assert!(!controller.snapshot().open);

    let failing = Rc::new(Spec::default());
    failing
        .options
        .borrow_mut()
        .push_back(OptionPlan::Ready(Ok(vec![option("pick", None)])));
    let (failure_sender, failure_receiver) = oneshot::channel();
    failing
        .selects
        .borrow_mut()
        .push_back(SelectPlan::Deferred(failure_receiver));
    controller.open("pick".to_owned(), failing, json!({}), segment(4));
    pool.run_until_stalled();
    pool.spawner().spawn_local(controller.select(0)).unwrap();
    pool.run_until_stalled();
    controller.dispose();
    assert!(failure_sender.send(Err("late".to_owned())).is_ok());
    pool.run_until_stalled();
    assert_eq!(controller.snapshot().error, None);
}
