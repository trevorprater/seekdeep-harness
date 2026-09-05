//! Structural and lifecycle-format coverage for Rust-owned Agent Note gates.

use seekdeep_repository_tools::agent_note_tree::{
    verify_agent_note_classification, verify_agent_note_format, walk_agent_note_tree,
};

fn write(root: &std::path::Path, relative: &str, content: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn implemented(title: &str, date: &str) -> (String, String) {
    (
        format!(".agents/notes/implemented/architecture/{date}-note.md"),
        format!(
            concat!(
                "# Agent Note: {title}\n\n",
                "Status: implemented\n\n",
                "## Problem\n\nProblem.\n\n",
                "## Decision\n\nDecision.\n\n",
                "## Alternatives considered\n\nAlternative.\n\n",
                "## Consequences\n\nConsequence.\n",
            ),
            title = title,
        ),
    )
}

#[test]
fn valid_tree_discovers_one_english_note_and_ignores_pair_and_allowlist_files() {
    let root = tempfile::tempdir().unwrap();
    let (path, source) = implemented("Valid", "2026-08-25");
    write(root.path(), &path, &source);
    write(
        root.path(),
        ".agents/notes/implemented/architecture/2026-08-25-note.zh.md",
        &source,
    );
    write(
        root.path(),
        ".agents/notes/implemented/AGENTS.md",
        "instructions\n",
    );
    write(
        root.path(),
        ".agents/notes/proposed/AGENTS.md",
        "instructions\n",
    );
    write(
        root.path(),
        ".agents/notes/rejected/AGENTS.md",
        "instructions\n",
    );

    let tree = walk_agent_note_tree(root.path()).unwrap();
    assert!(tree.errors.is_empty());
    assert_eq!(tree.notes.len(), 1);
    assert_eq!(
        tree.notes[0].relative_path,
        "implemented/architecture/2026-08-25-note.md"
    );
    assert!(
        verify_agent_note_format(root.path())
            .unwrap()
            .errors
            .is_empty()
    );
}

#[test]
fn classification_reports_closed_tree_depth_filename_and_legacy_path_errors() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), ".agents/notes/INDEX.md", "forbidden\n");
    write(root.path(), ".agents/notes/unknown/file.md", "unknown\n");
    write(
        root.path(),
        ".agents/notes/proposed/feature/nested/2026-08-25-too-deep.md",
        "deep\n",
    );
    write(
        root.path(),
        ".agents/notes/proposed/unknown/2026-08-25-class.md",
        "class\n",
    );
    write(
        root.path(),
        ".agents/notes/proposed/feature/not-dated.md",
        "date\n",
    );
    std::fs::create_dir_all(root.path().join("docs/rfc")).unwrap();

    let result = verify_agent_note_classification(root.path()).unwrap();
    assert!(result.errors.iter().any(|error| error.contains("INDEX.md")));
    assert!(
        result
            .errors
            .iter()
            .any(|error| error.contains("unknown lifecycle"))
    );
    assert!(
        result
            .errors
            .iter()
            .any(|error| error.contains("got depth 4"))
    );
    assert!(
        result
            .errors
            .iter()
            .any(|error| error.contains("unknown class"))
    );
    assert!(
        result
            .errors
            .iter()
            .any(|error| error.contains("filename must be"))
    );
    assert!(
        result
            .errors
            .iter()
            .any(|error| error.contains("legacy-path: docs/rfc/"))
    );
}

#[test]
fn format_reports_headers_sections_status_grandfather_and_fence_rules() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path(),
        ".agents/notes/implemented/feature/2026-08-25-broken.md",
        concat!(
            "# Wrong\n",
            "not blank\n",
            "Status: proposed\n",
            "not blank\n",
            "## Proposal\n",
            "Status: duplicate\n",
            "```md\n",
            "## Problem\n",
            "Status: hidden\n",
            "```\n",
            "<!-- agent-note-format: alternatives-not-recorded (pre-format Agent Note) -->\n",
            "XXX: legacy ADR/RFC body format\n",
        ),
    );
    let result = verify_agent_note_format(root.path()).unwrap();
    let errors = result.errors.join("\n");
    for expected in [
        "line 1 must be",
        "line 2 must be blank",
        "line 3 must match",
        "line 4 must be blank",
        "only one in the file",
        "first section must be `## Problem`",
        "missing the required `## Decision`",
        "proposal-era heading",
        "grandfather comment is only valid",
        "retired legacy-format debt marker",
    ] {
        assert!(errors.contains(expected), "missing {expected}:\n{errors}");
    }
}
