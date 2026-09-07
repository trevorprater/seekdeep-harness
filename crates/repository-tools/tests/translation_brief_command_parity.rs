//! The `gen-translation-brief` command over a temporary repository: scope
//! planning, request handling, and the `--apply` code-fence splice.

use std::path::Path;

use seekdeep_repository_tools::{
    translation_brief::{BriefDirection, BriefScope},
    translation_brief_command::{BriefOutcome, plan_scope, run_translation_brief},
    translation_pairing::render_pair_metadata,
    translation_pairing_git::store_git_blob,
};

const TERMINOLOGY: &str = "| English | 中文 | 首次出现 | 不要译作 | 备注 |\n|---|---|---|---|---|\n| agent | agent | agent（智能体） | 智能体 | |\n";
const EN: &str = "# Guide\n\n[中文](guide.zh.md)\n\nThe agent runs.\n\n```sh\nrun one\n```\n";
const ZH: &str =
    "# 指南\n\n[English](guide.md)\n\nagent（智能体）会运行。\n\n```sh\nrun one\n```\n";

fn git(root: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn repository() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "-q"]);
    std::fs::create_dir_all(root.path().join("scripts")).unwrap();
    std::fs::create_dir_all(root.path().join("docs/i18n")).unwrap();
    std::fs::write(
        root.path()
            .join("scripts/translation-pairing.manifest.json"),
        "{\"excluded\":[\"docs/skipped/\"]}\n",
    )
    .unwrap();
    std::fs::write(root.path().join("docs/i18n/terminology.md"), TERMINOLOGY).unwrap();
    std::fs::write(root.path().join("docs/guide.md"), EN).unwrap();
    std::fs::write(root.path().join("docs/guide.zh.md"), ZH).unwrap();
    let en_hash = store_git_blob(root.path(), EN.as_bytes()).unwrap();
    let zh_hash = store_git_blob(root.path(), ZH.as_bytes()).unwrap();
    std::fs::write(
        root.path().join("docs/guide.i18n.yaml"),
        render_pair_metadata("docs/guide.md", &en_hash, "docs/guide.zh.md", &zh_hash).unwrap(),
    )
    .unwrap();
    root
}

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn reports_nothing_to_brief_when_every_pair_matches_its_record() {
    let root = repository();
    let run = run_translation_brief(root.path(), &[]).unwrap();
    assert_eq!(run.outcome, BriefOutcome::Nothing);
}

#[test]
fn rejects_unknown_flags_and_fails_loud_on_consistent_or_out_of_scope_requests() {
    let root = repository();
    let run = run_translation_brief(root.path(), &args(&["--force"])).unwrap();
    assert_eq!(
        run.outcome,
        BriefOutcome::UnknownFlags(vec!["--force".to_owned()])
    );
    let run = run_translation_brief(
        root.path(),
        &args(&["docs/guide.zh.md", "docs/skipped/x.md", "docs/absent.md"]),
    )
    .unwrap();
    let BriefOutcome::Problems(messages) = run.outcome else {
        panic!("expected problems");
    };
    assert!(
        messages
            .iter()
            .any(|message| message.contains("docs/absent.md: incomplete pair"))
    );
    assert!(
        messages.iter().any(
            |message| message.contains("docs/skipped/x.md: not an in-scope documentation pair")
        )
    );
    assert!(
        messages
            .iter()
            .any(|message| message.contains("docs/guide.md: pair is consistent with its record"))
    );
}

#[test]
fn briefs_a_prose_edit_at_unit_granularity_with_terminology_and_applies_nothing() {
    let root = repository();
    std::fs::write(
        root.path().join("docs/guide.md"),
        EN.replace("The agent runs.", "The agent runs twice."),
    )
    .unwrap();
    let run = run_translation_brief(root.path(), &args(&["--apply", "docs/guide.md"])).unwrap();
    let BriefOutcome::Briefs(text) = run.outcome else {
        panic!("expected a briefing");
    };
    assert!(run.notices.is_empty());
    assert!(text.contains("# Translation update briefing: docs/guide.md"));
    assert!(text.contains("## Changed units"));
    assert!(text.contains("### #2 paragraph — counterpart at docs/guide.zh.md:5"));
    assert!(text.contains("-The agent runs.\n+The agent runs twice."));
    assert!(text.contains("| agent | agent | agent（智能体） | 智能体 | |"));
    assert_eq!(
        std::fs::read_to_string(root.path().join("docs/guide.zh.md")).unwrap(),
        ZH
    );
}

#[test]
fn splices_a_fence_only_edit_under_apply_and_briefs_the_mechanical_scope() {
    let root = repository();
    std::fs::write(
        root.path().join("docs/guide.md"),
        EN.replace("run one", "run two"),
    )
    .unwrap();
    let run = run_translation_brief(root.path(), &args(&["--apply"])).unwrap();
    let BriefOutcome::Briefs(text) = run.outcome else {
        panic!("expected a briefing");
    };
    assert!(text.contains("## Mechanical update — no translation judgment involved"));
    assert_eq!(
        run.notices,
        [
            "gen-translation-brief: applied code-fence splice to docs/guide.zh.md; review the diff, then record the pair."
        ]
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("docs/guide.zh.md")).unwrap(),
        ZH.replace("run one", "run two")
    );
}

#[test]
fn briefs_both_drifted_sides_as_whole_document_updates() {
    let root = repository();
    std::fs::write(
        root.path().join("docs/guide.md"),
        EN.replace("runs", "walks"),
    )
    .unwrap();
    std::fs::write(
        root.path().join("docs/guide.zh.md"),
        ZH.replace("运行", "行走"),
    )
    .unwrap();
    let run = run_translation_brief(root.path(), &[]).unwrap();
    let BriefOutcome::Briefs(text) = run.outcome else {
        panic!("expected briefings");
    };
    assert_eq!(text.matches("## Whole-document update required").count(), 2);
    assert!(text.contains("# Translation update briefing: docs/guide.md"));
    assert!(text.contains("# Translation update briefing: docs/guide.zh.md"));
    assert!(text.contains("\n\n---\n\n"));
}

#[test]
fn plans_sections_when_units_do_not_align_and_documents_when_nothing_does() {
    let last = "# A\n\nOne.\n\n## B\n\n- x\n- y\n";
    let current = "# A\n\nOne, more.\n\n## B\n\n- x\n- y\n";
    let counterpart = "# 甲\n\n一。\n\n## 乙\n\n合并段落。\n";
    let planned = plan_scope(
        TERMINOLOGY,
        last,
        current,
        counterpart,
        BriefDirection::EnToZh,
        false,
    )
    .unwrap();
    assert!(matches!(planned.scope, BriefScope::Sections { .. }));
    let reshaped = "# 甲\n\n一。\n";
    let planned = plan_scope(
        TERMINOLOGY,
        last,
        current,
        reshaped,
        BriefDirection::EnToZh,
        false,
    )
    .unwrap();
    assert!(
        matches!(planned.scope, BriefScope::Document { ref reason } if reason.starts_with("Neither"))
    );
}
