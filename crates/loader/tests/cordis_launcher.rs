//! Generic Cordis launcher file, base-URL, hold, and shutdown parity.

use std::time::Duration;

#[tokio::test]
async fn missing_file_fails_and_empty_file_exits_without_a_live_handle() {
    let missing = tempfile::tempdir().unwrap();
    let error =
        seekdeep_loader::launcher::run_cordis_file(missing.path(), futures::future::pending())
            .await
            .unwrap_err();
    assert!(error.to_string().contains("cordis.yml"), "{error:#}");

    let empty = tempfile::tempdir().unwrap();
    std::fs::write(empty.path().join("cordis.yml"), "[]\n").unwrap();
    tokio::time::timeout(
        Duration::from_secs(1),
        seekdeep_loader::launcher::run_cordis_file(empty.path(), futures::future::pending()),
    )
    .await
    .expect("empty composition must not hold")
    .unwrap();
}

#[tokio::test]
async fn relative_plugin_uses_cwd_base_url_and_active_generation_waits_for_shutdown() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("plugin.mjs"),
        "export function apply() {}\n",
    )
    .unwrap();
    std::fs::write(
        directory.path().join("cordis.yml"),
        "- id: probe\n  name: ./plugin.mjs\n",
    )
    .unwrap();
    let (shutdown, wait) = tokio::sync::oneshot::channel::<()>();
    let path = directory.path().to_owned();
    let task = tokio::spawn(async move {
        seekdeep_loader::launcher::run_cordis_file(&path, async {
            let _ = wait.await;
            Ok(())
        })
        .await
    });
    tokio::task::yield_now().await;
    assert!(!task.is_finished());
    shutdown.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .expect("launcher shutdown")
        .unwrap()
        .unwrap();
}
