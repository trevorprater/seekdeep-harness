//! Single-consumption queues; closing a subscription does not close its retained queue.

use std::{collections::VecDeque, time::Duration};

use parking_lot::{Condvar, Mutex};

pub(crate) struct Queue<T> {
    items: Mutex<VecDeque<T>>,
    ready: Condvar,
}

impl<T> Default for Queue<T> {
    fn default() -> Self {
        Self {
            items: Mutex::new(VecDeque::new()),
            ready: Condvar::new(),
        }
    }
}

impl<T> Queue<T> {
    pub(crate) fn push(&self, value: T) {
        self.items.lock().push_back(value);
        self.ready.notify_one();
    }

    pub(crate) fn pop(&self, timeout: Option<Duration>) -> Option<T> {
        let mut items = self.items.lock();
        if let Some(value) = items.pop_front() {
            return Some(value);
        }
        if let Some(timeout) = timeout {
            self.ready.wait_for(&mut items, timeout);
            return items.pop_front();
        }
        loop {
            self.ready.wait(&mut items);
            if let Some(value) = items.pop_front() {
                return Some(value);
            }
        }
    }

    pub(crate) fn try_pop(&self) -> Option<T> {
        self.items.lock().pop_front()
    }

    pub(crate) fn len(&self) -> usize {
        self.items.lock().len()
    }
}
