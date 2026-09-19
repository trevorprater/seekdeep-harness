//! Config watch lifetimes with real filesystem events and blocked callbacks.

use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use seekdeep_cordis::{Context, EventOptions, EventReply};
use seekdeep_hmr::{Config, HostHmrService};
use seekdeep_loader::{LOADER, PluginCatalog};

async fn eventually(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("watcher condition did not settle");
}

async fn empty_service(
    root: &Path,
) -> anyhow::Result<(
    Context,
    seekdeep_loader::LoadedComposition,
    Arc<HostHmrService>,
)> {
    let context = Context::new().intercept("logger", serde_json::json!({"level":3}));
    let composition = PluginCatalog::new().load_yaml(&context, "[]\n").await?;
    let service = HostHmrService::start(
        context.clone(),
        context.get(LOADER).unwrap(),
        serde_json::from_value::<Config>(serde_json::json!({
            "base": root,
            "root": [],
            "ignored": ["**"],
            "debounce": 20,
            "ignoreInitial": true,
            "cwd": root.join("unused-cwd"),
            "depth": 0,
        }))?,
        Arc::new(|| Box::pin(async { Ok(()) })),
    )?;
    Ok((context, composition, service))
}

#[tokio::test]
async fn exact_config_watch_observes_missing_ancestors_add_change_unlink_and_disposal()
-> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let (_context, composition, service) = empty_service(temporary.path()).await?;
    let file = temporary.path().join("missing/nested/.user.yml");
    let refreshes = Arc::new(AtomicUsize::new(0));
    let callback: seekdeep_hmr::ConfigRefresh = Arc::new({
        let refreshes = refreshes.clone();
        move || {
            let refreshes = refreshes.clone();
            Box::pin(async move {
                refreshes.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        }
    });
    let effect = service.register_config(&file, callback.clone()).await?;
    assert_eq!(refreshes.load(Ordering::SeqCst), 0);
    std::fs::create_dir_all(file.parent().unwrap())?;
    std::fs::write(&file, "one\n")?;
    eventually(|| refreshes.load(Ordering::SeqCst) >= 1).await;
    let previous = refreshes.load(Ordering::SeqCst);
    std::fs::write(&file, "two\n")?;
    eventually(|| refreshes.load(Ordering::SeqCst) > previous).await;
    let previous = refreshes.load(Ordering::SeqCst);
    std::fs::remove_file(&file)?;
    eventually(|| refreshes.load(Ordering::SeqCst) > previous).await;
    assert!(
        service
            .register_config(&file, callback.clone())
            .await
            .unwrap_err()
            .to_string()
            .contains("already registered")
    );
    effect.dispose().await?;
    let previous = refreshes.load(Ordering::SeqCst);
    std::fs::write(&file, "closed\n")?;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(refreshes.load(Ordering::SeqCst), previous);
    let replacement = service.register_config(&file, callback.clone()).await?;
    eventually(|| refreshes.load(Ordering::SeqCst) > previous).await;
    replacement.dispose().await?;
    service.dispose().await?;
    assert!(
        service
            .register_config(&file, callback)
            .await
            .unwrap_err()
            .to_string()
            .contains("HMR is not active")
    );
    composition.dispose().await?;
    Ok(())
}

#[tokio::test]
async fn refreshes_are_serial_dirty_changes_rerun_and_disposal_waits_for_the_callback()
-> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let (_context, composition, service) = empty_service(temporary.path()).await?;
    let file = temporary.path().join("user.yml");
    std::fs::write(&file, "initial\n")?;
    let started = Arc::new(AtomicUsize::new(0));
    let running = Arc::new(AtomicUsize::new(0));
    let maximum = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(tokio::sync::Notify::new());
    let effect = service
        .register_config(
            &file,
            Arc::new({
                let started = started.clone();
                let running = running.clone();
                let maximum = maximum.clone();
                let gate = gate.clone();
                move || {
                    let started = started.clone();
                    let running = running.clone();
                    let maximum = maximum.clone();
                    let gate = gate.clone();
                    Box::pin(async move {
                        let index = started.fetch_add(1, Ordering::SeqCst);
                        let active = running.fetch_add(1, Ordering::SeqCst) + 1;
                        maximum.fetch_max(active, Ordering::SeqCst);
                        if index == 0 {
                            gate.notified().await;
                        }
                        running.fetch_sub(1, Ordering::SeqCst);
                        Ok(())
                    })
                }
            }),
        )
        .await?;
    eventually(|| started.load(Ordering::SeqCst) == 1).await;
    for index in 0..3 {
        std::fs::write(&file, format!("changed {index}\n"))?;
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    assert_eq!(started.load(Ordering::SeqCst), 1);
    let disposal = tokio::spawn(async move { effect.dispose().await });
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(!disposal.is_finished());
    gate.notify_one();
    disposal.await??;
    assert_eq!(maximum.load(Ordering::SeqCst), 1);
    assert_eq!(running.load(Ordering::SeqCst), 0);
    assert_eq!(started.load(Ordering::SeqCst), 2);
    service.dispose().await?;
    composition.dispose().await?;
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn cancelled_registration_releases_the_watch_and_canonical_path() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let (_context, composition, service) = empty_service(temporary.path()).await?;
    let file = temporary.path().join("user.yml");
    std::fs::write(&file, "initial\n")?;
    let cancelled_calls = Arc::new(AtomicUsize::new(0));
    let mut pending = Box::pin(service.register_config(
        &file,
        Arc::new({
            let cancelled_calls = cancelled_calls.clone();
            move || {
                cancelled_calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(()) })
            }
        }),
    ));
    // The event task cannot run until this thread yields.
    assert!(futures::poll!(pending.as_mut()).is_pending());
    drop(pending);

    let active_calls = Arc::new(AtomicUsize::new(0));
    let effect = service
        .register_config(
            &file,
            Arc::new({
                let active_calls = active_calls.clone();
                move || {
                    active_calls.fetch_add(1, Ordering::SeqCst);
                    Box::pin(async { Ok(()) })
                }
            }),
        )
        .await?;
    eventually(|| active_calls.load(Ordering::SeqCst) >= 1).await;
    let previous = active_calls.load(Ordering::SeqCst);
    std::fs::write(&file, "changed\n")?;
    eventually(|| active_calls.load(Ordering::SeqCst) > previous).await;
    assert_eq!(cancelled_calls.load(Ordering::SeqCst), 0);
    effect.dispose().await?;
    service.dispose().await?;
    composition.dispose().await?;
    Ok(())
}

#[tokio::test]
async fn config_failures_and_rejected_failure_hooks_do_not_stop_future_refreshes()
-> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let (context, composition, service) = empty_service(temporary.path()).await?;
    let file = temporary.path().join("user.yml");
    std::fs::write(&file, "initial\n")?;
    let events = Arc::new(AtomicUsize::new(0));
    context.events().on(
        &context,
        "hmr/config-update-failed",
        {
            let events = events.clone();
            move |_, _| {
                let events = events.clone();
                Box::pin(async move {
                    events.fetch_add(1, Ordering::SeqCst);
                    anyhow::bail!("failure hook rejected")
                })
            }
        },
        EventOptions::default(),
    )?;
    let refreshes = Arc::new(AtomicUsize::new(0));
    let effect = service
        .register_config(
            &file,
            Arc::new({
                let refreshes = refreshes.clone();
                move || {
                    let refreshes = refreshes.clone();
                    Box::pin(async move {
                        refreshes.fetch_add(1, Ordering::SeqCst);
                        anyhow::bail!("refresh rejected")
                    })
                }
            }),
        )
        .await?;
    eventually(|| events.load(Ordering::SeqCst) >= 1).await;
    let previous = events.load(Ordering::SeqCst);
    std::fs::write(&file, "changed\n")?;
    eventually(|| events.load(Ordering::SeqCst) > previous).await;
    effect.dispose().await?;
    let logs = context.logger_service().buffer();
    assert!(
        logs.iter()
            .any(|record| record.args.iter().any(|value| value
                .as_str()
                .is_some_and(|value| value.contains("failure hook rejected")))),
        "logs: {logs:?}"
    );
    service.dispose().await?;
    composition.dispose().await?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn canonical_aliases_share_one_exact_registration_and_relative_base_uses_the_loader_url()
-> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let actual = temporary.path().join("actual");
    let alias = temporary.path().join("alias");
    std::fs::create_dir(&actual)?;
    std::os::unix::fs::symlink(&actual, &alias)?;
    let url = url::Url::from_directory_path(temporary.path())
        .unwrap()
        .to_string();
    let context = Context::new().with_meta("loader.base_url", serde_json::json!(url));
    let composition = PluginCatalog::new().load_yaml(&context, "[]\n").await?;
    let service = HostHmrService::start(
        context.clone(),
        context.get(LOADER).unwrap(),
        Config {
            base: Some("alias".into()),
            root: vec![],
            ignored: vec![],
            debounce: 0,
            ..Config::default()
        },
        Arc::new(|| Box::pin(async { Ok(()) })),
    )?;
    assert_eq!(service.base_dir(), alias);
    let callback: seekdeep_hmr::ConfigRefresh = Arc::new(|| Box::pin(async { Ok(()) }));
    let effect = service
        .register_config("user.yml", callback.clone())
        .await?;
    assert!(
        service
            .register_config(actual.join("user.yml"), callback)
            .await
            .unwrap_err()
            .to_string()
            .contains("already registered")
    );
    effect.dispose().await?;
    service.dispose().await?;
    composition.dispose().await?;
    Ok(())
}

#[tokio::test]
async fn one_burst_of_two_dependencies_creates_one_replacement_generation() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let main = temporary.path().join("main.mjs");
    let left = temporary.path().join("left.mjs");
    let right = temporary.path().join("right.mjs");
    std::fs::write(
        &main,
        "import {left} from './left.mjs'; import {right} from './right.mjs'; export function apply(ctx){ctx.provide('sum',left+right)}\n",
    )?;
    std::fs::write(&left, "export const left=1\n")?;
    std::fs::write(&right, "export const right=2\n")?;
    let context = Context::new();
    let composition = PluginCatalog::new()
        .load_yaml_at(
            &context,
            "- id: main\n  name: ./main.mjs\n",
            temporary.path().join("cordis.yml"),
        )
        .await?;
    let reloads = Arc::new(AtomicUsize::new(0));
    context.events().on(
        &context,
        "hmr/reload",
        {
            let reloads = reloads.clone();
            move |_, _| {
                let reloads = reloads.clone();
                Box::pin(async move {
                    reloads.fetch_add(1, Ordering::SeqCst);
                    Ok(EventReply::Undefined)
                })
            }
        },
        EventOptions::default(),
    )?;
    let service = HostHmrService::start(
        context.clone(),
        context.get(LOADER).unwrap(),
        Config {
            base: Some(temporary.path().into()),
            root: vec![".".into()],
            ignored: vec![],
            debounce: 80,
            ..Config::default()
        },
        Arc::new(|| Box::pin(async { Ok(()) })),
    )?;
    std::fs::write(&left, "export const left=10\n")?;
    std::fs::write(&right, "export const right=20\n")?;
    let key = seekdeep_cordis::ServiceKey::<serde_json::Value>::new("sum");
    eventually(|| {
        context
            .get(key)
            .is_some_and(|value| *value == serde_json::json!(30))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(reloads.load(Ordering::SeqCst), 1);
    service.dispose().await?;
    composition.dispose().await?;
    Ok(())
}
