//! Pure session projections for subagent identity and active-turn duration.

use seekdeep_core::session::SessionEvent;
use seekdeep_session_projection::{ProjectionDefinition, ProjectionTransition};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::descriptor::{SubagentDescriptorData, fold_subagent_descriptor};
use crate::projection_types::{
    SubagentActiveTiming, SubagentIdentityProjection, SubagentTimingProjection,
};

/// Internal timing-fold state.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TimingState {
    settled_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    active: Option<SubagentActiveTiming>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending_turn_start: Option<u64>,
    descriptor_seen: bool,
}

fn event_time(event: &SessionEvent) -> u64 {
    u64::try_from(event.time).unwrap_or(0)
}

/// The active-turn duration projection.
///
/// # Panics
///
/// Panics if the folded state is malformed.
#[must_use]
pub fn subagent_timing_projection_definition() -> ProjectionDefinition {
    ProjectionDefinition::new(
        "subagentTiming",
        2,
        || Ok(serde_json::to_value(TimingState::default())?),
        |state: &Value, event: &SessionEvent| {
            let mut current: TimingState = serde_json::from_value(state.clone())?;
            let next = match event.event_type.as_str() {
                "turn/start" => {
                    if current.descriptor_seen {
                        current.active = Some(SubagentActiveTiming {
                            since: event_time(event),
                            through: event_time(event),
                        });
                        current.pending_turn_start = None;
                        Some(current)
                    } else {
                        current.pending_turn_start = Some(event_time(event));
                        Some(current)
                    }
                }
                "subagent/descriptor" => {
                    let active_since = current
                        .active
                        .map_or(current.pending_turn_start, |active| Some(active.since));
                    current.descriptor_seen = true;
                    current.settled_ms = 0;
                    current.pending_turn_start = None;
                    current.active = active_since.map(|since| SubagentActiveTiming {
                        since,
                        through: event_time(event),
                    });
                    Some(current)
                }
                "turn/end" => {
                    if !current.descriptor_seen {
                        if current.pending_turn_start.is_none() {
                            None
                        } else {
                            current.pending_turn_start = None;
                            Some(current)
                        }
                    } else if current.active.is_none() {
                        None
                    } else {
                        let active = current.active.expect("checked above");
                        current.settled_ms += event_time(event).saturating_sub(active.since);
                        current.active = None;
                        Some(current)
                    }
                }
                _ => {
                    if current.active.is_none() {
                        None
                    } else {
                        let mut active = current.active.expect("checked above");
                        active.through = event_time(event);
                        current.active = Some(active);
                        Some(current)
                    }
                }
            };
            match next {
                None => Ok(ProjectionTransition::Unchanged),
                Some(next) => ProjectionTransition::changed(next),
            }
        },
        |state: &Value| {
            let current: TimingState = serde_json::from_value(state.clone())?;
            Ok(serde_json::to_value(SubagentTimingProjection {
                settled_ms: current.settled_ms,
                active: current.active,
            })?)
        },
    )
}

/// Internal identity-fold state.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IdentityState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    identity: Option<SubagentIdentityProjection>,
}

fn descriptor_identity(event: &SessionEvent) -> Option<SubagentIdentityProjection> {
    let descriptor: Option<SubagentDescriptorData> =
        fold_subagent_descriptor(std::slice::from_ref(event))
            .ok()
            .flatten();
    let descriptor = descriptor?;
    match descriptor {
        SubagentDescriptorData::OneShot { label, .. } => {
            Some(SubagentIdentityProjection::OneShot {
                label,
                seq: event.seq,
            })
        }
        SubagentDescriptorData::Continuable { label, .. } => {
            Some(SubagentIdentityProjection::Continuable {
                label,
                seq: event.seq,
            })
        }
    }
}

/// The durable mode/label identity projection.
#[must_use]
pub fn subagent_identity_projection_definition() -> ProjectionDefinition {
    ProjectionDefinition::new(
        "subagent",
        2,
        || Ok(serde_json::to_value(IdentityState::default())?),
        |_state: &Value, event: &SessionEvent| {
            if event.event_type != "subagent/descriptor" {
                return Ok(ProjectionTransition::Unchanged);
            }
            let identity = descriptor_identity(event);
            let next = IdentityState { identity };
            ProjectionTransition::changed(next)
        },
        |state: &Value| {
            let current: IdentityState = serde_json::from_value(state.clone())?;
            Ok(serde_json::to_value(current.identity)?)
        },
    )
}

#[cfg(test)]
mod tests {
    use seekdeep_session_projection::ProjectionTransition;
    use serde_json::json;

    use super::*;

    fn event(event_type: &str, seq: u64, time: i64) -> SessionEvent {
        SessionEvent {
            event_type: event_type.to_owned(),
            seq,
            time,
            data: json!({}),
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        }
    }

    fn fold(events: &[SessionEvent]) -> Value {
        let definition = subagent_timing_projection_definition();
        let mut state = definition.initial_state().unwrap();
        for event in events {
            if let ProjectionTransition::Changed(next) =
                definition.apply_event(&state, event).unwrap()
            {
                state = next;
            }
        }
        definition.project(&state).unwrap()
    }

    #[test]
    fn descriptor_resets_inherited_timing_and_later_turns_accumulate() {
        assert_eq!(
            fold(&[
                event("turn/start", 0, 100),
                event("subagent/descriptor", 1, 110),
                event("turn/end", 2, 300),
                event("turn/start", 3, 1_000),
                event("subagent/descriptor", 4, 1_100),
                event("turn/end", 5, 4_100),
                event("turn/start", 6, 10_000),
                event("turn/end", 7, 12_000),
            ]),
            json!({ "settledMs": 5_100 })
        );
    }

    #[test]
    fn open_turn_tracks_through_and_reversed_boundaries_never_subtract() {
        assert_eq!(
            fold(&[
                event("turn/start", 0, 1_000),
                event("subagent/descriptor", 1, 1_100),
                event("turn/end", 2, 900),
                event("turn/start", 3, 2_000),
                event("assistant/chunk", 4, 2_500),
            ]),
            json!({ "settledMs": 0, "active": { "since": 2_000, "through": 2_500 } })
        );
    }

    #[test]
    fn completed_pre_descriptor_turns_and_unrelated_idle_events_are_ignored() {
        assert_eq!(
            fold(&[
                event("turn/start", 0, 100),
                event("turn/end", 1, 200),
                event("subagent/descriptor", 2, 300),
                event("assistant/chunk", 3, 400),
            ]),
            json!({ "settledMs": 0 })
        );
    }
}
