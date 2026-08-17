//! Narrow native bridge for synchronous cleanup during ordinary process exit.
//!
//! Rust's standard library exposes no `atexit` registration API. This crate is
//! intentionally isolated so the one required platform call and its safety
//! proof do not weaken the rest of the workspace's `unsafe_code = "forbid"`
//! boundary.

use std::sync::{Arc, OnceLock, Weak};

use parking_lot::Mutex;
use uuid::Uuid;

/// Synchronous, best-effort finalization safe to invoke during host exit.
pub trait ProcessExitTarget: Send + Sync + 'static {
    /// Performs bounded, non-panicking final termination.
    fn terminate_for_process_exit(&self);
}

type Target = dyn ProcessExitTarget;
type TargetRegistry = Mutex<Vec<(Uuid, Weak<Target>)>>;

static TARGETS: OnceLock<TargetRegistry> = OnceLock::new();
static INSTALLED: OnceLock<anyhow::Result<(), String>> = OnceLock::new();

fn targets() -> &'static TargetRegistry {
    TARGETS.get_or_init(|| Mutex::new(Vec::new()))
}

extern "C" fn dispatch_process_exit() {
    let live = targets()
        .lock()
        .iter()
        .filter_map(|(_, target)| target.upgrade())
        .collect::<Vec<_>>();
    for target in live {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            target.terminate_for_process_exit();
        }));
    }
}

fn install_once() -> anyhow::Result<()> {
    let installed = INSTALLED.get_or_init(|| {
        // SAFETY: `dispatch_process_exit` has C ABI, no captured state, and a
        // static lifetime. Registration happens once. Its body contains every
        // target failure and only accesses a process-lifetime static registry.
        let result = unsafe { libc::atexit(dispatch_process_exit) };
        if result == 0 {
            Ok(())
        } else {
            Err("failed to register native process-exit hook".to_owned())
        }
    });
    installed
        .as_ref()
        .map_err(|message| anyhow::anyhow!(message.clone()))
        .copied()
}

/// Reversible registration in the shared native process-exit dispatcher.
#[derive(Debug)]
pub struct ProcessExitRegistration {
    id: Uuid,
    active: bool,
}

impl ProcessExitRegistration {
    /// Removes this target. Repeated calls are inert.
    pub fn unregister(&mut self) {
        if !self.active {
            return;
        }
        targets().lock().retain(|(id, _)| *id != self.id);
        self.active = false;
    }
}

impl Drop for ProcessExitRegistration {
    fn drop(&mut self) {
        self.unregister();
    }
}

/// Registers one weakly held synchronous process-exit target.
///
/// # Errors
///
/// Returns the platform registration failure without retaining the target.
pub fn register<T>(target: &Arc<T>) -> anyhow::Result<ProcessExitRegistration>
where
    T: ProcessExitTarget,
{
    install_once()?;
    let id = Uuid::now_v7();
    let target: Arc<Target> = target.clone();
    targets().lock().push((id, Arc::downgrade(&target)));
    Ok(ProcessExitRegistration { id, active: true })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[derive(Default)]
    struct Counter(AtomicUsize);

    impl ProcessExitTarget for Counter {
        fn terminate_for_process_exit(&self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn registration_is_weak_and_reversible() {
        let counter = Arc::new(Counter::default());
        let mut registration = register(&counter).unwrap();
        assert!(
            targets()
                .lock()
                .iter()
                .any(|(id, _)| *id == registration.id)
        );
        registration.unregister();
        assert!(
            !targets()
                .lock()
                .iter()
                .any(|(id, _)| *id == registration.id)
        );
    }
}
