//! Portable differential contract for provider-owned Windows ACL grants.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
};

use parking_lot::Mutex;
use seekdeep_cordis::Context;
use seekdeep_core::session::SessionId;
use seekdeep_sandbox::{ConfinedSandboxMode, SandboxPolicy, SandboxProvider};
use seekdeep_sandbox_local::{
    AclGrantFactory, LocalAclWriteGrant, LocalSandboxConfig, LocalSandboxInstallation,
    SandboxInternals, install,
};
use seekdeep_sandbox_windows_acl::{temp_write_sid, workspace_write_sid};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Failure {
    AddStanding,
    AddRevocable,
    Dispose,
    Remove,
}

#[derive(Clone, Debug, Default)]
struct FailurePlan {
    enabled: HashSet<Failure>,
    create_at: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AddedPath {
    path: PathBuf,
    standing: bool,
}

#[derive(Clone, Debug)]
struct GrantRecord {
    sid: String,
    added: Vec<AddedPath>,
    disposed: bool,
}

#[derive(Debug, Default)]
struct MockState {
    grants: Vec<GrantRecord>,
    failures: FailurePlan,
    warnings: Vec<String>,
    removed: Vec<PathBuf>,
}

#[derive(Debug)]
struct MockGrant {
    state: Arc<Mutex<MockState>>,
    index: usize,
}

impl LocalAclWriteGrant for MockGrant {
    fn add(&mut self, path: &Path, standing: bool) -> anyhow::Result<()> {
        let mut state = self.state.lock();
        state.grants[self.index].added.push(AddedPath {
            path: path.to_owned(),
            standing,
        });
        if (standing && state.failures.enabled.contains(&Failure::AddStanding))
            || (!standing && state.failures.enabled.contains(&Failure::AddRevocable))
        {
            anyhow::bail!(
                "{} grant exploded",
                if standing { "workspace" } else { "temp" }
            );
        }
        Ok(())
    }

    fn dispose(self: Box<Self>) -> anyhow::Result<()> {
        let mut state = self.state.lock();
        if state.failures.enabled.contains(&Failure::Dispose) {
            anyhow::bail!("revoke exploded");
        }
        state.grants[self.index].disposed = true;
        Ok(())
    }
}

fn mock_internals(state: &Arc<Mutex<MockState>>) -> SandboxInternals {
    let factory_state = state.clone();
    let factory: AclGrantFactory = Arc::new(move |sid| {
        let mut state = factory_state.lock();
        let index = state.grants.len();
        if state.failures.create_at == Some(index) {
            anyhow::bail!("temp SID creation exploded");
        }
        state.grants.push(GrantRecord {
            sid: sid.to_owned(),
            added: Vec::new(),
            disposed: false,
        });
        drop(state);
        Ok(Box::new(MockGrant {
            state: factory_state.clone(),
            index,
        }))
    });
    let remove_state = state.clone();
    let warning_state = state.clone();
    SandboxInternals {
        platform: Some("win32".into()),
        windows_acl_runner_args: Some(vec!["windows-acl-runner".into()]),
        acl_grant_factory: Some(factory),
        remove_temp_dir: Some(Arc::new(move |path| {
            let mut state = remove_state.lock();
            state.removed.push(path.to_owned());
            if state.failures.enabled.contains(&Failure::Remove) {
                anyhow::bail!("rm exploded");
            }
            drop(state);
            match std::fs::remove_dir_all(path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error.into()),
            }
        })),
        cleanup_warning: Some(Arc::new(move |message| {
            warning_state.lock().warnings.push(message.to_owned());
        })),
        ..SandboxInternals::default()
    }
}

fn setup(state: &Arc<Mutex<MockState>>) -> LocalSandboxInstallation {
    let context = Context::new();
    let installation = install(&context, &LocalSandboxConfig::default()).unwrap();
    installation.provider.set_internals(mock_internals(state));
    installation
}

fn policy(workspace: &Path, session: Option<&str>, mode: ConfinedSandboxMode) -> SandboxPolicy {
    SandboxPolicy {
        mode,
        workspace_root: workspace.to_owned(),
        session_id: session.map(SessionId::new),
    }
}

fn flag(argv: &[String], name: &str) -> Option<String> {
    argv.iter()
        .position(|arg| arg == name)
        .and_then(|index| argv.get(index + 1))
        .cloned()
}

#[tokio::test]
async fn materializes_reuses_and_disposes_standing_workspace_and_private_temp_grants() {
    let state = Arc::new(Mutex::new(MockState::default()));
    let installation = setup(&state);
    let workspace = tempfile::tempdir().unwrap();
    let policy = policy(
        workspace.path(),
        Some("sess-1"),
        ConfinedSandboxMode::WorkspaceWrite,
    );
    let confined = installation
        .provider
        .confine(&["pwsh".into(), "x".into()], &policy)
        .unwrap();
    let temp = PathBuf::from(flag(&confined.argv, "--temp").unwrap());
    let temp_sid = flag(&confined.argv, "--temp-write-sid").unwrap();
    let workspace_text = workspace.path().to_str().unwrap();
    assert!(
        temp.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("seekdeep-")
    );
    assert_eq!(temp_sid, temp_write_sid(temp.to_str().unwrap()));
    assert_ne!(temp_sid, workspace_write_sid(workspace_text));
    assert_eq!(
        flag(&confined.argv, "--write-sid"),
        Some(workspace_write_sid(workspace_text))
    );
    assert!(temp.is_dir());
    assert_eq!(
        installation
            .provider
            .confine(&["pwsh".into(), "x".into()], &policy)
            .unwrap(),
        confined
    );
    {
        let state = state.lock();
        assert_eq!(state.grants.len(), 2);
        assert_eq!(state.grants[0].sid, workspace_write_sid(workspace_text));
        assert_eq!(state.grants[1].sid, temp_sid);
        assert_eq!(
            state.grants[0].added,
            vec![AddedPath {
                path: workspace.path().to_owned(),
                standing: true,
            }]
        );
        assert_eq!(
            state.grants[1].added,
            vec![AddedPath {
                path: temp.clone(),
                standing: false,
            }]
        );
    }

    installation.dispose().await.unwrap();
    assert!(!temp.exists());
    assert!(state.lock().grants.iter().all(|grant| grant.disposed));
}

#[tokio::test]
async fn modes_sessions_and_workspaces_have_the_source_capability_partition() {
    let state = Arc::new(Mutex::new(MockState::default()));
    let installation = setup(&state);
    let workspace_a = tempfile::tempdir().unwrap();
    let workspace_b = tempfile::tempdir().unwrap();
    let read_only = policy(
        workspace_a.path(),
        Some("parent"),
        ConfinedSandboxMode::ReadOnly,
    );
    let read = installation
        .provider
        .confine(&["true".into()], &read_only)
        .unwrap();
    assert!(flag(&read.argv, "--write-sid").is_none());
    assert_eq!(state.lock().grants.len(), 0);

    let parent_policy = policy(
        workspace_a.path(),
        Some("parent"),
        ConfinedSandboxMode::WorkspaceWrite,
    );
    let child_policy = policy(
        workspace_a.path(),
        Some("child"),
        ConfinedSandboxMode::WorkspaceWrite,
    );
    let moved_policy = policy(
        workspace_b.path(),
        Some("parent"),
        ConfinedSandboxMode::WorkspaceWrite,
    );
    let parent = installation
        .provider
        .confine(&["true".into()], &parent_policy)
        .unwrap();
    let child = installation
        .provider
        .confine(&["true".into()], &child_policy)
        .unwrap();
    let moved = installation
        .provider
        .confine(&["true".into()], &moved_policy)
        .unwrap();
    assert_ne!(flag(&parent.argv, "--temp"), flag(&child.argv, "--temp"));
    assert_ne!(flag(&parent.argv, "--temp"), flag(&moved.argv, "--temp"));
    assert_eq!(state.lock().grants.len(), 5);

    let agentless = installation
        .provider
        .confine(
            &["true".into()],
            &policy(
                workspace_a.path(),
                None,
                ConfinedSandboxMode::WorkspaceWrite,
            ),
        )
        .unwrap();
    assert!(flag(&agentless.argv, "--write-sid").is_none());
    assert_eq!(
        flag(&agentless.argv, "--temp"),
        Some(std::env::temp_dir().to_string_lossy().into_owned())
    );
    assert_eq!(state.lock().grants.len(), 5);
    installation.dispose().await.unwrap();
}

#[test]
fn workspace_overlap_and_workspace_grant_failures_happen_before_temp_creation() {
    let state = Arc::new(Mutex::new(MockState::default()));
    let installation = setup(&state);
    let temp_root = std::env::temp_dir().canonicalize().unwrap();
    let error = installation
        .provider
        .confine(
            &["true".into()],
            &policy(
                &temp_root,
                Some("overlap"),
                ConfinedSandboxMode::WorkspaceWrite,
            ),
        )
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("temp root must be outside the workspace")
    );
    assert!(state.lock().grants.is_empty());

    let workspace = tempfile::tempdir().unwrap();
    state.lock().failures.enabled.insert(Failure::AddStanding);
    let error = installation
        .provider
        .confine(
            &["true".into()],
            &policy(
                workspace.path(),
                Some("workspace-fail"),
                ConfinedSandboxMode::WorkspaceWrite,
            ),
        )
        .unwrap_err();
    assert!(error.to_string().contains("workspace grant exploded"));
    let state = state.lock();
    assert_eq!(state.grants.len(), 1);
    assert!(state.grants[0].disposed);
}

#[test]
fn temp_creation_and_add_failures_remove_the_random_directory_and_aggregate_cleanup() {
    let state = Arc::new(Mutex::new(MockState::default()));
    let installation = setup(&state);
    let workspace = tempfile::tempdir().unwrap();
    state.lock().failures.create_at = Some(1);
    let error = installation
        .provider
        .confine(
            &["true".into()],
            &policy(
                workspace.path(),
                Some("create-fail"),
                ConfinedSandboxMode::WorkspaceWrite,
            ),
        )
        .unwrap_err();
    assert!(error.to_string().contains("temp SID creation exploded"));
    assert_eq!(state.lock().removed.len(), 1);
    assert!(!state.lock().removed[0].exists());

    {
        let mut state = state.lock();
        state.failures.create_at = None;
        state.failures.enabled.insert(Failure::AddRevocable);
    }
    let error = installation
        .provider
        .confine(
            &["true".into()],
            &policy(
                workspace.path(),
                Some("add-fail"),
                ConfinedSandboxMode::WorkspaceWrite,
            ),
        )
        .unwrap_err();
    assert!(error.to_string().contains("temp grant exploded"));
    let failed_temp = state.lock().grants.last().unwrap().added[0].path.clone();
    assert!(!failed_temp.exists());

    {
        let mut state = state.lock();
        state.failures.enabled.insert(Failure::Dispose);
        state.failures.enabled.insert(Failure::Remove);
    }
    let error = installation
        .provider
        .confine(
            &["true".into()],
            &policy(
                workspace.path(),
                Some("aggregate-fail"),
                ConfinedSandboxMode::WorkspaceWrite,
            ),
        )
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("temp grant materialization failed and its cleanup also failed")
    );
}

#[tokio::test]
async fn teardown_reports_every_cleanup_failure_without_aborting_the_fiber() {
    let state = Arc::new(Mutex::new(MockState::default()));
    let installation = setup(&state);
    let workspace = tempfile::tempdir().unwrap();
    let confined = installation
        .provider
        .confine(
            &["true".into()],
            &policy(
                workspace.path(),
                Some("dispose"),
                ConfinedSandboxMode::WorkspaceWrite,
            ),
        )
        .unwrap();
    let temp = PathBuf::from(flag(&confined.argv, "--temp").unwrap());
    {
        let mut state = state.lock();
        state.failures.enabled.insert(Failure::Dispose);
        state.failures.enabled.insert(Failure::Remove);
    }
    installation.dispose().await.unwrap();
    let state = state.lock();
    assert!(temp.exists());
    assert!(state.warnings[0].contains("cleanup completed with 3 failure(s)"));
    assert_eq!(
        state
            .warnings
            .iter()
            .filter(|line| line.contains("revoke exploded"))
            .count(),
        2
    );
    assert!(
        state
            .warnings
            .iter()
            .any(|line| line.contains("rm exploded"))
    );
    drop(state);
    std::fs::remove_dir_all(temp).unwrap();
}

#[tokio::test]
async fn fresh_provider_never_reuses_a_resumed_sessions_private_temp_capability() {
    let workspace = tempfile::tempdir().unwrap();
    let first_state = Arc::new(Mutex::new(MockState::default()));
    let second_state = Arc::new(Mutex::new(MockState::default()));
    let first = setup(&first_state);
    let second = setup(&second_state);
    let policy = policy(
        workspace.path(),
        Some("resumed"),
        ConfinedSandboxMode::WorkspaceWrite,
    );
    let first_temp = flag(
        &first
            .provider
            .confine(&["true".into()], &policy)
            .unwrap()
            .argv,
        "--temp",
    )
    .unwrap();
    let second_temp = flag(
        &second
            .provider
            .confine(&["true".into()], &policy)
            .unwrap()
            .argv,
        "--temp",
    )
    .unwrap();
    assert_ne!(first_temp, second_temp);
    first.dispose().await.unwrap();
    second.dispose().await.unwrap();
}
