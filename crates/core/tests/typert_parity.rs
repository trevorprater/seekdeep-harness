//! Optional Session Typert lookup composition and lifecycle parity.

use std::sync::Arc;

use seekdeep_cordis::{Context, Fiber};
use seekdeep_core::{
    session::{Session, SessionId},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_typert_protocol::{TypertBoundaryValue, TypertLookupRegistry as _};

async fn assert_lookup(
    registry: &seekdeep_typert_registry::TypertRegistry,
    session: &Arc<Session>,
) {
    let lookup = registry.lookups().get("session").expect("session lookup");
    assert_eq!(lookup.parameter, "session");
    assert_eq!(lookup.wire, "sessionId");
    assert_eq!(
        lookup.host_type_symbol,
        "@seekdeep-ai/seekdeep-session#Session"
    );
    assert_eq!(
        lookup.wire_type_symbol,
        "@seekdeep-ai/seekdeep-session/types#SessionId"
    );
    let resolved = (lookup.resolve)(TypertBoundaryValue::json(serde_json::json!(
        session.id().as_str()
    )))
    .await
    .unwrap()
    .expect("live session");
    let resolved = Arc::downcast::<Session>(resolved).expect("Session lookup result");
    assert!(Arc::ptr_eq(&resolved, session));
    assert!(
        (lookup.resolve)(TypertBoundaryValue::Undefined)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn late_typert_service_receives_live_session_lookup_until_sessions_dispose() {
    let root = Context::new();
    let session_fiber = Fiber::active_child("sessions");
    let session_context = root.with_fiber(session_fiber.clone());
    let sessions = SessionStore::install(&session_context).unwrap();
    let registry = seekdeep_typert_registry::install(&root).unwrap();
    let session = sessions
        .create(
            &session_context,
            Some(SessionId::new("remote-session")),
            CreateSessionOptions::default(),
        )
        .unwrap();

    assert_lookup(&registry, &session).await;
    session_fiber.dispose().await.unwrap();
    assert!(registry.lookups().get("session").is_none());
    root.root_fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn existing_typert_service_rebinds_after_provider_replacement() {
    let root = Context::new();
    let first_typert_fiber = Fiber::active_child("typert-first");
    let first_typert_context = root.with_fiber(first_typert_fiber.clone());
    let first = seekdeep_typert_registry::install(&first_typert_context).unwrap();
    let session_fiber = Fiber::active_child("sessions-rebind");
    let session_context = root.with_fiber(session_fiber.clone());
    let sessions = SessionStore::install(&session_context).unwrap();
    let session = sessions
        .create(
            &session_context,
            Some(SessionId::new("rebound-session")),
            CreateSessionOptions::default(),
        )
        .unwrap();

    assert_lookup(&first, &session).await;
    first_typert_fiber.dispose().await.unwrap();
    assert!(first.lookups().get("session").is_none());

    let second_typert_fiber = Fiber::active_child("typert-second");
    let second_typert_context = root.with_fiber(second_typert_fiber.clone());
    let second = seekdeep_typert_registry::install(&second_typert_context).unwrap();
    assert_lookup(&second, &session).await;

    session_fiber.dispose().await.unwrap();
    assert!(second.lookups().get("session").is_none());
    second_typert_fiber.dispose().await.unwrap();
    root.root_fiber().dispose().await.unwrap();
}
