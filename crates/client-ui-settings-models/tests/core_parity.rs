//! API-key, provider join, readiness, and welcome-store source parity.

use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

use futures::{FutureExt as _, future::LocalBoxFuture};
use seekdeep_client_ui_settings_models::{
    ApiKeyFailureKey, ConfigurableProviderView, CredentialView, ModelsSettingsState,
    ModelsSettingsStore, ModelsStatus, ModelsTransport, OnboardingReadiness,
    OnboardingUnavailableReason, ProviderRow, SettingsNamespaceView, WelcomeNoticeStore,
    WelcomePersistence, WelcomeStatus, WelcomeTransport, api_key_failure, derive_key_ref,
    onboarding_readiness, provider_usable,
};
use serde_json::json;

#[test]
fn api_key_judgment_matches_empty_blank_wrapped_assignment_and_ascii_rules() {
    for valid in ["", "sk-live", " sk-live ", "ABCD==", "\"", "\"abc"] {
        assert_eq!(api_key_failure(valid), None, "{valid:?}");
    }
    for blank in [" ", "\t\n", "\u{00a0}"] {
        assert_eq!(
            api_key_failure(blank),
            Some(ApiKeyFailureKey::KeyBlank),
            "{blank:?}"
        );
    }
    for invalid in [
        "DEEPSEEK_API_KEY=sk-live",
        "'sk-live'",
        "`sk-live`",
        "sk live",
        "sk-密钥",
        "sk-\u{007f}",
    ] {
        assert_eq!(
            api_key_failure(invalid),
            Some(ApiKeyFailureKey::KeyIllegalCharacters),
            "{invalid:?}"
        );
    }
}

fn credential(configured: bool, writable: bool) -> CredentialView {
    CredentialView {
        configured,
        writable,
        source: None,
    }
}

fn provider(
    provider: &str,
    namespace: &str,
    path: &[&str],
    active: bool,
) -> ConfigurableProviderView {
    ConfigurableProviderView {
        provider: provider.to_owned(),
        display_name: provider.to_owned(),
        settings_ns: namespace.to_owned(),
        settings_path: path.iter().map(ToString::to_string).collect(),
        active,
    }
}

fn row() -> ProviderRow {
    ProviderRow {
        entry: provider("deepseek-official", "llm-deepseek", &[], true),
        configured: true,
        removable: false,
        api_key_env: Some("DEEPSEEK_API_KEY".to_owned()),
        credential: Some(credential(false, true)),
    }
}

fn state(rows: Vec<ProviderRow>) -> ModelsSettingsState {
    ModelsSettingsState {
        status: ModelsStatus::Ready,
        error: None,
        credential_error: None,
        writable: true,
        rows,
        namespaces: BTreeMap::new(),
    }
}

#[test]
fn readiness_requires_active_credentials_and_any_usable_provider_ends_onboarding() {
    let official = row();
    assert_eq!(
        onboarding_readiness(&ModelsSettingsState::default()),
        OnboardingReadiness::Loading
    );
    let loading = ModelsSettingsState {
        status: ModelsStatus::Loading,
        ..ModelsSettingsState::default()
    };
    assert_eq!(onboarding_readiness(&loading), OnboardingReadiness::Loading);
    assert_eq!(
        onboarding_readiness(&state(Vec::new())),
        OnboardingReadiness::AdapterAbsent
    );
    let mut wrong_namespace = row();
    wrong_namespace.entry.settings_ns.clear();
    assert_eq!(
        onboarding_readiness(&state(vec![wrong_namespace])),
        OnboardingReadiness::AdapterAbsent
    );
    assert!(!provider_usable(&official));
    assert_eq!(
        onboarding_readiness(&state(vec![official.clone()])),
        OnboardingReadiness::CredentialMissing
    );
    let mut other = ProviderRow {
        entry: provider("hfai", "llm-pi-ai", &["providers", "hfai"], true),
        configured: true,
        removable: true,
        api_key_env: Some("HFAI_API_KEY".to_owned()),
        credential: Some(credential(true, true)),
    };
    assert!(provider_usable(&other));
    assert_eq!(
        onboarding_readiness(&state(vec![official.clone(), other.clone()])),
        OnboardingReadiness::ProviderReady
    );
    other.entry.active = false;
    assert!(!provider_usable(&other));
    other.entry.active = true;
    other.api_key_env = None;
    other.credential = None;
    assert!(provider_usable(&other));

    let mut unavailable = state(vec![official.clone()]);
    unavailable.status = ModelsStatus::Error;
    assert_eq!(
        onboarding_readiness(&unavailable),
        OnboardingReadiness::Unavailable(OnboardingUnavailableReason::LoadFailed)
    );
    unavailable = state(vec![official.clone()]);
    unavailable.credential_error = Some("offline".to_owned());
    assert_eq!(
        onboarding_readiness(&unavailable),
        OnboardingReadiness::Unavailable(OnboardingUnavailableReason::CredentialsUnavailable)
    );
    unavailable = state(vec![official]);
    unavailable.writable = false;
    assert_eq!(
        onboarding_readiness(&unavailable),
        OnboardingReadiness::Unavailable(OnboardingUnavailableReason::SettingsReadOnly)
    );
    let mut inactive = row();
    inactive.entry.active = false;
    assert_eq!(
        onboarding_readiness(&state(vec![inactive])),
        OnboardingReadiness::Unavailable(OnboardingUnavailableReason::ProviderInactive)
    );
    let mut unreadable = row();
    unreadable.credential = None;
    assert_eq!(
        onboarding_readiness(&state(vec![unreadable])),
        OnboardingReadiness::Unavailable(OnboardingUnavailableReason::CredentialsUnavailable)
    );
    let mut read_only = row();
    read_only.credential = Some(credential(false, false));
    assert_eq!(
        onboarding_readiness(&state(vec![read_only])),
        OnboardingReadiness::Unavailable(OnboardingUnavailableReason::CredentialReadOnly)
    );
    let mut configured = row();
    configured.credential = Some(CredentialView {
        configured: true,
        writable: false,
        source: Some("env".to_owned()),
    });
    assert_eq!(
        onboarding_readiness(&state(vec![configured])),
        OnboardingReadiness::ProviderReady
    );
    assert_eq!(derive_key_ref("minimax-cn"), "MINIMAX_CN_API_KEY");
}

struct ModelsFixture {
    credential_failure: bool,
    credential_refs: Rc<RefCell<Vec<Vec<String>>>>,
}

impl ModelsTransport for ModelsFixture {
    fn providers(&self) -> LocalBoxFuture<'static, Result<Vec<ConfigurableProviderView>, String>> {
        async {
            Ok(vec![
                provider("deepseek-official", "llm-deepseek", &[], true),
                provider("openai", "llm-pi-ai", &["providers", "openai"], true),
                provider("anthropic", "llm-pi-ai", &["providers", "anthropic"], false),
            ])
        }
        .boxed_local()
    }

    fn settings(
        &self,
    ) -> LocalBoxFuture<'static, Result<(bool, Vec<SettingsNamespaceView>), String>> {
        async {
            Ok((
                true,
                vec![
                    SettingsNamespaceView {
                        ns: "llm-deepseek".to_owned(),
                        schema: json!({}),
                        value: json!({"apiKeyEnv":"DEEPSEEK_API_KEY"}),
                        base: json!({"baseURL":"https://base"}),
                        user: json!({}),
                    },
                    SettingsNamespaceView {
                        ns: "llm-pi-ai".to_owned(),
                        schema: json!({}),
                        value: json!({"providers":{"openai":{"apiKeyEnv":"OPENAI_API_KEY"}}}),
                        base: json!({"providers":{}}),
                        user: json!({"providers":{"openai":{"apiKeyEnv":"OPENAI_API_KEY"}}}),
                    },
                ],
            ))
        }
        .boxed_local()
    }

    fn credentials(
        &self,
        references: Vec<String>,
    ) -> LocalBoxFuture<'static, Result<BTreeMap<String, CredentialView>, String>> {
        self.credential_refs.borrow_mut().push(references.clone());
        let failed = self.credential_failure;
        async move {
            if failed {
                return Err("credential transport down".to_owned());
            }
            Ok(references
                .into_iter()
                .map(|reference| {
                    let configured = reference == "OPENAI_API_KEY";
                    (reference, credential(configured, true))
                })
                .collect())
        }
        .boxed_local()
    }
}

#[tokio::test(flavor = "current_thread")]
async fn models_store_joins_layers_and_degrades_only_credential_enrichment() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let store = ModelsSettingsStore::new(Rc::new(ModelsFixture {
        credential_failure: false,
        credential_refs: seen.clone(),
    }));
    store.load().await;
    let snapshot = store.store.snapshot();
    assert_eq!(snapshot.status, ModelsStatus::Ready);
    assert_eq!(
        seen.borrow().as_slice(),
        &[vec![
            "DEEPSEEK_API_KEY".to_owned(),
            "OPENAI_API_KEY".to_owned(),
        ]]
    );
    let openai = snapshot
        .rows
        .iter()
        .find(|row| row.entry.provider == "openai")
        .unwrap();
    assert!(openai.configured);
    assert!(openai.removable);
    assert!(openai.credential.as_ref().unwrap().configured);
    let anthropic = snapshot
        .rows
        .iter()
        .find(|row| row.entry.provider == "anthropic")
        .unwrap();
    assert!(!anthropic.configured);
    assert!(anthropic.api_key_env.is_none());

    let degraded = ModelsSettingsStore::new(Rc::new(ModelsFixture {
        credential_failure: true,
        credential_refs: Rc::new(RefCell::new(Vec::new())),
    }));
    degraded.load().await;
    let snapshot = degraded.store.snapshot();
    assert_eq!(snapshot.status, ModelsStatus::Ready);
    assert_eq!(
        snapshot.credential_error.as_deref(),
        Some("credential transport down")
    );
    assert!(snapshot.rows.iter().all(|row| row.credential.is_none()));
}

#[derive(Default)]
struct WelcomeFixture {
    version: RefCell<Option<String>>,
    writes: RefCell<Vec<(&'static str, &'static str, &'static str)>>,
    failure: RefCell<Option<String>>,
}

impl WelcomeTransport for WelcomeFixture {
    fn describe(&self) -> LocalBoxFuture<'static, Result<Option<String>, String>> {
        let version = self.version.borrow().clone();
        let failure = self.failure.borrow().clone();
        async move { failure.map_or(Ok(version), Err) }.boxed_local()
    }

    fn acknowledge(
        &self,
        namespace: &'static str,
        field: &'static str,
        version: &'static str,
    ) -> LocalBoxFuture<'static, Result<(), String>> {
        self.writes.borrow_mut().push((namespace, field, version));
        let failure = self.failure.borrow().clone();
        async move { failure.map_or(Ok(()), Err) }.boxed_local()
    }
}

#[tokio::test(flavor = "current_thread")]
async fn welcome_store_uses_exact_version_and_memory_mode_never_calls_host() {
    let host = Rc::new(WelcomeFixture::default());
    let store = WelcomeNoticeStore::new(host.clone(), WelcomePersistence::Host);
    store.load().await;
    assert_eq!(store.store.snapshot().status, WelcomeStatus::Ready);
    assert!(!store.store.snapshot().acknowledged);
    assert!(store.acknowledge().await);
    assert!(store.store.snapshot().acknowledged);
    assert_eq!(host.writes.borrow().len(), 1);

    let current = Rc::new(WelcomeFixture::default());
    *current.version.borrow_mut() = Some("2026-08-13.1".to_owned());
    let store = WelcomeNoticeStore::new(current, WelcomePersistence::Host);
    store.load().await;
    assert!(store.store.snapshot().acknowledged);

    let memory = Rc::new(WelcomeFixture::default());
    let store = WelcomeNoticeStore::new(memory.clone(), WelcomePersistence::Memory);
    store.load().await;
    assert!(store.acknowledge().await);
    store.load().await;
    assert!(store.store.snapshot().acknowledged);
    assert!(memory.writes.borrow().is_empty());
    assert!(store.should_refresh());
}
