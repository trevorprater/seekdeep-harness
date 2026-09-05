//! Head/tail height-cap arithmetic shared by long block primitives.

/// Head/tail split metrics for a capped list.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeadTailCap {
    /// Rows beyond the cap; a non-positive value means nothing is hidden.
    pub hidden: f64,
    /// Whether the collapsed list renders only its head and tail.
    pub capped: bool,
    /// Head rows, calculated as `ceil(max_lines / 2)`.
    pub head_lines: f64,
    /// Tail rows left after the head.
    pub tail_lines: f64,
}

/// Computes the source-compatible split without normalizing JavaScript numbers.
#[must_use]
pub fn head_tail_cap(total: f64, max_lines: f64, expanded: bool) -> HeadTailCap {
    let hidden = total - max_lines;
    let head_lines = (max_lines / 2.0).ceil();
    HeadTailCap {
        hidden,
        capped: hidden > 0.0 && !expanded,
        head_lines,
        tail_lines: max_lines - head_lines,
    }
}
