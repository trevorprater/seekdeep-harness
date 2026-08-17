//! Exhaustive two-level request/value dispatch parity for every unary method.

use std::collections::BTreeSet;

use seekdeep_host_apiproxy::{
    ALL_RPC_METHODS, RpcError, RpcMethod, RpcResult, parse_unary_request, parse_unary_result,
    parse_unary_value,
};
use serde_json::{Map, Value, json};

fn workspace() -> Value {
    json!({
        "workspaceId": "w", "path": "/w", "title": "W", "sessionIds": [],
        "createdAt": "0", "updatedAt": "0"
    })
}

fn settings_namespace() -> Value {
    json!({
        "ns": "n", "schema": {}, "value": {}, "applies": "live",
        "secrets": [], "revision": 0
    })
}

#[test]
fn method_vocabulary_is_exact_unique_and_round_trips() {
    let expected = [
        "session.list",
        "session.search",
        "session.create",
        "session.history",
        "session.models",
        "session.selectModel",
        "session.rename",
        "session.fork",
        "session.prompt",
        "session.attachment",
        "session.updateQueue",
        "session.cancel",
        "subagent.list",
        "subagent.history",
        "subagent.prompt",
        "subagent.interrupt",
        "host.describe",
        "host.pickDirectory",
        "host.listDirectory",
        "host.createDirectory",
        "host.openPath",
        "workspace.list",
        "workspace.create",
        "workspace.rename",
        "workspace.delete",
        "workspace.insertBefore",
        "workspace.insertSessionBefore",
        "workspace.archiveSession",
        "skill.list",
        "agentPreset.list",
        "agentPreset.select",
        "agentPreset.read",
        "agentPreset.copy",
        "agentPreset.openDocument",
        "agentPreset.remove",
        "goal.create",
        "goal.edit",
        "goal.pause",
        "goal.resume",
        "goal.complete",
        "goal.clear",
        "settings.describe",
        "settings.openDocument",
        "settings.update",
        "settings.replace",
        "settings.mutate",
        "credentials.describe",
        "credentials.set",
        "credentials.unset",
        "llm.providers",
        "llm.models",
        "llm.discoverModels",
    ];
    assert_eq!(ALL_RPC_METHODS.len(), 52);
    assert_eq!(
        ALL_RPC_METHODS.map(RpcMethod::as_str),
        expected,
        "method ordering is an executable source-map snapshot"
    );
    assert_eq!(expected.into_iter().collect::<BTreeSet<_>>().len(), 52);
    for method in ALL_RPC_METHODS {
        assert_eq!(method.as_str().parse::<RpcMethod>().unwrap(), method);
        assert_eq!(method.to_string(), method.as_str());
    }
    assert!("events.mux".parse::<RpcMethod>().is_err());
    assert!("session.future".parse::<RpcMethod>().is_err());
}

#[test]
// Keeping the complete method/schema matrix in one literal makes omissions visible in review,
// and the array length gives us a compile-time count assertion.
#[allow(clippy::too_many_lines)]
fn every_method_has_a_working_request_and_value_schema_row() {
    let reference = json!({"id": "g", "revision": 1});
    let cases: [(RpcMethod, Value, Value); 52] = [
        (RpcMethod::SessionList, json!({}), json!({"items": []})),
        (
            RpcMethod::SessionSearch,
            json!({"query": "q"}),
            json!({"items": [], "hasMore": false}),
        ),
        (
            RpcMethod::SessionCreate,
            json!({}),
            json!({"sessionId": "s"}),
        ),
        (
            RpcMethod::SessionHistory,
            json!({"sessionId": "s"}),
            json!({"events": [], "hasMore": false}),
        ),
        (
            RpcMethod::SessionModels,
            json!({"sessionId": "s"}),
            json!({
                "current": {"provider": "p", "model": "m"}, "routable": true,
                "groups": [], "failures": []
            }),
        ),
        (
            RpcMethod::SessionSelectModel,
            json!({"sessionId": "s", "provider": "p", "model": "m"}),
            json!({"selected": {"provider": "p", "model": "m"}}),
        ),
        (
            RpcMethod::SessionRename,
            json!({"sessionId": "s", "title": "T"}),
            json!({"title": "T", "seq": 0}),
        ),
        (
            RpcMethod::SessionFork,
            json!({"sessionId": "s"}),
            json!({"sessionId": "c"}),
        ),
        (
            RpcMethod::SessionPrompt,
            json!({"sessionId": "s", "mode": "queue", "content": []}),
            json!({"accepted": true}),
        ),
        (
            RpcMethod::SessionAttachment,
            json!({"sessionId": "s", "attachmentId": "a"}),
            json!({
                "attachment": {"attachmentId": "a", "mediaType": "image/png", "bytes": 1, "width": 1, "height": 1},
                "data": "AA=="
            }),
        ),
        (
            RpcMethod::SessionUpdateQueue,
            json!({"sessionId": "s", "itemId": "i", "action": {"kind": "remove"}}),
            json!({"accepted": true}),
        ),
        (
            RpcMethod::SessionCancel,
            json!({"sessionId": "s"}),
            json!({"accepted": true}),
        ),
        (
            RpcMethod::SubagentList,
            json!({"parentSessionId": "p"}),
            json!({"entries": [], "parentAvailable": false}),
        ),
        (
            RpcMethod::SubagentHistory,
            json!({"parentSessionId": "p", "childSessionId": "c", "mode": "one-shot"}),
            json!({"events": [], "hasMore": false}),
        ),
        (
            RpcMethod::SubagentPrompt,
            json!({"parentSessionId": "p", "childSessionId": "c", "mode": "continuable", "content": []}),
            json!({"messageId": "i"}),
        ),
        (
            RpcMethod::SubagentInterrupt,
            json!({"parentSessionId": "p", "childSessionId": "c", "mode": "continuable"}),
            json!({"accepted": true}),
        ),
        (
            RpcMethod::HostDescribe,
            json!({}),
            json!({"version": "1", "cwd": "/", "attachedSessions": 0, "canOpenPath": false}),
        ),
        (
            RpcMethod::HostPickDirectory,
            json!({}),
            json!({"path": null}),
        ),
        (
            RpcMethod::HostListDirectory,
            json!({}),
            json!({"path": "/", "home": "/", "crumbs": [], "entries": [], "truncated": false}),
        ),
        (
            RpcMethod::HostCreateDirectory,
            json!({"path": "/", "name": "new"}),
            json!({"path": "/new"}),
        ),
        (
            RpcMethod::HostOpenPath,
            json!({"path": "/"}),
            json!({"opened": true}),
        ),
        (
            RpcMethod::WorkspaceList,
            json!({}),
            json!({"items": [], "archivedSessionIds": []}),
        ),
        (
            RpcMethod::WorkspaceCreate,
            json!({"path": "/w"}),
            json!({"workspace": workspace(), "created": true}),
        ),
        (
            RpcMethod::WorkspaceRename,
            json!({"workspaceId": "w", "title": "W"}),
            json!({"workspace": workspace()}),
        ),
        (
            RpcMethod::WorkspaceDelete,
            json!({"workspaceId": "w"}),
            json!({"deleted": true}),
        ),
        (
            RpcMethod::WorkspaceInsertBefore,
            json!({"workspaceId": "w"}),
            json!({"workspaceIds": ["w"]}),
        ),
        (
            RpcMethod::WorkspaceInsertSessionBefore,
            json!({"workspaceId": "w", "sessionId": "s"}),
            json!({"workspace": workspace()}),
        ),
        (
            RpcMethod::WorkspaceArchiveSession,
            json!({"sessionId": "s"}),
            json!({"archivedSessionIds": ["s"]}),
        ),
        (
            RpcMethod::SkillList,
            json!({"sessionId": "s"}),
            json!({"skills": []}),
        ),
        (
            RpcMethod::AgentPresetList,
            json!({}),
            json!({"presets": [], "authorable": false, "hasDocument": false}),
        ),
        (
            RpcMethod::AgentPresetSelect,
            json!({"sessionId": "s", "agentPreset": "p"}),
            json!({"agentPreset": "p"}),
        ),
        (
            RpcMethod::AgentPresetRead,
            json!({"agentPreset": "p"}),
            json!({"agentPreset": "p", "trust": "system", "content": ""}),
        ),
        (
            RpcMethod::AgentPresetCopy,
            json!({"from": "p", "agentPreset": "q"}),
            json!({"agentPreset": "q"}),
        ),
        (
            RpcMethod::AgentPresetOpenDocument,
            json!({"agentPreset": "p"}),
            json!({"opened": true}),
        ),
        (
            RpcMethod::AgentPresetRemove,
            json!({"agentPreset": "p"}),
            json!({}),
        ),
        (
            RpcMethod::GoalCreate,
            json!({"sessionId": "s", "objective": "O"}),
            json!({"ref": reference.clone()}),
        ),
        (
            RpcMethod::GoalEdit,
            json!({"sessionId": "s", "ref": reference.clone(), "objective": "O"}),
            json!({"ref": reference.clone()}),
        ),
        (
            RpcMethod::GoalPause,
            json!({"sessionId": "s", "ref": reference.clone()}),
            json!({"ref": reference.clone()}),
        ),
        (
            RpcMethod::GoalResume,
            json!({"sessionId": "s", "ref": reference.clone()}),
            json!({"ref": reference.clone()}),
        ),
        (
            RpcMethod::GoalComplete,
            json!({"sessionId": "s", "ref": reference.clone()}),
            json!({"ref": reference.clone()}),
        ),
        (
            RpcMethod::GoalClear,
            json!({"sessionId": "s", "ref": reference}),
            json!({"cleared": true}),
        ),
        (
            RpcMethod::SettingsDescribe,
            json!({}),
            json!({"writable": true, "hasDocument": false, "namespaces": []}),
        ),
        (
            RpcMethod::SettingsOpenDocument,
            json!({}),
            json!({"opened": true}),
        ),
        (
            RpcMethod::SettingsUpdate,
            json!({"ns": "n", "patch": {}}),
            settings_namespace(),
        ),
        (
            RpcMethod::SettingsReplace,
            json!({"ns": "n", "section": {}}),
            settings_namespace(),
        ),
        (
            RpcMethod::SettingsMutate,
            json!({"ns": "n", "ops": []}),
            settings_namespace(),
        ),
        (
            RpcMethod::CredentialsDescribe,
            json!({"refs": []}),
            json!({"credentials": {}}),
        ),
        (
            RpcMethod::CredentialsSet,
            json!({"ref": "KEY", "value": "v"}),
            json!({}),
        ),
        (
            RpcMethod::CredentialsUnset,
            json!({"ref": "KEY"}),
            json!({}),
        ),
        (RpcMethod::LlmProviders, json!({}), json!({"providers": []})),
        (
            RpcMethod::LlmModels,
            json!({}),
            json!({"groups": [], "failures": []}),
        ),
        (
            RpcMethod::LlmDiscoverModels,
            json!({"settingsNs": "n"}),
            json!({"models": []}),
        ),
    ];

    for (method, request, value) in cases {
        parse_unary_request(method.as_str(), &request)
            .unwrap_or_else(|error| panic!("{} request: {error:#}", method.as_str()));
        parse_unary_value(method.as_str(), &value)
            .unwrap_or_else(|error| panic!("{} value: {error:#}", method.as_str()));
    }
}

#[test]
fn registry_normalizes_objects_and_enforces_closed_error_or_required_success_value() {
    assert_eq!(
        parse_unary_request("session.search", &json!({"query": "  q  ", "extra": true})).unwrap(),
        json!({"query": "q"})
    );
    assert_eq!(
        parse_unary_value(
            "host.describe",
            &json!({
                "version": "1", "cwd": "/", "attachedSessions": 0,
                "canOpenPath": false, "extra": true
            })
        )
        .unwrap(),
        json!({"version": "1", "cwd": "/", "attachedSessions": 0, "canOpenPath": false})
    );
    assert!(parse_unary_request("future.method", &json!({})).is_err());
    assert!(parse_unary_value("session.list", &json!({"items": "bad"})).is_err());

    let success = parse_unary_result(
        "session.list",
        &RpcResult::Success {
            value: Some(json!({"items": [], "ignored": true})),
        },
    )
    .unwrap();
    assert_eq!(
        success,
        RpcResult::Success {
            value: Some(json!({"items": []}))
        }
    );
    assert!(parse_unary_result("session.list", &RpcResult::Success { value: None }).is_err());
    let future_error = RpcResult::Failure {
        error: RpcError {
            code: "future".to_owned(),
            message: "x".to_owned(),
            details: Map::new(),
        },
    };
    assert!(parse_unary_result("session.list", &future_error).is_err());
    let internal = RpcResult::Failure {
        error: RpcError {
            code: "internal".to_owned(),
            message: "x".to_owned(),
            details: Map::new(),
        },
    };
    assert!(parse_unary_result("session.list", &internal).is_ok());
}
