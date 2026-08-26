//! GFM fences, headings, depth-first pruning, comments, and prose fixtures.

use seekdeep_repository_tools::markdown_util::{
    MarkdownFence, MarkdownHeadingLine, MarkdownProseLine, markdown_fences, markdown_heading_lines,
    markdown_prose_lines, parse_markdown, visit_markdown,
};

#[test]
fn fences_preserve_info_body_locations_and_closed_state() {
    let source = "before\n\n```ts ignore-check\nconst x = 1\n```\n\n    indented\n\n~~~rust\nopen";
    assert_eq!(
        markdown_fences(source).unwrap(),
        [
            MarkdownFence {
                line: 3,
                lang: Some("ts".to_owned()),
                info: "ts ignore-check".to_owned(),
                code: "const x = 1".to_owned(),
                closed: true,
            },
            MarkdownFence {
                line: 7,
                lang: None,
                info: String::new(),
                code: "indented".to_owned(),
                closed: false,
            },
            MarkdownFence {
                line: 9,
                lang: Some("rust".to_owned()),
                info: "rust".to_owned(),
                code: "open".to_owned(),
                closed: false,
            },
        ]
    );
}

#[test]
fn headings_retain_raw_first_line_and_reader_visible_text() {
    let source = "# Hello `code` ![fish](fish.png) <!-- hidden -->\n\nSetext *title*  \n---\n";
    assert_eq!(
        markdown_heading_lines(source).unwrap(),
        [
            MarkdownHeadingLine {
                depth: 1,
                index: 1,
                raw: "# Hello `code` ![fish](fish.png) <!-- hidden -->".to_owned(),
                text: "Hello code fish ".to_owned(),
            },
            MarkdownHeadingLine {
                depth: 2,
                index: 3,
                raw: "Setext *title*  ".to_owned(),
                text: "Setext title".to_owned(),
            },
        ]
    );
}

#[test]
fn prose_excludes_code_and_comment_only_lines_without_normalizing_kept_text() {
    let source = "alpha <!-- hidden --> beta\n<!-- whole\ncomment -->\n\n```ts\ncode\n```\nomega\n";
    assert_eq!(
        markdown_prose_lines(source).unwrap(),
        [
            MarkdownProseLine {
                index: 1,
                raw: "alpha <!-- hidden --> beta".to_owned(),
            },
            MarkdownProseLine {
                index: 4,
                raw: String::new(),
            },
            MarkdownProseLine {
                index: 8,
                raw: "omega".to_owned(),
            },
            MarkdownProseLine {
                index: 9,
                raw: String::new(),
            },
        ]
    );
}

#[test]
fn visitor_is_depth_first_and_false_prunes_children() {
    let root = parse_markdown("# title\n\nparagraph **strong**").unwrap();
    let mut visited = Vec::new();
    visit_markdown(&root, &mut |node| {
        let kind = match node {
            markdown::mdast::Node::Root(_) => "root",
            markdown::mdast::Node::Heading(_) => "heading",
            markdown::mdast::Node::Paragraph(_) => "paragraph",
            markdown::mdast::Node::Strong(_) => "strong",
            markdown::mdast::Node::Text(_) => "text",
            _ => "other",
        };
        visited.push(kind);
        kind != "paragraph"
    });
    assert_eq!(visited, ["root", "heading", "text", "paragraph"]);
}
