//! Ordered profile-patch AST and composition parity.

use std::{cell::Cell, collections::HashSet};

use seekdeep_loader::profile_patch::{
    JavaScriptExpression, ProfileEntry, ProfileEntryId, ProfileNode, ProfilePatchError,
    ProfilePatchWarning, apply_entry_patches, compose_profile_layers, ensure_entry_id_with,
    parse_entry_list_yaml, parse_patch_list_yaml, render_entry_list_yaml, render_patch_list_yaml,
};

fn entries(source: &str) -> Vec<ProfileEntry> {
    parse_entry_list_yaml(source).unwrap_or_else(|error| panic!("{error}:\n{source}"))
}

fn entry<'a>(entries: &'a [ProfileEntry], id: &str) -> &'a ProfileEntry {
    entries
        .iter()
        .find(|entry| entry.id().is_some_and(|entry_id| entry_id.as_str() == id))
        .unwrap_or_else(|| panic!("entry {id:?} not found"))
}

fn children(entry: &ProfileEntry) -> Vec<ProfileEntry> {
    entry
        .config()
        .and_then(ProfileNode::as_sequence)
        .expect("entry config sequence")
        .iter()
        .map(|node| ProfileEntry::from_fields(node.as_mapping().expect("child mapping").clone()))
        .collect()
}

#[test]
fn yaml_ast_preserves_javascript_and_loader_context_fields() {
    let source = concat!(
        "- insert:\n",
        "    - id: group\n",
        "      name: cordis:group\n",
        "      group: true\n",
        "      inject: [alpha, beta]\n",
        "      intercept: { alpha: scoped }\n",
        "      isolate: { beta: true }\n",
        "      config:\n",
        "        - id: child\n",
        "          name: child-plugin\n",
        "          disabled: !!js process.platform === 'win32'\n",
        "          config:\n",
        "            value: !!js |\n",
        "              ctx.alpha.value\n",
    );
    let patches = parse_patch_list_yaml(source).unwrap();
    let composition = compose_profile_layers(&[patches.clone()]).unwrap();
    let group = entry(composition.entries(), "group");
    assert_eq!(group.group(), Some(&ProfileNode::Bool(true)));
    assert!(matches!(group.inject(), Some(ProfileNode::Sequence(_))));
    assert!(matches!(group.intercept(), Some(ProfileNode::Mapping(_))));
    assert!(matches!(group.isolate(), Some(ProfileNode::Mapping(_))));

    let child = children(group).pop().expect("child");
    assert_eq!(
        child.disabled().and_then(ProfileNode::as_javascript),
        Some(&JavaScriptExpression::new("process.platform === 'win32'")),
        "disabled node: {:?}",
        child.disabled()
    );
    let config = child.config().and_then(ProfileNode::as_mapping).unwrap();
    assert_eq!(
        config.get("value").and_then(ProfileNode::as_javascript),
        Some(&JavaScriptExpression::new("ctx.alpha.value\n"))
    );

    let rendered_entries = render_entry_list_yaml(composition.entries()).unwrap();
    assert!(rendered_entries.contains("!!js"), "{rendered_entries}");
    assert_eq!(
        parse_entry_list_yaml(&rendered_entries).unwrap(),
        composition.entries()
    );
    let rendered_patches = render_patch_list_yaml(&patches).unwrap();
    assert!(rendered_patches.contains("!!js"), "{rendered_patches}");
    assert_eq!(parse_patch_list_yaml(&rendered_patches).unwrap(), patches);
}

#[test]
fn ordered_insert_override_and_warning_rules_match_the_include() {
    let patches = parse_patch_list_yaml(concat!(
        "- insert:\n",
        "    - id: group\n",
        "      name: group-plugin\n",
        "      group: true\n",
        "      config: { discarded: true }\n",
        "    - id: plain\n",
        "      name: plain-plugin\n",
        "- id: group\n",
        "  insert:\n",
        "    - id: child\n",
        "      name: child-plugin\n",
        "      config: { keep: false, removed: true }\n",
        "- id: child\n",
        "  name: child-plugin\n",
        "  config: { replacement: true }\n",
        "  inject: [service]\n",
        "  intercept: { service: alternate }\n",
        "  isolate: { service: owned }\n",
        "  extension-field: null\n",
        "- id: child\n",
        "  name: ''\n",
        "  disabled: false\n",
        "- id: child\n",
        "  name: wrong-plugin\n",
        "  disabled: true\n",
        "- id: missing\n",
        "  config: {}\n",
        "- config: {}\n",
        "- id: plain\n",
        "  insert: []\n",
        "- id: absent-group\n",
        "  insert: []\n",
    ))
    .unwrap();
    let composition = apply_entry_patches(&[], &patches).unwrap();
    let group = entry(composition.entries(), "group");
    let child = children(group).pop().expect("inserted child");
    assert_eq!(child.name(), Some("child-plugin"));
    assert_eq!(
        child.config(),
        Some(&ProfileNode::Mapping(indexmap::indexmap! {
            "replacement".to_owned() => ProfileNode::Bool(true),
        }))
    );
    assert!(matches!(child.inject(), Some(ProfileNode::Sequence(_))));
    assert!(matches!(child.intercept(), Some(ProfileNode::Mapping(_))));
    assert!(matches!(child.isolate(), Some(ProfileNode::Mapping(_))));
    assert_eq!(child.disabled(), Some(&ProfileNode::Bool(false)));
    assert_eq!(child.field("extension-field"), Some(&ProfileNode::Null));

    assert_eq!(
        composition.warnings(),
        [
            ProfilePatchWarning::NameMismatch {
                id: ProfileEntryId::from_wire("child"),
                expected: Some("child-plugin".to_owned()),
                actual: "wrong-plugin".to_owned(),
            },
            ProfilePatchWarning::TargetNotFound {
                id: ProfileEntryId::from_wire("missing"),
            },
            ProfilePatchWarning::MissingId,
            ProfilePatchWarning::InsertTargetNotGroup {
                id: ProfileEntryId::from_wire("plain"),
            },
            ProfilePatchWarning::InsertTargetNotFound {
                id: ProfileEntryId::from_wire("absent-group"),
            },
        ]
    );
    assert_eq!(
        composition
            .warnings()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        [
            "patch: name mismatch for \"child\" (expected \"child-plugin\", got \"wrong-plugin\"), skipping",
            "patch: entry \"missing\" not found",
            "patch: id is required for non-insert patches",
            "patch insert: entry \"plain\" is not a group",
            "patch insert: entry \"absent-group\" not found",
        ]
    );
}

#[test]
fn one_index_keeps_stale_children_and_does_not_index_replacement_configs() {
    let base = entries(concat!(
        "- id: group\n",
        "  name: group-plugin\n",
        "  group: true\n",
        "  config:\n",
        "    - id: old-child\n",
        "      name: old-plugin\n",
        "      config: { old: true }\n",
    ));
    let pristine = base.clone();
    let patches = parse_patch_list_yaml(concat!(
        "- id: group\n",
        "  config:\n",
        "    - id: new-child\n",
        "      name: new-plugin\n",
        "      config: { new: true }\n",
        "- id: old-child\n",
        "  disabled: true\n",
        "- id: new-child\n",
        "  disabled: true\n",
    ))
    .unwrap();
    let composition = apply_entry_patches(&base, &patches).unwrap();
    assert_eq!(
        base, pristine,
        "base input must remain detached and unchanged"
    );

    let new_child = children(entry(composition.entries(), "group"))
        .pop()
        .expect("new child");
    assert_eq!(new_child.id().unwrap().as_str(), "new-child");
    assert_eq!(new_child.disabled(), None);
    assert_eq!(
        composition.warnings(),
        [ProfilePatchWarning::TargetNotFound {
            id: ProfileEntryId::from_wire("new-child"),
        }]
    );
}

#[test]
fn duplicate_ids_target_the_last_depth_first_indexed_entry() {
    let base = entries(concat!(
        "- id: duplicate\n",
        "  name: first\n",
        "  config: { value: first }\n",
        "- id: group\n",
        "  name: group-plugin\n",
        "  group: true\n",
        "  config:\n",
        "    - id: duplicate\n",
        "      name: nested-last\n",
        "      config: { value: nested }\n",
    ));
    let patches =
        parse_patch_list_yaml("- id: duplicate\n  config: { value: patched, only: replacement }\n")
            .unwrap();
    let composition = apply_entry_patches(&base, &patches).unwrap();
    assert_eq!(
        entry(composition.entries(), "duplicate").config(),
        Some(&ProfileNode::Mapping(indexmap::indexmap! {
            "value".to_owned() => ProfileNode::String("first".to_owned()),
        }))
    );
    let nested = children(entry(composition.entries(), "group"));
    assert_eq!(
        nested[0].config(),
        Some(&ProfileNode::Mapping(indexmap::indexmap! {
            "value".to_owned() => ProfileNode::String("patched".to_owned()),
            "only".to_owned() => ProfileNode::String("replacement".to_owned()),
        }))
    );
}

#[test]
fn layer_composition_is_flattened_and_recomposition_reverts_removed_overrides() {
    let bundle = parse_patch_list_yaml(concat!(
        "- insert:\n",
        "    - id: row\n",
        "      name: plugin\n",
        "      config: { bundled: true }\n",
        "    - id: group\n",
        "      name: group-plugin\n",
        "      group: true\n",
    ))
    .unwrap();
    let user = parse_patch_list_yaml(concat!(
        "- id: row\n",
        "  config: { user: true }\n",
        "- id: group\n",
        "  config:\n",
        "    - id: replacement-child\n",
        "      name: child\n",
    ))
    .unwrap();
    let overlay = parse_patch_list_yaml("- id: replacement-child\n  disabled: true\n").unwrap();
    let inputs = (bundle.clone(), user.clone(), overlay.clone());

    let composed = compose_profile_layers(&[bundle.clone(), user.clone(), overlay]).unwrap();
    assert_eq!(
        entry(composed.entries(), "row").config(),
        Some(&ProfileNode::Mapping(indexmap::indexmap! {
            "user".to_owned() => ProfileNode::Bool(true),
        }))
    );
    assert_eq!(
        composed.warnings(),
        [ProfilePatchWarning::TargetNotFound {
            id: ProfileEntryId::from_wire("replacement-child"),
        }]
    );

    let reverted = compose_profile_layers(&[bundle.clone()]).unwrap();
    assert_eq!(
        entry(reverted.entries(), "row").config(),
        Some(&ProfileNode::Mapping(indexmap::indexmap! {
            "bundled".to_owned() => ProfileNode::Bool(true),
        }))
    );
    assert_eq!(bundle, inputs.0);
    assert_eq!(user, inputs.1);
}

#[test]
fn id_generation_happens_only_at_the_explicit_materialization_seam() {
    let mut missing = entries("- name: generated\n").pop().unwrap();
    let patches = parse_patch_list_yaml("- id: deadbeef\n  disabled: true\n").unwrap();
    let before_materialization = apply_entry_patches(&[missing.clone()], &patches).unwrap();
    assert_eq!(
        before_materialization.warnings(),
        [ProfilePatchWarning::TargetNotFound {
            id: ProfileEntryId::from_wire("deadbeef"),
        }]
    );

    let occupied = HashSet::from([ProfileEntryId::from_wire("collision")]);
    let candidates = [
        ProfileEntryId::from_wire("collision"),
        ProfileEntryId::from_wire("deadbeef"),
    ];
    let next = Cell::new(0);
    let generated = ensure_entry_id_with(
        &mut missing,
        |candidate| occupied.contains(candidate),
        || {
            let index = next.get();
            next.set(index + 1);
            candidates[index].clone()
        },
    );
    assert_eq!(generated.as_str(), "deadbeef");
    assert_eq!(missing.id(), Some(generated));

    let mut stable = entries("- id: ' '\n  name: stable\n").pop().unwrap();
    let stable_id = ensure_entry_id_with(
        &mut stable,
        |_| true,
        || panic!("truthy source ids are never regenerated"),
    );
    assert_eq!(stable_id.as_str(), " ");

    let mut empty = entries("- id: ''\n  name: generated\n").pop().unwrap();
    let replacement = ensure_entry_id_with(
        &mut empty,
        |_| false,
        || ProfileEntryId::from_wire("0123abcd"),
    );
    assert_eq!(replacement.as_str(), "0123abcd");
}

#[test]
fn parser_rejects_invalid_document_shapes_but_accepts_empty_layers() {
    assert!(parse_patch_list_yaml("[]\n").unwrap().is_empty());
    assert!(matches!(
        parse_patch_list_yaml("# comments only\n"),
        Err(ProfilePatchError::TopLevelArrayRequired)
    ));
    assert!(matches!(
        parse_patch_list_yaml("key: value\n"),
        Err(ProfilePatchError::TopLevelArrayRequired)
    ));
    assert!(matches!(
        parse_patch_list_yaml("- valid: true\n- scalar\n"),
        Err(ProfilePatchError::MappingRequired { .. })
    ));
    assert!(matches!(
        parse_patch_list_yaml("- value: !future data\n"),
        Err(ProfilePatchError::UnsupportedTag(_))
    ));

    let quoted_empty = parse_patch_list_yaml("- value: !!js ''\n").unwrap();
    assert_eq!(
        quoted_empty[0]
            .field("value")
            .and_then(ProfileNode::as_javascript),
        Some(&JavaScriptExpression::new("")),
        "value node: {:?}",
        quoted_empty[0].field("value")
    );
    let invalid_insert = parse_patch_list_yaml("- insert: { not: an-array }\n").unwrap();
    assert!(matches!(
        apply_entry_patches(&[], &invalid_insert),
        Err(ProfilePatchError::InsertArrayRequired { patch_index: 0 })
    ));
    let falsey_insert = parse_patch_list_yaml("- insert: ''\n").unwrap();
    assert_eq!(
        apply_entry_patches(&[], &falsey_insert).unwrap().warnings(),
        [ProfilePatchWarning::MissingId]
    );
}
