//! Standing-document word ceilings and list-mode usage rows.

use std::{path::Path, sync::LazyLock};

use indexmap::IndexMap;
use regex::Regex;
use serde_json::Value;

static WHITESPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("valid regex"));

/// Document-budget inspection result in manifest order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentBudgetReport {
    /// Usage rows rendered by `--list`.
    pub rows: Vec<String>,
    /// Invalid ceiling, missing file, or over-budget diagnostics.
    pub failures: Vec<String>,
    /// Number of manifest entries.
    pub budgeted_documents: usize,
}

/// Inspects `scripts/doc-budgets.manifest.json` below a repository root.
///
/// # Errors
///
/// Returns manifest read/JSON/shape or budgeted-file read failures.
pub fn inspect_document_budgets(root: &Path) -> anyhow::Result<DocumentBudgetReport> {
    let value: Value = serde_json::from_slice(&std::fs::read(
        root.join("scripts/doc-budgets.manifest.json"),
    )?)?;
    let manifest = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("doc budget manifest must be a JSON object"))?;
    let manifest = manifest
        .iter()
        .map(|(path, ceiling)| (path.clone(), ceiling.clone()))
        .collect::<IndexMap<_, _>>();
    let mut rows = Vec::new();
    let mut failures = Vec::new();
    for (path, ceiling) in &manifest {
        let Some(ceiling) = positive_integer(ceiling) else {
            let printable = printable(ceiling);
            rows.push(format!("BAD   {:>6} / {printable:<6} {path}", "—"));
            failures.push(format!(
                "{path}: ceiling must be a positive integer, got {printable}"
            ));
            continue;
        };
        let absolute = root.join(path);
        if !absolute.exists() {
            rows.push(format!("MISS  {:>6} / {ceiling:<6} {path}", "—"));
            failures.push(format!(
                "{path}: budgeted file does not exist (renamed or deleted? update scripts/doc-budgets.manifest.json in the same change)"
            ));
            continue;
        }
        let bytes = std::fs::read(absolute)?;
        let words = count_words(&String::from_utf8_lossy(&bytes));
        let status = if words <= ceiling { "ok  " } else { "OVER" };
        rows.push(format!("{status}  {words:>6} / {ceiling:<6} {path}"));
        if words > ceiling {
            failures.push(format!(
                "{path}: {words} words exceeds the {ceiling}-word ceiling — relocate or condense per docs/AGENTS.md (raising the ceiling requires justification in the PR)"
            ));
        }
    }
    Ok(DocumentBudgetReport {
        rows,
        failures,
        budgeted_documents: manifest.len(),
    })
}

fn count_words(text: &str) -> usize {
    WHITESPACE
        .split(text)
        .filter(|token| !token.is_empty())
        .count()
}

fn positive_integer(value: &Value) -> Option<usize> {
    value
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value > 0)
}

fn printable(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "undefined".into())
}
