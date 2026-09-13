//! Separation of byte-identical Chinese Markdown blocks from canonical checks.

/// Canonical blocks and byte-identical paired Chinese derivatives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkdownDerivativePartition<T> {
    /// Blocks that still require the caller's owning check.
    pub primary: Vec<T>,
    /// Chinese blocks covered by the byte-identical unsuffixed sequence.
    pub derivatives: Vec<T>,
}

fn unsuffixed_sibling(document: &str) -> Option<String> {
    document
        .strip_suffix(".zh.md")
        .map(|stem| format!("{stem}.md"))
}

/// Partitions complete byte-identical `.zh.md` sequences from primary blocks.
///
/// Partial, reordered, changed, and orphan sequences stay primary. Both output
/// vectors preserve the input scan order.
pub fn partition_paired_markdown_derivatives<T, D, F>(
    blocks: &[T],
    document_of: D,
    fingerprint_of: F,
) -> MarkdownDerivativePartition<T>
where
    T: Clone,
    D: Fn(&T) -> String,
    F: Fn(&T) -> String,
{
    let mut by_document = Vec::<(String, Vec<usize>)>::new();
    for (index, block) in blocks.iter().enumerate() {
        let document = document_of(block);
        if let Some((_, group)) = by_document
            .iter_mut()
            .find(|(candidate, _)| candidate == &document)
        {
            group.push(index);
        } else {
            by_document.push((document, vec![index]));
        }
    }

    let mut derivative_documents = Vec::new();
    for (document, candidates) in &by_document {
        let Some(sibling) = unsuffixed_sibling(document) else {
            continue;
        };
        let Some((_, originals)) = by_document
            .iter()
            .find(|(candidate, _)| candidate == &sibling)
        else {
            continue;
        };
        if originals.len() != candidates.len() {
            continue;
        }
        if candidates
            .iter()
            .zip(originals)
            .all(|(candidate, original)| {
                fingerprint_of(&blocks[*candidate]) == fingerprint_of(&blocks[*original])
            })
        {
            derivative_documents.push(document.clone());
        }
    }

    let mut primary = Vec::new();
    let mut derivatives = Vec::new();
    for block in blocks {
        if derivative_documents.contains(&document_of(block)) {
            derivatives.push(block.clone());
        } else {
            primary.push(block.clone());
        }
    }
    MarkdownDerivativePartition {
        primary,
        derivatives,
    }
}
