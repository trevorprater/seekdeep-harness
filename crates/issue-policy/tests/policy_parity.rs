//! Pinned Issue and pull-request policy semantics.

use std::collections::BTreeMap;

use seekdeep_issue_policy::*;

const CANONICAL_KINDS: &[&str] = &[
    "kind/feature",
    "kind/bug-fix",
    "kind/doc",
    "kind/testing",
    "kind/cleanup",
    "kind/dependency",
];
const LEGACY_LABELS: &[&str] = &[
    "kind/bug",
    "kind/documentation",
    "feature",
    "bug-fix",
    "doc",
    "cleanup",
    "testing",
    "dependencies",
    "ci",
    "cli",
    "llm",
    "web-search",
];

fn config() -> IssuePolicyConfig {
    IssuePolicyConfig::bundled().unwrap()
}

fn with_details(summary: &str) -> String {
    format!("{summary}\n\n<details><summary>验收与细节</summary>待补充。</details>")
}

fn legal_issue() -> IssueSnapshot {
    IssueSnapshot {
        number: IssueNumber::new(2),
        title: "完成议题管理校验".to_owned(),
        body: with_details("完成议题管理校验。"),
        assignees: Vec::new(),
        labels: Vec::new(),
        issue_type: Some("Idea".to_owned()),
        priority: None,
        status: Some("In review".to_owned()),
        state: "open".to_owned(),
        state_reason: None,
    }
}

fn references(all: &[u64], resolving: &[u64], related: &[u64]) -> IssueReferences {
    IssueReferences {
        all: all.iter().copied().map(IssueNumber::new).collect(),
        resolving: resolving.iter().copied().map(IssueNumber::new).collect(),
        related: related.iter().copied().map(IssueNumber::new).collect(),
    }
}

fn issues(values: &[(u64, Option<&str>)]) -> BTreeMap<IssueNumber, ReferencedIssue> {
    values
        .iter()
        .map(|(number, priority)| {
            (
                IssueNumber::new(*number),
                ReferencedIssue {
                    priority: priority.map(str::to_owned),
                },
            )
        })
        .collect()
}

fn reviewed_pull(labels: &[&str]) -> PullRequestSnapshot {
    PullRequestSnapshot {
        number: IssueNumber::new(1),
        is_draft: false,
        author_type: "User".to_owned(),
        review_request_count: 1,
        review_count: 0,
        labels: labels.iter().map(|label| (*label).to_owned()).collect(),
        references: references(&[2], &[], &[2]),
        issues: issues(&[(2, None)]),
    }
}

#[test]
fn counts_only_text_outside_details() {
    assert_eq!(
        count_visible_units("支持 GitHub Project。<details>隐藏文字</details>"),
        VisibleUnits {
            units: 4,
            balanced: true,
            details_count: 1,
            all_collapsed: true,
        }
    );
}

#[test]
fn requires_a_balanced_default_collapsed_details_region() {
    assert_eq!(
        validate_body(&BodyInput {
            body: "完成工作。".to_owned(),
            assignees: Vec::new(),
            allow_unassigned_owner: true,
        }),
        ["正文必须包含默认收起的 <details> 区域"]
    );
    assert_eq!(
        validate_body(&BodyInput {
            body: "完成工作。\n\n<details open><summary>细节</summary>待补充。</details>"
                .to_owned(),
            assignees: Vec::new(),
            allow_unassigned_owner: true,
        }),
        ["details 必须默认收起，不得设置 open"]
    );
    assert_eq!(
        validate_body(&BodyInput {
            body: "完成工作。\n\n<details><summary>细节</summary>".to_owned(),
            assignees: Vec::new(),
            allow_unassigned_owner: true,
        }),
        ["details 标签必须成对闭合"]
    );
}

#[test]
fn requires_owner_for_multiple_assignees() {
    assert_eq!(
        validate_body(&BodyInput {
            body: with_details("完成工作。"),
            assignees: vec!["tianyicui".to_owned(), "tianyicui-bot".to_owned()],
            allow_unassigned_owner: true,
        }),
        ["多个 Assignees 时首个非空行必须是 Owner: @login"]
    );
}

#[test]
fn accepts_an_intended_owner_while_assignment_permission_is_pending() {
    assert!(
        validate_body(&BodyInput {
            body: with_details("Owner: @octocat\n\n完成工作。"),
            assignees: Vec::new(),
            allow_unassigned_owner: true,
        })
        .is_empty()
    );
    assert_eq!(
        validate_body(&BodyInput {
            body: with_details("Owner: @octocat\n\n完成工作。"),
            assignees: vec!["hubot".to_owned()],
            allow_unassigned_owner: true,
        }),
        ["零或一个 Assignee 时不得写 Owner 行"]
    );
}

#[test]
fn allows_optional_metadata_in_every_open_status() {
    let config = config();
    assert!(validate_issue(&config, &legal_issue()).is_empty());
    for status in ["Inbox", "Backlog", "Ready", "In progress", "In review"] {
        let mut issue = legal_issue();
        issue.status = Some(status.to_owned());
        assert!(validate_issue(&config, &issue).is_empty(), "{status}");
    }
}

#[test]
fn rejects_metadata_prefixes_in_an_issue_title() {
    let mut issue = legal_issue();
    issue.title = "[Bug] 修复恢复错误".to_owned();
    assert!(
        validate_issue(&config(), &issue)
            .contains(&"Issue 标题不得带 Type、Priority、Status、area 或 Owner 前缀".to_owned())
    );
}

#[test]
fn reserves_pr_kind_and_legacy_labels_for_pull_requests() {
    for label in CANONICAL_KINDS
        .iter()
        .copied()
        .chain(["kind/experimental"])
        .chain(LEGACY_LABELS.iter().copied())
    {
        let mut issue = legal_issue();
        issue.labels = vec![label.to_owned()];
        assert!(
            validate_issue(&config(), &issue)
                .iter()
                .any(|error| error.starts_with("Issue 不得使用 PR kind 或旧版标签：")),
            "{label}"
        );
    }
    let mut issue = legal_issue();
    issue.labels = vec!["area/web".to_owned(), "source/member".to_owned()];
    assert!(validate_issue(&config(), &issue).is_empty());
}

#[test]
fn keeps_terminal_status_aligned_with_the_native_close_reason() {
    let mut done = legal_issue();
    done.status = Some("Done".to_owned());
    done.state = "closed".to_owned();
    done.state_reason = Some("completed".to_owned());
    assert!(validate_issue(&config(), &done).is_empty());
    let mut no_action = legal_issue();
    no_action.status = Some("No action".to_owned());
    no_action.state = "closed".to_owned();
    no_action.state_reason = Some("not_planned".to_owned());
    assert!(validate_issue(&config(), &no_action).is_empty());
    let mut invalid = legal_issue();
    invalid.status = Some("Done".to_owned());
    assert!(
        validate_issue(&config(), &invalid)
            .contains(&"Done 必须对应 Completed 关闭原因".to_owned())
    );
}

#[test]
fn separates_resolving_and_informational_references() {
    assert_eq!(
        parse_references(
            "Fixes #12\nRelated to #4\nRefs deepseekharness/dsh-test#7",
            "deepseekharness/dsh-test"
        ),
        references(&[4, 7, 12], &[12], &[4, 7])
    );
}

#[test]
fn ignores_references_inside_comments_fences_and_inline_code() {
    assert_eq!(
        parse_references(
            "Related #1\n<!-- Fixes #2 -->\n```txt\nFixes #3\n```\n`Fixes #4`\nFixes #5",
            "deepseekharness/dsh-test"
        ),
        references(&[1, 5], &[5], &[1])
    );
    assert_eq!(
        parse_references("Related #6\r", "deepseekharness/dsh-test"),
        references(&[6], &[], &[6])
    );
}

#[test]
fn does_not_treat_pull_request_references_as_issue_associations() {
    let parsed = references(&[123, 1180, 1181], &[123, 1180], &[1181]);
    assert_eq!(
        retain_issue_references(&parsed, &issues(&[(1180, None), (1181, None)])),
        references(&[1180, 1181], &[1180], &[1181])
    );
}

#[test]
fn allows_informational_references_without_cross_object_constraints() {
    let pull = PullRequestSnapshot {
        labels: vec!["kind/cleanup".to_owned(), "area/infra".to_owned()],
        references: references(&[4], &[], &[4]),
        issues: issues(&[(4, None)]),
        ..reviewed_pull(&[])
    };
    assert!(validate_pull_request(&pull).is_empty());
}

#[test]
fn enforces_highest_resolving_priority_without_type_or_area_synchronization() {
    let pull = PullRequestSnapshot {
        review_request_count: 0,
        review_count: 1,
        labels: vec![
            "kind/cleanup".to_owned(),
            "p0".to_owned(),
            "area/web".to_owned(),
        ],
        references: references(&[2, 3], &[2, 3], &[]),
        issues: issues(&[(2, Some("P2")), (3, Some("P0"))]),
        ..reviewed_pull(&[])
    };
    assert!(validate_pull_request(&pull).is_empty());
    let mut wrong = pull;
    wrong.labels = vec![
        "kind/cleanup".to_owned(),
        "p2".to_owned(),
        "area/web".to_owned(),
    ];
    assert!(validate_pull_request(&wrong).contains(&"PR Priority 应为 p0".to_owned()));
}

#[test]
fn requires_policy_only_after_a_human_pr_enters_review() {
    let mut pull = reviewed_pull(&[]);
    assert!(requires_pull_request_policy(&pull));
    pull.review_request_count = 0;
    assert!(!requires_pull_request_policy(&pull));
}

#[test]
fn maps_only_explicit_review_handoffs_to_review_status_commands() {
    assert_eq!(
        resolving_issue_status_command(
            "pull_request",
            &LifecycleEvent {
                action: Some("review_requested".to_owned()),
                review_state: None,
            }
        ),
        Some(LifecycleCommand::ReviewRequested)
    );
    assert_eq!(
        resolving_issue_status_command(
            "pull_request_review",
            &LifecycleEvent {
                action: Some("submitted".to_owned()),
                review_state: Some("changes_requested".to_owned()),
            }
        ),
        Some(LifecycleCommand::ChangesRequested)
    );
    for state in ["approved", "commented"] {
        assert_eq!(
            resolving_issue_status_command(
                "pull_request_review",
                &LifecycleEvent {
                    action: Some("submitted".to_owned()),
                    review_state: Some(state.to_owned()),
                }
            ),
            None
        );
    }
}

#[test]
fn keeps_ordinary_pull_request_events_as_forward_only_implementation_signals() {
    for action in [
        "opened",
        "edited",
        "synchronize",
        "reopened",
        "labeled",
        "unlabeled",
    ] {
        assert_eq!(
            resolving_issue_status_command(
                "pull_request",
                &LifecycleEvent {
                    action: Some(action.to_owned()),
                    review_state: None,
                }
            ),
            Some(LifecycleCommand::Implementation),
            "{action}"
        );
    }
}

#[test]
fn toggles_automation_owned_work_on_request_changes_and_repeated_review_request() {
    let config = config();
    for status in ["Inbox", "Backlog", "Ready"] {
        assert_eq!(
            next_resolving_issue_status(
                &config,
                Some(status),
                LifecycleCommand::Implementation,
                None
            )
            .as_deref(),
            Some("In progress")
        );
        assert_eq!(
            next_resolving_issue_status(
                &config,
                Some(status),
                LifecycleCommand::ReviewRequested,
                None
            )
            .as_deref(),
            Some("In review")
        );
    }
    let status = next_resolving_issue_status(
        &config,
        Some("In review"),
        LifecycleCommand::ChangesRequested,
        Some("seekdeep-issue-management"),
    );
    assert_eq!(status.as_deref(), Some("In progress"));
    assert_eq!(
        next_resolving_issue_status(
            &config,
            status.as_deref(),
            LifecycleCommand::ReviewRequested,
            None
        )
        .as_deref(),
        Some("In review")
    );
}

#[test]
fn preserves_human_review_status_and_terminal_issues() {
    let config = config();
    for (status, command, actor) in [
        ("In progress", LifecycleCommand::Implementation, None),
        ("In review", LifecycleCommand::Implementation, None),
        ("In review", LifecycleCommand::ReviewRequested, None),
        (
            "In review",
            LifecycleCommand::ChangesRequested,
            Some("tianyicui"),
        ),
        ("In review", LifecycleCommand::ChangesRequested, None),
        ("Done", LifecycleCommand::ReviewRequested, None),
        ("No action", LifecycleCommand::ChangesRequested, None),
    ] {
        assert_eq!(
            next_resolving_issue_status(&config, Some(status), command, actor),
            None
        );
    }
    assert_eq!(
        next_resolving_issue_status(&config, None, LifecycleCommand::ReviewRequested, None),
        None
    );
}

#[test]
fn keeps_lifecycle_projection_independent_of_pr_metadata_enforcement() {
    let pull = PullRequestSnapshot {
        labels: Vec::new(),
        references: references(&[2], &[2], &[]),
        issues: issues(&[(2, None)]),
        ..reviewed_pull(&[])
    };
    assert!(!validate_pull_request(&pull).is_empty());
    assert_eq!(
        next_resolving_issue_status(
            &config(),
            Some("Inbox"),
            LifecycleCommand::ReviewRequested,
            None
        )
        .as_deref(),
        Some("In review")
    );
}

#[test]
fn exempts_draft_bot_and_app_prs() {
    let mut pull = PullRequestSnapshot {
        labels: Vec::new(),
        references: IssueReferences::default(),
        issues: BTreeMap::new(),
        ..reviewed_pull(&[])
    };
    pull.author_type = "Bot".to_owned();
    assert!(validate_pull_request(&pull).is_empty());
    pull.author_type = "App".to_owned();
    assert!(validate_pull_request(&pull).is_empty());
    pull.author_type = "User".to_owned();
    pull.is_draft = true;
    assert!(validate_pull_request(&pull).is_empty());
    pull.is_draft = false;
    assert!(!validate_pull_request(&pull).is_empty());
}

#[test]
fn requires_repository_pr_labels_in_the_enforcement_scope() {
    let pull = PullRequestSnapshot {
        labels: Vec::new(),
        ..reviewed_pull(&[])
    };
    let errors = validate_pull_request(&pull);
    assert!(errors.contains(&"PR 必须恰好有一个允许的 kind/*，当前为 0".to_owned()));
    assert!(errors.contains(&"PR 必须至少有一个 area/*".to_owned()));
}

#[test]
fn accepts_exactly_the_canonical_kinds_with_extensible_areas() {
    for kind in CANONICAL_KINDS {
        assert!(
            validate_pull_request(&reviewed_pull(&[kind, "area/future-domain"])).is_empty(),
            "{kind}"
        );
    }
}

#[test]
fn rejects_multiple_unknown_legacy_and_issue_source_pr_labels() {
    assert!(
        validate_pull_request(&reviewed_pull(&["kind/feature", "kind/doc", "area/web"]))
            .contains(&"PR 必须恰好有一个允许的 kind/*，当前为 2".to_owned())
    );
    assert!(
        validate_pull_request(&reviewed_pull(&["kind/experimental", "area/web"]))
            .contains(&"PR 含不支持的 kind/*：kind/experimental".to_owned())
    );
    for label in LEGACY_LABELS {
        assert!(
            validate_pull_request(&reviewed_pull(&["kind/feature", "area/web", label]))
                .iter()
                .any(|error| error.starts_with("PR 含旧版标签：")),
            "{label}"
        );
    }
    assert!(
        validate_pull_request(&reviewed_pull(&[
            "kind/feature",
            "area/web",
            "source/internal-pr"
        ]))
        .contains(&"source/* 仅用于 Issue：source/internal-pr".to_owned())
    );
}

#[test]
fn allows_missing_priority_only_when_resolving_issues_are_also_unprioritized() {
    let pull = PullRequestSnapshot {
        labels: vec!["kind/feature".to_owned(), "area/web".to_owned()],
        references: references(&[2], &[2], &[]),
        issues: issues(&[(2, None)]),
        ..reviewed_pull(&[])
    };
    assert!(validate_pull_request(&pull).is_empty());
    let mut issue_priority = pull.clone();
    issue_priority.issues = issues(&[(2, Some("P2"))]);
    assert!(validate_pull_request(&issue_priority).contains(&"PR Priority 应为 p2".to_owned()));
    let mut partial = pull;
    partial.labels.push("p2".to_owned());
    assert!(
        validate_pull_request(&partial)
            .contains(&"有 Priority 的解决型 PR 要求每个被解决 Issue 都设置 Priority".to_owned())
    );
}
