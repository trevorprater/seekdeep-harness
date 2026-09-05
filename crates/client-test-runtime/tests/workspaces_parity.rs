//! Behavioral mirror of the Workspaces groups in the source test runtime.

#![cfg(not(target_arch = "wasm32"))]

use std::{cell::Cell, rc::Rc};

use futures::FutureExt as _;
use seekdeep_client_runtime::{WorkspaceCreateInput, WorkspaceListPhase};
use seekdeep_client_test_runtime::{TestWorkspaceCall, TestWorkspaceStub, TestWorkspaces};
use seekdeep_host_directory_picker::DirectoryListing;
use seekdeep_identity::{SessionId, WorkspaceId};
use seekdeep_llm::AbortSignal;

#[test]
fn list_updates_actions_and_connection_stub_follow_the_stabilizer() {
    let stabilizations = Rc::new(Cell::new(0_usize));
    let observed = stabilizations.clone();
    let workspaces = TestWorkspaces::new(Rc::new(move |mutation| {
        observed.set(observed.get() + 1);
        mutation();
        async { Ok(()) }.boxed_local()
    }));
    assert_eq!(
        workspaces.list().snapshot().phase,
        WorkspaceListPhase::Ready
    );
    futures::executor::block_on(workspaces.update(|state| {
        state.phase = WorkspaceListPhase::Pending;
    }))
    .unwrap();
    assert_eq!(
        workspaces.list().snapshot().phase,
        WorkspaceListPhase::Pending
    );
    assert_eq!(stabilizations.get(), 1);

    workspaces.start_session(Some(WorkspaceId::new("w1")));
    assert_eq!(
        futures::executor::block_on(workspaces.connect_workspace(WorkspaceId::new("w2")))
            .unwrap()
            .as_str(),
        "session-of-w2"
    );
    assert_eq!(
        workspaces
            .calls()
            .iter()
            .map(TestWorkspaceCall::method)
            .collect::<Vec<_>>(),
        ["startSession", "connectWorkspace"]
    );
    workspaces.stub(TestWorkspaceStub::ConnectWorkspace(Rc::new(|_| {
        async { Ok(SessionId::new("other")) }.boxed_local()
    })));
    assert_eq!(
        futures::executor::block_on(workspaces.connect_workspace(WorkspaceId::new("w3")))
            .unwrap()
            .as_str(),
        "other"
    );
}

#[test]
fn browse_defaults_keep_root_to_target_crumbs_and_forward_the_signal() {
    let workspaces = TestWorkspaces::default();
    let listing = futures::executor::block_on(workspaces.list_directory(None, None)).unwrap();
    assert_eq!(listing.path, "/home/test");
    assert!(listing.entries.is_empty());
    assert_eq!(
        listing
            .crumbs
            .iter()
            .map(|crumb| crumb.path.as_str())
            .collect::<Vec<_>>(),
        ["/", "/home", "/home/test"]
    );
    assert_eq!(
        futures::executor::block_on(
            workspaces.create_directory("/home/test".to_owned(), "fresh".to_owned())
        )
        .unwrap(),
        "/home/test/fresh"
    );

    let signal = AbortSignal::default();
    let listing = DirectoryListing {
        path: "/x".to_owned(),
        home: "/x".to_owned(),
        crumbs: Vec::new(),
        entries: Vec::new(),
        truncated: false,
    };
    let expected = listing.clone();
    workspaces.stub(TestWorkspaceStub::ListDirectory(Rc::new(
        move |path, received| {
            assert_eq!(path.as_deref(), Some("/x"));
            received.expect("signal forwarded").abort();
            let listing = expected.clone();
            async move { Ok(listing) }.boxed_local()
        },
    )));
    let returned = futures::executor::block_on(
        workspaces.list_directory(Some("/x".to_owned()), Some(signal.clone())),
    )
    .unwrap();
    assert_eq!(returned, listing);
    assert!(signal.is_aborted());
}

#[test]
#[allow(clippy::too_many_lines)]
fn action_defaults_record_every_verb_and_stubs_replace_behavior() {
    let workspaces = TestWorkspaces::default();
    let created = futures::executor::block_on(workspaces.create(WorkspaceCreateInput {
        path: "/tmp/alpha".to_owned(),
    }))
    .unwrap();
    assert_eq!(created.title, "/tmp/alpha");
    assert!(
        futures::executor::block_on(workspaces.pick_directory())
            .unwrap()
            .is_none()
    );
    let renamed = futures::executor::block_on(
        workspaces.rename(WorkspaceId::new("w1"), "Renamed".to_owned()),
    )
    .unwrap();
    assert_eq!(renamed.title, "Renamed");
    futures::executor::block_on(workspaces.delete(WorkspaceId::new("w1"))).unwrap();
    futures::executor::block_on(workspaces.open_path("/proj/file.ts".to_owned())).unwrap();
    futures::executor::block_on(
        workspaces.insert_before(WorkspaceId::new("w1"), Some(WorkspaceId::new("w2"))),
    )
    .unwrap();
    let moved = futures::executor::block_on(workspaces.insert_session_before(
        WorkspaceId::new("w1"),
        SessionId::new("s1"),
        Some(SessionId::new("s2")),
    ))
    .unwrap();
    assert_eq!(moved.session_ids, [SessionId::new("s1")]);
    futures::executor::block_on(workspaces.archive_session(SessionId::new("s1"))).unwrap();
    assert_eq!(
        workspaces.list().snapshot().archived_session_ids.as_ref(),
        &[SessionId::new("s1")]
    );
    assert_eq!(
        workspaces
            .calls()
            .iter()
            .map(TestWorkspaceCall::method)
            .collect::<Vec<_>>(),
        [
            "create",
            "pickDirectory",
            "rename",
            "delete",
            "openPath",
            "insertBefore",
            "insertSessionBefore",
            "archiveSession",
        ]
    );

    workspaces.stub(TestWorkspaceStub::PickDirectory(Rc::new(|| {
        async { Ok(Some("/picked".to_owned())) }.boxed_local()
    })));
    workspaces.stub(TestWorkspaceStub::ArchiveSession(Rc::new(|_| {
        async { Ok(()) }.boxed_local()
    })));
    assert_eq!(
        futures::executor::block_on(workspaces.pick_directory())
            .unwrap()
            .as_deref(),
        Some("/picked")
    );
    futures::executor::block_on(workspaces.archive_session(SessionId::new("s2"))).unwrap();
    assert_eq!(
        workspaces.list().snapshot().archived_session_ids.as_ref(),
        &[SessionId::new("s1")]
    );
}
