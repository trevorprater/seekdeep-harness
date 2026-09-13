//! Native watcher and teardown parity specifications.

use std::sync::atomic::{AtomicBool, Ordering};
use std::{path::Path, sync::Arc, time::Duration};

use parking_lot::Mutex;
use seekdeep_cordis::{Context, EventOptions, EventReply};
use seekdeep_credentials::{CREDENTIALS, CredentialRef, credential_ref};
use seekdeep_credentials_local::{LocalCredentialConfig, install};
use tempfile::TempDir;

async fn write_owner_only(path: &Path, text: &str) {
    tokio::fs::write(path, text).await.unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .await
            .unwrap();
    }
}

fn record_updates(context: &Context) -> Arc<Mutex<Vec<CredentialRef>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorded = seen.clone();
    context
        .events()
        .on_sync(
            context,
            "credentials/updated",
            move |_, args| {
                recorded
                    .lock()
                    .push((*args.get::<CredentialRef>(0).unwrap()).clone());
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    seen
}

async fn wait_value(
    credentials: &Arc<seekdeep_credentials::CredentialService>,
    reference: &CredentialRef,
    expected: Option<&str>,
) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let actual = credentials
                .resolve(reference)
                .await
                .expect("resolve")
                .map(|resolved| resolved.value);
            if actual.as_deref() == expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("watcher value timed out");
}

fn watched(path: &Path) -> LocalCredentialConfig {
    LocalCredentialConfig {
        path: Some(path.to_owned()),
        debounce_ms: 10.0,
        ..LocalCredentialConfig::default()
    }
}

#[tokio::test]
async fn publishes_external_edits_replaces_snapshot_and_suppresses_self_write_echo() {
    let temporary = TempDir::new().unwrap();
    let path = temporary.path().join(".credentials.yaml");
    let key = credential_ref("SEEKDEEP_CRED_PIPE").unwrap();
    let other = credential_ref("SEEKDEEP_CRED_OTHER").unwrap();
    write_owner_only(&path, "SEEKDEEP_CRED_PIPE: boot\n").await;
    let context = Context::new();
    let fiber = install(&context, watched(&path)).unwrap();
    fiber.await_settled().await.unwrap();
    let seen = record_updates(&context);
    let credentials = context.get(CREDENTIALS).unwrap();

    write_owner_only(
        &path,
        "SEEKDEEP_CRED_PIPE: live\nSEEKDEEP_CRED_OTHER: extra\n",
    )
    .await;
    wait_value(&credentials, &key, Some("live")).await;
    wait_value(&credentials, &other, Some("extra")).await;

    write_owner_only(&path, "SEEKDEEP_CRED_PIPE: live\n").await;
    wait_value(&credentials, &other, None).await;

    let before = seen.lock().len();
    credentials.set(&key, "self-written").await.unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(seen.lock().len(), before + 1);
    assert_eq!(
        credentials.resolve(&key).await.unwrap().unwrap().value,
        "self-written"
    );
    fiber.dispose().await.unwrap();
}

#[tokio::test]
async fn invalid_reload_keeps_last_good_snapshot_and_repair_resumes_publication() {
    let temporary = TempDir::new().unwrap();
    let path = temporary.path().join(".credentials.yaml");
    let key = credential_ref("SEEKDEEP_CRED_PIPE").unwrap();
    write_owner_only(&path, "SEEKDEEP_CRED_PIPE: good\n").await;
    let context = Context::new();
    let fiber = install(&context, watched(&path)).unwrap();
    fiber.await_settled().await.unwrap();
    let seen = record_updates(&context);
    let credentials = context.get(CREDENTIALS).unwrap();

    write_owner_only(&path, "BAD-KEY: 2\nSEEKDEEP_CRED_PIPE: bad\n").await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        credentials.resolve(&key).await.unwrap().unwrap().value,
        "good"
    );
    assert!(seen.lock().is_empty());

    write_owner_only(&path, "SEEKDEEP_CRED_PIPE: repaired\n").await;
    wait_value(&credentials, &key, Some("repaired")).await;
    assert_eq!(*seen.lock(), vec![key]);
    fiber.dispose().await.unwrap();
}

#[tokio::test]
async fn deleting_document_empties_snapshot_and_emits_removals() {
    let temporary = TempDir::new().unwrap();
    let path = temporary.path().join(".credentials.yaml");
    let key = credential_ref("SEEKDEEP_CRED_PIPE").unwrap();
    write_owner_only(&path, "SEEKDEEP_CRED_PIPE: doomed\n").await;
    let context = Context::new();
    let fiber = install(&context, watched(&path)).unwrap();
    fiber.await_settled().await.unwrap();
    let seen = record_updates(&context);
    let credentials = context.get(CREDENTIALS).unwrap();

    tokio::fs::remove_file(&path).await.unwrap();
    wait_value(&credentials, &key, None).await;
    assert_eq!(*seen.lock(), vec![key]);
    fiber.dispose().await.unwrap();
}

#[tokio::test]
async fn ready_reconcile_closes_initial_load_to_watcher_setup_gap() {
    let temporary = TempDir::new().unwrap();
    let path = temporary.path().join(".credentials.yaml");
    let key = credential_ref("SEEKDEEP_CRED_PIPE").unwrap();
    write_owner_only(&path, "SEEKDEEP_CRED_PIPE: initial\n").await;
    let context = Context::new();
    let fiber = install(&context, watched(&path)).unwrap();
    // This write may race either side of watcher construction. The explicit
    // startup reconcile and the native event path must converge identically.
    write_owner_only(&path, "SEEKDEEP_CRED_PIPE: during-setup\n").await;
    fiber.await_settled().await.unwrap();
    let credentials = context.get(CREDENTIALS).unwrap();
    wait_value(&credentials, &key, Some("during-setup")).await;
    fiber.dispose().await.unwrap();
}

#[tokio::test]
async fn dispose_quiesces_watcher_before_service_withdrawal() {
    let temporary = TempDir::new().unwrap();
    let path = temporary.path().join(".credentials.yaml");
    write_owner_only(&path, "SEEKDEEP_CRED_PIPE: initial\n").await;
    let context = Context::new();
    let fiber = install(&context, watched(&path)).unwrap();
    fiber.await_settled().await.unwrap();
    let seen = record_updates(&context);

    write_owner_only(&path, "SEEKDEEP_CRED_PIPE: before-dispose\n").await;
    fiber.dispose().await.unwrap();
    let after_dispose = seen.lock().len();
    write_owner_only(&path, "SEEKDEEP_CRED_PIPE: after-dispose\n").await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(seen.lock().len(), after_dispose);
    assert!(context.get(CREDENTIALS).is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn loosened_runtime_permissions_keep_last_good_snapshot_until_repaired() {
    use std::os::unix::fs::PermissionsExt as _;

    let temporary = TempDir::new().unwrap();
    let path = temporary.path().join(".credentials.yaml");
    let key = credential_ref("SEEKDEEP_CRED_PIPE").unwrap();
    write_owner_only(&path, "SEEKDEEP_CRED_PIPE: good\n").await;
    let context = Context::new();
    let fiber = install(&context, watched(&path)).unwrap();
    fiber.await_settled().await.unwrap();
    let credentials = context.get(CREDENTIALS).unwrap();

    tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        credentials.resolve(&key).await.unwrap().unwrap().value,
        "good"
    );

    tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .await
        .unwrap();
    write_owner_only(&path, "SEEKDEEP_CRED_PIPE: repaired\n").await;
    wait_value(&credentials, &key, Some("repaired")).await;
    fiber.dispose().await.unwrap();
}

#[tokio::test]
async fn reload_queue_survives_an_invariant_failure_after_snapshot_commit() {
    let temporary = TempDir::new().unwrap();
    let path = temporary.path().join(".credentials.yaml");
    let key = credential_ref("SEEKDEEP_CRED_PIPE").unwrap();
    write_owner_only(&path, "{}\n").await;
    let context = Context::new();
    let fiber = install(&context, watched(&path)).unwrap();
    fiber.await_settled().await.unwrap();
    let credentials = context.get(CREDENTIALS).unwrap();
    let armed = Arc::new(AtomicBool::new(true));
    let listener_armed = armed.clone();
    context
        .events()
        .on_sync(
            &context,
            "credentials/updated",
            move |_, _| {
                if listener_armed.load(Ordering::SeqCst) {
                    return Err(seekdeep_invariants::InvariantError::new(
                        "forged-watch-relation",
                        "forged relation",
                    )
                    .into());
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();

    write_owner_only(&path, "SEEKDEEP_CRED_PIPE: first\n").await;
    wait_value(&credentials, &key, Some("first")).await;
    armed.store(false, Ordering::SeqCst);
    write_owner_only(&path, "SEEKDEEP_CRED_PIPE: second\n").await;
    wait_value(&credentials, &key, Some("second")).await;
    fiber.dispose().await.unwrap();
}

#[tokio::test]
async fn zero_debounce_and_transient_absence_are_supported() {
    let temporary = TempDir::new().unwrap();
    let path = temporary.path().join(".credentials.yaml");
    let key = credential_ref("SEEKDEEP_CRED_PIPE").unwrap();
    let context = Context::new();
    let fiber = install(
        &context,
        LocalCredentialConfig {
            path: Some(path.clone()),
            debounce_ms: 0.0,
            ..LocalCredentialConfig::default()
        },
    )
    .unwrap();
    fiber.await_settled().await.unwrap();
    let seen = record_updates(&context);
    let credentials = context.get(CREDENTIALS).unwrap();

    write_owner_only(&path, "SEEKDEEP_CRED_PIPE: transient\n").await;
    tokio::fs::remove_file(&path).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(credentials.resolve(&key).await.unwrap(), None);
    assert!(seen.lock().len() <= 2);

    write_owner_only(&path, "SEEKDEEP_CRED_PIPE: arrived\n").await;
    wait_value(&credentials, &key, Some("arrived")).await;
    fiber.dispose().await.unwrap();
}
