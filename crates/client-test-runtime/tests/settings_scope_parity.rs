//! Behavioral mirror of the `stubbed settings scope` source test group.

#![cfg(not(target_arch = "wasm32"))]

use std::{cell::Cell, rc::Rc};

use seekdeep_client_settings_contract::{ClientSettingsMode, ClientSettingsStatus};
use seekdeep_client_test_runtime::{SettingsScopePatch, StubSettingsScope};
use serde_json::json;

#[test]
fn starts_loading_records_writes_and_publishes_partial_acceptances() {
    let stub = StubSettingsScope::<String>::new();
    let scope = stub.scope();
    let initial = scope.snapshot();
    assert_eq!(initial.status, ClientSettingsStatus::Loading);
    assert!(initial.value.is_none());
    assert!(initial.base.is_none());
    assert!(initial.user.is_none());
    assert_eq!(initial.revision, None);
    assert!(!initial.writable);
    assert_eq!(initial.mode, ClientSettingsMode::Host);

    futures::executor::block_on(scope.set("preference".to_owned(), json!("dark"))).unwrap();
    futures::executor::block_on(scope.unset("preference".to_owned())).unwrap();
    assert_eq!(stub.set_calls(), [("preference".to_owned(), json!("dark"))]);
    assert_eq!(stub.unset_calls(), ["preference"]);

    let calls = Rc::new(Cell::new(0_usize));
    let observed = calls.clone();
    let disposer = scope.subscribe(Rc::new(move || observed.set(observed.get() + 1)));
    assert_eq!(stub.listener_count(), 1);
    stub.publish(SettingsScopePatch {
        status: Some(ClientSettingsStatus::Ready),
        value: Some(Some(Rc::new("dark".to_owned()))),
        revision: Some(Some(3.0)),
        writable: Some(true),
        ..SettingsScopePatch::default()
    });
    let accepted = scope.snapshot();
    assert!(!Rc::ptr_eq(&initial, &accepted));
    assert_eq!(accepted.status, ClientSettingsStatus::Ready);
    assert_eq!(accepted.value.as_deref().map(String::as_str), Some("dark"));
    assert_eq!(accepted.revision, Some(3.0));
    assert!(accepted.writable);
    assert_eq!(accepted.mode, ClientSettingsMode::Host);
    assert_eq!(calls.get(), 1);

    disposer.dispose();
    disposer.dispose();
    assert_eq!(stub.listener_count(), 0);
    stub.publish(SettingsScopePatch {
        value: Some(None),
        ..SettingsScopePatch::default()
    });
    assert!(scope.snapshot().value.is_none());
    assert_eq!(calls.get(), 1);
}

#[test]
fn duplicate_listener_identity_is_registered_once() {
    let stub = StubSettingsScope::<String>::new();
    let scope = stub.scope();
    let calls = Rc::new(Cell::new(0_usize));
    let observed = calls.clone();
    let listener: Rc<dyn Fn()> = Rc::new(move || observed.set(observed.get() + 1));
    let first = scope.subscribe(listener.clone());
    let _second = scope.subscribe(listener);
    assert_eq!(stub.listener_count(), 1);
    stub.publish(SettingsScopePatch::default());
    assert_eq!(calls.get(), 1);
    first.dispose();
    assert_eq!(stub.listener_count(), 0);
}
