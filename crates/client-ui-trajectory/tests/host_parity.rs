//! Host no-op, locale, and browser contract parity.

#![cfg(not(target_arch = "wasm32"))]

use seekdeep_client_ui_trajectory::{
    DEFAULT_ACTUAL_DURATION, DURATION_PERSISTENCE_KEY, INJECT, LOCALE_NAMESPACE, NAME,
    TRAJECTORY_EN, TRAJECTORY_ZH, host_plugin,
};
use seekdeep_cordis::{Context, FiberState};
use serde_json::Value;

#[test]
fn dictionaries_duration_key_and_client_edges_are_exact() {
    assert_eq!(
        INJECT,
        &[
            "slots",
            "conversationEvents",
            "conversationViews",
            "sessions",
            "locale",
        ]
    );
    assert_eq!(LOCALE_NAMESPACE, "trajectory");
    assert_eq!(DURATION_PERSISTENCE_KEY, "dsh.trajectory.duration");
    assert!(!std::hint::black_box(DEFAULT_ACTUAL_DURATION));
    assert_eq!(TRAJECTORY_ZH.len(), 14);
    assert_eq!(TRAJECTORY_EN.len(), 14);
    assert_eq!(
        TRAJECTORY_ZH
            .iter()
            .map(|(key, _)| *key)
            .collect::<Vec<_>>(),
        TRAJECTORY_EN
            .iter()
            .map(|(key, _)| *key)
            .collect::<Vec<_>>()
    );
    assert_eq!(TRAJECTORY_ZH[0], ("view.trajectory", "轨迹"));
    assert_eq!(TRAJECTORY_EN[13], ("toolbar.searchPlaceholder", "Search"));
}

#[tokio::test]
async fn host_plugin_is_dependency_free_and_effect_free() {
    let plugin = host_plugin();
    assert_eq!(plugin.name(), NAME);
    assert!(plugin.inject().is_empty());
    let context = Context::new();
    let fiber = context.plugin(plugin, Value::Null).unwrap();
    fiber.await_settled().await.unwrap();
    assert_eq!(fiber.fiber().state(), FiberState::Active);
    fiber.dispose().await.unwrap();
}
