//! Canonical documentation points to the declarations that supply its verbatim examples.

use std::path::Path;

use seekdeep_repository_tools::doc_source_links::{ORACLE_REPOSITORY, pin_oracle_source_links};

#[test]
fn oracle_links_preserve_lines_and_markdown_while_local_contracts_stay_local() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("packages/subagent/subagent-dsh-sdk/src")).unwrap();
    std::fs::write(
        root.path()
            .join("packages/subagent/subagent-dsh-sdk/src/index.ts"),
        "export {}\n",
    )
    .unwrap();
    let source = "中文 [source](../../packages/subagent/subagent-seekdeep-sdk/src/index.ts#L8 \"source\")\n[at line](../../packages/subagent/subagent-seekdeep-sdk/src/index.ts:0009?query#old)\n[README](../../crates/subagent-seekdeep-sdk/README.md)\n```md\n[example](missing.ts)\n```\n";
    let expected = format!(
        "中文 [source]({ORACLE_REPOSITORY}/blob/revision/packages/subagent/subagent-dsh-sdk/src/index.ts#L8 \"source\")\n[at line]({ORACLE_REPOSITORY}/blob/revision/packages/subagent/subagent-dsh-sdk/src/index.ts#L9)\n[README](../../crates/subagent-seekdeep-sdk/README.md)\n```md\n[example](missing.ts)\n```\n"
    );
    let actual = pin_oracle_source_links(
        source,
        Path::new("docs/subsystems/subagent.md"),
        root.path(),
        "revision",
    )
    .unwrap();
    assert_eq!(actual, expected);
    assert_eq!(
        pin_oracle_source_links(
            &actual,
            Path::new("docs/subsystems/subagent.md"),
            root.path(),
            "revision"
        )
        .unwrap(),
        actual
    );
    assert!(
        pin_oracle_source_links(
            "[missing](missing.ts)\n",
            Path::new("docs/a.md"),
            root.path(),
            "revision"
        )
        .unwrap_err()
        .to_string()
        .contains("does not exist")
    );
    assert!(
        pin_oracle_source_links(
            "[escape](../../outside.ts)\n",
            Path::new("docs/a.md"),
            root.path(),
            "revision"
        )
        .is_err()
    );
    let external = "[external](custom:declaration.ts) [file](file:source.ts)\n";
    assert_eq!(
        pin_oracle_source_links(external, Path::new("docs/a.md"), root.path(), "revision").unwrap(),
        external
    );
}
