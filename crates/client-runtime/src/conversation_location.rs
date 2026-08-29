//! Session-owned Turn/Step timeline and event-to-Location index.

use std::{cell::RefCell, rc::Rc};

use indexmap::{IndexMap, IndexSet};
use serde_json::Value;

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Minimal durable event retained by the Client location index.
#[derive(Clone, Debug, PartialEq)]
pub struct ConversationLocationEvent {
    /// Durable event sequence.
    pub seq: u64,
    /// Unix epoch milliseconds.
    pub time: i64,
    /// Merge-extensible Session event type.
    pub event_type: String,
    /// Complete event data object.
    pub data: Value,
    /// Exact wire event, retained when the browser transport supplied it.
    pub wire: Option<Value>,
}

impl ConversationLocationEvent {
    /// Creates one already-validated Session event view.
    #[must_use]
    pub fn new(seq: u64, event_type: impl Into<String>, data: Value) -> Rc<Self> {
        Rc::new(Self {
            seq,
            time: 0,
            event_type: event_type.into(),
            data,
            wire: None,
        })
    }

    /// Creates one already-validated Session event with its recorded timestamp.
    #[must_use]
    pub fn with_time(seq: u64, time: i64, event_type: impl Into<String>, data: Value) -> Rc<Self> {
        Rc::new(Self {
            seq,
            time,
            event_type: event_type.into(),
            data,
            wire: None,
        })
    }

    /// Creates one event while retaining its complete extension-bearing wire object.
    #[must_use]
    pub fn with_wire(
        seq: u64,
        time: i64,
        event_type: impl Into<String>,
        data: Value,
        wire: Value,
    ) -> Rc<Self> {
        Rc::new(Self {
            seq,
            time,
            event_type: event_type.into(),
            data,
            wire: Some(wire),
        })
    }

    /// Returns the exact wire event or the canonical minimal event shape.
    #[must_use]
    pub fn wire_value(&self) -> Value {
        self.wire.clone().unwrap_or_else(|| {
            serde_json::json!({
                "seq":self.seq,
                "time":self.time,
                "type":self.event_type,
                "data":self.data,
            })
        })
    }
}

/// One raw log event accepted by Conversation assembly.
#[derive(Clone, Debug)]
pub struct ConversationEventInput {
    /// Exact durable event reference.
    pub event: Rc<ConversationLocationEvent>,
    /// Optional envelope-level presentation view.
    pub view: Option<Rc<Value>>,
}

/// Open/closed knowledge for one Turn or Step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConversationBoundaryStatus {
    /// Start exists and no end exists.
    Open,
    /// End exists.
    Closed,
    /// Neither boundary is present in the current window.
    Unknown,
}

#[derive(Clone)]
struct OwnedLocationData {
    owner: String,
    value: Rc<Value>,
}

/// Stable keyed reader for independently owned Location business values.
#[derive(Default)]
pub struct ConversationLocationDataStore {
    entries: RefCell<IndexMap<String, OwnedLocationData>>,
}

impl ConversationLocationDataStore {
    /// Reads the latest immutable value for one business key.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<Rc<Value>> {
        self.entries
            .borrow()
            .get(key)
            .map(|entry| entry.value.clone())
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn snapshot_values(&self) -> IndexMap<String, Rc<Value>> {
        self.entries
            .borrow()
            .iter()
            .map(|(key, entry)| (key.clone(), entry.value.clone()))
            .collect()
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn from_values(values: IndexMap<String, Rc<Value>>) -> Self {
        Self {
            entries: RefCell::new(
                values
                    .into_iter()
                    .map(|(key, value)| {
                        (
                            key,
                            OwnedLocationData {
                                owner: "native-definition-bridge".to_owned(),
                                value,
                            },
                        )
                    })
                    .collect(),
            ),
        }
    }

    fn remove(&self, owner: &str, key: &str) -> bool {
        let mut entries = self.entries.borrow_mut();
        if entries
            .get(key)
            .is_none_or(|current| current.owner != owner)
        {
            return false;
        }
        entries.shift_remove(key);
        true
    }

    fn set(
        &self,
        owner: &str,
        key: &str,
        value: Rc<Value>,
    ) -> Result<bool, ConversationLocationError> {
        let mut entries = self.entries.borrow_mut();
        if let Some(current) = entries.get(key) {
            if current.owner != owner {
                return Err(ConversationLocationError::new(format!(
                    "conversation Location data \"{key}\" is already owned by {}",
                    current.owner
                )));
            }
            if Rc::ptr_eq(&current.value, &value) {
                return Ok(false);
            }
        }
        entries.insert(
            key.to_owned(),
            OwnedLocationData {
                owner: owner.to_owned(),
                value,
            },
        );
        Ok(true)
    }

    fn replace(&self, next: IndexMap<String, OwnedLocationData>) -> bool {
        let mut current = self.entries.borrow_mut();
        let mut changed = current.len() != next.len();
        if !changed {
            changed = next.iter().any(|(key, value)| {
                current.get(key).is_none_or(|known| {
                    known.owner != value.owner || !Rc::ptr_eq(&known.value, &value.value)
                })
            });
        }
        if changed {
            *current = next;
        }
        changed
    }
}

/// Immutable resolved boundary for one Agent Step.
pub struct StepLocation {
    /// Owning Turn number.
    pub turn: u64,
    /// Step number.
    pub step: u64,
    /// Start event when present in the window.
    pub start: Option<Rc<ConversationLocationEvent>>,
    /// End event when present in the window.
    pub end: Option<Rc<ConversationLocationEvent>>,
    /// Current boundary knowledge.
    pub status: ConversationBoundaryStatus,
    /// Stable Step-scoped business-value reader.
    pub data: Rc<ConversationLocationDataStore>,
}

/// Immutable resolved boundary for one Agent Turn.
pub struct TurnLocation {
    /// Turn number.
    pub turn: u64,
    /// Start event when present in the window.
    pub start: Option<Rc<ConversationLocationEvent>>,
    /// End event when present in the window.
    pub end: Option<Rc<ConversationLocationEvent>>,
    /// Current boundary knowledge.
    pub status: ConversationBoundaryStatus,
    /// Steps in first-observed sequence order.
    pub steps: Rc<Vec<Rc<StepLocation>>>,
    /// Stable Turn-scoped business-value reader.
    pub data: Rc<ConversationLocationDataStore>,
}

/// Engine-owned placement of one event in the Session hierarchy.
#[derive(Clone)]
pub enum ConversationLocation {
    /// No Turn/Step affinity.
    Session,
    /// Resolved Turn without a resolved Step.
    Turn {
        /// Owning Turn.
        turn: Rc<TurnLocation>,
    },
    /// Resolved Step within a Turn.
    Step {
        /// Owning Turn.
        turn: Rc<TurnLocation>,
        /// Owning Step.
        step: Rc<StepLocation>,
    },
    /// Coordinate referenced a Turn not present in the current timeline.
    Unresolved,
}

/// Reference-stable Turn/Step timeline snapshot.
pub struct ConversationTimelineSnapshot {
    /// Turns in first-observed sequence order.
    pub turn_order: Rc<Vec<u64>>,
    /// Turn values by Turn number.
    pub turns: Rc<IndexMap<u64, Rc<TurnLocation>>>,
}

impl Default for ConversationTimelineSnapshot {
    fn default() -> Self {
        Self {
            turn_order: Rc::new(Vec::new()),
            turns: Rc::new(IndexMap::new()),
        }
    }
}

/// Definition-owned business value attached to one Turn or Step.
#[derive(Clone)]
pub enum ConversationLocationData {
    /// Turn-scoped value.
    Turn {
        /// Turn number.
        turn: u64,
        /// Merge-extensible business key.
        key: String,
        /// Exact immutable value identity.
        value: Rc<Value>,
    },
    /// Step-scoped value. `step` remains optional so the source diagnostic is representable.
    Step {
        /// Turn number.
        turn: u64,
        /// Step number.
        step: Option<u64>,
        /// Merge-extensible business key.
        key: String,
        /// Exact immutable value identity.
        value: Rc<Value>,
    },
}

impl ConversationLocationData {
    fn key(&self) -> &str {
        match self {
            Self::Turn { key, .. } | Self::Step { key, .. } => key,
        }
    }
}

/// One Context's previous and next Location-data publication.
pub struct ConversationLocationDataChange {
    /// Stable Context owner key.
    pub owner: String,
    /// Previous publication.
    pub previous: Option<ConversationLocationData>,
    /// Next publication.
    pub next: Option<ConversationLocationData>,
}

/// One complete Definition-owned Location-data publication.
pub struct ConversationOwnedLocationData {
    /// Stable Context owner key.
    pub owner: String,
    /// Published Location value.
    pub data: ConversationLocationData,
}

#[derive(Clone, Copy, Debug, Default)]
struct Coordinates {
    turn: Option<u64>,
    step: Option<u64>,
    session: bool,
}

struct StepDraft {
    turn: u64,
    step: u64,
    first_seq: u64,
    start: Option<Rc<ConversationLocationEvent>>,
    end: Option<Rc<ConversationLocationEvent>>,
}

struct TurnDraft {
    turn: u64,
    first_seq: u64,
    start: Option<Rc<ConversationLocationEvent>>,
    end: Option<Rc<ConversationLocationEvent>>,
    steps: IndexMap<u64, StepDraft>,
}

/// Fail-loud malformed boundary or Location-data ownership diagnostic.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ConversationLocationError(String);

impl ConversationLocationError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// Incremental Session Turn/Step timeline and event-to-Location index.
pub struct ConversationLocationIndex {
    coordinates: IndexMap<u64, Coordinates>,
    locations: IndexMap<u64, ConversationLocation>,
    seqs_by_turn: IndexMap<u64, IndexSet<u64>>,
    timeline: Rc<ConversationTimelineSnapshot>,
    turn_data_stores: RefCell<IndexMap<u64, Rc<ConversationLocationDataStore>>>,
    step_data_stores: RefCell<IndexMap<String, Rc<ConversationLocationDataStore>>>,
    current_turn: Option<u64>,
    current_step: Option<u64>,
}

impl Default for ConversationLocationIndex {
    fn default() -> Self {
        Self {
            coordinates: IndexMap::new(),
            locations: IndexMap::new(),
            seqs_by_turn: IndexMap::new(),
            timeline: Rc::new(ConversationTimelineSnapshot::default()),
            turn_data_stores: RefCell::new(IndexMap::new()),
            step_data_stores: RefCell::new(IndexMap::new()),
            current_turn: None,
            current_step: None,
        }
    }
}

impl ConversationLocationIndex {
    /// Returns the current reference-stable timeline.
    #[must_use]
    pub fn snapshot(&self) -> Rc<ConversationTimelineSnapshot> {
        self.timeline.clone()
    }

    /// Replaces every Definition-owned Location value while retaining reader identities.
    ///
    /// # Errors
    ///
    /// Returns duplicate-owner or missing-Step diagnostics.
    pub fn replace_data(
        &self,
        entries: &[ConversationOwnedLocationData],
    ) -> Result<bool, ConversationLocationError> {
        let mut turns: IndexMap<u64, IndexMap<String, OwnedLocationData>> = IndexMap::new();
        let mut steps: IndexMap<String, IndexMap<String, OwnedLocationData>> = IndexMap::new();
        for entry in entries {
            let (values, key, value) = match &entry.data {
                ConversationLocationData::Turn { turn, key, value } => {
                    (turns.entry(*turn).or_default(), key, value)
                }
                ConversationLocationData::Step {
                    turn,
                    step,
                    key,
                    value,
                } => {
                    let step = require_step(key, *step)?;
                    (
                        steps.entry(step_data_key(*turn, step)).or_default(),
                        key,
                        value,
                    )
                }
            };
            if let Some(current) = values.get(key)
                && current.owner != entry.owner
            {
                return Err(ConversationLocationError::new(format!(
                    "conversation Location data \"{key}\" is already owned by {}",
                    current.owner
                )));
            }
            values.insert(
                key.clone(),
                OwnedLocationData {
                    owner: entry.owner.clone(),
                    value: value.clone(),
                },
            );
        }
        let mut did_change = false;
        let turn_keys = self
            .turn_data_stores
            .borrow()
            .keys()
            .copied()
            .chain(turns.keys().copied())
            .collect::<IndexSet<_>>();
        for turn in turn_keys {
            did_change = self
                .mutable_turn_data(turn)
                .replace(turns.shift_remove(&turn).unwrap_or_default())
                || did_change;
        }
        let step_keys = self
            .step_data_stores
            .borrow()
            .keys()
            .cloned()
            .chain(steps.keys().cloned())
            .collect::<IndexSet<_>>();
        for step in step_keys {
            did_change = self
                .mutable_step_data(&step)
                .replace(steps.shift_remove(&step).unwrap_or_default())
                || did_change;
        }
        Ok(did_change)
    }

    /// Applies incremental Context publication removals and replacements.
    ///
    /// # Errors
    ///
    /// Returns ownership conflicts or missing-Step diagnostics.
    pub fn apply_data(
        &self,
        changes: &[ConversationLocationDataChange],
    ) -> Result<bool, ConversationLocationError> {
        let mut did_change = false;
        for change in changes {
            if let Some(previous) = &change.previous {
                did_change = self
                    .store_for(previous)?
                    .remove(&change.owner, previous.key())
                    || did_change;
            }
        }
        for change in changes {
            if let Some(next) = &change.next {
                let value = match next {
                    ConversationLocationData::Turn { value, .. }
                    | ConversationLocationData::Step { value, .. } => value.clone(),
                };
                did_change = self
                    .store_for(next)?
                    .set(&change.owner, next.key(), value)?
                    || did_change;
            }
        }
        Ok(did_change)
    }

    /// Resolves one already-ingested event, defaulting to Session affinity.
    #[must_use]
    pub fn location_of(&self, event: &ConversationLocationEvent) -> ConversationLocation {
        self.locations
            .get(&event.seq)
            .cloned()
            .unwrap_or(ConversationLocation::Session)
    }

    /// Rebuilds timeline facts after replace, prepend, or a boundary repair.
    ///
    /// # Errors
    ///
    /// Returns when a typed boundary lacks its required non-negative safe coordinates.
    #[allow(clippy::too_many_lines)] // Preserve the source's one ordered rebuild transaction.
    pub fn rebuild(
        &mut self,
        entries: &[ConversationEventInput],
    ) -> Result<IndexSet<u64>, ConversationLocationError> {
        let previous_locations = self.locations.clone();
        let mut turns = IndexMap::<u64, TurnDraft>::new();
        let mut coordinates = IndexMap::new();
        let mut current_turn = None;
        let mut current_step = None;

        for input in entries {
            let event = &input.event;
            let explicit = payload_coordinates(event);
            match event.event_type.as_str() {
                "turn/start" => {
                    current_turn = Some(boundary_number(event, "turn")?);
                    current_step = None;
                }
                "step/start" => {
                    current_turn = Some(boundary_number(event, "turn")?);
                    current_step = Some(boundary_number(event, "step")?);
                }
                _ => {}
            }
            if !explicit.session
                && let Some(turn) = explicit.turn
            {
                if current_turn != Some(turn) {
                    current_step = None;
                }
                current_turn = Some(turn);
                if let Some(step) = explicit.step {
                    current_step = Some(step);
                }
            }
            let turn = (!explicit.session)
                .then_some(explicit.turn.or(current_turn))
                .flatten();
            let step = if explicit.session
                || matches!(event.event_type.as_str(), "turn/start" | "turn/end")
            {
                None
            } else {
                explicit
                    .step
                    .or_else(|| (turn == current_turn).then_some(current_step).flatten())
            };
            coordinates.insert(
                event.seq,
                Coordinates {
                    turn,
                    step: turn.and(step),
                    session: false,
                },
            );
            if let Some(turn) = turn {
                ensure_turn_draft(&mut turns, turn, event.seq);
                if let Some(step) = step {
                    ensure_step_draft(&mut turns, turn, step, event.seq);
                }
            }

            match event.event_type.as_str() {
                "turn/start" => {
                    let turn = boundary_number(event, "turn")?;
                    ensure_turn_draft(&mut turns, turn, event.seq).start = Some(event.clone());
                }
                "turn/end" => {
                    let turn = boundary_number(event, "turn")?;
                    ensure_turn_draft(&mut turns, turn, event.seq).end = Some(event.clone());
                }
                "step/start" => {
                    let turn = boundary_number(event, "turn")?;
                    let step = boundary_number(event, "step")?;
                    ensure_step_draft(&mut turns, turn, step, event.seq).start =
                        Some(event.clone());
                }
                "step/end" => {
                    let turn = boundary_number(event, "turn")?;
                    let step = boundary_number(event, "step")?;
                    ensure_step_draft(&mut turns, turn, step, event.seq).end = Some(event.clone());
                }
                _ => {}
            }

            if event.event_type == "step/end"
                && current_turn == safe_member(&event.data, "turn")
                && current_step == safe_member(&event.data, "step")
            {
                current_step = None;
            }
            if event.event_type == "turn/end" && current_turn == safe_member(&event.data, "turn") {
                current_turn = None;
                current_step = None;
            }
        }

        let previous_turns = self.timeline.turns.clone();
        let mut ordered_drafts = turns.into_values().collect::<Vec<_>>();
        ordered_drafts.sort_by_key(|draft| draft.first_seq);
        let mut next_turns = IndexMap::new();
        for draft in &mut ordered_drafts {
            let previous_turn = previous_turns.get(&draft.turn);
            let previous_steps = previous_turn
                .map(|turn| {
                    turn.steps
                        .iter()
                        .map(|step| (step.step, step.clone()))
                        .collect::<IndexMap<_, _>>()
                })
                .unwrap_or_default();
            let mut step_drafts = draft
                .steps
                .drain(..)
                .map(|(_, step)| step)
                .collect::<Vec<_>>();
            step_drafts.sort_by_key(|step| step.first_seq);
            let steps = step_drafts
                .into_iter()
                .map(|candidate| {
                    let status = if candidate.end.is_some() {
                        ConversationBoundaryStatus::Closed
                    } else if candidate.start.is_none() {
                        ConversationBoundaryStatus::Unknown
                    } else {
                        ConversationBoundaryStatus::Open
                    };
                    let value = Rc::new(StepLocation {
                        turn: candidate.turn,
                        step: candidate.step,
                        start: candidate.start,
                        end: candidate.end,
                        status,
                        data: self.step_data(candidate.turn, candidate.step),
                    });
                    previous_steps
                        .get(&candidate.step)
                        .filter(|previous| same_step(previous, &value))
                        .cloned()
                        .unwrap_or(value)
                })
                .collect::<Vec<_>>();
            let value = Rc::new(TurnLocation {
                turn: draft.turn,
                start: draft.start.clone(),
                end: draft.end.clone(),
                status: if draft.end.is_some() {
                    ConversationBoundaryStatus::Closed
                } else if draft.start.is_none() {
                    ConversationBoundaryStatus::Unknown
                } else {
                    ConversationBoundaryStatus::Open
                },
                steps: Rc::new(steps),
                data: self.turn_data(draft.turn),
            });
            let value = previous_turn
                .filter(|previous| same_turn(previous, &value))
                .cloned()
                .unwrap_or(value);
            next_turns.insert(draft.turn, value);
        }

        let next_order = ordered_drafts
            .iter()
            .map(|draft| draft.turn)
            .collect::<Vec<_>>();
        let turn_order = if self.timeline.turn_order.as_ref() == &next_order {
            self.timeline.turn_order.clone()
        } else {
            Rc::new(next_order)
        };
        let same_map = previous_turns.len() == next_turns.len()
            && next_turns.iter().all(|(turn, value)| {
                previous_turns
                    .get(turn)
                    .is_some_and(|previous| Rc::ptr_eq(previous, value))
            });
        if !same_map || !Rc::ptr_eq(&turn_order, &self.timeline.turn_order) {
            self.timeline = Rc::new(ConversationTimelineSnapshot {
                turn_order,
                turns: Rc::new(next_turns),
            });
        }
        self.coordinates = coordinates;
        self.locations.clear();
        self.seqs_by_turn.clear();
        for input in entries {
            let seq = input.event.seq;
            if let Some(turn) = self.coordinates.get(&seq).and_then(|value| value.turn) {
                self.index_turn_seq(turn, seq);
            }
            let location = self.resolve(seq);
            self.locations.insert(seq, location);
        }
        self.current_turn = current_turn;
        self.current_step = current_step;

        Ok(entries
            .iter()
            .filter_map(|input| {
                let seq = input.event.seq;
                (!same_location(previous_locations.get(&seq), self.locations.get(&seq)))
                    .then_some(seq)
            })
            .collect())
    }

    /// Appends one contiguous Turn/Step boundary while revisiting only its Turn.
    ///
    /// # Errors
    ///
    /// Returns non-boundary or missing-coordinate diagnostics.
    #[allow(clippy::too_many_lines)] // Preserve the source's Turn-local atomic boundary update.
    pub fn append_boundary(
        &mut self,
        event: &Rc<ConversationLocationEvent>,
    ) -> Result<IndexSet<u64>, ConversationLocationError> {
        if !matches!(
            event.event_type.as_str(),
            "turn/start" | "turn/end" | "step/start" | "step/end"
        ) {
            return Err(ConversationLocationError::new(format!(
                "conversation Location boundary expected, received {}",
                event.event_type
            )));
        }
        let explicit = payload_coordinates(event);
        match event.event_type.as_str() {
            "turn/start" => {
                self.current_turn = Some(boundary_number(event, "turn")?);
                self.current_step = None;
            }
            "step/start" => {
                self.current_turn = Some(boundary_number(event, "turn")?);
                self.current_step = Some(boundary_number(event, "step")?);
            }
            _ => {}
        }
        if let Some(turn) = explicit.turn {
            if self.current_turn != Some(turn) {
                self.current_step = None;
            }
            self.current_turn = Some(turn);
            if let Some(step) = explicit.step {
                self.current_step = Some(step);
            }
        }
        let turn_number = explicit.turn.or(self.current_turn).ok_or_else(|| {
            ConversationLocationError::new(format!(
                "conversation boundary {} has no turn",
                event.event_type
            ))
        })?;
        let step_number = if matches!(event.event_type.as_str(), "turn/start" | "turn/end") {
            None
        } else {
            explicit.step.or_else(|| {
                (self.current_turn == Some(turn_number))
                    .then_some(self.current_step)
                    .flatten()
            })
        };
        self.coordinates.insert(
            event.seq,
            Coordinates {
                turn: Some(turn_number),
                step: step_number,
                session: false,
            },
        );
        self.index_turn_seq(turn_number, event.seq);

        let previous_turn = self.timeline.turns.get(&turn_number).cloned();
        let mut steps = previous_turn
            .as_ref()
            .map_or_else(Vec::new, |turn| turn.steps.as_ref().clone());
        if matches!(event.event_type.as_str(), "step/start" | "step/end") {
            let number = boundary_number(event, "step")?;
            let previous_step = steps.iter().find(|step| step.step == number).cloned();
            let candidate = Rc::new(StepLocation {
                turn: turn_number,
                step: number,
                start: if event.event_type == "step/start" {
                    Some(event.clone())
                } else {
                    previous_step.as_ref().and_then(|step| step.start.clone())
                },
                end: if event.event_type == "step/end" {
                    Some(event.clone())
                } else {
                    previous_step.as_ref().and_then(|step| step.end.clone())
                },
                status: if event.event_type == "step/end"
                    || previous_step
                        .as_ref()
                        .is_some_and(|step| step.end.is_some())
                {
                    ConversationBoundaryStatus::Closed
                } else {
                    ConversationBoundaryStatus::Open
                },
                data: self.step_data(turn_number, number),
            });
            let next_step = previous_step
                .filter(|previous| same_step(previous, &candidate))
                .unwrap_or(candidate);
            if let Some(index) = steps.iter().position(|step| step.step == number) {
                steps[index] = next_step;
            } else {
                steps.push(next_step);
            }
        }
        let candidate = Rc::new(TurnLocation {
            turn: turn_number,
            start: if event.event_type == "turn/start" {
                Some(event.clone())
            } else {
                previous_turn.as_ref().and_then(|turn| turn.start.clone())
            },
            end: if event.event_type == "turn/end" {
                Some(event.clone())
            } else {
                previous_turn.as_ref().and_then(|turn| turn.end.clone())
            },
            status: if event.event_type == "turn/end"
                || previous_turn
                    .as_ref()
                    .is_some_and(|turn| turn.end.is_some())
            {
                ConversationBoundaryStatus::Closed
            } else if event.event_type == "turn/start"
                || previous_turn
                    .as_ref()
                    .is_some_and(|turn| turn.start.is_some())
            {
                ConversationBoundaryStatus::Open
            } else {
                ConversationBoundaryStatus::Unknown
            },
            steps: Rc::new(steps),
            data: self.turn_data(turn_number),
        });
        let turn = previous_turn
            .as_ref()
            .filter(|previous| same_turn(previous, &candidate))
            .cloned()
            .unwrap_or(candidate);
        let mut turns = self.timeline.turns.as_ref().clone();
        let is_new = previous_turn.is_none();
        turns.insert(turn_number, turn);
        let turn_order = if is_new {
            let mut order = self.timeline.turn_order.as_ref().clone();
            order.push(turn_number);
            Rc::new(order)
        } else {
            self.timeline.turn_order.clone()
        };
        self.timeline = Rc::new(ConversationTimelineSnapshot {
            turn_order,
            turns: Rc::new(turns),
        });

        let mut changed = IndexSet::new();
        for seq in self
            .seqs_by_turn
            .get(&turn_number)
            .cloned()
            .unwrap_or_default()
        {
            let previous = self.locations.get(&seq).cloned();
            let next = self.resolve(seq);
            self.locations.insert(seq, next.clone());
            if !same_location(previous.as_ref(), Some(&next)) {
                changed.insert(seq);
            }
        }
        if event.event_type == "step/end"
            && self.current_turn == safe_member(&event.data, "turn")
            && self.current_step == safe_member(&event.data, "step")
        {
            self.current_step = None;
        }
        if event.event_type == "turn/end" && self.current_turn == safe_member(&event.data, "turn") {
            self.current_turn = None;
            self.current_step = None;
        }
        Ok(changed)
    }

    /// Indexes one contiguous non-boundary event without rescanning the window.
    pub fn append_non_boundary(&mut self, event: &ConversationLocationEvent) {
        let explicit = payload_coordinates(event);
        if explicit.session {
            self.coordinates.insert(event.seq, Coordinates::default());
            self.locations
                .insert(event.seq, ConversationLocation::Session);
            return;
        }
        if let Some(turn) = explicit.turn {
            if self.current_turn != Some(turn) {
                self.current_step = None;
            }
            self.current_turn = Some(turn);
            if let Some(step) = explicit.step {
                self.current_step = Some(step);
            }
        }
        let turn = explicit.turn.or(self.current_turn);
        let step = explicit.step.or_else(|| {
            (turn == self.current_turn)
                .then_some(self.current_step)
                .flatten()
        });
        self.coordinates.insert(
            event.seq,
            Coordinates {
                turn,
                step: turn.and(step),
                session: false,
            },
        );
        if let Some(turn) = turn {
            self.index_turn_seq(turn, event.seq);
        }
        let location = self.resolve(event.seq);
        self.locations.insert(event.seq, location);
    }

    fn index_turn_seq(&mut self, turn: u64, seq: u64) {
        self.seqs_by_turn.entry(turn).or_default().insert(seq);
    }

    fn turn_data(&self, turn: u64) -> Rc<ConversationLocationDataStore> {
        self.mutable_turn_data(turn)
    }

    fn step_data(&self, turn: u64, step: u64) -> Rc<ConversationLocationDataStore> {
        self.mutable_step_data(&step_data_key(turn, step))
    }

    fn mutable_turn_data(&self, turn: u64) -> Rc<ConversationLocationDataStore> {
        self.turn_data_stores
            .borrow_mut()
            .entry(turn)
            .or_insert_with(|| Rc::new(ConversationLocationDataStore::default()))
            .clone()
    }

    fn mutable_step_data(&self, key: &str) -> Rc<ConversationLocationDataStore> {
        self.step_data_stores
            .borrow_mut()
            .entry(key.to_owned())
            .or_insert_with(|| Rc::new(ConversationLocationDataStore::default()))
            .clone()
    }

    fn store_for(
        &self,
        data: &ConversationLocationData,
    ) -> Result<Rc<ConversationLocationDataStore>, ConversationLocationError> {
        match data {
            ConversationLocationData::Turn { turn, .. } => Ok(self.mutable_turn_data(*turn)),
            ConversationLocationData::Step {
                turn, step, key, ..
            } => Ok(self.mutable_step_data(&step_data_key(*turn, require_step(key, *step)?))),
        }
    }

    fn resolve(&self, seq: u64) -> ConversationLocation {
        let Some(turn_number) = self.coordinates.get(&seq).and_then(|value| value.turn) else {
            return ConversationLocation::Session;
        };
        let Some(turn) = self.timeline.turns.get(&turn_number).cloned() else {
            return ConversationLocation::Unresolved;
        };
        let Some(step_number) = self.coordinates.get(&seq).and_then(|value| value.step) else {
            return ConversationLocation::Turn { turn };
        };
        if let Some(step) = turn
            .steps
            .iter()
            .find(|step| step.step == step_number)
            .cloned()
        {
            ConversationLocation::Step { turn, step }
        } else {
            ConversationLocation::Turn { turn }
        }
    }
}

fn ensure_turn_draft(turns: &mut IndexMap<u64, TurnDraft>, turn: u64, seq: u64) -> &mut TurnDraft {
    let draft = turns.entry(turn).or_insert_with(|| TurnDraft {
        turn,
        first_seq: seq,
        start: None,
        end: None,
        steps: IndexMap::new(),
    });
    draft.first_seq = draft.first_seq.min(seq);
    draft
}

fn ensure_step_draft(
    turns: &mut IndexMap<u64, TurnDraft>,
    turn: u64,
    step: u64,
    seq: u64,
) -> &mut StepDraft {
    let owner = ensure_turn_draft(turns, turn, seq);
    let draft = owner.steps.entry(step).or_insert_with(|| StepDraft {
        turn,
        step,
        first_seq: seq,
        start: None,
        end: None,
    });
    draft.first_seq = draft.first_seq.min(seq);
    draft
}

fn payload_coordinates(event: &ConversationLocationEvent) -> Coordinates {
    let session = event.data.get("turn").is_some_and(Value::is_null);
    if session {
        return Coordinates {
            session: true,
            ..Coordinates::default()
        };
    }
    Coordinates {
        turn: safe_member(&event.data, "turn"),
        step: safe_member(&event.data, "step"),
        session: false,
    }
}

fn safe_member(data: &Value, key: &str) -> Option<u64> {
    data.get(key)
        .and_then(Value::as_u64)
        .filter(|value| *value <= MAX_SAFE_INTEGER)
}

fn boundary_number(
    event: &ConversationLocationEvent,
    key: &str,
) -> Result<u64, ConversationLocationError> {
    safe_member(&event.data, key).ok_or_else(|| {
        ConversationLocationError::new(format!(
            "conversation boundary {} has no {key}",
            event.event_type
        ))
    })
}

fn step_data_key(turn: u64, step: u64) -> String {
    format!("{turn}:{step}")
}

fn require_step(key: &str, step: Option<u64>) -> Result<u64, ConversationLocationError> {
    step.ok_or_else(|| {
        ConversationLocationError::new(format!("conversation Step data \"{key}\" requires a step"))
    })
}

fn same_event(
    left: Option<&Rc<ConversationLocationEvent>>,
    right: Option<&Rc<ConversationLocationEvent>>,
) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => Rc::ptr_eq(left, right),
        (None, None) => true,
        _ => false,
    }
}

fn same_step(left: &StepLocation, right: &StepLocation) -> bool {
    same_event(left.start.as_ref(), right.start.as_ref())
        && same_event(left.end.as_ref(), right.end.as_ref())
        && left.status == right.status
        && Rc::ptr_eq(&left.data, &right.data)
}

fn same_turn(left: &TurnLocation, right: &TurnLocation) -> bool {
    same_event(left.start.as_ref(), right.start.as_ref())
        && same_event(left.end.as_ref(), right.end.as_ref())
        && left.status == right.status
        && Rc::ptr_eq(&left.data, &right.data)
        && left.steps.len() == right.steps.len()
        && left
            .steps
            .iter()
            .zip(right.steps.iter())
            .all(|(left, right)| Rc::ptr_eq(left, right))
}

fn same_location(
    left: Option<&ConversationLocation>,
    right: Option<&ConversationLocation>,
) -> bool {
    match (left, right) {
        (None, None)
        | (Some(ConversationLocation::Session), Some(ConversationLocation::Session))
        | (Some(ConversationLocation::Unresolved), Some(ConversationLocation::Unresolved)) => true,
        (
            Some(ConversationLocation::Turn { turn: left }),
            Some(ConversationLocation::Turn { turn: right }),
        ) => Rc::ptr_eq(left, right),
        (
            Some(ConversationLocation::Step {
                turn: left_turn,
                step: left_step,
            }),
            Some(ConversationLocation::Step {
                turn: right_turn,
                step: right_step,
            }),
        ) => Rc::ptr_eq(left_turn, right_turn) && Rc::ptr_eq(left_step, right_step),
        _ => false,
    }
}
