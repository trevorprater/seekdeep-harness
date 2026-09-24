//! GitHub slug, explicit-anchor, resolution, decoding, and scope fixtures.

use std::collections::HashSet;

use seekdeep_repository_tools::md_links::{
    MarkdownAnchorCache, MarkdownLinkViolationReason, document_anchors,
    find_markdown_link_violations, github_slug,
};
use tempfile::TempDir;

fn layout(files: &[(&str, &str)]) -> TempDir {
    let root = tempfile::tempdir().unwrap();
    for (relative, content) in files {
        let path = root.path().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    root
}

fn violations_in(root: &TempDir, relative: &str) -> Vec<(String, MarkdownLinkViolationReason)> {
    find_markdown_link_violations(
        root.path(),
        &root.path().join(relative),
        &mut MarkdownAnchorCache::new(),
    )
    .unwrap()
    .into_iter()
    .map(|violation| (violation.url, violation.reason))
    .collect()
}

fn set(values: &[&str]) -> HashSet<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn slugs_rendered_headings_suffixes_repeats_and_reads_explicit_anchors() {
    let source = [
        "# My Doc",
        "## Live `events` — mode!",
        "## Repeat",
        "## Repeat",
        "<a id=\"hand-anchor\"></a>",
        "",
    ]
    .join("\n");
    assert_eq!(
        document_anchors(&source).unwrap(),
        set(&[
            "my-doc",
            "live-events--mode",
            "repeat",
            "repeat-1",
            "hand-anchor",
        ])
    );
    assert_eq!(
        github_slug("Security and authority are non-goals"),
        "security-and-authority-are-non-goals"
    );
}

#[test]
fn underscores_survive_like_github() {
    assert_eq!(github_slug("Showcase: web_fetch"), "showcase-web_fetch");
    assert_eq!(
        document_anchors("## Showcase: web_fetch\n").unwrap(),
        set(&["showcase-web_fetch"])
    );
}

#[test]
fn heading_links_slug_from_rendered_text() {
    assert_eq!(
        document_anchors("## [Install](setup.md)\n").unwrap(),
        set(&["install"])
    );
}

#[test]
fn repeat_suffixes_skip_already_occupied_slugs() {
    let source = ["## Repeat", "## Repeat-1", "## Repeat", ""].join("\n");
    assert_eq!(
        document_anchors(&source).unwrap(),
        set(&["repeat", "repeat-1", "repeat-2"])
    );
}

#[test]
fn explicit_anchors_inside_code_and_comments_do_not_exist() {
    let source = [
        "# Doc",
        "```md",
        "<a id=\"fenced\"></a>",
        "```",
        "Inline `<a id=\"inline\"></a>` sample.",
        "<!-- <a id=\"commented\"></a> -->",
        "<a id=\"real\"></a>",
        "",
    ]
    .join("\n");
    assert_eq!(document_anchors(&source).unwrap(), set(&["doc", "real"]));
}

#[test]
fn valid_same_cross_non_markdown_and_external_fragments_resolve() {
    let root = layout(&[
        (
            "a.md",
            "# A\n\n## Deferred work\n\n[self](#deferred-work) [b](b.md#part-two) [code](x.ts#L10) [ext](https://x.example/#frag)\n",
        ),
        ("b.md", "# B\n\n## Part two\n"),
        ("x.ts", "export {}\n"),
    ]);
    assert!(violations_in(&root, "a.md").is_empty());
}

#[test]
fn missing_same_file_fragment_is_an_anchor_failure() {
    let root = layout(&[("a.md", "# A\n\n[gone](#deferred-work)\n")]);
    assert_eq!(
        violations_in(&root, "a.md"),
        vec![(
            "#deferred-work".to_owned(),
            MarkdownLinkViolationReason::Anchor,
        )]
    );
}

#[test]
fn anchor_matching_is_case_sensitive() {
    let root = layout(&[("a.md", "# A\n\n## Default Loop\n\n[case](#Default-Loop)\n")]);
    assert_eq!(
        violations_in(&root, "a.md"),
        vec![(
            "#Default-Loop".to_owned(),
            MarkdownLinkViolationReason::Anchor,
        )]
    );
}

#[test]
fn missing_cross_file_fragment_is_an_anchor_failure() {
    let root = layout(&[
        ("a.md", "# A\n\n[stale](b.md#old-heading)\n"),
        ("b.md", "# B\n\n## New heading\n"),
    ]);
    assert_eq!(
        violations_in(&root, "a.md"),
        vec![(
            "b.md#old-heading".to_owned(),
            MarkdownLinkViolationReason::Anchor,
        )]
    );
}

#[test]
fn missing_target_precedes_fragment_validation() {
    let root = layout(&[("a.md", "# A\n\n[ghost](missing.md#anything)\n")]);
    assert_eq!(
        violations_in(&root, "a.md"),
        vec![(
            "missing.md#anything".to_owned(),
            MarkdownLinkViolationReason::Target,
        )]
    );
}

#[test]
fn percent_decoding_queries_and_malformed_escapes_match_renderer_boundaries() {
    let root = layout(&[
        (
            "a.md",
            "[encoded](My%20File.md?view=1#part%20two) [bad](missing%zz.md) [partial](%41%zz.md)\n",
        ),
        ("My File.md", "<a id=\"part two\"></a>\n"),
        ("A%zz.md", "exists only after an invalid partial decode\n"),
    ]);
    assert_eq!(
        violations_in(&root, "a.md"),
        vec![
            (
                "missing%zz.md".to_owned(),
                MarkdownLinkViolationReason::Target,
            ),
            ("%41%zz.md".to_owned(), MarkdownLinkViolationReason::Target,)
        ]
    );
}
