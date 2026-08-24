//! Behavioral mirror of the permission-preset invariant source suite.

mod support;

use seekdeep_cordis::EventArgs;
use seekdeep_core::session::AppendOptions;
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use seekdeep_permission_presets::invariant::register_invariant;
use serde_json::json;

use support::{MountOptions, create_session, mount};

fn config() -> serde_json::Value {
    json!({
        "presets": {
            "safe": {"sandbox": "workspace-write", "approval": "ask"},
            "trusted": {"sandbox": "danger-full-access", "approval": "never"}
        },
        "defaultPreset": "safe"
    })
}

#[tokio::test]
async fn accepts_configured_presets_and_ignores_unrelated_dispatches() {
    let harness = mount(config(), MountOptions::default()).await.unwrap();
    let registry =
        InvariantRegistry::install(&harness.context, &InvariantConfig::default()).unwrap();
    let registration = register_invariant(&registry).unwrap();
    registration.await_ready().await.unwrap();
    let session = create_session(&harness, "permission-valid", None);

    session
        .append(
            "permission/preset",
            json!({"preset": "safe"}),
            AppendOptions::default(),
        )
        .unwrap();
    session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .unwrap();
    harness
        .context
        .events()
        .emit(&harness.context, "tools/change", &EventArgs::new())
        .unwrap();
}

#[tokio::test]
async fn rejects_unknown_live_preset_before_commit() {
    let harness = mount(config(), MountOptions::default()).await.unwrap();
    let registry =
        InvariantRegistry::install(&harness.context, &InvariantConfig::default()).unwrap();
    let registration = register_invariant(&registry).unwrap();
    registration.await_ready().await.unwrap();
    let session = create_session(&harness, "permission-invalid", None);
    let before = session.events();

    let error = session
        .append(
            "permission/preset",
            json!({"preset": "missing"}),
            AppendOptions::default(),
        )
        .unwrap_err();
    assert!(error.to_string().contains("unknown preset \"missing\""));
    assert_eq!(session.events(), before);
}

#[tokio::test]
async fn rejects_unknown_preset_already_present_during_late_registration() {
    let harness = mount(config(), MountOptions::default()).await.unwrap();
    let session = create_session(&harness, "permission-late", None);
    session
        .append(
            "permission/preset",
            json!({"preset": "missing"}),
            AppendOptions::default(),
        )
        .unwrap();
    let registry =
        InvariantRegistry::install(&harness.context, &InvariantConfig::default()).unwrap();
    let registration = register_invariant(&registry).unwrap();

    let error = registration.await_ready().await.unwrap_err();
    assert!(error.to_string().contains("seekdeep-permission-presets"));
    assert!(error.to_string().contains("unknown preset \"missing\""));
}
