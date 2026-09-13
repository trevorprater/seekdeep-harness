//! React-free Workspace entity with a client-local materialization lifecycle.

use std::{cell::RefCell, rc::Rc};

use futures::{FutureExt, future::LocalBoxFuture};
use seekdeep_identity::{SessionId, WorkspaceId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    ClientRpcError, ClientRpcResult, Notifier, NotifierScheduler, RuntimeDisposer,
    SessionTransport, SessionTransportRequest,
};

/// One Workspace record projection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientWorkspaceView {
    /// Stable Workspace identity.
    pub workspace_id: WorkspaceId,
    /// Canonical directory path.
    pub path: String,
    /// Display title.
    pub title: String,
    /// Manually ordered accounted Sessions.
    pub session_ids: Vec<SessionId>,
    /// ISO-8601 creation instant.
    pub created_at: String,
    /// ISO-8601 last-mutation instant.
    pub updated_at: String,
}

/// Host input retained by a local Workspace until materialization succeeds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceCreateInput {
    /// Existing absolute path to adopt.
    pub path: String,
}

/// Observable state of a client-local Workspace intent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceIntentSnapshot {
    /// Last path segment.
    pub name: String,
    /// Local creation phase.
    pub phase: WorkspaceIntentPhase,
    /// Folded business or transport failure.
    pub error: Option<String>,
}

/// Local Workspace materialization phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceIntentPhase {
    /// Ready for a materialization attempt or retry.
    Ready,
    /// One shared create request is in flight.
    Creating,
}

/// A Workspace is either a local intent or a materialized Host view.
#[derive(Clone)]
pub struct WorkspaceSnapshot {
    /// Latest Host projection.
    pub view: Option<Rc<ClientWorkspaceView>>,
    /// Local creation state before materialization.
    pub intent: Option<Rc<WorkspaceIntentSnapshot>>,
}

struct WorkspaceIntent {
    input: WorkspaceCreateInput,
    snapshot: Rc<WorkspaceIntentSnapshot>,
}

type Materialization = futures::future::Shared<LocalBoxFuture<'static, ClientRpcResult<Value>>>;

struct WorkspaceState {
    view: Option<Rc<ClientWorkspaceView>>,
    intent: Option<WorkspaceIntent>,
    materialization: Option<(u64, Materialization)>,
    next_token: u64,
    snapshot: Rc<WorkspaceSnapshot>,
}

/// Observable Workspace object whose identity survives Host materialization.
pub struct ClientWorkspace {
    transport: Rc<dyn SessionTransport>,
    state: RefCell<WorkspaceState>,
    notifier: Rc<Notifier>,
}

impl ClientWorkspace {
    /// Creates one local Workspace intent.
    #[must_use]
    pub fn local(
        transport: Rc<dyn SessionTransport>,
        scheduler: Rc<dyn NotifierScheduler>,
        input: WorkspaceCreateInput,
    ) -> Rc<Self> {
        let intent = WorkspaceIntent {
            snapshot: Rc::new(WorkspaceIntentSnapshot {
                name: intent_name(&input),
                phase: WorkspaceIntentPhase::Ready,
                error: None,
            }),
            input,
        };
        Self::new(transport, scheduler, None, Some(intent))
    }

    /// Creates one already materialized Workspace.
    #[must_use]
    pub fn materialized(
        transport: Rc<dyn SessionTransport>,
        scheduler: Rc<dyn NotifierScheduler>,
        view: Rc<ClientWorkspaceView>,
    ) -> Rc<Self> {
        Self::new(transport, scheduler, Some(view), None)
    }

    fn new(
        transport: Rc<dyn SessionTransport>,
        scheduler: Rc<dyn NotifierScheduler>,
        view: Option<Rc<ClientWorkspaceView>>,
        intent: Option<WorkspaceIntent>,
    ) -> Rc<Self> {
        Rc::new_cyclic(|weak: &std::rc::Weak<Self>| {
            let initial = Rc::new(WorkspaceSnapshot {
                view: view.clone(),
                intent: intent.as_ref().map(|intent| intent.snapshot.clone()),
            });
            let weak_workspace = weak.clone();
            let notifier = Notifier::new(
                Rc::new(move || {
                    if let Some(workspace) = weak_workspace.upgrade() {
                        workspace.rebuild_snapshot();
                    }
                }),
                scheduler,
            );
            Self {
                transport,
                state: RefCell::new(WorkspaceState {
                    view,
                    intent,
                    materialization: None,
                    next_token: 0,
                    snapshot: initial,
                }),
                notifier,
            }
        })
    }

    /// Starts or shares materialization; an already materialized Workspace returns `None`.
    #[must_use]
    pub fn materialize(self: &Rc<Self>) -> Option<Materialization> {
        if let Some((_, task)) = &self.state.borrow().materialization {
            return Some(task.clone());
        }
        let (token, input) = {
            let mut state = self.state.borrow_mut();
            let intent = state.intent.as_mut()?;
            intent.snapshot = Rc::new(WorkspaceIntentSnapshot {
                name: intent.snapshot.name.clone(),
                phase: WorkspaceIntentPhase::Creating,
                error: None,
            });
            let input = intent.input.clone();
            state.next_token = state.next_token.wrapping_add(1);
            (state.next_token, input)
        };
        self.notifier.notify_now();
        let weak = Rc::downgrade(self);
        let transport = self.transport.clone();
        let task = async move {
            let result = call_folded(
                &transport,
                "workspace.create",
                serde_json::json!({"path":input.path}),
            )
            .await;
            weak.upgrade().map_or(result.clone(), |workspace| {
                workspace.finish_materialization(token, result)
            })
        }
        .boxed_local()
        .shared();
        self.state.borrow_mut().materialization = Some((token, task.clone()));
        Some(task)
    }

    /// Adopts a Host view without replacing this Workspace object.
    ///
    /// # Errors
    ///
    /// Returns when an established object is asked to change identity.
    pub fn adopt(&self, view: Rc<ClientWorkspaceView>) -> Result<(), String> {
        {
            let mut state = self.state.borrow_mut();
            if state
                .view
                .as_ref()
                .is_some_and(|known| known.workspace_id != view.workspace_id)
            {
                return Err("cannot adopt a different Workspace id".to_owned());
            }
            state.view = Some(view);
            state.intent = None;
        }
        self.notifier.mark_dirty();
        Ok(())
    }

    /// Subscribes to committed Workspace snapshot changes.
    #[must_use]
    pub fn subscribe(&self, listener: Rc<dyn Fn()>) -> RuntimeDisposer {
        self.notifier.subscribe(listener)
    }

    /// Returns the cached Workspace snapshot.
    #[must_use]
    pub fn snapshot(&self) -> Rc<WorkspaceSnapshot> {
        self.notifier.ensure_fresh();
        self.state.borrow().snapshot.clone()
    }

    fn finish_materialization(
        &self,
        token: u64,
        result: ClientRpcResult<Value>,
    ) -> ClientRpcResult<Value> {
        let result = normalize_workspace_create_result(result);
        let (active, owns_intent) = {
            let state = self.state.borrow();
            (
                state
                    .materialization
                    .as_ref()
                    .is_some_and(|(current, _)| *current == token),
                state.intent.is_some(),
            )
        };
        if !active {
            return result;
        }
        if !owns_intent {
            self.state.borrow_mut().materialization = None;
            return result;
        }
        match &result {
            ClientRpcResult::Success(Some(value)) => {
                let view = workspace_view(value.get("workspace"));
                if let Some(view) = view {
                    let mut state = self.state.borrow_mut();
                    state.view = Some(Rc::new(view));
                    state.intent = None;
                }
            }
            ClientRpcResult::Failure(error) => {
                if let Some(intent) = self.state.borrow_mut().intent.as_mut() {
                    intent.snapshot = Rc::new(WorkspaceIntentSnapshot {
                        name: intent.snapshot.name.clone(),
                        phase: WorkspaceIntentPhase::Ready,
                        error: Some(format!("{}: {}", error.code, error.message)),
                    });
                }
            }
            ClientRpcResult::Success(None) => {}
        }
        self.state.borrow_mut().materialization = None;
        self.notifier.mark_dirty();
        result
    }

    fn rebuild_snapshot(&self) {
        let mut state = self.state.borrow_mut();
        state.snapshot = Rc::new(WorkspaceSnapshot {
            view: state.view.clone(),
            intent: state.intent.as_ref().map(|intent| intent.snapshot.clone()),
        });
    }
}

fn normalize_workspace_create_result(result: ClientRpcResult<Value>) -> ClientRpcResult<Value> {
    match &result {
        ClientRpcResult::Success(Some(value))
            if workspace_view(value.get("workspace")).is_some()
                && value.get("created").and_then(Value::as_bool).is_some() =>
        {
            result
        }
        ClientRpcResult::Success(_) => ClientRpcResult::Failure(internal_error(
            "workspace.create response omitted a valid workspace or created flag",
        )),
        ClientRpcResult::Failure(_) => result,
    }
}

pub(crate) fn workspace_view(value: Option<&Value>) -> Option<ClientWorkspaceView> {
    serde_json::from_value(value?.clone()).ok()
}

pub(crate) async fn call_folded(
    transport: &Rc<dyn SessionTransport>,
    method: &str,
    payload: Value,
) -> ClientRpcResult<Value> {
    match transport
        .call(SessionTransportRequest {
            method: method.to_owned(),
            payload,
        })
        .await
    {
        Ok(result) => result,
        Err(error) => ClientRpcResult::Failure(internal_error(error)),
    }
}

pub(crate) fn internal_error(message: impl Into<String>) -> ClientRpcError {
    ClientRpcError {
        code: "internal".to_owned(),
        message: message.into(),
        details: serde_json::Map::new(),
    }
}

fn intent_name(input: &WorkspaceCreateInput) -> String {
    input
        .path
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(&input.path)
        .to_owned()
}
