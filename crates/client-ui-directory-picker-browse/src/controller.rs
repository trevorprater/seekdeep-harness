//! Deterministic `DirectoryBrowser` lifecycle and effect requests.

use crate::{
    DirectoryEntry, DirectoryListing, DraftRead, ScannedDirectory, read_draft, resolve_landing,
    target_path,
};
use serde::{Deserialize, Serialize};

/// Whether a landing closes the editor and reports target failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LandingOptions {
    /// Retire the path editor after a successful landing.
    pub close_editor: bool,
    /// Surface target-list failure in the browser alert.
    pub announce: bool,
}

impl LandingOptions {
    /// Submitted path, crumb, or initial-home landing.
    pub const SUBMITTED: Self = Self {
        close_editor: true,
        announce: true,
    };
    /// Speculative draft-following landing.
    pub const PREVIEW: Self = Self {
        close_editor: false,
        announce: false,
    };
}

/// One Host listing request plus deterministic ownership tokens.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListingLaunch {
    /// Newer intent invalidates this sequence.
    pub seq: u64,
    /// Absent path asks for the Host home.
    pub path: Option<String>,
    /// Each physical scan owns a fresh slow-indicator window.
    pub scan_window: u64,
}

/// Parent leg requested after a target listing lands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParentLeg {
    /// Same supersession sequence as the target leg.
    pub seq: u64,
    /// Parent crumb path.
    pub path: String,
    /// Fresh slow-indicator window for this physical scan.
    pub scan_window: u64,
    /// Submitted landings arm the bounded single-pane fallback.
    pub bounded_wait: bool,
}

/// Result of accepting one target leg.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TargetLanding {
    /// A newer intent already owns the browser.
    Stale,
    /// Display root or missing ancestry committed single-pane immediately.
    CommittedSingle,
    /// Parent listing must be attempted before the final pane shape is known.
    Parent(ParentLeg),
}

/// Draft-preview debounce identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewToken {
    /// Supersession sequence at the keystroke.
    pub seq: u64,
    /// Exact draft that armed the wait.
    pub draft: String,
}

/// One child-directory creation request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateLaunch {
    /// Open/close generation fencing stale settlements.
    pub generation: u64,
    /// Parent directory.
    pub path: String,
    /// Untrimmed operator spelling.
    pub name: String,
}

/// One post-create relist and the child it must select.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreationRelist {
    /// Relist request.
    pub listing: ListingLaunch,
    /// Untrimmed created name.
    pub name: String,
    /// Host-resolved created path.
    pub created_path: String,
}

/// Focus destination requested after a render-invalidating transition.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FocusRequest {
    /// No controller-directed focus move.
    #[default]
    None,
    /// Re-park on the still-open path input after a preview swap.
    PathInput,
    /// Re-park on the newly selected row.
    Selection,
    /// Re-park on the breadcrumb edit zone when a row/input disappears.
    EditZone,
}

/// Render-facing browser state. Async handles and timers stay outside this model.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::struct_excessive_bools)] // Independent source UI axes, not one closed phase enum.
pub struct DirectoryBrowserState {
    /// Owner visibility.
    pub open: bool,
    /// Left/single pane.
    pub parent: Option<DirectoryListing>,
    /// Selected left-pane row.
    pub selected: Option<DirectoryEntry>,
    /// Right-pane child listing.
    pub child: Option<DirectoryListing>,
    /// A physical listing call is active.
    pub loading: bool,
    /// Current call outlived its silence window.
    pub slow_scan: bool,
    /// Current physical scan window token.
    pub scan_window: u64,
    /// Announced browse/relist failure.
    pub error: Option<String>,
    /// `None` is crumb mode; `Some` is editor mode.
    pub path_draft: Option<String>,
    /// Client-side hidden filter.
    pub show_hidden: bool,
    /// `None` closes the create dialog.
    pub folder_draft: Option<String>,
    /// Create request in flight.
    pub creating_folder: bool,
    /// Nested create failure.
    pub create_error: Option<String>,
    /// Newer intent wins.
    pub request_seq: u64,
    /// Reopen invalidates prior creation settlements.
    pub open_generation: u64,
    /// Last draft scan and Host spelling.
    pub scanned: Option<ScannedDirectory>,
    /// Submitted path owns the view until the next edit.
    pub preview_suspended: bool,
    /// Post-render focus request.
    pub focus: FocusRequest,
}

#[derive(Clone, Debug)]
struct PendingLanding {
    seq: u64,
    target: DirectoryListing,
    options: LandingOptions,
    landed_single: bool,
}

#[derive(Clone, Debug)]
struct PendingCreationRelist {
    seq: u64,
    name: String,
    created_path: String,
}

/// Deterministic owner for directory-browser state and async settlement fences.
#[derive(Clone, Debug, Default)]
pub struct DirectoryBrowserController {
    state: DirectoryBrowserState,
    pending_landing: Option<PendingLanding>,
    pending_creation_relist: Option<PendingCreationRelist>,
}

impl DirectoryBrowserController {
    /// Creates the closed, empty browser.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Current render state.
    #[must_use]
    pub const fn state(&self) -> &DirectoryBrowserState {
        &self.state
    }

    /// Clears a consumed focus request.
    pub fn consume_focus(&mut self) -> FocusRequest {
        std::mem::take(&mut self.state.focus)
    }

    fn bump_seq(&mut self) -> u64 {
        self.state.request_seq = self.state.request_seq.wrapping_add(1);
        self.pending_landing = None;
        self.pending_creation_relist = None;
        self.state.request_seq
    }

    fn restart_scan_window(&mut self) -> u64 {
        self.state.slow_scan = false;
        self.state.scan_window = self.state.scan_window.wrapping_add(1);
        self.state.scan_window
    }

    fn begin_listing(&mut self, path: Option<String>) -> ListingLaunch {
        ListingLaunch {
            seq: self.state.request_seq,
            path,
            scan_window: self.restart_scan_window(),
        }
    }

    /// Invalidates and aborts the current scan; callers own the actual `AbortController`.
    pub fn supersede(&mut self) -> u64 {
        self.bump_seq()
    }

    /// Invalidates every settlement owned by an unmounted component instance.
    pub fn dispose(&mut self) {
        self.state.open_generation = self.state.open_generation.wrapping_add(1);
        self.bump_seq();
        self.state.open = false;
        self.state.loading = false;
        self.state.slow_scan = false;
        self.state.focus = FocusRequest::None;
    }

    /// Opens fresh at Host home and returns the initial target listing.
    pub fn open(&mut self) -> ListingLaunch {
        self.state.open_generation = self.state.open_generation.wrapping_add(1);
        self.state.open = true;
        self.state.parent = None;
        self.state.selected = None;
        self.state.child = None;
        self.state.creating_folder = false;
        self.state.show_hidden = false;
        self.begin_landing(None, LandingOptions::SUBMITTED)
    }

    /// Closes and invalidates every pending listing/creation settlement.
    pub fn close(&mut self) {
        self.state.open_generation = self.state.open_generation.wrapping_add(1);
        self.state.open = false;
        self.bump_seq();
        self.state.loading = false;
        self.state.slow_scan = false;
        self.state.error = None;
        self.state.path_draft = None;
        self.state.folder_draft = None;
        self.state.create_error = None;
        self.state.focus = FocusRequest::None;
    }

    /// Starts a submitted or draft-preview whole-view landing.
    pub fn begin_landing(
        &mut self,
        path: Option<String>,
        options: LandingOptions,
    ) -> ListingLaunch {
        self.bump_seq();
        self.state.loading = true;
        if options.announce {
            self.state.error = None;
        }
        self.begin_listing(path)
    }

    fn settle_landing(&mut self, options: LandingOptions) {
        self.state.loading = false;
        if options.close_editor {
            let closed_editor = self.state.path_draft.is_some();
            self.state.path_draft = None;
            if closed_editor {
                self.state.focus = FocusRequest::EditZone;
            }
        } else {
            self.state.error = None;
            self.state.focus = FocusRequest::PathInput;
        }
    }

    fn commit_single(&mut self, target: DirectoryListing, options: LandingOptions) {
        self.state.parent = Some(target);
        self.state.selected = None;
        self.state.child = None;
        self.settle_landing(options);
    }

    /// Accepts the target leg and optionally requests its parent level.
    pub fn target_landed(
        &mut self,
        launch: &ListingLaunch,
        target: DirectoryListing,
        options: LandingOptions,
    ) -> TargetLanding {
        if launch.seq != self.state.request_seq {
            return TargetLanding::Stale;
        }
        if !options.close_editor
            && let Some(path) = &launch.path
        {
            self.state.scanned = Some(ScannedDirectory {
                directory: path.clone(),
                landed: target.path.clone(),
            });
        }
        let parent_path = if crate::is_display_root(&target) {
            None
        } else {
            target
                .crumbs
                .len()
                .checked_sub(2)
                .and_then(|index| target.crumbs.get(index))
                .map(|crumb| crumb.path.clone())
        };
        let Some(parent_path) = parent_path else {
            self.commit_single(target, options);
            return TargetLanding::CommittedSingle;
        };
        self.pending_landing = Some(PendingLanding {
            seq: launch.seq,
            target,
            options,
            landed_single: false,
        });
        TargetLanding::Parent(ParentLeg {
            seq: launch.seq,
            path: parent_path,
            scan_window: self.restart_scan_window(),
            bounded_wait: options.close_editor,
        })
    }

    /// Accepts the parent leg, upgrading even a previously timed-out landing.
    pub fn parent_landed(&mut self, seq: u64, parent: DirectoryListing) -> bool {
        if seq != self.state.request_seq {
            return false;
        }
        let Some(pending) = self
            .pending_landing
            .take()
            .filter(|pending| pending.seq == seq)
        else {
            return false;
        };
        let landing = resolve_landing(pending.target, Some(parent));
        self.state.parent = Some(landing.parent);
        self.state.selected = landing.selected;
        self.state.child = landing.child;
        self.settle_landing(pending.options);
        true
    }

    /// Parent failure commits the readable target alone and stays silent.
    pub fn parent_failed(&mut self, seq: u64) -> bool {
        if seq != self.state.request_seq {
            return false;
        }
        let Some(pending) = self
            .pending_landing
            .take()
            .filter(|pending| pending.seq == seq)
        else {
            return false;
        };
        self.commit_single(pending.target, pending.options);
        true
    }

    /// Submitted-navigation timeout commits the target but keeps the late upgrade live.
    pub fn parent_wait_elapsed(&mut self, seq: u64) -> bool {
        if seq != self.state.request_seq {
            return false;
        }
        let Some(pending) = self
            .pending_landing
            .as_mut()
            .filter(|pending| pending.seq == seq && !pending.landed_single)
        else {
            return false;
        };
        pending.landed_single = true;
        let target = pending.target.clone();
        let options = pending.options;
        self.commit_single(target, options);
        true
    }

    /// Target-list failure leaves stale panes standing.
    pub fn target_failed(&mut self, seq: u64, options: LandingOptions, message: String) -> bool {
        if seq != self.state.request_seq {
            return false;
        }
        self.pending_landing = None;
        self.state.loading = false;
        if options.announce {
            self.state.error = Some(message);
        }
        true
    }

    /// Selects one left/current-pane entry and requests its children.
    pub fn begin_selection(&mut self, entry: DirectoryEntry) -> ListingLaunch {
        let editing = self.state.path_draft.is_some();
        self.bump_seq();
        if editing {
            self.state.focus = FocusRequest::Selection;
        }
        self.state.path_draft = None;
        self.state.selected = Some(entry.clone());
        self.state.child = None;
        self.state.loading = true;
        self.state.error = None;
        self.begin_listing(Some(entry.path))
    }

    /// Lands one child preview.
    pub fn selection_landed(&mut self, seq: u64, child: DirectoryListing) -> bool {
        if seq != self.state.request_seq {
            return false;
        }
        self.state.child = Some(child);
        self.state.loading = false;
        true
    }

    /// Clears an unreadable selection and surfaces its failure.
    pub fn selection_failed(&mut self, seq: u64, message: String) -> bool {
        if seq != self.state.request_seq {
            return false;
        }
        self.state.loading = false;
        self.state.error = Some(message);
        self.state.selected = None;
        self.state.focus = FocusRequest::EditZone;
        true
    }

    /// Advances a right-pane row so the current child becomes the left pane.
    pub fn advance(&mut self, entry: DirectoryEntry) -> Option<ListingLaunch> {
        let child = self.state.child.clone()?;
        self.state.parent = Some(child);
        Some(self.begin_selection(entry))
    }

    /// Opens the path editor, seeded from selection or level with a trailing separator.
    pub fn open_path_editor(&mut self) {
        self.bump_seq();
        self.state.loading = false;
        self.state.preview_suspended = false;
        self.state.path_draft = Some(
            self.state
                .selected
                .as_ref()
                .map(|entry| entry.path.clone())
                .or_else(|| {
                    self.state
                        .parent
                        .as_ref()
                        .map(|listing| listing.path.clone())
                })
                .map_or_else(String::new, |mut base| {
                    if let Some(parent) = &self.state.parent {
                        let separator = crate::separator_of(parent).as_char();
                        if !base.ends_with(separator) {
                            base.push(separator);
                        }
                    }
                    base
                }),
        );
    }

    /// Applies one keystroke and arms the draft-preview debounce.
    pub fn edit_path(&mut self, draft: String) -> PreviewToken {
        self.bump_seq();
        self.state.loading = false;
        self.state.preview_suspended = false;
        self.state.path_draft = Some(draft.clone());
        PreviewToken {
            seq: self.state.request_seq,
            draft,
        }
    }

    /// Resolves the debounced draft into a speculative landing only when no pane answers it.
    pub fn preview_elapsed(&mut self, token: &PreviewToken) -> Option<ListingLaunch> {
        if token.seq != self.state.request_seq
            || self.state.preview_suspended
            || self.state.path_draft.as_deref() != Some(token.draft.as_str())
        {
            return None;
        }
        let current = self.state.child.as_ref().or(self.state.parent.as_ref())?;
        let DraftRead { directory, tail } =
            read_draft(current, &token.draft, self.state.scanned.as_ref());
        if tail.is_some() {
            return None;
        }
        self.begin_landing(directory, LandingOptions::PREVIEW)
            .into()
    }

    /// Submits the untrimmed path when it is not all whitespace.
    pub fn submit_path(&mut self) -> Option<ListingLaunch> {
        let draft = self.state.path_draft.clone()?;
        if draft.trim().is_empty() {
            return None;
        }
        self.state.preview_suspended = true;
        Some(self.begin_landing(Some(draft), LandingOptions::SUBMITTED))
    }

    /// Cancels path editing and optionally restarts a superseded initial home listing.
    pub fn cancel_path_edit(&mut self, focus_edit_zone: bool) -> Option<ListingLaunch> {
        self.bump_seq();
        self.state.loading = false;
        self.state.path_draft = None;
        self.state.error = None;
        if self.state.child.is_none() {
            self.state.selected = None;
        }
        self.state.focus = if focus_edit_zone {
            FocusRequest::EditZone
        } else {
            FocusRequest::None
        };
        self.state
            .parent
            .is_none()
            .then(|| self.begin_landing(None, LandingOptions::SUBMITTED))
    }

    /// Toggles the pure client-side hidden-entry filter.
    pub fn toggle_show_hidden(&mut self) {
        self.state.show_hidden = !self.state.show_hidden;
    }

    /// Opens the nested create dialog when a target exists.
    pub fn open_create_dialog(&mut self) -> bool {
        if target_path(self.state.parent.as_ref(), self.state.selected.as_ref()).is_none() {
            return false;
        }
        self.state.folder_draft = Some(String::new());
        self.state.create_error = None;
        true
    }

    /// Updates the untrimmed folder draft.
    pub fn edit_folder_name(&mut self, name: String) {
        self.state.folder_draft = Some(name);
    }

    /// Closes the nested dialog unless creation is in flight.
    pub fn close_create_dialog(&mut self) -> bool {
        if self.state.creating_folder {
            return false;
        }
        self.state.folder_draft = None;
        true
    }

    /// Starts creation with the exact untrimmed folder spelling.
    pub fn confirm_create(&mut self) -> Option<CreateLaunch> {
        if self.state.creating_folder {
            return None;
        }
        let name = self.state.folder_draft.clone()?;
        if name.trim().is_empty() {
            return None;
        }
        let path =
            target_path(self.state.parent.as_ref(), self.state.selected.as_ref())?.to_owned();
        self.state.creating_folder = true;
        self.state.create_error = None;
        Some(CreateLaunch {
            generation: self.state.open_generation,
            path,
            name,
        })
    }

    /// Accepts creation and starts the target-level relist.
    pub fn creation_succeeded(
        &mut self,
        launch: &CreateLaunch,
        created_path: String,
    ) -> Option<CreationRelist> {
        if launch.generation != self.state.open_generation {
            return None;
        }
        self.state.creating_folder = false;
        self.state.folder_draft = None;
        self.bump_seq();
        self.state.loading = true;
        self.state.error = None;
        let listing = self.begin_listing(Some(launch.path.clone()));
        self.pending_creation_relist = Some(PendingCreationRelist {
            seq: listing.seq,
            name: launch.name.clone(),
            created_path: created_path.clone(),
        });
        Some(CreationRelist {
            listing,
            name: launch.name.clone(),
            created_path,
        })
    }

    /// Surfaces a creation failure only inside the same open generation.
    pub fn creation_failed(&mut self, launch: &CreateLaunch, message: String) -> bool {
        if launch.generation != self.state.open_generation {
            return false;
        }
        self.state.creating_folder = false;
        self.state.create_error = Some(message);
        true
    }

    /// Lands the post-create relist and requests the new folder's child preview.
    pub fn creation_relist_landed(
        &mut self,
        seq: u64,
        level: DirectoryListing,
    ) -> Option<ListingLaunch> {
        if seq != self.state.request_seq {
            return None;
        }
        let pending = self
            .pending_creation_relist
            .take()
            .filter(|pending| pending.seq == seq)?;
        self.state.parent = Some(level);
        self.state.loading = false;
        Some(self.begin_selection(DirectoryEntry {
            name: pending.name,
            path: pending.created_path,
            hidden: false,
        }))
    }

    /// Surfaces a post-create relist failure on the browser surface.
    pub fn creation_relist_failed(&mut self, seq: u64, message: String) -> bool {
        if seq != self.state.request_seq {
            return false;
        }
        if self
            .pending_creation_relist
            .take()
            .filter(|pending| pending.seq == seq)
            .is_none()
        {
            return false;
        }
        self.state.loading = false;
        self.state.error = Some(message);
        true
    }

    /// Marks the loading indicator visible only for the current physical scan window.
    pub fn slow_scan_elapsed(&mut self, scan_window: u64) -> bool {
        if !self.state.loading || scan_window != self.state.scan_window {
            return false;
        }
        self.state.slow_scan = true;
        true
    }
}
