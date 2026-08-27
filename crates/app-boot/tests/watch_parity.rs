//! Exact-path watcher add/change/unlink, alias, serialization, and failure parity.

use std::{path::Path, sync::Arc, time::Duration};

use parking_lot::Mutex;
use seekdeep_app_boot::{ConfigWatchRegistry, ExactConfigWatcher, canonical_watch_key};

async fn eventually(mut test: impl FnMut() -> bool, message: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !test() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{message}"));
}

fn observed_refresh(
    filename: std::path::PathBuf,
    observed: Arc<Mutex<Vec<String>>>,
) -> seekdeep_app_boot::ConfigRefresh {
    Arc::new(move || {
        let filename = filename.clone();
        let observed = observed.clone();
        Box::pin(async move {
            match std::fs::read_to_string(&filename) {
                Ok(content) => observed.lock().push(content),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    observed.lock().push("missing".to_owned());
                }
                Err(error) => return Err(error.into()),
            }
            Ok(())
        })
    })
}

fn ignore_failure() -> seekdeep_app_boot::ConfigRefreshFailure {
    Arc::new(|_, _| {})
}

#[tokio::test]
async fn observes_initial_add_change_and_unlink_outside_module_roots() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let filename = temporary.path().join("plugins.yml");
    let observed = Arc::new(Mutex::new(Vec::new()));
    let watcher = ExactConfigWatcher::open(
        &filename,
        observed_refresh(filename.clone(), observed.clone()),
        ignore_failure(),
    )?;
    std::fs::write(&filename, "one")?;
    eventually(
        || observed.lock().iter().any(|value| value == "one"),
        "creation",
    )
    .await;
    std::fs::write(&filename, "two")?;
    eventually(
        || observed.lock().iter().any(|value| value == "two"),
        "change",
    )
    .await;
    std::fs::remove_file(&filename)?;
    eventually(
        || observed.lock().iter().any(|value| value == "missing"),
        "unlink",
    )
    .await;
    watcher.dispose().await
}

#[tokio::test]
async fn observes_existing_file_and_creation_under_a_missing_parent() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let existing = temporary.path().join("existing.yml");
    std::fs::write(&existing, "initial")?;
    let initial = Arc::new(Mutex::new(Vec::new()));
    let existing_watcher = ExactConfigWatcher::open(
        &existing,
        observed_refresh(existing.clone(), initial.clone()),
        ignore_failure(),
    )?;
    eventually(
        || initial.lock().as_slice() == ["initial"],
        "initial scan did not refresh",
    )
    .await;
    existing_watcher.dispose().await?;

    let nested = temporary.path().join("later/plugins.yml");
    let observed = Arc::new(Mutex::new(Vec::new()));
    let watcher = ExactConfigWatcher::open(
        &nested,
        observed_refresh(nested.clone(), observed.clone()),
        ignore_failure(),
    )?;
    std::fs::create_dir(nested.parent().expect("parent"))?;
    std::fs::write(&nested, "created")?;
    eventually(
        || observed.lock().iter().any(|value| value == "created"),
        "missing parent creation",
    )
    .await;
    watcher.dispose().await
}

#[cfg(unix)]
#[tokio::test]
async fn canonicalizes_aliases_for_identity_and_observation() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let target = temporary.path().join("target");
    std::fs::create_dir(&target)?;
    let alias = temporary.path().join("alias");
    std::os::unix::fs::symlink(&target, &alias)?;
    let alias_file = alias.join("plugins.yml");
    let real_file = target.join("plugins.yml");
    assert_eq!(
        canonical_watch_key(&alias_file)?,
        canonical_watch_key(&real_file)?
    );
    let registry = ConfigWatchRegistry::new();
    let reservation = registry.register(
        &alias_file,
        Arc::new(|| Box::pin(async { Ok(()) })),
        ignore_failure(),
    )?;
    assert!(
        registry
            .register(
                &real_file,
                Arc::new(|| Box::pin(async { Ok(()) })),
                ignore_failure(),
            )
            .unwrap_err()
            .to_string()
            .contains("config path already registered")
    );
    reservation.dispose().await?;
    registry
        .register(
            &real_file,
            Arc::new(|| Box::pin(async { Ok(()) })),
            ignore_failure(),
        )?
        .dispose()
        .await?;
    let observed = Arc::new(Mutex::new(Vec::new()));
    let watcher = ExactConfigWatcher::open(
        &alias_file,
        observed_refresh(alias_file.clone(), observed.clone()),
        ignore_failure(),
    )?;
    std::fs::write(&real_file, "through-real-path")?;
    eventually(
        || {
            observed
                .lock()
                .iter()
                .any(|value| value == "through-real-path")
        },
        "alias observation",
    )
    .await;
    watcher.dispose().await
}

#[tokio::test]
async fn serializes_refreshes_and_disposal_waits_for_the_admitted_queue() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let filename = temporary.path().join("plugins.yml");
    std::fs::write(&filename, "one")?;
    let observed = Arc::new(Mutex::new(Vec::new()));
    let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let max_active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let refresh: seekdeep_app_boot::ConfigRefresh = Arc::new({
        let filename = filename.clone();
        let observed = observed.clone();
        let active = active.clone();
        let max_active = max_active.clone();
        let started = started.clone();
        let release = release.clone();
        move || {
            let filename = filename.clone();
            let observed = observed.clone();
            let active = active.clone();
            let max_active = max_active.clone();
            let started = started.clone();
            let release = release.clone();
            Box::pin(async move {
                let now = active.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1;
                max_active.fetch_max(now, std::sync::atomic::Ordering::AcqRel);
                let content = std::fs::read_to_string(filename)?;
                observed.lock().push(content);
                if observed.lock().len() == 1 {
                    started.notify_one();
                    release.notified().await;
                }
                active.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
                Ok(())
            })
        }
    });
    let watcher = Arc::new(ExactConfigWatcher::open(
        &filename,
        refresh,
        ignore_failure(),
    )?);
    started.notified().await;
    std::fs::write(&filename, "two")?;
    tokio::time::sleep(Duration::from_millis(250)).await;
    let disposal = tokio::spawn({
        let watcher = watcher.clone();
        async move { watcher.dispose().await }
    });
    tokio::task::yield_now().await;
    assert!(!disposal.is_finished());
    release.notify_one();
    disposal.await??;
    assert_eq!(max_active.load(std::sync::atomic::Ordering::Acquire), 1);
    assert_eq!(observed.lock().as_slice(), ["one", "two"]);
    Ok(())
}

#[tokio::test]
async fn one_registry_serializes_refreshes_across_distinct_paths() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let first = temporary.path().join("first.yml");
    let second = temporary.path().join("second.yml");
    std::fs::write(&first, "one")?;
    std::fs::write(&second, "two")?;
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let max_active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let first_started = Arc::new(tokio::sync::Notify::new());
    let release_first = Arc::new(tokio::sync::Notify::new());
    let refresh: seekdeep_app_boot::ConfigRefresh = Arc::new({
        let calls = calls.clone();
        let active = active.clone();
        let max_active = max_active.clone();
        let first_started = first_started.clone();
        let release_first = release_first.clone();
        move || {
            let calls = calls.clone();
            let active = active.clone();
            let max_active = max_active.clone();
            let first_started = first_started.clone();
            let release_first = release_first.clone();
            Box::pin(async move {
                let call = calls.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                let now = active.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1;
                max_active.fetch_max(now, std::sync::atomic::Ordering::AcqRel);
                if call == 0 {
                    first_started.notify_one();
                    release_first.notified().await;
                }
                active.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
                Ok(())
            })
        }
    });
    let registry = ConfigWatchRegistry::new();
    let first_watcher = registry.register(&first, refresh.clone(), ignore_failure())?;
    first_started.notified().await;
    let second_watcher = registry.register(&second, refresh, ignore_failure())?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(active.load(std::sync::atomic::Ordering::Acquire), 1);
    assert_eq!(max_active.load(std::sync::atomic::Ordering::Acquire), 1);
    release_first.notify_one();
    eventually(
        || calls.load(std::sync::atomic::Ordering::Acquire) == 2,
        "second path refresh",
    )
    .await;
    second_watcher.dispose().await?;
    first_watcher.dispose().await?;
    assert_eq!(max_active.load(std::sync::atomic::Ordering::Acquire), 1);
    Ok(())
}

#[tokio::test]
async fn refresh_failures_are_contained_and_future_changes_continue() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let filename = temporary.path().join("plugins.yml");
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let failures = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let refresh: seekdeep_app_boot::ConfigRefresh = Arc::new({
        let attempts = attempts.clone();
        move || {
            let attempts = attempts.clone();
            Box::pin(async move {
                attempts.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                anyhow::bail!("42")
            })
        }
    });
    let failure_filename = filename.clone();
    let failure: seekdeep_app_boot::ConfigRefreshFailure = Arc::new({
        let failures = failures.clone();
        move |path: &Path, error| {
            assert_eq!(path, failure_filename);
            assert_eq!(error.to_string(), "42");
            failures.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        }
    });
    let watcher = ExactConfigWatcher::open(&filename, refresh, failure)?;
    std::fs::write(&filename, "invalid")?;
    eventually(
        || failures.load(std::sync::atomic::Ordering::Acquire) == 1,
        "first failure",
    )
    .await;
    std::fs::write(&filename, "invalid again")?;
    eventually(
        || failures.load(std::sync::atomic::Ordering::Acquire) == 2,
        "second failure",
    )
    .await;
    assert_eq!(attempts.load(std::sync::atomic::Ordering::Acquire), 2);
    watcher.dispose().await
}
