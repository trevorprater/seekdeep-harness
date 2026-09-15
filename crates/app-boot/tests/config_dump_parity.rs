//! Behavioral mirror of `app-boot/tests/config-dump.spec.ts`.

use std::{
    fs,
    path::{Path, PathBuf},
};

use seekdeep_app_boot::{
    ConfigDumpLayer, load_overlay_patches, render_config_dump, resolve_config_path,
};
use seekdeep_loader::profile_patch::{ProfileNode, parse_entry_list_yaml, parse_patch_list_yaml};

const BIN: &str = "seekdeep-test-bin";

fn write_base(dir: &Path) -> PathBuf {
    let path = dir.join("base.yml");
    fs::write(
        &path,
        concat!(
            "- id: shared\n",
            "  name: ./noop.mjs\n",
            "  config:\n",
            "    value: base\n",
            "    key: !!js process.env.SEEKDEEP_DUMP_SPEC\n",
            "- id: untouched\n",
            "  name: ./noop.mjs\n",
        ),
    )
    .unwrap();
    path
}

#[test]
fn replay_config_path_swaps_only_the_cordis_suffix() {
    let cwd = Path::new("/workspace");
    assert_eq!(
        resolve_config_path(Path::new("config/cordis.yml"), Some("replay"), cwd).unwrap(),
        Path::new("/workspace/config/cordis.snapshot.yml")
    );
    assert_eq!(
        resolve_config_path(Path::new("config/mycordis.yaml"), Some("replay"), cwd).unwrap(),
        Path::new("/workspace/config/mycordis.snapshot.yml")
    );
    assert_eq!(
        resolve_config_path(Path::new("config/custom.yml"), Some("replay"), cwd).unwrap(),
        Path::new("/workspace/config/custom.yml")
    );
    assert_eq!(
        resolve_config_path(Path::new("cordis.yml"), Some("record"), cwd).unwrap(),
        Path::new("/workspace/cordis.yml")
    );
}

#[test]
fn dump_composes_layers_round_trips_javascript_and_labels_provenance() {
    let temporary = tempfile::tempdir().unwrap();
    let base = write_base(temporary.path());
    let surface = temporary.path().join("surface.yml");
    fs::write(
        &surface,
        concat!(
            "- id: shared\n",
            "  config:\n",
            "    value: surface\n",
            "    key: !!js process.env.SEEKDEEP_DUMP_SPEC\n",
            "- insert:\n",
            "    - id: surface-extra\n",
            "      name: ./noop.mjs\n",
        ),
    )
    .unwrap();
    let user = temporary.path().join("user.yml");
    fs::write(&user, "- id: surface-extra\n  config:\n    value: user\n").unwrap();
    let dump = render_config_dump(
        BIN,
        &base,
        &[
            ConfigDumpLayer {
                label: "surface.yml".to_owned(),
                patches: load_overlay_patches(BIN, &surface).unwrap(),
            },
            ConfigDumpLayer {
                label: "user.yml".to_owned(),
                patches: load_overlay_patches(BIN, &user).unwrap(),
            },
        ],
        |_| {},
    )
    .unwrap();
    let entries = parse_entry_list_yaml(&dump).unwrap();
    assert_eq!(entries.len(), 3);
    let shared = entries[0]
        .config()
        .and_then(ProfileNode::as_mapping)
        .unwrap();
    assert_eq!(shared["value"], ProfileNode::String("surface".to_owned()));
    assert_eq!(
        shared["key"].as_javascript().unwrap().as_str(),
        "process.env.SEEKDEEP_DUMP_SPEC"
    );
    assert_eq!(
        entries[2]
            .config()
            .and_then(ProfileNode::as_mapping)
            .unwrap()["value"],
        ProfileNode::String("user".to_owned())
    );
    assert!(dump.contains("!!js process.env.SEEKDEEP_DUMP_SPEC"));
    assert!(dump.contains("# == base.yml, patched by surface.yml"));
    assert!(dump.contains("# == base.yml\n- id: untouched"));
    assert!(dump.contains("# == surface.yml, patched by user.yml\n- id: surface-extra"));
}

#[test]
fn dump_groups_contiguous_rows_and_uses_one_flattened_patch_index() {
    let temporary = tempfile::tempdir().unwrap();
    let base = temporary.path().join("base.yml");
    fs::write(
        &base,
        "- id: a\n  name: ./noop.mjs\n- id: b\n  name: ./noop.mjs\n",
    )
    .unwrap();
    let plain = render_config_dump(BIN, &base, &[], |_| {}).unwrap();
    assert_eq!(plain.matches("# == base.yml").count(), 1);

    fs::write(
        &base,
        "- id: g\n  name: ./group.mjs\n  group: true\n  config: []\n",
    )
    .unwrap();
    let warnings = std::cell::RefCell::new(Vec::new());
    let dump = render_config_dump(
        BIN,
        &base,
        &[
            ConfigDumpLayer {
                label: "a.yml".to_owned(),
                patches: parse_patch_list_yaml(
                    "- id: g\n  config:\n    - id: child\n      name: ./noop.mjs\n      config: {v: 1}\n",
                )
                .unwrap(),
            },
            ConfigDumpLayer {
                label: "b.yml".to_owned(),
                patches: parse_patch_list_yaml("- id: child\n  config: {v: 2}\n").unwrap(),
            },
        ],
        |line| warnings.borrow_mut().push(line),
    )
    .unwrap();
    assert_eq!(
        &*warnings.borrow(),
        &[format!("{BIN}: [b.yml] patch: entry \"child\" not found")]
    );
    let parsed = parse_entry_list_yaml(&dump).unwrap();
    let child = parsed[0]
        .config()
        .and_then(ProfileNode::as_sequence)
        .unwrap()[0]
        .as_mapping()
        .unwrap();
    assert_eq!(
        child["config"].as_mapping().unwrap()["v"],
        ProfileNode::Number(serde_yml::Number::from(1))
    );
    assert!(dump.contains("# == base.yml, patched by a.yml\n- id: g"));
    assert!(!dump.contains("b.yml\n- id: g"));
}

#[test]
fn skipped_target_warns_with_layer_and_composition_continues() {
    let temporary = tempfile::tempdir().unwrap();
    let base = write_base(temporary.path());
    let warnings = std::cell::RefCell::new(Vec::new());
    let dump = render_config_dump(
        BIN,
        &base,
        &[ConfigDumpLayer {
            label: "overlay.yml".to_owned(),
            patches: parse_patch_list_yaml(
                "- id: only-on-another-surface\n  config: {}\n- id: shared\n  config: {value: patched}\n",
            )
            .unwrap(),
        }],
        |line| warnings.borrow_mut().push(line),
    )
    .unwrap();
    assert_eq!(
        &*warnings.borrow(),
        &[format!(
            "{BIN}: [overlay.yml] patch: entry \"only-on-another-surface\" not found"
        )]
    );
    assert_eq!(
        parse_entry_list_yaml(&dump).unwrap()[0]
            .config()
            .and_then(ProfileNode::as_mapping)
            .unwrap()["value"],
        ProfileNode::String("patched".to_owned())
    );
}

#[test]
fn missing_invalid_and_non_array_base_configs_fail_loud() {
    let temporary = tempfile::tempdir().unwrap();
    let absent = temporary.path().join("absent.yml");
    assert!(
        render_config_dump(BIN, &absent, &[], |_| {})
            .unwrap_err()
            .to_string()
            .starts_with(&format!("{BIN}: failed to read config"))
    );
    let invalid = temporary.path().join("invalid.yml");
    fs::write(&invalid, "invalid: [unclosed\n").unwrap();
    assert!(
        render_config_dump(BIN, &invalid, &[], |_| {})
            .unwrap_err()
            .to_string()
            .starts_with(&format!("{BIN}: failed to parse config"))
    );
    let scalar = temporary.path().join("scalar.yml");
    fs::write(&scalar, "id: not-a-list\n").unwrap();
    assert!(
        render_config_dump(BIN, &scalar, &[], |_| {})
            .unwrap_err()
            .to_string()
            .contains("top-level array")
    );
    assert!(
        load_overlay_patches(BIN, &absent)
            .unwrap_err()
            .to_string()
            .starts_with(&format!("{BIN}: failed to read overlay"))
    );
}
