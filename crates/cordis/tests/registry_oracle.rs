//! Plugin-registry map and runtime-group semantics pinned from source Cordis.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use seekdeep_cordis::{Context, Plugin, fiber::EffectHandle};
use serde_json::json;

#[tokio::test]
async fn sibling_mounts_share_one_runtime_and_delete_joins_every_fiber() {
    let context = Context::new();
    let starts = Arc::new(AtomicUsize::new(0));
    let stops = Arc::new(AtomicUsize::new(0));
    let plugin = Plugin::new("probe", std::iter::empty::<String>(), {
        let starts = starts.clone();
        let stops = stops.clone();
        move |context, _| {
            starts.fetch_add(1, Ordering::AcqRel);
            let stops = stops.clone();
            Box::pin(async move {
                context.own(EffectHandle::synchronous("probe cleanup", move || {
                    stops.fetch_add(1, Ordering::AcqRel);
                    Ok(())
                }))?;
                Ok(())
            })
        }
    });
    let first = context.plugin(plugin.clone(), json!({"n":1})).unwrap();
    let second = context.plugin(plugin.clone(), json!({"n":2})).unwrap();
    first.await_settled().await.unwrap();
    second.await_settled().await.unwrap();

    let runtime = context.registry().get(&plugin).expect("runtime");
    assert_eq!(context.registry().len(), 1);
    assert_eq!(context.registry().fiber_count(), 2);
    assert!(context.registry().has(&plugin));
    assert_eq!(runtime.fibers.len(), 2);
    assert_eq!(
        runtime
            .fibers
            .iter()
            .map(|fiber| fiber.uid())
            .collect::<Vec<_>>(),
        [Some(1), Some(2)]
    );
    assert_eq!(context.registry().keys(), [plugin.id()]);
    assert_eq!(context.registry().values().len(), 1);
    assert_eq!(context.registry().entries().len(), 1);
    let mut visited = Vec::new();
    context
        .registry()
        .for_each(|runtime, id| visited.push((id, runtime.fibers.len())));
    assert_eq!(visited, [(plugin.id(), 2)]);
    assert_eq!(starts.load(Ordering::Acquire), 2);
    assert_eq!(stops.load(Ordering::Acquire), 0);

    first.dispose().await.unwrap();
    assert_eq!(first.uid(), None);
    assert_eq!(second.uid(), Some(2));
    assert_eq!(context.registry().len(), 1);
    assert_eq!(context.registry().fiber_count(), 1);
    assert_eq!(stops.load(Ordering::Acquire), 1);

    let removed = context
        .registry()
        .delete_joined(&plugin)
        .await
        .expect("removed runtime");
    assert_eq!(removed.fibers.len(), 1);
    assert_eq!(second.uid(), None);
    assert_eq!(context.registry().len(), 0);
    assert_eq!(context.registry().fiber_count(), 0);
    assert!(!context.registry().has(&plugin));
    assert_eq!(stops.load(Ordering::Acquire), 2);
    assert!(context.registry().delete(&plugin).is_none());
}
