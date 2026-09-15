//! Unavailable public-repository reference detection across tracked files.

use std::{path::Path, process::Command, sync::LazyLock};

use regex::{Captures, Regex};
use unicode_normalization::UnicodeNormalization as _;

const ARCHIVED_AGENT_NOTE_PREFIX: &str = ".agents/notes/archived/";

static UNICODE_SEPARATOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\\u(0023|002d|002f)").expect("valid regex"));
static PERCENT_SEPARATOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)%(23|2d|2f)").expect("valid regex"));
static NUMERIC_ENTITY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)&#(?:(\d+)|x([\da-f]+));").expect("valid regex"));
static NAMED_ENTITY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)&(hyphen|num|sol);").expect("valid regex"));

/// One tracked reference to the unavailable repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnavailableRepositoryReference {
    /// Repository-relative file path.
    pub file: String,
    /// One-based source line.
    pub line: usize,
}

/// Locates unavailable-repository references in one active text file.
#[must_use]
pub fn find_unavailable_repository_references(
    file: &str,
    source: &str,
) -> Vec<UnavailableRepositoryReference> {
    if file.starts_with(ARCHIVED_AGENT_NOTE_PREFIX) {
        return Vec::new();
    }
    let unavailable = unavailable_repository();
    source
        .split('\n')
        .enumerate()
        .filter(|(_, line)| canonical_reference_text(line).contains(&unavailable))
        .map(|(index, _)| UnavailableRepositoryReference {
            file: file.to_owned(),
            line: index + 1,
        })
        .collect()
}

/// Scans tracked regular files and symlink targets below a repository root.
///
/// Binary files containing a NUL byte are ignored.
///
/// # Errors
///
/// Returns Git, filesystem metadata/readlink/read, or tracked-path failures.
pub fn scan_public_repository_links(
    repository_root: &Path,
) -> anyhow::Result<Vec<UnavailableRepositoryReference>> {
    let output = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(repository_root)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "git ls-files failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let mut references = Vec::new();
    for raw in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|raw| !raw.is_empty())
    {
        let file = String::from_utf8_lossy(raw).into_owned();
        let path = repository_root.join(&file);
        if !path.exists() {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&path)?;
        let source = if metadata.file_type().is_symlink() {
            std::fs::read_link(&path)?.to_string_lossy().into_owned()
        } else if metadata.is_file() {
            String::from_utf8_lossy(&std::fs::read(&path)?).into_owned()
        } else {
            continue;
        };
        if source.contains('\0') {
            continue;
        }
        references.extend(find_unavailable_repository_references(&file, &source));
    }
    Ok(references)
}

fn unavailable_repository() -> String {
    let owner = ["deepseek", "ai"].join("-");
    let name = ["deepseek", "harness", "sdk"].join("-");
    format!("{owner}/{name}")
}

fn canonical_reference_text(source: &str) -> String {
    let source = source.replace("\\/", "/");
    let source = UNICODE_SEPARATOR.replace_all(&source, |captures: &Captures<'_>| {
        separator_from_hex(&captures[1]).to_string()
    });
    let source = PERCENT_SEPARATOR.replace_all(&source, |captures: &Captures<'_>| {
        separator_from_hex(&captures[1]).to_string()
    });
    let source = NUMERIC_ENTITY.replace_all(&source, |captures: &Captures<'_>| {
        let value = captures.get(1).map_or_else(
            || u32::from_str_radix(captures.get(2).map_or("", |value| value.as_str()), 16),
            |value| value.as_str().parse(),
        );
        value.ok().and_then(char::from_u32).map_or_else(
            || captures[0].to_owned(),
            |character| {
                if matches!(character, '#' | '-' | '/') {
                    character.to_string()
                } else {
                    captures[0].to_owned()
                }
            },
        )
    });
    let source = NAMED_ENTITY.replace_all(&source, |captures: &Captures<'_>| {
        match captures[1].to_ascii_lowercase().as_str() {
            "hyphen" => "-".to_owned(),
            "sol" => "/".to_owned(),
            _ => captures[0].to_owned(),
        }
    });
    source.nfkc().collect::<String>().to_lowercase()
}

fn separator_from_hex(value: &str) -> char {
    u32::from_str_radix(value, 16)
        .ok()
        .and_then(char::from_u32)
        .unwrap_or('\u{fffd}')
}
