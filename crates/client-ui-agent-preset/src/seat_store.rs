//! Target-portable new-session Agent preset seat semantics.

use std::{cell::RefCell, rc::Rc};

use futures::future::LocalBoxFuture;
use seekdeep_client_runtime::SnapshotStore;
use seekdeep_identity::SessionId;
use serde::Serialize;

use crate::{
    AgentPresetOption, RosterValue, preset_options, settings_store::preset_snapshot_store,
};

/// Transport used by the staged new-session seat.
pub trait AgentPresetSeatTransport {
    /// Reads the current roster.
    fn list(&self) -> LocalBoxFuture<'static, Result<RosterValue, String>>;
    /// Applies one preset to one blank Session and returns Host truth.
    fn select_session(
        &self,
        session_id: SessionId,
        agent_preset: String,
    ) -> LocalBoxFuture<'static, Result<String, String>>;
}

/// Current Session facts relevant to applying a staged preset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeatSessionSummary {
    /// Nominal Session identity.
    pub id: SessionId,
    /// Whether no Turn has started.
    pub blank: bool,
    /// Current composition identity when reported.
    pub agent_preset: Option<String>,
}

/// Hero seat snapshot.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentPresetSeatState {
    /// Healthy Host options.
    pub options: Vec<AgentPresetOption>,
    /// Staged/applied/default identity.
    pub current: String,
    /// Latest failure copy.
    pub error: Option<String>,
    /// Whether a select call is in flight.
    pub busy: bool,
    /// One-shot cross-screen introduction cue.
    pub introduce: bool,
}

type CurrentSession = Rc<dyn Fn() -> Option<SeatSessionSummary>>;
type AppliedCallback = Rc<dyn Fn(SessionId, String)>;

/// Stages the next Session's preset and spends that stage once.
pub struct AgentPresetSeatController {
    transport: Rc<dyn AgentPresetSeatTransport>,
    current_session: CurrentSession,
    on_applied: Option<AppliedCallback>,
    store: Rc<SnapshotStore<AgentPresetSeatState>>,
    fallback: RefCell<String>,
    staged: RefCell<Option<String>>,
}

impl AgentPresetSeatController {
    /// Creates an empty seat over the current-Session reader.
    #[must_use]
    pub fn new(
        transport: Rc<dyn AgentPresetSeatTransport>,
        current_session: CurrentSession,
        on_applied: Option<AppliedCallback>,
    ) -> Rc<Self> {
        Rc::new(Self {
            transport,
            current_session,
            on_applied,
            store: preset_snapshot_store(AgentPresetSeatState::default()),
            fallback: RefCell::new(String::new()),
            staged: RefCell::new(None),
        })
    }

    /// Reference-stable observable seat state.
    #[must_use]
    pub fn store(&self) -> Rc<SnapshotStore<AgentPresetSeatState>> {
        self.store.clone()
    }

    fn set(&self, update: impl FnOnce(&mut AgentPresetSeatState)) {
        let mut next = self.store.snapshot().as_ref().clone();
        update(&mut next);
        self.store.set(next);
    }

    /// Loads options while retaining any unspent stage or applied Session identity.
    pub async fn load(&self) {
        let roster = match self.transport.list().await {
            Ok(roster) => roster,
            Err(error) => {
                self.set(|state| state.error = Some(error));
                return;
            }
        };
        let fallback = roster
            .presets
            .iter()
            .find(|preset| preset.is_default)
            .or_else(|| roster.presets.first())
            .map_or_else(String::new, |preset| preset.id.clone());
        self.fallback.borrow_mut().clone_from(&fallback);
        let current = self
            .staged
            .borrow()
            .clone()
            .or_else(|| (self.current_session)().and_then(|session| session.agent_preset))
            .unwrap_or(fallback);
        self.set(|state| {
            state.options = preset_options(&roster.presets);
            state.current = current;
            state.error = None;
        });
    }

    /// Stages and immediately attempts to apply one option.
    pub async fn select(&self, id: &str) {
        if self.store.snapshot().busy {
            return;
        }
        self.stage(id, false);
        self.apply().await;
    }

    /// Stages a choice without applying it to the currently running Session.
    pub fn stage(&self, id: &str, introduce: bool) {
        *self.staged.borrow_mut() = Some(id.to_owned());
        self.set(|state| {
            id.clone_into(&mut state.current);
            state.error = None;
            state.introduce = introduce;
        });
    }

    /// Clears the one-shot introduction cue.
    pub fn introduced(&self) {
        if self.store.snapshot().introduce {
            self.set(|state| state.introduce = false);
        }
    }

    /// Applies an unspent stage to the current blank Session, if eligible.
    pub async fn apply(&self) {
        let Some(staged) = self.staged.borrow().clone() else {
            return;
        };
        let Some(session) = (self.current_session)() else {
            return;
        };
        if !session.blank || session.agent_preset.as_deref() == Some(staged.as_str()) {
            *self.staged.borrow_mut() = None;
            return;
        }
        self.set(|state| {
            state.busy = true;
            state.error = None;
        });
        let result = self
            .transport
            .select_session(session.id.clone(), staged)
            .await;
        *self.staged.borrow_mut() = None;
        match result {
            Ok(agent_preset) => {
                self.set(|state| {
                    state.busy = false;
                    state.current.clone_from(&agent_preset);
                });
                if let Some(on_applied) = &self.on_applied {
                    on_applied(session.id, agent_preset);
                }
            }
            Err(error) => {
                let fallback = self.fallback.borrow().clone();
                self.set(|state| {
                    state.busy = false;
                    state.error = Some(error);
                    state.current = fallback;
                });
            }
        }
    }
}
