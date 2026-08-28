//! Persisted browser-wide duration preference parity.

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use seekdeep_client_runtime::{StoreFlushScheduler, StorePersistence, StorePersistenceFactory};
use seekdeep_client_ui_trajectory::{DURATION_PERSISTENCE_KEY, create_trajectory_duration_store};

struct SyncScheduler;

impl StoreFlushScheduler for SyncScheduler {
    fn queue(&self, callback: Box<dyn FnOnce()>) {
        callback();
    }
}

#[derive(Clone)]
struct MemoryPersistence {
    name: String,
    values: Rc<RefCell<HashMap<String, bool>>>,
}

impl StorePersistence<bool> for MemoryPersistence {
    fn read(&self) -> Result<Option<bool>, String> {
        Ok(self.values.borrow().get(&self.name).copied())
    }

    fn write(&self, value: &bool) -> Result<(), String> {
        self.values.borrow_mut().insert(self.name.clone(), *value);
        Ok(())
    }

    fn remove(&self) -> Result<(), String> {
        self.values.borrow_mut().remove(&self.name);
        Ok(())
    }
}

#[test]
fn root_preference_defaults_false_persists_exact_key_and_rehydrates() {
    let values = Rc::new(RefCell::new(HashMap::new()));
    let observed_names = Rc::new(RefCell::new(Vec::new()));
    let factory_values = values.clone();
    let factory_names = observed_names.clone();
    let factory: StorePersistenceFactory<bool> = Rc::new(move |name| {
        factory_names.borrow_mut().push(name.to_owned());
        Rc::new(MemoryPersistence {
            name: name.to_owned(),
            values: factory_values.clone(),
        })
    });
    let first = create_trajectory_duration_store(
        Rc::new(SyncScheduler),
        Some(factory.clone()),
        Rc::new(|_| {}),
    );
    assert!(!*first.snapshot());
    first.set(true);
    assert_eq!(values.borrow().get(DURATION_PERSISTENCE_KEY), Some(&true));

    let revived =
        create_trajectory_duration_store(Rc::new(SyncScheduler), Some(factory), Rc::new(|_| {}));
    assert!(*revived.snapshot());
    assert_eq!(
        observed_names.borrow().as_slice(),
        [DURATION_PERSISTENCE_KEY, DURATION_PERSISTENCE_KEY]
    );
    assert!(
        observed_names
            .borrow()
            .iter()
            .all(|name| !name.ends_with(".session"))
    );
}
