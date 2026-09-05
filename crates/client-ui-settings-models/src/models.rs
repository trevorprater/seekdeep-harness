//! Provider/settings/credential join and onboarding readiness.

use std::{cell::Cell, collections::BTreeMap, rc::Rc};

use futures::future::LocalBoxFuture;
use indexmap::IndexMap;
use seekdeep_client_runtime::{SnapshotStore, StoreFlushMode, StoreFlushScheduler, StoreLogger};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// One configurable provider-directory entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigurableProviderView {
    /// Provider route id.
    pub provider: String,
    /// Human-facing provider name.
    pub display_name: String,
    /// Settings namespace, empty when the provider has no settings address.
    pub settings_ns: String,
    /// Path from namespace root to this provider profile.
    pub settings_path: Vec<String>,
    /// Authentication setup declared by the owning adapter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authentication: Option<String>,
    /// Whether an adapter currently serves the route.
    pub active: bool,
    /// Whether the adapter knows this route only from user configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared: Option<bool>,
}

/// One credential-reference description.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialView {
    /// Whether a resolver currently supplies the credential.
    pub configured: bool,
    /// Whether the Host accepts writes for this reference.
    pub writable: bool,
    /// Optional winning source label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// JSON field that distinguishes wire omission from an explicit `null` value.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum OptionalJsonValue {
    /// The property was absent from the wire object.
    #[default]
    Missing,
    /// The property was present, including when its value was JSON `null`.
    Present(Value),
}

impl OptionalJsonValue {
    fn is_missing(&self) -> bool {
        matches!(self, Self::Missing)
    }

    fn as_value(&self) -> Option<&Value> {
        match self {
            Self::Missing => None,
            Self::Present(value) => Some(value),
        }
    }
}

impl From<Value> for OptionalJsonValue {
    fn from(value: Value) -> Self {
        Self::Present(value)
    }
}

impl Serialize for OptionalJsonValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Missing => serializer.serialize_unit(),
            Self::Present(value) => value.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for OptionalJsonValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Value::deserialize(deserializer).map(Self::Present)
    }
}

/// One settings namespace view used by the Models editor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingsNamespaceView {
    /// Namespace id.
    pub ns: String,
    /// Serialized schema.
    #[serde(default)]
    pub schema: Value,
    /// Effective value.
    #[serde(default)]
    pub value: Value,
    /// Composition base layer.
    #[serde(default, skip_serializing_if = "OptionalJsonValue::is_missing")]
    pub base: OptionalJsonValue,
    /// User-owned layer.
    #[serde(default, skip_serializing_if = "OptionalJsonValue::is_missing")]
    pub user: OptionalJsonValue,
    /// Whether changes apply live or require restart.
    #[serde(default)]
    pub applies: String,
    /// Redacted schema-declared secret slots.
    #[serde(default)]
    pub secrets: Vec<Value>,
    /// Monotonic raw user-section revision.
    #[serde(default)]
    pub revision: u64,
}

/// One joined provider row rendered by the page.
#[derive(Clone, Debug, PartialEq)]
pub struct ProviderRow {
    /// Provider directory entry.
    pub entry: ConfigurableProviderView,
    /// Whether an effective profile exists.
    pub configured: bool,
    /// Whether removal reveals no composition base profile.
    pub removable: bool,
    /// Credential reference named by the effective profile.
    pub api_key_env: Option<String>,
    /// Described credential state.
    pub credential: Option<CredentialView>,
}

/// Page load status.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ModelsStatus {
    /// No load has started.
    #[default]
    Idle,
    /// Provider and settings calls are pending.
    Loading,
    /// Complete provider rows are available.
    Ready,
    /// Provider or settings loading failed.
    Error,
}

/// Complete Models settings snapshot.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelsSettingsState {
    /// Current load status.
    pub status: ModelsStatus,
    /// Whole-load failure, retaining last-good rows.
    pub error: Option<String>,
    /// Credential enrichment failure.
    pub credential_error: Option<String>,
    /// Whether settings accepts writes.
    pub writable: bool,
    /// Joined provider rows.
    pub rows: Vec<ProviderRow>,
    /// Namespace views by id.
    pub namespaces: IndexMap<String, SettingsNamespaceView>,
}

struct NoopScheduler;

impl StoreFlushScheduler for NoopScheduler {
    fn queue(&self, callback: Box<dyn FnOnce()>) {
        callback();
    }
}

fn snapshot_store<T: Clone + 'static>(initial: T) -> Rc<SnapshotStore<T>> {
    SnapshotStore::new(
        initial,
        StoreFlushMode::Sync,
        Rc::new(NoopScheduler),
        None,
        Rc::new(|_| {}) as StoreLogger,
    )
}

/// API calls required for one Models-page refresh.
pub trait ModelsTransport {
    /// Lists configurable provider routes.
    fn providers(&self) -> LocalBoxFuture<'static, Result<Vec<ConfigurableProviderView>, String>>;
    /// Describes every settings namespace and write capability.
    fn settings(
        &self,
    ) -> LocalBoxFuture<'static, Result<(bool, Vec<SettingsNamespaceView>), String>>;
    /// Describes a de-duplicated credential reference batch.
    fn credentials(
        &self,
        references: Vec<String>,
    ) -> LocalBoxFuture<'static, Result<BTreeMap<String, CredentialView>, String>>;
}

/// Models settings page controller.
pub struct ModelsSettingsStore {
    /// Immutable observable snapshot source.
    pub store: Rc<SnapshotStore<ModelsSettingsState>>,
    transport: Rc<dyn ModelsTransport>,
    generation: Cell<u64>,
}

impl std::fmt::Debug for ModelsSettingsStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ModelsSettingsStore")
            .field("generation", &self.generation.get())
            .finish_non_exhaustive()
    }
}

impl ModelsSettingsStore {
    /// Creates an idle page controller.
    #[must_use]
    pub fn new(transport: Rc<dyn ModelsTransport>) -> Rc<Self> {
        Rc::new(Self {
            store: snapshot_store(ModelsSettingsState::default()),
            transport,
            generation: Cell::new(0),
        })
    }

    /// Refreshes directory and settings in parallel, then enriches credentials.
    pub async fn load(&self) {
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        self.store.update(|state| {
            state.status = ModelsStatus::Loading;
            state.error = None;
        });
        let (providers, (writable, views)) =
            match futures::future::try_join(self.transport.providers(), self.transport.settings())
                .await
            {
                Ok(loaded) => loaded,
                Err(error) => {
                    if self.generation.get() == generation {
                        self.store.update(|state| {
                            state.status = ModelsStatus::Error;
                            state.error = Some(error);
                        });
                    }
                    return;
                }
            };
        let namespaces = views
            .into_iter()
            .map(|view| (view.ns.clone(), view))
            .collect::<IndexMap<_, _>>();
        let mut rows = providers
            .into_iter()
            .map(|entry| join_row(entry, &namespaces))
            .collect::<Vec<_>>();
        let mut references = Vec::new();
        for reference in rows.iter().filter_map(|row| row.api_key_env.as_ref()) {
            if !references.contains(reference) {
                references.push(reference.clone());
            }
        }
        let (credentials, credential_error) = if references.is_empty() {
            (BTreeMap::new(), None)
        } else {
            match self.transport.credentials(references).await {
                Ok(credentials) => (credentials, None),
                Err(error) => (BTreeMap::new(), Some(error)),
            }
        };
        if self.generation.get() != generation {
            return;
        }
        for row in &mut rows {
            row.credential = row
                .api_key_env
                .as_ref()
                .and_then(|reference| credentials.get(reference).cloned());
        }
        self.store.set(ModelsSettingsState {
            status: ModelsStatus::Ready,
            error: None,
            credential_error,
            writable,
            rows,
            namespaces,
        });
    }
}

fn path_value<'a>(value: &'a Value, path: &[String]) -> Option<&'a Value> {
    path.iter().try_fold(value, |value, segment| {
        value.as_object().and_then(|object| object.get(segment))
    })
}

fn join_row(
    entry: ConfigurableProviderView,
    namespaces: &IndexMap<String, SettingsNamespaceView>,
) -> ProviderRow {
    let namespace = namespaces.get(&entry.settings_ns);
    let configured = namespace.is_some_and(|namespace| {
        entry.settings_path.is_empty()
            || path_value(&namespace.value, &entry.settings_path).is_some()
    });
    let removable = namespace.is_some_and(|namespace| {
        !entry.settings_path.is_empty()
            && namespace
                .user
                .as_value()
                .and_then(|user| path_value(user, &entry.settings_path))
                .is_some()
            && namespace
                .base
                .as_value()
                .and_then(|base| path_value(base, &entry.settings_path))
                .is_none()
    });
    let api_key_env = namespace
        .and_then(|namespace| path_value(&namespace.value, &entry.settings_path))
        .and_then(Value::as_object)
        .and_then(|profile| profile.get("apiKeyEnv"))
        .and_then(Value::as_str)
        .filter(|reference| !reference.is_empty())
        .map(ToOwned::to_owned);
    ProviderRow {
        entry,
        configured,
        removable,
        api_key_env,
        credential: None,
    }
}

/// Derives the conventional credential reference for one provider route.
#[must_use]
pub fn derive_key_ref(provider: &str) -> String {
    let mut result = String::new();
    let mut separator = false;
    for character in provider.chars().flat_map(char::to_uppercase) {
        if character.is_ascii_uppercase() || character.is_ascii_digit() {
            if separator {
                result.push('_');
            }
            separator = false;
            result.push(character);
        } else {
            separator = true;
        }
    }
    if separator {
        result.push('_');
    }
    result.push_str("_API_KEY");
    result
}

/// Whether one joined row can currently serve model requests.
#[must_use]
pub fn provider_usable(row: &ProviderRow) -> bool {
    row.entry.active
        && row.api_key_env.as_ref().is_none_or(|_| {
            row.credential
                .as_ref()
                .is_some_and(|credential| credential.configured)
        })
}

/// Diagnostic reason for an unavailable onboarding repair path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnboardingUnavailableReason {
    /// Provider/settings join failed.
    LoadFailed,
    /// Official provider route exists but has no active adapter.
    ProviderInactive,
    /// Credential state could not be read.
    CredentialsUnavailable,
    /// Settings is read-only.
    SettingsReadOnly,
    /// Credential resolver is read-only.
    CredentialReadOnly,
}

/// First-run readiness projected from the shared Models join.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnboardingReadiness {
    /// First join is pending.
    Loading,
    /// Official configurable provider declaration is absent.
    AdapterAbsent,
    /// At least one provider can serve requests.
    ProviderReady,
    /// Official provider can be repaired by entering a credential.
    CredentialMissing,
    /// Onboarding cannot repair the current state.
    Unavailable(OnboardingUnavailableReason),
}

/// Projects first-run readiness from the Models page snapshot.
#[must_use]
pub fn onboarding_readiness(state: &ModelsSettingsState) -> OnboardingReadiness {
    if matches!(state.status, ModelsStatus::Idle | ModelsStatus::Loading) && state.rows.is_empty() {
        return OnboardingReadiness::Loading;
    }
    if state.status == ModelsStatus::Error {
        return OnboardingReadiness::Unavailable(OnboardingUnavailableReason::LoadFailed);
    }
    if state.rows.iter().any(provider_usable) {
        return OnboardingReadiness::ProviderReady;
    }
    let Some(row) = state.rows.iter().find(|row| {
        row.entry.provider == "deepseek-official"
            && row.entry.settings_ns == "llm-deepseek"
            && row.entry.settings_path.is_empty()
    }) else {
        return OnboardingReadiness::AdapterAbsent;
    };
    if !row.entry.active {
        return OnboardingReadiness::Unavailable(OnboardingUnavailableReason::ProviderInactive);
    }
    if state.credential_error.is_some() || row.credential.is_none() {
        return OnboardingReadiness::Unavailable(
            OnboardingUnavailableReason::CredentialsUnavailable,
        );
    }
    if !state.writable {
        return OnboardingReadiness::Unavailable(OnboardingUnavailableReason::SettingsReadOnly);
    }
    if !row
        .credential
        .as_ref()
        .is_some_and(|credential| credential.writable)
    {
        return OnboardingReadiness::Unavailable(OnboardingUnavailableReason::CredentialReadOnly);
    }
    OnboardingReadiness::CredentialMissing
}
