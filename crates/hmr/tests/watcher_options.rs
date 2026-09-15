//! Chokidar options exercised through the Rust Host HMR service.

use std::{path::PathBuf, sync::Arc, time::Duration};

use parking_lot::Mutex;
use seekdeep_cordis::{Context, EventOptions, EventReply};
use seekdeep_hmr::{Config, HostHmrService};
use seekdeep_loader::{LOADER, PluginCatalog};
use serde_json::json;

#[tokio::test]
async fn polling_depth_and_write_stabilization_options_control_real_change_events()
-> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().canonicalize()?;
    let visible = root.join("visible.txt");
    let hidden = root.join("nested/hidden.txt");
    std::fs::create_dir(hidden.parent().unwrap())?;
    std::fs::write(&visible, "initial\n")?;
    std::fs::write(&hidden, "initial\n")?;
    let context = Context::new().intercept("logger", json!({"level":3}));
    let composition = PluginCatalog::new().load_yaml(&context, "[]\n").await?;
    let changes = Arc::new(Mutex::new(Vec::<PathBuf>::new()));
    context.events().on(
        &context,
        "hmr/change",
        {
            let changes = changes.clone();
            move |_, args| {
                let changes = changes.clone();
                let path = args.get::<PathBuf>(0).expect("changed file");
                Box::pin(async move {
                    changes.lock().push((*path).clone());
                    Ok(EventReply::Undefined)
                })
            }
        },
        EventOptions::default(),
    )?;
    let config: Config = serde_json::from_value(json!({
        "base": root,
        "root": ["."],
        "debounce": 10,
        "ignored": [],
        "depth": 0,
        "usePolling": true,
        "interval": 15,
        "binaryInterval": 15,
        "awaitWriteFinish": {"stabilityThreshold":250,"pollInterval":15},
        "cwd": root.join("unused-cwd"),
        "ignoreInitial": false,
    }))?;
    let service = HostHmrService::start(
        context.clone(),
        context.get(LOADER).expect("attached loader"),
        config,
        Arc::new(|| Box::pin(async { Ok(()) })),
    )?;

    std::fs::write(&visible, "partial\n")?;
    std::fs::write(&hidden, "nested file changed\n")?;
    tokio::time::sleep(Duration::from_millis(70)).await;
    assert!(
        changes.lock().is_empty(),
        "a change escaped write stabilization"
    );
    std::fs::write(&visible, "completed content after the write burst\n")?;
    tokio::time::timeout(Duration::from_secs(10), async {
        while changes.lock().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    assert_eq!(*changes.lock(), [visible]);
    service.dispose().await?;
    composition.dispose().await?;
    Ok(())
}
