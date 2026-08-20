//! Local Boa job queue that sleeps while native bindings await.

use std::{
    cell::RefCell,
    collections::VecDeque,
    rc::Rc,
    time::{Duration, Instant},
};

use boa_engine::{
    Context, JsResult,
    job::{GenericJob, Job, JobExecutor, NativeAsyncJob, PromiseJob, TimeoutJob},
};
use futures::{StreamExt as _, stream::FuturesUnordered};

/// FIFO ECMAScript job executor whose async queue waits on wakeups instead of
/// repeatedly polling pending host futures.
#[derive(Debug, Default)]
pub(crate) struct WorkflowJobExecutor {
    promise_queue: RefCell<VecDeque<PromiseJob>>,
    async_queue: RefCell<VecDeque<NativeAsyncJob>>,
    timeouts: RefCell<Vec<(Instant, TimeoutJob)>>,
    generic_queue: RefCell<VecDeque<GenericJob>>,
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
    }

    fn run_jobs(self: Rc<Self>, context: &mut Context) -> JsResult<()> {
        futures::executor::block_on(self.run_jobs_async(&RefCell::new(context)))
    }

    async fn run_jobs_async(self: Rc<Self>, context: &RefCell<&mut Context>) -> JsResult<()> {
        let mut active = FuturesUnordered::new();
        loop {
            tokio::task::yield_now().await;
            let async_jobs = std::mem::take(&mut *self.async_queue.borrow_mut());
            for job in async_jobs {
                active.push(job.call(context));
            }
            if let Err(error) = self.run_sync_jobs(context) {
                self.clear();
                return Err(error);
            }
            if self.has_immediate_jobs() {
                continue;
            }
            let Some(result) = active.next().await else {
                return Ok(());
            };
            if let Err(error) = result {
                self.clear();
                return Err(error);
            }
        }
    }
}
