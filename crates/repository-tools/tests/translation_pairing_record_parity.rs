//! Path derivation plus canonical, duplicate, unexpected, and malformed records.

use seekdeep_repository_tools::translation_pairing_record::{
    TranslationPairingRecord, parse_translation_pairing_record, render_translation_pairing_record,
    translation_pair_paths, translation_pair_paths_from_metadata,
};

#[test]
fn paths_derive_from_english_and_metadata_anchors() {
    let paths = translation_pair_paths("docs/foo.md").unwrap();
    assert_eq!(paths.source, "docs/foo.md");
    assert_eq!(paths.zh, "docs/foo.zh.md");
    assert_eq!(paths.metadata, "docs/foo.i18n.yaml");
    assert_eq!(
        translation_pair_paths_from_metadata("docs/foo.i18n.yaml").unwrap(),
        paths
    );
    assert!(translation_pair_paths("docs/foo.zh.md").is_err());
    assert!(translation_pair_paths("docs/foo.txt").is_err());
}

#[test]
fn canonical_record_round_trips_exactly() {
    let paths = translation_pair_paths("docs/foo.md").unwrap();
    let record = TranslationPairingRecord {
        source_hash: "1".repeat(40),
        zh_hash: "2".repeat(40),
    };
    let rendered = render_translation_pairing_record(&paths, &record);
    assert!(rendered.ends_with('\n'));
    assert_eq!(
        parse_translation_pairing_record(&rendered, &paths),
        Some(record)
    );
}

#[test]
fn duplicate_unexpected_and_malformed_keys_are_rejected() {
    let paths = translation_pair_paths("docs/foo.md").unwrap();
    for source in [
        format!(
            "foo.md: {}\nfoo.md: {}\nfoo.zh.md: {}\n",
            "1".repeat(40),
            "3".repeat(40),
            "2".repeat(40)
        ),
        format!(
            "foo.md: {}\nbar.zh.md: {}\n",
            "1".repeat(40),
            "2".repeat(40)
        ),
        format!("foo.md: {}\nfoo.zh.md: uppercase\n", "1".repeat(40)),
    ] {
        assert_eq!(parse_translation_pairing_record(&source, &paths), None);
    }
}
