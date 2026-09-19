//! Background-job list models and Rust/WASM UI semantics.

use serde::{Deserialize, Serialize};

#[cfg(target_arch = "wasm32")]
mod wasm;

#[cfg(target_arch = "wasm32")]
pub use wasm::*;

/// Compiled background-job popover stylesheet.
pub const JOB_LIST_STYLES: &str = include_str!("../data/job-list-action.css");

/// Stable Host plugin identity.
pub const NAME: &str = "client-ui-jobs";
/// Dictionary namespace.
pub const JOB_NS: &str = "job";
/// Key, Simplified Chinese, and English values in source order.
pub const JOB_LOCALES: [(&str, &str, &str); 15] = [
    (
        "count.live.one",
        "{count} 个后台任务运行中",
        "{count} background job running",
    ),
    (
        "count.live.other",
        "{count} 个后台任务运行中",
        "{count} background jobs running",
    ),
    (
        "count.idle.one",
        "{count} 个后台任务",
        "{count} background job",
    ),
    (
        "count.idle.other",
        "{count} 个后台任务",
        "{count} background jobs",
    ),
    ("list.aria", "后台任务", "Background jobs"),
    ("status.running", "运行中", "running"),
    ("status.stopping", "正在停止", "stopping"),
    ("status.completed", "已完成", "completed"),
    ("status.killed", "已取消", "cancelled"),
    ("status.failed", "已失败", "failed"),
    ("duration.seconds", "{seconds}秒", "{seconds}s"),
    (
        "duration.minutes",
        "{minutes}分{seconds}秒",
        "{minutes}m {seconds}s",
    ),
    (
        "duration.hours",
        "{hours}小时{minutes}分",
        "{hours}h {minutes}m",
    ),
    (
        "duration.title.live",
        "已运行 {duration}",
        "Running for {duration}",
    ),
    ("duration.title.done", "耗时 {duration}", "Took {duration}"),
];

/// Closed background-job status vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    /// Process is running.
    Running,
    /// Stop has been requested.
    Stopping,
    /// Process exited successfully.
    Completed,
    /// Process was killed.
    Killed,
    /// Process failed.
    Failed,
}

/// State-dot semantic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobDotState {
    /// Ongoing work.
    Ongoing,
    /// Requested stop/kill attention state.
    Warning,
    /// Clean completion.
    Done,
    /// Failure.
    Error,
}

/// One durable background-job row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobView {
    /// Stable job id.
    pub id: String,
    /// Producer kind.
    pub kind: String,
    /// Human label.
    pub label: String,
    /// Current status.
    pub status: JobStatus,
    /// Start epoch milliseconds.
    pub started_at: i64,
    /// Optional settlement epoch milliseconds.
    pub finished_at: Option<i64>,
    /// Optional producer detail replacing the generic status label.
    pub detail: Option<String>,
}

/// Whether the registry still holds this job open and its duration ticks.
#[must_use]
pub const fn is_live(job: &JobView) -> bool {
    matches!(job.status, JobStatus::Running | JobStatus::Stopping)
}

/// Maps job status to the primitive dot state.
#[must_use]
pub const fn dot_state(status: JobStatus) -> JobDotState {
    match status {
        JobStatus::Running => JobDotState::Ongoing,
        JobStatus::Stopping | JobStatus::Killed => JobDotState::Warning,
        JobStatus::Completed => JobDotState::Done,
        JobStatus::Failed => JobDotState::Error,
    }
}

/// Locale key suffix for one status.
#[must_use]
pub const fn status_key(status: JobStatus) -> &'static str {
    match status {
        JobStatus::Running => "running",
        JobStatus::Stopping => "stopping",
        JobStatus::Completed => "completed",
        JobStatus::Killed => "killed",
        JobStatus::Failed => "failed",
    }
}

/// At-most-two-unit elapsed duration model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobDuration {
    /// Seconds only.
    Seconds(u64),
    /// Minutes and residual seconds.
    Minutes {
        /// Whole minutes.
        minutes: u64,
        /// Residual seconds.
        seconds: u64,
    },
    /// Total hours and residual minutes.
    Hours {
        /// Total hours.
        hours: u64,
        /// Residual minutes.
        minutes: u64,
    },
}

/// Formats elapsed milliseconds after whole-second flooring and zero clamping.
#[must_use]
pub fn format_duration(elapsed_ms: i128) -> JobDuration {
    let total = u64::try_from(elapsed_ms.max(0) / 1_000).unwrap_or(u64::MAX);
    let seconds = total % 60;
    let minutes = (total / 60) % 60;
    let hours = total / 3_600;
    if hours > 0 {
        JobDuration::Hours { hours, minutes }
    } else if minutes > 0 {
        JobDuration::Minutes { minutes, seconds }
    } else {
        JobDuration::Seconds(seconds)
    }
}

/// Live rows by start time, then settled rows newest-finish-first with start tiebreak.
#[must_use]
pub fn ordered(jobs: &[JobView]) -> Vec<JobView> {
    let mut jobs = jobs.to_vec();
    jobs.sort_by(|left, right| {
        let live_left = is_live(left);
        let live_right = is_live(right);
        live_right.cmp(&live_left).then_with(|| {
            if live_left {
                left.started_at.cmp(&right.started_at)
            } else {
                let left_finished = left.finished_at.unwrap_or(left.started_at);
                let right_finished = right.finished_at.unwrap_or(right.started_at);
                right_finished
                    .cmp(&left_finished)
                    .then_with(|| left.started_at.cmp(&right.started_at))
            }
        })
    });
    jobs
}

/// Trigger count mode and value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JobCount {
    /// Whether the count describes live jobs rather than total jobs.
    pub live: bool,
    /// Count value.
    pub count: usize,
}

/// Counts only live jobs, falling back to total jobs when none remain live.
#[must_use]
pub fn job_count(jobs: &[JobView]) -> JobCount {
    let live = jobs.iter().filter(|job| is_live(job)).count();
    JobCount {
        live: live > 0,
        count: if live > 0 { live } else { jobs.len() },
    }
}

/// Builds the no-op Host half of this pure Client plugin.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn host_plugin() -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new(NAME, std::iter::empty::<String>(), |_, _| {
        Box::pin(async { Ok(()) })
    })
}
