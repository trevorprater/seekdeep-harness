//! Fold, rail, pointer geometry, dictionary, Host, and slot-contract parity.

#![cfg(not(target_arch = "wasm32"))]

use seekdeep_client_ui_sidebar::*;
use seekdeep_cordis::{Context, FiberState};
use serde_json::Value;

#[test]
fn collapse_keeps_wide_content_until_settle_then_enters_the_live_rail() {
    let mut state = SidebarVisualState::new(false, 280.0);
    assert!(state.wide());
    assert!(!state.apply_layout(false, 300.0));
    assert_eq!(state.rendered_width(300.0), Some(300.0));
    assert!(state.apply_layout(true, 56.0));
    assert!(state.wide());
    assert!(state.fading());
    assert_eq!(state.rendered_width(56.0), Some(300.0));
    state.settle_collapse();
    assert!(!state.wide());
    assert!(state.rail_in());
    assert_eq!(state.rendered_width(56.0), None);
    assert!(!state.apply_layout(false, 320.0));
    assert!(state.wide());
    assert_eq!(state.rendered_width(320.0), Some(320.0));
}

#[test]
fn refresh_into_collapsed_is_static_without_a_rail_in_crossfade() {
    let state = SidebarVisualState::new(true, 56.0);
    assert!(!state.wide());
    assert!(!state.rail_in());
    assert!(!state.fading());
}

#[test]
fn pointer_geometry_arms_one_linger_and_returning_inside_cancels_it() {
    let bounds = SidebarBounds {
        left: 0.0,
        right: 280.0,
        top: 10.0,
        bottom: 710.0,
    };
    let mut state = SidebarVisualState::new(false, 280.0);
    assert!(state.quiet_bars());
    state.pointer_enter();
    assert!(!state.quiet_bars());
    assert!(state.pointer_move(bounds, 300.0, 20.0));
    assert!(state.linger_armed());
    assert!(!state.pointer_move(bounds, 100.0, 20.0));
    assert!(!state.linger_armed());
    assert!(state.pointer_leave());
    assert!(!state.pointer_leave());
    state.linger_elapsed();
    assert!(state.quiet_bars());
}

#[test]
fn source_durations_dictionaries_and_invariant_identity_are_exact() {
    assert_eq!(COLLAPSE_SETTLE_MS, 150);
    assert_eq!(SCROLLBAR_LINGER_MS, 2_000);
    assert_eq!(
        SIDEBAR_ZH.iter().map(|(key, _)| *key).collect::<Vec<_>>(),
        SIDEBAR_EN.iter().map(|(key, _)| *key).collect::<Vec<_>>()
    );
    assert_eq!(SIDEBAR_EN[0], ("session.new", "New Session"));
    assert_eq!(INVARIANT_NAME, "client-ui-sidebar-invariant");
    for expected in [
        "--seekdeep-sidebar-inline-padding: 12px",
        "--seekdeep-scrollbar-thumb: transparent",
        "--seekdeep-scrollbar-thumb-hover: transparent",
        "animation: seekdeep-sidebar-rail-in 150ms",
        "transform: translateX(49px)",
    ] {
        assert!(SIDEBAR_STYLES.contains(expected), "{expected:?}");
    }
    assert!(!SIDEBAR_STYLES.contains("--dsh-"));
    assert!(
        !SIDEBAR_STYLES
            .lines()
            .any(|line| line.trim_start().starts_with("scrollbar-gutter:"))
    );
}

#[tokio::test]
async fn node_half_is_a_dependency_free_no_op_placeholder() {
    let plugin = host_plugin();
    assert!(plugin.inject().is_empty());
    let context = Context::new();
    let fiber = context.plugin(plugin, Value::Null).unwrap();
    fiber.await_settled().await.unwrap();
    assert_eq!(fiber.fiber().state(), FiberState::Active);
    fiber.dispose().await.unwrap();
}
