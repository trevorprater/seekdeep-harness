//! Due-reminder selection and the disposable timer projection.

use std::{sync::Arc, time::Duration};

use seekdeep_agent::{AGENTS, Agent};
use seekdeep_cordis::Context;
use seekdeep_core::session::AppendOptions;
use seekdeep_llm::{ContentBlock, MessageSource, UserMessage};
use serde_json::json;
use tokio::{
    sync::Notify,
    task::{AbortHandle, JoinHandle},
};

use crate::{
    domain::{
        EveryReminder, FoldedSchedules, ScheduleLogError, fold_schedule_events, format_utc_instant,
        parse_utc_instant, render_every_reminder_batch_framing, render_reminder_framing,
        resolve_every_occurrence,
    },
    persistence::flush_schedule_persistence,
    transaction::run_schedule_transaction,
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

struct RuntimeInner {
    state: tokio::sync::Mutex<RuntimeState>,
    stop: Notify,
    disposed: tokio::sync::OnceCell<()>,
}

#[derive(Default)]
struct RuntimeState {
    run: Option<JoinHandle<()>>,
    idle_wait: Option<JoinHandle<()>>,
    timer: Option<AbortHandle>,
    requested: bool,
    stopping: bool,
    faulted: bool,
}

/// One process-local, disposable projection of an exact agent's durable schedules.
pub struct ScheduleRuntime {
    context: Context,
    agent: Arc<Agent>,
    inner: Arc<RuntimeInner>,
}

impl ScheduleRuntime {
    /// Constructs an inactive runtime; `start` begins the first preflight.
    #[must_use]
    pub fn new(context: &Context, agent: Arc<Agent>) -> Arc<Self> {
        Arc::new(Self {
            context: context.clone(),
            agent,
            inner: Arc::new(RuntimeInner {
                state: tokio::sync::Mutex::new(RuntimeState::default()),
                stop: Notify::new(),
                disposed: tokio::sync::OnceCell::new(),
            }),
        })
    }

    /// Begins the initial durability preflight and timer derivation.
    pub fn start(self: &Arc<Self>) {
        self.request_drive();
    }

    /// Recomputes the live projection after a committed mutation or idle transition.
    pub fn request_drive(self: &Arc<Self>) {
        let mut state = self.inner.state.blocking_lock();
        if state.stopping || state.faulted {
            return;
        }
        if let Some(timer) = state.timer.take() {
            timer.abort();
        }
        state.requested = true;
        if state.run.is_some() {
            return;
        }
        let registry = self.context.get(AGENTS);
        let this = self.clone();
        let handle = tokio::spawn(async move {
            if let Some(registry) = registry {
                let loop_this = this.clone();
                let _ = registry
                    .scope_without_initiator(async move {
                        run_requested_loop(loop_this).await;
                    })
                    .await;
            }
            let rearm = {
                let mut state = this.inner.state.lock().await;
                state.run = None;
                state.requested && !state.stopping && !state.faulted
            };
            if rearm {
                this.request_drive();
            }
        });
        state.run = Some(handle);
    }

    /// Stops future work, cancels timers, and awaits every outstanding runtime promise.
    pub async fn dispose(&self) {
        self.inner
            .disposed
            .get_or_init(|| async {
                let (run, idle_wait) = {
                    let mut state = self.inner.state.lock().await;
                    state.stopping = true;
                    state.requested = false;
                    if let Some(timer) = state.timer.take() {
                        timer.abort();
                    }
                    (state.run.take(), state.idle_wait.take())
                };
                self.inner.stop.notify_waiters();
                if let Some(run) = run {
                    let _ = run.await;
                }
                if let Some(idle_wait) = idle_wait {
                    let _ = idle_wait.await;
                }
            })
            .await;
    }

    fn clear_timer(&self) {
        let mut state = self.inner.state.blocking_lock();
        if let Some(timer) = state.timer.take() {
            timer.abort();
        }
    }

    fn arm(self: &Arc<Self>, target: i64, now: i64) {
        let max_delay = i64::try_from(MAX_TIMER_DELAY_MS).unwrap_or(i64::MAX);
        let delay_ms = u64::try_from((target - now).clamp(0, max_delay)).unwrap_or(0);
        let this = self.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            {
                let mut state = this.inner.state.blocking_lock();
                state.timer = None;
            }
            this.request_drive();
        });
        self.inner.state.blocking_lock().timer = Some(handle.abort_handle());
    }

    fn is_live(&self) -> bool {
        self.context.get(AGENTS).is_some_and(|registry| {
            registry
                .get(self.agent.id())
                .is_some_and(|agent| Arc::ptr_eq(&agent, &self.agent))
                && registry
                    .roots()
                    .iter()
                    .any(|agent| Arc::ptr_eq(agent, &self.agent))
        })
    }

    fn is_runnable(&self) -> bool {
        !self.inner.state.blocking_lock().stopping && self.is_live()
    }

    fn read_folded(&self) -> Option<FoldedSchedules> {
        let seed_length = self
            .agent
            .session()
            .header()
            .seed_length
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(0);
        match fold_schedule_events(&self.agent.session().events(), seed_length) {
            Ok(folded) => Some(folded),
            Err(error @ ScheduleLogError { .. }) => {
                self.inner.state.blocking_lock().faulted = true;
                tracing::warn!(
                    agent = %self.agent.id().as_str(),
                    detail = %error.message,
                    "schedule: corrupt schedule log",
                );
                None
            }
        }
    }

    fn decide(&self, folded: &FoldedSchedules, now: i64) -> Option<DueDecision> {
        match due_decision(folded, now) {
            Ok(decision) => Some(decision),
            Err(error) => {
                tracing::warn!(
                    agent = %self.agent.id().as_str(),
                    %error,
                    "schedule: fixed-rate decision failed",
                );
                None
            }
        }
    }

    fn wait_for_idle(self: &Arc<Self>) {
        {
            let state = self.inner.state.blocking_lock();
            if state.idle_wait.is_some() {
                return;
            }
        }
        let idle = match self.agent.when_idle() {
            Ok(idle) => idle,
            Err(error) => {
                if self.is_live() {
                    tracing::warn!(
                        agent = %self.agent.id().as_str(),
                        %error,
                        "schedule: idle wait failed",
                    );
                }
                return;
            }
        };
        let this = self.clone();
        let handle = tokio::spawn(async move {
            tokio::select! {
                () = idle => {}
                () = this.inner.stop.notified() => {}
            }
            {
                let mut state = this.inner.state.blocking_lock();
                state.idle_wait = None;
            }
            this.request_drive();
        });
        self.inner.state.blocking_lock().idle_wait = Some(handle);
    }

    async fn drive_once(self: &Arc<Self>) {
        self.clear_timer();
        if !self.is_runnable() {
            return;
        }
        if let Err(error) = flush_schedule_persistence(&self.context, self.agent.session()).await {
            if self.is_live() {
                tracing::warn!(
                    agent = %self.agent.id().as_str(),
                    %error,
                    "schedule: preflight failed",
                );
            }
            return;
        }
        if !self.is_runnable() {
            return;
        }
        let Some(folded) = self.read_folded() else {
            return;
        };
        let wake_now = now_millis();
        let Some(wake_decision) = self.decide(&folded, wake_now) else {
            return;
        };
        if let DueDecision::Wait { target } = &wake_decision {
            if let Some(target) = target {
                self.arm(*target, wake_now);
            }
            return;
        }
        let this = self.clone();
        let Ok(maintenance) = self.agent.run_maintenance(move |_signal| {
            let this = this.clone();
            async move { this.dispatch_decision() }
        }) else {
            if self.is_live() {
                self.wait_for_idle();
            }
            return;
        };
        if !maintenance.await {
            return;
        }
        if let Err(error) = flush_schedule_persistence(&self.context, self.agent.session()).await {
            if self.is_live() {
                tracing::warn!(
                    agent = %self.agent.id().as_str(),
                    %error,
                    "schedule: dispatch barrier failed",
                );
            }
            return;
        }
        if self.is_runnable() {
            self.request_drive();
        }
    }

    fn dispatch_decision(self: &Arc<Self>) -> bool {
        if !self.is_runnable() {
            return false;
        }
        let Some(claimed) = self.read_folded() else {
            return false;
        };
        let decision_now = now_millis();
        let Some(decision) = self.decide(&claimed, decision_now) else {
            return false;
        };
        if let DueDecision::Wait { target } = decision {
            if let Some(target) = target {
                self.arm(target, decision_now);
            }
            return false;
        }
        let text = match &decision {
            DueDecision::OneShot { record } => render_reminder_framing(record),
            DueDecision::Every { reminders, .. } => render_every_reminder_batch_framing(reminders),
            DueDecision::Wait { .. } => return false,
        };
        let message = UserMessage::new(
            vec![ContentBlock::Text { text }],
            MessageSource::plugin("schedule"),
        );
        if let Err(error) = self.agent.followup(message) {
            if self.is_live() {
                tracing::warn!(
                    agent = %self.agent.id().as_str(),
                    %error,
                    "schedule: framing or followup failed",
                );
            }
            return false;
        }
        if let Err(error) = self.append_dispatch(&decision) {
            self.inner.state.blocking_lock().faulted = true;
            self.clear_timer();
            tracing::warn!(
                agent = %self.agent.id().as_str(),
                %error,
                "schedule: dispatch append failed",
            );
            return false;
        }
        true
    }

    fn append_dispatch(
        &self,
        decision: &DueDecision,
    ) -> Result<(), seekdeep_core::session::SessionError> {
        match decision {
            DueDecision::OneShot { record } => {
                let id = match record {
                    OneShotScheduleRecord::After(record) => &record.id,
                    OneShotScheduleRecord::At(record) => &record.id,
                };
                self.agent
                    .session()
                    .append(
                        "schedule/change",
                        json!({ "version": 1, "operation": "dispatch", "id": id.as_str() }),
                        AppendOptions::default(),
                    )
                    .map(|_| ())
            }
            DueDecision::Every {
                reminders,
                accepted_at,
            } => {
                for reminder in reminders {
                    self.agent
                        .session()
                        .append(
                            "schedule/change",
                            json!({
                                "version": 1,
                                "operation": "dispatch",
                                "id": reminder.record.id.as_str(),
                                "acceptedAt": accepted_at,
                            }),
                            AppendOptions::default(),
                        )
                        .map(|_| ())?;
                }
                Ok(())
            }
            DueDecision::Wait { .. } => Ok(()),
        }
    }
}

async fn run_requested_loop(this: Arc<ScheduleRuntime>) {
    loop {
        let should_run = {
            let mut state = this.inner.state.lock().await;
            if state.stopping || state.faulted || !state.requested {
                false
            } else {
                state.requested = false;
                true
            }
        };
        if !should_run {
            break;
        }
        let loop_this = this.clone();
        run_schedule_transaction(this.agent.clone(), move || {
            let this = loop_this.clone();
            Box::pin(async move { this.drive_once().await })
        })
        .await;
    }
}

fn now_millis() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_millis()),
    )
    .unwrap_or(i64::MAX)
}
