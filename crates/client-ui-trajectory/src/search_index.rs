//! Incremental full-text index for the trajectory ledger.

use std::rc::Rc;

use indexmap::{IndexMap, IndexSet};

use crate::{
    TrajectoryCell, TrajectoryCellKind, TrajectoryTurnModel, trajectory_preview_text,
    trajectory_record_id,
};

#[derive(Clone, Debug, PartialEq, Eq)]
struct SearchEntry {
    sources: Vec<String>,
    text: String,
}

/// Session-view-local index that reparses Markdown only when one record changes.
#[derive(Debug, Default)]
pub struct TrajectorySearchIndex {
    entries: IndexMap<String, SearchEntry>,
    layouts: Option<Rc<Vec<Vec<TrajectoryTurnModel>>>>,
}

impl TrajectorySearchIndex {
    /// Creates an empty index.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Incrementally synchronizes finalized and optional streaming layout slices.
    ///
    /// Returns `false` only for the same outer layout object identity.
    pub fn update(&mut self, layouts: &Rc<Vec<Vec<TrajectoryTurnModel>>>) -> bool {
        if self
            .layouts
            .as_ref()
            .is_some_and(|current| Rc::ptr_eq(current, layouts))
        {
            return false;
        }
        self.layouts = Some(layouts.clone());
        let mut seen = IndexSet::new();
        for turns in layouts.iter() {
            for turn in turns {
                for group in &turn.groups {
                    for cell in &group.cells {
                        if cell.request_only == Some(true) {
                            continue;
                        }
                        let id = trajectory_record_id(cell);
                        let sources = record_sources(turn.turn, &group.title, cell);
                        if self
                            .entries
                            .get(&id)
                            .is_none_or(|previous| previous.sources != sources)
                        {
                            let mut indexed = sources.join("\n");
                            indexed.push('\n');
                            indexed.push_str(&markdown_preview(cell));
                            indexed.push('\n');
                            indexed.push_str(&result_preview(cell));
                            self.entries.insert(
                                id.clone(),
                                SearchEntry {
                                    sources,
                                    text: indexed.to_lowercase(),
                                },
                            );
                        }
                        seen.insert(id);
                    }
                }
            }
        }
        self.entries.retain(|id, _| seen.contains(id));
        true
    }

    /// Matches space-separated case-insensitive terms in insertion order.
    #[must_use]
    pub fn search(&self, query: &str) -> Option<IndexSet<String>> {
        let terms = query
            .trim()
            .to_lowercase()
            .split_whitespace()
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        if terms.is_empty() {
            return None;
        }
        Some(
            self.entries
                .iter()
                .filter(|(_, entry)| terms.iter().all(|term| entry.text.contains(term)))
                .map(|(id, _)| id.clone())
                .collect(),
        )
    }
}

fn markdown_preview(cell: &TrajectoryCell) -> String {
    let Some(markdown) = &cell.preview_markdown else {
        return String::new();
    };
    let preview = trajectory_preview_text(markdown).unwrap_or_default();
    if cell.text.is_empty() {
        preview
    } else if preview.is_empty() {
        cell.text.clone()
    } else {
        format!("{} · {preview}", cell.text)
    }
}

fn result_preview(cell: &TrajectoryCell) -> String {
    cell.result_preview_markdown.as_ref().map_or_else(
        || cell.result.clone().unwrap_or_default(),
        |markdown| trajectory_preview_text(markdown).unwrap_or_default(),
    )
}

fn record_sources(turn: Option<u64>, group: &str, cell: &TrajectoryCell) -> Vec<String> {
    let mut sources = vec![
        turn.map_or_else(|| "between turns".to_owned(), |turn| format!("turn {turn}")),
        group.to_owned(),
        cell.kind.as_str().to_owned(),
        if matches!(cell.kind, TrajectoryCellKind::Message) {
            "assistant".to_owned()
        } else {
            String::new()
        },
        cell.text.clone(),
        cell.preview_markdown.clone().unwrap_or_default(),
        cell.input_detail.clone().unwrap_or_default(),
        cell.output_detail.clone().unwrap_or_default(),
        cell.thinking_detail.clone().unwrap_or_default(),
        cell.schema_detail.clone().unwrap_or_default(),
        cell.result.clone().unwrap_or_default(),
        cell.result_preview_markdown.clone().unwrap_or_default(),
        cell.call_id.clone().unwrap_or_default(),
    ];
    for block in cell.source_blocks.iter().chain(&cell.output_blocks) {
        sources.extend([
            block.kind.clone(),
            block.content.clone(),
            block.call_id.clone().unwrap_or_default(),
            block.tool_name.clone().unwrap_or_default(),
            block.image_alt.clone().unwrap_or_default(),
        ]);
    }
    sources.extend([
        searchable_json(cell.message_source.as_ref()),
        searchable_json(cell.prompt_detail.as_ref()),
        searchable_json(cell.previous_prompt_detail.as_ref()),
    ]);
    sources
}

fn searchable_json(value: Option<&serde_json::Value>) -> String {
    value
        .and_then(|value| serde_json::to_string(value).ok())
        .unwrap_or_default()
}
