//! The client slot catalog's judgement, proven on hand-built inputs (the
//! pinned `gen-client-catalog.spec.ts` cases) and against the pinned source
//! workspace, whose committed catalog the Rust projection must reproduce.

use std::path::Path;

use indexmap::IndexMap;
use seekdeep_repository_tools::{
    client_catalog::{
        client_catalog_json, collect_slot_entries, oversized_slot_reports, resolve_slot_entries,
        validate_slot_contracts,
    },
    slot_walk::{SlotDeclaration, SlotRegistration, TypeDeclaration},
};

fn declaration() -> SlotDeclaration {
    SlotDeclaration {
        key: "demo.seat".to_owned(),
        kind: "single".to_owned(),
        scope: "root".to_owned(),
        js_doc: "/** A seat. Registering here replaces the shipped entry. */".to_owned(),
        package: "@deepseek-ai/dsh-client-demo".to_owned(),
        source: "packages/client/demo/src/client/contract/slots.ts:1".to_owned(),
        ..SlotDeclaration::default()
    }
}

fn registration() -> SlotRegistration {
    SlotRegistration {
        key: "demo.seat".to_owned(),
        package: "@deepseek-ai/dsh-client-demo".to_owned(),
        component: "DemoSeat".to_owned(),
        children: Vec::new(),
        source: "packages/client/demo/src/client/index.ts:10".to_owned(),
        ..SlotRegistration::default()
    }
}

fn owner_types() -> IndexMap<String, TypeDeclaration> {
    let mut types = IndexMap::new();
    types.insert(
        "DemoOwnerProps".to_owned(),
        TypeDeclaration {
            name: "DemoOwnerProps".to_owned(),
            text: "/** Owner share. */\nexport interface DemoOwnerProps {\n  /** Column width. */\n  width: number\n}".to_owned(),
            source: "packages/client/demo/src/client/contract/slots.ts:20".to_owned(),
        },
    );
    types
}

fn kits() -> IndexMap<String, Vec<String>> {
    let mut kits = IndexMap::new();
    kits.insert("root".to_owned(), vec!["useSessions: Hook".to_owned()]);
    kits
}

#[test]
fn accepts_a_documented_slot_whose_owner_props_resolve() {
    let mut owned = declaration();
    owned.owner_type = Some("DemoOwnerProps".to_owned());
    assert!(validate_slot_contracts(&[owned], &[registration()], &owner_types()).is_empty());
}

#[test]
fn rejects_a_slot_with_no_registrant_facing_prose_naming_the_writing_template() {
    let mut undocumented = declaration();
    undocumented.js_doc = String::new();
    let problems = validate_slot_contracts(&[undocumented], &[], &IndexMap::new());
    assert_eq!(problems.len(), 1);
    assert!(problems[0].contains("has no JSDoc prose"));
    assert!(problems[0].contains("ui-settings"));
}

#[test]
fn rejects_a_slot_whose_kind_or_scope_is_not_one_of_the_contract_literals() {
    let mut bad_kind = declaration();
    bad_kind.kind = "whatever".to_owned();
    let mut bad_scope = declaration();
    bad_scope.scope = "whatever".to_owned();
    for (field, bad) in [("kind", bad_kind), ("scope", bad_scope)] {
        let problems = validate_slot_contracts(&[bad], &[], &IndexMap::new());
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains(&format!("no literal '{field}'")));
    }
}

#[test]
fn rejects_owner_props_no_exported_declaration_provides() {
    let mut missing = declaration();
    missing.owner_type = Some("MissingProps".to_owned());
    let problems = validate_slot_contracts(&[missing], &[], &IndexMap::new());
    assert_eq!(problems.len(), 1);
    assert!(problems[0].contains("MissingProps"));
}

#[test]
fn rejects_the_same_key_declared_twice_because_a_merge_would_hide_one_contract() {
    let mut other = declaration();
    other.source = "packages/client/other/src/client/slots.ts:3".to_owned();
    let problems = validate_slot_contracts(&[declaration(), other], &[], &IndexMap::new());
    assert_eq!(problems.len(), 1);
    assert!(problems[0].contains("is also declared at"));
}

#[test]
fn rejects_a_registration_into_an_undeclared_slot_as_a_scan_blind_spot() {
    let mut ghost = registration();
    ghost.key = "ghost.seat".to_owned();
    let problems = validate_slot_contracts(&[declaration()], &[ghost], &IndexMap::new());
    assert_eq!(problems.len(), 1);
    assert!(problems[0].contains("blind spot"));
}

#[test]
fn rejects_a_children_declaration_for_a_slot_no_merge_types() {
    let mut parent = registration();
    parent.children = vec!["ghost.child".to_owned()];
    let problems = validate_slot_contracts(&[declaration()], &[parent], &IndexMap::new());
    assert_eq!(problems.len(), 1);
    assert!(problems[0].contains("child slot 'ghost.child'"));
}

#[test]
fn warns_that_a_single_seat_with_a_shipped_occupant_is_replaced_not_shared() {
    let entries =
        resolve_slot_entries(&[declaration()], &[registration()], &owner_types(), &kits());
    assert_eq!(entries[0].replace_risk, "shadows-shipped-ui");
    assert_eq!(entries[0].occupants, ["client-demo DemoSeat"]);
}

#[test]
fn treats_a_list_seat_as_additive_even_when_shipped_entries_exist() {
    let mut list = declaration();
    list.kind = "list".to_owned();
    let mut shipped = registration();
    shipped.id = Some("shipped".to_owned());
    let entries = resolve_slot_entries(&[list], &[shipped], &owner_types(), &kits());
    assert_eq!(entries[0].replace_risk, "none");
    assert_eq!(entries[0].occupants, ["client-demo DemoSeat id 'shipped'"]);
    assert_eq!(
        entries[0]
            .register_options
            .iter()
            .map(|option| option.name)
            .collect::<Vec<_>>(),
        ["id", "order", "label"]
    );
}

#[test]
fn names_the_entry_whose_mount_makes_a_child_seat_exist() {
    let mut parent = registration();
    parent.key = "demo.parent".to_owned();
    parent.children = vec!["demo.seat".to_owned()];
    let mut parent_declaration = declaration();
    parent_declaration.key = "demo.parent".to_owned();
    let entries = resolve_slot_entries(
        &[declaration(), parent_declaration],
        &[parent],
        &owner_types(),
        &kits(),
    );
    let seat = entries
        .iter()
        .find(|entry| entry.key == "demo.seat")
        .unwrap();
    assert!(
        seat.declared_by
            .contains("an entry in 'demo.parent' (client-demo)")
    );
    let parent = entries
        .iter()
        .find(|entry| entry.key == "demo.parent")
        .unwrap();
    assert!(parent.declared_by.contains("built in"));
}

#[test]
fn reports_an_open_keyed_domain_and_the_keys_already_taken() {
    let mut keyed = declaration();
    keyed.kind = "keyed".to_owned();
    let mut bash = registration();
    bash.entry_key = Some("bash".to_owned());
    let mut read = registration();
    read.entry_key = Some("read".to_owned());
    let entries = resolve_slot_entries(&[keyed], &[bash, read], &owner_types(), &kits());
    assert!(entries[0].key_domain.contains("open: any string"));
    assert!(entries[0].key_domain.contains("already taken: bash, read"));
}

#[test]
fn carries_owner_props_documentation_into_the_entry_not_just_the_type_name() {
    let mut owned = declaration();
    owned.owner_type = Some("DemoOwnerProps".to_owned());
    let entries = resolve_slot_entries(&[owned], &[], &owner_types(), &kits());
    assert!(entries[0].owner_props.join("\n").contains("Column width."));
}

#[test]
fn expands_owner_props_one_level_and_only_names_the_shapes_they_reference() {
    let mut types = owner_types();
    types.insert(
        "Zone".to_owned(),
        TypeDeclaration {
            name: "Zone".to_owned(),
            text: "export interface Zone {\n  session: BigSnapshot\n}".to_owned(),
            source: "packages/client/demo/src/client/contract/slots.ts:30".to_owned(),
        },
    );
    types.insert(
        "BigSnapshot".to_owned(),
        TypeDeclaration {
            name: "BigSnapshot".to_owned(),
            text: "export interface BigSnapshot {\n  turns: number\n}".to_owned(),
            source: "packages/client/demo/src/client/snapshot.ts:1".to_owned(),
        },
    );
    let mut zoned = declaration();
    zoned.owner_type = Some("Zone".to_owned());
    let entries = resolve_slot_entries(&[zoned], &[], &types, &kits());
    let owner = entries[0].owner_props.join("\n");
    assert!(owner.contains("export interface Zone"));
    assert!(!owner.contains("export interface BigSnapshot"));
    assert_eq!(entries[0].owner_props_references, ["BigSnapshot"]);
}

#[test]
fn offers_a_runnable_registration_whose_options_match_the_cardinality() {
    let mut list = declaration();
    list.kind = "list".to_owned();
    let entries = resolve_slot_entries(&[list], &[], &owner_types(), &kits());
    assert!(entries[0].example.contains("ctx.slots.inject('demo.seat'"));
    assert!(entries[0].example.contains("id: 'my-entry'"));
}

#[test]
fn rejects_a_slot_whose_report_a_model_could_not_finish_reading() {
    let mut manual = vec!["/**".to_owned()];
    manual.extend((0..150).map(|index| format!(" * Paragraph {index} about this seat.")));
    manual.push(" */".to_owned());
    let mut long = declaration();
    long.js_doc = manual.join("\n");
    let entries = resolve_slot_entries(&[long], &[], &owner_types(), &IndexMap::new());
    let problems = oversized_slot_reports(&entries);
    assert_eq!(problems.len(), 1);
    assert!(problems[0].contains("slot 'demo.seat'"));
    assert!(problems[0].contains("tighten"));
}

#[test]
fn passes_a_slot_whose_report_stays_within_the_budget() {
    let mut owned = declaration();
    owned.owner_type = Some("DemoOwnerProps".to_owned());
    let entries = resolve_slot_entries(&[owned], &[], &owner_types(), &IndexMap::new());
    assert!(oversized_slot_reports(&entries).is_empty());
}

#[test]
fn collects_every_declared_slot_of_the_pinned_workspace_and_reproduces_the_committed_catalog() {
    let source = std::env::var_os("SEEKDEEP_PARITY_SOURCE").map_or_else(
        || std::path::PathBuf::from("/Users/trevor/ws/deepseek-harness"),
        Into::into,
    );
    let entries = collect_slot_entries(Path::new(&source)).unwrap();
    assert!(entries.len() > 30);
    for entry in &entries {
        assert!(!entry.summary.is_empty(), "{} has no summary", entry.key);
        assert!(["single", "list", "keyed", "chain"].contains(&entry.kind.as_str()));
        assert!(["root", "session", "session-maybe"].contains(&entry.scope.as_str()));
    }
    let root = entries.iter().find(|entry| entry.key == "root").unwrap();
    assert_eq!(root.replace_risk, "shadows-shipped-ui");
    assert!(root.occupants.join(" ").contains("AppFrame"));
    let committed: serde_json::Value = serde_json::from_str(include_str!(
        "../../cordis-client-runner/data/slot-catalog.json"
    ))
    .unwrap();
    let actual = client_catalog_json(&entries);
    if actual != committed {
        let actual_entries = actual["entries"].as_array().unwrap();
        let committed_entries = committed["entries"].as_array().unwrap();
        assert_eq!(actual["notes"], committed["notes"]);
        assert_eq!(actual_entries.len(), committed_entries.len());
        for (index, (left, right)) in actual_entries.iter().zip(committed_entries).enumerate() {
            for (key, value) in left.as_object().unwrap() {
                assert_eq!(value, &right[key], "entry {index} field {key}");
            }
        }
        panic!("catalog differs");
    }
}
