//! Source-turn registration, disposal starts, and injectable continuation scheduling.

use std::sync::Arc;

use parking_lot::Mutex;
use seekdeep_cordis::{
    Context, EventArgs, EventOptions, EventReply, Plugin, PluginFiber,
    fiber::EffectHandle,
    plugin::{LifecycleScheduler, LifecycleTask},
};
use serde_json::{Value, json};

#[derive(Default)]
struct ManualScheduler(Mutex<Vec<LifecycleTask>>);

impl LifecycleScheduler for ManualScheduler {
    fn schedule(&self, task: LifecycleTask) {
        self.0.lock().push(task);
    }
}

impl ManualScheduler {
    async fn settle(&self) {
        loop {
            let tasks = std::mem::take(&mut *self.0.lock());
            if tasks.is_empty() {
                break;
            }
            futures::future::join_all(tasks).await;
        }
    }
}

fn plugin(
    generation: &'static str,
    trace: Arc<Mutex<Vec<String>>>,
    cleanup: Arc<tokio::sync::Semaphore>,
) -> Plugin {
    Plugin::new(
        generation,
        std::iter::empty::<&str>(),
        move |context, config| {
            let trace = trace.clone();
            let cleanup = cleanup.clone();
            Box::pin(async move {
                let id = config.as_str().unwrap().to_owned();
                trace.lock().push(format!("apply:{generation}:{id}"));
                context.provide_named(&id, Arc::new(json!(generation)))?;
                context.own(EffectHandle::new("trace cleanup", move || {
                    let trace = trace.clone();
                    let id = id.clone();
                    let cleanup = cleanup.clone();
                    Box::pin(async move {
                        trace
                            .lock()
                            .push(format!("dispose-start:{generation}:{id}"));
                        if generation == "old" {
                            cleanup.acquire().await?.forget();
                        }
                        trace.lock().push(format!("dispose-end:{generation}:{id}"));
                        Ok(())
                    })
                }))?;
                Ok(())
            })
        },
    )
}

fn observe_plugin_publications(context: &Context) -> anyhow::Result<Arc<Mutex<Vec<Value>>>> {
    let publications = Arc::new(Mutex::new(Vec::new()));
    context.events().on(
        context,
        "internal/plugin",
        {
            let publications = publications.clone();
            move |context, args| {
                let fiber = args.get::<PluginFiber>(0).unwrap();
                let id = fiber.config().as_str().unwrap().to_owned();
                publications.lock().push(json!({
                    "generation": fiber.plugin_name(),
                    "id": id,
                    "disposed": fiber.uid().is_none(),
                    "state": format!("{:?}", fiber.fiber().state()),
                    "strict": context.get_named::<Value>(&id).as_deref(),
                    "loose": context.get_named_relaxed::<Value>(&id).as_deref(),
                }));
                Box::pin(async { Ok(EventReply::Undefined) })
            }
        },
        EventOptions::default(),
    )?;
    Ok(publications)
}

#[tokio::test]
async fn source_turn_defers_bodies_and_starts_old_disposers_before_new_applies()
-> anyhow::Result<()> {
    let context = Context::new();
    let trace = Arc::new(Mutex::new(Vec::new()));
    let cleanup = Arc::new(tokio::sync::Semaphore::new(0));
    let old = plugin("old", trace.clone(), cleanup.clone());
    let first = context.plugin(old.clone(), json!("first"))?;
    first.await_settled().await?;
    let second = context.plugin(old.clone(), json!("second"))?;
    second.await_settled().await?;
    trace.lock().clear();
    let scheduler = Arc::new(ManualScheduler::default());
    context
        .registry()
        .set_lifecycle_scheduler(scheduler.clone());
    let publications = observe_plugin_publications(&context)?;
    context.events().on(
        &context,
        "hmr/reload",
        {
            let trace = trace.clone();
            move |_, _| {
                trace.lock().push("event:hmr/reload".to_owned());
                Box::pin(async { Ok(EventReply::Undefined) })
            }
        },
        EventOptions::default(),
    )?;
    let deferred = context.registry().defer_lifecycle();
    let nested = context.registry().defer_lifecycle();
    context
        .registry()
        .delete_with_disposal(&old, seekdeep_cordis::DisposalScheduling::Concurrent)
        .expect("old runtime");
    let new = plugin("new", trace.clone(), cleanup.clone());
    let first_new = context.plugin(new.clone(), json!("first"))?;
    let second_new = context.plugin(new, json!("second"))?;
    drop(nested);
    assert!(trace.lock().is_empty());
    assert_eq!(
        *publications.lock(),
        [
            json!({"generation":"old","id":"first","disposed":true,"state":"Active","strict":"old","loose":"old"}),
            json!({"generation":"old","id":"second","disposed":true,"state":"Active","strict":"old","loose":"old"}),
            json!({"generation":"new","id":"first","disposed":false,"state":"Pending","strict":null,"loose":"old"}),
            json!({"generation":"new","id":"second","disposed":false,"state":"Pending","strict":null,"loose":"old"}),
        ]
    );
    assert_eq!(
        first.fiber().state(),
        seekdeep_cordis::FiberState::Unloading
    );
    assert_eq!(
        second.fiber().state(),
        seekdeep_cordis::FiberState::Unloading
    );
    assert_eq!(
        first_new.fiber().state(),
        seekdeep_cordis::FiberState::Loading
    );
    assert_eq!(
        second_new.fiber().state(),
        seekdeep_cordis::FiberState::Loading
    );
    context
        .events()
        .emit(&context, "hmr/reload", &EventArgs::new())?;
    drop(deferred);
    assert_eq!(
        *trace.lock(),
        [
            "event:hmr/reload",
            "dispose-start:old:first",
            "dispose-start:old:second",
            "apply:new:first",
            "apply:new:second"
        ]
    );
    cleanup.add_permits(2);
    scheduler.settle().await;
    first_new.await_settled().await?;
    second_new.await_settled().await?;
    assert_eq!(
        context.get_named::<Value>("first").as_deref(),
        Some(&json!("new"))
    );
    assert_eq!(
        context.get_named::<Value>("second").as_deref(),
        Some(&json!("new"))
    );
    assert_eq!(
        &trace.lock()[5..],
        ["dispose-end:old:first", "dispose-end:old:second"]
    );
    context.fiber().dispose().await?;
    scheduler.settle().await;
    Ok(())
}

#[tokio::test]
async fn publication_rollback_cancels_queued_candidate_bodies_before_their_first_poll()
-> anyhow::Result<()> {
    let context = Context::new();
    let trace = Arc::new(Mutex::new(Vec::new()));
    let cleanup = Arc::new(tokio::sync::Semaphore::new(2));
    let old = plugin("old", trace.clone(), cleanup.clone());
    for name in ["first", "second"] {
        context
            .plugin(old.clone(), json!(name))?
            .await_settled()
            .await?;
    }
    trace.lock().clear();
    let scheduler = Arc::new(ManualScheduler::default());
    context
        .registry()
        .set_lifecycle_scheduler(scheduler.clone());
    context.events().on(
        &context,
        "internal/plugin",
        move |_, args| {
            let fiber = args.get::<PluginFiber>(0).unwrap();
            Box::pin(async move {
                anyhow::ensure!(
                    fiber.uid().is_none()
                        || fiber.plugin_name() != "new"
                        || fiber.config() != Value::String("second".to_owned()),
                    "second replacement registration rejected"
                );
                Ok(EventReply::Undefined)
            })
        },
        EventOptions::default(),
    )?;
    let deferred = context.registry().defer_lifecycle();
    context
        .registry()
        .delete_with_disposal(&old, seekdeep_cordis::DisposalScheduling::Concurrent)
        .unwrap();
    let new = plugin("new", trace.clone(), cleanup.clone());
    context.plugin(new.clone(), json!("first"))?;
    assert!(
        context
            .plugin(new.clone(), json!("second"))
            .unwrap_err()
            .to_string()
            .contains("second replacement registration rejected")
    );
    context
        .registry()
        .delete_with_disposal(&new, seekdeep_cordis::DisposalScheduling::Concurrent);
    let first = context.plugin(old.clone(), json!("first"))?;
    let second = context.plugin(old, json!("second"))?;
    drop(deferred);
    assert_eq!(
        *trace.lock(),
        [
            "dispose-start:old:first",
            "dispose-end:old:first",
            "dispose-start:old:second",
            "dispose-end:old:second",
            "apply:old:first",
            "apply:old:second"
        ]
    );
    scheduler.settle().await;
    first.await_settled().await?;
    second.await_settled().await?;
    cleanup.add_permits(2);
    context.fiber().dispose().await?;
    scheduler.settle().await;
    Ok(())
}

#[tokio::test]
async fn runtime_deletion_releases_the_plugin_capture_before_its_parent_context()
-> anyhow::Result<()> {
    let context = Context::new();
    let captured = Arc::new(());
    let weak = Arc::downgrade(&captured);
    let plugin = Plugin::new("owned capture", std::iter::empty::<&str>(), move |_, _| {
        let captured = captured.clone();
        Box::pin(async move {
            drop(captured);
            Ok(())
        })
    });
    let fiber = context.plugin(plugin.clone(), Value::Null)?;
    fiber.await_settled().await?;
    let scheduler = Arc::new(ManualScheduler::default());
    context
        .registry()
        .set_lifecycle_scheduler(scheduler.clone());
    let deleted = context
        .registry()
        .delete_with_disposal(&plugin, seekdeep_cordis::DisposalScheduling::Concurrent)
        .unwrap();
    drop(plugin);
    fiber.dispose().await?;
    scheduler.settle().await;
    drop(deleted);
    drop(fiber);
    assert!(
        weak.upgrade().is_none(),
        "the parent structural effect retained a disposed plugin"
    );
    assert!(context.registry().is_empty());
    context.fiber().dispose().await?;
    Ok(())
}
