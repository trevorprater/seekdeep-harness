//! Discovery, metadata, identity, and durable Session preset parity.

use std::path::Path;

use seekdeep_agent_presets::{
    AgentPresetConfig, COMPOSITION_FILE, METADATA_FILE, PresetMetadata, PresetRoot, PresetTrust,
    UnknownPresetError, discover_presets, read_preset_metadata, render_preset_metadata,
    resolve_session_preset, scan_root, valid_preset_id,
};
use seekdeep_core::session::{SessionEvent, SessionHeader, SessionId};
use serde_json::json;

async fn preset(directory: &Path, id: &str, composition: Option<&str>) {
    let path = directory.join(id);
    tokio::fs::create_dir_all(&path).await.unwrap();
    if let Some(composition) = composition {
        tokio::fs::write(path.join(COMPOSITION_FILE), composition)
            .await
            .unwrap();
    }
}

fn root(path: &Path, trust: PresetTrust) -> PresetRoot {
    PresetRoot {
        path: path.to_string_lossy().into_owned(),
        trust,
    }
}

fn selection(seq: u64, id: &str) -> SessionEvent {
    SessionEvent {
        event_type: "agent-preset/selected".to_owned(),
        seq,
        time: i64::try_from(seq).unwrap(),
        data: json!({ "agentPreset": id }),
        source_event_seqs: None,
        surface_op: None,
        ignorable: None,
    }
}

#[test]
fn ids_and_closed_trust_config_match_the_directory_boundary() {
    for valid in ["standard", "minimal-2", "0", "a-b-c"] {
        assert!(valid_preset_id(valid), "rejected {valid:?}");
    }
    for invalid in ["", "UPPER", "-lead", "a_b", "..", "a/b"] {
        assert!(!valid_preset_id(invalid), "accepted {invalid:?}");
    }
    let config = AgentPresetConfig {
        default: "standard".to_owned(),
        roots: vec![PresetRoot {
            path: "/presets".to_owned(),
            trust: PresetTrust::System,
        }],
        include_user_root: true,
    };
    assert_eq!(
        serde_json::to_value(config).unwrap()["includeUserRoot"],
        true
    );
    let error =
        UnknownPresetError::new("missing", vec!["minimal".to_owned(), "standard".to_owned()]);
    assert_eq!(error.preset_id, "missing");
    assert_eq!(error.available, ["minimal", "standard"]);
    assert_eq!(
        error.to_string(),
        "agent-presets: preset \"missing\" not found (available: minimal, standard)"
    );
    assert!(
        UnknownPresetError::new("missing", Vec::new())
            .to_string()
            .ends_with("available: none)")
    );
}

#[test]
fn latest_logged_selection_overrides_the_creation_header() {
    let mut header = SessionHeader::new(SessionId::new("session"));
    header.agent_preset = Some("created".to_owned());
    assert_eq!(
        resolve_session_preset(&header, &[]),
        Some("created".to_owned())
    );
    let events = vec![
        selection(0, "first"),
        SessionEvent {
            event_type: "session/title".to_owned(),
            seq: 1,
            time: 1,
            data: json!({}),
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        },
        selection(2, "last"),
    ];
    assert_eq!(
        resolve_session_preset(&header, &events),
        Some("last".to_owned())
    );
    header.agent_preset = None;
    assert_eq!(resolve_session_preset(&header, &[]), None);
}

#[tokio::test]
async fn metadata_reads_trimmed_display_fields_and_never_identity_or_trust() {
    let directory = tempfile::tempdir().unwrap();
    tokio::fs::write(
        directory.path().join(METADATA_FILE),
        "name: \"  标准模式  \"\ndescription: 完整的编码 agent。\norder: 1\nid: forged\ntrust: system\n",
    )
    .await
    .unwrap();
    assert_eq!(
        read_preset_metadata(directory.path()).await,
        PresetMetadata {
            name: Some("标准模式".to_owned()),
            description: Some("完整的编码 agent。".to_owned()),
            order: Some(1.0),
        }
    );
}

#[tokio::test]
async fn metadata_failures_and_non_text_fields_degrade_to_empty() {
    let absent = tempfile::tempdir().unwrap();
    assert_eq!(
        read_preset_metadata(absent.path()).await,
        PresetMetadata::default()
    );
    for source in [
        "name: [unclosed\n",
        "- name: x\n",
        "just a string\n",
        "",
        "name: 42\ndescription:\n  nested: true\norder: .inf\n",
        "name: \"   \"\ndescription: \"\"\n",
    ] {
        let directory = tempfile::tempdir().unwrap();
        tokio::fs::write(directory.path().join(METADATA_FILE), source)
            .await
            .unwrap();
        assert_eq!(
            read_preset_metadata(directory.path()).await,
            PresetMetadata::default(),
            "accepted {source:?}"
        );
    }
}

#[tokio::test]
async fn rendered_metadata_is_minimal_stable_and_round_trips() {
    assert_eq!(
        render_preset_metadata(&PresetMetadata {
            name: Some("标准模式".to_owned()),
            description: None,
            order: Some(1.0),
        })
        .as_deref(),
        Some("name: 标准模式\norder: 1\n")
    );
    assert_eq!(
        render_preset_metadata(&PresetMetadata {
            name: None,
            description: Some("只做检索。".to_owned()),
            order: None,
        })
        .as_deref(),
        Some("description: 只做检索。\n")
    );
    assert!(render_preset_metadata(&PresetMetadata::default()).is_none());
    let directory = tempfile::tempdir().unwrap();
    let rendered = render_preset_metadata(&PresetMetadata {
        name: Some("创造模式".to_owned()),
        description: Some("可以改自己的组装。".to_owned()),
        order: None,
    })
    .unwrap();
    tokio::fs::write(directory.path().join(METADATA_FILE), rendered)
        .await
        .unwrap();
    assert_eq!(
        read_preset_metadata(directory.path()).await,
        PresetMetadata {
            name: Some("创造模式".to_owned()),
            description: Some("可以改自己的组装。".to_owned()),
            order: None,
        }
    );
}

#[tokio::test]
async fn discovery_lists_healthy_and_broken_slots_in_declared_order() {
    let directory = tempfile::tempdir().unwrap();
    preset(directory.path(), "z-authored", Some("[]\n")).await;
    preset(directory.path(), "standard", Some("- name: plugin\n")).await;
    preset(directory.path(), "broken", None).await;
    preset(directory.path(), "BAD", None).await;
    tokio::fs::write(directory.path().join("plain"), "not a directory")
        .await
        .unwrap();
    tokio::fs::write(
        directory.path().join("standard").join(METADATA_FILE),
        "name: Standard\norder: 1\n",
    )
    .await
    .unwrap();
    let presets = scan_root(&root(directory.path(), PresetTrust::System))
        .await
        .unwrap();
    assert_eq!(
        presets
            .iter()
            .map(|preset| preset.id.as_str())
            .collect::<Vec<_>>(),
        ["standard", "broken", "z-authored"]
    );
    assert!(presets[0].broken.is_none());
    assert_eq!(presets[0].name.as_deref(), Some("Standard"));
    assert_eq!(presets[0].trust, PresetTrust::System);
    assert!(presets[1].broken.as_deref().unwrap().contains("is missing"));
    assert!(presets.iter().all(|preset| preset.path.is_absolute()));
}

#[tokio::test]
async fn discovery_reports_yaml_and_recursive_group_shape_failures() {
    let directory = tempfile::tempdir().unwrap();
    let cases = [
        ("yaml", "name: [unclosed\n", "not valid YAML"),
        ("scalar", "name: plugin\n", "top-level list"),
        ("missing-name", "- id: one\n", "names no plugin"),
        (
            "group-shape",
            "- name: group\n  group: true\n  config: nope\n",
            "must hold a list",
        ),
        (
            "nested-name",
            "- name: group\n  group: true\n  config:\n    - id: nested\n",
            "names no plugin",
        ),
    ];
    for (id, source, expected) in cases {
        preset(directory.path(), id, Some(source)).await;
        let found = scan_root(&root(directory.path(), PresetTrust::User))
            .await
            .unwrap();
        let row = found.iter().find(|preset| preset.id == id).unwrap();
        assert!(
            row.broken.as_deref().unwrap().contains(expected),
            "{id}: {:?}",
            row.broken
        );
    }
}

#[tokio::test]
async fn loader_dialect_and_healthy_groups_are_accepted() {
    let directory = tempfile::tempdir().unwrap();
    preset(
        directory.path(),
        "dialect",
        Some(
            "- name: plugin\n  config:\n    dynamic: !!js process.env.FLAG === '1'\n- name: group\n  group: true\n  config:\n    - name: nested\n",
        ),
    )
    .await;
    let found = scan_root(&root(directory.path(), PresetTrust::User))
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    assert!(found[0].broken.is_none(), "{:?}", found[0].broken);
}

#[tokio::test]
async fn roots_are_first_wins_and_absent_roots_are_empty() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    preset(first.path(), "shared", Some("[]\n")).await;
    preset(first.path(), "first", Some("[]\n")).await;
    preset(second.path(), "shared", Some("[]\n")).await;
    preset(second.path(), "second", Some("[]\n")).await;
    let missing = first.path().join("absent");
    assert!(
        scan_root(&root(&missing, PresetTrust::User))
            .await
            .unwrap()
            .is_empty()
    );
    let found = discover_presets(&[
        root(first.path(), PresetTrust::System),
        root(second.path(), PresetTrust::User),
    ])
    .await
    .unwrap();
    assert_eq!(
        found
            .iter()
            .map(|preset| preset.id.as_str())
            .collect::<Vec<_>>(),
        ["first", "shared", "second"]
    );
    assert_eq!(
        found
            .iter()
            .find(|preset| preset.id == "shared")
            .unwrap()
            .trust,
        PresetTrust::System
    );
}
