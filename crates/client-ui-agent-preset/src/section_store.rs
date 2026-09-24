//! Target-portable Agent preset management-section semantics.

use std::rc::Rc;

use futures::future::LocalBoxFuture;
use indexmap::IndexMap;
use seekdeep_client_runtime::SnapshotStore;
use serde::Serialize;

use crate::{
    CopyDraft, RosterPreset, RosterValue, draft_blocker, settings_store::preset_snapshot_store,
};

/// Preset content returned by the Host reader.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresetReadValue {
    /// Optional file-authored display name.
    pub name: Option<String>,
    /// Exact composition text.
    pub content: String,
}

/// Result of asking the Host to open one preset directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PresetOpenResult {
    /// The native desktop opened the directory.
    Opened,
    /// No opener exists; reveal this exact path instead.
    Path(String),
}

/// Transport used by the management section.
pub trait AgentPresetSectionTransport {
    /// Reads roster and capability facts.
    fn list(&self) -> LocalBoxFuture<'static, Result<RosterValue, String>>;
    /// Reads one composition.
    fn read(&self, id: String) -> LocalBoxFuture<'static, Result<PresetReadValue, String>>;
    /// Copies one preset directory.
    fn copy(
        &self,
        from: String,
        id: String,
        name: Option<String>,
    ) -> LocalBoxFuture<'static, Result<(), String>>;
    /// Opens or resolves one preset directory.
    fn open_document(
        &self,
        id: String,
    ) -> LocalBoxFuture<'static, Result<PresetOpenResult, String>>;
    /// Removes one user preset.
    fn remove(&self, id: String) -> LocalBoxFuture<'static, Result<(), String>>;
    /// Writes only the default preset field.
    fn update_default(&self, id: String) -> LocalBoxFuture<'static, Result<(), String>>;
}

/// Management-section lifecycle status.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentPresetSectionStatus {
    /// No read has started.
    #[default]
    Idle,
    /// A roster read is in flight.
    Loading,
    /// At least one row is available.
    Ready,
    /// A valid deployment supplies no presets.
    Unavailable,
    /// The last whole-section read failed.
    Error,
}

/// Open read-only composition viewer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PresetView {
    /// Preset id.
    pub id: String,
    /// Display title.
    pub title: String,
    /// Exact composition text.
    pub content: String,
}

/// Management-section snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentPresetSectionState {
    /// Whole-section lifecycle.
    pub status: AgentPresetSectionStatus,
    /// Page-level failure copy.
    pub error: Option<String>,
    /// Whether a writable user preset root exists.
    pub authorable: bool,
    /// Whether the Host can open directories natively.
    pub has_document: bool,
    /// Complete roster, including broken entries.
    pub rows: Vec<RosterPreset>,
    /// Open copy dialog.
    pub copy: Option<CopyDraft>,
    /// Open read-only viewer.
    pub view: Option<PresetView>,
    /// Preset awaiting delete confirmation.
    pub pending_delete: Option<String>,
    /// Whether a delete is in flight.
    pub deleting: bool,
    /// Paths revealed on hosts without a desktop opener.
    pub revealed_paths: IndexMap<String, String>,
}

impl Default for AgentPresetSectionState {
    fn default() -> Self {
        Self {
            status: AgentPresetSectionStatus::Idle,
            error: None,
            authorable: false,
            has_document: false,
            rows: Vec::new(),
            copy: None,
            view: None,
            pending_delete: None,
            deleting: false,
            revealed_paths: IndexMap::new(),
        }
    }
}

type RosterChanged = Rc<dyn Fn()>;

/// Host-authoritative roster management controller.
pub struct AgentPresetSectionController {
    transport: Rc<dyn AgentPresetSectionTransport>,
    roster_changed: RosterChanged,
    store: Rc<SnapshotStore<AgentPresetSectionState>>,
}

impl AgentPresetSectionController {
    /// Creates an idle management controller.
    #[must_use]
    pub fn new(
        transport: Rc<dyn AgentPresetSectionTransport>,
        roster_changed: Option<RosterChanged>,
    ) -> Rc<Self> {
        Rc::new(Self {
            transport,
            roster_changed: roster_changed.unwrap_or_else(|| Rc::new(|| {})),
            store: preset_snapshot_store(AgentPresetSectionState::default()),
        })
    }

    /// Reference-stable observable page state.
    #[must_use]
    pub fn store(&self) -> Rc<SnapshotStore<AgentPresetSectionState>> {
        self.store.clone()
    }

    fn set(&self, update: impl FnOnce(&mut AgentPresetSectionState)) {
        let mut next = self.store.snapshot().as_ref().clone();
        update(&mut next);
        self.store.set(next);
    }

    fn patch_copy(&self, update: impl FnOnce(&mut CopyDraft)) {
        self.set(|state| {
            if let Some(copy) = &mut state.copy {
                update(copy);
            }
        });
    }

    /// Loads the roster, refusing duplicate in-flight reads.
    pub async fn load(&self) {
        if self.store.snapshot().status == AgentPresetSectionStatus::Loading {
            return;
        }
        self.set(|state| {
            state.status = AgentPresetSectionStatus::Loading;
            state.error = None;
        });
        let roster = match self.transport.list().await {
            Ok(roster) => roster,
            Err(error) => {
                self.set(|state| {
                    state.status = AgentPresetSectionStatus::Error;
                    state.error = Some(error);
                });
                return;
            }
        };
        if roster.presets.is_empty() {
            self.set(|state| {
                state.status = AgentPresetSectionStatus::Unavailable;
                state.rows.clear();
                state.authorable = roster.authorable;
                state.has_document = roster.has_document;
                state.copy = None;
                state.view = None;
            });
            return;
        }
        let ids = roster
            .presets
            .iter()
            .map(|preset| preset.id.clone())
            .collect::<Vec<_>>();
        self.set(|state| {
            state.status = AgentPresetSectionStatus::Ready;
            state.error = None;
            state.authorable = roster.authorable;
            state.has_document = roster.has_document;
            state.rows = roster.presets;
            state
                .revealed_paths
                .retain(|id, _| ids.iter().any(|known| known == id));
        });
    }

    /// Opens a read-only composition viewer.
    pub async fn view(&self, id: &str) {
        self.set(|state| state.error = None);
        match self.transport.read(id.to_owned()).await {
            Ok(value) => self.set(|state| {
                state.view = Some(PresetView {
                    id: id.to_owned(),
                    title: value.name.unwrap_or_else(|| id.to_owned()),
                    content: value.content,
                });
            }),
            Err(error) => self.set(|state| state.error = Some(error)),
        }
    }

    /// Closes the read-only viewer.
    pub fn close_view(&self) {
        self.set(|state| state.view = None);
    }

    /// Opens a copy draft over one roster row.
    pub fn begin_copy(&self, from: &str) {
        let title = self
            .store
            .snapshot()
            .rows
            .iter()
            .find(|row| row.id == from)
            .and_then(|row| row.name.clone())
            .unwrap_or_else(|| from.to_owned());
        self.set(|state| {
            state.error = None;
            state.copy = Some(CopyDraft {
                source_id: from.to_owned(),
                source_title: title,
                id: String::new(),
                name: String::new(),
                saving: false,
                error: None,
            });
        });
    }

    /// Cancels the copy draft.
    pub fn cancel_copy(&self) {
        self.set(|state| state.copy = None);
    }

    /// Edits the new preset id and clears prior copy failure.
    pub fn set_copy_id(&self, id: &str) {
        self.patch_copy(|copy| {
            id.clone_into(&mut copy.id);
            copy.error = None;
        });
    }

    /// Edits the optional display name and clears prior copy failure.
    pub fn set_copy_name(&self, name: &str) {
        self.patch_copy(|copy| {
            name.clone_into(&mut copy.name);
            copy.error = None;
        });
    }

    /// Copies, reloads, broadcasts the roster change, and opens/reveals the new directory.
    pub async fn confirm_copy(&self) {
        let Some(draft) = self.store.snapshot().copy.clone() else {
            return;
        };
        if draft.saving || draft_blocker(&draft, &self.store.snapshot().rows).is_some() {
            return;
        }
        self.patch_copy(|copy| {
            copy.saving = true;
            copy.error = None;
        });
        let name = trim_ecmascript_whitespace(&draft.name);
        let name = (!name.is_empty()).then(|| name.to_owned());
        if let Err(error) = self
            .transport
            .copy(draft.source_id, draft.id.clone(), name)
            .await
        {
            self.patch_copy(|copy| {
                copy.saving = false;
                copy.error = Some(error);
            });
            return;
        }
        self.set(|state| state.copy = None);
        self.load().await;
        (self.roster_changed)();
        self.open_location(&draft.id).await;
    }

    /// Opens one directory or reveals its returned path.
    pub async fn open_location(&self, id: &str) {
        match self.transport.open_document(id.to_owned()).await {
            Ok(PresetOpenResult::Opened) => {}
            Ok(PresetOpenResult::Path(path)) => self.set(|state| {
                state.revealed_paths.insert(id.to_owned(), path);
            }),
            Err(error) => self.set(|state| state.error = Some(error)),
        }
    }

    /// Selects or dismisses the row awaiting delete confirmation.
    pub fn confirm_delete(&self, id: Option<&str>) {
        if self.store.snapshot().deleting {
            return;
        }
        self.set(|state| state.pending_delete = id.map(str::to_owned));
    }

    /// Removes the confirmed row and reloads the roster.
    pub async fn remove(&self) {
        let before = self.store.snapshot();
        let Some(id) = before.pending_delete.clone() else {
            return;
        };
        if before.deleting {
            return;
        }
        self.set(|state| {
            state.deleting = true;
            state.error = None;
        });
        if let Err(error) = self.transport.remove(id).await {
            self.set(|state| {
                state.deleting = false;
                state.pending_delete = None;
                state.error = Some(error);
            });
            return;
        }
        self.set(|state| {
            state.deleting = false;
            state.pending_delete = None;
        });
        self.load().await;
        (self.roster_changed)();
    }

    /// Writes a new default and reloads Host truth.
    pub async fn make_default(&self, id: &str) {
        if let Err(error) = self.transport.update_default(id.to_owned()).await {
            self.set(|state| state.error = Some(error));
            return;
        }
        self.load().await;
    }
}

fn ecmascript_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'
            | '\u{000A}'
            | '\u{000B}'
            | '\u{000C}'
            | '\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

fn trim_ecmascript_whitespace(value: &str) -> &str {
    value.trim_matches(ecmascript_whitespace)
}
