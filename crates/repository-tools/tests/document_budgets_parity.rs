//! Word counting, ordered rows, and failure coverage for documentation budgets.

use seekdeep_repository_tools::document_budgets::inspect_document_budgets;

fn write(root: &std::path::Path, relative: &str, content: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

#[test]
fn reports_ordered_ok_missing_invalid_and_over_budget_rows() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path(),
        "scripts/doc-budgets.manifest.json",
        r#"{"ok.md":3,"missing.md":4,"invalid.md":0,"over.md":2}"#,
    );
    write(root.path(), "ok.md", "one  two\nthree\n");
    write(root.path(), "over.md", "one two three");
    let report = inspect_document_budgets(root.path()).unwrap();
    assert_eq!(report.budgeted_documents, 4);
    assert_eq!(
        report.rows,
        [
            "ok         3 / 3      ok.md",
            "MISS       — / 4      missing.md",
            "BAD        — / 0      invalid.md",
            "OVER       3 / 2      over.md",
        ]
    );
    assert_eq!(report.failures.len(), 3);
    assert!(report.failures[0].contains("does not exist"));
    assert!(report.failures[1].contains("positive integer"));
    assert!(report.failures[2].contains("3 words exceeds"));
}
