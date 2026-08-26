//! Scope, manifest, CLI, switcher, signature, generated-region, and blob fixtures.

use seekdeep_repository_tools::translation_pairing::{
    TranslationPairingInput, TranslationPairingMode, TranslationPairingScope, blob_hash,
    is_translation_scope_file, language_switcher_targets, links_to, pair_anchor_of_argument,
    parse_translation_markdown, parse_translation_pairing_cli_args,
    parse_translation_pairing_manifest, partition_generated_regions,
    requires_source_language_switcher, translation_structure_diff, translation_structure_signature,
};

fn signature(
    markdown: &str,
) -> seekdeep_repository_tools::translation_pairing::TranslationStructureSignature {
    translation_structure_signature(
        &parse_translation_markdown(markdown).unwrap(),
        &["counterpart.zh.md".to_owned()],
    )
}

#[test]
fn exclusions_only_manifest_is_required_and_obsolete_fields_fail() {
    assert_eq!(
        parse_translation_pairing_manifest("{\"excluded\":[\"docs/generated/\"]}")
            .unwrap()
            .excluded,
        ["docs/generated/"]
    );
    for source in [
        "{}",
        "{\"excluded\":[42]}",
        "{\"excluded\":[],\"required\":[]}",
    ] {
        assert!(parse_translation_pairing_manifest(source).is_err());
    }
}

#[test]
fn scope_includes_all_authored_classes_and_excludes_generated_trees() {
    for file in [
        "README.md",
        "CONTRIBUTING.zh.md",
        "apps/cli/README.i18n.yaml",
        ".agents/notes/proposed/feature.md",
        "docs/guide.md",
        "python/guide.md",
    ] {
        assert!(is_translation_scope_file(file), "{file}");
    }
    for file in [
        "packages/example/guide.md",
        "vendor/example/README.md",
        "packages/example/node_modules/dependency/README.md",
        "coverage/report/README.md",
        "python/sdk-runtime/src/deepseek_harness_runtime/runtime/seekdeep-jsonrpc-agent-macos-arm64/README.md",
        "python/sdk-runtime/src/deepseek_harness_runtime/runtime/node/README.md",
    ] {
        assert!(!is_translation_scope_file(file), "{file}");
    }
}

#[test]
fn cli_normalizes_deduplicates_and_sorts_pair_spellings() {
    assert_eq!(pair_anchor_of_argument(".\\docs\\foo.zh.md"), "docs/foo.md");
    let request = parse_translation_pairing_cli_args(&[
        "docs/foo.zh.md".to_owned(),
        "docs/foo.i18n.yaml".to_owned(),
        "docs/bar.md".to_owned(),
    ])
    .unwrap();
    assert_eq!(request.input, TranslationPairingInput::Worktree);
    assert_eq!(request.mode, TranslationPairingMode::Check);
    assert_eq!(request.scope, TranslationPairingScope::Pairs);
    assert_eq!(request.anchors, ["docs/bar.md", "docs/foo.md"]);
}

#[test]
fn cli_enforces_write_list_cached_and_unknown_flag_boundaries() {
    assert!(parse_translation_pairing_cli_args(&["--write".to_owned()]).is_err());
    let write =
        parse_translation_pairing_cli_args(&["--write".to_owned(), "docs/foo.md".to_owned()])
            .unwrap();
    assert_eq!(write.mode, TranslationPairingMode::Write);
    assert_eq!(write.scope, TranslationPairingScope::Pairs);
    let cached = parse_translation_pairing_cli_args(&[
        "--cached".to_owned(),
        "docs/foo.i18n.yaml".to_owned(),
    ])
    .unwrap();
    assert_eq!(cached.input, TranslationPairingInput::Index);
    assert!(parse_translation_pairing_cli_args(&["--cached".to_owned()]).is_err());
    assert!(
        parse_translation_pairing_cli_args(&["--list".to_owned(), "docs/foo.md".to_owned()])
            .is_err()
    );
    assert!(parse_translation_pairing_cli_args(&["--frobnicate".to_owned()]).is_err());
}

#[test]
fn switchers_accept_relative_and_product_repository_urls() {
    let targets = language_switcher_targets("python/sdk/README.zh.md");
    let relative = parse_translation_markdown("[中文](README.zh.md)").unwrap();
    let public = parse_translation_markdown(
        "[中文](https://github.com/deepseek-ai/seekdeep-harness/blob/master/python/sdk/README.zh.md)",
    )
    .unwrap();
    assert!(links_to(&relative, &targets));
    assert!(links_to(&public, &targets));
    assert!(!requires_source_language_switcher("docs/config-catalog.md"));
    assert!(requires_source_language_switcher("docs/architecture.md"));
}

#[test]
fn list_and_table_signatures_match_source_diagnostics() {
    assert!(
        translation_structure_diff(
            &signature("3. One\n4. Two\n\n- A\n- B\n"),
            &signature("3. 一\n4. 二\n\n- 甲\n- 乙\n"),
        )
        .is_empty()
    );
    assert_eq!(
        translation_structure_diff(&signature("3. One\n4. Two\n"), &signature("1. 一\n2. 二\n"),),
        [
            "list (kind, start, item count) #1 diverges between the pair: \"ordered:start=3:items=2\" vs \"ordered:start=1:items=2\""
        ]
    );
    assert_eq!(
        translation_structure_diff(
            &signature("| A | B |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n"),
            &signature("| 甲 | 乙 |\n|---|---|\n| 一 | 二 |\n"),
        ),
        ["table (row x column count) #1 diverges between the pair: \"3x2\" vs \"2x2\""]
    );
}

#[test]
fn signatures_capture_headings_code_lists_tables_and_non_switcher_links() {
    let tree = parse_translation_markdown(
        "## H\n\n```rust meta\nlet x = 1;\n```\n\n- a\n\n| A |\n|---|\n| B |\n\n[中文](pair.zh.md) [kept](other.md)\n",
    )
    .unwrap();
    let result = translation_structure_signature(&tree, &["pair.zh.md".to_owned()]);
    assert_eq!(result.headings, [2]);
    assert_eq!(result.code, ["```rust meta\nlet x = 1;"]);
    assert_eq!(result.lists, ["bullet:items=1"]);
    assert_eq!(result.tables, ["2x1"]);
    assert_eq!(result.links, ["other.md"]);
}

#[test]
fn generated_regions_partition_exact_marker_bytes() {
    let begin = "<!-- BEGIN GENERATED cordis-surface (generator) — do not edit between markers -->";
    let end = "<!-- END GENERATED cordis-surface -->";
    let document = format!("# T\n\nprose\n\n{begin}\ninjected\n{end}\ntail\n");
    let partition = partition_generated_regions(&document).unwrap();
    assert_eq!(partition.regions, [format!("{begin}\ninjected\n{end}")]);
    assert_eq!(partition.stripped, "# T\n\nprose\n\ntail\n");
}

#[test]
fn generated_regions_reject_unbalanced_nested_mismatched_and_malformed_markers() {
    for (source, expected) in [
        ("<!-- END GENERATED a -->\n", "without a BEGIN"),
        ("<!-- BEGIN GENERATED a -->\n", "without an END"),
        (
            "<!-- BEGIN GENERATED a -->\n<!-- BEGIN GENERATED a -->\n<!-- END GENERATED a -->\n",
            "nested",
        ),
        (
            "<!-- BEGIN GENERATED a -->\nx\n<!-- END GENERATED b -->\n",
            "does not match",
        ),
        (
            "<!-- BEGIN GENERATED a --> trailing\n",
            "malformed generated region marker line",
        ),
    ] {
        assert!(
            partition_generated_regions(source)
                .unwrap_err()
                .to_string()
                .contains(expected)
        );
    }
}

#[test]
fn blob_hash_matches_pinned_git_values() {
    assert_eq!(blob_hash(b""), "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391");
    assert_eq!(
        blob_hash(b"x\n"),
        "587be6b4c3f93f93c489c0111bba5596147a26cb"
    );
}
