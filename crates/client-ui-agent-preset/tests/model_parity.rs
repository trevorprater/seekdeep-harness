//! Agent preset display, option, copy-id, and namespace parity.

use seekdeep_client_ui_agent_preset::{
    AGENT_PRESET_SETTINGS_NS, AgentPresetOption, CopyDraft, DraftBlocker, PresetDisplaySource,
    PresetDisplayText, PresetTrust, RosterPreset, draft_blocker, preset_display_text,
    preset_options,
};

fn row(id: &str, trust: PresetTrust) -> RosterPreset {
    RosterPreset {
        id: id.to_owned(),
        trust,
        is_default: false,
        name: None,
        description: None,
        broken: None,
    }
}

#[test]
fn known_system_copy_and_file_metadata_fallbacks_are_exact() {
    for (id, name_key, description_key) in [
        (
            "standard",
            "presetStandardName",
            "presetStandardDescription",
        ),
        ("code", "presetCodeName", "presetCodeDescription"),
        ("minimal", "presetMinimalName", "presetMinimalDescription"),
        ("cordis", "presetCordisName", "presetCordisDescription"),
    ] {
        assert_eq!(
            preset_display_text(&PresetDisplaySource {
                id: id.to_owned(),
                trust: PresetTrust::System,
                name: Some("file name".to_owned()),
                description: Some("file description".to_owned()),
            }),
            PresetDisplayText::BuiltIn {
                name_key,
                description_key,
            }
        );
    }
    assert_eq!(
        preset_display_text(&PresetDisplaySource {
            id: "standard".to_owned(),
            trust: PresetTrust::User,
            name: Some("我的标准".to_owned()),
            description: Some("团队 preset".to_owned()),
        }),
        PresetDisplayText::File {
            name: "我的标准".to_owned(),
            description: Some("团队 preset".to_owned()),
        }
    );
    assert_eq!(
        preset_display_text(&PresetDisplaySource {
            id: "bare".to_owned(),
            trust: PresetTrust::User,
            name: None,
            description: None,
        }),
        PresetDisplayText::File {
            name: "bare".to_owned(),
            description: None,
        }
    );
}

#[test]
fn broken_filter_and_duplicate_id_blockers_match_the_source() {
    let mut broken = row("broken", PresetTrust::User);
    broken.broken = Some("invalid composition".to_owned());
    let mut named = row("custom", PresetTrust::User);
    named.name = Some("Custom".to_owned());
    assert_eq!(
        preset_options(&[row("standard", PresetTrust::System), broken, named]),
        [
            AgentPresetOption {
                id: "standard".to_owned(),
                trust: PresetTrust::System,
                name: None,
                description: None,
            },
            AgentPresetOption {
                id: "custom".to_owned(),
                trust: PresetTrust::User,
                name: Some("Custom".to_owned()),
                description: None,
            },
        ]
    );
    let rows = [row("taken", PresetTrust::User)];
    let draft = |id: &str| CopyDraft {
        source_id: "standard".to_owned(),
        source_title: "Standard".to_owned(),
        id: id.to_owned(),
        name: String::new(),
        saving: false,
        error: None,
    };
    assert_eq!(
        draft_blocker(&draft(""), &rows),
        Some(DraftBlocker::IdRequired)
    );
    assert_eq!(
        draft_blocker(&draft("UPPER"), &rows),
        Some(DraftBlocker::IdInvalid)
    );
    assert_eq!(
        draft_blocker(&draft("-leading"), &rows),
        Some(DraftBlocker::IdInvalid)
    );
    assert_eq!(
        draft_blocker(&draft("taken"), &rows),
        Some(DraftBlocker::IdTaken)
    );
    assert_eq!(draft_blocker(&draft("my-agent-"), &rows), None);
    assert_eq!(AGENT_PRESET_SETTINGS_NS, "agent-presets");
}
