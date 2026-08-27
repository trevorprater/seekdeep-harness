//! Incremental business-Context assembly over a contiguous Session event window.

use std::{cell::RefCell, cmp::Ordering, rc::Rc};

use indexmap::{IndexMap, IndexSet};
use serde_json::Value;

use crate::{
    ConversationEventInput, ConversationLocation, ConversationLocationData,
    ConversationLocationDataChange, ConversationLocationError, ConversationLocationEvent,
    ConversationLocationIndex, ConversationOwnedLocationData, ConversationTimelineSnapshot,
};

/// Requested cadence for materializing updated business State.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConversationPublication {
    /// Retain dirty State without scheduling a flush.
    None,
    /// Coalesce publication to the next animation frame.
    AnimationFrame,
    /// Publish through the immediate structural channel.
    Immediate,
}

impl ConversationPublication {
    fn rank(self) -> u8 {
        match self {
            Self::None => 0,
            Self::AnimationFrame => 1,
            Self::Immediate => 2,
        }
    }

    fn maximum(self, other: Self) -> Self {
        if self.rank() >= other.rank() {
            self
        } else {
            other
        }
    }
}

/// Definition-local lifecycle role extracted from one event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConversationMatchRole {
    /// Unique lifecycle start.
    Start,
    /// Post-start state update.
    Update,
}

/// Stable business identity and lifecycle role returned by a Definition matcher.
pub struct ConversationMatchResult {
    /// Definition-local identity.
    pub id: String,
    /// Lifecycle role.
    pub role: ConversationMatchRole,
}

/// One event accepted by a Definition with its resolved Location.
pub struct ConversationMatch {
    /// Exact durable event reference.
    pub event: Rc<ConversationLocationEvent>,
    /// Optional envelope-level presentation view.
    pub view: Option<Rc<Value>>,
    /// Definition-local lifecycle role.
    pub role: ConversationMatchRole,
    /// Current engine-owned Location.
    pub location: ConversationLocation,
}

/// Target-neutral materialized business Node.
pub struct ConversationViewNode {
    /// Engine-owned Context key.
    pub key: String,
    /// Definition kind.
    pub kind: String,
    /// Definition-local business identity.
    pub id: String,
    /// Registered view target.
    pub target: String,
    /// Complete JSON-compatible target data.
    pub data: Rc<Value>,
    /// Chat-only ordering, placement, and visibility metadata.
    pub chat: Option<ChatConversationViewMetadata>,
}

/// Chat target fields carried in addition to target-neutral Node identity/data.
#[derive(Clone)]
pub struct ChatConversationViewMetadata {
    /// Numeric ordering anchor; may be fractional for synthetic command rows.
    pub anchor_seq: f64,
    /// Engine-owned Turn/Step placement.
    pub location: ConversationLocation,
    /// Whether the Chat builder includes this Node in the visible stream.
    pub visibility: ConversationVisibility,
}

/// Closed Chat visibility vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConversationVisibility {
    /// Node participates in the visible Chat stream.
    Visible,
    /// Node remains materialized but contributes no visible row.
    Hidden,
}

/// Immutable public view of one assembled business Context.
pub struct ConversationNodeContext {
    /// Engine-owned Context key.
    pub key: String,
    /// Definition kind.
    pub kind: String,
    /// Definition-local identity.
    pub id: String,
    /// Identity-stable Match collection on the append path.
    pub matches: Rc<RefCell<Vec<Rc<ConversationMatch>>>>,
    /// Unique start Match when loaded.
    pub start: Option<Rc<ConversationMatch>>,
    /// Current business State.
    pub state: Option<Rc<Value>>,
    /// Latest materialized Node or null per target.
    pub current: Rc<RefCell<IndexMap<String, Option<Rc<ConversationViewNode>>>>>,
}

/// Read-only predecessor returned to a Definition start callback.
pub struct ConversationPreviousContext {
    /// Engine-owned predecessor key.
    pub key: String,
    /// Definition kind.
    pub kind: String,
    /// Definition-local identity.
    pub id: String,
    /// Start event sequence.
    pub start_seq: u64,
    /// Current predecessor State.
    pub state: Rc<Value>,
    /// Complete predecessor Match collection.
    pub matches: Rc<RefCell<Vec<Rc<ConversationMatch>>>>,
}

/// Strictly-backward Context lookup available only during start evaluation.
pub trait ConversationContextReader {
    /// Reads the nearest initialized predecessor without recording a dependency.
    fn peek_previous(&mut self, kind: &str) -> Option<ConversationPreviousContext>;

    /// Returns the nearest initialized predecessor of `kind`.
    fn previous(&mut self, kind: &str) -> Option<ConversationPreviousContext>;
}

type MatchCallback = Rc<
    dyn Fn(
        &ConversationLocationEvent,
    ) -> Result<Option<ConversationMatchResult>, ConversationAssemblerError>,
>;
type StartCallback = Rc<
    dyn Fn(
        &ConversationNodeContext,
        &Rc<ConversationMatch>,
        &mut dyn ConversationContextReader,
    ) -> Result<Option<Rc<Value>>, ConversationAssemblerError>,
>;
type UpdateCallback = Rc<
    dyn Fn(
        &ConversationNodeContext,
        &Rc<ConversationMatch>,
    ) -> Result<Option<Rc<Value>>, ConversationAssemblerError>,
>;
type PublicationCallback =
    Rc<dyn Fn(&ConversationMatch) -> Result<ConversationPublication, ConversationAssemblerError>>;
type LocationDataCallback = Rc<
    dyn Fn(
        &ConversationNodeContext,
        ConversationLocationDataScope,
    ) -> Result<Option<Rc<ConversationLocationData>>, ConversationAssemblerError>,
>;
type NodeCallback = Rc<
    dyn Fn(
        &ConversationNodeContext,
    ) -> Result<Option<Rc<ConversationViewNode>>, ConversationAssemblerError>,
>;

/// Engine-owned Location-data publication phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConversationLocationDataScope {
    /// Step values publish first.
    Step,
    /// Turn values publish after Step readers update.
    Turn,
}

impl ConversationLocationDataScope {
    fn name(self) -> &'static str {
        match self {
            Self::Step => "step",
            Self::Turn => "turn",
        }
    }
}

/// One independently registered Event-to-State-to-Node Definition.
pub struct AssemblerNodeDefinition {
    /// Unique Definition kind and Location-data key.
    pub kind: String,
    /// Sole view target, absent for state-only Definitions.
    pub target: Option<String>,
    /// Pure event matcher.
    pub match_event: MatchCallback,
    /// State initializer.
    pub start: StartCallback,
    /// Post-start State fold.
    pub update: UpdateCallback,
    /// Optional publication cadence selector.
    pub publication: Option<PublicationCallback>,
    /// Optional Step/Turn data publisher.
    pub build_location_data: Option<LocationDataCallback>,
    /// Optional final view-Node builder.
    pub build_view_node: Option<NodeCallback>,
}

/// Event Registry subset consumed by one Session assembler.
pub trait AssemblerEventDefinitions {
    /// Ordinary Definitions in registration order.
    fn entries(&self) -> Vec<Rc<AssemblerNodeDefinition>>;
    /// Unmatched-event fallback.
    fn fallback_entry(&self) -> Option<Rc<AssemblerNodeDefinition>>;
}

/// Per-target incremental view builder.
pub trait AssemblerViewBuilder {
    /// Empty target snapshot.
    fn empty(&self) -> Rc<Value>;
    /// Replaces the complete target Node set.
    ///
    /// # Errors
    ///
    /// Returns a target-specific replacement failure.
    fn replace(
        &mut self,
        nodes: &[Rc<ConversationViewNode>],
        timeline: Rc<ConversationTimelineSnapshot>,
    ) -> Result<Rc<Value>, ConversationAssemblerError>;
    /// Applies changed target Nodes.
    ///
    /// # Errors
    ///
    /// Returns a target-specific incremental-update failure.
    fn apply(
        &mut self,
        upserts: &[Rc<ConversationViewNode>],
        timeline: Rc<ConversationTimelineSnapshot>,
    ) -> Result<Rc<Value>, ConversationAssemblerError>;
}

/// Registry contribution creating one isolated target builder per Session.
pub struct AssemblerViewDefinition {
    /// Unique target name.
    pub target: String,
    /// Session-local builder factory.
    pub create: Rc<dyn Fn() -> Box<dyn AssemblerViewBuilder>>,
}

/// View Registry subset consumed by one Session assembler.
pub trait AssemblerViewDefinitions {
    /// Builder factories in registration order.
    fn entries(&self) -> Vec<Rc<AssemblerViewDefinition>>;
}

#[derive(Clone)]
struct Dependency {
    kind: String,
    key: Option<String>,
    revision: Option<u64>,
    window_gap: bool,
}

struct InternalContext {
    key: String,
    kind: String,
    id: String,
    definition: Rc<AssemblerNodeDefinition>,
    start_seq: Option<u64>,
    start: Option<Rc<ConversationMatch>>,
    matches: Rc<RefCell<Vec<Rc<ConversationMatch>>>>,
    state: Option<Rc<Value>>,
    revision: u64,
    current: Rc<RefCell<IndexMap<String, Option<Rc<ConversationViewNode>>>>>,
    location_data: [Option<Rc<ConversationLocationData>>; 2],
    dependencies: IndexMap<String, Dependency>,
}

struct PendingMatch {
    definition: Rc<AssemblerNodeDefinition>,
    id: String,
    accepted: Rc<ConversationMatch>,
}

struct ViewState {
    target: String,
    builder: Box<dyn AssemblerViewBuilder>,
    snapshot: Rc<Value>,
}

/// Fail-loud Definition, lifecycle, dependency, Location, or builder error.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ConversationAssemblerError(String);

impl ConversationAssemblerError {
    /// Wraps a Definition or target-specific failure.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl From<ConversationLocationError> for ConversationAssemblerError {
    fn from(error: ConversationLocationError) -> Self {
        Self(error.to_string())
    }
}

/// Session-owned incremental business-Context assembler.
pub struct ConversationNodeAssembler {
    event_definitions: Rc<dyn AssemblerEventDefinitions>,
    view_definitions: Rc<dyn AssemblerViewDefinitions>,
    contexts: IndexMap<String, Rc<RefCell<InternalContext>>>,
    contexts_by_kind: IndexMap<String, Vec<String>>,
    contexts_by_seq: IndexMap<u64, IndexSet<String>>,
    inputs: IndexMap<u64, ConversationEventInput>,
    location_index: ConversationLocationIndex,
    dirty: IndexSet<String>,
    revised: IndexSet<String>,
    dependents: IndexMap<String, IndexSet<String>>,
    views: IndexMap<String, ViewState>,
    has_more: bool,
    replace_pending: bool,
    timeline_dirty: bool,
}

impl ConversationNodeAssembler {
    /// Creates one Session assembler and its initial target builders.
    #[must_use]
    pub fn new(
        event_definitions: Rc<dyn AssemblerEventDefinitions>,
        view_definitions: Rc<dyn AssemblerViewDefinitions>,
    ) -> Self {
        let mut assembler = Self {
            event_definitions,
            view_definitions,
            contexts: IndexMap::new(),
            contexts_by_kind: IndexMap::new(),
            contexts_by_seq: IndexMap::new(),
            inputs: IndexMap::new(),
            location_index: ConversationLocationIndex::default(),
            dirty: IndexSet::new(),
            revised: IndexSet::new(),
            dependents: IndexMap::new(),
            views: IndexMap::new(),
            has_more: false,
            replace_pending: true,
            timeline_dirty: true,
        };
        assembler.reset_view_builders();
        assembler
    }

    /// Replaces the complete loaded window after open, resync, or gap repair.
    ///
    /// # Errors
    ///
    /// Returns the first boundary, Definition, lifecycle, or dependency failure.
    pub fn replace_window(
        &mut self,
        entries: &[ConversationEventInput],
        has_more: bool,
    ) -> Result<ConversationPublication, ConversationAssemblerError> {
        self.contexts.clear();
        self.contexts_by_kind.clear();
        self.contexts_by_seq.clear();
        self.inputs.clear();
        self.dirty.clear();
        self.revised.clear();
        self.dependents.clear();
        self.has_more = has_more;
        let mut sorted = entries.to_vec();
        sorted.sort_by_key(|input| input.event.seq);
        for entry in &sorted {
            self.inputs.insert(entry.event.seq, entry.clone());
        }
        self.location_index.rebuild(&sorted)?;
        self.timeline_dirty = true;
        for entry in &sorted {
            self.match_input(entry)?;
        }
        self.replay_dependencies()?;
        self.revised.clear();
        self.dirty.extend(self.contexts.keys().cloned());
        self.replace_pending = true;
        Ok(ConversationPublication::Immediate)
    }

    /// Adds one contiguous live tail event without scanning existing Contexts.
    ///
    /// # Errors
    ///
    /// Returns boundary, Definition, lifecycle, or dependent-replay failures.
    pub fn append(
        &mut self,
        input: &ConversationEventInput,
    ) -> Result<ConversationPublication, ConversationAssemblerError> {
        if self.inputs.contains_key(&input.event.seq) {
            return Ok(ConversationPublication::None);
        }
        self.revised.clear();
        self.inputs.insert(input.event.seq, input.clone());
        let mut publication = ConversationPublication::None;
        if is_location_boundary(&input.event.event_type) {
            let previous_timeline = self.location_index.snapshot();
            let changed = self.location_index.append_boundary(&input.event)?;
            if !Rc::ptr_eq(&self.location_index.snapshot(), &previous_timeline) {
                self.timeline_dirty = true;
                publication = ConversationPublication::Immediate;
            }
            let affected = self.refresh_match_locations(&changed);
            self.replay_contexts(&affected)?;
            if !changed.is_empty() {
                publication = ConversationPublication::Immediate;
            }
        } else {
            self.location_index.append_non_boundary(&input.event);
        }
        publication = publication.maximum(self.match_input(input)?);
        if self.replay_revised_dependents()? {
            publication = ConversationPublication::Immediate;
        }
        self.revised.clear();
        Ok(publication)
    }

    /// Prepends one older page while preserving existing Context and view identities.
    ///
    /// # Errors
    ///
    /// Returns boundary, Definition, lifecycle, dependency, or merge failures.
    pub fn prepend(
        &mut self,
        entries: &[ConversationEventInput],
        has_more: bool,
    ) -> Result<ConversationPublication, ConversationAssemblerError> {
        self.revised.clear();
        let mut publication = ConversationPublication::None;
        let previous_has_more = self.has_more;
        let mut fresh = entries
            .iter()
            .filter(|entry| !self.inputs.contains_key(&entry.event.seq))
            .cloned()
            .collect::<Vec<_>>();
        fresh.sort_by_key(|entry| entry.event.seq);
        for entry in &fresh {
            self.inputs.insert(entry.event.seq, entry.clone());
        }
        self.has_more = has_more;
        let previous_timeline = self.location_index.snapshot();
        let changed_locations = self.location_index.rebuild(&self.sorted_inputs())?;
        if !Rc::ptr_eq(&self.location_index.snapshot(), &previous_timeline) {
            self.timeline_dirty = true;
        }
        let mut affected = self.refresh_match_locations(&changed_locations);
        let mut pending = IndexMap::<String, Vec<PendingMatch>>::new();
        for entry in &fresh {
            publication = publication.maximum(self.collect_input(entry, &mut pending)?);
        }
        self.apply_pending_matches(&pending, &mut affected)?;
        self.replay_contexts(&affected)?;
        if (!self.revised.is_empty() || previous_has_more != has_more)
            && self.replay_dependencies()?
        {
            publication = ConversationPublication::Immediate;
        }
        if !changed_locations.is_empty() {
            publication = ConversationPublication::Immediate;
        }
        self.revised.clear();
        Ok(publication)
    }

    /// Rebuilds against the current low-frequency Registry set.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::replace_window`].
    pub fn rebuild_registry(
        &mut self,
    ) -> Result<ConversationPublication, ConversationAssemblerError> {
        self.reset_view_builders();
        self.replace_window(&self.sorted_inputs(), self.has_more)
    }

    /// Materializes dirty Contexts and advances every registered view builder.
    ///
    /// # Errors
    ///
    /// Returns Location-data, Node-contract, withdrawal, or builder failures.
    pub fn flush(&mut self) -> Result<bool, ConversationAssemblerError> {
        if !self.replace_pending && self.dirty.is_empty() && !self.timeline_dirty {
            return Ok(false);
        }
        if self.replace_pending {
            self.replace_location_data()?;
            let mut all_by_target = self
                .views
                .keys()
                .map(|target| (target.clone(), Vec::new()))
                .collect::<IndexMap<String, Vec<Rc<ConversationViewNode>>>>();
            let context_keys = self.contexts.keys().cloned().collect::<Vec<_>>();
            for key in context_keys {
                let context = self.context(&key)?;
                let target = context.borrow().definition.target.clone();
                let Some(target) = target.filter(|target| self.views.contains_key(target)) else {
                    continue;
                };
                let node = Self::build_node(&context, &target)?;
                context
                    .borrow()
                    .current
                    .borrow_mut()
                    .insert(target.clone(), node.clone());
                if let Some(node) = node {
                    all_by_target.entry(target).or_default().push(node);
                }
            }
            let timeline = self.location_index.snapshot();
            for view in self.views.values_mut() {
                view.snapshot = view.builder.replace(
                    all_by_target.get(&view.target).map_or(&[], Vec::as_slice),
                    timeline.clone(),
                )?;
            }
            self.replace_pending = false;
            self.dirty.clear();
            self.timeline_dirty = false;
            return Ok(true);
        }

        let mut upserts_by_target = self
            .views
            .keys()
            .map(|target| (target.clone(), Vec::new()))
            .collect::<IndexMap<String, Vec<Rc<ConversationViewNode>>>>();
        if self.apply_dirty_location_data()? {
            self.timeline_dirty = true;
        }
        let dirty = self.dirty.iter().cloned().collect::<Vec<_>>();
        for key in dirty {
            let context = self.context(&key)?;
            let target = context.borrow().definition.target.clone();
            let Some(target) = target.filter(|target| self.views.contains_key(target)) else {
                continue;
            };
            let previous = context
                .borrow()
                .current
                .borrow()
                .get(&target)
                .cloned()
                .flatten();
            let node = Self::build_node(&context, &target)?;
            if node.is_none() && previous.is_some() {
                return Err(ConversationAssemblerError::new(format!(
                    "conversation Definition \"{}\" withdrew materialized target \"{target}\"; return the same key with hidden visibility instead",
                    context.borrow().kind
                )));
            }
            context
                .borrow()
                .current
                .borrow_mut()
                .insert(target.clone(), node.clone());
            if let Some(node) = node {
                upserts_by_target.entry(target).or_default().push(node);
            }
        }
        self.dirty.clear();
        let timeline_dirty = self.timeline_dirty;
        self.timeline_dirty = false;
        let timeline = self.location_index.snapshot();
        for view in self.views.values_mut() {
            let upserts = upserts_by_target
                .get(&view.target)
                .map_or(&[] as &[Rc<ConversationViewNode>], Vec::as_slice);
            if upserts.is_empty() && !timeline_dirty {
                continue;
            }
            view.snapshot = view.builder.apply(upserts, timeline.clone())?;
        }
        Ok(true)
    }

    /// Returns the latest snapshot for one registered target.
    #[must_use]
    pub fn snapshot(&self, target: &str) -> Option<Rc<Value>> {
        self.views.get(target).map(|view| view.snapshot.clone())
    }

    fn sorted_inputs(&self) -> Vec<ConversationEventInput> {
        let mut inputs = self.inputs.values().cloned().collect::<Vec<_>>();
        inputs.sort_by_key(|input| input.event.seq);
        inputs
    }

    fn context(
        &self,
        key: &str,
    ) -> Result<Rc<RefCell<InternalContext>>, ConversationAssemblerError> {
        self.contexts.get(key).cloned().ok_or_else(|| {
            ConversationAssemblerError::new(format!("conversation Context {key} disappeared"))
        })
    }

    fn reset_view_builders(&mut self) {
        self.views.clear();
        for definition in self.view_definitions.entries() {
            let builder = (definition.create)();
            let snapshot = builder.empty();
            self.views.insert(
                definition.target.clone(),
                ViewState {
                    target: definition.target.clone(),
                    builder,
                    snapshot,
                },
            );
        }
        self.replace_pending = true;
    }

    fn matching_definitions(
        &self,
        input: &ConversationEventInput,
    ) -> Result<
        Vec<(Rc<AssemblerNodeDefinition>, ConversationMatchResult)>,
        ConversationAssemblerError,
    > {
        let mut matched_targets = IndexSet::new();
        let mut matches = Vec::new();
        for definition in self.event_definitions.entries() {
            let Some(result) = (definition.match_event)(&input.event)? else {
                continue;
            };
            if let Some(target) = &definition.target {
                matched_targets.insert(target.clone());
            }
            matches.push((definition, result));
        }
        if let Some(fallback) = self.event_definitions.fallback_entry()
            && let Some(target) = &fallback.target
            && !matched_targets.contains(target)
            && let Some(result) = (fallback.match_event)(&input.event)?
        {
            matches.push((fallback, result));
        }
        Ok(matches)
    }

    fn match_input(
        &mut self,
        input: &ConversationEventInput,
    ) -> Result<ConversationPublication, ConversationAssemblerError> {
        let mut publication = ConversationPublication::None;
        for (definition, result) in self.matching_definitions(input)? {
            publication = publication.maximum(self.accept_match(
                &definition,
                result.id,
                result.role,
                input,
            )?);
        }
        Ok(publication)
    }

    fn collect_input(
        &self,
        input: &ConversationEventInput,
        pending: &mut IndexMap<String, Vec<PendingMatch>>,
    ) -> Result<ConversationPublication, ConversationAssemblerError> {
        let mut publication = ConversationPublication::None;
        for (definition, result) in self.matching_definitions(input)? {
            let key = conversation_context_key(&definition.kind, &result.id);
            let accepted = Rc::new(ConversationMatch {
                event: input.event.clone(),
                view: input.view.clone(),
                role: result.role,
                location: self.location_index.location_of(&input.event),
            });
            publication = publication.maximum(publication_for(&definition, &accepted)?);
            pending.entry(key).or_default().push(PendingMatch {
                definition,
                id: result.id,
                accepted,
            });
        }
        Ok(publication)
    }

    fn accept_match(
        &mut self,
        definition: &Rc<AssemblerNodeDefinition>,
        id: String,
        role: ConversationMatchRole,
        input: &ConversationEventInput,
    ) -> Result<ConversationPublication, ConversationAssemblerError> {
        let key = conversation_context_key(&definition.kind, &id);
        if role == ConversationMatchRole::Start
            && self
                .contexts
                .get(&key)
                .is_some_and(|context| context.borrow().start.is_some())
        {
            return Err(ConversationAssemblerError::new(format!(
                "conversation Context {key} received more than one start Match"
            )));
        }
        let context = if let Some(context) = self.contexts.get(&key) {
            context.clone()
        } else {
            let context = new_context(key.clone(), definition.clone(), id);
            self.contexts.insert(key.clone(), context.clone());
            context
        };
        let accepted = Rc::new(ConversationMatch {
            event: input.event.clone(),
            view: input.view.clone(),
            role,
            location: self.location_index.location_of(&input.event),
        });
        {
            let current = context.borrow();
            if current
                .matches
                .borrow()
                .last()
                .is_some_and(|previous| previous.event.seq >= input.event.seq)
            {
                return Err(ConversationAssemblerError::new(format!(
                    "conversation Context {key} received non-appended Match {}",
                    input.event.seq
                )));
            }
            if role == ConversationMatchRole::Start && !current.matches.borrow().is_empty() {
                return Err(ConversationAssemblerError::new(format!(
                    "conversation Context {key} received an update before its start Match"
                )));
            }
        }
        context.borrow().matches.borrow_mut().push(accepted.clone());
        if role == ConversationMatchRole::Start {
            {
                let mut current = context.borrow_mut();
                current.start_seq = Some(input.event.seq);
                current.start = Some(accepted.clone());
            }
            self.index_started_context(&key);
        }
        self.contexts_by_seq
            .entry(input.event.seq)
            .or_default()
            .insert(key.clone());

        if role == ConversationMatchRole::Start {
            self.replay_context(&key)?;
        } else if context.borrow().state.is_some() {
            let snapshot = context_snapshot(&context);
            let next = (definition.update)(&snapshot, &accepted)?;
            context.borrow_mut().state = Some(require_state(definition, "update", next)?);
            let revision = context.borrow().revision.wrapping_add(1);
            context.borrow_mut().revision = revision;
            self.revised.insert(key.clone());
        }
        self.dirty.insert(key);
        publication_for(definition, &accepted)
    }

    fn apply_pending_matches(
        &mut self,
        pending: &IndexMap<String, Vec<PendingMatch>>,
        affected: &mut IndexSet<String>,
    ) -> Result<(), ConversationAssemblerError> {
        let mut starts_by_kind = IndexMap::<String, Vec<String>>::new();
        for (key, entries) in pending {
            let Some(first) = entries.first() else {
                continue;
            };
            let context = if let Some(context) = self.contexts.get(key) {
                context.clone()
            } else {
                let context = new_context(key.clone(), first.definition.clone(), first.id.clone());
                self.contexts.insert(key.clone(), context.clone());
                context
            };
            let mut discovered_start = None;
            let mut additions = Vec::new();
            for entry in entries {
                let current = context.borrow();
                if !Rc::ptr_eq(&entry.definition, &current.definition) || entry.id != current.id {
                    return Err(ConversationAssemblerError::new(format!(
                        "conversation Context {key} received inconsistent Definition identity"
                    )));
                }
                drop(current);
                if entry.accepted.role == ConversationMatchRole::Start {
                    if discovered_start.is_some() || context.borrow().start.is_some() {
                        return Err(ConversationAssemblerError::new(format!(
                            "conversation Context {key} received more than one start Match"
                        )));
                    }
                    discovered_start = Some(entry.accepted.clone());
                }
                self.contexts_by_seq
                    .entry(entry.accepted.event.seq)
                    .or_default()
                    .insert(key.clone());
                additions.push(entry.accepted.clone());
            }
            additions.sort_by_key(|accepted| accepted.event.seq);
            let existing = context.borrow().matches.borrow().clone();
            context.borrow_mut().matches =
                Rc::new(RefCell::new(merge_matches(key, &additions, &existing)?));
            if let Some(start) = discovered_start {
                let mut current = context.borrow_mut();
                current.start_seq = Some(start.event.seq);
                current.start = Some(start);
                starts_by_kind
                    .entry(current.kind.clone())
                    .or_default()
                    .push(key.clone());
            }
            let invalid_start_order = {
                let current = context.borrow();
                current.start.as_ref().is_some_and(|start| {
                    current
                        .matches
                        .borrow()
                        .first()
                        .is_none_or(|first| !Rc::ptr_eq(first, start))
                })
            };
            if invalid_start_order {
                return Err(ConversationAssemblerError::new(format!(
                    "conversation Context {key} received an update before its start Match"
                )));
            }
            affected.insert(key.clone());
            self.dirty.insert(key.clone());
        }
        for (kind, starts) in starts_by_kind {
            self.index_started_contexts(&kind, &starts);
        }
        Ok(())
    }

    fn replay_contexts(
        &mut self,
        contexts: &IndexSet<String>,
    ) -> Result<(), ConversationAssemblerError> {
        let mut ordered = contexts.iter().cloned().collect::<Vec<_>>();
        ordered.sort_by(|left, right| {
            compare_start_seq(self.contexts.get(left), self.contexts.get(right))
        });
        for key in ordered {
            if self.context(&key)?.borrow().start.is_none() {
                self.context(&key)?.borrow_mut().state = None;
                self.dirty.insert(key);
            } else {
                self.replay_context(&key)?;
            }
        }
        Ok(())
    }

    fn replay_context(&mut self, key: &str) -> Result<(), ConversationAssemblerError> {
        let context = self.context(key)?;
        let Some(start) = context.borrow().start.clone() else {
            context.borrow_mut().state = None;
            return Ok(());
        };
        if context
            .borrow()
            .matches
            .borrow()
            .first()
            .is_none_or(|first| !Rc::ptr_eq(first, &start))
        {
            return Err(ConversationAssemblerError::new(format!(
                "conversation Context {key} received an update before its start Match"
            )));
        }
        let definition = context.borrow().definition.clone();
        let mut dependencies = IndexMap::new();
        context.borrow_mut().state = None;
        let snapshot = context_snapshot(&context);
        let state = {
            let mut reader = AssemblerReader {
                assembler: self,
                before_seq: start.event.seq,
                dependencies: &mut dependencies,
            };
            (definition.start)(&snapshot, &start, &mut reader)?
        };
        context.borrow_mut().state = Some(require_state(&definition, "start", state)?);
        self.replace_dependencies(key, dependencies)?;
        let matches = context.borrow().matches.borrow().clone();
        for accepted in matches.iter().skip(1) {
            if accepted.role != ConversationMatchRole::Update {
                continue;
            }
            let snapshot = context_snapshot(&context);
            let state = (definition.update)(&snapshot, accepted)?;
            context.borrow_mut().state = Some(require_state(&definition, "update", state)?);
        }
        let revision = context.borrow().revision.wrapping_add(1);
        context.borrow_mut().revision = revision;
        self.revised.insert(key.to_owned());
        self.dirty.insert(key.to_owned());
        Ok(())
    }

    fn replace_dependencies(
        &mut self,
        key: &str,
        dependencies: IndexMap<String, Dependency>,
    ) -> Result<(), ConversationAssemblerError> {
        let context = self.context(key)?;
        let previous = context.borrow().dependencies.clone();
        for dependency in previous.values() {
            let Some(provider) = &dependency.key else {
                continue;
            };
            if let Some(current) = self.dependents.get_mut(provider) {
                current.shift_remove(key);
                if current.is_empty() {
                    self.dependents.shift_remove(provider);
                }
            }
        }
        for dependency in dependencies.values() {
            let Some(provider) = &dependency.key else {
                continue;
            };
            self.dependents
                .entry(provider.clone())
                .or_default()
                .insert(key.to_owned());
        }
        context.borrow_mut().dependencies = dependencies;
        Ok(())
    }

    fn replay_revised_dependents(&mut self) -> Result<bool, ConversationAssemblerError> {
        let mut pending = self.revised.iter().cloned().collect::<Vec<_>>();
        let mut affected = IndexSet::new();
        let mut index = 0;
        while index < pending.len() {
            let dependency = pending[index].clone();
            index += 1;
            for dependent in self
                .dependents
                .get(&dependency)
                .cloned()
                .unwrap_or_default()
            {
                if affected.insert(dependent.clone()) {
                    pending.push(dependent);
                }
            }
        }
        self.replay_contexts(&affected)?;
        Ok(!affected.is_empty())
    }

    fn previous_context(&self, kind: &str, before_seq: u64) -> Option<String> {
        let candidates = self
            .contexts_by_kind
            .get(kind)
            .map_or(&[] as &[String], Vec::as_slice);
        let index_before = insertion_index(candidates, before_seq, &self.contexts);
        (0..index_before).rev().find_map(|index| {
            let key = candidates.get(index)?;
            self.contexts
                .get(key)
                .filter(|context| context.borrow().state.is_some())
                .map(|_| key.clone())
        })
    }

    fn index_started_context(&mut self, key: &str) {
        let Some(context) = self.contexts.get(key) else {
            return;
        };
        let (kind, Some(seq)) = (context.borrow().kind.clone(), context.borrow().start_seq) else {
            return;
        };
        let candidates = self.contexts_by_kind.entry(kind).or_default();
        let append = candidates.last().is_none_or(|previous| {
            self.contexts
                .get(previous)
                .and_then(|context| context.borrow().start_seq)
                .is_some_and(|previous| previous < seq)
        });
        if append {
            candidates.push(key.to_owned());
            return;
        }
        let at = insertion_index(candidates, seq, &self.contexts);
        candidates.insert(at, key.to_owned());
    }

    fn index_started_contexts(&mut self, kind: &str, additions: &[String]) {
        if additions.is_empty() {
            return;
        }
        let mut sorted = additions.to_vec();
        sorted.sort_by_key(|key| {
            self.contexts
                .get(key)
                .and_then(|context| context.borrow().start_seq)
                .unwrap_or(u64::MAX)
        });
        let existing = self.contexts_by_kind.get(kind).cloned().unwrap_or_default();
        let mut merged = Vec::with_capacity(existing.len() + sorted.len());
        let mut before = 0;
        let mut added = 0;
        while before < existing.len() || added < sorted.len() {
            let left = existing.get(before);
            let right = sorted.get(added);
            let left_seq = left.and_then(|key| {
                self.contexts
                    .get(key)
                    .and_then(|context| context.borrow().start_seq)
            });
            let right_seq = right.and_then(|key| {
                self.contexts
                    .get(key)
                    .and_then(|context| context.borrow().start_seq)
            });
            if right.is_none() || left.is_some() && left_seq < right_seq {
                merged.push(left.cloned().unwrap_or_default());
                before += 1;
            } else {
                merged.push(right.cloned().unwrap_or_default());
                added += 1;
            }
        }
        self.contexts_by_kind.insert(kind.to_owned(), merged);
    }

    fn replay_dependencies(&mut self) -> Result<bool, ConversationAssemblerError> {
        let mut ordered = self
            .contexts
            .iter()
            .filter_map(|(key, context)| context.borrow().start_seq.map(|seq| (seq, key.clone())))
            .collect::<Vec<_>>();
        ordered.sort_by_key(|(seq, _)| *seq);
        let mut replayed = false;
        for (_, key) in ordered {
            let context = self.context(&key)?;
            let (state_present, before, dependencies) = {
                let current = context.borrow();
                (
                    current.state.is_some(),
                    current.start_seq,
                    current.dependencies.clone(),
                )
            };
            if !state_present || dependencies.is_empty() {
                continue;
            }
            let Some(before) = before else {
                continue;
            };
            let changed = dependencies.values().any(|dependency| {
                let current = self.previous_context(&dependency.kind, before);
                let (current_key, current_revision) = current
                    .as_ref()
                    .and_then(|key| self.contexts.get(key).map(|context| (key, context)))
                    .map_or((None, None), |(key, context)| {
                        (Some(key.clone()), Some(context.borrow().revision))
                    });
                let window_gap = current.is_none() && self.has_more;
                current_key != dependency.key
                    || current_revision != dependency.revision
                    || window_gap != dependency.window_gap
            });
            if changed {
                self.replay_context(&key)?;
                replayed = true;
            }
        }
        Ok(replayed)
    }

    fn refresh_match_locations(&mut self, changed_seqs: &IndexSet<u64>) -> IndexSet<String> {
        let mut affected = IndexSet::new();
        for seq in changed_seqs {
            affected.extend(self.contexts_by_seq.get(seq).cloned().unwrap_or_default());
        }
        for key in &affected {
            let Some(context) = self.contexts.get(key) else {
                continue;
            };
            let old_start = context.borrow().start.clone();
            let refreshed = context
                .borrow()
                .matches
                .borrow()
                .iter()
                .map(|accepted| {
                    if !changed_seqs.contains(&accepted.event.seq) {
                        return accepted.clone();
                    }
                    Rc::new(ConversationMatch {
                        event: accepted.event.clone(),
                        view: accepted.view.clone(),
                        role: accepted.role,
                        location: self.location_index.location_of(&accepted.event),
                    })
                })
                .collect::<Vec<_>>();
            let new_start = old_start.as_ref().and_then(|start| {
                refreshed
                    .iter()
                    .find(|accepted| {
                        accepted.event.seq == start.event.seq && accepted.role == start.role
                    })
                    .cloned()
            });
            let mut current = context.borrow_mut();
            current.matches = Rc::new(RefCell::new(refreshed));
            current.start = new_start;
        }
        affected
    }

    fn build_node(
        context: &Rc<RefCell<InternalContext>>,
        target: &str,
    ) -> Result<Option<Rc<ConversationViewNode>>, ConversationAssemblerError> {
        let definition = context.borrow().definition.clone();
        let Some(builder) = &definition.build_view_node else {
            return Ok(None);
        };
        if definition.target.as_deref() != Some(target) {
            return Ok(None);
        }
        let Some(node) = builder(&context_snapshot(context))? else {
            return Ok(None);
        };
        let current = context.borrow();
        if node.key != current.key {
            return Err(ConversationAssemblerError::new(format!(
                "conversation Definition \"{}\" returned unstable key \"{}\"; expected \"{}\"",
                current.kind, node.key, current.key
            )));
        }
        if node.target != target {
            return Err(ConversationAssemblerError::new(format!(
                "conversation Definition \"{}\" returned target \"{}\" while building \"{target}\"",
                current.kind, node.target
            )));
        }
        if target == "chat" && node.chat.is_none() {
            return Err(ConversationAssemblerError::new(format!(
                "conversation Definition \"{}\" returned a Chat Node without anchorSeq, location, and visibility",
                current.kind
            )));
        }
        if target != "chat" && node.chat.is_some() {
            return Err(ConversationAssemblerError::new(format!(
                "conversation Definition \"{}\" returned Chat metadata for target \"{target}\"",
                current.kind
            )));
        }
        Ok(Some(node))
    }

    fn build_location_data(
        context: &Rc<RefCell<InternalContext>>,
        scope: ConversationLocationDataScope,
    ) -> Result<Option<Rc<ConversationLocationData>>, ConversationAssemblerError> {
        let definition = context.borrow().definition.clone();
        let Some(builder) = &definition.build_location_data else {
            return Ok(None);
        };
        let Some(data) = builder(&context_snapshot(context), scope)? else {
            return Ok(None);
        };
        let (kind, turn, step, key) = match data.as_ref() {
            ConversationLocationData::Turn { turn, key, .. } => {
                (ConversationLocationDataScope::Turn, *turn, None, key)
            }
            ConversationLocationData::Step {
                turn, step, key, ..
            } => (ConversationLocationDataScope::Step, *turn, *step, key),
        };
        if kind != scope {
            return Err(ConversationAssemblerError::new(format!(
                "conversation Definition \"{}\" published {} data through its {} scope",
                definition.kind,
                kind.name(),
                scope.name()
            )));
        }
        if key != &definition.kind {
            return Err(ConversationAssemblerError::new(format!(
                "conversation Definition \"{}\" published Location data key \"{key}\"; expected its owned kind",
                definition.kind
            )));
        }
        if turn > 9_007_199_254_740_991 {
            return Err(ConversationAssemblerError::new(format!(
                "conversation Definition \"{}\" published invalid turn {turn}",
                definition.kind
            )));
        }
        if kind == ConversationLocationDataScope::Step
            && step.is_none_or(|step| step > 9_007_199_254_740_991)
        {
            return Err(ConversationAssemblerError::new(format!(
                "conversation Definition \"{}\" published invalid step {}",
                definition.kind,
                step.map_or_else(|| "undefined".to_owned(), |step| step.to_string())
            )));
        }
        Ok(Some(data))
    }

    fn replace_location_data(&mut self) -> Result<(), ConversationAssemblerError> {
        let mut entries = Vec::new();
        for scope in [
            ConversationLocationDataScope::Step,
            ConversationLocationDataScope::Turn,
        ] {
            let keys = self.contexts.keys().cloned().collect::<Vec<_>>();
            for key in keys {
                let context = self.context(&key)?;
                let data = Self::build_location_data(&context, scope)?;
                context.borrow_mut().location_data[scope_index(scope)].clone_from(&data);
                if let Some(data) = data {
                    entries.push(ConversationOwnedLocationData {
                        owner: key,
                        data: data.as_ref().clone(),
                    });
                }
            }
            self.location_index.replace_data(&entries)?;
        }
        Ok(())
    }

    fn apply_dirty_location_data(&mut self) -> Result<bool, ConversationAssemblerError> {
        let mut did_change = false;
        for scope in [
            ConversationLocationDataScope::Step,
            ConversationLocationDataScope::Turn,
        ] {
            let mut data_changes = Vec::new();
            let dirty = self.dirty.iter().cloned().collect::<Vec<_>>();
            for key in dirty {
                let context = self.context(&key)?;
                let previous = context.borrow().location_data[scope_index(scope)].clone();
                let next = Self::build_location_data(&context, scope)?;
                context.borrow_mut().location_data[scope_index(scope)].clone_from(&next);
                if !same_location_data(previous.as_ref(), next.as_ref()) {
                    data_changes.push(ConversationLocationDataChange {
                        owner: key,
                        previous: previous.as_ref().map(|data| data.as_ref().clone()),
                        next: next.as_ref().map(|data| data.as_ref().clone()),
                    });
                }
            }
            did_change = self.location_index.apply_data(&data_changes)? || did_change;
        }
        Ok(did_change)
    }
}

/// Builds a collision-free Context key using JavaScript UTF-16 length semantics.
#[must_use]
pub fn conversation_context_key(kind: &str, id: &str) -> String {
    format!("{}:{kind}{id}", kind.encode_utf16().count())
}

fn is_location_boundary(event_type: &str) -> bool {
    matches!(
        event_type,
        "turn/start" | "turn/end" | "step/start" | "step/end"
    )
}

struct AssemblerReader<'a> {
    assembler: &'a mut ConversationNodeAssembler,
    before_seq: u64,
    dependencies: &'a mut IndexMap<String, Dependency>,
}

impl ConversationContextReader for AssemblerReader<'_> {
    fn peek_previous(&mut self, kind: &str) -> Option<ConversationPreviousContext> {
        let predecessor = self.assembler.previous_context(kind, self.before_seq);
        let context = self.assembler.contexts.get(predecessor.as_ref()?)?.borrow();
        Some(ConversationPreviousContext {
            key: context.key.clone(),
            kind: context.kind.clone(),
            id: context.id.clone(),
            start_seq: context.start_seq?,
            state: context.state.clone()?,
            matches: context.matches.clone(),
        })
    }

    fn previous(&mut self, kind: &str) -> Option<ConversationPreviousContext> {
        let predecessor = self.assembler.previous_context(kind, self.before_seq);
        let revision = predecessor.as_ref().and_then(|key| {
            self.assembler
                .contexts
                .get(key)
                .map(|context| context.borrow().revision)
        });
        self.dependencies.insert(
            kind.to_owned(),
            Dependency {
                kind: kind.to_owned(),
                key: predecessor.clone(),
                revision,
                window_gap: predecessor.is_none() && self.assembler.has_more,
            },
        );
        self.peek_previous(kind)
    }
}

fn new_context(
    key: String,
    definition: Rc<AssemblerNodeDefinition>,
    id: String,
) -> Rc<RefCell<InternalContext>> {
    Rc::new(RefCell::new(InternalContext {
        kind: definition.kind.clone(),
        key,
        id,
        definition,
        start_seq: None,
        start: None,
        matches: Rc::new(RefCell::new(Vec::new())),
        state: None,
        revision: 0,
        current: Rc::new(RefCell::new(IndexMap::new())),
        location_data: [None, None],
        dependencies: IndexMap::new(),
    }))
}

fn context_snapshot(context: &Rc<RefCell<InternalContext>>) -> ConversationNodeContext {
    let current = context.borrow();
    ConversationNodeContext {
        key: current.key.clone(),
        kind: current.kind.clone(),
        id: current.id.clone(),
        matches: current.matches.clone(),
        start: current.start.clone(),
        state: current.state.clone(),
        current: current.current.clone(),
    }
}

fn publication_for(
    definition: &AssemblerNodeDefinition,
    accepted: &ConversationMatch,
) -> Result<ConversationPublication, ConversationAssemblerError> {
    definition
        .publication
        .as_ref()
        .map_or(Ok(ConversationPublication::Immediate), |publication| {
            publication(accepted)
        })
}

fn require_state(
    definition: &AssemblerNodeDefinition,
    phase: &str,
    state: Option<Rc<Value>>,
) -> Result<Rc<Value>, ConversationAssemblerError> {
    state.ok_or_else(|| {
        ConversationAssemblerError::new(format!(
            "conversation Definition \"{}\" returned undefined from {phase}()",
            definition.kind
        ))
    })
}

fn merge_matches(
    key: &str,
    additions: &[Rc<ConversationMatch>],
    existing: &[Rc<ConversationMatch>],
) -> Result<Vec<Rc<ConversationMatch>>, ConversationAssemblerError> {
    let mut merged = Vec::with_capacity(additions.len() + existing.len());
    let mut added = 0;
    let mut current = 0;
    while added < additions.len() || current < existing.len() {
        let left = additions.get(added);
        let right = existing.get(current);
        if let (Some(left), Some(right)) = (left, right)
            && left.event.seq == right.event.seq
        {
            return Err(ConversationAssemblerError::new(format!(
                "conversation Context {key} received duplicate Match {}",
                left.event.seq
            )));
        }
        if right.is_none()
            || left
                .zip(right)
                .is_some_and(|(left, right)| left.event.seq < right.event.seq)
        {
            merged.push(left.cloned().unwrap_or_else(|| unreachable!()));
            added += 1;
        } else {
            merged.push(right.cloned().unwrap_or_else(|| unreachable!()));
            current += 1;
        }
    }
    Ok(merged)
}

fn compare_start_seq(
    left: Option<&Rc<RefCell<InternalContext>>>,
    right: Option<&Rc<RefCell<InternalContext>>>,
) -> Ordering {
    let left = left
        .and_then(|context| context.borrow().start_seq)
        .unwrap_or(u64::MAX);
    let right = right
        .and_then(|context| context.borrow().start_seq)
        .unwrap_or(u64::MAX);
    left.cmp(&right)
}

fn insertion_index(
    contexts: &[String],
    seq: u64,
    by_key: &IndexMap<String, Rc<RefCell<InternalContext>>>,
) -> usize {
    let mut low = 0;
    let mut high = contexts.len();
    while low < high {
        let middle = low + (high - low) / 2;
        let candidate = contexts
            .get(middle)
            .and_then(|key| by_key.get(key))
            .and_then(|context| context.borrow().start_seq)
            .unwrap_or(u64::MAX);
        if candidate < seq {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    low
}

fn same_location_data(
    left: Option<&Rc<ConversationLocationData>>,
    right: Option<&Rc<ConversationLocationData>>,
) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => Rc::ptr_eq(left, right),
        (None, None) => true,
        _ => false,
    }
}

fn scope_index(scope: ConversationLocationDataScope) -> usize {
    match scope {
        ConversationLocationDataScope::Step => 0,
        ConversationLocationDataScope::Turn => 1,
    }
}
