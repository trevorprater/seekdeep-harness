//! Client Context allowlist, Slot priority/key, and Theme ownership parity.

use seekdeep_cordis_client_runner::*;
use seekdeep_cordis_dynamic_types::{
    CordisDynamicPackageId, CordisDynamicPluginId, CordisDynamicPluginRunId, DynamicCordisPackage,
};
use serde_json::{Value, json};

fn package() -> DynamicCordisPackage {
    DynamicCordisPackage {
        plugin_id: CordisDynamicPluginId::new("panel-1"),
        package_id: CordisDynamicPackageId::new("pkg-2"),
        plugin_run_id: CordisDynamicPluginRunId::new("run-3"),
        name: "panel".to_owned(),
    }
}

#[test]
fn context_reads_has_and_writes_use_one_declared_service_allowlist() {
    let guard = ClientContextGuard::new(["slots".to_owned(), "timer".to_owned()]);
    assert_eq!(guard.read("get", false).unwrap(), ClientContextAccess::Get);
    assert_eq!(guard.read("on", false).unwrap(), ClientContextAccess::Verb);
    assert_eq!(
        guard.read("setTimeout", false).unwrap(),
        ClientContextAccess::Verb
    );
    assert_eq!(
        guard.read("slots", true).unwrap(),
        ClientContextAccess::Service
    );
    assert!(guard.contains("get"));
    assert!(guard.contains("on"));
    assert!(guard.contains("setTimeout"));
    assert!(guard.contains("slots"));
    assert!(!guard.contains("root"));

    let undeclared = ClientContextGuard::new(Vec::new());
    let service = undeclared.read("slots", true).unwrap_err();
    assert!(service.contains("is not declared by your plugin"));
    assert!(service.contains("plain `function` has no declaration site"));
    let internal = undeclared.read("root", false).unwrap_err();
    assert!(internal.contains("Framework internals are withheld"));
    assert_eq!(
        undeclared.read("timeout", false).unwrap(),
        ClientContextAccess::Verb
    );
    let timer = undeclared.invoke_verb("timeout", true).unwrap_err();
    assert!(timer.contains("service \"timer\" is not declared"));
    assert_eq!(
        ClientContextGuard::assignment_failure("stash"),
        "dynamic ctx is read-only; cannot assign \"stash\""
    );
    assert!(
        ClientContextGuard::context_return_failure("theme").contains("returned a cordis Context")
    );
}

#[test]
fn slot_registration_assigns_descending_priority_preserves_chain_and_binds_self_key() {
    let priorities = ClientPriorityAllocator::default();
    let first = normalize_slot_registration(
        &package(),
        &json!({"name": "settings.section", "priority": 99}),
        Some("single"),
        &priorities,
    )
    .unwrap();
    let second = normalize_slot_registration(
        &package(),
        &json!({"name": "settings.section"}),
        None,
        &priorities,
    )
    .unwrap();
    assert_eq!(first.priority, Some(json!(-1)));
    assert_eq!(first.options["priority"], json!(-1));
    assert_eq!(second.priority, Some(json!(-2)));

    let chain = normalize_slot_registration(
        &package(),
        &json!({"name": "chain.slot", "priority": 7}),
        Some("chain"),
        &priorities,
    )
    .unwrap();
    assert_eq!(chain.priority, Some(json!(7)));
    assert_eq!(chain.options["priority"], json!(7));
    let self_view = normalize_slot_registration(
        &package(),
        &json!({"name": "tool.view.cordis", "key": "self"}),
        Some("single"),
        &priorities,
    )
    .unwrap();
    assert_eq!(self_view.options["key"], "panel-1.pkg-2");
}

#[test]
fn malformed_slots_and_impersonating_self_keys_fail_before_registration() {
    let priorities = ClientPriorityAllocator::default();
    for (options, expected) in [
        (Value::Null, "needs an options object"),
        (json!({}), "need a string `name`"),
        (
            json!({"name": "tool.view.cordis", "key": "panel-9.pkg-9"}),
            "only accepts key \"self\"",
        ),
    ] {
        let error =
            normalize_slot_registration(&package(), &options, None, &priorities).unwrap_err();
        assert!(error.contains(expected), "{error}");
    }
}

#[test]
fn theme_override_forces_package_source_and_teaches_two_argument_shape() {
    let normalized = normalize_theme_override(
        &package(),
        &json!("impersonated"),
        Some(&json!({"--token": {"light": "x", "dark": "y"}})),
    )
    .unwrap();
    assert_eq!(normalized.source, "panel-1.pkg-2");
    assert_eq!(
        normalized.tokens,
        Some(json!({"--token": {"light": "x", "dark": "y"}}))
    );
    let error = normalize_theme_override(&package(), &json!({"--token": {}}), None).unwrap_err();
    assert!(error.contains("takes two arguments"));
    assert!(error.contains("source is replaced with your package id"));
}
