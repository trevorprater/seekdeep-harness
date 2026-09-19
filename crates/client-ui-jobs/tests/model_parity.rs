//! Job ordering, state, duration, count, and locale parity.

use seekdeep_client_ui_jobs::{
    JOB_LOCALES, JOB_NS, JobDotState, JobDuration, JobStatus, JobView, dot_state, format_duration,
    is_live, job_count, ordered, status_key,
};

const START: i64 = 1_700_000_000_000;

fn job(
    id: &str,
    label: &str,
    status: JobStatus,
    started_at: i64,
    finished_at: Option<i64>,
) -> JobView {
    JobView {
        id: id.to_owned(),
        kind: "bash".to_owned(),
        label: label.to_owned(),
        status,
        started_at,
        finished_at,
        detail: None,
    }
}

#[test]
fn statuses_counts_and_deterministic_order_match_the_source() {
    assert!(is_live(&job("a", "a", JobStatus::Running, START, None)));
    assert!(is_live(&job("b", "b", JobStatus::Stopping, START, None)));
    assert_eq!(dot_state(JobStatus::Running), JobDotState::Ongoing);
    assert_eq!(dot_state(JobStatus::Stopping), JobDotState::Warning);
    assert_eq!(dot_state(JobStatus::Completed), JobDotState::Done);
    assert_eq!(dot_state(JobStatus::Killed), JobDotState::Warning);
    assert_eq!(dot_state(JobStatus::Failed), JobDotState::Error);
    assert_eq!(status_key(JobStatus::Killed), "killed");

    let jobs = vec![
        job(
            "3",
            "old done",
            JobStatus::Completed,
            START,
            Some(START + 1_000),
        ),
        job(
            "4",
            "new done",
            JobStatus::Failed,
            START,
            Some(START + 9_000),
        ),
        job("2", "later live", JobStatus::Running, START + 5_000, None),
        job("1", "earlier live", JobStatus::Running, START, None),
    ];
    assert_eq!(
        ordered(&jobs)
            .iter()
            .map(|job| job.label.as_str())
            .collect::<Vec<_>>(),
        ["earlier live", "later live", "new done", "old done"]
    );
    assert_eq!(job_count(&jobs).count, 2);
    assert!(job_count(&jobs).live);
    let settled = vec![job("a", "a", JobStatus::Completed, START, None)];
    assert_eq!(job_count(&settled).count, 1);
    assert!(!job_count(&settled).live);

    let ties = vec![
        job("later", "later", JobStatus::Failed, START + 1_000, None),
        job("earlier", "earlier", JobStatus::Failed, START, None),
    ];
    assert_eq!(
        ordered(&ties)
            .iter()
            .map(|job| job.label.as_str())
            .collect::<Vec<_>>(),
        ["later", "earlier"]
    );
}

#[test]
fn duration_and_locale_buckets_are_exact() {
    assert_eq!(format_duration(-5_000), JobDuration::Seconds(0));
    assert_eq!(format_duration(9_999), JobDuration::Seconds(9));
    assert_eq!(
        format_duration(125_000),
        JobDuration::Minutes {
            minutes: 2,
            seconds: 5,
        }
    );
    assert_eq!(
        format_duration(7_380_000),
        JobDuration::Hours {
            hours: 2,
            minutes: 3,
        }
    );
    assert_eq!(JOB_NS, "job");
    assert_eq!(JOB_LOCALES.len(), 15);
    assert_eq!(JOB_LOCALES[8], ("status.killed", "已取消", "cancelled"));
    assert_eq!(
        JOB_LOCALES[14],
        ("duration.title.done", "耗时 {duration}", "Took {duration}")
    );
}
