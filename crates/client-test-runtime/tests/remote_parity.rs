//! Behavioral mirror of `packages/test-support/client-runtime/tests/remote.client.spec.ts`.

#![cfg(not(target_arch = "wasm32"))]

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use parking_lot::Mutex;
use seekdeep_client_test_runtime::{TEST_REMOTE, TestRemote, TestRemoteArgument};
use seekdeep_cordis::Context;

fn string_argument(value: &TestRemoteArgument) -> &str {
    value
        .downcast_ref::<String>()
        .expect("forwarded string argument")
}

#[tokio::test]
async fn delivers_subscribed_events_and_idempotent_disposal_stops_them() {
    let context = Context::new();
    let remote = TestRemote::install(&context).unwrap();
    assert!(Arc::ptr_eq(&remote, &context.get(TEST_REMOTE).unwrap()));
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let observed = seen.clone();
    let subscription = remote.subscribe(
        "settings/document-updated",
        Arc::new(move |arguments| {
            observed
                .lock()
                .push(string_argument(&arguments[0]).to_owned());
            Ok(())
        }),
    );

    remote
        .dispatch(
            "settings/document-updated",
            &[Arc::new("ui-theme".to_owned())],
        )
        .unwrap();
    assert_eq!(&*seen.lock(), &["ui-theme"]);
    subscription.dispose();
    subscription.dispose();
    remote
        .dispatch(
            "settings/document-updated",
            &[Arc::new("ignored".to_owned())],
        )
        .unwrap();
    assert_eq!(&*seen.lock(), &["ui-theme"]);
    context.root_fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn unknown_events_are_inert_and_mount_refuses_the_fake_path() {
    let context = Context::new();
    let remote = TestRemote::install(&context).unwrap();
    remote
        .dispatch(
            "credentials/updated",
            &[Arc::new("DEEPSEEK_API_KEY".to_owned())],
        )
        .unwrap();
    assert_eq!(
        remote.mount().unwrap_err().to_string(),
        "TestRemote: $mount needs the real Client Remote service"
    );
    context.root_fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn dispatch_snapshots_registration_order_and_propagates_the_first_failure() {
    let context = Context::new();
    let remote = TestRemote::install(&context).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let first_calls = calls.clone();
    let _first = remote.subscribe(
        "event",
        Arc::new(move |_| {
            first_calls.fetch_add(1, Ordering::AcqRel);
            anyhow::bail!("listener failed")
        }),
    );
    let second_calls = calls.clone();
    let _second = remote.subscribe(
        "event",
        Arc::new(move |_| {
            second_calls.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }),
    );

    assert_eq!(
        remote.dispatch("event", &[]).unwrap_err().to_string(),
        "listener failed"
    );
    assert_eq!(calls.load(Ordering::Acquire), 1);
    context.root_fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn subscribing_the_same_listener_twice_keeps_set_identity_semantics() {
    let context = Context::new();
    let remote = TestRemote::install(&context).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let listener = Arc::new(move |_: &[TestRemoteArgument]| {
        observed.fetch_add(1, Ordering::AcqRel);
        Ok(())
    });
    let first = remote.subscribe("event", listener.clone());
    let _second = remote.subscribe("event", listener);
    remote.dispatch("event", &[]).unwrap();
    assert_eq!(calls.load(Ordering::Acquire), 1);
    first.dispose();
    remote.dispatch("event", &[]).unwrap();
    assert_eq!(calls.load(Ordering::Acquire), 1);
    context.root_fiber().dispose().await.unwrap();
}
