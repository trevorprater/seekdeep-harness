//! Pinned-source settings scope queue, decoding, recovery, and teardown parity.

#![cfg(not(target_arch = "wasm32"))]

use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    future::Future,
    rc::Rc,
};

use futures::{FutureExt, channel::oneshot, future::LocalBoxFuture};
use seekdeep_client_settings_contract::{
    ClientSettingsMode, ClientSettingsNamespace, ClientSettingsScopeSpec, ClientSettingsStatus,
};
use seekdeep_client_ui_settings::{
    ClientSettingsDescribeValue, ClientSettingsMutateRequest, ClientSettingsNamespaceView,
    ClientSettingsOperationError, ClientSettingsScopeController, ClientSettingsTaskSpawner,
    ClientSettingsTransport, SettingsRpcResult,
};
use seekdeep_schemastery::Schema;
use serde_json::{Value, json};

type DescribeResult = Result<SettingsRpcResult<ClientSettingsDescribeValue>, String>;
type MutateResult = Result<SettingsRpcResult<ClientSettingsNamespaceView>, String>;
type DeferredResult<T> = Result<SettingsRpcResult<T>, String>;
type DeferredResponse<T> = (
    oneshot::Sender<DeferredResult<T>>,
    LocalBoxFuture<'static, DeferredResult<T>>,
);

#[derive(Default)]
struct ScriptedTransport {
    describes: RefCell<VecDeque<LocalBoxFuture<'static, DescribeResult>>>,
    mutations: RefCell<VecDeque<LocalBoxFuture<'static, MutateResult>>>,
    describe_count: Cell<usize>,
    mutate_requests: RefCell<Vec<ClientSettingsMutateRequest>>,
}

impl ScriptedTransport {
    fn push_describe(&self, result: DescribeResult) {
        self.describes
            .borrow_mut()
            .push_back(futures::future::ready(result).boxed_local());
    }

    fn push_mutation(&self, result: MutateResult) {
        self.mutations
            .borrow_mut()
            .push_back(futures::future::ready(result).boxed_local());
    }

    fn push_mutation_future(&self, future: LocalBoxFuture<'static, MutateResult>) {
        self.mutations.borrow_mut().push_back(future);
    }
}

impl ClientSettingsTransport for ScriptedTransport {
    fn describe(&self) -> LocalBoxFuture<'static, DescribeResult> {
        self.describe_count.set(self.describe_count.get() + 1);
        self.describes.borrow_mut().pop_front().unwrap_or_else(|| {
            futures::future::ready(Err("unexpected settings.describe call".to_owned()))
                .boxed_local()
        })
    }

    fn mutate(
        &self,
        request: ClientSettingsMutateRequest,
    ) -> LocalBoxFuture<'static, MutateResult> {
        self.mutate_requests.borrow_mut().push(request);
        self.mutations.borrow_mut().pop_front().unwrap_or_else(|| {
            futures::future::ready(Err("unexpected settings.mutate call".to_owned())).boxed_local()
        })
    }
}

struct LocalSpawner;

impl ClientSettingsTaskSpawner for LocalSpawner {
    fn spawn(&self, task: LocalBoxFuture<'static, ()>) {
        tokio::task::spawn_local(task);
    }
}

async fn run_local(future: impl Future<Output = ()>) {
    tokio::task::LocalSet::new().run_until(future).await;
}

async fn wait_until(mut predicate: impl FnMut() -> bool) {
    for _ in 0..100 {
        if predicate() {
            return;
        }
        tokio::task::yield_now().await;
    }
    assert!(predicate(), "condition did not become true");
}

fn envelope() -> Value {
    Schema::object([(
        "preference",
        Schema::union([
            Schema::constant("light"),
            Schema::constant("dark"),
            Schema::constant("system"),
        ])
        .with_default("system"),
    )])
    .to_json()
}

fn view(value: Value, revision: f64) -> ClientSettingsNamespaceView {
    ClientSettingsNamespaceView {
        ns: "ui-test".to_owned(),
        schema: envelope(),
        value,
        base: None,
        user: None,
        revision,
    }
}

fn described(value: Value, revision: f64) -> SettingsRpcResult<ClientSettingsDescribeValue> {
    SettingsRpcResult::Success(ClientSettingsDescribeValue {
        writable: true,
        namespaces: vec![view(value, revision)],
    })
}

fn controller(
    transport: &Rc<ScriptedTransport>,
    mode: ClientSettingsMode,
    decode: Option<seekdeep_client_settings_contract::ClientSettingsDecoder<Value>>,
) -> Rc<ClientSettingsScopeController> {
    let transport: Rc<dyn ClientSettingsTransport> = transport.clone();
    ClientSettingsScopeController::new(
        transport,
        Rc::new(LocalSpawner),
        ClientSettingsScopeSpec {
            namespace: ClientSettingsNamespace::new("ui-test"),
            decode,
        },
        mode,
    )
}

fn deferred<T: 'static>() -> DeferredResponse<T> {
    let (sender, receiver) = oneshot::channel();
    let future = async move {
        receiver
            .await
            .unwrap_or_else(|_| Err("deferred response sender dropped".to_owned()))
    }
    .boxed_local();
    (sender, future)
}

fn track_values(scope: &Rc<ClientSettingsScopeController>) -> Rc<RefCell<Vec<Option<Value>>>> {
    let initial = scope.snapshot().value.clone();
    let last = Rc::new(RefCell::new(initial.clone()));
    let seen = Rc::new(RefCell::new(vec![initial.as_deref().cloned()]));
    let weak = Rc::downgrade(scope);
    let listener_last = last.clone();
    let listener_seen = seen.clone();
    let _ = scope.subscribe_fallible(Rc::new(move || {
        let Some(scope) = weak.upgrade() else {
            return Ok(());
        };
        let next = scope.snapshot().value.clone();
        let same = match (listener_last.borrow().as_ref(), next.as_ref()) {
            (Some(previous), Some(next)) => Rc::ptr_eq(previous, next),
            (None, None) => true,
            (Some(_), None) | (None, Some(_)) => false,
        };
        if !same {
            listener_seen.borrow_mut().push(next.as_deref().cloned());
            *listener_last.borrow_mut() = next;
        }
        Ok(())
    }));
    seen
}

#[tokio::test(flavor = "current_thread")]
async fn starts_loading_and_publishes_a_schema_valid_section_with_revision_and_writability() {
    run_local(async {
        let transport = Rc::new(ScriptedTransport::default());
        transport.push_describe(Ok(described(json!({"preference": "dark"}), 3.0)));
        let scope = controller(&transport, ClientSettingsMode::Host, None);
        let initial = scope.snapshot();
        assert_eq!(initial.status, ClientSettingsStatus::Loading);
        assert!(initial.value.is_none());
        assert_eq!(initial.revision, None);
        assert!(!initial.writable);
        assert_eq!(initial.mode, ClientSettingsMode::Host);

        scope.load().await.unwrap();
        let snapshot = scope.snapshot();
        assert_eq!(snapshot.status, ClientSettingsStatus::Ready);
        assert_eq!(
            snapshot.value.as_deref(),
            Some(&json!({"preference": "dark"}))
        );
        assert_eq!(snapshot.revision, Some(3.0));
        assert!(snapshot.writable);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn keeps_last_good_value_across_invalid_rejected_and_failed_reads_while_tracking_revisions() {
    run_local(async {
        let transport = Rc::new(ScriptedTransport::default());
        for (value, revision) in [
            (json!({"preference": "dark"}), 3.0),
            (json!({"preference": "sepia"}), 4.0),
            (Value::Null, 5.0),
            (json!("scalar"), 6.0),
            (json!(["queue"]), 7.0),
        ] {
            transport.push_describe(Ok(described(value, revision)));
        }
        transport.push_describe(Ok(SettingsRpcResult::Rejected));
        transport.push_describe(Err("offline".to_owned()));
        let scope = controller(&transport, ClientSettingsMode::Host, None);
        let values = track_values(&scope);
        for _ in 0..7 {
            scope.load().await.unwrap();
        }
        let snapshot = scope.snapshot();
        assert_eq!(snapshot.status, ClientSettingsStatus::Ready);
        assert_eq!(
            snapshot.value.as_deref(),
            Some(&json!({"preference": "dark"}))
        );
        assert_eq!(snapshot.revision, Some(7.0));
        assert_eq!(
            values.borrow().as_slice(),
            [None, Some(json!({"preference": "dark"}))]
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn malformed_schema_envelope_vouches_for_no_section() {
    run_local(async {
        let transport = Rc::new(ScriptedTransport::default());
        let mut broken = view(json!({"preference": "dark"}), 2.0);
        broken.schema = Value::Null;
        transport.push_describe(Ok(SettingsRpcResult::Success(
            ClientSettingsDescribeValue {
                writable: true,
                namespaces: vec![broken],
            },
        )));
        let scope = controller(&transport, ClientSettingsMode::Host, None);
        scope.load().await.unwrap();
        let snapshot = scope.snapshot();
        assert_eq!(snapshot.status, ClientSettingsStatus::Loading);
        assert!(snapshot.value.is_none());
        assert_eq!(snapshot.revision, Some(2.0));
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn suppresses_a_superseded_read_of_an_unexposed_namespace() {
    run_local(async {
        let transport = Rc::new(ScriptedTransport::default());
        transport.push_describe(Ok(SettingsRpcResult::Success(
            ClientSettingsDescribeValue {
                writable: true,
                namespaces: Vec::new(),
            },
        )));
        transport.push_describe(Ok(described(json!({"preference": "dark"}), 1.0)));
        let scope = controller(&transport, ClientSettingsMode::Host, None);
        let statuses = Rc::new(RefCell::new(Vec::new()));
        let status_capture = statuses.clone();
        let weak = Rc::downgrade(&scope);
        let _ = scope.subscribe_fallible(Rc::new(move || {
            if let Some(scope) = weak.upgrade() {
                status_capture.borrow_mut().push(scope.snapshot().status);
            }
            Ok(())
        }));
        let stale = scope.load();
        let fresh = scope.load();
        let (stale, fresh) = futures::join!(stale, fresh);
        stale.unwrap();
        fresh.unwrap();
        assert!(
            !statuses
                .borrow()
                .contains(&ClientSettingsStatus::Unavailable)
        );
        assert_eq!(scope.snapshot().status, ClientSettingsStatus::Ready);
        assert_eq!(
            scope.snapshot().value.as_deref(),
            Some(&json!({"preference": "dark"}))
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn reports_an_unexposed_namespace_as_unavailable_and_recovers_when_it_reappears() {
    run_local(async {
        let transport = Rc::new(ScriptedTransport::default());
        transport.push_describe(Ok(described(json!({"preference": "light"}), 1.0)));
        transport.push_describe(Ok(SettingsRpcResult::Success(
            ClientSettingsDescribeValue {
                writable: true,
                namespaces: Vec::new(),
            },
        )));
        transport.push_describe(Ok(described(json!({"preference": "system"}), 2.0)));
        let scope = controller(&transport, ClientSettingsMode::Host, None);
        scope.load().await.unwrap();
        assert_eq!(scope.snapshot().status, ClientSettingsStatus::Ready);
        scope.load().await.unwrap();
        assert_eq!(scope.snapshot().status, ClientSettingsStatus::Unavailable);
        assert_eq!(
            scope.snapshot().value.as_deref(),
            Some(&json!({"preference": "light"}))
        );
        scope.load().await.unwrap();
        assert_eq!(scope.snapshot().status, ClientSettingsStatus::Ready);
        assert_eq!(
            scope.snapshot().value.as_deref(),
            Some(&json!({"preference": "system"}))
        );
        assert_eq!(scope.snapshot().revision, Some(2.0));
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn custom_decoder_replaces_the_wire_schema_decoder() {
    run_local(async {
        let transport = Rc::new(ScriptedTransport::default());
        transport.push_describe(Ok(described(json!({"preference": "light"}), 1.0)));
        transport.push_describe(Ok(described(json!({"preference": "dark"}), 2.0)));
        let decode = Rc::new(|value: &Value| {
            Ok((value.get("preference") == Some(&json!("dark"))).then(|| value.clone()))
        });
        let scope = controller(&transport, ClientSettingsMode::Host, Some(decode));
        scope.load().await.unwrap();
        assert_eq!(scope.snapshot().status, ClientSettingsStatus::Loading);
        assert_eq!(scope.snapshot().revision, Some(1.0));
        scope.load().await.unwrap();
        assert_eq!(scope.snapshot().status, ClientSettingsStatus::Ready);
        assert_eq!(
            scope.snapshot().value.as_deref(),
            Some(&json!({"preference": "dark"}))
        );
        assert_eq!(scope.snapshot().revision, Some(2.0));
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn serializes_rapid_sets_carries_revisions_and_publishes_only_latest_settlement() {
    run_local(async {
        let transport = Rc::new(ScriptedTransport::default());
        transport.push_describe(Ok(described(json!({"preference": "system"}), 4.0)));
        let (first_sender, first) = deferred();
        transport.push_mutation_future(first);
        transport.push_mutation(Ok(SettingsRpcResult::Success(view(
            json!({"preference": "light"}),
            6.0,
        ))));
        let scope = controller(&transport, ClientSettingsMode::Host, None);
        let values = track_values(&scope);
        scope.load().await.unwrap();
        let dark = scope.set_field("preference", json!("dark"));
        let light = scope.set_field("preference", json!("light"));
        wait_until(|| transport.mutate_requests.borrow().len() == 1).await;
        first_sender
            .send(Ok(SettingsRpcResult::Success(view(
                json!({"preference": "dark"}),
                5.0,
            ))))
            .unwrap();
        let (dark, light) = futures::join!(dark, light);
        dark.unwrap();
        light.unwrap();
        assert_eq!(
            values.borrow().as_slice(),
            [
                None,
                Some(json!({"preference": "system"})),
                Some(json!({"preference": "light"})),
            ]
        );
        let requests = transport.mutate_requests.borrow();
        assert_eq!(requests[0].expected_revision, Some(4.0));
        assert_eq!(requests[1].expected_revision, Some(5.0));
        assert_eq!(scope.snapshot().revision, Some(6.0));
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn recovers_each_latest_rejected_or_thrown_write_from_host_state() {
    run_local(async {
        let transport = Rc::new(ScriptedTransport::default());
        transport.push_mutation(Ok(SettingsRpcResult::Rejected));
        transport.push_mutation(Err("offline".to_owned()));
        transport.push_describe(Ok(described(json!({"preference": "system"}), 2.0)));
        transport.push_describe(Ok(described(json!({"preference": "light"}), 3.0)));
        let scope = controller(&transport, ClientSettingsMode::Host, None);
        let values = track_values(&scope);
        scope.set_field("preference", json!("dark")).await.unwrap();
        scope
            .set_field("preference", json!("system"))
            .await
            .unwrap();
        assert_eq!(
            values.borrow().as_slice(),
            [
                None,
                Some(json!({"preference": "system"})),
                Some(json!({"preference": "light"})),
            ]
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn does_not_recover_superseded_rejected_or_thrown_writes() {
    run_local(async {
        let transport = Rc::new(ScriptedTransport::default());
        transport.push_mutation(Ok(SettingsRpcResult::Rejected));
        transport.push_mutation(Err("offline".to_owned()));
        transport.push_mutation(Ok(SettingsRpcResult::Success(view(
            json!({"preference": "light"}),
            3.0,
        ))));
        let scope = controller(&transport, ClientSettingsMode::Host, None);
        let values = track_values(&scope);
        let dark = scope.set_field("preference", json!("dark"));
        let system = scope.set_field("preference", json!("system"));
        let light = scope.set_field("preference", json!("light"));
        let (dark, system, light) = futures::join!(dark, system, light);
        dark.unwrap();
        system.unwrap();
        light.unwrap();
        assert_eq!(transport.describe_count.get(), 0);
        assert_eq!(
            values.borrow().as_slice(),
            [None, Some(json!({"preference": "light"}))]
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn keeps_write_queue_usable_when_a_subscriber_throws() {
    run_local(async {
        let transport = Rc::new(ScriptedTransport::default());
        transport.push_describe(Ok(described(json!({"preference": "dark"}), 1.0)));
        transport.push_describe(Ok(described(json!({"preference": "light"}), 2.0)));
        let scope = controller(&transport, ClientSettingsMode::Host, None);
        let thrown = Rc::new(Cell::new(false));
        let thrown_capture = thrown.clone();
        let _ = scope.subscribe_fallible(Rc::new(move || {
            if thrown_capture.replace(true) {
                Ok(())
            } else {
                Err(ClientSettingsOperationError::new("subscriber failed"))
            }
        }));
        assert_eq!(
            scope.load().await.unwrap_err().to_string(),
            "subscriber failed"
        );
        scope.load().await.unwrap();
        assert_eq!(
            scope.snapshot().value.as_deref(),
            Some(&json!({"preference": "light"}))
        );
        assert_eq!(scope.snapshot().revision, Some(2.0));
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn cancels_queued_and_post_dispose_writes_while_draining_the_crossing_mutation() {
    run_local(async {
        let transport = Rc::new(ScriptedTransport::default());
        let (first_sender, first) = deferred();
        transport.push_mutation_future(first);
        let scope = controller(&transport, ClientSettingsMode::Host, None);
        let values = track_values(&scope);
        let dark = scope.set_field("preference", json!("dark"));
        wait_until(|| transport.mutate_requests.borrow().len() == 1).await;
        let light = scope.set_field("preference", json!("light"));
        let stopped = Rc::new(Cell::new(false));
        let stopped_capture = stopped.clone();
        let disposing_scope = scope.clone();
        let stop = tokio::task::spawn_local(async move {
            disposing_scope.dispose().await;
            stopped_capture.set(true);
        });
        tokio::task::yield_now().await;
        assert!(!stopped.get());
        first_sender
            .send(Ok(SettingsRpcResult::Success(view(
                json!({"preference": "dark"}),
                1.0,
            ))))
            .unwrap();
        let (dark, light, stop) = futures::join!(dark, light, stop);
        dark.unwrap();
        light.unwrap();
        stop.unwrap();
        scope
            .set_field("preference", json!("system"))
            .await
            .unwrap();
        scope.load().await.unwrap();
        assert_eq!(transport.mutate_requests.borrow().len(), 1);
        assert_eq!(transport.describe_count.get(), 0);
        assert_eq!(values.borrow().as_slice(), [None]);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn remote_browser_stays_in_memory_mode_without_host_calls() {
    run_local(async {
        let transport = Rc::new(ScriptedTransport::default());
        let scope = controller(&transport, ClientSettingsMode::Memory, None);
        let snapshot = scope.snapshot();
        assert_eq!(snapshot.status, ClientSettingsStatus::Unavailable);
        assert!(snapshot.value.is_none());
        assert_eq!(snapshot.revision, None);
        assert!(!snapshot.writable);
        assert_eq!(snapshot.mode, ClientSettingsMode::Memory);
        scope.load().await.unwrap();
        scope.set_field("preference", json!("dark")).await.unwrap();
        scope.dispose().await;
        assert_eq!(transport.describe_count.get(), 0);
        assert!(transport.mutate_requests.borrow().is_empty());
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn carries_composition_base_and_user_layer_into_snapshot() {
    run_local(async {
        let transport = Rc::new(ScriptedTransport::default());
        let mut layered = view(json!({"preference": "dark"}), 3.0);
        layered.base = Some(json!({"preference": "system"}));
        layered.user = Some(json!({"preference": "dark"}));
        transport.push_describe(Ok(SettingsRpcResult::Success(
            ClientSettingsDescribeValue {
                writable: true,
                namespaces: vec![layered],
            },
        )));
        let scope = controller(&transport, ClientSettingsMode::Host, None);
        scope.load().await.unwrap();
        assert_eq!(scope.snapshot().base, Some(json!({"preference": "system"})));
        assert_eq!(scope.snapshot().user, Some(json!({"preference": "dark"})));
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn reports_an_inherited_field_as_absent_from_user_layer() {
    run_local(async {
        let transport = Rc::new(ScriptedTransport::default());
        let mut inherited = view(json!({"preference": "system"}), 1.0);
        inherited.base = Some(json!({"preference": "system"}));
        transport.push_describe(Ok(SettingsRpcResult::Success(
            ClientSettingsDescribeValue {
                writable: true,
                namespaces: vec![inherited],
            },
        )));
        let scope = controller(&transport, ClientSettingsMode::Host, None);
        scope.load().await.unwrap();
        assert!(scope.snapshot().user.is_none());
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn clears_one_field_through_an_unset_op_fenced_by_the_held_revision() {
    run_local(async {
        let transport = Rc::new(ScriptedTransport::default());
        transport.push_describe(Ok(described(json!({"preference": "dark"}), 3.0)));
        transport.push_mutation(Ok(SettingsRpcResult::Success(view(
            json!({"preference": "system"}),
            4.0,
        ))));
        let scope = controller(&transport, ClientSettingsMode::Host, None);
        scope.load().await.unwrap();
        scope.unset_field("preference").await.unwrap();
        let requests = transport.mutate_requests.borrow();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].expected_revision, Some(3.0));
        assert_eq!(
            serde_json::to_value(&requests[0].ops).unwrap(),
            json!([{"op": "unset", "path": ["preference"]}])
        );
        assert_eq!(
            scope.snapshot().value.as_deref(),
            Some(&json!({"preference": "system"}))
        );
        assert_eq!(scope.snapshot().revision, Some(4.0));
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn recovers_host_state_when_the_latest_clear_is_refused() {
    run_local(async {
        let transport = Rc::new(ScriptedTransport::default());
        transport.push_describe(Ok(described(json!({"preference": "dark"}), 3.0)));
        transport.push_mutation(Ok(SettingsRpcResult::Rejected));
        transport.push_describe(Ok(described(json!({"preference": "light"}), 5.0)));
        let scope = controller(&transport, ClientSettingsMode::Host, None);
        scope.load().await.unwrap();
        scope.unset_field("preference").await.unwrap();
        assert_eq!(
            scope.snapshot().value.as_deref(),
            Some(&json!({"preference": "light"}))
        );
        assert_eq!(scope.snapshot().revision, Some(5.0));
    })
    .await;
}
