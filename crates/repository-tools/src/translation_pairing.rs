//! Pure scope, structure, region, and CLI rules for bilingual pairing.

use std::collections::{HashMap, HashSet};

use markdown::mdast::Node;
use regex::Regex;
use serde_json::Value;

use crate::{
    markdown_util::{parse_markdown, visit_markdown},
    translation_pairing_git::git_blob_hash,
    translation_pairing_record::{
        TranslationPairPaths, TranslationPairingRecord, render_translation_pairing_record,
        translation_pair_paths,
    },
};

/// Generated regions and the same document with them removed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneratedRegionPartition {
    /// Complete generated regions, markers included, in document order.
    pub regions: Vec<String>,
    /// Human-owned remainder.
    pub stripped: String,
}

/// Validated fields of `scripts/translation-pairing.manifest.json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranslationPairingManifest {
    /// Source documents exempt from pairing.
    pub excluded: Vec<String>,
}

/// Content plane read by a pairing check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranslationPairingInput {
    /// Current working-tree bytes.
    Worktree,
    /// Stage-zero Git index bytes.
    Index,
}

/// Pairing command operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranslationPairingMode {
    /// Enforce consistency.
    Check,
    /// List every pair state without failing.
    List,
    /// Record confirmed current bytes.
    Write,
}

/// Pairing command discovery scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranslationPairingScope {
    /// Discover the full documentation corpus.
    Corpus,
    /// Touch only named pair anchors.
    Pairs,
}

/// Parsed `verify-translation-pairing` invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranslationPairingCliRequest {
    /// Content plane.
    pub input: TranslationPairingInput,
    /// Operation.
    pub mode: TranslationPairingMode,
    /// Discovery scope.
    pub scope: TranslationPairingScope,
    /// English anchor paths, empty for corpus scope.
    pub anchors: Vec<String>,
}

/// Structural signature compared between two Markdown documents.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TranslationStructureSignature {
    /// Heading depths in document order.
    pub headings: Vec<u8>,
    /// Fenced code blocks as info plus body.
    pub code: Vec<String>,
    /// Table row and column counts.
    pub tables: Vec<String>,
    /// List kind, ordered start, and direct item counts.
    pub lists: Vec<String>,
    /// Link targets except the language switcher.
    pub links: Vec<String>,
}

/// Partitions line-delimited generated regions from human-owned content.
///
/// # Errors
///
/// Returns malformed, nested, unopened, unclosed, or mismatched marker
/// diagnostics.
pub fn partition_generated_regions(content: &str) -> anyhow::Result<GeneratedRegionPartition> {
    static BEGIN: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"^<!-- BEGIN GENERATED (\S+)(?: [^>]*)? -->$")
            .expect("static generated-region opener")
    });
    static END: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"^<!-- END GENERATED (\S+) -->$").expect("static generated-region closer")
    });
    static HINT: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"^<!-- (?:BEGIN|END) GENERATED ").expect("static generated-region hint")
    });

    let mut regions = Vec::new();
    let mut kept = Vec::new();
    let mut open: Option<(String, Vec<String>)> = None;
    for line in content.split('\n') {
        if let Some(slug) = BEGIN
            .captures(line)
            .and_then(|captures| captures.get(1))
            .map(|capture| capture.as_str())
        {
            if open.is_some() {
                anyhow::bail!("generated region BEGIN marker nested inside an open region");
            }
            open = Some((slug.to_owned(), vec![line.to_owned()]));
            continue;
        }
        if let Some(slug) = END
            .captures(line)
            .and_then(|captures| captures.get(1))
            .map(|capture| capture.as_str())
        {
            let Some((opened_slug, mut lines)) = open.take() else {
                anyhow::bail!("generated region END marker without a BEGIN");
            };
            if slug != opened_slug {
                anyhow::bail!(
                    "generated region END slug '{slug}' does not match its BEGIN slug '{opened_slug}'"
                );
            }
            lines.push(line.to_owned());
            regions.push(lines.join("\n"));
            continue;
        }
        if HINT.is_match(line) {
            anyhow::bail!(
                "malformed generated region marker line: {}",
                json_string(line)
            );
        }
        if let Some((_, lines)) = &mut open {
            lines.push(line.to_owned());
        } else {
            kept.push(line.to_owned());
        }
    }
    if open.is_some() {
        anyhow::bail!("generated region BEGIN marker without an END");
    }
    Ok(GeneratedRegionPartition {
        regions,
        stripped: kept.join("\n"),
    })
}

/// Computes the full SHA-1 Git blob hash.
#[must_use]
pub fn blob_hash(content: &[u8]) -> String {
    git_blob_hash(content)
}

/// Parses basename-to-blob-hash entries from a sidecar.
#[must_use]
pub fn parse_pair_metadata(content: &str) -> Option<HashMap<String, String>> {
    static LINE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"^([^:#]+\.md): ([0-9a-f]{40})$").expect("static pair-metadata regex")
    });
    let mut output = HashMap::new();
    for line in content.split('\n') {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let captures = LINE.captures(line)?;
        let key = captures.get(1)?.as_str();
        if output.contains_key(key) {
            return None;
        }
        output.insert(key.to_owned(), captures.get(2)?.as_str().to_owned());
    }
    Some(output)
}

/// Renders canonical pairing metadata.
///
/// # Errors
///
/// Returns invalid English path diagnostics.
pub fn render_pair_metadata(
    source: &str,
    source_hash: &str,
    zh: &str,
    zh_hash: &str,
) -> anyhow::Result<String> {
    let paths = translation_pair_paths(source)?;
    let paths = TranslationPairPaths {
        zh: zh.to_owned(),
        ..paths
    };
    Ok(render_translation_pairing_record(
        &paths,
        &TranslationPairingRecord {
            source_hash: source_hash.to_owned(),
            zh_hash: zh_hash.to_owned(),
        },
    ))
}

/// Whether a path belongs to the evolving bilingual source corpus.
#[must_use]
pub fn is_translation_scope_file(file: &str) -> bool {
    static README: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"(?i)(?:^|/)readme(?:\.md|\.zh\.md|\.i18n\.yaml)$")
            .expect("static README artifact regex")
    });
    static CONTRIBUTING: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"(?i)^contributing(?:\.md|\.zh\.md|\.i18n\.yaml)$")
            .expect("static CONTRIBUTING artifact regex")
    });
    !file.starts_with(".agents/notes/archived/")
        && !translation_source_excluded(file)
        && (README.is_match(file)
            || CONTRIBUTING.is_match(file)
            || file.starts_with(".agents/notes/")
            || file.starts_with("docs/")
            || file.starts_with("python/"))
}

/// Parses the exclusions-only bilingual manifest.
///
/// # Errors
///
/// Returns JSON, top-level shape, obsolete-field, or exclusion-list errors.
pub fn parse_translation_pairing_manifest(
    content: &str,
) -> anyhow::Result<TranslationPairingManifest> {
    let value = serde_json::from_str::<Value>(content)?;
    let Some(record) = value.as_object() else {
        anyhow::bail!("translation-pairing.manifest.json: expected an object");
    };
    let unsupported = record
        .keys()
        .filter(|field| field.as_str() != "excluded")
        .cloned()
        .collect::<Vec<_>>();
    if !unsupported.is_empty() {
        anyhow::bail!(
            "translation-pairing.manifest.json: unsupported field(s): {}; every in-scope document is required",
            unsupported.join(", ")
        );
    }
    let Some(excluded) = record.get("excluded").and_then(Value::as_array) else {
        anyhow::bail!("translation-pairing.manifest.json: excluded must be an array of strings");
    };
    let excluded = excluded
        .iter()
        .map(Value::as_str)
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "translation-pairing.manifest.json: excluded must be an array of strings"
            )
        })?;
    Ok(TranslationPairingManifest {
        excluded: excluded.into_iter().map(str::to_owned).collect(),
    })
}

/// Normalizes any pair artifact or bare stem to its English anchor.
#[must_use]
pub fn pair_anchor_of_argument(argument: &str) -> String {
    let normalized = argument.replace('\\', "/");
    let normalized = normalized
        .strip_prefix("./")
        .unwrap_or(&normalized)
        .to_owned();
    for suffix in [".zh.md", ".i18n.yaml"] {
        if let Some(stem) = normalized.strip_suffix(suffix) {
            return format!("{stem}.md");
        }
    }
    if normalized.strip_suffix(".md").is_some() {
        normalized
    } else {
        format!("{normalized}.md")
    }
}

/// Parses and validates pairing command arguments.
///
/// # Errors
///
/// Returns unknown or conflicting flag/path diagnostics.
pub fn parse_translation_pairing_cli_args(
    arguments: &[String],
) -> anyhow::Result<TranslationPairingCliRequest> {
    let flags = arguments
        .iter()
        .filter(|argument| argument.starts_with("--"))
        .cloned()
        .collect::<Vec<_>>();
    let mut anchors = arguments
        .iter()
        .filter(|argument| !argument.starts_with("--"))
        .map(|argument| pair_anchor_of_argument(argument))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    anchors.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
    let unknown = flags
        .iter()
        .filter(|flag| !matches!(flag.as_str(), "--list" | "--write" | "--all" | "--cached"))
        .cloned()
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        anyhow::bail!("unknown flag(s): {}", unknown.join(", "));
    }
    let list = flags.iter().any(|flag| flag == "--list");
    let write = flags.iter().any(|flag| flag == "--write");
    let all = flags.iter().any(|flag| flag == "--all");
    let cached = flags.iter().any(|flag| flag == "--cached");
    if list && (write || all || cached || !anchors.is_empty()) {
        anyhow::bail!("--list reports the whole corpus and takes no other flags or paths");
    }
    if all && !write {
        anyhow::bail!("--all only applies to --write");
    }
    if cached && write {
        anyhow::bail!("--cached is a read-only index check and cannot be combined with --write");
    }
    if cached && anchors.is_empty() {
        anyhow::bail!("--cached requires the staged pair paths to check");
    }
    if write {
        if !anchors.is_empty() && all {
            anyhow::bail!("--write takes either pair paths or --all, not both");
        }
        if anchors.is_empty() && !all {
            anyhow::bail!(
                "--write requires the pair(s) you confirmed (any file of a pair), or --all to re-record every complete pair; recording pairs you did not review blesses unconfirmed content"
            );
        }
        return Ok(TranslationPairingCliRequest {
            input: TranslationPairingInput::Worktree,
            mode: TranslationPairingMode::Write,
            scope: if all {
                TranslationPairingScope::Corpus
            } else {
                TranslationPairingScope::Pairs
            },
            anchors,
        });
    }
    if list {
        return Ok(TranslationPairingCliRequest {
            input: TranslationPairingInput::Worktree,
            mode: TranslationPairingMode::List,
            scope: TranslationPairingScope::Corpus,
            anchors: Vec::new(),
        });
    }
    Ok(TranslationPairingCliRequest {
        input: if cached {
            TranslationPairingInput::Index
        } else {
            TranslationPairingInput::Worktree
        },
        mode: TranslationPairingMode::Check,
        scope: if anchors.is_empty() {
            TranslationPairingScope::Corpus
        } else {
            TranslationPairingScope::Pairs
        },
        anchors,
    })
}

/// Parses Markdown with the repository's GFM grammar.
///
/// # Errors
///
/// Returns Markdown parser diagnostics.
pub fn parse_translation_markdown(content: &str) -> Result<Node, String> {
    parse_markdown(content)
}

/// Accepted relative and public-repository links to one counterpart.
#[must_use]
pub fn language_switcher_targets(counterpart: &str) -> Vec<String> {
    let basename = std::path::Path::new(counterpart)
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or(counterpart);
    vec![
        basename.to_owned(),
        format!("https://github.com/deepseek-ai/seekdeep-harness/blob/master/{counterpart}"),
    ]
}

/// Whether a Markdown tree links to any accepted target.
#[must_use]
pub fn links_to(tree: &Node, targets: &[String]) -> bool {
    let accepted = targets.iter().map(String::as_str).collect::<HashSet<_>>();
    let mut found = false;
    visit_markdown(tree, &mut |node| {
        if let Node::Link(link) = node
            && accepted.contains(link.url.as_str())
        {
            found = true;
        }
        true
    });
    found
}

/// Whether an English source must carry a reciprocal language switcher.
#[must_use]
pub fn requires_source_language_switcher(source: &str) -> bool {
    !GENERATED_ENGLISH_SOURCES.contains(&source)
}

/// Collects the ordered structural signature, excluding accepted switchers.
#[must_use]
pub fn translation_structure_signature(
    tree: &Node,
    switcher_targets: &[String],
) -> TranslationStructureSignature {
    let accepted = switcher_targets
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let mut signature = TranslationStructureSignature::default();
    visit_markdown(tree, &mut |node| {
        match node {
            Node::Heading(heading) => signature.headings.push(heading.depth),
            Node::Code(code) => signature.code.push(format!(
                "```{}{}\n{}",
                code.lang.as_deref().unwrap_or_default(),
                code.meta
                    .as_ref()
                    .filter(|meta| !meta.is_empty())
                    .map_or_else(String::new, |meta| format!(" {meta}")),
                code.value
            )),
            Node::Table(table) => signature.tables.push(format!(
                "{}x{}",
                table.children.len(),
                table
                    .children
                    .first()
                    .and_then(Node::children)
                    .map_or(0, Vec::len)
            )),
            Node::List(list) => signature.lists.push(if list.ordered {
                format!(
                    "ordered:start={}:items={}",
                    list.start.unwrap_or(1),
                    list.children.len()
                )
            } else {
                format!("bullet:items={}", list.children.len())
            }),
            Node::Link(link) if !accepted.contains(link.url.as_str()) => {
                signature.links.push(link.url.clone());
            }
            _ => {}
        }
        true
    });
    signature
}

/// Returns the first divergence for each structural signature field.
#[must_use]
pub fn translation_structure_diff(
    source: &TranslationStructureSignature,
    zh: &TranslationStructureSignature,
) -> Vec<String> {
    let mut output = Vec::new();
    compare_field(
        "heading (depth)",
        &source.headings,
        &zh.headings,
        &mut output,
    );
    compare_field("code block", &source.code, &zh.code, &mut output);
    compare_field(
        "table (row x column count)",
        &source.tables,
        &zh.tables,
        &mut output,
    );
    compare_field(
        "list (kind, start, item count)",
        &source.lists,
        &zh.lists,
        &mut output,
    );
    compare_field("link target", &source.links, &zh.links, &mut output);
    output
}

fn compare_field<T: PartialEq + serde::Serialize>(
    field: &str,
    source: &[T],
    zh: &[T],
    output: &mut Vec<String>,
) {
    let length = source.len().max(zh.len());
    for index in 0..length {
        if source.get(index) != zh.get(index) {
            output.push(format!(
                "{field} #{} diverges between the pair: {} vs {}",
                index + 1,
                show(source.get(index)),
                show(zh.get(index))
            ));
            break;
        }
    }
}

fn show<T: serde::Serialize>(value: Option<&T>) -> String {
    let Some(value) = value else {
        return "nothing".to_owned();
    };
    let text = serde_json::to_string(value).unwrap_or_else(|_| "null".to_owned());
    let units = text.encode_utf16().collect::<Vec<_>>();
    if units.len() <= 72 {
        text
    } else {
        format!("{}…", String::from_utf16_lossy(&units[..72]))
    }
}

fn translation_source_excluded(file: &str) -> bool {
    const DIRECTORIES: &[&str] = &[
        "node_modules",
        "lib",
        ".pnpm-store",
        ".cache",
        "coverage",
        ".sessions",
        ".storages",
        "tmp",
        "dist-exe",
        "__pycache__",
        ".pytest_cache",
        ".artifacts",
        "vendor",
        "target",
    ];
    file.split('/').any(|segment| {
        DIRECTORIES.contains(&segment)
            || segment.starts_with(".doc-typecheck-")
            || segment.starts_with(".node-next-types-")
    }) || file.starts_with("apps/web/dist/")
        || file.starts_with(
            "python/sdk-runtime/src/deepseek_harness_runtime/runtime/seekdeep-jsonrpc-agent-",
        )
        || file.starts_with("python/sdk-runtime/src/deepseek_harness_runtime/runtime/node/")
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned())
}

const GENERATED_ENGLISH_SOURCES: &[&str] = &[
    "docs/agent-lifecycle.md",
    "docs/capability-seams.md",
    "docs/config-catalog.md",
    "docs/cordis-api/context.md",
    "docs/cordis-api/events.md",
    "docs/cordis-api/fiber.md",
    "docs/cordis-api/inherited.md",
    "docs/cordis-api/registry.md",
    "docs/cordis-api/service.md",
    "docs/event-producer-consumer.md",
    "docs/graph-atlas.md",
    "docs/module-graph.md",
    "docs/persistence-catalog.md",
    "docs/tool-catalog.md",
    "docs/tool-execution-pipeline.md",
];
