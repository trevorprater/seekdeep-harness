//! Pure projection from trajectory records to measurable virtual-ledger rows.

use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};

use crate::{TrajectoryCell, trajectory_record_id};

const CONTENT_ROW_HEIGHT: u16 = 30;
const COLLAPSED_SUMMARY_HEIGHT: u16 = 20;
const TERMINAL_BOUNDARY_HEIGHT: u16 = 9;
const ENCODE_URI_COMPONENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'$')
    .add(b'%')
    .add(b'&')
    .add(b'+')
    .add(b',')
    .add(b'/')
    .add(b':')
    .add(b';')
    .add(b'<')
    .add(b'=')
    .add(b'>')
    .add(b'?')
    .add(b'@')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

/// Synthetic collapsed-summary class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollapsedSummaryKind {
    /// Whole-turn summary.
    Turn,
    /// Assistant-group summary.
    Assistant,
}

impl CollapsedSummaryKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Turn => "turn",
            Self::Assistant => "assistant",
        }
    }
}

/// Minimal record shape consumed by virtual-row projection.
#[derive(Clone, Debug, PartialEq)]
pub struct VirtualizableTrajectoryRecord {
    /// Source record.
    pub cell: TrajectoryCell,
    /// Synthetic fold summary class.
    pub collapsed_summary_kind: Option<CollapsedSummaryKind>,
}

/// One logical record retained inside a measurable virtual row.
#[derive(Clone, Debug, PartialEq)]
pub struct TrajectoryVirtualRowEntry {
    /// Original logical position.
    pub logical_index: usize,
    /// Original record.
    pub record: VirtualizableTrajectoryRecord,
}

/// One virtualizer item, possibly carrying preceding zero-height boundaries.
#[derive(Clone, Debug, PartialEq)]
pub struct TrajectoryVirtualRow {
    /// Grouped logical records.
    pub entries: Vec<TrajectoryVirtualRowEntry>,
    /// Measurable rendered height in pixels.
    pub height: u16,
    /// DOM-safe stable row identity.
    pub key: String,
}

/// Derives the DOM-safe stable identity shared by React and the virtualizer.
#[must_use]
pub fn trajectory_virtual_record_key(record: &VirtualizableTrajectoryRecord) -> String {
    let record_id = trajectory_record_id(&record.cell);
    let identity = utf8_percent_encode(&record_id, ENCODE_URI_COMPONENT);
    match record.collapsed_summary_kind {
        Some(kind) => format!("{identity}\0summary\0{}", kind.as_str()),
        None => identity.to_string(),
    }
}

/// Attaches separator-only records to the next measurable content row.
#[must_use]
pub fn group_trajectory_virtual_rows(
    records: &[VirtualizableTrajectoryRecord],
) -> Vec<TrajectoryVirtualRow> {
    let mut rows = Vec::new();
    let mut pending = Vec::new();
    for (logical_index, record) in records.iter().cloned().enumerate() {
        let entry = TrajectoryVirtualRowEntry {
            logical_index,
            record,
        };
        if entry.record.cell.request_only == Some(true) {
            pending.push(entry);
            continue;
        }
        let key = trajectory_virtual_record_key(&entry.record);
        let height = if entry.record.collapsed_summary_kind.is_some() {
            COLLAPSED_SUMMARY_HEIGHT
        } else {
            CONTENT_ROW_HEIGHT
        };
        pending.push(entry);
        rows.push(TrajectoryVirtualRow {
            entries: std::mem::take(&mut pending),
            height,
            key,
        });
    }
    if !pending.is_empty() {
        let key = pending
            .iter()
            .map(|candidate| trajectory_virtual_record_key(&candidate.record))
            .collect::<Vec<_>>()
            .join("|");
        rows.push(TrajectoryVirtualRow {
            entries: pending,
            height: TERMINAL_BOUNDARY_HEIGHT,
            key,
        });
    }
    rows
}
