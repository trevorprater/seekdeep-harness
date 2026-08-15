//! Ownership of one unpublished session before registry publication.

use std::sync::Arc;

use crate::session::Session;

/// One unpublished session plus provider state that keeps it usable.
pub struct SessionPreparation {
    session: Arc<Session>,
    release: Option<Box<dyn FnOnce() + Send + 'static>>,
}

impl std::fmt::Debug for SessionPreparation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionPreparation")
            .field("session", &self.session)
            .field("released", &self.release.is_none())
            .finish()
    }
}

impl SessionPreparation {
    /// Wraps an exact unpublished session and optional release callback.
    #[must_use]
    pub fn new(session: Arc<Session>, release: impl FnOnce() + Send + 'static) -> Self {
        Self {
            session,
            release: Some(Box::new(release)),
        }
    }

    /// Wraps an unpublished session with no provider-owned state.
    #[must_use]
    pub fn without_release(session: Arc<Session>) -> Self {
        Self {
            session,
            release: None,
        }
    }

    /// Returns the exact prepared session.
    #[must_use]
    pub fn session(&self) -> &Arc<Session> {
        &self.session
    }

    /// Releases provider state once.
    pub fn release(&mut self) {
        if let Some(release) = self.release.take() {
            release();
        }
    }
}

impl Drop for SessionPreparation {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::session::SessionId;

    #[test]
    fn release_is_synchronous_and_idempotent() {
        let session = Session::create(&SessionId::new("prepared"), None, None).expect("session");
        let count = Arc::new(AtomicUsize::new(0));
        let release_count = count.clone();
        let mut preparation = SessionPreparation::new(session, move || {
            release_count.fetch_add(1, Ordering::SeqCst);
        });
        preparation.release();
        preparation.release();
        drop(preparation);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
}
