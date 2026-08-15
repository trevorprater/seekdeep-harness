//! Declarative plugin-tree configuration and patch layering.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One declarative plugin row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// Stable row identifier used by overlay patches.
    pub id: String,
    /// Rust plugin registry key.
    pub plugin: String,
    /// Plugin configuration.
    #[serde(default)]
    pub config: Value,
    /// Whether this row is inactive.
    #[serde(default)]
    pub disabled: bool,
    /// Nested rows.
    #[serde(default)]
    pub children: Vec<Entry>,
}

/// A full ordered configuration tree.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ConfigTree {
    /// Top-level rows.
    #[serde(default)]
    pub entries: Vec<Entry>,
}

/// Whole-row patch indexed by row id.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Patch {
    /// Replacements and insertions in declaration order.
    #[serde(flatten)]
    pub rows: IndexMap<String, Entry>,
}

impl ConfigTree {
    /// Applies a patch using the source harness's whole-row replacement rule.
    pub fn apply_patch(&mut self, patch: Patch) {
        for (id, mut replacement) in patch.rows {
            replacement.id.clone_from(&id);
            if !replace_entry(&mut self.entries, &id, &replacement) {
                self.entries.push(replacement);
            }
        }
    }
}

fn replace_entry(entries: &mut [Entry], id: &str, replacement: &Entry) -> bool {
    for entry in entries {
        if entry.id == id {
            entry.clone_from(replacement);
            return true;
        }
        if replace_entry(&mut entry.children, id, replacement) {
            return true;
        }
    }
    false
}
