//! Source fixtures for GFM plain-text projection and incremental block freezing.

use std::{cell::RefCell, fmt::Write as _, rc::Rc};

use markdown::mdast::Node;
use seekdeep_client_ui_primitives::{
    IncrementalMarkdownParser, MarkdownPlainTextMode, extract_markdown_plain_text, parse_gfm,
};

const MARKDOWN: &str = "# Release notes\n\nFirst **paragraph** with [a link](https://example.com) and ![diagram](diagram.png).\n\n- shipped\n- `verified`\n\n```ts\nconst ready = true\n```";

fn extract(markdown: &str, mode: MarkdownPlainTextMode) -> String {
    extract_markdown_plain_text(markdown, mode).unwrap()
}

fn node_kind(node: &Node) -> &'static str {
    match node {
        Node::Root(_) => "root",
        Node::Paragraph(_) => "paragraph",
        Node::Heading(_) => "heading",
        Node::Blockquote(_) => "blockquote",
        Node::List(_) => "list",
        Node::ListItem(_) => "listItem",
        Node::Code(_) => "code",
        Node::Table(_) => "table",
        Node::Definition(_) => "definition",
        _ => "other",
    }
}

fn top_level(node: &Node) -> &[Node] {
    let Node::Root(root) = node else {
        panic!("root")
    };
    &root.children
}

fn strip_positions(node: &mut Node) {
    node.position_set(None);
    if let Some(children) = node.children_mut() {
        for child in children {
            strip_positions(child);
        }
    }
}

fn collect_strong_text(node: &Node, output: &mut Vec<String>) {
    if let Node::Strong(strong) = node {
        output.push(
            strong
                .children
                .iter()
                .filter_map(|child| match child {
                    Node::Text(text) => Some(text.value.as_str()),
                    _ => None,
                })
                .collect(),
        );
    }
    if let Some(children) = node.children() {
        for child in children {
            collect_strong_text(child, output);
        }
    }
}

#[test]
fn complete_first_line_and_first_paragraph_projection_match_the_oracle() {
    assert_eq!(
        extract(MARKDOWN, MarkdownPlainTextMode::All),
        "Release notes\n\nFirst paragraph with a link and diagram.\n\nshipped\nverified\n\nconst ready = true"
    );
    assert_eq!(
        extract(MARKDOWN, MarkdownPlainTextMode::FirstLine),
        "Release notes"
    );
    assert_eq!(
        extract(MARKDOWN, MarkdownPlainTextMode::FirstParagraph),
        "First paragraph with a link and diagram."
    );
}

#[test]
fn raw_html_tables_references_breaks_and_block_structure_are_preserved() {
    let block = "<background-job-complete id=\"trajectory-ui-watch\">\nCommand: pnpm test\nExit code: 0\n</background-job-complete>";
    assert_eq!(extract(block, MarkdownPlainTextMode::All), block);
    assert_eq!(
        extract(
            "**Status:** <span data-state=\"ok\">ready</span>",
            MarkdownPlainTextMode::All
        ),
        "Status: <span data-state=\"ok\">ready</span>"
    );
    assert_eq!(
        extract(block, MarkdownPlainTextMode::FirstParagraph),
        "<background-job-complete id=\"trajectory-ui-watch\">"
    );
    let source = "> first\\\n> second with ![diagram][asset] and <span>visible</span>\n\n---\n\n| Name | Value |\n| --- | --- |\n| alpha | `1` |\n\n[asset]: diagram.png";
    assert_eq!(
        extract(source, MarkdownPlainTextMode::All),
        "first second with diagram and <span>visible</span>\n\nName\tValue\nalpha\t1"
    );
}

#[test]
fn gfm_parser_emits_positioned_frontier_sensitive_top_level_blocks() {
    let root = parse_gfm(
        "# title\n\nparagraph\n\n- one\n- two\n\n| a | b |\n| - | - |\n| 1 | 2 |\n\n```ts\ncode\n```",
    )
    .unwrap();
    assert_eq!(
        top_level(&root).iter().map(node_kind).collect::<Vec<_>>(),
        ["heading", "paragraph", "list", "table", "code"]
    );
    assert!(
        top_level(&root)
            .iter()
            .all(|node| node.position().is_some())
    );
}

#[test]
fn cjk_friendly_strong_closes_after_punctuation_without_changing_other_contexts() {
    let cases = [
        ("**注意：**内容", "注意："),
        ("**Notice:**内容", "Notice:"),
        ("**事件中间件（waterfall）**实现", "事件中间件（waterfall）"),
        ("**事件中间件(waterfall)**实现", "事件中间件(waterfall)"),
        ("**句号。**后续", "句号。"),
        ("**Period.**后续", "Period."),
        ("**提醒！**继续", "提醒！"),
        ("**Warning!**继续", "Warning!"),
        ("**版权©**内容", "版权©"),
    ];
    let source = cases
        .iter()
        .map(|(markdown, _)| *markdown)
        .collect::<Vec<_>>()
        .join("\n\n");
    let root = parse_gfm(&source).unwrap();
    let mut strong = Vec::new();
    collect_strong_text(&root, &mut strong);
    assert_eq!(
        strong,
        cases
            .iter()
            .map(|(_, expected)| (*expected).to_owned())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        extract(&source, MarkdownPlainTextMode::All),
        cases
            .iter()
            .map(|(markdown, _)| markdown.replace("**", ""))
            .collect::<Vec<_>>()
            .join("\n\n")
    );

    let excluded = "\\**注意：**内容\n\n`**注意：**内容`\n\n**Notice:**text\n\n*提醒！*继续\n\n**普通**内容\n\n*普通*内容";
    let root = parse_gfm(excluded).unwrap();
    let mut strong = Vec::new();
    collect_strong_text(&root, &mut strong);
    assert_eq!(strong, ["普通"]);
}

#[test]
fn incremental_parser_freezes_all_but_two_blocks_and_reuses_cached_allocations() {
    let mut parser = IncrementalMarkdownParser::default();
    let first = parser.update("a\n\nb\n\nc\n\nd\n\ne").unwrap();
    assert_eq!(first.frozen.len(), 3);
    assert_eq!(first.tail.len(), 2);
    assert!(Rc::ptr_eq(
        &first,
        &parser.update("a\n\nb\n\nc\n\nd\n\ne").unwrap()
    ));
    let second = parser.update("a\n\nb\n\nc\n\nd\n\ne\n\nf\n\ng").unwrap();
    assert_eq!(second.frozen.len(), 5);
    assert_eq!(second.tail.len(), 2);
    assert!(
        first
            .frozen
            .iter()
            .zip(&second.frozen)
            .all(|(left, right)| Rc::ptr_eq(left, right))
    );
    assert_eq!(first.generation, second.generation);
}

#[test]
fn two_blocks_stay_unfrozen_and_frozen_keys_remain_a_stable_prefix() {
    let mut parser = IncrementalMarkdownParser::default();
    let short = parser.update("only\n\ntwo blocks").unwrap();
    assert!(short.frozen.is_empty());
    assert_eq!(short.tail.len(), 2);

    let mut parser = IncrementalMarkdownParser::default();
    let mut previous = Vec::<i64>::new();
    let mut text = String::new();
    for index in 0..12 {
        write!(text, "Stable paragraph {index}.\n\n").unwrap();
        let result = parser.update(&text).unwrap();
        let keys = result
            .frozen
            .iter()
            .map(|block| block.key)
            .collect::<Vec<_>>();
        assert_eq!(&keys[..previous.len()], previous);
        previous = keys;
    }
    assert!(previous.len() > 4);
}

#[test]
fn non_append_input_resets_the_prefix_and_generation() {
    let mut parser = IncrementalMarkdownParser::default();
    let before = parser.update("a\n\nb\n\nc\n\nd").unwrap();
    assert!(!before.frozen.is_empty());
    let after = parser.update("different").unwrap();
    assert_eq!(after.generation, before.generation + 1);
    assert!(after.frozen.is_empty());
    assert_eq!(after.tail.len(), 1);
    assert_eq!(node_kind(&after.tail[0].node), "paragraph");
}

#[test]
fn keys_are_absolute_utf16_offsets_across_freezes_and_astral_text() {
    let document = "# 标题 🎉\n\n中文段落 😀\n\nthird\n\nfourth\n\nfifth";
    let expected = ["# 标题 🎉", "中文段落 😀", "third", "fourth", "fifth"].map(|needle| {
        i64::try_from(
            document[..document.find(needle).unwrap()]
                .encode_utf16()
                .count(),
        )
        .unwrap()
    });
    let mut parser = IncrementalMarkdownParser::default();
    let result = parser.update(document).unwrap();
    assert_eq!(
        result
            .frozen
            .iter()
            .chain(&result.tail)
            .map(|block| block.key)
            .collect::<Vec<_>>(),
        expected
    );
}

#[test]
fn positionless_grammars_keep_every_block_in_the_tail_with_unique_fallback_keys() {
    let parser = Rc::new(|text: &str| {
        let mut root = parse_gfm(text)?;
        strip_positions(&mut root);
        Ok(root)
    });
    let mut parser = IncrementalMarkdownParser::new(parser);
    let result = parser.update("a\n\nb\n\nc\n\nd\n\ne").unwrap();
    assert!(result.frozen.is_empty());
    assert_eq!(result.tail.len(), 5);
    assert_eq!(
        result
            .tail
            .iter()
            .map(|block| block.key)
            .collect::<Vec<_>>(),
        [-1, -2, -3, -4, -5]
    );
}

#[test]
fn parsing_work_stays_bounded_to_the_unstable_source_tail() {
    let calls = Rc::new(RefCell::new(Vec::<String>::new()));
    let recorded = calls.clone();
    let grammar = Rc::new(move |text: &str| {
        recorded.borrow_mut().push(text.to_owned());
        parse_gfm(text)
    });
    let mut parser = IncrementalMarkdownParser::new(grammar);
    let paragraphs = (0..40)
        .map(|index| format!("Paragraph number {index} with some words."))
        .collect::<Vec<_>>();
    let mut text = String::new();
    for paragraph in paragraphs {
        text.push_str(&paragraph);
        text.push_str("\n\n");
        parser.update(&text).unwrap();
    }
    let calls = calls.borrow();
    assert!(text.len() > 1_500);
    assert!(calls.iter().skip(5).map(String::len).max().unwrap() < 200);
    assert!(
        calls
            .iter()
            .skip(5)
            .all(|call| !call.contains("Paragraph number 0 "))
    );
    assert!(calls.iter().map(String::len).sum::<usize>() < text.len() * 5);
}

#[test]
fn unclosed_fences_hold_the_frontier_until_the_fence_closes() {
    let mut parser = IncrementalMarkdownParser::default();
    let mut text = "p1.\n\np2.\n\np3.\n\n```ts\n".to_owned();
    let opened = parser.update(&text).unwrap();
    let frozen_at_open = opened.frozen.len();
    assert_eq!(node_kind(&opened.tail.last().unwrap().node), "code");
    for line in [
        "const a = 1\n",
        "\n",
        "looks like a paragraph\n",
        "- looks like a list\n",
    ] {
        text.push_str(line);
        let grown = parser.update(&text).unwrap();
        assert_eq!(grown.frozen.len(), frozen_at_open);
        assert_eq!(node_kind(&grown.tail.last().unwrap().node), "code");
    }
    text.push_str("```\n\nafter one.\n\nafter two.\n");
    let closed = parser.update(&text).unwrap();
    assert!(closed.frozen.len() > frozen_at_open);
    assert!(closed.frozen.iter().any(|block| {
        matches!(block.node.as_ref(), Node::Code(code) if code.value.contains("looks like a list"))
    }));
}

#[test]
fn a_list_extends_across_blank_lines_and_freezes_as_one_block() {
    let mut parser = IncrementalMarkdownParser::default();
    let mut text = "intro.\n\nsecond.\n\nthird.\n\n- item a\n- item b\n".to_owned();
    let before = parser.update(&text).unwrap();
    let frozen_before = before.frozen.len();
    text.push_str("\n- item c\n");
    let extended = parser.update(&text).unwrap();
    assert_eq!(extended.frozen.len(), frozen_before);
    assert!(matches!(
        extended.tail.last().map(|block| block.node.as_ref()),
        Some(Node::List(list)) if list.children.len() == 3
    ));
    text.push_str("\nafter.\n\nmore.\n\nend.\n");
    let after = parser.update(&text).unwrap();
    assert!(after.frozen.iter().any(|block| {
        matches!(block.node.as_ref(), Node::List(list) if list.children.len() == 3)
    }));
}
