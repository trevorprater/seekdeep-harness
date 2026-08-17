//! Executable parity specifications for Host, Workspace, and Skill schemas.

use seekdeep_host_apiproxy::api::{
    host::{
        DirectoryListing, HostCreateDirectoryRequest, HostCreateDirectoryValue,
        HostDescribeRequest, HostDescribeValue, HostListDirectoryRequest, HostOpenPathRequest,
        HostOpenPathValue, HostPickDirectoryValue,
    },
    skills::{SkillListRequest, SkillListValue},
    workspace::{
        WorkspaceArchiveSessionRequest, WorkspaceArchiveSessionValue, WorkspaceCreateRequest,
        WorkspaceCreateValue, WorkspaceDeleteRequest, WorkspaceDeleteValue,
        WorkspaceInsertBeforeRequest, WorkspaceInsertBeforeValue,
        WorkspaceInsertSessionBeforeRequest, WorkspaceInsertSessionBeforeValue,
        WorkspaceListRequest, WorkspaceListValue, WorkspaceRenameRequest, WorkspaceRenameValue,
        WorkspaceView,
    },
};
use serde_json::json;

fn workspace() -> serde_json::Value {
    json!({
        "workspaceId": "w1",
        "path": "/p",
        "title": "p",
        "sessionIds": ["s1"],
        "createdAt": "2026-07-25T00:00:00.000Z",
        "updatedAt": "2026-07-25T00:00:00.000Z"
    })
}

#[test]
fn host_describe_picker_and_open_contracts_preserve_required_markers() {
    assert_eq!(
        serde_json::to_value(HostDescribeRequest::parse(&json!({"ignored": true})).unwrap())
            .unwrap(),
        json!({})
    );
    let description = HostDescribeValue::parse(&json!({
        "version": "1",
        "cwd": "/x",
        "provider": "p",
        "model": "m",
        "attachedSessions": 2,
        "canOpenPath": true
    }))
    .unwrap();
    assert_eq!(description.provider.as_deref(), Some("p"));
    assert_eq!(description.attached_sessions, 2);
    assert!(
        HostDescribeValue::parse(&json!({"version": "1", "cwd": "/x", "attachedSessions": 0}))
            .is_err()
    );
    assert!(
        HostDescribeValue::parse(&json!({
            "version": "1", "cwd": "/x", "provider": null,
            "attachedSessions": 0, "canOpenPath": false
        }))
        .is_err()
    );

    assert_eq!(
        HostPickDirectoryValue::parse(&json!({"path": null}))
            .unwrap()
            .path,
        None
    );
    assert!(HostPickDirectoryValue::parse(&json!({})).is_err());
    assert!(HostOpenPathRequest::parse(&json!({"path": "/x"})).is_ok());
    assert!(HostOpenPathRequest::parse(&json!({"path": ""})).is_err());
    assert!(HostOpenPathValue::parse(&json!({"opened": true})).is_ok());
    assert!(HostOpenPathValue::parse(&json!({"opened": false})).is_err());
}

#[test]
fn host_directory_browse_contract_requires_complete_rows_and_plain_segment_names() {
    assert_eq!(
        HostListDirectoryRequest::parse(&json!({})).unwrap().path,
        None
    );
    assert_eq!(
        HostListDirectoryRequest::parse(&json!({"path": "/x"}))
            .unwrap()
            .path
            .as_deref(),
        Some("/x")
    );
    assert!(HostListDirectoryRequest::parse(&json!({"path": null})).is_err());

    let listing = DirectoryListing::parse(&json!({
        "path": "/home/u/p",
        "home": "/home/u",
        "crumbs": [
            {"name": "/", "path": "/", "hidden": false},
            {"name": "p", "path": "/home/u/p", "hidden": false}
        ],
        "entries": [{"name": ".dot", "path": "/home/u/p/.dot", "hidden": true}],
        "truncated": false
    }))
    .unwrap();
    assert!(listing.entries[0].hidden);
    assert!(
        DirectoryListing::parse(&json!({
            "path": "/x", "home": "/x", "crumbs": [], "entries": []
        }))
        .is_err()
    );

    assert!(HostCreateDirectoryRequest::parse(&json!({"path": "/x", "name": "new"})).is_ok());
    for name in ["", " ", ".", "..", "a/b", "a\\b", "\u{FEFF}"] {
        assert!(
            HostCreateDirectoryRequest::parse(&json!({"path": "/x", "name": name})).is_err(),
            "accepted {name:?}"
        );
    }
    // ECMAScript trim does not classify U+0085 as whitespace.
    assert!(HostCreateDirectoryRequest::parse(&json!({"path": "/x", "name": "\u{0085}"})).is_ok());
    assert_eq!(
        HostCreateDirectoryValue::parse(&json!({"path": "/x/new"}))
            .unwrap()
            .path,
        "/x/new"
    );
}

#[test]
fn workspace_rows_list_create_and_rename_match_wire_contract() {
    let row = WorkspaceView::parse(&workspace()).unwrap();
    assert_eq!(row.workspace_id.as_str(), "w1");
    assert_eq!(row.session_ids[0].as_str(), "s1");
    assert!(WorkspaceView::parse(&json!({"workspaceId": ""})).is_err());

    assert!(WorkspaceListRequest::parse(&json!({})).is_ok());
    assert!(
        WorkspaceListValue::parse(&json!({"items": [workspace()], "archivedSessionIds": ["s1"]}))
            .is_ok()
    );
    assert!(WorkspaceListValue::parse(&json!({"items": [workspace()]})).is_err());

    assert_eq!(
        WorkspaceCreateRequest::parse(&json!({"path": "/p"}))
            .unwrap()
            .path,
        "/p"
    );
    assert!(WorkspaceCreateRequest::parse(&json!({"name": "retired"})).is_err());
    assert!(
        !WorkspaceCreateValue::parse(&json!({"workspace": workspace(), "created": false}))
            .unwrap()
            .created
    );

    let renamed = WorkspaceRenameRequest::parse(&json!({
        "workspaceId": "w1", "title": "  new  "
    }))
    .unwrap();
    assert_eq!(renamed.title, "  new  ");
    for title in ["", "  ", "\u{FEFF}"] {
        assert!(
            WorkspaceRenameRequest::parse(&json!({"workspaceId": "w1", "title": title})).is_err()
        );
    }
    assert!(WorkspaceRenameValue::parse(&json!({"workspace": workspace()})).is_ok());
}

#[test]
fn workspace_delete_and_both_insert_operations_keep_closed_receipts_and_optional_anchors() {
    assert!(WorkspaceDeleteRequest::parse(&json!({"workspaceId": "w1"})).is_ok());
    assert!(WorkspaceDeleteValue::parse(&json!({"deleted": true})).is_ok());
    assert!(WorkspaceDeleteValue::parse(&json!({"deleted": false})).is_err());

    assert_eq!(
        WorkspaceInsertBeforeRequest::parse(
            &json!({"workspaceId": "w1", "beforeWorkspaceId": "w2"})
        )
        .unwrap()
        .before_workspace_id
        .unwrap()
        .as_str(),
        "w2"
    );
    assert!(WorkspaceInsertBeforeRequest::parse(&json!({"workspaceId": "w1"})).is_ok());
    assert!(WorkspaceInsertBeforeRequest::parse(&json!({"beforeWorkspaceId": "w2"})).is_err());
    assert_eq!(
        WorkspaceInsertBeforeValue::parse(&json!({"workspaceIds": ["w2", "w1"]}))
            .unwrap()
            .workspace_ids
            .len(),
        2
    );

    assert!(
        WorkspaceInsertSessionBeforeRequest::parse(&json!({
            "workspaceId": "w1", "sessionId": "s1", "beforeSessionId": "s2"
        }))
        .is_ok()
    );
    assert!(
        WorkspaceInsertSessionBeforeRequest::parse(
            &json!({"workspaceId": "w1", "sessionId": "s1"})
        )
        .is_ok()
    );
    assert!(WorkspaceInsertSessionBeforeValue::parse(&json!({"workspace": workspace()})).is_ok());

    assert!(WorkspaceArchiveSessionRequest::parse(&json!({"sessionId": "s1"})).is_ok());
    assert!(WorkspaceArchiveSessionRequest::parse(&json!({})).is_err());
    assert_eq!(
        WorkspaceArchiveSessionValue::parse(&json!({"archivedSessionIds": ["s1", "s2"]}))
            .unwrap()
            .archived_session_ids
            .len(),
        2
    );
}

#[test]
fn skill_catalog_is_session_addressed_and_model_invocable_is_required() {
    assert_eq!(
        SkillListRequest::parse(&json!({"sessionId": "s1"}))
            .unwrap()
            .session_id
            .as_str(),
        "s1"
    );
    assert!(SkillListRequest::parse(&json!({})).is_err());
    let value = SkillListValue::parse(&json!({"skills": [
        {"name": "commit-helper", "description": "Git commits", "whenToUse": "when committing", "modelInvocable": true},
        {"name": "bare", "description": "No guidance", "modelInvocable": false}
    ]}))
    .unwrap();
    assert_eq!(
        value.skills[0].when_to_use.as_deref(),
        Some("when committing")
    );
    assert!(!value.skills[1].model_invocable);
    assert!(
        SkillListValue::parse(
            &json!({"skills": [{"name": "", "description": "d", "modelInvocable": true}]})
        )
        .is_err()
    );
    assert!(
        SkillListValue::parse(&json!({"skills": [{"name": "n", "description": "d"}]})).is_err()
    );
}
