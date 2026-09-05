//! Behavioral mirror of `packages/credentials/credentials/tests/invariant.spec.ts`.

use std::sync::Arc;

use async_trait::async_trait;
use seekdeep_cordis::{Context, EventArgs};
use seekdeep_credentials::{
    CredentialInfo, CredentialProvider, CredentialRef, CredentialService, ResolvedCredential,
    credential_ref, register_invariant,
};
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};

struct NoopCredentials;

#[async_trait]
impl CredentialProvider for NoopCredentials {
    async fn resolve(&self, _: &CredentialRef) -> anyhow::Result<Option<ResolvedCredential>> {
        Ok(None)
    }

    async fn describe(&self, _: &CredentialRef) -> anyhow::Result<CredentialInfo> {
        Ok(CredentialInfo {
            configured: false,
            source: None,
            writable: true,
        })
    }

    async fn set(&self, _: &CredentialRef, _: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn unset(&self, _: &CredentialRef) -> anyhow::Result<()> {
        Ok(())
    }
}

async fn setup() -> (
    Context,
    Arc<InvariantRegistry>,
    seekdeep_invariants::InvariantRegistration,
) {
    let context = Context::new();
    let registry = InvariantRegistry::install(&context, &InvariantConfig::default()).unwrap();
    let registration = register_invariant(&registry).unwrap();
    registration.await_ready().await.unwrap();
    (context, registry, registration)
}

#[tokio::test]
async fn accepts_a_committed_change_emitted_by_a_live_service() {
    let (context, _, _registration) = setup().await;
    let service = CredentialService::new(Arc::new(NoopCredentials));
    let _effect = service.provide(&context).unwrap();
    let reference = credential_ref("DEEPSEEK_API_KEY").unwrap();

    context
        .events()
        .emit(&context, "credentials/updated", &EventArgs::one(reference))
        .unwrap();
}

#[tokio::test]
async fn fails_an_update_event_emitted_without_a_live_service() {
    let (context, _, _registration) = setup().await;
    let reference = credential_ref("DEEPSEEK_API_KEY").unwrap();
    let error = context
        .events()
        .emit(&context, "credentials/updated", &EventArgs::one(reference))
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("invariant violated by \"@deepseek-ai/seekdeep-credentials\"")
    );
    assert!(
        error
            .to_string()
            .contains("without a live credentials service")
    );
}

#[tokio::test]
async fn reserves_the_package_name_against_duplicate_registration() {
    let (_, registry, registration) = setup().await;
    let duplicate = register_invariant(&registry).unwrap_err();
    assert!(duplicate.to_string().contains("already registered"));

    registration.dispose().await.unwrap();
    register_invariant(&registry)
        .unwrap()
        .await_ready()
        .await
        .unwrap();
}
