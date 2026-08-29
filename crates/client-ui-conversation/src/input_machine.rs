//! Pure per-session input reducer: events in, effects out.

use std::{cell::Cell, collections::BTreeSet, rc::Rc};

use crate::{BusyEnterBehavior, DraftAttachmentId, OccurrenceId, QueuedMessage};

/// Object-replacement character backing every reference chip.
pub const INPUT_PLACEHOLDER: char = '\u{FFFC}';
const LOG_LIMIT: usize = 100;

/// Monotonic draft revision used by span compare-and-swap guards.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct DraftRevision(u64);

impl DraftRevision {
    /// Returns the ordinary revision number.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// One machine-minted paste attempt identity.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PasteAttemptId(u64);

impl PasteAttemptId {
    /// Brands one exact attempt number.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the ordinary attempt number.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// One machine-minted submit attempt identity.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SubmitAttemptId(u64);

impl SubmitAttemptId {
    /// Brands one exact attempt number.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the ordinary attempt number.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Half-open UTF-16 selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditSelection {
    /// Inclusive start.
    pub start: u32,
    /// Exclusive end.
    pub end: u32,
}

/// One prior-draft replacement shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditRange {
    /// Inclusive start in the previous draft.
    pub start: u32,
    /// Exclusive end in the previous draft.
    pub end: u32,
    /// UTF-16 length inserted in its place.
    pub inserted_length: u32,
}

/// Pick-time trigger span with draft-revision CAS currency.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputTokenSpan {
    /// Inclusive UTF-16 start.
    pub start: u32,
    /// Exclusive UTF-16 end.
    pub end: u32,
    /// Draft revision observed by the picker.
    pub draft_rev: DraftRevision,
}

/// Stable command-submit closure identity carried through reducer effects.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct CommandSubmitId(u64);

impl CommandSubmitId {
    /// Brands one submit closure identity.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
}

/// Command claim data and opaque submit closure identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputCommandClaim {
    /// Integrity-watched leading token.
    pub token: String,
    /// Optional ghost hint.
    pub hint: Option<String>,
    /// Shell-owned submit closure identity.
    pub submit_id: CommandSubmitId,
}

/// Inline reference insertion payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputReferenceInsert {
    /// Owning source name.
    pub source: String,
    /// Owner-scoped reference id.
    pub reference: String,
    /// Chip label.
    pub label: String,
    /// Clipboard/persistence projection.
    pub clipboard_text: String,
}

/// One minted reference occurrence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputOccurrence {
    /// Stable per-machine identity.
    pub occurrence_id: OccurrenceId,
    /// Owning source name.
    pub source: String,
    /// Owner-scoped reference id.
    pub reference: String,
    /// UTF-16 placeholder offset.
    pub offset: u32,
    /// Cached chip label.
    pub label: String,
    /// Cached clipboard projection.
    pub clipboard_text: String,
    /// Owner-resolution failure bit; absent and false are equivalent on the source wire.
    pub invalid: bool,
}

/// One sync-matched paste component relative to pasted text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PasteComponent {
    /// Inclusive relative start.
    pub start: u32,
    /// Exclusive relative end.
    pub end: u32,
    /// Reference inserted for this token.
    pub reference: InputReferenceInsert,
}

/// Live async paste-match attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PasteAttemptState {
    /// Attempt identity.
    pub attempt_id: PasteAttemptId,
    /// Current pasted range in draft coordinates.
    pub inserted_range: EditSelection,
    /// Projection generation echoed to the controller.
    pub generation: u64,
}

/// Published input phase.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InputPhase {
    /// Ordinary editable draft.
    #[default]
    Plain,
    /// Awaiting leading-slash arbitration.
    Adjudicating,
    /// Command claim owns the leading token.
    Claimed,
    /// Command submission is in flight.
    Submitting,
}

/// Published claim projection (submit behavior stays private).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedClaim {
    /// Watched token.
    pub token: String,
    /// Optional hint.
    pub hint: Option<String>,
}

/// Published reducer snapshot (queue/images are empty at this tier).
#[derive(Clone, Debug, PartialEq)]
pub struct InputMachineState {
    /// Complete draft with U+FFFC placeholders.
    pub draft: String,
    /// Monotonic revision.
    pub draft_rev: DraftRevision,
    /// Current phase.
    pub phase: InputPhase,
    /// Claim projection while claimed/submitting.
    pub claim: Option<PublishedClaim>,
    /// Reference-stable occurrence table.
    pub occurrences: Rc<Vec<InputOccurrence>>,
    /// Live paste attempt.
    pub paste: Option<PasteAttemptState>,
    /// Browser-owned image ids; empty in the pure reducer.
    pub image_ids: Rc<Vec<DraftAttachmentId>>,
    /// Queue projection; empty in the pure reducer.
    pub queue: Rc<Vec<QueuedMessage>>,
}

/// Cloneable cancellation signal for one submit attempt.
#[derive(Clone, Debug, Default)]
pub struct InputAbortSignal(Rc<Cell<bool>>);

impl InputAbortSignal {
    fn abort(&self) {
        self.0.set(true);
    }

    /// Returns whether release aborted this attempt.
    #[must_use]
    pub fn aborted(&self) -> bool {
        self.0.get()
    }
}

impl PartialEq for InputAbortSignal {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for InputAbortSignal {}

/// One in-flight submit attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubmitAttempt {
    /// Attempt identity.
    pub id: SubmitAttemptId,
    /// Release-driven cancellation signal.
    pub signal: InputAbortSignal,
    /// Draft at Enter time.
    pub draft_snapshot: String,
    /// Delivery intent retained through arbitration.
    pub mode: BusyEnterBehavior,
}

/// Settled command outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandSubmitOutcome {
    /// Success/error severity.
    pub kind: CommandSubmitOutcomeKind,
    /// Optional result copy.
    pub text: Option<String>,
}

/// Command result severity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandSubmitOutcomeKind {
    /// Successful command result.
    Success,
    /// Business error result.
    Error,
}

/// Enter-time arbitration result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputPickOutcome {
    /// Command claim won.
    Claim(InputCommandClaim),
    /// Reference path handled the pick.
    Insert(InputReferenceInsert),
    /// Plain text path handled the pick.
    Text(String),
    /// Source handled the gesture internally.
    Handled,
    /// No source claimed the line.
    Miss,
}

/// Consume-token guard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConsumeTokenGuard {
    /// Exact span CAS.
    Span(InputTokenSpan),
    /// Whole trimmed-draft token equality.
    BareToken(String),
}

/// Input reducer event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputMachineEvent {
    /// Full next draft with optional DOM edit shape.
    DraftChanged {
        /// Next draft.
        draft: String,
        /// Previous-draft edit range.
        edit_range: Option<EditRange>,
    },
    /// Apply a command claim.
    BeginCommand {
        /// Claim.
        claim: InputCommandClaim,
        /// Pick-time span.
        span: InputTokenSpan,
    },
    /// Insert one reference occurrence.
    InsertReference {
        /// Reference.
        reference: InputReferenceInsert,
        /// Pick-time span.
        span: InputTokenSpan,
    },
    /// Delete a settled token.
    ConsumeToken(ConsumeTokenGuard),
    /// Mark exactly these occurrence ids invalid.
    SetInvalid(Vec<OccurrenceId>),
    /// Undo one transaction.
    Undo,
    /// Redo one transaction.
    Redo,
    /// Begin one paste transaction.
    PasteBegin {
        /// Raw pasted text.
        text: String,
        /// Replaced selection.
        selection: EditSelection,
        /// Synchronous components.
        components: Vec<PasteComponent>,
        /// Projection generation.
        generation: u64,
    },
    /// Upgrade one pasted token in its own transaction.
    PasteUpgrade {
        /// Attempt id.
        attempt_id: PasteAttemptId,
        /// Current span CAS.
        span: InputTokenSpan,
        /// Reference.
        reference: InputReferenceInsert,
    },
    /// End the current paste attempt.
    InvalidatePaste,
    /// Enter submission.
    Enter(BusyEnterBehavior),
    /// Arbitration settled.
    Adjudicated {
        /// Attempt.
        attempt: SubmitAttempt,
        /// Outcome.
        outcome: InputPickOutcome,
    },
    /// Arbitration failed.
    AdjudicationFailed {
        /// Attempt.
        attempt: SubmitAttempt,
        /// Notice text.
        message: String,
    },
    /// Command submit settled.
    SubmitSettled {
        /// Attempt.
        attempt: SubmitAttempt,
        /// Transport/application success bit.
        ok: bool,
        /// Optional command outcome.
        outcome: Option<CommandSubmitOutcome>,
        /// Optional failure text.
        message: Option<String>,
    },
    /// Ordinary send committed.
    SendCommitted,
    /// Session input released.
    Release,
}

/// Reducer output effect executed by the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputMachineEffect {
    /// Run leading-slash arbitration.
    Adjudicate {
        /// Attempt.
        attempt: SubmitAttempt,
        /// Draft snapshot.
        draft: String,
    },
    /// Execute a claimed command.
    BeginSubmit {
        /// Attempt.
        attempt: SubmitAttempt,
        /// Claim with submit identity.
        claim: InputCommandClaim,
        /// Verbatim args.
        args: String,
    },
    /// Deliver ordinary text.
    DefaultSink {
        /// Draft.
        draft: String,
        /// Delivery mode.
        mode: BusyEnterBehavior,
    },
    /// Surface a notice.
    Notice {
        /// Severity.
        level: InputNoticeLevel,
        /// Copy.
        text: String,
    },
}

/// Notice severity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputNoticeLevel {
    /// Informational result.
    Info,
    /// Error result.
    Error,
}

#[derive(Clone)]
struct Transaction {
    draft_before: String,
    occurrences_before: Rc<Vec<InputOccurrence>>,
}

struct Inflight {
    attempt: SubmitAttempt,
}

/// Construction knobs with an injected monotonic clock.
#[derive(Clone)]
pub struct InputMachineOptions {
    /// Single-character typing merge window.
    pub merge_window_ms: f64,
    /// Injected monotonic clock.
    pub now: Rc<dyn Fn() -> f64>,
}

impl Default for InputMachineOptions {
    fn default() -> Self {
        Self {
            merge_window_ms: 1_000.0,
            now: Rc::new(|| 0.0),
        }
    }
}

/// Pure input state machine.
pub struct InputMachine {
    draft: String,
    draft_rev: DraftRevision,
    phase: InputPhase,
    claim: Option<InputCommandClaim>,
    occurrences: Rc<Vec<InputOccurrence>>,
    occurrence_seq: u64,
    submit_seq: u64,
    inflight: Option<Inflight>,
    log: Vec<Transaction>,
    redo_stack: Vec<Transaction>,
    typing_run: Option<(u32, f64)>,
    paste: Option<PasteAttemptState>,
    paste_seq: u64,
    options: InputMachineOptions,
    empty_queue: Rc<Vec<QueuedMessage>>,
}

impl Default for InputMachine {
    fn default() -> Self {
        Self::new(InputMachineOptions::default())
    }
}

impl InputMachine {
    /// Creates one isolated machine.
    #[must_use]
    pub fn new(options: InputMachineOptions) -> Self {
        Self {
            draft: String::new(),
            draft_rev: DraftRevision::default(),
            phase: InputPhase::Plain,
            claim: None,
            occurrences: Rc::new(Vec::new()),
            occurrence_seq: 0,
            submit_seq: 0,
            inflight: None,
            log: Vec::new(),
            redo_stack: Vec::new(),
            typing_run: None,
            paste: None,
            paste_seq: 0,
            options,
            empty_queue: Rc::new(Vec::new()),
        }
    }

    /// Returns the current published snapshot.
    #[must_use]
    pub fn state(&self) -> InputMachineState {
        InputMachineState {
            draft: self.draft.clone(),
            draft_rev: self.draft_rev,
            phase: self.phase,
            claim: self.claim.as_ref().map(|claim| PublishedClaim {
                token: claim.token.clone(),
                hint: claim.hint.clone(),
            }),
            occurrences: self.occurrences.clone(),
            paste: self.paste,
            image_ids: Rc::new(Vec::new()),
            queue: self.empty_queue.clone(),
        }
    }

    /// Feeds one event through the reducer.
    pub fn dispatch(&mut self, event: InputMachineEvent) -> Vec<InputMachineEffect> {
        match event {
            InputMachineEvent::DraftChanged { draft, edit_range } => {
                self.on_draft_changed(draft, edit_range)
            }
            InputMachineEvent::BeginCommand { claim, span } => self.on_begin_command(claim, span),
            InputMachineEvent::InsertReference { reference, span } => {
                self.on_insert_reference(reference, span)
            }
            InputMachineEvent::ConsumeToken(guard) => self.on_consume_token(guard),
            InputMachineEvent::SetInvalid(ids) => self.on_set_invalid(&ids),
            InputMachineEvent::Undo => self.on_undo(),
            InputMachineEvent::Redo => self.on_redo(),
            InputMachineEvent::PasteBegin {
                text,
                selection,
                components,
                generation,
            } => self.on_paste_begin(&text, selection, components, generation),
            InputMachineEvent::PasteUpgrade {
                attempt_id,
                span,
                reference,
            } => self.on_paste_upgrade(attempt_id, span, reference),
            InputMachineEvent::InvalidatePaste => {
                self.paste = None;
                Vec::new()
            }
            InputMachineEvent::Enter(mode) => self.on_enter(mode),
            InputMachineEvent::Adjudicated { attempt, outcome } => {
                self.on_adjudicated(attempt, outcome)
            }
            InputMachineEvent::AdjudicationFailed { attempt, message } => {
                self.on_adjudication_failed(&attempt, message)
            }
            InputMachineEvent::SubmitSettled {
                attempt,
                ok,
                outcome,
                message,
            } => self.on_submit_settled(&attempt, ok, outcome, message),
            InputMachineEvent::SendCommitted => self.on_send_committed(),
            InputMachineEvent::Release => self.on_release(),
        }
    }

    fn adopt(&mut self, draft: String) {
        self.draft = draft;
        self.draft_rev.0 = self.draft_rev.0.wrapping_add(1);
    }

    fn push_transaction(&mut self) {
        self.log.push(Transaction {
            draft_before: self.draft.clone(),
            occurrences_before: self.occurrences.clone(),
        });
        if self.log.len() > LOG_LIMIT {
            self.log.remove(0);
        }
        self.redo_stack.clear();
    }

    fn reconcile(&mut self, range: EditRange) {
        let removed = i64::from(range.end) - i64::from(range.start);
        let delta = i64::from(range.inserted_length) - removed;
        let mut kept = Vec::new();
        for occurrence in self.occurrences.iter() {
            if occurrence.offset < range.start {
                kept.push(occurrence.clone());
            } else if occurrence.offset >= range.end {
                let mut occurrence = occurrence.clone();
                if delta != 0 {
                    occurrence.offset =
                        u32::try_from(i64::from(occurrence.offset) + delta).unwrap_or_default();
                }
                kept.push(occurrence);
            }
        }
        self.occurrences = Rc::new(kept);
    }

    fn watch_claim(&mut self) {
        if self.phase == InputPhase::Claimed
            && self
                .claim
                .as_ref()
                .is_some_and(|claim| !self.draft.starts_with(&claim.token))
        {
            self.phase = InputPhase::Plain;
            self.claim = None;
        }
    }

    fn mint(&mut self, reference: InputReferenceInsert, offset: u32) -> InputOccurrence {
        self.occurrence_seq = self.occurrence_seq.wrapping_add(1);
        InputOccurrence {
            occurrence_id: OccurrenceId::new(self.occurrence_seq),
            source: reference.source,
            reference: reference.reference,
            offset,
            label: reference.label,
            clipboard_text: reference.clipboard_text,
            invalid: false,
        }
    }

    fn with_minted(&mut self, minted: impl IntoIterator<Item = InputOccurrence>) {
        let mut occurrences = self.occurrences.as_ref().clone();
        occurrences.extend(minted);
        occurrences.sort_by_key(|occurrence| occurrence.offset);
        self.occurrences = Rc::new(occurrences);
    }

    fn on_draft_changed(
        &mut self,
        draft: String,
        edit_range: Option<EditRange>,
    ) -> Vec<InputMachineEffect> {
        if draft == self.draft {
            return Vec::new();
        }
        let range = edit_range.unwrap_or_else(|| diff_edit(&self.draft, &draft));
        let typing = range.start == range.end && range.inserted_length == 1;
        let at = (self.options.now)();
        let merges = typing
            && self.typing_run.is_some_and(|(end, previous)| {
                end == range.start && at - previous <= self.options.merge_window_ms
            });
        if !merges {
            self.push_transaction();
        }
        self.typing_run = typing.then_some((range.start.saturating_add(1), at));
        self.reconcile(range);
        self.adopt(draft);
        self.watch_claim();
        self.paste = None;
        Vec::new()
    }

    fn cas_ok(&self, span: InputTokenSpan) -> bool {
        span.draft_rev == self.draft_rev
            && span.start <= span.end
            && span.end <= js_len(&self.draft)
    }

    fn on_begin_command(
        &mut self,
        claim: InputCommandClaim,
        span: InputTokenSpan,
    ) -> Vec<InputMachineEffect> {
        if !matches!(self.phase, InputPhase::Plain | InputPhase::Claimed)
            || !self.cas_ok(span)
            || !js_trim(&js_slice(&self.draft, 0, span.start)).is_empty()
        {
            return Vec::new();
        }
        self.push_transaction();
        self.typing_run = None;
        self.reconcile(EditRange {
            start: 0,
            end: span.end,
            inserted_length: js_len(&claim.token),
        });
        self.adopt(format!(
            "{}{}",
            claim.token,
            js_slice(&self.draft, span.end, js_len(&self.draft))
        ));
        self.claim = Some(claim);
        self.phase = InputPhase::Claimed;
        self.paste = None;
        Vec::new()
    }

    fn on_insert_reference(
        &mut self,
        reference: InputReferenceInsert,
        span: InputTokenSpan,
    ) -> Vec<InputMachineEffect> {
        if matches!(self.phase, InputPhase::Plain | InputPhase::Claimed) && self.cas_ok(span) {
            self.replace_span_with_chip(reference, span);
            self.paste = None;
        }
        Vec::new()
    }

    fn replace_span_with_chip(
        &mut self,
        reference: InputReferenceInsert,
        span: InputTokenSpan,
    ) -> u32 {
        self.push_transaction();
        self.typing_run = None;
        let tail = js_slice(&self.draft, span.end, js_len(&self.draft));
        let gap = if tail.is_empty() || !tail.starts_with(' ') {
            " "
        } else {
            ""
        };
        let inserted = format!("{INPUT_PLACEHOLDER}{gap}");
        let inserted_length = js_len(&inserted);
        self.reconcile(EditRange {
            start: span.start,
            end: span.end,
            inserted_length,
        });
        let occurrence = self.mint(reference, span.start);
        self.with_minted([occurrence]);
        self.adopt(format!(
            "{}{}{}",
            js_slice(&self.draft, 0, span.start),
            inserted,
            tail
        ));
        self.watch_claim();
        inserted_length
    }

    fn on_consume_token(&mut self, guard: ConsumeTokenGuard) -> Vec<InputMachineEffect> {
        if !matches!(self.phase, InputPhase::Plain | InputPhase::Claimed) {
            return Vec::new();
        }
        match guard {
            ConsumeTokenGuard::Span(span) => {
                if !self.cas_ok(span) || span.start == span.end {
                    return Vec::new();
                }
                self.push_transaction();
                self.typing_run = None;
                self.reconcile(EditRange {
                    start: span.start,
                    end: span.end,
                    inserted_length: 0,
                });
                self.adopt(format!(
                    "{}{}",
                    js_slice(&self.draft, 0, span.start),
                    js_slice(&self.draft, span.end, js_len(&self.draft))
                ));
            }
            ConsumeTokenGuard::BareToken(token) => {
                if token.is_empty() || js_trim(&self.draft) != token {
                    return Vec::new();
                }
                self.push_transaction();
                self.typing_run = None;
                self.occurrences = Rc::new(Vec::new());
                self.adopt(String::new());
            }
        }
        self.watch_claim();
        self.paste = None;
        Vec::new()
    }

    fn on_set_invalid(&mut self, invalid_ids: &[OccurrenceId]) -> Vec<InputMachineEffect> {
        let ids = invalid_ids.iter().copied().collect::<BTreeSet<_>>();
        if !self
            .occurrences
            .iter()
            .any(|occurrence| occurrence.invalid != ids.contains(&occurrence.occurrence_id))
        {
            return Vec::new();
        }
        self.occurrences = Rc::new(
            self.occurrences
                .iter()
                .cloned()
                .map(|mut occurrence| {
                    occurrence.invalid = ids.contains(&occurrence.occurrence_id);
                    occurrence
                })
                .collect(),
        );
        Vec::new()
    }

    fn on_undo(&mut self) -> Vec<InputMachineEffect> {
        let Some(entry) = self.log.pop() else {
            return Vec::new();
        };
        self.redo_stack.push(Transaction {
            draft_before: self.draft.clone(),
            occurrences_before: self.occurrences.clone(),
        });
        self.occurrences = entry.occurrences_before;
        self.adopt(entry.draft_before);
        self.watch_claim();
        self.typing_run = None;
        self.paste = None;
        Vec::new()
    }

    fn on_redo(&mut self) -> Vec<InputMachineEffect> {
        let Some(entry) = self.redo_stack.pop() else {
            return Vec::new();
        };
        self.log.push(Transaction {
            draft_before: self.draft.clone(),
            occurrences_before: self.occurrences.clone(),
        });
        if self.log.len() > LOG_LIMIT {
            self.log.remove(0);
        }
        self.occurrences = entry.occurrences_before;
        self.adopt(entry.draft_before);
        self.watch_claim();
        self.typing_run = None;
        self.paste = None;
        Vec::new()
    }

    fn on_paste_begin(
        &mut self,
        raw_text: &str,
        selection: EditSelection,
        mut components: Vec<PasteComponent>,
        generation: u64,
    ) -> Vec<InputMachineEffect> {
        if selection.start > selection.end || selection.end > js_len(&self.draft) {
            return Vec::new();
        }
        let text = raw_text.replace(INPUT_PLACEHOLDER, "");
        self.push_transaction();
        self.typing_run = None;
        components.sort_by_key(|component| component.start);
        let mut minted = Vec::new();
        let mut inserted = String::new();
        let mut cursor = 0;
        for component in components {
            inserted.push_str(&js_slice(&text, cursor, component.start));
            let offset = selection.start.saturating_add(js_len(&inserted));
            minted.push(self.mint(component.reference, offset));
            inserted.push(INPUT_PLACEHOLDER);
            cursor = component.end;
        }
        inserted.push_str(&js_slice(&text, cursor, js_len(&text)));
        let inserted_length = js_len(&inserted);
        self.reconcile(EditRange {
            start: selection.start,
            end: selection.end,
            inserted_length,
        });
        self.with_minted(minted);
        self.adopt(format!(
            "{}{}{}",
            js_slice(&self.draft, 0, selection.start),
            inserted,
            js_slice(&self.draft, selection.end, js_len(&self.draft))
        ));
        self.watch_claim();
        if matches!(self.phase, InputPhase::Plain | InputPhase::Claimed) {
            self.paste_seq = self.paste_seq.wrapping_add(1);
            self.paste = Some(PasteAttemptState {
                attempt_id: PasteAttemptId(self.paste_seq),
                inserted_range: EditSelection {
                    start: selection.start,
                    end: selection.start.saturating_add(inserted_length),
                },
                generation,
            });
        } else {
            self.paste = None;
        }
        Vec::new()
    }

    fn on_paste_upgrade(
        &mut self,
        attempt_id: PasteAttemptId,
        span: InputTokenSpan,
        reference: InputReferenceInsert,
    ) -> Vec<InputMachineEffect> {
        let Some(attempt) = self.paste else {
            return Vec::new();
        };
        if attempt.attempt_id != attempt_id
            || !matches!(self.phase, InputPhase::Plain | InputPhase::Claimed)
            || !self.cas_ok(span)
            || span.start == span.end
        {
            return Vec::new();
        }
        let inserted = self.replace_span_with_chip(reference, span);
        let removed = span.end - span.start;
        self.paste = Some(PasteAttemptState {
            inserted_range: EditSelection {
                start: attempt.inserted_range.start,
                end: attempt
                    .inserted_range
                    .end
                    .saturating_add(inserted)
                    .saturating_sub(removed),
            },
            ..attempt
        });
        Vec::new()
    }

    fn begin_attempt(&mut self, mode: BusyEnterBehavior) -> SubmitAttempt {
        self.submit_seq = self.submit_seq.wrapping_add(1);
        let attempt = SubmitAttempt {
            id: SubmitAttemptId(self.submit_seq),
            signal: InputAbortSignal::default(),
            draft_snapshot: self.draft.clone(),
            mode,
        };
        self.inflight = Some(Inflight {
            attempt: attempt.clone(),
        });
        attempt
    }

    fn on_enter(&mut self, mode: BusyEnterBehavior) -> Vec<InputMachineEffect> {
        if matches!(
            self.phase,
            InputPhase::Adjudicating | InputPhase::Submitting
        ) {
            return Vec::new();
        }
        if self.phase == InputPhase::Claimed
            && let Some(claim) = self.claim.clone()
        {
            let attempt = self.begin_attempt(mode);
            self.phase = InputPhase::Submitting;
            self.paste = None;
            return vec![InputMachineEffect::BeginSubmit {
                args: args_after(&self.draft, &claim.token),
                attempt,
                claim,
            }];
        }
        let trimmed = js_trim(&self.draft);
        if trimmed.is_empty() {
            return Vec::new();
        }
        self.paste = None;
        if trimmed.starts_with('/') {
            let attempt = self.begin_attempt(mode);
            self.phase = InputPhase::Adjudicating;
            vec![InputMachineEffect::Adjudicate {
                attempt,
                draft: self.draft.clone(),
            }]
        } else {
            vec![InputMachineEffect::DefaultSink {
                draft: self.draft.clone(),
                mode,
            }]
        }
    }

    fn on_adjudicated(
        &mut self,
        attempt: SubmitAttempt,
        outcome: InputPickOutcome,
    ) -> Vec<InputMachineEffect> {
        if self.phase != InputPhase::Adjudicating
            || self
                .inflight
                .as_ref()
                .is_none_or(|flight| flight.attempt.id != attempt.id)
        {
            return Vec::new();
        }
        if let InputPickOutcome::Claim(claim) = outcome {
            self.claim = Some(claim.clone());
            self.phase = InputPhase::Submitting;
            return vec![InputMachineEffect::BeginSubmit {
                args: args_after(&attempt.draft_snapshot, &claim.token),
                attempt,
                claim,
            }];
        }
        self.inflight = None;
        self.phase = InputPhase::Plain;
        if outcome == InputPickOutcome::Miss {
            vec![InputMachineEffect::DefaultSink {
                draft: attempt.draft_snapshot,
                mode: attempt.mode,
            }]
        } else {
            Vec::new()
        }
    }

    fn on_adjudication_failed(
        &mut self,
        attempt: &SubmitAttempt,
        message: String,
    ) -> Vec<InputMachineEffect> {
        if self.phase != InputPhase::Adjudicating
            || self
                .inflight
                .as_ref()
                .is_none_or(|flight| flight.attempt.id != attempt.id)
        {
            return Vec::new();
        }
        self.inflight = None;
        self.phase = InputPhase::Plain;
        vec![InputMachineEffect::Notice {
            level: InputNoticeLevel::Error,
            text: message,
        }]
    }

    fn on_submit_settled(
        &mut self,
        attempt: &SubmitAttempt,
        ok: bool,
        outcome: Option<CommandSubmitOutcome>,
        message: Option<String>,
    ) -> Vec<InputMachineEffect> {
        let Some(flight) = self.inflight.as_ref() else {
            return Vec::new();
        };
        if self.phase != InputPhase::Submitting || flight.attempt.id != attempt.id {
            return Vec::new();
        }
        let draft_snapshot = flight.attempt.draft_snapshot.clone();
        self.inflight = None;
        if ok {
            self.phase = InputPhase::Plain;
            self.claim = None;
            self.occurrences = Rc::new(Vec::new());
            self.adopt(String::new());
            self.log.clear();
            self.redo_stack.clear();
            self.typing_run = None;
            self.paste = None;
            return outcome
                .and_then(|outcome| {
                    outcome.text.map(|text| InputMachineEffect::Notice {
                        level: if outcome.kind == CommandSubmitOutcomeKind::Error {
                            InputNoticeLevel::Error
                        } else {
                            InputNoticeLevel::Info
                        },
                        text,
                    })
                })
                .into_iter()
                .collect();
        }
        let text = message
            .or_else(|| outcome.and_then(|outcome| outcome.text))
            .unwrap_or_else(|| "command failed".to_owned());
        if self.draft == draft_snapshot
            && self
                .claim
                .as_ref()
                .is_some_and(|claim| self.draft.starts_with(&claim.token))
        {
            self.phase = InputPhase::Claimed;
        } else {
            self.phase = InputPhase::Plain;
            self.claim = None;
        }
        vec![InputMachineEffect::Notice {
            level: InputNoticeLevel::Error,
            text,
        }]
    }

    fn on_send_committed(&mut self) -> Vec<InputMachineEffect> {
        self.claim = None;
        self.occurrences = Rc::new(Vec::new());
        self.adopt(String::new());
        self.log.clear();
        self.redo_stack.clear();
        self.typing_run = None;
        self.paste = None;
        Vec::new()
    }

    fn on_release(&mut self) -> Vec<InputMachineEffect> {
        if let Some(flight) = self.inflight.take() {
            flight.attempt.signal.abort();
        }
        self.phase = InputPhase::Plain;
        self.claim = None;
        self.typing_run = None;
        self.paste = None;
        Vec::new()
    }
}

/// Expands placeholders into occurrence clipboard text in draft order.
#[must_use]
pub fn project_input_clipboard(draft: &str, occurrences: &[InputOccurrence]) -> String {
    if occurrences.is_empty() {
        return draft.to_owned();
    }
    let mut output = String::new();
    let mut cursor = 0;
    for occurrence in occurrences {
        output.push_str(&js_slice(draft, cursor, occurrence.offset));
        output.push_str(&occurrence.clipboard_text);
        cursor = occurrence.offset.saturating_add(1);
    }
    output.push_str(&js_slice(draft, cursor, js_len(draft)));
    output
}

fn args_after(draft: &str, token: &str) -> String {
    let trimmed = js_trim_start(draft);
    if let Some(arguments) = trimmed.strip_prefix(token) {
        return arguments.to_owned();
    }
    let base = js_trim_end(token);
    if trimmed.starts_with(&base) {
        let rest = &trimmed[base.len()..];
        return rest
            .chars()
            .next()
            .filter(|value| is_javascript_whitespace(*value))
            .map_or_else(String::new, |_| rest.chars().skip(1).collect());
    }
    String::new()
}

fn diff_edit(previous: &str, next: &str) -> EditRange {
    let previous = utf16(previous);
    let next = utf16(next);
    let max_common = previous.len().min(next.len());
    let mut prefix = 0;
    while prefix < max_common && previous[prefix] == next[prefix] {
        prefix += 1;
    }
    let max_suffix = max_common - prefix;
    let mut suffix = 0;
    while suffix < max_suffix
        && previous[previous.len() - 1 - suffix] == next[next.len() - 1 - suffix]
    {
        suffix += 1;
    }
    EditRange {
        start: usize_to_u32(prefix),
        end: usize_to_u32(previous.len() - suffix),
        inserted_length: usize_to_u32(next.len() - suffix - prefix),
    }
}

fn js_len(value: &str) -> u32 {
    usize_to_u32(value.encode_utf16().count())
}

fn js_slice(value: &str, start: u32, end: u32) -> String {
    let units = utf16(value);
    let start = usize::try_from(start)
        .unwrap_or(usize::MAX)
        .min(units.len());
    let end = usize::try_from(end).unwrap_or(usize::MAX).min(units.len());
    if end <= start {
        String::new()
    } else {
        String::from_utf16_lossy(&units[start..end])
    }
}

fn utf16(value: &str) -> Vec<u16> {
    value.encode_utf16().collect()
}

fn usize_to_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn js_trim(value: &str) -> String {
    js_trim_end(&js_trim_start(value))
}

fn js_trim_start(value: &str) -> String {
    value
        .trim_start_matches(is_javascript_whitespace)
        .to_owned()
}

fn js_trim_end(value: &str) -> String {
    value.trim_end_matches(is_javascript_whitespace).to_owned()
}

const fn is_javascript_whitespace(value: char) -> bool {
    matches!(
        value,
        '\u{0009}'..='\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}
