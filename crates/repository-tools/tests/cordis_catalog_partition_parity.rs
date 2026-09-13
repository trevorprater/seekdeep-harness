//! Behavioral mirror of `scripts/gen-cordis-catalog-partition.spec.ts` and the
//! `spliceRegion` / `maybeRecordPair` halves of `gen-cordis-catalog-record.spec.ts`.

use std::collections::HashMap;

use indexmap::{IndexMap, IndexSet};
use seekdeep_repository_tools::{
    cordis_catalog_partition::{
        REGION_BEGIN, REGION_END, WalkPartitionInput, WalkPartitionMaps, maybe_record_pair,
        splice_region, walk_partition_problems,
    },
    translation_pairing::{blob_hash, render_pair_metadata},
};

fn map(entries: &[(&str, &str)]) -> IndexMap<String, String> {
    entries
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

fn set(values: &[&str]) -> IndexSet<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

/// A consistent baseline the red cases mutate one facet at a time.
fn baseline() -> (WalkPartitionInput, WalkPartitionMaps) {
    (
        WalkPartitionInput {
            rendered_keys: map(&[("llm", "packages/llm/llm/src/index.ts:10")]),
            rendered_scopes: set(&["llm"]),
            rendered_event_names: set(&["llm/request"]),
            declared_keys: map(&[
                ("llm", "packages/llm/llm/src/index.ts"),
                ("theme", "packages/client/ui-theme/src/client/index.ts"),
            ]),
            declared_events: map(&[
                ("llm/request", "packages/llm/llm/src/index.ts"),
                (
                    "theme/change",
                    "packages/client/ui-theme/src/client/index.ts",
                ),
            ]),
        },
        WalkPartitionMaps {
            service_page: map(&[("llm", "llm-streaming.md")]),
            service_walk_exemptions: map(&[(
                "theme",
                "client-side — packages/client/ui-theme/README.md owns the surface",
            )]),
            event_scope_page: map(&[("llm", "llm-streaming.md")]),
            event_walk_exemptions: map(&[(
                "theme/change",
                "client-face — packages/client/ui-theme/README.md owns the surface",
            )]),
        },
    )
}

#[test]
fn accepts_a_partition_where_every_declared_key_and_event_is_rendered_or_exempted() {
    let (input, maps) = baseline();
    assert!(walk_partition_problems(&input, &maps).is_empty());
}

#[test]
fn rejects_a_declared_event_that_is_neither_rendered_nor_exempted_naming_its_file() {
    let (input, mut maps) = baseline();
    maps.event_walk_exemptions.clear();
    let problems = walk_partition_problems(&input, &maps);
    assert_eq!(problems.len(), 1);
    assert!(problems[0].contains(
        "event 'theme/change' (packages/client/ui-theme/src/client/index.ts) is declared in an Events merge but invisible"
    ));
}

#[test]
fn rejects_an_event_exemption_whose_event_the_projection_renders() {
    let (mut input, mut maps) = baseline();
    input.rendered_scopes = set(&["llm", "theme"]);
    input.rendered_event_names = set(&["llm/request", "theme/change"]);
    maps.event_scope_page = map(&[("llm", "llm-streaming.md"), ("theme", "client-modules.md")]);
    let problems = walk_partition_problems(&input, &maps);
    assert_eq!(problems.len(), 1);
    assert!(problems[0].contains(
        "event 'theme/change' is rendered by the projection but still listed in EVENT_WALK_EXEMPTIONS"
    ));
}

#[test]
fn rejects_rendered_surface_the_independent_scan_cannot_see_naming_the_scan_as_the_defect() {
    let (mut input, maps) = baseline();
    input.declared_keys = map(&[("theme", "packages/client/ui-theme/src/client/index.ts")]);
    input.declared_events = map(&[(
        "theme/change",
        "packages/client/ui-theme/src/client/index.ts",
    )]);
    let problems = walk_partition_problems(&input, &maps);
    assert_eq!(problems.len(), 2);
    assert!(problems[0].contains(
        "ctx.llm is rendered by the projection but the independent scan finds no Context merge declaring it"
    ));
    assert!(problems[1].contains(
        "event 'llm/request' is rendered by the projection but the independent scan finds no Events merge declaring it"
    ));
}

#[test]
fn rejects_an_event_exemption_no_events_merge_declares() {
    let (input, mut maps) = baseline();
    maps.event_walk_exemptions
        .insert("gone/away".to_owned(), "nothing owns this".to_owned());
    let problems = walk_partition_problems(&input, &maps);
    assert_eq!(problems.len(), 1);
    assert!(
        problems[0]
            .contains("EVENT_WALK_EXEMPTIONS names 'gone/away' but no Events merge declares it")
    );
}

#[test]
fn rejects_a_declared_context_key_that_is_neither_rendered_nor_exempted() {
    let (input, mut maps) = baseline();
    maps.service_walk_exemptions.clear();
    let problems = walk_partition_problems(&input, &maps);
    assert_eq!(problems.len(), 1);
    assert!(problems[0].contains(
        "ctx.theme (packages/client/ui-theme/src/client/index.ts) is declared in a Context merge but invisible"
    ));
}

#[test]
fn rejects_an_unmapped_rendered_service_with_its_source_pointer_and_stale_page_maps_both_ways() {
    let (input, mut maps) = baseline();
    maps.service_page = map(&[("ghost", "core.md")]);
    maps.event_scope_page = map(&[("specter", "core.md")]);
    let problems = walk_partition_problems(&input, &maps);
    assert_eq!(problems.len(), 4);
    for expected in [
        "service ctx.llm (packages/llm/llm/src/index.ts:10) has no SERVICE_PAGE entry",
        "event scope 'llm/*' has no EVENT_SCOPE_PAGE entry",
        "SERVICE_PAGE maps 'ctx.ghost' but the projection discovers no such service",
        "EVENT_SCOPE_PAGE maps 'specter/*' but the projection discovers no such scope",
    ] {
        assert!(
            problems.iter().any(|problem| problem.contains(expected)),
            "{expected}"
        );
    }
}

#[test]
fn splice_replaces_exactly_the_cordis_surface_region() {
    let document = format!("# T\n\nprose\n\n{REGION_BEGIN}\nold\n{REGION_END}\ntail\n");
    assert_eq!(
        splice_region(&document, &format!("{REGION_BEGIN}\nnew\n{REGION_END}")).unwrap(),
        format!("# T\n\nprose\n\n{REGION_BEGIN}\nnew\n{REGION_END}\ntail\n")
    );
}

#[test]
fn splice_fails_loud_on_a_page_carrying_only_some_other_generators_region() {
    let foreign = "# T\n\n<!-- BEGIN GENERATED other-surface (other-gen.ts) — do not edit between markers -->\ntheirs\n<!-- END GENERATED other-surface -->\n";
    let error = splice_region(foreign, &format!("{REGION_BEGIN}\nnew\n{REGION_END}")).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("expected exactly 1 cordis-surface region, found 0 BEGIN/0 END")
    );
}

#[test]
fn splice_fails_loud_on_duplicate_cordis_surface_markers() {
    let doubled = format!("{REGION_BEGIN}\na\n{REGION_END}\n{REGION_BEGIN}\nb\n{REGION_END}\n");
    let error = splice_region(&doubled, &format!("{REGION_BEGIN}\nnew\n{REGION_END}")).unwrap_err();
    assert!(error.to_string().contains("found 2 BEGIN/2 END"));
}

const PAGE: &str = "docs/subsystems/fix.md";
const ZH: &str = "docs/subsystems/fix.zh.md";
const META: &str = "docs/subsystems/fix.i18n.yaml";

fn page(prose: &str, region: &str) -> String {
    format!("# Fix\n\n{prose}\n\n{REGION_BEGIN}\n{region}\n{REGION_END}\n")
}

enum Record {
    Consistent,
    Explicit(String),
    Absent,
}

struct Setup {
    root: tempfile::TempDir,
    before: HashMap<String, Vec<u8>>,
}

fn setup(
    before_en: &str,
    before_zh: &str,
    current_en: &str,
    current_zh: &str,
    meta: Record,
    omit_zh_snapshot: bool,
) -> Setup {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("docs/subsystems")).unwrap();
    std::fs::write(root.path().join(PAGE), current_en).unwrap();
    std::fs::write(root.path().join(ZH), current_zh).unwrap();
    let meta = match meta {
        Record::Consistent => Some(
            render_pair_metadata(
                PAGE,
                &blob_hash(before_en.as_bytes()),
                ZH,
                &blob_hash(before_zh.as_bytes()),
            )
            .unwrap(),
        ),
        Record::Explicit(explicit) => Some(explicit),
        Record::Absent => None,
    };
    if let Some(meta) = meta {
        std::fs::write(root.path().join(META), meta).unwrap();
    }
    let mut before = HashMap::new();
    before.insert(PAGE.to_owned(), before_en.as_bytes().to_vec());
    if !omit_zh_snapshot {
        before.insert(ZH.to_owned(), before_zh.as_bytes().to_vec());
    }
    Setup { root, before }
}

fn read(setup: &Setup, rel: &str) -> String {
    std::fs::read_to_string(setup.root.path().join(rel)).unwrap()
}

#[test]
fn record_re_records_a_region_confined_write_over_a_consistent_record() {
    let (before_en, before_zh) = (page("prose.", "old region"), page("散文。", "old region"));
    let (current_en, current_zh) = (page("prose.", "new region"), page("散文。", "new region"));
    let setup = setup(
        &before_en,
        &before_zh,
        &current_en,
        &current_zh,
        Record::Consistent,
        false,
    );
    assert!(maybe_record_pair(PAGE, &setup.before, setup.root.path()).unwrap());
    assert_eq!(
        read(&setup, META),
        render_pair_metadata(
            PAGE,
            &blob_hash(current_en.as_bytes()),
            ZH,
            &blob_hash(current_zh.as_bytes())
        )
        .unwrap()
    );
}

#[test]
fn record_refuses_when_the_pair_was_already_out_of_sync_before_the_run() {
    let (before_en, before_zh) = (page("prose.", "old region"), page("散文。", "old region"));
    let (current_en, current_zh) = (page("prose.", "new region"), page("散文。", "new region"));
    let stale = render_pair_metadata(
        PAGE,
        &blob_hash(b"drifted long ago\n"),
        ZH,
        &blob_hash(before_zh.as_bytes()),
    )
    .unwrap();
    let setup = setup(
        &before_en,
        &before_zh,
        &current_en,
        &current_zh,
        Record::Explicit(stale.clone()),
        false,
    );
    assert!(!maybe_record_pair(PAGE, &setup.before, setup.root.path()).unwrap());
    assert_eq!(read(&setup, META), stale);
}

#[test]
fn record_refuses_a_malformed_record_even_when_its_hashes_are_current() {
    let (before_en, before_zh) = (page("prose.", "old region"), page("散文。", "old region"));
    let (current_en, current_zh) = (page("prose.", "new region"), page("散文。", "new region"));
    let renamed = format!(
        "# comment\nfixXmd: {}\nfix.zh.md: {}\n",
        blob_hash(before_en.as_bytes()),
        blob_hash(before_zh.as_bytes())
    );
    let setup = setup(
        &before_en,
        &before_zh,
        &current_en,
        &current_zh,
        Record::Explicit(renamed.clone()),
        false,
    );
    assert!(!maybe_record_pair(PAGE, &setup.before, setup.root.path()).unwrap());
    assert_eq!(read(&setup, META), renamed);
}

#[test]
fn record_refuses_a_record_with_extra_entries() {
    let (before_en, before_zh) = (page("prose.", "old region"), page("散文。", "old region"));
    let (current_en, current_zh) = (page("prose.", "new region"), page("散文。", "new region"));
    let extra = format!(
        "{}other.md: {}\n",
        render_pair_metadata(
            PAGE,
            &blob_hash(before_en.as_bytes()),
            ZH,
            &blob_hash(before_zh.as_bytes())
        )
        .unwrap(),
        blob_hash(before_en.as_bytes())
    );
    let setup = setup(
        &before_en,
        &before_zh,
        &current_en,
        &current_zh,
        Record::Explicit(extra),
        false,
    );
    assert!(!maybe_record_pair(PAGE, &setup.before, setup.root.path()).unwrap());
}

#[test]
fn record_refuses_a_record_with_a_duplicated_expected_key() {
    let (before_en, before_zh) = (page("prose.", "old region"), page("散文。", "old region"));
    let (current_en, current_zh) = (page("prose.", "new region"), page("散文。", "new region"));
    let duplicated = format!(
        "fix.md: {0}\nfix.md: {0}\nfix.zh.md: {1}\n",
        blob_hash(before_en.as_bytes()),
        blob_hash(before_zh.as_bytes())
    );
    let setup = setup(
        &before_en,
        &before_zh,
        &current_en,
        &current_zh,
        Record::Explicit(duplicated.clone()),
        false,
    );
    assert!(!maybe_record_pair(PAGE, &setup.before, setup.root.path()).unwrap());
    assert_eq!(read(&setup, META), duplicated);
}

#[test]
fn record_refuses_when_prose_drifted_alongside_the_region_write() {
    let (before_en, before_zh) = (page("prose.", "old region"), page("散文。", "old region"));
    let drifted = page("prose, edited by a human.", "new region");
    let current_zh = page("散文。", "new region");
    let setup = setup(
        &before_en,
        &before_zh,
        &drifted,
        &current_zh,
        Record::Consistent,
        false,
    );
    assert!(!maybe_record_pair(PAGE, &setup.before, setup.root.path()).unwrap());
}

#[test]
fn record_refuses_a_brand_new_pair_with_no_record() {
    let (before_en, before_zh) = (page("prose.", "old region"), page("散文。", "old region"));
    let (current_en, current_zh) = (page("prose.", "new region"), page("散文。", "new region"));
    let setup = setup(
        &before_en,
        &before_zh,
        &current_en,
        &current_zh,
        Record::Absent,
        false,
    );
    assert!(!maybe_record_pair(PAGE, &setup.before, setup.root.path()).unwrap());
}

#[test]
fn record_refuses_when_a_side_has_no_pre_write_snapshot() {
    let (before_en, before_zh) = (page("prose.", "old region"), page("散文。", "old region"));
    let (current_en, current_zh) = (page("prose.", "new region"), page("散文。", "new region"));
    let setup = setup(
        &before_en,
        &before_zh,
        &current_en,
        &current_zh,
        Record::Consistent,
        true,
    );
    assert!(!maybe_record_pair(PAGE, &setup.before, setup.root.path()).unwrap());
}
