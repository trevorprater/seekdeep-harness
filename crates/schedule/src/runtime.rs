//! Due-reminder selection and the disposable timer projection.
//!
//! The pure due-decision fold is ported here; the process-local
//! `ScheduleRuntime` timer service is ported separately.

use crate::{
    domain::{
        EveryReminder, FoldedSchedules, ScheduleLogError, format_utc_instant, parse_utc_instant,
        resolve_every_occurrence,
    },
    types::{OneShotScheduleRecord, ScheduleRecord},
};

/// Largest delay the runtime timers represent without clamping.
pub const MAX_TIMER_DELAY_MS: u64 = 2_147_483_647;

/// One due one-shot, one complete fixed-rate batch, or the next wake.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DueDecision {
    /// One due one-shot reminder.
    OneShot {
        /// The due record.
        record: OneShotScheduleRecord,
    },
    /// A complete fixed-rate batch.
    Every {
        /// One latest occurrence per overdue rule.
        reminders: Vec<EveryReminder>,
        /// Wall-clock decision time in epoch milliseconds.
        accepted_at: String,
    },
    /// No work is due; wait for the next target.
    Wait {
        /// Earliest future target, when any active record remains.
        target: Option<i64>,
    },
}

fn record_scheduled_at(record: &ScheduleRecord) -> &str {
    match record {
        ScheduleRecord::After(record) => &record.scheduled_at,
        ScheduleRecord::At(record) => &record.scheduled_at,
        ScheduleRecord::Every(record) => &record.scheduled_at,
    }
}

/// Selects one due one-shot, one complete fixed-rate batch, or the next wake.
///
/// # Errors
///
/// Returns a durable-log failure when a fixed-rate occurrence cannot be
/// resolved at the decision time.
pub fn due_decision(folded: &FoldedSchedules, now: i64) -> Result<DueDecision, ScheduleLogError> {
    let one_shot = folded
        .active
        .iter()
        .enumerate()
        .filter(|(_, record)| {
            !matches!(record, ScheduleRecord::Every(_))
                && parse_utc_instant(record_scheduled_at(record)) <= now
        })
        .min_by(|(left_index, left), (right_index, right)| {
            parse_utc_instant(record_scheduled_at(left))
                .cmp(&parse_utc_instant(record_scheduled_at(right)))
                .then_with(|| left_index.cmp(right_index))
        })
        .map(|(_, record)| record);
    if let Some(record) = one_shot {
        let record = match record {
            ScheduleRecord::After(record) => OneShotScheduleRecord::After(record.clone()),
            ScheduleRecord::At(record) => OneShotScheduleRecord::At(record.clone()),
            ScheduleRecord::Every(_) => unreachable!(),
        };
        return Ok(DueDecision::OneShot { record });
    }

    let mut every = folded
        .active
        .iter()
        .enumerate()
        .filter(|(_, record)| {
            matches!(record, ScheduleRecord::Every(_))
                && parse_utc_instant(record_scheduled_at(record)) <= now
        })
        .collect::<Vec<_>>();
    every.sort_by(|(left_index, left), (right_index, right)| {
        parse_utc_instant(record_scheduled_at(left))
            .cmp(&parse_utc_instant(record_scheduled_at(right)))
            .then_with(|| left_index.cmp(right_index))
    });
    if !every.is_empty() {
        let mut reminders = Vec::with_capacity(every.len());
        for (_, record) in every {
            let ScheduleRecord::Every(record) = record else {
                unreachable!();
            };
            reminders.push(EveryReminder {
                record: record.clone(),
                occurrence_at: resolve_every_occurrence(record, now)?.occurrence_at,
            });
        }
        return Ok(DueDecision::Every {
            reminders,
            accepted_at: format_utc_instant(now),
        });
    }

    let target = folded
        .active
        .iter()
        .map(|record| parse_utc_instant(record_scheduled_at(record)))
        .filter(|candidate| *candidate > now)
        .min();
    Ok(DueDecision::Wait { target })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AfterScheduleRecord, EveryScheduleRecord};

    fn after(id: &str, scheduled_at: &str) -> ScheduleRecord {
        ScheduleRecord::After(AfterScheduleRecord {
            id: crate::ScheduleId::new(id),
            prompt: "x".to_owned(),
            after_seconds: 30,
            scheduled_at: scheduled_at.to_owned(),
        })
    }

    fn every(id: &str, scheduled_at: &str) -> ScheduleRecord {
        ScheduleRecord::Every(EveryScheduleRecord {
            id: crate::ScheduleId::new(id),
            prompt: "x".to_owned(),
            every_seconds: 300,
            scheduled_at: scheduled_at.to_owned(),
        })
    }

    #[test]
    fn selects_one_shot_every_and_wait() {
        let folded = FoldedSchedules {
            active: vec![
                after("a", "2026-08-05T12:00:00.000Z"),
                every("b", "2026-08-05T12:05:00.000Z"),
            ],
            seen_ids: vec![],
        };
        let now = parse_utc_instant("2026-08-05T12:00:00.000Z");
        let decision = due_decision(&folded, now).expect("decision");
        match decision {
            DueDecision::OneShot { record } => match record {
                OneShotScheduleRecord::After(record) => assert_eq!(record.id.as_str(), "a"),
                OneShotScheduleRecord::At(_) => panic!("expected after"),
            },
            _ => panic!("expected one-shot"),
        }

        let now = parse_utc_instant("2026-08-05T12:06:00.000Z");
        let folded = FoldedSchedules {
            active: vec![after("done", "2026-08-05T12:00:00.000Z")],
            seen_ids: vec![],
        };
        // after is due; one-shot wins even if there is also every (omitted here)
        let decision = due_decision(&folded, now).expect("decision");
        assert!(matches!(decision, DueDecision::OneShot { .. }));

        let wait = FoldedSchedules {
            active: vec![after("future", "2026-08-06T12:00:00.000Z")],
            seen_ids: vec![],
        };
        let decision = due_decision(&wait, now).expect("wait");
        assert!(matches!(decision, DueDecision::Wait { target: Some(_) }));
    }

    #[test]
    fn every_batch_resolves_latest_occurrences() {
        let folded = FoldedSchedules {
            active: vec![every("e", "2026-08-05T12:05:00.000Z")],
            seen_ids: vec![],
        };
        let now = parse_utc_instant("2026-08-05T12:17:34.000Z");
        let decision = due_decision(&folded, now).expect("decision");
        match decision {
            DueDecision::Every { reminders, .. } => {
                assert_eq!(reminders.len(), 1);
                assert_eq!(reminders[0].occurrence_at, "2026-08-05T12:15:00.000Z");
            }
            _ => panic!("expected every batch"),
        }
    }
}
