//! Bounded per-session write batching with durable flush barriers.

use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use futures::future::BoxFuture;
use seekdeep_core::session::SessionEvent;
use tokio::sync::{mpsc, oneshot};

type WriteFuture = BoxFuture<'static, anyhow::Result<()>>;
type WriteFn = Arc<dyn Fn(Vec<SessionEvent>) -> WriteFuture + Send + Sync>;
type FailureFn = Arc<dyn Fn(&anyhow::Error) + Send + Sync>;

enum Command {
    Enqueue(SessionEvent),
    Flush(oneshot::Sender<anyhow::Result<()>>),
    CancelAutomaticWait,
}

struct ActiveWrite {
    batch: Vec<SessionEvent>,
    background: bool,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

/// One live session's pending events, fixed batching deadline, active durable
/// write, failure retention, and explicit quiescence barrier.
#[derive(Clone)]
pub struct SessionWriteBehind {
    sender: mpsc::UnboundedSender<Command>,
    has_work: Arc<AtomicBool>,
}

impl std::fmt::Debug for SessionWriteBehind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionWriteBehind")
            .field("has_work", &self.has_work())
            .finish_non_exhaustive()
    }
}

impl SessionWriteBehind {
    /// Starts one controller actor.
    #[must_use]
    pub fn new<Write, Fut, Report>(
        max_delay: Duration,
        write: Write,
        report_background_failure: Report,
    ) -> Self
    where
        Write: Fn(Vec<SessionEvent>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
        Report: Fn(&anyhow::Error) + Send + Sync + 'static,
    {
        let (sender, receiver) = mpsc::unbounded_channel();
        let has_work = Arc::new(AtomicBool::new(false));
        let actor_has_work = has_work.clone();
        let write: WriteFn = Arc::new(move |events| Box::pin(write(events)));
        let report: FailureFn = Arc::new(report_background_failure);
        tokio::spawn(run_actor(
            receiver,
            actor_has_work,
            max_delay,
            write,
            report,
        ));
        Self { sender, has_work }
    }

    /// Whether this controller owns queued events or an active durable write.
    #[must_use]
    pub fn has_work(&self) -> bool {
        self.has_work.load(Ordering::Acquire)
    }

    /// Copies one event into the persistence-owned queue and starts a fixed
    /// deadline when the automatic path is idle.
    ///
    /// # Errors
    ///
    /// Returns when the controller actor has already stopped.
    pub fn enqueue(&self, event: &SessionEvent) -> anyhow::Result<()> {
        self.has_work.store(true, Ordering::Release);
        self.sender
            .send(Command::Enqueue(event.clone()))
            .map_err(|_| anyhow::anyhow!("session write-behind controller stopped"))
    }

    /// Cancels batching delay and durably drains through a quiescent point.
    /// Concurrent callers join the same logical drain.
    ///
    /// # Errors
    ///
    /// Returns the durable retry failure or actor shutdown.
    pub async fn flush(&self) -> anyhow::Result<()> {
        let (sender, receiver) = oneshot::channel();
        self.sender
            .send(Command::Flush(sender))
            .map_err(|_| anyhow::anyhow!("session write-behind controller stopped"))?;
        receiver
            .await
            .map_err(|_| anyhow::anyhow!("session write-behind flush was abandoned"))?
    }

    /// Cancels the current automatic deadline without draining retained work.
    pub fn cancel_automatic_wait(&self) {
        let _ = self.sender.send(Command::CancelAutomaticWait);
    }
}

#[allow(clippy::too_many_lines)]
async fn run_actor(
    mut commands: mpsc::UnboundedReceiver<Command>,
    has_work: Arc<AtomicBool>,
    max_delay: Duration,
    write: WriteFn,
    report: FailureFn,
) {
    let mut pending = Vec::new();
    let mut timer: Option<Pin<Box<tokio::time::Sleep>>> = None;
    let mut active: Option<ActiveWrite> = None;
    let mut deadline_expired = false;
    let mut automatic_paused = false;
    let mut barriers = Vec::<oneshot::Sender<anyhow::Result<()>>>::new();
    let mut commands_closed = false;

    loop {
        tokio::select! {
            biased;
            command = wait_command(&mut commands, commands_closed) => {
                let Some(command) = command else {
                    commands_closed = true;
                    if active.is_none() && pending.is_empty() {
                        break;
                    }
                    if active.is_none() {
                        start_write(&mut active, &mut pending, false, &mut timer, &mut deadline_expired, &write);
                    }
                    continue;
                };
                match command {
                    Command::Enqueue(event) => {
                        let was_empty = pending.is_empty();
                        pending.push(event);
                        has_work.store(true, Ordering::Release);
                        if !barriers.is_empty() {
                            if active.is_none() {
                                start_write(&mut active, &mut pending, false, &mut timer, &mut deadline_expired, &write);
                            }
                        } else if automatic_paused {
                            automatic_paused = false;
                            deadline_expired = false;
                            timer = Some(Box::pin(tokio::time::sleep(max_delay)));
                        } else if was_empty {
                            timer = Some(Box::pin(tokio::time::sleep(max_delay)));
                        }
                    }
                    Command::Flush(waiter) => {
                        timer = None;
                        deadline_expired = false;
                        automatic_paused = false;
                        barriers.push(waiter);
                        if active.is_none() {
                            if pending.is_empty() {
                                resolve_barriers(&mut barriers, None);
                                has_work.store(false, Ordering::Release);
                            } else {
                                start_write(&mut active, &mut pending, false, &mut timer, &mut deadline_expired, &write);
                            }
                        }
                    }
                    Command::CancelAutomaticWait => {
                        timer = None;
                        deadline_expired = false;
                    }
                }
            }
            () = wait_timer(&mut timer) => {
                timer = None;
                if active.is_some() {
                    deadline_expired = true;
                } else if !pending.is_empty() {
                    start_write(&mut active, &mut pending, true, &mut timer, &mut deadline_expired, &write);
                }
            }
            result = wait_active(&mut active) => {
                let Some((batch, background, result)) = result else { continue };
                match result {
                    Ok(()) => {
                        if !barriers.is_empty() {
                            if pending.is_empty() {
                                resolve_barriers(&mut barriers, None);
                                has_work.store(false, Ordering::Release);
                            } else {
                                start_write(&mut active, &mut pending, false, &mut timer, &mut deadline_expired, &write);
                            }
                        } else if !pending.is_empty() && deadline_expired {
                            deadline_expired = false;
                            start_write(&mut active, &mut pending, true, &mut timer, &mut deadline_expired, &write);
                        } else if pending.is_empty() {
                            has_work.store(false, Ordering::Release);
                        }
                    }
                    Err(error) => {
                        let mut retained = batch;
                        retained.append(&mut pending);
                        pending = retained;
                        timer = None;
                        deadline_expired = false;
                        automatic_paused = true;
                        if background {
                            report(&error);
                        }
                        if !barriers.is_empty() {
                            if background {
                                automatic_paused = false;
                                start_write(&mut active, &mut pending, false, &mut timer, &mut deadline_expired, &write);
                            } else {
                                resolve_barriers(&mut barriers, Some(&error.to_string()));
                            }
                        }
                    }
                }
            }
        }
    }

    if !barriers.is_empty() {
        resolve_barriers(
            &mut barriers,
            Some("session write-behind controller stopped"),
        );
    }
}

fn start_write(
    active: &mut Option<ActiveWrite>,
    pending: &mut Vec<SessionEvent>,
    background: bool,
    timer: &mut Option<Pin<Box<tokio::time::Sleep>>>,
    deadline_expired: &mut bool,
    write: &WriteFn,
) {
    debug_assert!(active.is_none());
    let batch = std::mem::take(pending);
    *timer = None;
    *deadline_expired = false;
    let operation = write(batch.clone());
    *active = Some(ActiveWrite {
        batch,
        background,
        task: tokio::spawn(operation),
    });
}

async fn wait_timer(timer: &mut Option<Pin<Box<tokio::time::Sleep>>>) {
    match timer {
        Some(timer) => timer.as_mut().await,
        None => futures::future::pending().await,
    }
}

async fn wait_command(
    commands: &mut mpsc::UnboundedReceiver<Command>,
    closed: bool,
) -> Option<Command> {
    if closed {
        futures::future::pending().await
    } else {
        commands.recv().await
    }
}

async fn wait_active(
    active: &mut Option<ActiveWrite>,
) -> Option<(Vec<SessionEvent>, bool, anyhow::Result<()>)> {
    let Some(active_write) = active else {
        return futures::future::pending().await;
    };
    let result = (&mut active_write.task)
        .await
        .map_err(|error| anyhow::anyhow!("session write task failed: {error}"))
        .and_then(std::convert::identity);
    let finished = active.take().expect("active write exists after await");
    Some((finished.batch, finished.background, result))
}

fn resolve_barriers(barriers: &mut Vec<oneshot::Sender<anyhow::Result<()>>>, error: Option<&str>) {
    for barrier in std::mem::take(barriers) {
        let value = match &error {
            Some(error) => Err(anyhow::Error::msg((*error).to_owned())),
            None => Ok(()),
        };
        let _ = barrier.send(value);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use serde_json::json;
    use tokio::sync::Notify;

    use super::*;

    fn event(seq: u64) -> SessionEvent {
        SessionEvent {
            event_type: "test/event".to_owned(),
            seq,
            time: 1,
            data: json!({"seq": seq}),
            source_event_seqs: None,
            surface_op: None,
            ignorable: Some(true),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn fixed_window_batches_events_and_flushes_immediately() {
        let batches = Arc::new(Mutex::new(Vec::<Vec<u64>>::new()));
        let sink = batches.clone();
        let writes = SessionWriteBehind::new(
            Duration::from_millis(200),
            move |events| {
                let sink = sink.clone();
                async move {
                    sink.lock()
                        .expect("batches")
                        .push(events.iter().map(|event| event.seq).collect());
                    Ok(())
                }
            },
            |_| {},
        );
        writes.enqueue(&event(0)).expect("enqueue");
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(199)).await;
        writes.enqueue(&event(1)).expect("enqueue");
        tokio::task::yield_now().await;
        assert!(batches.lock().expect("batches").is_empty());
        tokio::time::advance(Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(*batches.lock().expect("batches"), vec![vec![0, 1]]);

        writes.enqueue(&event(2)).expect("enqueue");
        writes.flush().await.expect("flush");
        assert_eq!(*batches.lock().expect("batches"), vec![vec![0, 1], vec![2]]);
        assert!(!writes.has_work());
    }

    #[tokio::test]
    async fn failed_background_batch_is_retained_and_flush_retries_it() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let observed = Arc::new(AtomicUsize::new(0));
        let attempt_count = attempts.clone();
        let observed_count = observed.clone();
        let batches = Arc::new(Mutex::new(Vec::<Vec<u64>>::new()));
        let sink = batches.clone();
        let writes = SessionWriteBehind::new(
            Duration::from_millis(1),
            move |events| {
                let attempt_count = attempt_count.clone();
                let sink = sink.clone();
                async move {
                    let attempt = attempt_count.fetch_add(1, Ordering::SeqCst);
                    if attempt == 0 {
                        anyhow::bail!("first write failed");
                    }
                    sink.lock()
                        .expect("batches")
                        .push(events.iter().map(|event| event.seq).collect());
                    Ok(())
                }
            },
            move |_| {
                observed_count.fetch_add(1, Ordering::SeqCst);
            },
        );
        writes.enqueue(&event(0)).expect("enqueue");
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(writes.has_work());
        assert_eq!(observed.load(Ordering::SeqCst), 1);
        writes.enqueue(&event(1)).expect("enqueue");
        writes.flush().await.expect("retry flush");
        assert_eq!(*batches.lock().expect("batches"), vec![vec![0, 1]]);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn flush_joins_overlapping_background_failure_and_retries_prefix() {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let attempts = Arc::new(AtomicUsize::new(0));
        let successful = Arc::new(Mutex::new(Vec::<Vec<u64>>::new()));
        let entered_write = entered.clone();
        let release_write = release.clone();
        let attempt_count = attempts.clone();
        let sink = successful.clone();
        let writes = SessionWriteBehind::new(
            Duration::from_millis(1),
            move |events| {
                let entered_write = entered_write.clone();
                let release_write = release_write.clone();
                let attempt_count = attempt_count.clone();
                let sink = sink.clone();
                async move {
                    let attempt = attempt_count.fetch_add(1, Ordering::SeqCst);
                    if attempt == 0 {
                        entered_write.notify_one();
                        release_write.notified().await;
                        anyhow::bail!("background failed");
                    }
                    sink.lock()
                        .expect("successful")
                        .push(events.iter().map(|event| event.seq).collect());
                    Ok(())
                }
            },
            |_| {},
        );
        writes.enqueue(&event(0)).expect("enqueue");
        entered.notified().await;
        writes.enqueue(&event(1)).expect("enqueue");
        let flushing = tokio::spawn({
            let writes = writes.clone();
            async move { writes.flush().await }
        });
        release.notify_one();
        flushing.await.expect("join").expect("flush retry");
        assert_eq!(*successful.lock().expect("successful"), vec![vec![0, 1]]);
    }

    #[tokio::test]
    async fn concurrent_flush_callers_share_failure_and_pending_is_not_lost() {
        let writes = SessionWriteBehind::new(
            Duration::from_secs(60),
            |_| async { anyhow::bail!("durability failed") },
            |_| {},
        );
        writes.enqueue(&event(0)).expect("enqueue");
        let (first, second) = tokio::join!(writes.flush(), writes.flush());
        assert!(
            first
                .expect_err("first")
                .to_string()
                .contains("durability failed")
        );
        assert!(
            second
                .expect_err("second")
                .to_string()
                .contains("durability failed")
        );
        assert!(writes.has_work());
    }

    #[tokio::test(start_paused = true)]
    async fn twenty_staggered_events_share_the_first_fixed_window() {
        let batches = Arc::new(Mutex::new(Vec::<Vec<u64>>::new()));
        let sink = batches.clone();
        let writes = SessionWriteBehind::new(
            Duration::from_millis(200),
            move |events| {
                let sink = sink.clone();
                async move {
                    sink.lock()
                        .expect("batches")
                        .push(events.iter().map(|event| event.seq).collect());
                    Ok(())
                }
            },
            |_| {},
        );
        writes.enqueue(&event(0)).expect("enqueue first");
        tokio::task::yield_now().await;
        for sequence in 1..20 {
            tokio::time::advance(Duration::from_millis(10)).await;
            writes.enqueue(&event(sequence)).expect("enqueue tail");
            tokio::task::yield_now().await;
        }
        assert!(batches.lock().expect("batches").is_empty());
        tokio::time::advance(Duration::from_millis(10)).await;
        tokio::task::yield_now().await;
        assert_eq!(
            *batches.lock().expect("batches"),
            vec![(0..20).collect::<Vec<_>>()]
        );
        writes.flush().await.expect("flush");
    }

    #[tokio::test(start_paused = true)]
    async fn work_after_a_quiescent_barrier_starts_a_new_window() {
        let batches = Arc::new(Mutex::new(Vec::<Vec<u64>>::new()));
        let sink = batches.clone();
        let writes = SessionWriteBehind::new(
            Duration::from_millis(200),
            move |events| {
                let sink = sink.clone();
                async move {
                    sink.lock()
                        .expect("batches")
                        .push(events.iter().map(|event| event.seq).collect());
                    Ok(())
                }
            },
            |_| {},
        );
        writes.flush().await.expect("empty barrier");
        writes.enqueue(&event(0)).expect("enqueue after barrier");
        tokio::task::yield_now().await;
        assert!(batches.lock().expect("batches").is_empty());
        tokio::time::advance(Duration::from_millis(199)).await;
        tokio::task::yield_now().await;
        assert!(batches.lock().expect("batches").is_empty());
        tokio::time::advance(Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(*batches.lock().expect("batches"), vec![vec![0]]);
    }

    #[tokio::test(start_paused = true)]
    async fn expired_tail_runs_immediately_after_active_write() {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let batches = Arc::new(Mutex::new(Vec::<Vec<u64>>::new()));
        let attempt = Arc::new(AtomicUsize::new(0));
        let writes = SessionWriteBehind::new(
            Duration::from_millis(200),
            {
                let entered = entered.clone();
                let release = release.clone();
                let batches = batches.clone();
                let attempt = attempt.clone();
                move |events| {
                    let entered = entered.clone();
                    let release = release.clone();
                    let batches = batches.clone();
                    let attempt = attempt.clone();
                    async move {
                        batches
                            .lock()
                            .expect("batches")
                            .push(events.iter().map(|event| event.seq).collect());
                        if attempt.fetch_add(1, Ordering::SeqCst) == 0 {
                            entered.notify_one();
                            release.notified().await;
                        }
                        Ok(())
                    }
                }
            },
            |_| {},
        );
        writes.enqueue(&event(0)).expect("enqueue first");
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(200)).await;
        entered.notified().await;
        writes.enqueue(&event(1)).expect("enqueue tail");
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(200)).await;
        release.notify_one();
        for _ in 0..10 {
            tokio::task::yield_now().await;
            if batches.lock().expect("batches").len() == 2 {
                break;
            }
        }
        assert_eq!(*batches.lock().expect("batches"), vec![vec![0], vec![1]]);
        writes.flush().await.expect("flush");
    }

    #[tokio::test(start_paused = true)]
    async fn unexpired_tail_keeps_its_original_deadline() {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let batches = Arc::new(Mutex::new(Vec::<Vec<u64>>::new()));
        let attempt = Arc::new(AtomicUsize::new(0));
        let writes = SessionWriteBehind::new(
            Duration::from_millis(200),
            {
                let entered = entered.clone();
                let release = release.clone();
                let batches = batches.clone();
                let attempt = attempt.clone();
                move |events| {
                    let entered = entered.clone();
                    let release = release.clone();
                    let batches = batches.clone();
                    let attempt = attempt.clone();
                    async move {
                        batches
                            .lock()
                            .expect("batches")
                            .push(events.iter().map(|event| event.seq).collect());
                        if attempt.fetch_add(1, Ordering::SeqCst) == 0 {
                            entered.notify_one();
                            release.notified().await;
                        }
                        Ok(())
                    }
                }
            },
            |_| {},
        );
        writes.enqueue(&event(0)).expect("enqueue first");
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(200)).await;
        entered.notified().await;
        writes.enqueue(&event(1)).expect("enqueue tail");
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(50)).await;
        release.notify_one();
        tokio::task::yield_now().await;
        assert_eq!(*batches.lock().expect("batches"), vec![vec![0]]);
        tokio::time::advance(Duration::from_millis(149)).await;
        tokio::task::yield_now().await;
        assert_eq!(*batches.lock().expect("batches"), vec![vec![0]]);
        tokio::time::advance(Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(*batches.lock().expect("batches"), vec![vec![0], vec![1]]);
    }

    #[tokio::test(start_paused = true)]
    async fn automatic_failure_pauses_retries_and_preserves_new_work_order() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let reports = Arc::new(AtomicUsize::new(0));
        let batches = Arc::new(Mutex::new(Vec::<Vec<u64>>::new()));
        let writes = SessionWriteBehind::new(
            Duration::from_millis(200),
            {
                let attempts = attempts.clone();
                let batches = batches.clone();
                move |events| {
                    let attempts = attempts.clone();
                    let batches = batches.clone();
                    async move {
                        batches
                            .lock()
                            .expect("batches")
                            .push(events.iter().map(|event| event.seq).collect());
                        if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                            anyhow::bail!("storage unavailable");
                        }
                        Ok(())
                    }
                }
            },
            {
                let reports = reports.clone();
                move |_| {
                    reports.fetch_add(1, Ordering::SeqCst);
                }
            },
        );
        writes.enqueue(&event(0)).expect("enqueue first");
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(200)).await;
        tokio::task::yield_now().await;
        assert_eq!(reports.load(Ordering::SeqCst), 1);
        tokio::time::advance(Duration::from_millis(1_000)).await;
        tokio::task::yield_now().await;
        assert_eq!(*batches.lock().expect("batches"), vec![vec![0]]);
        writes.enqueue(&event(1)).expect("enqueue second");
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(200)).await;
        tokio::task::yield_now().await;
        assert_eq!(*batches.lock().expect("batches"), vec![vec![0], vec![0, 1]]);
        writes.flush().await.expect("flush");
    }

    #[tokio::test]
    async fn explicit_barrier_failure_is_not_reported_and_retains_large_batch() {
        const BATCH_SIZE: u64 = 150_000;
        let attempts = Arc::new(AtomicUsize::new(0));
        let reports = Arc::new(AtomicUsize::new(0));
        let sizes = Arc::new(Mutex::new(Vec::<usize>::new()));
        let writes = SessionWriteBehind::new(
            Duration::from_secs(60),
            {
                let attempts = attempts.clone();
                let sizes = sizes.clone();
                move |events| {
                    let attempts = attempts.clone();
                    let sizes = sizes.clone();
                    async move {
                        sizes.lock().expect("sizes").push(events.len());
                        if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                            anyhow::bail!("durability failed");
                        }
                        Ok(())
                    }
                }
            },
            {
                let reports = reports.clone();
                move |_| {
                    reports.fetch_add(1, Ordering::SeqCst);
                }
            },
        );
        for sequence in 0..BATCH_SIZE {
            writes.enqueue(&event(sequence)).expect("enqueue");
        }
        assert!(
            writes
                .flush()
                .await
                .expect_err("first barrier")
                .to_string()
                .contains("durability failed")
        );
        assert_eq!(reports.load(Ordering::SeqCst), 0);
        assert!(writes.has_work());
        writes.flush().await.expect("retry barrier");
        let expected_size = usize::try_from(BATCH_SIZE).expect("bounded batch size");
        assert_eq!(
            *sizes.lock().expect("sizes"),
            vec![expected_size, expected_size]
        );
        assert!(!writes.has_work());
    }
}
