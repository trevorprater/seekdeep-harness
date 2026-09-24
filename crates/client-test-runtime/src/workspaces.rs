//! Controllable Workspaces face with production-shaped snapshots and typed call records.

use std::{cell::RefCell, rc::Rc};

use futures::{FutureExt as _, future::LocalBoxFuture};
use seekdeep_client_runtime::{
    ClientWorkspaceView, RuntimeWorkspaceListState, SnapshotStore, StoreFlushMode,
    StoreFlushScheduler, WorkspaceCreateInput, WorkspaceListPhase, WorkspaceListState,
};
use seekdeep_host_directory_picker::{DirectoryEntry, DirectoryListing};
use seekdeep_identity::{SessionId, WorkspaceId};
use seekdeep_llm::AbortSignal;

/// Asynchronous action result used by typed Workspace stubs.
pub type TestWorkspaceFuture<T> = LocalBoxFuture<'static, Result<T, String>>;

/// Mutation runner used to integrate a list update with a test framework's stabilization step.
pub type TestStabilizer =
    Rc<dyn Fn(Box<dyn FnOnce()>) -> LocalBoxFuture<'static, Result<(), String>>>;

/// Immediate stabilizer for non-rendering Rust tests.
#[must_use]
pub fn immediate_stabilizer() -> TestStabilizer {
    Rc::new(|mutation| {
        mutation();
        async { Ok(()) }.boxed_local()
    })
}

/// One recorded call on the Workspace action face.
#[derive(Clone, Debug)]
pub enum TestWorkspaceCall {
    /// `connectWorkspace(workspaceId)`.
    ConnectWorkspace(WorkspaceId),
    /// `startSession(workspaceId?)`.
    StartSession(Option<WorkspaceId>),
    /// `create({ path })`.
    Create(WorkspaceCreateInput),
    /// `openPath(path)`.
    OpenPath(String),
    /// `pickDirectory()`.
    PickDirectory,
    /// `listDirectory(path?, signal?)`.
    ListDirectory {
        /// Optional directory target.
        path: Option<String>,
        /// Exact caller cancellation signal.
        signal: Option<AbortSignal>,
    },
    /// `createDirectory(path, name)`.
    CreateDirectory {
        /// Existing parent directory.
        path: String,
        /// Requested child name.
        name: String,
    },
    /// `rename(workspaceId, title)`.
    Rename {
        /// Target Workspace.
        workspace_id: WorkspaceId,
        /// Requested title.
        title: String,
    },
    /// `delete(workspaceId)`.
    Delete(WorkspaceId),
    /// `insertBefore(workspaceId, beforeWorkspaceId?)`.
    InsertBefore {
        /// Workspace being moved.
        workspace_id: WorkspaceId,
        /// Optional insertion anchor.
        before_workspace_id: Option<WorkspaceId>,
    },
    /// `insertSessionBefore(workspaceId, sessionId, beforeSessionId?)`.
    InsertSessionBefore {
        /// Destination Workspace.
        workspace_id: WorkspaceId,
        /// Session being moved.
        session_id: SessionId,
        /// Optional insertion anchor.
        before_session_id: Option<SessionId>,
    },
    /// `archiveSession(sessionId)`.
    ArchiveSession(SessionId),
}

impl TestWorkspaceCall {
    /// Source action name for this call record.
    #[must_use]
    pub const fn method(&self) -> &'static str {
        match self {
            Self::ConnectWorkspace(_) => "connectWorkspace",
            Self::StartSession(_) => "startSession",
            Self::Create(_) => "create",
            Self::OpenPath(_) => "openPath",
            Self::PickDirectory => "pickDirectory",
            Self::ListDirectory { .. } => "listDirectory",
            Self::CreateDirectory { .. } => "createDirectory",
            Self::Rename { .. } => "rename",
            Self::Delete(_) => "delete",
            Self::InsertBefore { .. } => "insertBefore",
            Self::InsertSessionBefore { .. } => "insertSessionBefore",
            Self::ArchiveSession(_) => "archiveSession",
        }
    }
}

type ConnectWorkspaceStub = Rc<dyn Fn(WorkspaceId) -> TestWorkspaceFuture<SessionId>>;
type StartSessionStub = Rc<dyn Fn(Option<WorkspaceId>)>;
type CreateStub = Rc<dyn Fn(WorkspaceCreateInput) -> TestWorkspaceFuture<ClientWorkspaceView>>;
type OpenPathStub = Rc<dyn Fn(String) -> TestWorkspaceFuture<()>>;
type PickDirectoryStub = Rc<dyn Fn() -> TestWorkspaceFuture<Option<String>>>;
type ListDirectoryStub =
    Rc<dyn Fn(Option<String>, Option<AbortSignal>) -> TestWorkspaceFuture<DirectoryListing>>;
type CreateDirectoryStub = Rc<dyn Fn(String, String) -> TestWorkspaceFuture<String>>;
type RenameStub = Rc<dyn Fn(WorkspaceId, String) -> TestWorkspaceFuture<ClientWorkspaceView>>;
type DeleteStub = Rc<dyn Fn(WorkspaceId) -> TestWorkspaceFuture<()>>;
type InsertBeforeStub = Rc<dyn Fn(WorkspaceId, Option<WorkspaceId>) -> TestWorkspaceFuture<()>>;
type InsertSessionBeforeStub = Rc<
    dyn Fn(WorkspaceId, SessionId, Option<SessionId>) -> TestWorkspaceFuture<ClientWorkspaceView>,
>;
type ArchiveSessionStub = Rc<dyn Fn(SessionId) -> TestWorkspaceFuture<()>>;

/// Typed replacement for one Workspace action.
pub enum TestWorkspaceStub {
    /// Replaces `connectWorkspace`.
    ConnectWorkspace(ConnectWorkspaceStub),
    /// Replaces `startSession`.
    StartSession(StartSessionStub),
    /// Replaces `create`.
    Create(CreateStub),
    /// Replaces `openPath`.
    OpenPath(OpenPathStub),
    /// Replaces `pickDirectory`.
    PickDirectory(PickDirectoryStub),
    /// Replaces `listDirectory`.
    ListDirectory(ListDirectoryStub),
    /// Replaces `createDirectory`.
    CreateDirectory(CreateDirectoryStub),
    /// Replaces `rename`.
    Rename(RenameStub),
    /// Replaces `delete`.
    Delete(DeleteStub),
    /// Replaces `insertBefore`.
    InsertBefore(InsertBeforeStub),
    /// Replaces `insertSessionBefore`.
    InsertSessionBefore(InsertSessionBeforeStub),
    /// Replaces `archiveSession`.
    ArchiveSession(ArchiveSessionStub),
}

#[derive(Default)]
struct WorkspaceStubs {
    connect_workspace: Option<ConnectWorkspaceStub>,
    start_session: Option<StartSessionStub>,
    create: Option<CreateStub>,
    open_path: Option<OpenPathStub>,
    pick_directory: Option<PickDirectoryStub>,
    list_directory: Option<ListDirectoryStub>,
    create_directory: Option<CreateDirectoryStub>,
    rename: Option<RenameStub>,
    delete: Option<DeleteStub>,
    insert_before: Option<InsertBeforeStub>,
    insert_session_before: Option<InsertSessionBeforeStub>,
    archive_session: Option<ArchiveSessionStub>,
}

#[derive(Debug)]
struct ImmediateStoreScheduler;

impl StoreFlushScheduler for ImmediateStoreScheduler {
    fn queue(&self, callback: Box<dyn FnOnce()>) {
        callback();
    }
}

/// Workspaces test double with an observable production-shaped list and recorded actions.
pub struct TestWorkspaces {
    list: Rc<SnapshotStore<RuntimeWorkspaceListState>>,
    calls: Rc<RefCell<Vec<TestWorkspaceCall>>>,
    stubs: Rc<RefCell<WorkspaceStubs>>,
    stabilize: TestStabilizer,
}

impl std::fmt::Debug for TestWorkspaces {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TestWorkspaces")
            .field("list", &self.list)
            .field("calls", &self.calls.borrow().len())
            .finish_non_exhaustive()
    }
}

impl Default for TestWorkspaces {
    fn default() -> Self {
        Self::new(immediate_stabilizer())
    }
}

impl TestWorkspaces {
    /// Constructs an empty ready list under the supplied stabilization policy.
    #[must_use]
    pub fn new(stabilize: TestStabilizer) -> Self {
        Self {
            list: SnapshotStore::new(
                workspace_list_state(),
                StoreFlushMode::Sync,
                Rc::new(ImmediateStoreScheduler),
                None,
                Rc::new(|_| {}),
            ),
            calls: Rc::new(RefCell::new(Vec::new())),
            stubs: Rc::new(RefCell::new(WorkspaceStubs::default())),
            stabilize,
        }
    }

    /// Observable Workspace list used by renderer standard props.
    #[must_use]
    pub fn list(&self) -> Rc<SnapshotStore<RuntimeWorkspaceListState>> {
        self.list.clone()
    }

    /// Recorded action calls, oldest first.
    #[must_use]
    pub fn calls(&self) -> Vec<TestWorkspaceCall> {
        self.calls.borrow().clone()
    }

    /// Installs or replaces one typed action stub.
    pub fn stub(&self, stub: TestWorkspaceStub) {
        let mut stubs = self.stubs.borrow_mut();
        match stub {
            TestWorkspaceStub::ConnectWorkspace(value) => stubs.connect_workspace = Some(value),
            TestWorkspaceStub::StartSession(value) => stubs.start_session = Some(value),
            TestWorkspaceStub::Create(value) => stubs.create = Some(value),
            TestWorkspaceStub::OpenPath(value) => stubs.open_path = Some(value),
            TestWorkspaceStub::PickDirectory(value) => stubs.pick_directory = Some(value),
            TestWorkspaceStub::ListDirectory(value) => stubs.list_directory = Some(value),
            TestWorkspaceStub::CreateDirectory(value) => stubs.create_directory = Some(value),
            TestWorkspaceStub::Rename(value) => stubs.rename = Some(value),
            TestWorkspaceStub::Delete(value) => stubs.delete = Some(value),
            TestWorkspaceStub::InsertBefore(value) => stubs.insert_before = Some(value),
            TestWorkspaceStub::InsertSessionBefore(value) => {
                stubs.insert_session_before = Some(value);
            }
            TestWorkspaceStub::ArchiveSession(value) => stubs.archive_session = Some(value),
        }
    }

    /// Replaces the list snapshot through the injected stabilization owner.
    ///
    /// # Errors
    ///
    /// Returns the stabilizer failure.
    pub async fn update(
        &self,
        mutation: impl FnOnce(&mut RuntimeWorkspaceListState) + 'static,
    ) -> Result<(), String> {
        let list = self.list.clone();
        (self.stabilize)(Box::new(move || list.update(mutation))).await
    }

    /// Records and resolves one Workspace-to-blank-Session connection.
    ///
    /// # Errors
    ///
    /// Returns the installed stub failure.
    pub async fn connect_workspace(&self, workspace_id: WorkspaceId) -> Result<SessionId, String> {
        self.calls
            .borrow_mut()
            .push(TestWorkspaceCall::ConnectWorkspace(workspace_id.clone()));
        let stub = self.stubs.borrow().connect_workspace.clone();
        if let Some(stub) = stub {
            return stub(workspace_id).await;
        }
        Ok(SessionId::new(format!(
            "session-of-{}",
            workspace_id.as_str()
        )))
    }

    /// Records one New Session intent and calls its optional synchronous stub.
    pub fn start_session(&self, workspace_id: Option<WorkspaceId>) {
        self.calls
            .borrow_mut()
            .push(TestWorkspaceCall::StartSession(workspace_id.clone()));
        if let Some(stub) = self.stubs.borrow().start_session.clone() {
            stub(workspace_id);
        }
    }

    /// Records and creates one Workspace view.
    ///
    /// # Errors
    ///
    /// Returns the installed stub failure.
    pub async fn create(&self, input: WorkspaceCreateInput) -> Result<ClientWorkspaceView, String> {
        self.calls
            .borrow_mut()
            .push(TestWorkspaceCall::Create(input.clone()));
        let stub = self.stubs.borrow().create.clone();
        if let Some(stub) = stub {
            return stub(input).await;
        }
        Ok(ClientWorkspaceView {
            workspace_id: WorkspaceId::new(format!("ws-{}", input.path)),
            title: input.path.clone(),
            path: input.path,
            session_ids: Vec::new(),
            created_at: String::new(),
            updated_at: String::new(),
        })
    }

    /// Records a Host path-open request.
    ///
    /// # Errors
    ///
    /// Returns the installed stub failure.
    pub async fn open_path(&self, path: String) -> Result<(), String> {
        self.calls
            .borrow_mut()
            .push(TestWorkspaceCall::OpenPath(path.clone()));
        let stub = self.stubs.borrow().open_path.clone();
        if let Some(stub) = stub {
            stub(path).await
        } else {
            Ok(())
        }
    }

    /// Records a directory-picker request; the default represents cancellation.
    ///
    /// # Errors
    ///
    /// Returns the installed stub failure.
    pub async fn pick_directory(&self) -> Result<Option<String>, String> {
        self.calls
            .borrow_mut()
            .push(TestWorkspaceCall::PickDirectory);
        let stub = self.stubs.borrow().pick_directory.clone();
        if let Some(stub) = stub {
            stub().await
        } else {
            Ok(None)
        }
    }

    /// Records and resolves one directory listing with the caller's exact signal.
    ///
    /// # Errors
    ///
    /// Returns the installed stub failure.
    pub async fn list_directory(
        &self,
        path: Option<String>,
        signal: Option<AbortSignal>,
    ) -> Result<DirectoryListing, String> {
        self.calls
            .borrow_mut()
            .push(TestWorkspaceCall::ListDirectory {
                path: path.clone(),
                signal: signal.clone(),
            });
        let stub = self.stubs.borrow().list_directory.clone();
        if let Some(stub) = stub {
            return stub(path, signal).await;
        }
        Ok(default_directory_listing())
    }

    /// Records and creates one child directory path.
    ///
    /// # Errors
    ///
    /// Returns the installed stub failure.
    pub async fn create_directory(&self, path: String, name: String) -> Result<String, String> {
        self.calls
            .borrow_mut()
            .push(TestWorkspaceCall::CreateDirectory {
                path: path.clone(),
                name: name.clone(),
            });
        let stub = self.stubs.borrow().create_directory.clone();
        if let Some(stub) = stub {
            stub(path, name).await
        } else {
            Ok(format!("{path}/{name}"))
        }
    }

    /// Records and resolves one Workspace rename.
    ///
    /// # Errors
    ///
    /// Returns the installed stub failure.
    pub async fn rename(
        &self,
        workspace_id: WorkspaceId,
        title: String,
    ) -> Result<ClientWorkspaceView, String> {
        self.calls.borrow_mut().push(TestWorkspaceCall::Rename {
            workspace_id: workspace_id.clone(),
            title: title.clone(),
        });
        let stub = self.stubs.borrow().rename.clone();
        if let Some(stub) = stub {
            return stub(workspace_id, title).await;
        }
        Ok(ClientWorkspaceView {
            workspace_id,
            path: format!("/{title}"),
            title,
            session_ids: Vec::new(),
            created_at: String::new(),
            updated_at: String::new(),
        })
    }

    /// Records one Workspace deletion.
    ///
    /// # Errors
    ///
    /// Returns the installed stub failure.
    pub async fn delete(&self, workspace_id: WorkspaceId) -> Result<(), String> {
        self.calls
            .borrow_mut()
            .push(TestWorkspaceCall::Delete(workspace_id.clone()));
        let stub = self.stubs.borrow().delete.clone();
        if let Some(stub) = stub {
            stub(workspace_id).await
        } else {
            Ok(())
        }
    }

    /// Records one Workspace reorder.
    ///
    /// # Errors
    ///
    /// Returns the installed stub failure.
    pub async fn insert_before(
        &self,
        workspace_id: WorkspaceId,
        before_workspace_id: Option<WorkspaceId>,
    ) -> Result<(), String> {
        self.calls
            .borrow_mut()
            .push(TestWorkspaceCall::InsertBefore {
                workspace_id: workspace_id.clone(),
                before_workspace_id: before_workspace_id.clone(),
            });
        let stub = self.stubs.borrow().insert_before.clone();
        if let Some(stub) = stub {
            stub(workspace_id, before_workspace_id).await
        } else {
            Ok(())
        }
    }

    /// Records one accounted-Session move and resolves the updated Workspace view.
    ///
    /// # Errors
    ///
    /// Returns the installed stub failure.
    pub async fn insert_session_before(
        &self,
        workspace_id: WorkspaceId,
        session_id: SessionId,
        before_session_id: Option<SessionId>,
    ) -> Result<ClientWorkspaceView, String> {
        self.calls
            .borrow_mut()
            .push(TestWorkspaceCall::InsertSessionBefore {
                workspace_id: workspace_id.clone(),
                session_id: session_id.clone(),
                before_session_id: before_session_id.clone(),
            });
        let stub = self.stubs.borrow().insert_session_before.clone();
        if let Some(stub) = stub {
            return stub(workspace_id, session_id, before_session_id).await;
        }
        Ok(ClientWorkspaceView {
            workspace_id,
            path: String::new(),
            title: String::new(),
            session_ids: vec![session_id],
            created_at: String::new(),
            updated_at: String::new(),
        })
    }

    /// Records one archive request; the default appends to the observable archive list.
    ///
    /// # Errors
    ///
    /// Returns the installed stub or stabilizer failure.
    pub async fn archive_session(&self, session_id: SessionId) -> Result<(), String> {
        self.calls
            .borrow_mut()
            .push(TestWorkspaceCall::ArchiveSession(session_id.clone()));
        let stub = self.stubs.borrow().archive_session.clone();
        if let Some(stub) = stub {
            return stub(session_id).await;
        }
        self.update(move |state| {
            let mut archived = state.archived_session_ids.as_ref().clone();
            archived.push(session_id);
            state.archived_session_ids = Rc::new(archived);
        })
        .await
    }
}

/// Ready empty Workspace state projected after both baselines settle.
#[must_use]
pub fn workspace_list_state() -> RuntimeWorkspaceListState {
    RuntimeWorkspaceListState {
        items: Rc::new(Vec::new()),
        archived_session_ids: Rc::new(Vec::new()),
        state: WorkspaceListState::Idle,
        phase: WorkspaceListPhase::Ready,
        error: None,
        baselines_ready: true,
        recent_workspace_id: None,
    }
}

fn default_directory_listing() -> DirectoryListing {
    DirectoryListing {
        path: "/home/test".to_owned(),
        home: "/home/test".to_owned(),
        crumbs: [("/", "/"), ("home", "/home"), ("test", "/home/test")]
            .into_iter()
            .map(|(name, path)| DirectoryEntry {
                name: name.to_owned(),
                path: path.to_owned(),
                hidden: false,
            })
            .collect(),
        entries: Vec::new(),
        truncated: false,
    }
}
