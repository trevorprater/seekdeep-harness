//! Permission schema, labels, popup, risk gate, and locale parity.

use seekdeep_client_ui_commands::SelectConfirmation;
use seekdeep_client_ui_permission_presets::{
    ACCESS_LOCALES, ACCESS_NS, FULL_ACCESS_PRESET, PermissionOption, PermissionSelect,
    SETTINGS_LOCALES, SETTINGS_NS, display_permission_preset, display_preset_name,
    permission_default_of, popup_options,
};
use serde_json::json;

fn schema() -> serde_json::Value {
    json!({
        "uid":6,
        "refs":{
            "1":{"type":"const","value":"read-only"},
            "2":{"type":"const","meta":{"description":"Workspace"},"value":"workspace-write"},
            "3":{"type":"union","list":[1,2]},
            "6":{"type":"object","dict":{"defaultPreset":3}},
        },
    })
}

#[test]
fn dynamic_settings_schema_and_labels_match_the_source() {
    let resolved = permission_default_of(&schema(), &json!({"defaultPreset":"read-only"})).unwrap();
    assert_eq!(resolved.current_value, "read-only");
    assert_eq!(resolved.options[0].label, "Read Only");
    assert_eq!(resolved.options[1].label, "Workspace");
    assert!(permission_default_of(&schema(), &json!({})).is_err());
    assert!(permission_default_of(&schema(), &json!({"defaultPreset":"missing"})).is_err());
    assert_eq!(display_preset_name("read-only-2"), "Read Only 2");
    assert_eq!(display_preset_name("double--dash"), "double--dash");
    assert_eq!(display_preset_name("Ask Every Time"), "Ask Every Time");
    assert_eq!(
        display_permission_preset(FULL_ACCESS_PRESET, "ignored"),
        "Full access"
    );
}

#[test]
fn popup_excludes_custom_marks_current_and_gates_full_access() {
    let select = PermissionSelect {
        current_value: "workspace-write".to_owned(),
        options: vec![
            PermissionOption {
                value: "read-only".to_owned(),
                name: "read-only".to_owned(),
                description: None,
            },
            PermissionOption {
                value: "workspace-write".to_owned(),
                name: "workspace-write".to_owned(),
                description: Some("write files".to_owned()),
            },
            PermissionOption {
                value: FULL_ACCESS_PRESET.to_owned(),
                name: "ignored".to_owned(),
                description: None,
            },
            PermissionOption {
                value: "custom".to_owned(),
                name: "Custom".to_owned(),
                description: None,
            },
        ],
    };
    let rows = popup_options(&select, || SelectConfirmation {
        title: "Enable Full access?".to_owned(),
        description: "Risk".to_owned(),
        acknowledge_label: "Acknowledge".to_owned(),
        cancel_label: "Cancel".to_owned(),
        confirm_label: "Enable".to_owned(),
    });
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[1].active, Some(true));
    assert_eq!(rows[2].label, "Full access");
    assert!(rows[2].confirmation.is_some());
    assert!(rows[0].confirmation.is_none());
}

#[test]
fn both_locale_domains_are_exact() {
    assert_eq!(SETTINGS_NS, "settings.permission");
    assert_eq!(ACCESS_NS, "permission.access");
    assert_eq!(SETTINGS_LOCALES.len(), 9);
    assert_eq!(ACCESS_LOCALES.len(), 5);
    assert_ne!(SETTINGS_LOCALES[5].1, ACCESS_LOCALES[1].1);
    assert_eq!(
        ACCESS_LOCALES[4],
        ("confirm.enable", "启用 Full access", "Enable Full access")
    );
}
