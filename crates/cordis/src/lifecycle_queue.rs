//! Registry-owned deferral of source-turn lifecycle work.

use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, Waker},
};

use parking_lot::Mutex;

/// Owned native lifecycle task handed to the selected executor.
pub type LifecycleTask = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Scheduling seam for lifecycle continuations after their source-ordered first poll.
pub trait LifecycleScheduler: Send + Sync {
    /// Admits a task whose completion remains owned by its fiber.
    fn schedule(&self, task: LifecycleTask);
}

struct BackgroundScheduler;

impl LifecycleScheduler for BackgroundScheduler {
    fn schedule(&self, task: LifecycleTask) {
        super::spawn_background(task);
    }
}

#[derive(Default)]
struct QueueState {
    depth: usize,
    flushing: bool,
    tasks: VecDeque<LifecycleTask>,
    waiters: Vec<Waker>,
}

pub(super) struct LifecycleQueue {
    state: Mutex<QueueState>,
    scheduler: Mutex<Arc<dyn LifecycleScheduler>>,
}

impl Default for LifecycleQueue {
    fn default() -> Self {
        Self {
            state: Mutex::default(),
            scheduler: Mutex::new(Arc::new(BackgroundScheduler)),
        }
    }
}

impl LifecycleQueue {
    pub(super) fn set_scheduler(&self, scheduler: Arc<dyn LifecycleScheduler>) {
        *self.scheduler.lock() = scheduler;
    }

    pub(super) fn defer(self: &Arc<Self>) -> LifecycleDeferral {
        self.state.lock().depth += 1;
        LifecycleDeferral {
            queue: self.clone(),
        }
    }

    pub(super) fn dispatch(&self, task: LifecycleTask) {
        let deferred = {
            let state = self.state.lock();
            state.depth > 0 || state.flushing
        };
        if deferred {
            let mut task = task;
            let mut context = Context::from_waker(Waker::noop());
            if task.as_mut().poll(&mut context).is_pending() {
                self.state.lock().tasks.push_back(task);
            }
            return;
        }
        let scheduler = self.scheduler.lock().clone();
        scheduler.schedule(task);
    }

    pub(super) async fn after_turn(&self) {
        let mut admitted = false;
        std::future::poll_fn(|context| {
            let mut state = self.state.lock();
            if state.depth > 0 || state.flushing && !admitted {
                admitted = true;
                if !context.waker().will_wake(Waker::noop())
                    && !state
                        .waiters
                        .iter()
                        .any(|waker| waker.will_wake(context.waker()))
                {
                    state.waiters.push(context.waker().clone());
                }
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })
        .await;
    }

    fn release(&self) {
        {
            let mut state = self.state.lock();
            state.depth -= 1;
            if state.depth > 0 || state.flushing {
                return;
            }
            state.flushing = true;
        }
        let mut pending = Vec::new();
        loop {
            let task = {
                let mut state = self.state.lock();
                if let Some(task) = state.tasks.pop_front() {
                    Some(task)
                } else {
                    state.flushing = false;
                    None
                }
            };
            let Some(mut task) = task else {
                break;
            };
            let mut context = Context::from_waker(Waker::noop());
            if task.as_mut().poll(&mut context) == Poll::Pending {
                pending.push(task);
            }
        }
        let scheduler = self.scheduler.lock().clone();
        for task in pending {
            scheduler.schedule(task);
        }
        let waiters = std::mem::take(&mut self.state.lock().waiters);
        for waker in waiters {
            waker.wake();
        }
    }
}

/// Keeps newly admitted lifecycle tasks behind one synchronous source turn.
///
/// Releasing the last nested guard starts each queued task in insertion order.
/// Pending continuations are handed to the registry's selected scheduler after
/// every queued task has received its first poll.
pub struct LifecycleDeferral {
    queue: Arc<LifecycleQueue>,
}

impl Drop for LifecycleDeferral {
    fn drop(&mut self) {
        self.queue.release();
    }
}
