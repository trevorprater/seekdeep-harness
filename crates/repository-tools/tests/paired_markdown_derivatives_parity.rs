//! Source-oracle coverage for bilingual Markdown derivative partitioning.

use seekdeep_repository_tools::paired_markdown_derivatives::{
    MarkdownDerivativePartition, partition_paired_markdown_derivatives,
};

#[derive(Clone, Debug, PartialEq, Eq)]
struct Block {
    document: String,
    kind: String,
    code: String,
}

fn block(document: &str, kind: &str, code: &str) -> Block {
    Block {
        document: document.into(),
        kind: kind.into(),
        code: code.into(),
    }
}

fn partition(blocks: &[Block]) -> MarkdownDerivativePartition<Block> {
    partition_paired_markdown_derivatives(
        blocks,
        |block| block.document.clone(),
        |block| format!("{}\0{}", block.kind, block.code),
    )
}

#[test]
fn complete_byte_identical_chinese_sequence_is_derivative() {
    let english = vec![
        block("docs/example.md", "ts", "const one = 1"),
        block("docs/example.md", "type-equiv", "interface Example {}"),
    ];
    let chinese = english
        .iter()
        .cloned()
        .map(|mut block| {
            block.document = "docs/example.zh.md".into();
            block
        })
        .collect::<Vec<_>>();
    let unrelated = block("docs/other.md", "ts", "const other = 2");
    let blocks = english
        .iter()
        .chain(&chinese)
        .cloned()
        .chain([unrelated.clone()])
        .collect::<Vec<_>>();
    assert_eq!(
        partition(&blocks),
        MarkdownDerivativePartition {
            primary: english.into_iter().chain([unrelated]).collect(),
            derivatives: chinese,
        }
    );
}

#[test]
fn reordered_changed_partial_and_orphan_sequences_stay_primary() {
    let sequence = |document: &str| {
        vec![
            block(document, "ts", "const one = 1"),
            block(document, "ts", "const two = 2"),
        ]
    };
    let english = sequence("docs/example.md");
    let mut changed = english.clone();
    for (index, block) in changed.iter_mut().enumerate() {
        block.document = "docs/example.zh.md".into();
        if index == 0 {
            block.code = "const one = 0".into();
        }
    }
    let reordered_english = sequence("docs/reordered.md");
    let reordered = reordered_english
        .iter()
        .rev()
        .cloned()
        .map(|mut block| {
            block.document = "docs/reordered.zh.md".into();
            block
        })
        .collect::<Vec<_>>();
    let partial_english = sequence("docs/partial.md");
    let mut partial = partial_english[0].clone();
    partial.document = "docs/partial.zh.md".into();
    let orphan = block("docs/orphan.zh.md", "ts", "const orphan = true");
    let blocks = english
        .into_iter()
        .chain(changed)
        .chain(reordered_english)
        .chain(reordered)
        .chain(partial_english)
        .chain([partial, orphan])
        .collect::<Vec<_>>();
    assert_eq!(
        partition(&blocks),
        MarkdownDerivativePartition {
            primary: blocks,
            derivatives: vec![],
        }
    );
}

#[test]
fn fence_kind_must_match_the_body() {
    let english = block("docs/example.md", "type-equiv", "interface Example {}");
    let chinese = block("docs/example.zh.md", "public-api", "interface Example {}");
    assert_eq!(
        partition(&[english.clone(), chinese.clone()]),
        MarkdownDerivativePartition {
            primary: vec![english, chinese],
            derivatives: vec![],
        }
    );
}
