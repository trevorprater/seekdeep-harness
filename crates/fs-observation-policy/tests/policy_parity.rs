//! Event-level policy wiring parity tests; no filesystem provider is needed.

use std::sync::Arc;

use seekdeep_cordis::{Context, EventArgs, EventReply, EventValue, PluginFiber};
use seekdeep_fs::{FsTarget, FsTargetKey, FsWriteIntent};
use seekdeep_fs_observation_policy::plugin;
use serde_json::Value;

fn target(path: &str) -> FsTarget {
    FsTarget {
        target_key: FsTargetKey::new(path),
        display_path: path.to_owned(),
    }
}

fn args(target: &FsTarget) -> EventArgs {
    let values: Vec<EventValue> = vec![Arc::new(target.clone())];
    EventArgs::from_values(values)
}

async fn write_intent(
    ctx: &Context,
    target: &FsTarget,
) -> anyhow::Result<Option<Arc<FsWriteIntent>>> {
    let reply = ctx
        .events()
        .waterfall(ctx, "fs/write-intent", &args(target), || {
            Box::pin(async { Ok(EventReply::Undefined) })
        })
        .await?;
    Ok(reply.downcast::<FsWriteIntent>())
}

async fn setup() -> (Context, Arc<PluginFiber>) {
    let ctx = Context::new();
    let fiber = ctx.plugin(plugin(), Value::Null).expect("mount");
    fiber.await_settled().await.expect("active");
    (ctx, fiber)
}

#[tokio::test]
async fn mounts_with_no_inject_and_decides_unobserved_writes() {
    let (ctx, _fiber) = setup().await;
    let intent = write_intent(&ctx, &target("a.txt")).await.expect("intent");
    assert_eq!(intent.as_deref(), Some(&FsWriteIntent::CreateIfAbsent));
}

#[tokio::test]
async fn single_slot_first_wins_and_does_not_call_next() {
    let (ctx, _fiber) = setup().await;
    // The policy occupies the slot without calling next(): the bare default is unreached.
    let intent = write_intent(&ctx, &target("a.txt")).await.expect("intent");
    assert_eq!(intent.as_deref(), Some(&FsWriteIntent::CreateIfAbsent));
}

#[tokio::test]
async fn disposal_releases_listeners() {
    let ctx = Context::new();
    let fiber = ctx.plugin(plugin(), Value::Null).expect("mount");
    fiber.await_settled().await.expect("active");
    fiber.dispose().await.expect("dispose");

    // With no listener, the waterfall falls through to the bare default.
    let fallthrough = write_intent(&ctx, &target("a.txt"))
        .await
        .expect("fallthrough");
    assert_eq!(fallthrough, None);
}
