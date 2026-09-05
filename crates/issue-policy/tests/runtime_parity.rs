//! REST/GraphQL assembly, lifecycle mutation, audit, and HTTP transport parity.

use std::{
    io::{Read as _, Write as _},
    net::TcpListener,
    process::Command,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use async_trait::async_trait;
use seekdeep_issue_policy::*;
use serde_json::{Value, json};

#[derive(Clone, Debug)]
struct PlannedResponse {
    method: ApiMethod,
    path: String,
    value: Option<Value>,
}

#[derive(Clone, Debug, Default)]
struct FakeTransport {
    responses: Arc<Mutex<Vec<PlannedResponse>>>,
    requests: Arc<Mutex<Vec<ApiRequest>>>,
}

impl FakeTransport {
    fn respond(&self, method: ApiMethod, path: impl Into<String>, value: Option<Value>) {
        self.responses.lock().unwrap().push(PlannedResponse {
            method,
            path: path.into(),
            value,
        });
    }

    fn requests(&self) -> Vec<ApiRequest> {
        self.requests.lock().unwrap().clone()
    }

    fn assert_drained(&self) {
        assert!(self.responses.lock().unwrap().is_empty());
    }
}

#[async_trait]
impl GitHubTransport for FakeTransport {
    async fn request(&self, request: ApiRequest) -> Result<Option<Value>> {
        self.requests.lock().unwrap().push(request.clone());
        let mut responses = self.responses.lock().unwrap();
        let index = responses
            .iter()
            .position(|response| response.method == request.method && response.path == request.path)
            .ok_or_else(|| anyhow::anyhow!("unexpected {:?} {}", request.method, request.path))?;
        Ok(responses.remove(index).value)
    }
}

fn config() -> IssuePolicyConfig {
    IssuePolicyConfig::bundled().unwrap()
}

fn issue(number: u64, valid: bool) -> Value {
    json!({
        "node_id": format!("ISSUE_{number}"),
        "title": if valid { "完成议题管理校验" } else { "invalid title" },
        "body": if valid {
            "完成议题管理校验。\n\n<details><summary>验收与细节</summary>待补充。</details>"
        } else {
            "too exposed"
        },
        "assignees": [],
        "labels": [],
        "type": { "name": "Idea" },
        "state": "open",
        "state_reason": null
    })
}

fn fields(priority: Option<&str>) -> Value {
    json!([
        {
            "issue_field_name": "Priority",
            "single_select_option": priority.map(|name| json!({ "name": name }))
        }
    ])
}

fn project(status: Option<&str>, actor: Option<&str>) -> Value {
    let item = status.map(|status| {
        json!({
            "id": "ITEM_2",
            "project": { "id": "PROJECT_1" },
            "fieldValueByName": { "name": status, "optionId": "STATUS" }
        })
    });
    let event = actor.map(|actor| {
        json!({
            "actor": { "login": actor },
            "project": { "id": "PROJECT_1" },
            "status": status
        })
    });
    json!({
        "data": {
            "organization": {
                "projectV2": {
                    "id": "PROJECT_1",
                    "title": "SEEKDEEP Issue Management",
                    "fields": {
                        "nodes": [{
                            "id": "STATUS_FIELD",
                            "name": "Status",
                            "options": [
                                { "id": "INBOX", "name": "Inbox" },
                                { "id": "IN_PROGRESS", "name": "In progress" },
                                { "id": "IN_REVIEW", "name": "In review" },
                                { "id": "DONE", "name": "Done" },
                                { "id": "NO_ACTION", "name": "No action" }
                            ]
                        }]
                    }
                }
            },
            "repository": {
                "issue": {
                    "id": "ISSUE_2",
                    "timelineItems": { "nodes": event.into_iter().collect::<Vec<_>>() },
                    "projectItems": { "nodes": item.into_iter().collect::<Vec<_>>() }
                }
            }
        }
    })
}

fn mutation(name: &str) -> Value {
    json!({ "data": { name: { "id": "ok" } } })
}

fn repo_path(suffix: &str) -> String {
    format!("/repos/seekdeep-harness/seekdeep-harness{suffix}")
}

#[tokio::test]
async fn pull_request_check_fetches_real_issues_and_filters_pr_references() {
    let transport = FakeTransport::default();
    transport.respond(
        ApiMethod::Get,
        repo_path("/pulls/7"),
        Some(json!({
            "draft": false,
            "body": "Fixes #2\nRelated #3",
            "user": { "type": "User" },
            "labels": [
                { "name": "kind/feature" },
                { "name": "area/web" },
                { "name": "p2" }
            ]
        })),
    );
    transport.respond(
        ApiMethod::Get,
        repo_path("/pulls/7/requested_reviewers"),
        Some(json!({ "users": [{}], "teams": [] })),
    );
    transport.respond(
        ApiMethod::Get,
        repo_path("/pulls/7/reviews?per_page=100"),
        Some(json!([])),
    );
    transport.respond(ApiMethod::Get, repo_path("/issues/2"), Some(issue(2, true)));
    transport.respond(
        ApiMethod::Get,
        repo_path("/issues/2/issue-field-values?per_page=100"),
        Some(fields(Some("P2"))),
    );
    transport.respond(
        ApiMethod::Get,
        repo_path("/issues/3"),
        Some(json!({
            "title": "PR",
            "body": "",
            "assignees": [],
            "labels": [],
            "type": null,
            "state": "open",
            "state_reason": null,
            "pull_request": {}
        })),
    );
    let runtime = IssuePolicyRuntime::new(config(), transport.clone());
    let outcome = runtime
        .check_pull_request_event(&json!({ "pull_request": { "number": 7 } }))
        .await
        .unwrap();
    assert!(outcome.enforced);
    assert!(outcome.errors.is_empty());
    assert!(
        !transport
            .requests()
            .iter()
            .any(|request| request.path.contains("issues/3/issue-field-values"))
    );
    transport.assert_drained();
}

#[tokio::test]
async fn issue_open_sets_inbox_then_creates_one_audit_comment_for_invalid_metadata() {
    let transport = FakeTransport::default();
    transport.respond(
        ApiMethod::Post,
        "/graphql",
        Some(project(Some("Backlog"), None)),
    );
    transport.respond(
        ApiMethod::Post,
        "/graphql",
        Some(mutation("updateProjectV2ItemFieldValue")),
    );
    transport.respond(
        ApiMethod::Post,
        "/graphql",
        Some(project(Some("Inbox"), None)),
    );
    transport.respond(
        ApiMethod::Get,
        repo_path("/issues/2"),
        Some(issue(2, false)),
    );
    transport.respond(
        ApiMethod::Get,
        repo_path("/issues/2/issue-field-values?per_page=100"),
        Some(fields(None)),
    );
    transport.respond(
        ApiMethod::Post,
        "/graphql",
        Some(project(Some("Inbox"), None)),
    );
    transport.respond(
        ApiMethod::Get,
        repo_path("/issues/2/comments?per_page=100"),
        Some(json!([])),
    );
    transport.respond(
        ApiMethod::Post,
        repo_path("/issues/2/comments"),
        Some(json!({ "id": 9 })),
    );
    let runtime = IssuePolicyRuntime::new(config(), transport.clone());
    runtime
        .handle_lifecycle_event(
            "issues",
            &json!({ "action": "opened", "issue": { "number": 2 } }),
        )
        .await
        .unwrap();
    let requests = transport.requests();
    let update = requests
        .iter()
        .find(|request| {
            request
                .body
                .as_ref()
                .and_then(|body| body.get("query"))
                .and_then(Value::as_str)
                .is_some_and(|query| query.contains("updateProjectV2ItemFieldValue"))
        })
        .unwrap();
    assert_eq!(
        update
            .body
            .as_ref()
            .unwrap()
            .pointer("/variables/optionId")
            .and_then(Value::as_str),
        Some("INBOX")
    );
    let audit = requests
        .iter()
        .find(|request| request.method == ApiMethod::Post && request.path.ends_with("/comments"))
        .unwrap();
    let body = audit
        .body
        .as_ref()
        .unwrap()
        .get("body")
        .and_then(Value::as_str)
        .unwrap();
    assert!(body.starts_with("<!-- seekdeep-issue-policy -->"));
    assert!(body.contains("Issue 标题必须包含中文"));
    transport.assert_drained();
}

#[tokio::test]
async fn changes_requested_regresses_only_automation_owned_review_then_reaudits() {
    let transport = FakeTransport::default();
    transport.respond(
        ApiMethod::Get,
        repo_path("/pulls/7"),
        Some(json!({
            "draft": false,
            "body": "Fixes #2",
            "user": { "type": "User" },
            "labels": []
        })),
    );
    transport.respond(ApiMethod::Get, repo_path("/issues/2"), Some(issue(2, true)));
    transport.respond(
        ApiMethod::Get,
        repo_path("/issues/2/issue-field-values?per_page=100"),
        Some(fields(None)),
    );
    transport.respond(
        ApiMethod::Post,
        "/graphql",
        Some(project(
            Some("In review"),
            Some("seekdeep-issue-management"),
        )),
    );
    transport.respond(
        ApiMethod::Post,
        "/graphql",
        Some(mutation("updateProjectV2ItemFieldValue")),
    );
    transport.respond(ApiMethod::Get, repo_path("/issues/2"), Some(issue(2, true)));
    transport.respond(
        ApiMethod::Get,
        repo_path("/issues/2/issue-field-values?per_page=100"),
        Some(fields(None)),
    );
    transport.respond(
        ApiMethod::Post,
        "/graphql",
        Some(project(Some("In progress"), None)),
    );
    transport.respond(
        ApiMethod::Get,
        repo_path("/issues/2/comments?per_page=100"),
        Some(json!([])),
    );
    let runtime = IssuePolicyRuntime::new(config(), transport.clone());
    runtime
        .handle_lifecycle_event(
            "pull_request_review",
            &json!({
                "action": "submitted",
                "review": { "state": "changes_requested" },
                "pull_request": { "number": 7 }
            }),
        )
        .await
        .unwrap();
    let update = transport
        .requests()
        .into_iter()
        .find(|request| {
            request
                .body
                .as_ref()
                .and_then(|body| body.get("query"))
                .and_then(Value::as_str)
                .is_some_and(|query| query.contains("updateProjectV2ItemFieldValue"))
        })
        .unwrap();
    assert_eq!(
        update
            .body
            .unwrap()
            .pointer("/variables/optionId")
            .and_then(Value::as_str),
        Some("IN_PROGRESS")
    );
    transport.assert_drained();
}

#[tokio::test]
async fn reqwest_transport_sends_exact_auth_version_agent_and_json_body() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let read = socket.read(&mut buffer).await.unwrap();
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..read]);
            if let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..header_end + 4]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .map(str::to_owned)
                    })
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or_default();
                if bytes.len() >= header_end + 4 + content_length {
                    break;
                }
            }
        }
        let body = b"{\"ok\":true}";
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        socket.write_all(body).await.unwrap();
        String::from_utf8(bytes).unwrap()
    });
    let transport = ReqwestGitHubTransport::new(
        format!("http://{address}"),
        GitHubToken::new("test-token".to_owned()).unwrap(),
    )
    .unwrap();
    let response = transport
        .request(ApiRequest {
            method: ApiMethod::Post,
            path: "/graphql".to_owned(),
            body: Some(json!({ "query": "query { viewer { login } }" })),
        })
        .await
        .unwrap();
    assert_eq!(response, Some(json!({ "ok": true })));
    let request = server.await.unwrap();
    let lower = request.to_ascii_lowercase();
    assert!(lower.starts_with("post /graphql http/1.1\r\n"));
    assert!(lower.contains("accept: application/vnd.github+json\r\n"));
    assert!(lower.contains("authorization: bearer test-token\r\n"));
    assert!(lower.contains("x-github-api-version: 2026-03-10\r\n"));
    assert!(lower.contains("user-agent: seekdeep-issue-policy\r\n"));
    assert!(request.contains("query { viewer { login } }"));
    assert!(!format!("{transport:?}").contains("test-token"));
}

#[test]
fn built_pr_command_runs_the_complete_http_snapshot_and_validation_path() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let mut planned = vec![
        (
            repo_path("/pulls/7"),
            json!({
                "draft": false,
                "body": "Fixes #2",
                "user": { "type": "User" },
                "labels": [
                    { "name": "kind/feature" },
                    { "name": "area/web" },
                    { "name": "p2" }
                ]
            }),
        ),
        (
            repo_path("/pulls/7/requested_reviewers"),
            json!({ "users": [{}], "teams": [] }),
        ),
        (repo_path("/pulls/7/reviews?per_page=100"), json!([])),
        (repo_path("/issues/2"), issue(2, true)),
        (
            repo_path("/issues/2/issue-field-values?per_page=100"),
            fields(Some("P2")),
        ),
    ];
    let server = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut requests = Vec::new();
        while !planned.is_empty() {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    let mut bytes = Vec::new();
                    let mut buffer = [0_u8; 4096];
                    while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                        let read = stream.read(&mut buffer).unwrap();
                        assert_ne!(read, 0);
                        bytes.extend_from_slice(&buffer[..read]);
                    }
                    let request = String::from_utf8(bytes).unwrap();
                    let first = request.lines().next().unwrap().to_owned();
                    let path = first.split_whitespace().nth(1).unwrap();
                    let index = planned
                        .iter()
                        .position(|(candidate, _)| candidate == path)
                        .unwrap_or_else(|| panic!("unexpected request {first}"));
                    let (_, value) = planned.remove(index);
                    let body = serde_json::to_vec(&value).unwrap();
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .unwrap();
                    stream.write_all(&body).unwrap();
                    requests.push(first);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "timed out waiting for {planned:?}"
                    );
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("fixture server failed: {error}"),
            }
        }
        requests
    });
    let scratch = tempfile::tempdir().unwrap();
    let event = scratch.path().join("event.json");
    std::fs::write(&event, r#"{"pull_request":{"number":7}}"#).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_seekdeep-issue-policy"))
        .arg("pr")
        .env("GITHUB_API_URL", format!("http://{address}"))
        .env("GITHUB_TOKEN", "test-token")
        .env_remove("GH_TOKEN")
        .env("GITHUB_EVENT_PATH", event)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "Issue policy 通过。\n"
    );
    assert!(output.stderr.is_empty());
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 5);
    assert!(requests.iter().all(|request| request.starts_with("GET ")));
}
