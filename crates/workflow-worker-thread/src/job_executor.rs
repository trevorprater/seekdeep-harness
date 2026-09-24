//! Local Boa job queue that sleeps while native bindings await.

use std::{
    cell::RefCell,
    collections::VecDeque,
    rc::Rc,
    time::{Duration, Instant},
};

use boa_engine::{
    Context, JsResult,
    builtins::promise::PromiseState,
    job::{GenericJob, Job, JobExecutor, NativeAsyncJob, PromiseJob, TimeoutJob},
    object::builtins::JsPromise,
};
use futures::{FutureExt as _, StreamExt as _, stream::FuturesUnordered};
use seekdeep_llm::AbortSignal;

/// FIFO ECMAScript job executor whose async queue waits on wakeups instead of
/// repeatedly polling pending host futures.
#[derive(Debug, Default)]
pub(crate) struct WorkflowJobExecutor {
    promise_queue: RefCell<VecDeque<PromiseJob>>,
    async_queue: RefCell<VecDeque<NativeAsyncJob>>,
    timeouts: RefCell<Vec<(Instant, TimeoutJob)>>,
    generic_queue: RefCell<VecDeque<GenericJob>>,
    wake: tokio::sync::Notify,
}

impl WorkflowJobExecutor {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn clear(&self) {
        self.promise_queue.borrow_mut().clear();
        self.async_queue.borrow_mut().clear();
        self.timeouts.borrow_mut().clear();
        self.generic_queue.borrow_mut().clear();
    }

    fn run_sync_jobs(&self, context: &RefCell<&mut Context>) -> JsResult<()> {
        let now = Instant::now();
        let mut waiting = Vec::new();
        let mut due = Vec::new();
        let timeout_jobs = std::mem::take(&mut *self.timeouts.borrow_mut());
        for (deadline, job) in timeout_jobs {
            if deadline <= now {
                due.push(job);
            } else if !job.is_cancelled() {
                waiting.push((deadline, job));
            }
        }
        *self.timeouts.borrow_mut() = waiting;
        for job in due {
            if !job.is_cancelled() {
                job.call(&mut context.borrow_mut())?;
            }
        }
        let promise_jobs = std::mem::take(&mut *self.promise_queue.borrow_mut());
        for job in promise_jobs {
            job.call(&mut context.borrow_mut())?;
        }
        let generic_jobs = std::mem::take(&mut *self.generic_queue.borrow_mut());
        for job in generic_jobs {
            job.call(&mut context.borrow_mut())?;
        }
        context.borrow_mut().clear_kept_objects();
        Ok(())
    }

    fn has_immediate_jobs(&self) -> bool {
        !self.promise_queue.borrow().is_empty()
            || !self.async_queue.borrow().is_empty()
            || !self.generic_queue.borrow().is_empty()
            || self
                .timeouts
                .borrow()
                .iter()
                .any(|(deadline, _)| *deadline <= Instant::now())
    }

    fn next_timeout_delay(&self) -> Option<Duration> {
        let now = Instant::now();
        self.timeouts
            .borrow()
            .iter()
            .map(|(deadline, _)| deadline.saturating_duration_since(now))
            .min()
    }

    fn root_settled(root: Option<&JsPromise>) -> bool {
        root.is_some_and(|root| !matches!(root.state(), PromiseState::Pending))
    }

    pub(crate) async fn run_jobs_until(
        self: Rc<Self>,
        context: &RefCell<&mut Context>,
        root: &JsPromise,
        cancel: &AbortSignal,
    ) -> JsResult<()> {
        self.run_jobs_async(context, Some(root), Some(cancel)).await
    }

    #[allow(clippy::too_many_lines)] // exhaustive root/active/timer/cancellation wait matrix
    async fn run_jobs_async(
        self: Rc<Self>,
        context: &RefCell<&mut Context>,
        root: Option<&JsPromise>,
        cancel: Option<&AbortSignal>,
    ) -> JsResult<()> {
        let mut active = FuturesUnordered::new();
        loop {
            tokio::task::yield_now().await;
            let async_jobs = std::mem::take(&mut *self.async_queue.borrow_mut());
            for job in async_jobs {
                active.push(job.call(context));
            }
            loop {
                match active.next().now_or_never() {
                    Some(Some(Ok(_))) => {}
                    Some(Some(Err(error))) => {
                        self.clear();
                        return Err(error);
                    }
                    Some(None) | None => break,
                }
            }
            if let Err(error) = self.run_sync_jobs(context) {
                self.clear();
                return Err(error);
            }
            if Self::root_settled(root) {
                self.clear();
                return Ok(());
            }
            if active.is_empty() && cancel.is_some_and(AbortSignal::is_aborted) {
                self.clear();
                return Ok(());
            }
            if self.has_immediate_jobs() {
                continue;
            }
            let timeout = self.next_timeout_delay();
            let cancellation = cancel.filter(|signal| !signal.is_aborted());
            match (active.is_empty(), timeout, cancellation) {
                (true, None, None) => return Ok(()),
                (true, None, Some(cancel)) => {
                    tokio::select! {
                        () = cancel.cancelled() => {}
                        () = self.wake.notified() => {}
                    }
                }
                (true, Some(delay), None) => {
                    tokio::select! {
                        () = tokio::time::sleep(delay) => {}
                        () = self.wake.notified() => {}
                    }
                }
                (true, Some(delay), Some(cancel)) => {
                    tokio::select! {
                        () = cancel.cancelled() => {}
                        () = tokio::time::sleep(delay) => {}
                        () = self.wake.notified() => {}
                    }
                }
                (false, None, None) => {
                    tokio::select! {
                        result = active.next() => {
                            if let Some(Err(error)) = result {
                                self.clear();
                                return Err(error);
                            }
                        }
                        () = self.wake.notified() => {}
                    }
                }
                (false, None, Some(cancel)) => {
                    tokio::select! {
                        () = cancel.cancelled() => {}
                        result = active.next() => {
                            if let Some(Err(error)) = result {
                                self.clear();
                                return Err(error);
                            }
                        }
                        () = self.wake.notified() => {}
                    }
                }
                (false, Some(delay), None) => {
                    tokio::select! {
                        result = active.next() => {
                            if let Some(Err(error)) = result {
                                self.clear();
                                return Err(error);
                            }
                        }
                        () = tokio::time::sleep(delay) => {}
                        () = self.wake.notified() => {}
                    }
                }
                (false, Some(delay), Some(cancel)) => {
                    tokio::select! {
                        () = cancel.cancelled() => {}
                        result = active.next() => {
                            if let Some(Err(error)) = result {
                                self.clear();
                                return Err(error);
                            }
                        }
                        () = tokio::time::sleep(delay) => {}
                        () = self.wake.notified() => {}
                    }
                }
            }
        }
    }
}

impl JobExecutor for WorkflowJobExecutor {
    fn enqueue_job(self: Rc<Self>, job: Job, _context: &mut Context) {
        match job {
            Job::PromiseJob(job) => self.promise_queue.borrow_mut().push_back(job),
            Job::AsyncJob(job) => self.async_queue.borrow_mut().push_back(job),
            Job::TimeoutJob(job) => {
                let deadline = Instant::now() + Duration::from_millis(job.timeout().as_millis());
                self.timeouts.borrow_mut().push((deadline, job));
            }
            Job::GenericJob(job) => self.generic_queue.borrow_mut().push_back(job),
            _ => unreachable!("unknown Boa job kind"),
        }
        self.wake.notify_one();
    }

    fn run_jobs(self: Rc<Self>, context: &mut Context) -> JsResult<()> {
        futures::executor::block_on(self.run_jobs_async(&RefCell::new(context), None, None))
    }
}
