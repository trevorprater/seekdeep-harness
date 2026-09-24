//! Replay and live-dispatch parity for the title-source invariant.

use std::sync::Arc;

use seekdeep_cordis::Context;
use seekdeep_core::{
    session::AppendOptions,
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use seekdeep_session_title::invariant::register_invariant;
use serde_json::json;

async fn setup() -> (Context, Arc<SessionStore>) {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let registry =
        InvariantRegistry::install(&context, &InvariantConfig::default()).expect("invariants");
    let registration = register_invariant(&registry).expect("registration");
    registration.await_ready().await.expect("invariant ready");
    (context, sessions)
}

#[tokio::test]
async fn accepts_cited_automatic_titles_and_citation_free_user_renames() {
    let (context, sessions) = setup().await;
    let session = sessions
        .create(&context, None, CreateSessionOptions::default())
        .expect("session");
    session
        .append(
            "session/title",
            json!({"title": "auto", "messageSeqs": [1], "source": {"kind": "fallback"}}),
            AppendOptions::default(),
        )
        .expect("automatic title");
    session
        .append(
            "session/title",
            json!({"title": "named", "messageSeqs": [], "source": {"kind": "user"}}),
            AppendOptions::default(),
        )
        .expect("user rename");
}

#[tokio::test]
async fn rejects_citation_free_automatic_titles_and_citing_user_renames() {
    let (context, sessions) = setup().await;
    let session = sessions
        .create(&context, None, CreateSessionOptions::default())
        .expect("session");
    let first = session
        .append(
            "session/title",
            json!({"title": "auto", "messageSeqs": [], "source": {"kind": "fallback"}}),
            AppendOptions::default(),
        )
        .expect_err("citation-free automatic title");
    assert!(format!("{first:#}").contains("cite at least one message seq"));

    let second = session
        .append(
            "session/title",
            json!({"title": "named", "messageSeqs": [1], "source": {"kind": "user"}}),
            AppendOptions::default(),
        )
        .expect_err("citing user rename");
    assert!(format!("{second:#}").contains("cite no message seqs"));
}
