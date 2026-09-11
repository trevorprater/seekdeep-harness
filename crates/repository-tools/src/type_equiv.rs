//! Structural and `JSDoc` equivalence of manifest-backed TypeScript declarations.

use std::{
    collections::{HashMap, HashSet},
    fmt::Write as _,
    path::Path,
    sync::LazyLock,
};

use indexmap::IndexMap;
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::{
    doc_typecheck::{MARKDOWN_GLOBS, markdown_files},
    markdown_util::markdown_fences,
    paired_markdown_derivatives::partition_paired_markdown_derivatives,
    ts_project::{RepositoryCompiler, RepositoryDeclaration},
};

/// One source declaration associated with a primary Markdown fence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    /// Repository-relative Markdown path.
    pub doc: String,
    /// Named top-level interface, type, class, or enum.
    pub symbol: String,
    /// Repository-relative source declaration file.
    pub source: String,
    /// `public-api` selects the public class projection; absent means complete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection: Option<String>,
}

/// The source verifier's manifest schema.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeEquivManifest {
    /// Ordered declaration-to-document associations.
    pub entries: Vec<ManifestEntry>,
}

/// One parsed declaration-equivalence fence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EquivBlock {
    /// Repository-relative Markdown path.
    pub doc: String,
    /// One-based line of the opening fence.
    pub line: usize,
    /// Name parsed from the first supported declaration.
    pub symbol: String,
    /// Authored declaration text.
    pub code: String,
    /// Selected projection, absent for a complete declaration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection: Option<String>,
}

/// Complete verification results in source ordering.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeEquivReport {
    /// Successfully matched primary entries.
    pub verified: usize,
    /// Number of primary declaration blocks, including duplicate blocks.
    pub primary_blocks: usize,
    /// Number of documents containing primary blocks.
    pub primary_documents: usize,
    /// Number of byte-identical paired derivative blocks.
    pub derivatives: usize,
    /// Manifest, correspondence, or declaration drift errors.
    pub errors: Vec<String>,
}

impl TypeEquivReport {
    /// Whether every primary block has exactly one matching source entry.
    pub fn passed(&self) -> bool {
        self.errors.is_empty()
    }

    /// Source-compatible output for stdout on success or stderr on failure.
    pub fn render(&self) -> String {
        if self.passed() {
            return format!(
                "verify-type-equiv: {} type-equiv block(s) match source structure and JSDoc (1:1 with manifest); {} paired derivative(s).\n",
                self.verified, self.derivatives
            );
        }
        let mut output = "verify-type-equiv: type-equiv verification failed:\n".to_owned();
        for error in &self.errors {
            let _ = writeln!(output, "  {error}");
        }
        let _ = writeln!(
            output,
            "\n(checked {} primary block(s) across {} doc(s), {} paired derivative(s); manifest at scripts/type-equiv.manifest.json)",
            self.primary_blocks, self.primary_documents, self.derivatives
        );
        output
    }
}

/// Extracts supported declaration fences and rejects invalid or unclosed ones.
///
/// # Errors
/// Returns source-compatible fence errors and compiler parser failures.
pub fn extract_equiv_blocks(
    compiler: &mut RepositoryCompiler,
    doc: &str,
    source: &str,
) -> anyhow::Result<Vec<EquivBlock>> {
    let mut blocks = Vec::new();
    for fence in markdown_fences(source).map_err(anyhow::Error::msg)? {
        if fence.info == "ts type-equiv public-api" {
            anyhow::bail!(
                "verify-type-equiv: {doc}:{} — use the concise `ts public-api` fence",
                fence.line
            );
        }
        if fence.info != "ts type-equiv" && fence.info != "ts public-api" {
            continue;
        }
        if !fence.closed {
            anyhow::bail!(
                "verify-type-equiv: {doc}:{} — unterminated type-equivalence fence (missing closing ```)",
                fence.line
            );
        }
        let symbol = compiler.block_symbol(&fence.code)?.ok_or_else(|| anyhow::anyhow!("verify-type-equiv: {doc}:{} — type-equiv block has no parseable interface/type/class declaration", fence.line))?;
        blocks.push(EquivBlock {
            doc: doc.to_owned(),
            line: fence.line,
            symbol,
            code: fence.code,
            projection: (fence.info == "ts public-api").then(|| "public-api".to_owned()),
        });
    }
    Ok(blocks)
}

/// Removes nonstructural comments and normalizes ECMAScript whitespace.
pub fn normalize_structure(code: &str) -> String {
    static BLOCK: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)/\*.*?\*/").expect("static block comment regex"));
    static LINE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?m)(^|[^:])//[^\r\n\x{2028}\x{2029}]*").expect("static line comment regex")
    });
    let code = BLOCK.replace_all(code, "");
    let code = LINE.replace_all(&code, "$1");
    collapse_spaces(&code).trim_matches(is_js_space).to_owned()
}

/// Extracts every original `JSDoc` in order, with whitespace normalized.
pub fn normalize_jsdoc(code: &str) -> Vec<String> {
    static DOC: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)/\*\*.*?\*/").expect("static JSDoc regex"));
    DOC.find_iter(code)
        .map(|comment| {
            collapse_spaces(comment.as_str())
                .trim_matches(is_js_space)
                .to_owned()
        })
        .collect()
}

/// Removes a leading source-only export/default modifier.
pub fn strip_export(code: &str) -> String {
    let Some(rest) = code.strip_prefix("export") else {
        return code.to_owned();
    };
    if !rest.starts_with(is_js_space) {
        return code.to_owned();
    }
    let rest = rest.trim_start_matches(is_js_space);
    if let Some(default) = rest.strip_prefix("default")
        && default.starts_with(is_js_space)
    {
        default.trim_start_matches(is_js_space).to_owned()
    } else {
        rest.to_owned()
    }
}

/// Runs the full Markdown/manifest/source correspondence and drift checks.
///
/// # Errors
/// Returns malformed manifest, filesystem, fence, or compiler errors.
pub fn verify_type_equiv(root: &Path) -> anyhow::Result<TypeEquivReport> {
    verify_type_equiv_with_compiler(root, None)
}

/// Runs the full gate with an explicitly supplied compiler for fixture projects.
///
/// # Errors
/// Returns malformed manifest, filesystem, fence, or compiler errors.
pub fn verify_type_equiv_with_compiler(
    root: &Path,
    library: Option<&Path>,
) -> anyhow::Result<TypeEquivReport> {
    let manifest = serde_json::from_str::<TypeEquivManifest>(&std::fs::read_to_string(
        root.join("scripts/type-equiv.manifest.json"),
    )?)?;
    let mut compiler =
        library.map_or_else(|| RepositoryCompiler::new(root), RepositoryCompiler::load)?;
    let mut documents = markdown_files(root)?;
    documents.dedup();
    let mut blocks = Vec::new();
    for document in &documents {
        blocks.extend(extract_equiv_blocks(
            &mut compiler,
            document,
            &std::fs::read_to_string(root.join(document))?,
        )?);
    }
    verify_extracted(root, &manifest.entries, &documents, &blocks, &mut compiler)
}

/// Verifies an extracted scope while sharing one parser across source files.
///
/// # Errors
/// Returns unreadable source files or compiler projection errors.
pub fn verify_extracted(
    root: &Path,
    entries: &[ManifestEntry],
    documents: &[String],
    blocks: &[EquivBlock],
    compiler: &mut RepositoryCompiler,
) -> anyhow::Result<TypeEquivReport> {
    let partition = partition_paired_markdown_derivatives(
        blocks,
        |block| block.doc.clone(),
        |block| {
            format!(
                "{}\0{}",
                projection_name(block.projection.as_deref()),
                block.code
            )
        },
    );
    let blocks = partition.primary;
    let mut report = TypeEquivReport {
        primary_blocks: blocks.len(),
        primary_documents: blocks
            .iter()
            .map(|block| &block.doc)
            .collect::<HashSet<_>>()
            .len(),
        derivatives: partition.derivatives.len(),
        ..TypeEquivReport::default()
    };
    let mut seen_documents = HashSet::new();
    for entry in entries {
        if !seen_documents.insert(&entry.doc) {
            continue;
        }
        if !root.join(&entry.doc).exists() {
            report.errors.push(format!(
                "manifest references {}, which does not exist",
                entry.doc
            ));
        } else if !documents.contains(&entry.doc) {
            report.errors.push(format!(
                "manifest references {}, which is outside the scanned markdown scope ({})",
                entry.doc,
                MARKDOWN_GLOBS.join(", ")
            ));
        }
    }
    let mut block_by_key = IndexMap::<String, &EquivBlock>::new();
    for block in &blocks {
        let key = key(&block.doc, &block.symbol, block.projection.as_deref());
        if let Some(prior) = block_by_key.get(&key) {
            report.errors.push(format!(
                "duplicate type-equiv block for {} in {} (lines {} and {})",
                block.symbol, block.doc, prior.line, block.line
            ));
        } else {
            block_by_key.insert(key, block);
        }
    }
    let mut entry_by_key = IndexMap::<String, &ManifestEntry>::new();
    for entry in entries {
        let key = key(&entry.doc, &entry.symbol, entry.projection.as_deref());
        if entry_by_key.contains_key(&key) {
            report.errors.push(format!(
                "duplicate manifest entry for {} in {}",
                entry.symbol, entry.doc
            ));
        } else {
            entry_by_key.insert(key, entry);
        }
    }
    for block in &blocks {
        if !entry_by_key.contains_key(&key(&block.doc, &block.symbol, block.projection.as_deref()))
        {
            report.errors.push(format!("type-equiv block {} ({}:{}) has no manifest entry — add one to scripts/type-equiv.manifest.json", block.symbol, block.doc, block.line));
        }
    }
    for entry in entries {
        if !block_by_key.contains_key(&key(&entry.doc, &entry.symbol, entry.projection.as_deref()))
        {
            report.errors.push(format!("manifest entry {} ({}) has no matching type-equiv block — remove it or add the block", entry.symbol, entry.doc));
        }
    }
    verify_sources(root, entries, &block_by_key, compiler, &mut report)?;
    Ok(report)
}

fn verify_sources(
    root: &Path,
    entries: &[ManifestEntry],
    blocks: &IndexMap<String, &EquivBlock>,
    compiler: &mut RepositoryCompiler,
    report: &mut TypeEquivReport,
) -> anyhow::Result<()> {
    let mut sources = HashMap::<String, Vec<RepositoryDeclaration>>::new();
    for entry in entries {
        let Some(block) = blocks.get(&key(&entry.doc, &entry.symbol, entry.projection.as_deref()))
        else {
            continue;
        };
        let declarations = match sources.entry(entry.source.clone()) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(vacant) => {
                let path = root.join(&entry.source);
                let code = std::fs::read_to_string(&path)?;
                vacant.insert(compiler.declarations(&path.to_string_lossy(), &code)?)
            }
        };
        let source = declarations.iter().find_map(|declaration| {
            if declaration.symbol != entry.symbol {
                return None;
            }
            if entry.projection.as_deref() == Some("public-api") {
                declaration.public_api.as_deref()
            } else {
                Some(declaration.declaration.as_str())
            }
        });
        let Some(source) = source else {
            report.errors.push(format!(
                "symbol {} not found in {} (manifest entry for {})",
                entry.symbol, entry.source, entry.doc
            ));
            continue;
        };
        let document = strip_export(&block.code);
        let source_structure = normalize_structure(source);
        let doc_structure = normalize_structure(&document);
        let source_jsdoc = normalize_jsdoc(source);
        let doc_jsdoc = normalize_jsdoc(&document);
        if source_structure != doc_structure || source_jsdoc != doc_jsdoc {
            report.errors.push(format!("DRIFT: {}:{} — type-equiv block for {} does not match {}.\n    source structure: {source_structure}\n    doc structure:    {doc_structure}\n    source JSDoc:     {}\n    doc JSDoc:        {}", entry.doc, block.line, entry.symbol, entry.source, serde_json::to_string(&source_jsdoc)?, serde_json::to_string(&doc_jsdoc)?));
            continue;
        }
        report.verified += 1;
    }
    Ok(())
}

fn projection_name(projection: Option<&str>) -> &str {
    projection.unwrap_or("declaration")
}

fn key(doc: &str, symbol: &str, projection: Option<&str>) -> String {
    format!("{doc}::{symbol}::{}", projection_name(projection))
}

fn is_js_space(character: char) -> bool {
    matches!(character, '\u{0009}'..='\u{000D}' | ' ' | '\u{00A0}' | '\u{1680}' | '\u{2000}'..='\u{200A}' | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}')
}

fn collapse_spaces(code: &str) -> String {
    let mut result = String::with_capacity(code.len());
    let mut in_space = false;
    for character in code.chars() {
        if is_js_space(character) {
            if !in_space {
                result.push(' ');
                in_space = true;
            }
        } else {
            result.push(character);
            in_space = false;
        }
    }
    result
}
