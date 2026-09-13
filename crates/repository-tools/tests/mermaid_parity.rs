//! Scope, fence extraction, archived exclusion, ordering, and diagnostics.

use seekdeep_repository_tools::mermaid::{render_mermaid_report, verify_mermaid_with};

#[test]
fn extracts_only_live_mermaid_fences_and_preserves_order() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("docs")).unwrap();
    std::fs::create_dir_all(root.path().join(".agents/notes/archived/feature")).unwrap();
    std::fs::write(
        root.path().join("docs/graph.md"),
        "# Graph\n\n```mermaid\nflowchart TD\n  A --> B\n```\n\n```js\nignored()\n```\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join(".agents/notes/archived/feature/old.md"),
        "```mermaid\nbroken\n```\n",
    )
    .unwrap();
    let report = verify_mermaid_with(root.path(), |blocks| {
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].file, "docs/graph.md");
        assert_eq!(blocks[0].line, 3);
        Ok(vec![None])
    })
    .unwrap();
    assert_eq!(report.blocks, 1);
    assert!(report.violations.is_empty());
}

#[test]
fn parser_errors_are_whitespace_normalized_and_located() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("docs")).unwrap();
    std::fs::write(root.path().join("docs/bad.md"), "```mermaid\nbad\n```\n").unwrap();
    let report = verify_mermaid_with(root.path(), |_blocks| {
        Ok(vec![Some(" parse\n  failed ".to_owned())])
    })
    .unwrap();
    assert_eq!(report.violations[0].message, "parse failed");
    assert!(render_mermaid_report(&report).contains("docs/bad.md:1  parse failed"));
}
