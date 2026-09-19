//! Canonical bilingual pairing paths and consistency records.

use std::{collections::HashMap, path::Path};

use regex::Regex;

/// The three repository-relative files forming one bilingual pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranslationPairPaths {
    /// English document path.
    pub source: String,
    /// Simplified Chinese document path.
    pub zh: String,
    /// Generated consistency-record path.
    pub metadata: String,
}

/// The two Git blob hashes recorded for a bilingual pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranslationPairingRecord {
    /// English document blob hash.
    pub source_hash: String,
    /// Simplified Chinese document blob hash.
    pub zh_hash: String,
}

/// Derives a counterpart and record from an English Markdown path.
///
/// # Errors
///
/// Returns a source-compatible diagnostic for non-Markdown or Chinese paths.
pub fn translation_pair_paths(source: &str) -> anyhow::Result<TranslationPairPaths> {
    let Some(stem) = source.strip_suffix(".md") else {
        anyhow::bail!(
            "expected an English Markdown path, received {}",
            json_string(source)
        );
    };
    if source.strip_suffix(".zh.md").is_some() {
        anyhow::bail!(
            "expected an English Markdown path, received {}",
            json_string(source)
        );
    }
    Ok(TranslationPairPaths {
        source: source.to_owned(),
        zh: format!("{stem}.zh.md"),
        metadata: format!("{stem}.i18n.yaml"),
    })
}

/// Derives a pair from its bilingual consistency-record path.
///
/// # Errors
///
/// Returns a source-compatible diagnostic for non-record paths.
pub fn translation_pair_paths_from_metadata(
    metadata: &str,
) -> anyhow::Result<TranslationPairPaths> {
    let Some(stem) = metadata.strip_suffix(".i18n.yaml") else {
        anyhow::bail!(
            "expected a bilingual consistency-record path, received {}",
            json_string(metadata)
        );
    };
    translation_pair_paths(&format!("{stem}.md"))
}

/// Parses exactly two expected sibling hashes from a consistency record.
#[must_use]
pub fn parse_translation_pairing_record(
    content: &str,
    paths: &TranslationPairPaths,
) -> Option<TranslationPairingRecord> {
    static METADATA_LINE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"^([^:#]+\.md): ([0-9a-f]{40})$").expect("static pairing-record regex")
    });
    let mut hashes = HashMap::new();
    for line in content.split('\n') {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let captures = METADATA_LINE.captures(line)?;
        let name = captures.get(1)?.as_str();
        if hashes.contains_key(name) {
            return None;
        }
        hashes.insert(name.to_owned(), captures.get(2)?.as_str().to_owned());
    }
    let source_name = basename(&paths.source)?;
    let zh_name = basename(&paths.zh)?;
    let source_hash = hashes.get(source_name)?;
    let zh_hash = hashes.get(zh_name)?;
    (hashes.len() == 2).then(|| TranslationPairingRecord {
        source_hash: source_hash.clone(),
        zh_hash: zh_hash.clone(),
    })
}

/// Renders the canonical consistency record with one trailing newline.
#[must_use]
pub fn render_translation_pairing_record(
    paths: &TranslationPairPaths,
    record: &TranslationPairingRecord,
) -> String {
    let source_name = basename(&paths.source).unwrap_or(&paths.source);
    let zh_name = basename(&paths.zh).unwrap_or(&paths.zh);
    format!(
        "# Bilingual-pair consistency record (docs/i18n/README.md): the git blob hash of each\n\
# side as of the last confirmed-consistent state. Both languages carry equal authority;\n\
# after editing either side, bring the other along and re-record with:\n\
#   pnpm run verify-translation-pairing --write {}\n\
{}: {}\n\
{}: {}\n",
        paths.source, source_name, record.source_hash, zh_name, record.zh_hash
    )
}

fn basename(path: &str) -> Option<&str> {
    Path::new(path).file_name()?.to_str()
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned())
}
