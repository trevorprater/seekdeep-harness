//! Target-portable plugin-card controller projections and credential coordination.

use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

use futures::{FutureExt as _, future::LocalBoxFuture};
use seekdeep_client_runtime::SnapshotStore;
use seekdeep_client_settings_contract::{
    ClientSettingsDisposer, ClientSettingsScope, ClientSettingsScopeSnapshot,
};
use serde::Serialize;
use serde_json::Value;

use crate::{
    CardFieldState, CardForm, CardSecretSpec, CardShell, card_snapshot_store, number_field,
    text_field,
};

/// Agent-loop Settings namespace.
pub const AGENT_LOOP_NS: &str = "agent-loop";
/// Shell Settings namespace shared by the executor families.
pub const SHELL_NS: &str = "shell";
/// `DeepSeek` Web-search Settings namespace.
pub const WEB_SEARCH_NS: &str = "web-search-deepseek";
/// Default credential reference used by the Web-search provider.
pub const DEFAULT_API_KEY_REF: &str = "DEEPSEEK_API_KEY";
/// Write-only field name used inside the Web-search card form.
pub const API_KEY_FIELD: &str = "apiKey";

/// Agent-loop card projection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentLoopCardState {
    /// Shared card shell.
    #[serde(flatten)]
    pub shell: CardShell,
    /// Parallel tool-call cap.
    pub max_parallel_tool_calls: CardFieldState,
}

/// Shell card projection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BashCardState {
    /// Shared card shell.
    #[serde(flatten)]
    pub shell: CardShell,
    /// Foreground command timeout.
    pub timeout_ms: CardFieldState,
    /// Per-stream output cap.
    pub max_output_bytes: CardFieldState,
}

/// Credential state projected without exposing its value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialView {
    /// Whether any provider layer supplies the reference.
    pub configured: bool,
    /// Whether the writable provider can change it.
    pub writable: bool,
}

/// Web-search card projection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebSearchCardState {
    /// Shared card shell.
    #[serde(flatten)]
    pub shell: CardShell,
    /// Provider endpoint.
    #[serde(rename = "baseURL")]
    pub base_url: CardFieldState,
    /// Search budget per request.
    pub max_uses: CardFieldState,
    /// Blank-until-typed credential draft.
    pub api_key: CardFieldState,
    /// Whether the watched reference is configured.
    pub api_key_configured: bool,
    /// Whether the watched reference accepts writes.
    pub api_key_writable: bool,
}

/// Credential RPC boundary used by the Web-search card.
pub trait CardCredentialsTransport {
    /// Reads one reference; `None` means the Host returned no view for it.
    fn describe(
        &self,
        reference: String,
    ) -> LocalBoxFuture<'static, Result<Option<CredentialView>, String>>;
    /// Attempts one credential write. Business and transport failures are returned uniformly.
    fn set(&self, reference: String, value: String) -> LocalBoxFuture<'static, Result<(), String>>;
}

/// Detached task owner used for eager reads and void card actions.
pub trait CardTaskSpawner {
    /// Owns one task through settlement.
    fn spawn(&self, task: LocalBoxFuture<'static, ()>);
}

/// Agent-loop card controller.
pub struct AgentLoopCardController {
    form: Rc<CardForm>,
    store: Rc<SnapshotStore<AgentLoopCardState>>,
}

impl AgentLoopCardController {
    /// Binds the card to one `agent-loop` Settings scope.
    #[must_use]
    pub fn new(scope: Rc<dyn ClientSettingsScope<Value>>) -> Rc<Self> {
        let form = CardForm::new(
            scope,
            vec![number_field("maxParallelToolCalls").into()],
            Vec::new(),
        );
        let store = card_snapshot_store(Self::projection(&form));
        let weak_form = Rc::downgrade(&form);
        let projection_store = store.clone();
        form.subscribe_projection(Rc::new(move || {
            if let Some(form) = weak_form.upgrade() {
                projection_store.set(Self::projection(&form));
            }
        }));
        Rc::new(Self { form, store })
    }

    fn projection(form: &CardForm) -> AgentLoopCardState {
        AgentLoopCardState {
            shell: form.shell(),
            max_parallel_tool_calls: form.field("maxParallelToolCalls"),
        }
    }

    /// Staged form owner.
    #[must_use]
    pub fn form(&self) -> Rc<CardForm> {
        self.form.clone()
    }

    /// Reference-stable observable card projection.
    #[must_use]
    pub fn store(&self) -> Rc<SnapshotStore<AgentLoopCardState>> {
        self.store.clone()
    }
}

/// Shell card controller.
pub struct BashCardController {
    form: Rc<CardForm>,
    store: Rc<SnapshotStore<BashCardState>>,
}

impl BashCardController {
    /// Binds the card to one `shell` Settings scope.
    #[must_use]
    pub fn new(scope: Rc<dyn ClientSettingsScope<Value>>) -> Rc<Self> {
        let form = CardForm::new(
            scope,
            vec![
                number_field("timeoutMs").into(),
                number_field("maxOutputBytes").into(),
            ],
            Vec::new(),
        );
        let store = card_snapshot_store(Self::projection(&form));
        let weak_form = Rc::downgrade(&form);
        let projection_store = store.clone();
        form.subscribe_projection(Rc::new(move || {
            if let Some(form) = weak_form.upgrade() {
                projection_store.set(Self::projection(&form));
            }
        }));
        Rc::new(Self { form, store })
    }

    fn projection(form: &CardForm) -> BashCardState {
        BashCardState {
            shell: form.shell(),
            timeout_ms: form.field("timeoutMs"),
            max_output_bytes: form.field("maxOutputBytes"),
        }
    }

    /// Staged form owner.
    #[must_use]
    pub fn form(&self) -> Rc<CardForm> {
        self.form.clone()
    }

    /// Reference-stable observable card projection.
    #[must_use]
    pub fn store(&self) -> Rc<SnapshotStore<BashCardState>> {
        self.store.clone()
    }
}

#[derive(Clone)]
struct CredentialState {
    reference: String,
    configured: bool,
    writable: bool,
}

impl Default for CredentialState {
    fn default() -> Self {
        Self {
            reference: String::new(),
            configured: false,
            writable: true,
        }
    }
}

/// Web-search card controller with credential read/write coordination.
pub struct WebSearchCardController {
    scope: Rc<dyn ClientSettingsScope<Value>>,
    credentials: Rc<dyn CardCredentialsTransport>,
    spawner: Rc<dyn CardTaskSpawner>,
    form: Rc<CardForm>,
    store: Rc<SnapshotStore<WebSearchCardState>>,
    credential: RefCell<CredentialState>,
    _credential_scope_subscription: ClientSettingsDisposer,
}

impl WebSearchCardController {
    /// Binds Settings fields and the credential domain into one staged card.
    #[must_use]
    pub fn new(
        scope: Rc<dyn ClientSettingsScope<Value>>,
        credentials: Rc<dyn CardCredentialsTransport>,
        spawner: Rc<dyn CardTaskSpawner>,
    ) -> Rc<Self> {
        let controller = Rc::new_cyclic(|weak: &std::rc::Weak<Self>| {
            let writer = weak.clone();
            let secret = CardSecretSpec {
                field: API_KEY_FIELD.to_owned(),
                write: Rc::new(move |value| {
                    let controller = writer.upgrade();
                    async move {
                        let Some(controller) = controller else {
                            return false;
                        };
                        controller.write_key(value).await
                    }
                    .boxed_local()
                }),
            };
            let form = CardForm::new(
                scope.clone(),
                vec![text_field("baseURL").into(), number_field("maxUses").into()],
                vec![secret],
            );
            let credential = CredentialState::default();
            let store = card_snapshot_store(Self::projection(&form, &credential));
            let projection = weak.clone();
            form.subscribe_projection(Rc::new(move || {
                if let Some(controller) = projection.upgrade() {
                    controller.publish();
                }
            }));
            let refresh = weak.clone();
            let credential_scope_subscription = scope.subscribe(Rc::new(move || {
                if let Some(controller) = refresh.upgrade() {
                    controller.spawn_read();
                }
            }));
            Self {
                scope,
                credentials,
                spawner,
                form,
                store,
                credential: RefCell::new(credential),
                _credential_scope_subscription: credential_scope_subscription,
            }
        });
        controller.spawn_read();
        controller
    }

    fn projection(form: &CardForm, credential: &CredentialState) -> WebSearchCardState {
        WebSearchCardState {
            shell: form.shell(),
            base_url: form.field("baseURL"),
            max_uses: form.field("maxUses"),
            api_key: form.field(API_KEY_FIELD),
            api_key_configured: credential.configured,
            api_key_writable: credential.writable,
        }
    }

    fn publish(&self) {
        self.store
            .set(Self::projection(&self.form, &self.credential.borrow()));
    }

    fn spawn_read(self: &Rc<Self>) {
        let controller = self.clone();
        self.spawner.spawn(
            async move {
                controller.read_credential().await;
            }
            .boxed_local(),
        );
    }

    async fn read_credential(&self) {
        let reference = reference_of(&self.scope.snapshot());
        if reference != self.credential.borrow().reference {
            *self.credential.borrow_mut() = CredentialState {
                reference: reference.clone(),
                configured: false,
                writable: true,
            };
            self.publish();
        }
        let Ok(view) = self.credentials.describe(reference.clone()).await else {
            return;
        };
        if reference != reference_of(&self.scope.snapshot()) {
            return;
        }
        let next = CredentialState {
            reference,
            configured: view.as_ref().is_some_and(|view| view.configured),
            writable: view.as_ref().is_none_or(|view| view.writable),
        };
        let current = self.credential.borrow();
        if next.configured == current.configured && next.writable == current.writable {
            return;
        }
        drop(current);
        *self.credential.borrow_mut() = next;
        self.publish();
    }

    async fn write_key(&self, value: String) -> bool {
        let reference = reference_of(&self.scope.snapshot());
        let _ = self.credentials.set(reference, value).await;
        self.read_credential().await;
        self.credential.borrow().configured
    }

    /// Re-reads only when the Host names the currently watched reference.
    pub fn refresh_credential(self: &Rc<Self>, reference: &str) {
        if reference != self.credential.borrow().reference {
            return;
        }
        self.spawn_read();
    }

    /// Staged form owner.
    #[must_use]
    pub fn form(&self) -> Rc<CardForm> {
        self.form.clone()
    }

    /// Reference-stable observable card projection.
    #[must_use]
    pub fn store(&self) -> Rc<SnapshotStore<WebSearchCardState>> {
        self.store.clone()
    }
}

fn reference_of(snapshot: &ClientSettingsScopeSnapshot<Value>) -> String {
    snapshot
        .value
        .as_deref()
        .and_then(Value::as_object)
        .and_then(|value| value.get("apiKeyEnv"))
        .and_then(Value::as_str)
        .filter(|reference| !reference.is_empty())
        .unwrap_or(DEFAULT_API_KEY_REF)
        .to_owned()
}

/// Utility fixture type for tests that describe several credentials at once.
pub type CredentialMap = BTreeMap<String, CredentialView>;
