//! Source-shaped unified prompt diff projection.

/// Semantic class for one rendered prompt-diff line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrajectoryPromptDiffKind {
    /// Hunk header or inter-hunk separator.
    Meta,
    /// Unchanged context.
    Context,
    /// Added line.
    Added,
    /// Removed line.
    Removed,
}

impl TrajectoryPromptDiffKind {
    /// Source CSS suffix.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Meta => "meta",
            Self::Context => "context",
            Self::Added => "added",
            Self::Removed => "removed",
        }
    }
}

/// One source-equivalent prompt-diff display line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrajectoryPromptDiffLine {
    /// Semantic line class.
    pub kind: TrajectoryPromptDiffKind,
    /// Prefixed unified-diff text.
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DiffOperation<'a> {
    Equal(&'a str),
    Insert(&'a str),
    Delete(&'a str),
}

impl DiffOperation<'_> {
    const fn consumes_old(self) -> bool {
        matches!(self, Self::Equal(_) | Self::Delete(_))
    }

    const fn consumes_new(self) -> bool {
        matches!(self, Self::Equal(_) | Self::Insert(_))
    }

    const fn changed(self) -> bool {
        !matches!(self, Self::Equal(_))
    }
}

/// Produces the source `structuredPatch` display shape with three context lines.
#[must_use]
pub fn trajectory_prompt_diff_lines(before: &str, after: &str) -> Vec<TrajectoryPromptDiffLine> {
    if before == after {
        return Vec::new();
    }
    let before = split_lines(before);
    let after = split_lines(after);
    let operations = myers_diff(&before, &after);
    let hunks = hunk_ranges(&operations, 3);
    let mut output = Vec::new();
    for (hunk_index, (start, end)) in hunks.into_iter().enumerate() {
        if hunk_index > 0 {
            output.push(TrajectoryPromptDiffLine {
                kind: TrajectoryPromptDiffKind::Meta,
                text: String::new(),
            });
        }
        let old_start = operations[..start]
            .iter()
            .filter(|operation| operation.consumes_old())
            .count()
            + 1;
        let new_start = operations[..start]
            .iter()
            .filter(|operation| operation.consumes_new())
            .count()
            + 1;
        let old_lines = operations[start..end]
            .iter()
            .filter(|operation| operation.consumes_old())
            .count();
        let new_lines = operations[start..end]
            .iter()
            .filter(|operation| operation.consumes_new())
            .count();
        output.push(TrajectoryPromptDiffLine {
            kind: TrajectoryPromptDiffKind::Meta,
            text: format!("@@ -{old_start},{old_lines} +{new_start},{new_lines} @@"),
        });
        output.extend(
            operations[start..end]
                .iter()
                .map(|operation| match operation {
                    DiffOperation::Equal(line) => TrajectoryPromptDiffLine {
                        kind: TrajectoryPromptDiffKind::Context,
                        text: format!(" {line}"),
                    },
                    DiffOperation::Insert(line) => TrajectoryPromptDiffLine {
                        kind: TrajectoryPromptDiffKind::Added,
                        text: format!("+{line}"),
                    },
                    DiffOperation::Delete(line) => TrajectoryPromptDiffLine {
                        kind: TrajectoryPromptDiffKind::Removed,
                        text: format!("-{line}"),
                    },
                }),
        );
    }
    output
}

fn split_lines(value: &str) -> Vec<&str> {
    value.split_terminator('\n').collect()
}

fn myers_diff<'a>(before: &[&'a str], after: &[&'a str]) -> Vec<DiffOperation<'a>> {
    let maximum = before.len() + after.len();
    let offset = isize::try_from(maximum).expect("line count fits isize") + 1;
    let mut frontier = vec![0_isize; maximum.saturating_mul(2) + 3];
    let mut trace = Vec::with_capacity(maximum + 1);
    for depth in 0..=maximum {
        trace.push(frontier.clone());
        let depth = isize::try_from(depth).expect("diff depth fits isize");
        let mut diagonal = -depth;
        while diagonal <= depth {
            let at = usize::try_from(offset + diagonal).expect("offset diagonal is non-negative");
            let mut old = if diagonal == -depth
                || (diagonal != depth && frontier[at - 1] < frontier[at + 1])
            {
                frontier[at + 1]
            } else {
                frontier[at - 1] + 1
            };
            let mut new = old - diagonal;
            while old < isize::try_from(before.len()).expect("line count fits isize")
                && new < isize::try_from(after.len()).expect("line count fits isize")
                && before[usize::try_from(old).expect("old line is non-negative")]
                    == after[usize::try_from(new).expect("new line is non-negative")]
            {
                old += 1;
                new += 1;
            }
            frontier[at] = old;
            if old == isize::try_from(before.len()).expect("line count fits isize")
                && new == isize::try_from(after.len()).expect("line count fits isize")
            {
                return backtrack(before, after, &trace, depth, offset);
            }
            diagonal += 2;
        }
    }
    unreachable!("a Myers path always exists");
}

fn backtrack<'a>(
    before: &[&'a str],
    after: &[&'a str],
    trace: &[Vec<isize>],
    depth: isize,
    offset: isize,
) -> Vec<DiffOperation<'a>> {
    let mut old = isize::try_from(before.len()).expect("line count fits isize");
    let mut new = isize::try_from(after.len()).expect("line count fits isize");
    let mut reversed = Vec::with_capacity(before.len() + after.len());
    for current_depth in (0..=depth).rev() {
        let frontier = &trace[usize::try_from(current_depth).expect("depth is non-negative")];
        let diagonal = old - new;
        let at = usize::try_from(offset + diagonal).expect("offset diagonal is non-negative");
        let previous_diagonal = if diagonal == -current_depth
            || (diagonal != current_depth && frontier[at - 1] < frontier[at + 1])
        {
            diagonal + 1
        } else {
            diagonal - 1
        };
        let previous_old = frontier
            [usize::try_from(offset + previous_diagonal).expect("previous diagonal is in range")];
        let previous_new = previous_old - previous_diagonal;
        while old > previous_old && new > previous_new {
            reversed.push(DiffOperation::Equal(
                before[usize::try_from(old - 1).expect("old line is non-negative")],
            ));
            old -= 1;
            new -= 1;
        }
        if current_depth == 0 {
            break;
        }
        if old == previous_old {
            reversed.push(DiffOperation::Insert(
                after[usize::try_from(new - 1).expect("new line is non-negative")],
            ));
            new -= 1;
        } else {
            reversed.push(DiffOperation::Delete(
                before[usize::try_from(old - 1).expect("old line is non-negative")],
            ));
            old -= 1;
        }
    }
    reversed.reverse();
    reversed
}

fn hunk_ranges(operations: &[DiffOperation<'_>], context: usize) -> Vec<(usize, usize)> {
    let changed = operations
        .iter()
        .enumerate()
        .filter_map(|(index, operation)| operation.changed().then_some(index))
        .collect::<Vec<_>>();
    let Some(&first) = changed.first() else {
        return Vec::new();
    };
    let mut clusters = Vec::new();
    let mut cluster_start = first;
    let mut cluster_end = first;
    for &next in &changed[1..] {
        let equal_gap = operations[cluster_end + 1..next]
            .iter()
            .filter(|operation| matches!(operation, DiffOperation::Equal(_)))
            .count();
        if equal_gap > context * 2 {
            clusters.push((cluster_start, cluster_end));
            cluster_start = next;
        }
        cluster_end = next;
    }
    clusters.push((cluster_start, cluster_end));
    clusters
        .into_iter()
        .map(|(start, end)| {
            let mut hunk_start = start;
            let mut remaining = context;
            while hunk_start > 0 && remaining > 0 {
                hunk_start -= 1;
                if matches!(operations[hunk_start], DiffOperation::Equal(_)) {
                    remaining -= 1;
                }
            }
            let mut hunk_end = end + 1;
            remaining = context;
            while hunk_end < operations.len() && remaining > 0 {
                if matches!(operations[hunk_end], DiffOperation::Equal(_)) {
                    remaining -= 1;
                }
                hunk_end += 1;
            }
            (hunk_start, hunk_end)
        })
        .collect()
}
