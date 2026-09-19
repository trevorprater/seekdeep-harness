//! The one-shot lifecycle pair's ordering contract, against the assembled
//! stack: `subagent/end` publishes before the run's result is observable, as
//! the source's promise-reaction order guarantees, and it publishes exactly
//! once whether or not anyone awaits the run.

mod support;

use std::{sync::Arc, time::Duration};

use futures::{
    FutureExt as _,
    future::{BoxFuture, Shared as SharedFuture},
};
use seekdeep_agent::Agent;
use seekdeep_cordis::{EventOptions, EventReply};
use seekdeep_core::session::SessionId;
use seekdeep_llm::ContentBlock;
use seekdeep_subagent::{
    SubagentResult, SubagentRun, SubagentRunEndInfo, SubagentStopReason, observe_run,
};
use support::continuation::*;

type Outcome = Result<SubagentResult, String>;

/// A one-shot run whose result the test settles by hand.
struct GatedRun {
    id: SessionId,
    outcome: SharedFuture<BoxFuture<'static, Outcome>>,
}

impl GatedRun {
    fn open(id: &str) -> (tokio::sync::oneshot::Sender<Outcome>, Arc<dyn SubagentRun>) {
        let (sender, receiver) = tokio::sync::oneshot::channel::<Outcome>();
        let outcome = async move {
            receiver
                .await
                .unwrap_or_else(|_| Err("the run was never settled".to_owned()))
        }
        .boxed()
        .shared();
        (
            sender,
            Arc::new(Self {
                id: SessionId::new(id),
                outcome,
            }),
        )
    }
}

impl SubagentRun for GatedRun {
    fn id(&self) -> &SessionId {
        &self.id
    }

    fn local_agent(&self) -> Option<&Arc<Agent>> {
        None
    }

    fn result(&self) -> BoxFuture<'static, anyhow::Result<SubagentResult>> {
        Box::pin(
            self.outcome
                .clone()
                .map(|outcome| outcome.map_err(anyhow::Error::msg)),
        )
    }

    fn dispose(&self) -> BoxFuture<'static, anyhow::Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

fn record_ends(stack: &Stack) -> Shared<Vec<SubagentRunEndInfo>> {
    let ends: Shared<Vec<SubagentRunEndInfo>> = Arc::default();
    let observed = Arc::clone(&ends);
    stack
        .context
        .events()
        .on_sync(
            &stack.context,
            "subagent/end",
            move |_, args| {
                if let Some(info) = args.get::<SubagentRunEndInfo>(0) {
                    observed.lock().unwrap().push((*info).clone());
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    ends
}

fn completed(text: &str) -> SubagentResult {
    SubagentResult {
        output: vec![ContentBlock::Text { text: text.into() }],
        structured: None,
        stop_reason: SubagentStopReason::Completed,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_end_edge_publishes_before_any_awaiter_resumes_with_the_result() {
    let stack = setup(vec![]).await;
    let ends = record_ends(&stack);
    let (settle, run) = GatedRun::open("child-ordered");
    let observed = observe_run(&stack.context, "scripted", &stack.parent.agent, run).unwrap();

    // Two independent awaiters, as a tool and a settlement watcher would be.
    let awaiters = (0..2)
        .map(|_| {
            let observed = Arc::clone(&observed);
            let ends = Arc::clone(&ends);
            tokio::spawn(async move {
                let result = observed.result().await.unwrap();
                (result, ends.lock().unwrap().len())
            })
        })
        .collect::<Vec<_>>();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        ends.lock().unwrap().is_empty(),
        "no end edge before settlement"
    );

    settle.send(Ok(completed("DIRECT_CHILD_OK"))).unwrap();
    for awaiter in awaiters {
        let (result, ends_seen) = awaiter.await.unwrap();
        assert_eq!(result, completed("DIRECT_CHILD_OK"));
        assert_eq!(
            ends_seen, 1,
            "the awaiter resumed before the end edge published"
        );
    }
    let ends = ends.lock().unwrap().clone();
    assert_eq!(ends.len(), 1, "one end edge per run: {ends:?}");
    assert_eq!(ends[0].stop_reason, SubagentStopReason::Completed);
    assert_eq!(
        ends[0].last_assistant_message,
        Some(vec![ContentBlock::Text {
            text: "DIRECT_CHILD_OK".into()
        }])
    );
    assert_eq!(ends[0].id, SessionId::new("child-ordered"));
}

#[tokio::test]
async fn the_end_edge_publishes_without_an_awaiter_and_reports_a_failed_channel() {
    let stack = setup(vec![]).await;
    let ends = record_ends(&stack);
    let (settle, run) = GatedRun::open("child-unawaited");
    let observed = observe_run(&stack.context, "scripted", &stack.parent.agent, run).unwrap();
    drop(observed);
    settle.send(Err("channel closed".to_owned())).unwrap();
    let ends = wait_for(Duration::from_secs(5), || {
        let ends = ends.lock().unwrap();
        (!ends.is_empty()).then(|| ends.clone())
    })
    .await;
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].stop_reason, SubagentStopReason::Error);
    assert_eq!(ends[0].last_assistant_message, None);
}
