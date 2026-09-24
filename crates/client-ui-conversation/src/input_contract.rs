//! Frozen portable input currencies shared by facade, hub, machine, and UI.

use std::rc::Rc;

use seekdeep_identity::SessionId;

use crate::{
    BusyEnterBehavior, EditRange, EditSelection, InputCommandClaim, InputMachineState,
    InputReferenceInsert, InputTokenSpan, PasteComponent,
};

/// Browser-runtime identity of one unsent image draft.
#[repr(transparent)]
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DraftAttachmentId(String);

impl DraftAttachmentId {
    /// Brands one exact browser attachment id.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the ordinary id string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Removes the nominal brand.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

/// Stable notice sequence preventing repeat-copy collapse.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct InputNoticeSeq(u64);

impl InputNoticeSeq {
    /// Brands one exact notice sequence.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the ordinary sequence.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// One surfaced command/adjudication notice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputNotice {
    /// Severity.
    pub level: crate::InputNoticeLevel,
    /// Notice copy.
    pub text: String,
    /// Repeat-safe identity.
    pub seq: InputNoticeSeq,
}

/// Exact transient queue row type supplied by the Client runtime.
pub type QueuedMessage = seekdeep_client_runtime::QueuedMessage;

/// Published input-state currency.
pub type InputState = InputMachineState;

/// Readable observable input-state source.
pub trait InputStateSource {
    /// Returns the current whole snapshot.
    fn snapshot(&self) -> InputState;
    /// Subscribes to replacements and returns a disposer.
    fn subscribe(&self, listener: Rc<dyn Fn()>) -> Box<dyn FnOnce()>;
}

/// Scoped mutation verbs whose boolean is the bail-event result.
pub trait InputTarget {
    /// Applies one command claim after span CAS.
    fn begin_command(&self, claim: InputCommandClaim, span: InputTokenSpan) -> bool;
    /// Applies one reference insertion after span CAS.
    fn insert_reference(&self, reference: InputReferenceInsert, span: InputTokenSpan) -> bool;
}

/// Stable public action face supplied to session-scope slot components.
pub trait InputActions {
    /// Writes a complete next draft.
    fn set_draft(&self, text: String);
    /// Appends browser-owned image ids; false while admission is locked.
    fn add_images(&self, ids: Vec<DraftAttachmentId>) -> bool;
    /// Removes one image id.
    fn remove_image(&self, id: &DraftAttachmentId);
    /// Keeps only ids still owned by the browser registry.
    fn prune_images(&self, ids: &[DraftAttachmentId]);
    /// Enters ordinary queue-mode submission.
    fn submit(&self);
}

/// Full per-session input facade owned by conversation wiring.
pub trait SessionInput: InputTarget + InputActions {
    /// Sends with explicit delivery mode.
    fn submit_mode(&self, mode: BusyEnterBehavior);
    /// Surfaces an external notice.
    fn notify(&self, level: crate::InputNoticeLevel, text: String);
    /// Returns the stable state source.
    fn state(&self) -> &dyn InputStateSource;
}

/// Session-addressed access to input facades.
pub trait SessionInputResolver {
    /// Resolved facade type.
    type Input: SessionInput;
    /// Resolves one session facade.
    fn for_session(&self, session_id: &SessionId) -> Option<Rc<Self::Input>>;
}

/// Menu keys intercepted while the popup is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArbitrateKey {
    /// Previous row.
    Up,
    /// Next row.
    Down,
    /// Pick highlighted row.
    Enter,
    /// Dismiss menu.
    Escape,
}

/// Menu arbitration verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArbitrateOutcome {
    /// Key was consumed.
    Consumed,
    /// Enter picked the highlight.
    PickHighlighted,
    /// Let the input process the key.
    Pass,
}

/// InputBar-exclusive synchronous keyboard/DOM face.
pub trait ComposerKeyboard {
    /// Returns the live machine snapshot.
    fn snapshot(&self) -> InputState;
    /// Writes a draft with an optional observed edit shape.
    fn set_draft(&self, text: String, edit_range: Option<EditRange>);
    /// Submits with explicit delivery mode.
    fn submit(&self, mode: BusyEnterBehavior);
    /// Steers every pending queued message.
    fn steer_queue(&self);
    /// Undo.
    fn undo(&self);
    /// Redo.
    fn redo(&self);
    /// Begins one paste transaction.
    fn paste_begin(
        &self,
        text: String,
        selection: EditSelection,
        components: Vec<PasteComponent>,
        generation: u64,
    );
    /// Invalidates the live paste attempt.
    fn invalidate_paste(&self);
    /// Tracks draft/caret through trigger detection.
    fn track(&self, draft: &str, caret: u32);
    /// Arbitrates a menu key.
    fn arbitrate(&self, key: ArbitrateKey, composing: bool) -> ArbitrateOutcome;
    /// Runs synchronous Space adjudication.
    fn space(&self) -> bool;
    /// Dismisses the popup shell.
    fn dismiss_popup(&self);
}
