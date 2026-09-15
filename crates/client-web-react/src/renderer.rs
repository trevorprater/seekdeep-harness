//! Pure slot-kind row/election decisions used by the live React renderer.

use std::collections::HashSet;

/// Minimal renderer projection of one stored entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderEntry {
    /// Stable renderer identity.
    pub identity: u64,
    /// Optional keyed/list cell id.
    pub cell: Option<String>,
    /// Explicit list order, defaulted before construction.
    pub order: i64,
    /// Whether this entry remains the active shadowing winner.
    pub winner: bool,
}

/// One list row: a winner or a dry-cell crash anchor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderListRow {
    /// Winning entry identity; absent for a dry cell.
    pub entry: Option<u64>,
    /// Cell id used by filters and dry-row keys.
    pub cell: Option<String>,
    /// Stable sort value.
    pub order: i64,
}

/// Builds source-order list rows, adds dry cells, sorts by order, and applies `only`.
#[must_use]
pub fn list_rows(entries: &[RenderEntry], only: Option<&str>) -> Vec<RenderListRow> {
    let mut rows = entries
        .iter()
        .filter(|entry| entry.winner)
        .map(|entry| RenderListRow {
            entry: Some(entry.identity),
            cell: entry.cell.clone(),
            order: entry.order,
        })
        .collect::<Vec<_>>();
    let mut cells = rows
        .iter()
        .map(|row| row.cell.clone())
        .collect::<HashSet<_>>();
    for entry in entries {
        if cells.insert(entry.cell.clone()) {
            rows.push(RenderListRow {
                entry: None,
                cell: entry.cell.clone(),
                order: entry.order,
            });
        }
    }
    rows.sort_by_key(|row| row.order);
    if let Some(only) = only {
        rows.retain(|row| row.cell.as_deref() == Some(only));
    }
    rows
}

/// Single/keyed dispatch outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShadowOutcome {
    /// Natural empty cell; render owner fallback.
    Fallback,
    /// Every occupant abdicated; render the crash face.
    Dead,
    /// Render this winning entry identity.
    Winner(u64),
}

/// Resolves a single slot or one keyed cell.
#[must_use]
pub fn shadow_outcome(entries: &[RenderEntry], cell: Option<&str>) -> ShadowOutcome {
    let matches = |entry: &&RenderEntry| match cell {
        Some(cell) => entry.cell.as_deref() == Some(cell),
        None => true,
    };
    if let Some(entry) = entries.iter().filter(matches).find(|entry| entry.winner) {
        return ShadowOutcome::Winner(entry.identity);
    }
    if entries.iter().any(|entry| match cell {
        Some(cell) => entry.cell.as_deref() == Some(cell),
        None => true,
    }) {
        ShadowOutcome::Dead
    } else {
        ShadowOutcome::Fallback
    }
}
