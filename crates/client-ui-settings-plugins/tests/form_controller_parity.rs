//! Staged form, card projection, and credential-controller source parity.

use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
};

use futures::{FutureExt as _, future::LocalBoxFuture};
use seekdeep_client_settings_contract::{
    ClientSettingsDisposer, ClientSettingsMode, ClientSettingsScope, ClientSettingsScopeSnapshot,
    ClientSettingsStatus,
};
use seekdeep_client_ui_settings_plugins::{
    AgentLoopCardController, BashCardController, CardCredentialsTransport, CardFieldSpec, CardForm,
    CardTaskSpawner, CredentialView, DEFAULT_API_KEY_REF, WebSearchCardController, number_field,
    text_field,
};
use serde_json::{Value, json};

#[derive(Clone)]
struct ScopeFixture {
    inner: Rc<ScopeInner>,
}

struct ScopeInner {
    snapshot: RefCell<Rc<ClientSettingsScopeSnapshot<Value>>>,
    listeners: RefCell<Vec<Rc<dyn Fn()>>>,
    calls: RefCell<Vec<(String, String, Option<Value>)>>,
    accept: Cell<bool>,
}

impl ScopeFixture {
    fn new(value: Value, base: Option<Value>, user: Option<Value>) -> Rc<Self> {
        Rc::new(Self {
            inner: Rc::new(ScopeInner {
                snapshot: RefCell::new(Rc::new(ClientSettingsScopeSnapshot {
                    status: ClientSettingsStatus::Ready,
                    value: Some(Rc::new(value)),
                    base,
                    user,
                    revision: Some(1.0),
                    writable: true,
                    mode: ClientSettingsMode::Host,
                })),
                listeners: RefCell::new(Vec::new()),
                calls: RefCell::new(Vec::new()),
                accept: Cell::new(true),
            }),
        })
    }

    fn publish(&self, snapshot: ClientSettingsScopeSnapshot<Value>) {
        *self.inner.snapshot.borrow_mut() = Rc::new(snapshot);
        for listener in self.inner.listeners.borrow().iter() {
            listener();
        }
    }

    fn set_writable(&self, writable: bool) {
        let current = self.snapshot();
        self.publish(ClientSettingsScopeSnapshot {
            status: current.status,
            value: current.value.clone(),
            base: current.base.clone(),
            user: current.user.clone(),
            revision: current.revision,
            writable,
            mode: current.mode,
        });
    }

    fn set_value(&self, value: Value) {
        let current = self.snapshot();
        self.publish(ClientSettingsScopeSnapshot {
            status: current.status,
            value: Some(Rc::new(value)),
            base: current.base.clone(),
            user: current.user.clone(),
            revision: current.revision,
            writable: current.writable,
            mode: current.mode,
        });
    }
}

impl ClientSettingsScope<Value> for ScopeFixture {
    fn snapshot(&self) -> Rc<ClientSettingsScopeSnapshot<Value>> {
        self.inner.snapshot.borrow().clone()
    }

    fn subscribe(&self, listener: Rc<dyn Fn()>) -> ClientSettingsDisposer {
        self.inner.listeners.borrow_mut().push(listener);
        ClientSettingsDisposer::new(|| {})
    }

    fn set(&self, field: String, value: Value) -> LocalBoxFuture<'static, Result<(), String>> {
        let fixture = self.clone();
        async move {
            fixture.inner.calls.borrow_mut().push((
                "set".to_owned(),
                field.clone(),
                Some(value.clone()),
            ));
            if fixture.inner.accept.get() {
                let current = fixture.snapshot();
                let mut effective = current
                    .value
                    .as_deref()
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                effective.insert(field.clone(), value.clone());
                let mut user = current
                    .user
                    .as_ref()
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                user.insert(field, value);
                fixture.publish(ClientSettingsScopeSnapshot {
                    status: current.status,
                    value: Some(Rc::new(Value::Object(effective))),
                    base: current.base.clone(),
                    user: Some(Value::Object(user)),
                    revision: current.revision.map(|revision| revision + 1.0),
                    writable: current.writable,
                    mode: current.mode,
                });
            }
            Ok(())
        }
        .boxed_local()
    }

    fn unset(&self, field: String) -> LocalBoxFuture<'static, Result<(), String>> {
        let fixture = self.clone();
        async move {
            fixture
                .inner
                .calls
                .borrow_mut()
                .push(("unset".to_owned(), field.clone(), None));
            if fixture.inner.accept.get() {
                let current = fixture.snapshot();
                let mut user = current
                    .user
                    .as_ref()
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                user.remove(&field);
                let inherited = current
                    .base
                    .as_ref()
                    .and_then(Value::as_object)
                    .and_then(|base| base.get(&field))
                    .cloned();
                let mut effective = current
                    .value
                    .as_deref()
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                if let Some(inherited) = inherited {
                    effective.insert(field, inherited);
                } else {
                    effective.remove(&field);
                }
                fixture.publish(ClientSettingsScopeSnapshot {
                    status: current.status,
                    value: Some(Rc::new(Value::Object(effective))),
                    base: current.base.clone(),
                    user: Some(Value::Object(user)),
                    revision: current.revision.map(|revision| revision + 1.0),
                    writable: current.writable,
                    mode: current.mode,
                });
            }
            Ok(())
        }
        .boxed_local()
    }
}

#[derive(Default)]
struct ManualSpawner {
    tasks: RefCell<Vec<LocalBoxFuture<'static, ()>>>,
}

impl ManualSpawner {
    fn drain(&self) {
        for task in self.tasks.borrow_mut().drain(..) {
            futures::executor::block_on(task);
        }
    }
}

impl CardTaskSpawner for ManualSpawner {
    fn spawn(&self, task: LocalBoxFuture<'static, ()>) {
        self.tasks.borrow_mut().push(task);
    }
}

#[derive(Default)]
struct CredentialFixture {
    views: RefCell<BTreeMap<String, CredentialView>>,
    describes: RefCell<Vec<String>>,
    sets: RefCell<Vec<(String, String)>>,
    reject_reads: Cell<bool>,
    accept_writes: Cell<bool>,
}

impl CardCredentialsTransport for CredentialFixture {
    fn describe(
        &self,
        reference: String,
    ) -> LocalBoxFuture<'static, Result<Option<CredentialView>, String>> {
        self.describes.borrow_mut().push(reference.clone());
        let rejected = self.reject_reads.get();
        let view = self.views.borrow().get(&reference).cloned();
        async move {
            if rejected {
                Err("credential read failed".to_owned())
            } else {
                Ok(view)
            }
        }
        .boxed_local()
    }

    fn set(&self, reference: String, value: String) -> LocalBoxFuture<'static, Result<(), String>> {
        self.sets.borrow_mut().push((reference.clone(), value));
        if self.accept_writes.get() {
            self.views.borrow_mut().insert(
                reference,
                CredentialView {
                    configured: true,
                    writable: true,
                },
            );
        }
        async { Ok(()) }.boxed_local()
    }
}

#[test]
fn form_stages_in_order_and_reads_back_host_authority() {
    let scope = ScopeFixture::new(
        json!({"timeoutMs":60000,"baseURL":"https://search.test/v1"}),
        Some(json!({"timeoutMs":60000,"baseURL":"https://search.test/v1"})),
        Some(json!({})),
    );
    let form = CardForm::new(
        scope.clone(),
        vec![
            CardFieldSpec::Number(number_field("timeoutMs")),
            CardFieldSpec::Text(text_field("baseURL")),
        ],
        Vec::new(),
    );
    assert_eq!(form.field("timeoutMs").text, "60000");
    form.edit("timeoutMs", "9000");
    form.edit("baseURL", "  https://other.test  ");
    assert!(form.shell().dirty);
    assert!(scope.inner.calls.borrow().is_empty());

    futures::executor::block_on(form.save()).unwrap();

    assert_eq!(
        scope.inner.calls.borrow().as_slice(),
        [
            (
                "set".to_owned(),
                "timeoutMs".to_owned(),
                Some(json!(9000.0))
            ),
            (
                "set".to_owned(),
                "baseURL".to_owned(),
                Some(json!("https://other.test"))
            )
        ]
    );
    assert!(!form.shell().dirty);
    assert!(!form.shell().failed);
}

#[test]
fn form_refuses_invalid_saves_and_keeps_rejected_drafts() {
    let scope = ScopeFixture::new(json!({"timeoutMs":60000}), Some(json!({})), Some(json!({})));
    let form = CardForm::new(
        scope.clone(),
        vec![number_field("timeoutMs").into()],
        Vec::new(),
    );
    form.edit("timeoutMs", "soon");
    assert!(form.shell().invalid);
    futures::executor::block_on(form.save()).unwrap();
    assert!(scope.inner.calls.borrow().is_empty());

    form.edit("timeoutMs", "9000");
    scope.inner.accept.set(false);
    futures::executor::block_on(form.save()).unwrap();
    assert!(form.shell().failed);
    assert!(form.shell().dirty);
    assert_eq!(form.field("timeoutMs").text, "9000");
    form.edit("timeoutMs", "9001");
    assert!(!form.shell().failed);
    form.discard();
    assert_eq!(form.field("timeoutMs").text, "60000");
}

#[test]
fn reset_is_staged_and_unknown_fields_fail_loud() {
    let scope = ScopeFixture::new(
        json!({"timeoutMs":9000}),
        Some(json!({"timeoutMs":60000})),
        Some(json!({"timeoutMs":9000})),
    );
    let form = CardForm::new(
        scope.clone(),
        vec![number_field("timeoutMs").into()],
        Vec::new(),
    );
    form.reset_field("timeoutMs");
    assert_eq!(form.field("timeoutMs").text, "60000");
    assert!(!form.field("timeoutMs").overridden);
    assert!(scope.inner.calls.borrow().is_empty());
    futures::executor::block_on(form.save()).unwrap();
    assert_eq!(scope.inner.calls.borrow()[0].0, "unset");
    assert_eq!(
        form.try_reset_field("missing").unwrap_err(),
        "plugin card has no field missing"
    );
}

#[test]
fn bash_and_agent_controllers_publish_scope_and_draft_changes() {
    let bash_scope = ScopeFixture::new(
        json!({"timeoutMs":5000,"maxOutputBytes":64000}),
        Some(json!({"timeoutMs":60000,"maxOutputBytes":64000})),
        Some(json!({"timeoutMs":5000})),
    );
    let bash = BashCardController::new(bash_scope.clone());
    assert!(bash.store().snapshot().timeout_ms.overridden);
    bash.form().edit("maxOutputBytes", "1024");
    assert_eq!(bash.store().snapshot().max_output_bytes.text, "1024");
    bash_scope.set_writable(false);
    assert!(!bash.store().snapshot().shell.writable);

    let agent_scope = ScopeFixture::new(
        json!({"maxParallelToolCalls":10}),
        Some(json!({"maxParallelToolCalls":10})),
        Some(json!({})),
    );
    let agent = AgentLoopCardController::new(agent_scope);
    agent.form().edit("maxParallelToolCalls", "4");
    futures::executor::block_on(agent.form().save()).unwrap();
    assert_eq!(agent.store().snapshot().max_parallel_tool_calls.text, "4");
}

#[test]
fn web_search_tracks_reference_changes_and_stages_credentials() {
    let scope = ScopeFixture::new(json!({}), Some(json!({})), Some(json!({})));
    let credentials = Rc::new(CredentialFixture::default());
    credentials.views.borrow_mut().insert(
        DEFAULT_API_KEY_REF.to_owned(),
        CredentialView {
            configured: false,
            writable: true,
        },
    );
    credentials.accept_writes.set(true);
    let spawner = Rc::new(ManualSpawner::default());
    let controller =
        WebSearchCardController::new(scope.clone(), credentials.clone(), spawner.clone());
    spawner.drain();
    assert!(!controller.store().snapshot().api_key_configured);

    controller.form().edit("apiKey", " ds-secret ");
    futures::executor::block_on(controller.form().save()).unwrap();
    assert_eq!(
        credentials.sets.borrow().as_slice(),
        [(DEFAULT_API_KEY_REF.to_owned(), "ds-secret".to_owned())]
    );
    assert!(controller.store().snapshot().api_key_configured);

    scope.set_value(json!({"apiKeyEnv":"SEARCH_KEY"}));
    spawner.drain();
    assert_eq!(credentials.describes.borrow().last().unwrap(), "SEARCH_KEY");
    controller.refresh_credential("OTHER_KEY");
    assert!(spawner.tasks.borrow().is_empty());
    controller.refresh_credential("SEARCH_KEY");
    assert_eq!(spawner.tasks.borrow().len(), 1);
}

#[test]
fn web_search_contains_credential_read_failures() {
    let scope = ScopeFixture::new(
        json!({"baseURL":"https://search.test/v1"}),
        Some(json!({})),
        Some(json!({})),
    );
    let credentials = Rc::new(CredentialFixture::default());
    credentials.reject_reads.set(true);
    let spawner = Rc::new(ManualSpawner::default());
    let controller = WebSearchCardController::new(scope, credentials, spawner.clone());
    spawner.drain();
    assert_eq!(
        controller.store().snapshot().base_url.text,
        "https://search.test/v1"
    );
    assert!(!controller.store().snapshot().api_key_configured);
    assert!(controller.store().snapshot().api_key_writable);
}
