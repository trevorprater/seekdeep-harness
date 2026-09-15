//! Regression cases for the minimal-update briefing assembly, ported from the
//! pinned `translation-brief.spec.ts`.

use std::collections::BTreeSet;

use seekdeep_repository_tools::translation_brief::{
    BriefBundle, BriefDirection, BriefScope, BundleReason, TranslationBriefInput,
    changed_span_indices, compute_mechanical_update, first_occurrence_context, markdown_units,
    parse_terminology_rows, relevant_terminology_rows, render_translation_brief, section_spans,
    spans_aligned, term_offsets,
};

const DOC: &str = "Preamble line.\n\n# Title\n\nIntro paragraph.\n\n## First\n\nFirst body.\n\n```ts\nconst value = 1\n```\n\n## Second\n\n| A | B |\n|---|---|\n| 1 | 2 |\n\n- item one\n- item two";

const TERMINOLOGY: &str = "| English | 中文 | 首次出现 | 不要译作 | 备注 |\n|---|---|---|---|---|\n| agent | agent | agent（智能体） | 智能体 | |\n| session log | 会话日志 | | 会话记录 | |\n| gate | 门禁 | | | |\n| registry | 注册表 | | | |";

#[test]
fn lists_units_with_container_scoped_kinds_in_document_order() {
    let kinds = markdown_units(DOC)
        .unwrap()
        .into_iter()
        .map(|span| span.kind)
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        [
            "root.0:paragraph",
            "root.1:heading:1",
            "root.2:paragraph",
            "root.3:heading:2",
            "root.4:paragraph",
            "root.5:code",
            "root.6:heading:2",
            "root.7.0:tableRow",
            "root.7.1:tableRow",
            "root.8.0:listItem",
            "root.8.1:listItem",
        ]
    );
}

#[test]
fn lists_heading_sections_with_a_preamble_span_and_heading_labels() {
    let sections = section_spans(DOC).unwrap();
    assert_eq!(
        sections
            .iter()
            .map(|span| span.label.as_str())
            .collect::<Vec<_>>(),
        [
            "(preamble before the first heading)",
            "Title",
            "First",
            "Second"
        ]
    );
    assert_eq!((sections[0].start_line, sections[0].end_line), (1, 2));
    assert_eq!((sections[2].start_line, sections[2].end_line), (7, 14));
}

#[test]
fn labels_units_by_their_node_type() {
    let units = markdown_units(DOC).unwrap();
    assert_eq!(units[0].label, "paragraph");
    assert_eq!(units[1].label, "heading");
    assert_eq!(units[7].label, "tableRow");
}

#[test]
fn aligns_sections_by_depth_only_so_translated_heading_text_still_maps() {
    let zh = DOC
        .replace("## First", "## 第一节")
        .replace("## Second", "## 第二节")
        .replace("# Title", "# 标题");
    assert!(spans_aligned(
        &section_spans(DOC).unwrap(),
        &section_spans(&zh).unwrap()
    ));
}

#[test]
fn aligns_span_lists_only_on_equal_non_empty_kind_sequences() {
    let zh = DOC
        .replace("First body.", "第一段。")
        .replace("item one", "第一项")
        .replace("Intro paragraph.", "导语。");
    assert!(spans_aligned(
        &markdown_units(DOC).unwrap(),
        &markdown_units(&zh).unwrap()
    ));
    let reshaped = DOC.replace("- item one\n- item two", "merged paragraph");
    assert!(!spans_aligned(
        &markdown_units(DOC).unwrap(),
        &markdown_units(&reshaped).unwrap()
    ));
    assert!(!spans_aligned(&[], &[]));
}

#[test]
fn reports_the_indices_whose_text_changed() {
    let edited = DOC
        .replace("First body.", "First body, revised.")
        .replace("| 1 | 2 |", "| 1 | 3 |");
    assert_eq!(
        changed_span_indices(
            &markdown_units(DOC).unwrap(),
            &markdown_units(&edited).unwrap()
        ),
        [4, 8]
    );
}

const EN: &str = "# T\n\nProse.\n\n```sh\nrun one\n```\n";
const ZH: &str = "# T\n\n中文。\n\n```sh\nrun one\n```\n";

#[test]
fn splices_a_fence_only_edit_into_the_counterpart() {
    let edited = EN.replace("run one", "run two");
    assert_eq!(
        compute_mechanical_update(EN, &edited, ZH).unwrap(),
        Some(ZH.replace("run one", "run two"))
    );
}

#[test]
fn refuses_when_prose_changed_too() {
    let edited = EN.replace("Prose.", "Prose!").replace("run one", "run two");
    assert_eq!(compute_mechanical_update(EN, &edited, ZH).unwrap(), None);
}

#[test]
fn refuses_when_the_counterpart_fences_already_diverge_from_last_confirmed() {
    let edited = EN.replace("run one", "run two");
    assert_eq!(
        compute_mechanical_update(EN, &edited, &ZH.replace("run one", "run stale")).unwrap(),
        None
    );
}

#[test]
fn refuses_when_fence_counts_differ_or_nothing_changed() {
    assert_eq!(
        compute_mechanical_update(EN, &format!("{EN}\n```sh\nextra\n```\n"), ZH).unwrap(),
        None
    );
    assert_eq!(compute_mechanical_update(EN, EN, ZH).unwrap(), None);
}

#[test]
fn parses_data_rows_and_skips_the_header_and_separator() {
    let rows = parse_terminology_rows(TERMINOLOGY);
    assert_eq!(
        rows.iter()
            .map(|row| row.english.as_str())
            .collect::<Vec<_>>(),
        ["agent", "session log", "gate", "registry"]
    );
    assert_eq!(rows[0].chinese, "agent");
    assert_eq!(rows[0].first, "agent（智能体）");
}

#[test]
fn matches_english_terms_on_word_boundaries_with_plural_inflections() {
    assert_eq!(term_offsets("two agents met", "agent", true), [4]);
    assert_eq!(term_offsets("two registries", "registry", true), [4]);
    assert!(term_offsets("reagents", "agent", true).is_empty());
    assert!(term_offsets("", "agent", true).is_empty());
}

#[test]
fn selects_rows_for_the_changed_text_per_direction() {
    let english = |direction, text| {
        relevant_terminology_rows(TERMINOLOGY, direction, text)
            .into_iter()
            .map(|row| row.english)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        english(BriefDirection::EnToZh, "All agents write a session log."),
        ["agent", "session log"]
    );
    assert_eq!(
        english(BriefDirection::ZhToEn, "门禁在提交时运行。"),
        ["gate"]
    );
    assert!(english(BriefDirection::EnToZh, "delegate the work").is_empty());
}

const BEFORE: &str = "# T\n\nAlpha paragraph.\n\nThe agent runs.\n";
const AFTER: &str = "# T\n\nAlpha paragraph with an agent.\n\nThe agent runs.\n";

fn agent_rows() -> Vec<seekdeep_repository_tools::translation_brief::TerminologyRow> {
    parse_terminology_rows(TERMINOLOGY)
        .into_iter()
        .filter(|row| row.english == "agent")
        .collect()
}

#[test]
fn flags_a_moved_first_occurrence_and_pulls_the_vacated_span_in() {
    let context = first_occurrence_context(
        BEFORE,
        AFTER,
        &markdown_units(BEFORE).unwrap(),
        &markdown_units(AFTER).unwrap(),
        &agent_rows(),
        &BTreeSet::from([1]),
    );
    assert_eq!(context.notes.len(), 1);
    assert!(context.notes[0].contains("moved from #2 to #1"));
    assert_eq!(context.extra_span_indices, [2]);
}

#[test]
fn stays_silent_when_the_first_occurrence_does_not_move() {
    let unmoved = BEFORE.replace("Alpha paragraph.", "Alpha paragraph, revised.");
    let context = first_occurrence_context(
        BEFORE,
        &unmoved,
        &markdown_units(BEFORE).unwrap(),
        &markdown_units(&unmoved).unwrap(),
        &agent_rows(),
        &BTreeSet::from([1]),
    );
    assert!(context.notes.is_empty());
    assert!(context.extra_span_indices.is_empty());
}

#[test]
fn ignores_rows_without_a_first_occurrence_rendering() {
    let bare = parse_terminology_rows(TERMINOLOGY)
        .into_iter()
        .filter(|row| row.english == "gate")
        .collect::<Vec<_>>();
    let with_gate = AFTER.replace("The agent runs.", "The gate runs.");
    let context = first_occurrence_context(
        BEFORE,
        &with_gate,
        &markdown_units(BEFORE).unwrap(),
        &markdown_units(&with_gate).unwrap(),
        &bare,
        &BTreeSet::from([2]),
    );
    assert!(context.notes.is_empty());
}

fn base(scope: BriefScope) -> TranslationBriefInput {
    TranslationBriefInput {
        source_path: "docs/foo.md".to_owned(),
        counterpart_path: "docs/foo.zh.md".to_owned(),
        direction: BriefDirection::EnToZh,
        diff: "@@ -5 +5 @@\n-old text about the agent\n+new text about the agent".to_owned(),
        scope,
        terminology: relevant_terminology_rows(TERMINOLOGY, BriefDirection::EnToZh, "the agent"),
    }
}

fn bundle() -> BriefBundle {
    BriefBundle {
        index: 4,
        label: "paragraph".to_owned(),
        reason: None,
        confirmed_source_text: "old text about the agent\n".to_owned(),
        current_source_text: "new text about the agent\n".to_owned(),
        counterpart_text: "关于 agent 的旧文本\n".to_owned(),
        counterpart_start_line: 9,
    }
}

#[test]
fn renders_unit_bundles_with_three_way_context_and_line_anchors() {
    let brief = render_translation_brief(&base(BriefScope::Units {
        bundles: vec![bundle()],
        first_occurrence_notes: vec!["agent: the document-wide first occurrence moved from #2 to #1; the agent（智能体） form moves with it (later occurrences drop the annotation).".to_owned()],
    }));
    assert!(brief.contains("# Translation update briefing: docs/foo.md"));
    assert!(brief.contains("## Changed units"));
    assert!(brief.contains("### #4 paragraph — counterpart at docs/foo.zh.md:9"));
    assert!(brief.contains("Last-confirmed English:"));
    assert!(brief.contains("Current Chinese (bring this along):"));
    assert!(brief.contains("## First-occurrence notes"));
    assert!(brief.contains("agent（智能体）"));
    assert!(
        brief.contains("首次出现 annotations attach to the document-wide first occurrence only")
    );
    assert!(brief.contains("verify-translation-pairing --write docs/foo.md"));
}

#[test]
fn marks_first_occurrence_bundles_and_omits_their_unchanged_confirmed_text() {
    let mut unchanged = bundle();
    unchanged.reason = Some(BundleReason::FirstOccurrence);
    unchanged.confirmed_source_text = unchanged.current_source_text.clone();
    let brief = render_translation_brief(&base(BriefScope::Units {
        bundles: vec![unchanged],
        first_occurrence_notes: Vec::new(),
    }));
    assert!(brief.contains("unchanged; included for a first-occurrence move"));
    assert!(!brief.contains("Last-confirmed English:"));
}

#[test]
fn renders_the_mechanical_scope_with_the_apply_command() {
    let brief = render_translation_brief(&base(BriefScope::Mechanical));
    assert!(brief.contains("## Mechanical update — no translation judgment involved"));
    assert!(brief.contains("gen-translation-brief --apply docs/foo.md"));
    assert!(!brief.contains("## Changed units"));
}

#[test]
fn renders_the_section_fallback_under_its_own_heading() {
    let brief = render_translation_brief(&base(BriefScope::Sections {
        bundles: vec![bundle()],
        first_occurrence_notes: Vec::new(),
    }));
    assert!(brief.contains("## Changed sections"));
    assert!(brief.contains("fine-grained units do not align"));
}

#[test]
fn renders_the_document_fallback_with_its_reason_and_no_bundles() {
    let brief = render_translation_brief(&base(BriefScope::Document {
        reason: "BOTH sides changed since the pair was last confirmed consistent, so no side is a trustworthy mapping anchor; decide which side owns each divergence.".to_owned(),
    }));
    assert!(brief.contains("## Whole-document update required"));
    assert!(brief.contains("BOTH sides changed"));
    assert!(brief.contains("locate the affected regions yourself"));
}

#[test]
fn renders_the_english_target_digest_for_zh_to_en_updates() {
    let mut input = base(BriefScope::Units {
        bundles: vec![bundle()],
        first_occurrence_notes: Vec::new(),
    });
    input.direction = BriefDirection::ZhToEn;
    input.source_path = "docs/foo.zh.md".to_owned();
    input.counterpart_path = "docs/foo.md".to_owned();
    let brief = render_translation_brief(&input);
    assert!(brief.contains("exactly what the new Chinese states"));
    assert!(brief.contains("verify-translation-pairing --write docs/foo.md"));
}

#[test]
fn grows_bundle_fences_past_tilde_runs_in_the_text() {
    let mut tilde = bundle();
    tilde.counterpart_text = "~~~~\ninner\n~~~~\n".to_owned();
    let brief = render_translation_brief(&base(BriefScope::Units {
        bundles: vec![tilde],
        first_occurrence_notes: Vec::new(),
    }));
    assert!(brief.contains("~~~~~markdown"));
}
