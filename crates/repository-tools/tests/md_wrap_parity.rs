//! Paragraph, list, frontmatter, container, code, and UTF-16 report fixtures.

use seekdeep_repository_tools::md_wrap::{
    MarkdownWrapReport, MarkdownWrapViolation, find_markdown_wraps, render_markdown_wrap_report,
};

#[test]
fn paragraphs_and_nested_list_prose_are_rejected_but_structure_is_not() {
    let source = "one physical\nwrapped continuation\n\n- list first\n  list continuation\n\n# heading\n\n```md\ncode\nwrap\n```\n";
    assert_eq!(
        find_markdown_wraps("docs/test.md", source).unwrap(),
        [
            MarkdownWrapViolation {
                file: "docs/test.md".to_owned(),
                line: 1,
                text: "one physical".to_owned(),
            },
            MarkdownWrapViolation {
                file: "docs/test.md".to_owned(),
                line: 4,
                text: "- list first".to_owned(),
            },
        ]
    );
}

#[test]
fn vitepress_frontmatter_and_custom_container_delimiters_are_masked() {
    let source =
        "---\ntitle: long\n  folded: value\n---\n\n::: details\ninside one physical line\n:::\n";
    assert!(
        find_markdown_wraps("docs/test.md", source)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn report_uses_source_wording_and_javascript_utf16_truncation() {
    let text = format!("{}tail", "😀".repeat(41));
    let report = MarkdownWrapReport {
        checked: 1,
        violations: vec![MarkdownWrapViolation {
            file: "docs/test.md".to_owned(),
            line: 7,
            text,
        }],
    };
    let rendered = render_markdown_wrap_report(&report);
    assert!(rendered.starts_with("verify-md-wrap: hard-wrapped prose paragraphs found"));
    assert!(rendered.contains("docs/test.md:7"));
    assert_eq!(rendered.matches('😀').count(), 40);
    assert!(rendered.ends_with("…\n"));
    let split_surrogate = render_markdown_wrap_report(&MarkdownWrapReport {
        checked: 1,
        violations: vec![MarkdownWrapViolation {
            file: "docs/test.md".to_owned(),
            line: 8,
            text: format!("{}😀tail", "a".repeat(79)),
        }],
    });
    assert!(split_surrogate.ends_with(&format!("{}�…\n", "a".repeat(79))));
    assert_eq!(
        render_markdown_wrap_report(&MarkdownWrapReport {
            checked: 12,
            violations: Vec::new(),
        }),
        "verify-md-wrap: 12 file(s) checked, no hard-wrapped prose paragraphs.\n"
    );
}
