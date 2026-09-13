//! Incremental full-text index for the trajectory ledger.

use std::rc::Rc;

use indexmap::{IndexMap, IndexSet};
use seekdeep_lossless_json::{JsonString, JsonValue};

use crate::{
    TrajectoryCell, TrajectoryCellKind, TrajectoryTurnModel, trajectory_preview_text,
    trajectory_record_id,
};

#[derive(Clone, Debug, PartialEq, Eq)]
struct SearchEntry {
    sources: Vec<JsonString>,
    text: JsonString,
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
                            let mut indexed = JsonString::join(&sources, "\n");
                            indexed.push_str("\n");
                            indexed.push_utf16(markdown_preview(cell).utf16_units());
                            indexed.push_str("\n");
                            indexed.push_utf16(result_preview(cell).utf16_units());
                            self.entries.insert(
                                id.clone(),
                                SearchEntry {
                                    sources,
                                    text: crate::text_value::lowercase(&indexed),
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
        self.search_json(&query.into())
    }

    /// Matches exact UTF-16 terms, preserving lone surrogates in the query.
    #[must_use]
    pub fn search_json(&self, query: &JsonString) -> Option<IndexSet<String>> {
        let query = crate::text_value::lowercase(&query.trim());
        let terms = query
            .utf16_units()
            .split(|unit| crate::text_value::is_space(*unit))
            .filter(|units| !units.is_empty())
            .map(JsonString::from_utf16)
            .collect::<Vec<_>>();
        if terms.is_empty() {
            return None;
        }
        Some(
            self.entries
                .iter()
                .filter(|(_, entry)| {
                    terms
                        .iter()
                        .all(|term| crate::text_value::contains(&entry.text, term))
                })
                .map(|(id, _)| id.clone())
                .collect(),
        )
    }
}

fn markdown_preview(cell: &TrajectoryCell) -> JsonString {
    let Some(markdown) = &cell.preview_markdown else {
        return JsonString::default();
    };
    let preview = trajectory_preview_text(markdown.clone()).unwrap_or_default();
    if cell.text.is_empty() {
        preview
    } else if preview.is_empty() {
        cell.text.clone()
    } else {
        JsonString::join(&[cell.text.clone(), preview], " · ")
    }
}

fn result_preview(cell: &TrajectoryCell) -> JsonString {
    cell.result_preview_markdown.as_ref().map_or_else(
        || cell.result.clone().unwrap_or_default(),
        |markdown| trajectory_preview_text(markdown.clone()).unwrap_or_default(),
    )
}

fn record_sources(turn: Option<u64>, group: &str, cell: &TrajectoryCell) -> Vec<JsonString> {
    let mut sources = vec![
        turn.map_or_else(|| "between turns".to_owned(), |turn| format!("turn {turn}"))
            .into(),
        group.into(),
        cell.kind.as_str().into(),
        if matches!(cell.kind, TrajectoryCellKind::Message) {
            JsonString::from("assistant")
        } else {
            JsonString::default()
        },
        cell.text.clone(),
        cell.preview_markdown.clone().unwrap_or_default(),
        cell.input_detail.clone().unwrap_or_default(),
        cell.output_detail.clone().unwrap_or_default(),
        cell.thinking_detail.clone().unwrap_or_default(),
        cell.schema_detail.clone().unwrap_or_default(),
        cell.result.clone().unwrap_or_default(),
        cell.result_preview_markdown.clone().unwrap_or_default(),
        cell.call_id.clone().unwrap_or_default().into(),
    ];
    for block in cell.source_blocks.iter().chain(&cell.output_blocks) {
        sources.extend([
            block.kind.clone().into(),
            block.content.clone(),
            block.call_id.clone().unwrap_or_default().into(),
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

fn searchable_json(value: Option<&JsonValue>) -> JsonString {
    value
        .map(|value| value.stringify().into())
        .unwrap_or_default()
}
