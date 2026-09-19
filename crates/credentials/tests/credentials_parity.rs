//! Behavioral mirror of `packages/credentials/credentials/tests/credentials.spec.ts`.

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use parking_lot::Mutex;
use seekdeep_cordis::{Context, EventOptions, EventReply, fiber::EffectHandle};
use seekdeep_credentials::{
    CREDENTIALS, CredentialInfo, CredentialNotifier, CredentialProvider, CredentialRef,
    CredentialService, ResolvedCredential, credential_ref,
};

const SOURCE: &str = "memory";

struct MemoryCredentials {
    store: Mutex<HashMap<CredentialRef, String>>,
    notifier: CredentialNotifier,
}

impl MemoryCredentials {
    fn new(context: &Context, seed: impl IntoIterator<Item = (CredentialRef, String)>) -> Self {
        Self {
            store: Mutex::new(seed.into_iter().collect()),
            notifier: CredentialNotifier::new(context),
        }
    }
}

#[async_trait]
impl CredentialProvider for MemoryCredentials {
    async fn resolve(
        &self,
        reference: &CredentialRef,
    ) -> anyhow::Result<Option<ResolvedCredential>> {
        Ok(self
            .store
            .lock()
            .get(reference)
            .filter(|value| !value.is_empty())
            .map(|value| ResolvedCredential {
                value: value.clone(),
                source: SOURCE.to_owned(),
            }))
    }

    async fn describe(&self, reference: &CredentialRef) -> anyhow::Result<CredentialInfo> {
        let configured = self
            .store
            .lock()
            .get(reference)
            .is_some_and(|value| !value.is_empty());
        Ok(CredentialInfo {
            configured,
            source: configured.then(|| SOURCE.to_owned()),
            writable: true,
        })
    }

    async fn set(&self, reference: &CredentialRef, value: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            !value.is_empty(),
            "memory credentials: an empty value cannot be stored; use unset"
        );
        self.store
            .lock()
            .insert(reference.clone(), value.to_owned());
        self.notifier.notify_updated(reference)
    }

    async fn unset(&self, reference: &CredentialRef) -> anyhow::Result<()> {
        if self.store.lock().remove(reference).is_some() {
            self.notifier.notify_updated(reference)?;
        }
        Ok(())
    }
}

fn boot(
    seed: impl IntoIterator<Item = (CredentialRef, String)>,
) -> (
    Context,
    Arc<CredentialService>,
    EffectHandle,
    Arc<MemoryCredentials>,
) {
    let context = Context::new();
    let provider = Arc::new(MemoryCredentials::new(&context, seed));
    let service = CredentialService::new(provider.clone());
    let effect = service.provide(&context).expect("publish credentials");
    (context, service, effect, provider)
}

#[test]
fn brands_posix_shell_identifiers() {
    for valid in ["DEEPSEEK_API_KEY", "_private", "lower_case9"] {
        assert_eq!(credential_ref(valid).unwrap().as_str(), valid);
    }
}

#[test]
fn rejects_every_other_reference_shape() {
    for invalid in ["", "9LEADING", "WITH-DASH", "WITH SPACE", "ns:key", "é"] {
        let error = credential_ref(invalid).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("must match /^[A-Za-z_][A-Za-z0-9_]*$/")
        );
    }
}

#[tokio::test]
async fn mounts_and_resolves_a_seeded_reference_with_its_source() {
    let reference = credential_ref("DEEPSEEK_API_KEY").unwrap();
    let (context, service, _effect, _) = boot([(reference.clone(), "sk-seeded".to_owned())]);

    assert!(Arc::ptr_eq(&context.get(CREDENTIALS).unwrap(), &service));
    assert_eq!(
        service.resolve(&reference).await.unwrap(),
        Some(ResolvedCredential {
            value: "sk-seeded".to_owned(),
            source: SOURCE.to_owned(),
        })
    );
    assert_eq!(
        service.describe(&reference).await.unwrap(),
        CredentialInfo {
            configured: true,
            source: Some(SOURCE.to_owned()),
            writable: true,
        }
    );
}

#[tokio::test]
async fn treats_an_empty_stored_value_as_absent_everywhere() {
    let reference = credential_ref("DEEPSEEK_API_KEY").unwrap();
    let (_, service, _effect, _) = boot([(reference.clone(), String::new())]);

    assert_eq!(service.resolve(&reference).await.unwrap(), None);
    assert_eq!(
        service.describe(&reference).await.unwrap(),
        CredentialInfo {
            configured: false,
            source: None,
            writable: true,
        }
    );
}

#[tokio::test]
async fn set_and_unset_emit_only_committed_changes() {
    let reference = credential_ref("DEEPSEEK_API_KEY").unwrap();
    let (context, service, _effect, _) = boot([]);
    let events = Arc::new(Mutex::new(Vec::new()));
    let recorded = events.clone();
    context
        .events()
        .on_sync(
            &context,
            "credentials/updated",
            move |_, args| {
                recorded
                    .lock()
                    .push((*args.get::<CredentialRef>(0).expect("reference")).clone());
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();

    service.set(&reference, "sk-live").await.unwrap();
    assert_eq!(
        service.resolve(&reference).await.unwrap().unwrap().value,
        "sk-live"
    );
    service.unset(&reference).await.unwrap();
    assert_eq!(service.resolve(&reference).await.unwrap(), None);
    assert_eq!(*events.lock(), vec![reference.clone(), reference]);
}

#[tokio::test]
async fn rejects_empty_set_and_keeps_absent_unset_silent() {
    let reference = credential_ref("DEEPSEEK_API_KEY").unwrap();
    let (context, service, _effect, _) = boot([]);
    let events = Arc::new(Mutex::new(0_u32));
    let recorded = events.clone();
    context
        .events()
        .on_sync(
            &context,
            "credentials/updated",
            move |_, _| {
                *recorded.lock() += 1;
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();

    let error = service.set(&reference, "").await.unwrap_err();
    assert!(error.to_string().contains("empty value"));
    service.unset(&reference).await.unwrap();
    assert_eq!(*events.lock(), 0);
}

#[tokio::test]
async fn removes_the_service_with_its_effect() {
    let (context, _service, effect, _) = boot([]);
    assert!(context.get(CREDENTIALS).is_some());
    effect.dispose().await.unwrap();
    assert!(context.get(CREDENTIALS).is_none());
}

#[tokio::test]
async fn committed_notifications_contain_sync_observer_failures_and_run_every_listener() {
    let reference = credential_ref("DEEPSEEK_API_KEY").unwrap();
    let (context, service, _effect, _) = boot([]);
    let calls = Arc::new(Mutex::new(Vec::new()));

    for (name, fails) in [("first", true), ("second", false)] {
        let calls = calls.clone();
        context
            .events()
            .on_sync(
                &context,
                "credentials/updated",
                move |_, _| {
                    calls.lock().push(name);
                    if fails {
                        anyhow::bail!("observer failed");
                    }
                    Ok(EventReply::Undefined)
                },
                EventOptions::default(),
            )
            .unwrap();
    }

    service.set(&reference, "durable").await.unwrap();
    assert_eq!(*calls.lock(), vec!["first", "second"]);
    assert_eq!(
        service.resolve(&reference).await.unwrap().unwrap().value,
        "durable"
    );
}
