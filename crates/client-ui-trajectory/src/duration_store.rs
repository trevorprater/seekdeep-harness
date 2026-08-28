//! Browser-wide persisted trajectory duration preference.

use std::rc::Rc;

use seekdeep_client_runtime::{
    SnapshotStore, StoreFlushMode, StoreFlushScheduler, StoreLogger, StorePersistenceFactory,
};

use crate::{DEFAULT_ACTUAL_DURATION, DURATION_PERSISTENCE_KEY};

/// Creates one plugin-lifecycle duration preference source over injected runtime seams.
#[must_use]
pub fn create_trajectory_duration_store(
    scheduler: Rc<dyn StoreFlushScheduler>,
    persistence: Option<StorePersistenceFactory<bool>>,
    logger: StoreLogger,
) -> Rc<SnapshotStore<bool>> {
    let persistence = persistence.map(|factory| {
        (
            DURATION_PERSISTENCE_KEY.to_owned(),
            factory(DURATION_PERSISTENCE_KEY),
        )
    });
    SnapshotStore::new(
        DEFAULT_ACTUAL_DURATION,
        StoreFlushMode::Sync,
        scheduler,
        persistence,
        logger,
    )
}
