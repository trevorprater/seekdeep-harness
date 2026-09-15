//! Snapshot freshness and microtask/frame notification batching.

use std::{
    cell::RefCell,
    collections::BTreeMap,
    rc::{Rc, Weak},
};

use crate::RuntimeDisposer;

/// Injected browser notification scheduler.
pub trait NotifierScheduler {
    /// Whether an animation-frame queue exists.
    fn has_animation_frame(&self) -> bool;
    /// Queues one microtask.
    fn queue_microtask(&self, callback: Box<dyn FnOnce()>);
    /// Queues one animation-frame callback.
    fn queue_animation_frame(&self, callback: Box<dyn FnOnce()>);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScheduleKind {
    None,
    Microtask,
    Frame,
}

struct NotifierState {
    listeners: BTreeMap<u64, Rc<dyn Fn()>>,
    next_listener: u64,
    dirty: bool,
    notify_pending: bool,
    scheduled: ScheduleKind,
    generation: u64,
}

/// Shared snapshot rebuild and notification primitive.
pub struct Notifier {
    rebuild: Rc<dyn Fn()>,
    scheduler: Rc<dyn NotifierScheduler>,
    state: RefCell<NotifierState>,
}

impl Notifier {
    /// Creates a clean notifier over one snapshot-cache rebuild callback.
    #[must_use]
    pub fn new(rebuild: Rc<dyn Fn()>, scheduler: Rc<dyn NotifierScheduler>) -> Rc<Self> {
        Rc::new(Self {
            rebuild,
            scheduler,
            state: RefCell::new(NotifierState {
                listeners: BTreeMap::new(),
                next_listener: 0,
                dirty: false,
                notify_pending: false,
                scheduled: ScheduleKind::None,
                generation: 0,
            }),
        })
    }

    /// Subscribes to committed snapshot changes.
    #[must_use]
    pub fn subscribe(self: &Rc<Self>, listener: Rc<dyn Fn()>) -> RuntimeDisposer {
        let id = {
            let mut state = self.state.borrow_mut();
            state.next_listener = state.next_listener.wrapping_add(1);
            let id = state.next_listener;
            state.listeners.insert(id, listener);
            id
        };
        let weak = Rc::downgrade(self);
        RuntimeDisposer::new(move || {
            if let Some(notifier) = weak.upgrade() {
                notifier.state.borrow_mut().listeners.remove(&id);
            }
        })
    }

    /// Marks a structural change and schedules a microtask publication.
    pub fn mark_dirty(self: &Rc<Self>) {
        {
            let mut state = self.state.borrow_mut();
            state.dirty = true;
            state.notify_pending = true;
            if state.scheduled == ScheduleKind::Microtask {
                return;
            }
        }
        self.schedule(ScheduleKind::Microtask);
    }

    /// Marks a streaming change and publishes at most once per frame.
    pub fn mark_frame_dirty(self: &Rc<Self>) {
        {
            let mut state = self.state.borrow_mut();
            state.dirty = true;
            state.notify_pending = true;
            if state.scheduled != ScheduleKind::None {
                return;
            }
        }
        self.schedule(if self.scheduler.has_animation_frame() {
            ScheduleKind::Frame
        } else {
            ScheduleKind::Microtask
        });
    }

    /// Rebuilds and notifies synchronously, invalidating any older scheduled callback.
    pub fn notify_now(self: &Rc<Self>) {
        {
            let mut state = self.state.borrow_mut();
            state.dirty = true;
            state.notify_pending = true;
            state.generation = state.generation.wrapping_add(1);
            state.scheduled = ScheduleKind::None;
        }
        self.flush();
    }

    /// Rebuilds dirty snapshot state on the read path without consuming pending notification.
    pub fn ensure_fresh(&self) {
        let rebuild = {
            let mut state = self.state.borrow_mut();
            if state.dirty {
                state.dirty = false;
                true
            } else {
                false
            }
        };
        if rebuild {
            (self.rebuild)();
        }
    }

    fn schedule(self: &Rc<Self>, kind: ScheduleKind) {
        let generation = {
            let mut state = self.state.borrow_mut();
            state.generation = state.generation.wrapping_add(1);
            state.scheduled = kind;
            state.generation
        };
        let weak = Rc::downgrade(self);
        let publish = Box::new(move || publish(&weak, generation));
        match kind {
            ScheduleKind::Microtask => self.scheduler.queue_microtask(publish),
            ScheduleKind::Frame => self.scheduler.queue_animation_frame(publish),
            ScheduleKind::None => unreachable!("none is not scheduled"),
        }
    }

    fn flush(&self) {
        let (rebuild, listeners) = {
            let mut state = self.state.borrow_mut();
            if !state.notify_pending || state.listeners.is_empty() {
                return;
            }
            state.notify_pending = false;
            let rebuild = state.dirty;
            state.dirty = false;
            let listeners = state.listeners.values().cloned().collect::<Vec<_>>();
            (rebuild, listeners)
        };
        if rebuild {
            (self.rebuild)();
        }
        for listener in listeners {
            listener();
        }
    }
}

fn publish(notifier: &Weak<Notifier>, generation: u64) {
    let Some(notifier) = notifier.upgrade() else {
        return;
    };
    {
        let mut state = notifier.state.borrow_mut();
        if generation != state.generation {
            return;
        }
        state.scheduled = ScheduleKind::None;
    }
    notifier.flush();
}
