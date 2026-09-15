//! Portable parity coverage for per-session composer blocks.

#![cfg(not(target_arch = "wasm32"))]

use std::{cell::Cell, rc::Rc};

use seekdeep_client_ui_conversation::{ComposerBlock, ComposerBlockRegistry};
use seekdeep_identity::SessionId;

#[test]
fn stores_are_identity_stable_isolated_idempotent_and_reborn_after_forget() {
    let registry = ComposerBlockRegistry::new();
    let one = SessionId::new("s1");
    let two = SessionId::new("s2");
    registry.set(
        one.clone(),
        Some(ComposerBlock {
            reason: "choose model".to_owned(),
        }),
    );
    let store = registry.store_for(one.clone());
    assert_eq!(store.snapshot().unwrap().reason, "choose model");
    assert!(Rc::ptr_eq(&store, &registry.store_for(one.clone())));
    assert!(!Rc::ptr_eq(&store, &registry.store_for(two.clone())));

    let notifications = Rc::new(Cell::new(0));
    let counted = notifications.clone();
    let subscription = store.subscribe(Rc::new(move || counted.set(counted.get() + 1)));
    registry.set(
        one.clone(),
        Some(ComposerBlock {
            reason: "choose model".to_owned(),
        }),
    );
    assert_eq!(notifications.get(), 0);
    registry.set(
        one.clone(),
        Some(ComposerBlock {
            reason: "connect workspace".to_owned(),
        }),
    );
    assert_eq!(notifications.get(), 1);
    store.update(|block| {
        block.as_mut().unwrap().reason = "updated directly".to_owned();
    });
    assert_eq!(notifications.get(), 2);
    assert_eq!(store.snapshot().unwrap().reason, "updated directly");
    registry.set(one.clone(), None);
    registry.set(one.clone(), None);
    assert_eq!(notifications.get(), 3);
    drop(subscription);
    registry.set(
        one.clone(),
        Some(ComposerBlock {
            reason: "old handle".to_owned(),
        }),
    );
    assert_eq!(notifications.get(), 3);

    registry.forget(&one);
    let reborn = registry.store_for(one);
    assert!(!Rc::ptr_eq(&store, &reborn));
    assert_eq!(store.snapshot().unwrap().reason, "old handle");
    assert_eq!(reborn.snapshot(), None);
    assert_eq!(registry.len(), 2);
}

#[test]
fn clearing_an_absent_session_still_creates_its_store_without_notification() {
    let registry = ComposerBlockRegistry::new();
    let session = SessionId::new("s1");
    registry.set(session.clone(), None);
    assert_eq!(registry.len(), 1);
    assert_eq!(registry.store_for(session).snapshot(), None);
}
