//! Durable clock propagation through the registered store and its fork path.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{AppendOptions, Session, SessionId},
    session_store::{CreateSessionOptions, SESSIONS, SessionStore, plugin_with_clock},
};

fn append_marker(session: &Session) -> i64 {
    session
        .append(
            "test/clock",
            serde_json::Value::Null,
            AppendOptions {
                ignorable: true,
                ..AppendOptions::default()
            },
        )
        .unwrap()
        .time
}

#[tokio::test]
async fn registered_store_uses_one_clock_for_headers_forks_and_appends() {
    let context = Context::new();
    let time = Arc::new(AtomicU64::new(10_000));
    let samples = time.clone();
    let mounted = context
        .plugin(
            plugin_with_clock(Arc::new(move || samples.fetch_add(1, Ordering::SeqCst))),
            serde_json::Value::Null,
        )
        .unwrap();
    mounted.await_settled().await.unwrap();
    let store = context.get(SESSIONS).unwrap();
    let parent = store
        .create(
            &context,
            Some(SessionId::new("clock-parent")),
            CreateSessionOptions::default(),
        )
        .unwrap();
    assert_eq!(parent.header().created_at, 10_000);
    assert_eq!(append_marker(&parent), 10_001);
    let child = store
        .fork(&context, &parent, None, Some(SessionId::new("clock-child")))
        .unwrap();
    assert_eq!(child.header().created_at, 10_002);
    assert_eq!(child.events()[0], parent.events()[0]);
    assert_eq!(child.events()[1].event_type, "session/end-seed");
    assert_eq!(child.events()[1].time, 10_003);
    assert_eq!(append_marker(&child), 10_004);
    assert_eq!(time.load(Ordering::SeqCst), 10_005);
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn explicit_header_time_preserves_the_clock_for_seed_and_live_events() {
    let context = Context::new();
    let time = Arc::new(AtomicU64::new(20_000));
    let samples = time.clone();
    let store = SessionStore::install_with_clock(
        &context,
        Arc::new(move || samples.fetch_add(1, Ordering::SeqCst)),
    )
    .unwrap();
    let session = store
        .prepare(
            Some(SessionId::new("explicit-clock")),
            CreateSessionOptions {
                created_at: Some(99),
                seed: Some(Vec::new()),
                ..CreateSessionOptions::default()
            },
        )
        .unwrap();
    assert_eq!(session.header().created_at, 99);
    assert_eq!(session.events()[0].event_type, "session/end-seed");
    assert_eq!(session.events()[0].time, 20_000);
    assert_eq!(append_marker(&session), 20_001);
    assert_eq!(time.load(Ordering::SeqCst), 20_002);
    context.fiber().dispose().await.unwrap();
}
