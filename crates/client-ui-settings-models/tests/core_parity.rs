//! API-key, provider join, readiness, and welcome-store source parity.

#![cfg(not(target_arch = "wasm32"))]

use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

use futures::{FutureExt as _, future::LocalBoxFuture};
use indexmap::IndexMap;
use seekdeep_client_ui_settings_models::{
    ApiKeyFailureKey, ConfigurableProviderView, CredentialView, ModelValidationKey,
    ModelsSettingsState, ModelsSettingsStore, ModelsStatus, ModelsTransport, OnboardingReadiness,
    OnboardingUnavailableReason, OptionalJsonValue, ProviderRow, SettingsNamespaceView,
    SettingsPathOp, WELCOME_NOTICE_EN, WELCOME_NOTICE_ZH, WelcomeNoticeStore, WelcomePersistence,
    WelcomeStatus, WelcomeTransport, api_key_failure, derive_key_ref, format_capacity, needs_setup,
    onboarding_readiness, parse_capacity, path_ops, provider_copy, provider_target_label,
    provider_usable, route_valid, trim_api_key, validate_models,
};
use serde_json::json;

#[test]
fn namespace_wire_round_trip_preserves_missing_and_explicit_null_layers() {
    let missing: SettingsNamespaceView = serde_json::from_value(json!({
        "ns":"sample", "schema":{}, "value":{}, "applies":"live", "secrets":[], "revision":1
    }))
    .unwrap();
    assert!(matches!(&missing.base, OptionalJsonValue::Missing));
    assert!(matches!(&missing.user, OptionalJsonValue::Missing));
    let encoded = serde_json::to_value(&missing).unwrap();
    assert!(encoded.get("base").is_none());
    assert!(encoded.get("user").is_none());

    let nulls: SettingsNamespaceView = serde_json::from_value(json!({
        "ns":"sample", "schema":{}, "value":{}, "base":null, "user":null,
        "applies":"live", "secrets":[], "revision":1
    }))
    .unwrap();
    assert!(matches!(&nulls.base, OptionalJsonValue::Present(value) if value.is_null()));
    assert!(matches!(&nulls.user, OptionalJsonValue::Present(value) if value.is_null()));
    let encoded = serde_json::to_value(&nulls).unwrap();
    assert_eq!(encoded.get("base"), Some(&serde_json::Value::Null));
    assert_eq!(encoded.get("user"), Some(&serde_json::Value::Null));
}

#[test]
fn compiled_locale_dictionaries_use_the_exact_versioned_owner_copy() {
    let dictionaries: serde_json::Value =
        serde_json::from_str(include_str!("../data/models-locales.json")).unwrap();
    assert_eq!(dictionaries["en"]["welcomeTitle"], WELCOME_NOTICE_EN.title);
    assert_eq!(dictionaries["en"]["welcomeBody"], WELCOME_NOTICE_EN.body);
    assert_eq!(
        dictionaries["en"]["welcomeContinue"],
        WELCOME_NOTICE_EN.continue_label
    );
    assert_eq!(dictionaries["zh"]["welcomeTitle"], WELCOME_NOTICE_ZH.title);
    assert_eq!(dictionaries["zh"]["welcomeBody"], WELCOME_NOTICE_ZH.body);
    assert_eq!(
        dictionaries["zh"]["welcomeContinue"],
        WELCOME_NOTICE_ZH.continue_label
    );
}

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
    assert_eq!(trim_api_key("\u{feff} sk-live \u{feff}"), "sk-live");
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
        authentication: Some("api-key".to_owned()),
        active,
        declared: None,
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
        namespaces: IndexMap::new(),
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
    assert_eq!(derive_key_ref("-minimax-"), "_MINIMAX__API_KEY");
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
                        base: json!({"baseURL":"https://base"}).into(),
                        user: json!({}).into(),
                        applies: "live".to_owned(),
                        secrets: Vec::new(),
                        revision: 3,
                    },
                    SettingsNamespaceView {
                        ns: "llm-pi-ai".to_owned(),
                        schema: json!({}),
                        value: json!({"providers":{"openai":{"apiKeyEnv":"OPENAI_API_KEY"}}}),
                        base: json!({"providers":{}}).into(),
                        user: json!({
                            "providers":{"openai":{"apiKeyEnv":"OPENAI_API_KEY"}}
                        })
                        .into(),
                        applies: "live".to_owned(),
                        secrets: Vec::new(),
                        revision: 4,
                    },
                ]
                .into_iter()
                .rev()
                .collect(),
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

struct EarlyModelsFailure;

impl ModelsTransport for EarlyModelsFailure {
    fn providers(&self) -> LocalBoxFuture<'static, Result<Vec<ConfigurableProviderView>, String>> {
        async { Err("directory unavailable".to_owned()) }.boxed_local()
    }

    fn settings(
        &self,
    ) -> LocalBoxFuture<'static, Result<(bool, Vec<SettingsNamespaceView>), String>> {
        futures::future::pending().boxed_local()
    }

    fn credentials(
        &self,
        _references: Vec<String>,
    ) -> LocalBoxFuture<'static, Result<BTreeMap<String, CredentialView>, String>> {
        unreachable!("the initial join failed")
    }
}

#[test]
fn provider_or_settings_failure_does_not_wait_for_the_other_request() {
    let store = ModelsSettingsStore::new(Rc::new(EarlyModelsFailure));
    assert!(store.load().now_or_never().is_some());
    let snapshot = store.store.snapshot();
    assert_eq!(snapshot.status, ModelsStatus::Error);
    assert_eq!(snapshot.error.as_deref(), Some("directory unavailable"));
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
        snapshot
            .namespaces
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["llm-pi-ai", "llm-deepseek"]
    );
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

#[test]
fn editor_models_preserve_capacity_path_identity_and_route_contracts() {
    for (text, expected) in [
        ("256K", Some(256_000.0)),
        ("\u{feff}256K\u{feff}", Some(256_000.0)),
        ("1m", Some(1_000_000.0)),
        ("2.3M", Some(2_300_000.0)),
        ("131072", Some(131_072.0)),
        ("", None),
    ] {
        assert_eq!(parse_capacity(text), expected, "{text:?}");
    }
    for text in ["abc", "12x", "1 000", "-5", ".5", "1."] {
        assert!(parse_capacity(text).is_some_and(f64::is_nan), "{text:?}");
    }
    for (value, expected) in [
        (1_000_000.0, "1M"),
        (256_000.0, "256K"),
        (131_072.0, "131072"),
        (2_500.5, "2500.5"),
        (-0.0, "0"),
        (0.000_000_1, "1e-7"),
    ] {
        assert_eq!(format_capacity(value), expected);
    }

    let models = json!([
        {"id":"deepseek-chat","hidden":true},
        {"id":"deepseek-reasoner","name":"Reasoner","contextWindow":128_000,"maxTokens":8192},
    ]);
    assert_eq!(validate_models(Some(&models)), None);
    let duplicate = json!([{"id":"model"},{"id":" model "}]);
    assert_eq!(
        validate_models(Some(&duplicate)).unwrap().key,
        ModelValidationKey::IdDuplicate
    );
    let duplicate = json!([{"id":"model"},{"id":"\u{feff}model\u{feff}"}]);
    assert_eq!(
        validate_models(Some(&duplicate)).unwrap().key,
        ModelValidationKey::IdDuplicate
    );
    let invalid = json!([{"id":"model","contextWindow":0}]);
    assert_eq!(
        validate_models(Some(&invalid)).unwrap().key,
        ModelValidationKey::ContextInvalid
    );

    let base = vec!["providers".to_owned(), "acme".to_owned()];
    let after = json!({"api":"openai","models":[],"hidden":true});
    let operations = path_ops(
        &base,
        Some(&json!({"api":"anthropic","baseURL":"old","hidden":true})),
        after.as_object().unwrap(),
    );
    assert_eq!(operations.len(), 3);
    assert!(
        matches!(&operations[0], SettingsPathOp::Set { path, .. } if path.ends_with(&["api".to_owned()]))
    );
    assert!(
        matches!(&operations[2], SettingsPathOp::Unset { path } if path.ends_with(&["baseURL".to_owned()]))
    );

    let mut setup = row();
    assert!(needs_setup(&setup, false));
    assert!(!needs_setup(&setup, true));
    setup.entry.settings_path.push("nested".to_owned());
    assert!(!needs_setup(&setup, false));
    assert_eq!(provider_target_label("acme", "Acme"), "Acme (acme)");
    assert_eq!(
        provider_copy("Delete {provider}?", "acme", "Acme"),
        "Delete Acme (acme)?"
    );
    for route in ["a", "acme-gateway", "g2-mini"] {
        assert!(route_valid(route), "{route:?}");
    }
    for route in ["", "2fast", "Acme", "acme_foo", "acme--foo", "acme-"] {
        assert!(!route_valid(route), "{route:?}");
    }
}
