//! `observeRun` publishes `subagent/end` before any awaiter of the run's result resumes,
//! exactly once, and even when nobody awaits the result, as the pinned source's
//! first-attached promise reaction did.

mod support;

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use futures::future::{BoxFuture, FutureExt as _, Shared};
use seekdeep_agent::Agent;
use seekdeep_cordis::{Context, EventOptions, EventReply};
use seekdeep_core::session::SessionId;
use seekdeep_llm::ContentBlock;
use seekdeep_subagent::{
    SubagentResult, SubagentRun, SubagentRunEndInfo, SubagentStopReason, observe_run,
};
use support::continuation::{MockAdapter, boot_with, create_agent, wait_for};

type GatedResult = Shared<BoxFuture<'static, Result<SubagentResult, String>>>;

/// A run whose result resolves when the test releases it.
struct GatedRun {
    id: SessionId,
    result: GatedResult,
}

impl SubagentRun for GatedRun {
    fn id(&self) -> &SessionId {
        &self.id
    }

    fn local_agent(&self) -> Option<&Arc<Agent>> {
        None
    }

    fn result(&self) -> BoxFuture<'static, anyhow::Result<SubagentResult>> {
        let result = self.result.clone();
        Box::pin(async move { result.await.map_err(anyhow::Error::msg) })
    }

    fn dispose(&self) -> BoxFuture<'static, anyhow::Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

fn gated_run(id: &str) -> (Arc<GatedRun>, tokio::sync::oneshot::Sender<()>) {
    let (release, released) = tokio::sync::oneshot::channel::<()>();
    let result: BoxFuture<'static, Result<SubagentResult, String>> = Box::pin(async move {
        released.await.map_err(|_| "release dropped".to_owned())?;
        Ok(SubagentResult {
            output: vec![ContentBlock::Text {
                text: "done".into(),
            }],
            structured: None,
            stop_reason: SubagentStopReason::Completed,
        })
    });
    (
        Arc::new(GatedRun {
            id: SessionId::new(id),
            result: result.shared(),
        }),
        release,
    )
}

fn record_ends(context: &Context, order: &Arc<Mutex<Vec<String>>>) {
    let order = Arc::clone(order);
    context
        .events()
        .on_sync(
            context,
            "subagent/end",
            move |_, args| {
                let info = args.get::<SubagentRunEndInfo>(0).expect("end info");
                order
                    .lock()
                    .unwrap()
                    .push(format!("end:{}", info.id.as_str()));
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn end_edge_precedes_every_awaiter_and_publishes_once() {
    let adapter = MockAdapter::new(Vec::new());
    let (context, dependencies, _subagents) = boot_with(None, &adapter, true, false).await;
    let parent = create_agent(&dependencies.agents, "parent", true).await;
    let order: Arc<Mutex<Vec<String>>> = Arc::default();
    record_ends(&context, &order);

    let (run, release) = gated_run("awaited");
    let observed = observe_run(&context, "gated", &parent.agent, run).unwrap();
    let awaiters = (0..4)
        .map(|index| {
            let observed = Arc::clone(&observed);
            let order = Arc::clone(&order);
            tokio::spawn(async move {
                let result = observed.result().await.unwrap();
                order.lock().unwrap().push(format!("awaiter:{index}"));
                result
            })
        })
        .collect::<Vec<_>>();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        order.lock().unwrap().is_empty(),
        "nothing resolves before release"
    );
    release.send(()).unwrap();
    for awaiter in awaiters {
        let result = awaiter.await.unwrap();
        assert_eq!(result.stop_reason, SubagentStopReason::Completed);
    }
    let recorded = order.lock().unwrap().clone();
    assert_eq!(recorded[0], "end:awaited", "{recorded:?}");
    assert_eq!(
        recorded
            .iter()
            .filter(|entry| entry.starts_with("end:"))
            .count(),
        1,
        "{recorded:?}"
    );
    assert_eq!(recorded.len(), 5, "{recorded:?}");

    // Nobody awaits this run's result; the observer still publishes its end edge once.
    let (run, release) = gated_run("unawaited");
    let _observed = observe_run(&context, "gated", &parent.agent, run).unwrap();
    release.send(()).unwrap();
    wait_for(Duration::from_secs(5), || {
        order
            .lock()
            .unwrap()
            .iter()
            .any(|entry| entry == "end:unawaited")
            .then_some(())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(
        order
            .lock()
            .unwrap()
            .iter()
            .filter(|entry| *entry == "end:unawaited")
            .count(),
        1
    );
    context.fiber().dispose().await.unwrap();
}
