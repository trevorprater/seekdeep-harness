//! Pure session projections for subagent identity and active-turn duration.

use seekdeep_core::session::SessionEvent;
use seekdeep_session_projection::ProjectionDefinition;
use serde::{Deserialize, Serialize};

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
    ProjectionDefinition::typed(
        "subagentTiming",
        2,
        TimingState::default,
        |current: &mut TimingState, event: &SessionEvent| {
            match event.event_type.as_str() {
                "turn/start" => {
                    if current.descriptor_seen {
                        current.active = Some(SubagentActiveTiming {
                            since: event_time(event),
                            through: event_time(event),
                        });
                        current.pending_turn_start = None;
                    } else {
                        current.pending_turn_start = Some(event_time(event));
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
                }
                "turn/end" => {
                    if current.descriptor_seen {
                        let Some(active) = current.active else {
                            return Ok(false);
                        };
                        current.settled_ms += event_time(event).saturating_sub(active.since);
                        current.active = None;
                    } else {
                        if current.pending_turn_start.is_none() {
                            return Ok(false);
                        }
                        current.pending_turn_start = None;
                    }
                }
                _ => {
                    let Some(active) = current.active.as_mut() else {
                        return Ok(false);
                    };
                    active.through = event_time(event);
                }
            }
            Ok(true)
        },
        |current: &TimingState| {
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
    ProjectionDefinition::typed(
        "subagent",
        2,
        IdentityState::default,
        |state: &mut IdentityState, event: &SessionEvent| {
            if event.event_type != "subagent/descriptor" {
                return Ok(false);
            }
            state.identity = descriptor_identity(event);
            Ok(true)
        },
        |current: &IdentityState| Ok(serde_json::to_value(&current.identity)?),
    )
}

#[cfg(test)]
mod tests {
    use seekdeep_core::session::JsonValue;
    use seekdeep_session_projection::ProjectionTransition;
    use serde_json::json;

    use super::*;

    fn event(event_type: &str, seq: u64, time: i64) -> SessionEvent {
        SessionEvent {
            event_type: event_type.to_owned(),
            seq,
            time,
            data: json!({}).into(),
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        }
    }

    fn fold(events: &[SessionEvent]) -> JsonValue {
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
