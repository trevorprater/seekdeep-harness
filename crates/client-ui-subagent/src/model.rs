//! Pure subagent trigger, read-only, token, and duration policy.

use std::collections::{BTreeMap, BTreeSet};

use seekdeep_identity::SessionId;

/// Addressed subagent mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubagentMode {
    /// History-only task.
    OneShot,
    /// Parent-routable continuation.
    Continuable,
}

/// Why the composer is replaced with read-only copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubagentReadOnlyReason {
    /// One-shot tasks never accept follow-ups.
    OneShot,
    /// Continuable child stopped while its parent is unavailable.
    ParentUnavailable,
}

/// Minimal addressed-session facts consumed by the composer selector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AddressedSubagentState {
    /// Address mode.
    pub mode: SubagentMode,
    /// Whether the parent route is currently available.
    pub parent_available: bool,
    /// Whether the child itself is still running.
    pub running: bool,
}

/// Applies the exact read-only takeover rules for one addressed child.
#[must_use]
pub const fn select_read_only_subagent(
    state: Option<AddressedSubagentState>,
) -> Option<SubagentReadOnlyReason> {
    let Some(state) = state else {
        return None;
    };
    if matches!(state.mode, SubagentMode::OneShot) {
        return Some(SubagentReadOnlyReason::OneShot);
    }
    if state.parent_available || state.running {
        None
    } else {
        Some(SubagentReadOnlyReason::ParentUnavailable)
    }
}

/// Session-list fields consumed by reference and lineage policies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubagentListSummary {
    /// Stable Session identity.
    pub id: SessionId,
    /// Parent Session identity.
    pub parent_id: Option<SessionId>,
    /// Whether the durable origin is subagent.
    pub subagent_origin: bool,
    /// Current running bit.
    pub running: bool,
    /// User-facing label.
    pub display_title: String,
}

/// Descendant totals for one possible parent Session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SubagentDescendantSummary {
    /// All descendants connected by uninterrupted subagent-origin lineage.
    pub count: usize,
    /// Descendants whose exact summary is running.
    pub running_count: usize,
}

/// Indexes each subagent descendant under every uninterrupted subagent-origin ancestor.
#[must_use]
pub fn index_subagent_descendants(
    summaries: &[SubagentListSummary],
) -> BTreeMap<SessionId, SubagentDescendantSummary> {
    let by_id = summaries
        .iter()
        .map(|summary| (summary.id.clone(), summary))
        .collect::<BTreeMap<_, _>>();
    let mut indexed = BTreeMap::<SessionId, SubagentDescendantSummary>::new();
    for descendant in summaries.iter().filter(|summary| summary.subagent_origin) {
        let mut seen = BTreeSet::new();
        let mut current = Some(descendant);
        while let Some(summary) = current.filter(|summary| summary.subagent_origin) {
            let Some(parent) = &summary.parent_id else {
                break;
            };
            if !seen.insert(summary.id.clone()) {
                break;
            }
            let aggregate = indexed.entry(parent.clone()).or_default();
            aggregate.count += 1;
            if descendant.running {
                aggregate.running_count += 1;
            }
            current = by_id.get(parent).copied();
        }
    }
    indexed
}

/// Running direct-child labels matching one case-sensitive query in list order.
#[must_use]
pub fn child_labels(
    summaries: &[SubagentListSummary],
    parent_session_id: &SessionId,
    query: &str,
) -> Vec<String> {
    summaries
        .iter()
        .filter(|child| {
            child.parent_id.as_ref() == Some(parent_session_id)
                && child.running
                && child.display_title.contains(query)
        })
        .map(|child| child.display_title.clone())
        .collect()
}

/// Plain-text menu-pick insertion including its token-closing space.
#[must_use]
pub fn picked_reference(label: &str) -> String {
    format!("@{label} ")
}

/// Clipboard and current model serialization of one subagent reference.
#[must_use]
pub fn serialized_reference(label: &str) -> String {
    format!("@{label}")
}

fn js_round_positive(value: f64) -> f64 {
    (value + 0.5).floor()
}

fn number_string(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        value.to_string()
    }
}

fn as_f64(value: u64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
        value as f64
    }
}

/// Compact token count shared with the conversation stats strip.
#[must_use]
pub fn format_tokens(value: u64) -> String {
    if value < 1_000 {
        return value.to_string();
    }
    let (scaled, suffix) = if value < 1_000_000 {
        (as_f64(value) / 1_000.0, "K")
    } else {
        (as_f64(value) / 1_000_000.0, "M")
    };
    let rounded = if scaled >= 100.0 {
        js_round_positive(scaled)
    } else {
        js_round_positive(scaled * 10.0) / 10.0
    };
    format!("{}{suffix}", number_string(rounded))
}

/// Four disjoint provider token buckets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TokenUsage {
    /// Uncached input.
    pub uncached_input_tokens: u64,
    /// Generated output.
    pub output_tokens: u64,
    /// Cache reads.
    pub cache_read_tokens: u64,
    /// Cache writes.
    pub cache_write_tokens: u64,
}

/// One active timing interval retained in the Session projection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubagentActiveTiming {
    /// Interval start in epoch milliseconds.
    pub since: i128,
    /// Latest durable interval edge in epoch milliseconds.
    pub through: i128,
}

/// Settled and optionally active duration retained in the Session projection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubagentTiming {
    /// Duration already settled before the current active interval.
    pub settled_ms: i128,
    /// Current or most recently interrupted active interval.
    pub active: Option<SubagentActiveTiming>,
}

/// Returns the active duration sampled at `now` for running rows and at `through` otherwise.
#[must_use]
pub const fn activity_duration(
    timing: Option<SubagentTiming>,
    running: bool,
    now: i128,
) -> Option<i128> {
    let Some(timing) = timing else {
        return None;
    };
    let Some(active) = timing.active else {
        return Some(timing.settled_ms);
    };
    let end = if running { now } else { active.through };
    let elapsed = end.saturating_sub(active.since);
    Some(
        timing
            .settled_ms
            .saturating_add(if elapsed < 0 { 0 } else { elapsed }),
    )
}

/// Sums every durable provider-usage bucket.
#[must_use]
pub const fn token_total(usage: Option<TokenUsage>) -> Option<u64> {
    let Some(usage) = usage else {
        return None;
    };
    Some(
        usage.uncached_input_tokens
            + usage.output_tokens
            + usage.cache_read_tokens
            + usage.cache_write_tokens,
    )
}

/// Decomposed whole-second active duration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DurationParts {
    /// Residual seconds.
    pub seconds: u64,
    /// Residual minutes.
    pub minutes: u64,
    /// Residual hours.
    pub hours: u64,
    /// Whole days.
    pub days: u64,
    /// Whole total minutes.
    pub total_minutes: u64,
    /// Whole total hours.
    pub total_hours: u64,
}

/// Splits a millisecond duration after clamping negative/future values to zero.
#[must_use]
pub fn split_duration(milliseconds: i128) -> DurationParts {
    let total_seconds = u64::try_from(milliseconds.max(0) / 1_000).unwrap_or(u64::MAX);
    let total_minutes = total_seconds / 60;
    let total_hours = total_minutes / 60;
    DurationParts {
        seconds: total_seconds % 60,
        minutes: total_minutes % 60,
        hours: total_hours % 24,
        days: total_hours / 24,
        total_minutes,
        total_hours,
    }
}

/// Compact duration localization key and ordered numeric parameters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DurationFormat {
    /// Locale key suffix under `duration.`.
    pub key: &'static str,
    /// Ordered named values.
    pub values: Vec<(&'static str, DurationValue)>,
}

/// JavaScript-observable duration interpolation value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DurationValue {
    /// An unpadded numeric unit.
    Number(u64),
    /// A zero-padded textual unit.
    Text(String),
}

/// Chooses decreasing visual precision at larger scales.
#[must_use]
pub fn format_duration(milliseconds: i128) -> DurationFormat {
    let parts = split_duration(milliseconds);
    if parts.days >= 365 {
        let years = parts.days / 365;
        let months = (parts.days % 365) / 30;
        return DurationFormat {
            key: if months == 0 { "years" } else { "yearsMonths" },
            values: if months == 0 {
                vec![("years", DurationValue::Number(years))]
            } else {
                vec![
                    ("years", DurationValue::Number(years)),
                    ("months", DurationValue::Number(months)),
                ]
            },
        };
    }
    if parts.days >= 30 {
        let months = parts.days / 30;
        let days = parts.days % 30;
        return DurationFormat {
            key: if days == 0 { "months" } else { "monthsDays" },
            values: if days == 0 {
                vec![("months", DurationValue::Number(months))]
            } else {
                vec![
                    ("months", DurationValue::Number(months)),
                    ("days", DurationValue::Number(days)),
                ]
            },
        };
    }
    if parts.days > 0 {
        return DurationFormat {
            key: if parts.hours == 0 {
                "days"
            } else {
                "daysHours"
            },
            values: if parts.hours == 0 {
                vec![("days", DurationValue::Number(parts.days))]
            } else {
                vec![
                    ("days", DurationValue::Number(parts.days)),
                    ("hours", DurationValue::Number(parts.hours)),
                ]
            },
        };
    }
    if parts.total_hours > 0 {
        return DurationFormat {
            key: "hours",
            values: vec![
                ("hours", DurationValue::Number(parts.total_hours)),
                (
                    "minutes",
                    DurationValue::Text(format!("{:02}", parts.minutes)),
                ),
                (
                    "seconds",
                    DurationValue::Text(format!("{:02}", parts.seconds)),
                ),
            ],
        };
    }
    if parts.total_minutes > 0 {
        return DurationFormat {
            key: "minutes",
            values: vec![
                ("minutes", DurationValue::Number(parts.total_minutes)),
                (
                    "seconds",
                    DurationValue::Text(format!("{:02}", parts.seconds)),
                ),
            ],
        };
    }
    DurationFormat {
        key: "seconds",
        values: vec![("seconds", DurationValue::Number(parts.seconds))],
    }
}

/// Exact whole-second duration used by hover/accessibility copy.
#[must_use]
pub fn format_exact_duration(milliseconds: i128) -> DurationFormat {
    let parts = split_duration(milliseconds);
    if parts.days == 0 {
        return format_duration(milliseconds);
    }
    DurationFormat {
        key: "exactDays",
        values: vec![
            ("days", DurationValue::Number(parts.days)),
            ("hours", DurationValue::Text(format!("{:02}", parts.hours))),
            (
                "minutes",
                DurationValue::Text(format!("{:02}", parts.minutes)),
            ),
            (
                "seconds",
                DurationValue::Text(format!("{:02}", parts.seconds)),
            ),
        ],
    }
}
