//! Pure closing-turn selection, mention resolution, and chip-fit policy.

use std::collections::BTreeSet;

use crate::DeliverablesTurnData;

/// At most six file chips compete for the one-line summary.
pub const SHOWN_LIMIT: usize = 6;

/// Files produced no later than one closing Assistant sequence, in first-seen order.
#[must_use]
pub fn produced_for_closing(
    data: Option<&DeliverablesTurnData>,
    closing_seq: Option<u64>,
) -> Vec<String> {
    let Some(data) = data else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    let mut seen = BTreeSet::new();
    for produced in &data.produced {
        if closing_seq.is_some_and(|closing| produced.seq > closing)
            || !seen.insert(produced.path.clone())
        {
            continue;
        }
        paths.push(produced.path.clone());
    }
    paths
}

/// Claims the turn-tail entry only when produced files exist.
#[must_use]
pub fn select_produced_files(
    data: Option<&DeliverablesTurnData>,
    closing_seq: u64,
) -> Option<Vec<String>> {
    let paths = produced_for_closing(data, Some(closing_seq));
    (!paths.is_empty()).then_some(paths)
}

/// Returns the trailing slash- or backslash-separated path segment.
#[must_use]
pub fn basename(path: &str) -> &str {
    path.rfind(['/', '\\']).map_or(path, |at| &path[at + 1..])
}

/// Resolved inline-code mention metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProducedFileMention {
    /// Full path opened by the UI action.
    pub path: String,
    /// Localized accessible action label.
    pub label: String,
    /// Full-path disambiguating tooltip.
    pub title: String,
}

/// Resolves an exact path or exactly-one matching basename without guessing.
#[must_use]
pub fn produced_file_mention(
    paths: &[String],
    value: &str,
    label: impl FnOnce(&str) -> String,
) -> Option<ProducedFileMention> {
    let path = paths
        .iter()
        .find(|path| path.as_str() == value)
        .or_else(|| {
            let mut matches = paths.iter().filter(|path| basename(path) == value);
            let first = matches.next()?;
            matches.next().is_none().then_some(first)
        })?;
    Some(ProducedFileMention {
        path: path.clone(),
        label: label(path),
        title: path.clone(),
    })
}

/// Selects the largest chip prefix whose exact localized remainder still fits.
#[must_use]
pub fn fit_produced_files(
    available: f64,
    gap: f64,
    chip_widths: &[f64],
    more_widths_by_shown: &[Option<f64>],
) -> usize {
    if available <= 0.0 {
        return chip_widths.len();
    }
    let mut prefix = Vec::with_capacity(chip_widths.len() + 1);
    prefix.push(0.0);
    let mut prefix_width = 0.0;
    for width in chip_widths {
        prefix_width += width;
        prefix.push(prefix_width);
    }
    let mut largest_fit = 0;
    for (shown, width) in prefix.into_iter().enumerate() {
        let more = more_widths_by_shown.get(shown).copied().flatten();
        let items = shown + usize::from(more.is_some());
        #[allow(clippy::cast_precision_loss)]
        let gaps = items.saturating_sub(1) as f64;
        let needed = width + more.unwrap_or_default() + gaps * gap;
        if needed <= available {
            largest_fit = shown;
        }
    }
    largest_fit
}
