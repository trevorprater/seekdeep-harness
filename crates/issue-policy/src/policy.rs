//! Deterministic Issue body, metadata, PR reference, and lifecycle policy.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::LazyLock,
};

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::IssuePolicyConfig;

const BODY_LIMIT: usize = 50;
const TYPES: &[&str] = &["Idea", "Feature", "Bug", "Research", "Task"];
const PRIORITIES: &[&str] = &["p0", "p1", "p2", "p3"];
const PR_KINDS: &[&str] = &[
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
const IMPLEMENTATION_PULL_REQUEST_ACTIONS: &[&str] = &[
    "opened",
    "edited",
    "synchronize",
    "reopened",
    "labeled",
    "unlabeled",
];

static COMMENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<!--[\s\S]*?-->").expect("valid comment regex"));
static DETAILS_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)</?details\b[^>]*>").expect("valid details tag regex"));
static DETAILS_OPEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\sopen(?:\s|=|>)").expect("valid details open regex"));
static OWNER_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^Owner: @([A-Za-z0-9](?:[A-Za-z0-9-]{0,37}[A-Za-z0-9])?)$")
        .expect("valid owner regex")
});
static IMAGE_LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"!\[([^\]]*)\]\([^)]*\)").expect("valid image-link regex"));
static INLINE_LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([^\]]+)\]\([^)]*\)").expect("valid inline-link regex"));
static REFERENCE_LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([^\]]+)\]\[[^\]]*\]").expect("valid reference-link regex"));
static AUTO_LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)<((?:https?://|mailto:)[^>]+)>").expect("valid autolink regex")
});
static HTML_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<[^>]+>").expect("valid HTML tag regex"));
static ENTITY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"&(?:[A-Za-z]+|#\d+|#x[0-9A-Fa-f]+);").expect("valid entity regex")
});
static MARKDOWN_PUNCTUATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[`*~\[\]{}()<>#!|]").expect("valid Markdown punctuation regex"));
static HAN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\p{Script=Han}").expect("valid Han regex"));
static VISIBLE_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[\p{Script=Latin}\p{Number}_./:@+\-]+").expect("valid visible token regex")
});
static TITLE_PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?iu)^\s*(?:\[(?:Idea|Feature|Bug|Research|Task|P[0-3]|Inbox|Backlog|Ready|In progress|In review|Done|No action|Owner|area/[^\]]+)[^\]]*\]|(?:Idea|Feature|Bug|Research|Task|P[0-3]|Inbox|Backlog|Ready|In progress|In review|Done|No action|Owner|area/[^:： ]+)\s*[:：-])",
    )
    .expect("valid Issue title-prefix regex")
});
static FENCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*([`~]{3,})").expect("valid fence regex"));
static INLINE_CODE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"`[^`]*`").expect("valid inline-code regex"));
static REFERENCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:([A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+)#|#)(\d+)|https://github\.com/([A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+)/issues/(\d+)",
    )
    .expect("valid Issue reference regex")
});
static CLOSING_REFERENCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(?:close(?:s|d)?|fix(?:es|ed)?|resolve(?:s|d)?)\s*:?\s+(?:(?:([A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+)#|#)(\d+)|https://github\.com/([A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+)/issues/(\d+))",
    )
    .expect("valid closing reference regex")
});

/// Issue number crossing REST, GraphQL, and event boundaries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IssueNumber(u64);

impl IssueNumber {
    /// Construct one Issue number.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Raw numeric value for GitHub paths and GraphQL variables.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for IssueNumber {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Markdown outside details elements plus structural facts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutsideDetails {
    /// Markdown not nested inside details.
    pub text: String,
    /// Whether every opening and closing details tag is paired.
    pub balanced: bool,
    /// Opening details tag count.
    pub details_count: usize,
    /// Whether every details element omits the `open` attribute.
    pub all_collapsed: bool,
}

/// Visible-unit count plus details shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisibleUnits {
    /// Han characters plus contiguous Latin, numeric, or code tokens.
    pub units: usize,
    /// Whether every details tag is paired.
    pub balanced: bool,
    /// Opening details tag count.
    pub details_count: usize,
    /// Whether every details element is collapsed by default.
    pub all_collapsed: bool,
}

/// Body and assignment input shared by Issue validation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BodyInput {
    /// Markdown body.
    pub body: String,
    /// Assigned GitHub logins.
    pub assignees: Vec<String>,
    /// Whether an Owner may be named before assignment permission lands.
    pub allow_unassigned_owner: bool,
}

/// One Issue snapshot with native metadata and Project fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueSnapshot {
    /// Issue number.
    pub number: IssueNumber,
    /// Title.
    pub title: String,
    /// Markdown body.
    pub body: String,
    /// Assigned logins.
    pub assignees: Vec<String>,
    /// Repository label names.
    pub labels: Vec<String>,
    /// Native Issue type.
    pub issue_type: Option<String>,
    /// Project priority.
    pub priority: Option<String>,
    /// Project status.
    pub status: Option<String>,
    /// REST Issue state.
    pub state: String,
    /// REST close reason.
    pub state_reason: Option<String>,
}

/// Parsed same-repository Issue references.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IssueReferences {
    /// Every same-repository reference.
    pub all: Vec<IssueNumber>,
    /// References preceded by a resolving keyword.
    pub resolving: Vec<IssueNumber>,
    /// Non-resolving references.
    pub related: Vec<IssueNumber>,
}

/// Referenced Issue metadata used by PR validation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReferencedIssue {
    /// Project priority.
    pub priority: Option<String>,
}

/// Pull-request snapshot used by the pure metadata validator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestSnapshot {
    /// PR number.
    pub number: IssueNumber,
    /// Whether GitHub marks the PR draft.
    pub is_draft: bool,
    /// REST author type (`User`, `Bot`, or `App`).
    pub author_type: String,
    /// Requested user plus team reviewer count.
    pub review_request_count: usize,
    /// Submitted review count.
    pub review_count: usize,
    /// Repository label names.
    pub labels: Vec<String>,
    /// Parsed and Issue-filtered references.
    pub references: IssueReferences,
    /// Same-repository Issue metadata by number.
    pub issues: BTreeMap<IssueNumber, ReferencedIssue>,
}

/// Event facts used to derive one lifecycle command.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LifecycleEvent {
    /// GitHub webhook action.
    pub action: Option<String>,
    /// Submitted review state.
    pub review_state: Option<String>,
}

/// One resolving-Issue lifecycle instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleCommand {
    /// PR implementation activity.
    Implementation,
    /// Explicit review handoff.
    ReviewRequested,
    /// Reviewer requested changes.
    ChangesRequested,
}

/// Return Markdown outside balanced details elements.
#[must_use]
pub fn extract_outside_details(body: &str) -> OutsideDetails {
    let source = COMMENT.replace_all(body, "");
    let mut depth = 0_usize;
    let mut cursor = 0;
    let mut balanced = true;
    let mut text = String::new();
    let mut details_count = 0;
    let mut all_collapsed = true;
    for matched in DETAILS_TAG.find_iter(&source) {
        if depth == 0 {
            text.push_str(&source[cursor..matched.start()]);
        }
        if matched.as_str()[1..].starts_with('/') {
            if depth == 0 {
                balanced = false;
            } else {
                depth -= 1;
            }
        } else {
            depth += 1;
            details_count += 1;
            if DETAILS_OPEN.is_match(matched.as_str()) {
                all_collapsed = false;
            }
        }
        cursor = matched.end();
    }
    if depth == 0 {
        text.push_str(&source[cursor..]);
    } else {
        balanced = false;
    }
    OutsideDetails {
        text,
        balanced,
        details_count,
        all_collapsed,
    }
}

/// Count visible Han characters and contiguous Latin, numeric, or code tokens.
#[must_use]
pub fn count_visible_units(body: &str) -> VisibleUnits {
    let outside = extract_outside_details(body);
    let visible = IMAGE_LINK.replace_all(&outside.text, "$1");
    let visible = INLINE_LINK.replace_all(&visible, "$1");
    let visible = REFERENCE_LINK.replace_all(&visible, "$1");
    let visible = AUTO_LINK.replace_all(&visible, "$1");
    let visible = HTML_TAG.replace_all(&visible, " ");
    let visible = ENTITY.replace_all(&visible, " ");
    let visible = MARKDOWN_PUNCTUATION.replace_all(&visible, " ");
    VisibleUnits {
        units: HAN.find_iter(&visible).count() + VISIBLE_TOKEN.find_iter(&visible).count(),
        balanced: outside.balanced,
        details_count: outside.details_count,
        all_collapsed: outside.all_collapsed,
    }
}

/// Validate required body structure and Owner assignment.
#[must_use]
pub fn validate_body(input: &BodyInput) -> Vec<String> {
    let mut errors = Vec::new();
    let count = count_visible_units(&input.body);
    let owner = input
        .body
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .and_then(|line| OWNER_LINE.captures(line))
        .and_then(|captures| captures.get(1))
        .map(|owner| owner.as_str().to_owned());
    let normalized = input
        .assignees
        .iter()
        .map(|login| login.to_lowercase())
        .collect::<BTreeSet<_>>();
    if !count.balanced {
        errors.push("details 标签必须成对闭合".to_owned());
    }
    if count.details_count == 0 {
        errors.push("正文必须包含默认收起的 <details> 区域".to_owned());
    }
    if !count.all_collapsed {
        errors.push("details 必须默认收起，不得设置 open".to_owned());
    }
    if count.units > BODY_LIMIT {
        errors.push(format!("正文外露部分为 {} 单位，超过 50 单位", count.units));
    }
    if normalized.len() >= 2 && owner.is_none() {
        errors.push("多个 Assignees 时首个非空行必须是 Owner: @login".to_owned());
    } else if normalized.len() >= 2
        && owner
            .as_ref()
            .is_some_and(|owner| !normalized.contains(&owner.to_lowercase()))
    {
        errors.push("Owner 必须属于 Assignees".to_owned());
    } else if normalized.len() < 2
        && owner.is_some()
        && !(normalized.is_empty() && input.allow_unassigned_owner)
    {
        errors.push("零或一个 Assignee 时不得写 Owner 行".to_owned());
    }
    errors
}

/// Decide whether human-review PR metadata is mandatory.
#[must_use]
pub fn requires_pull_request_policy(pull: &PullRequestSnapshot) -> bool {
    let automated = matches!(pull.author_type.as_str(), "Bot" | "App");
    !pull.is_draft && !automated && (pull.review_request_count > 0 || pull.review_count > 0)
}

/// Translate one repository webhook into a resolving-Issue command.
#[must_use]
pub fn resolving_issue_status_command(
    event_name: &str,
    event: &LifecycleEvent,
) -> Option<LifecycleCommand> {
    if event_name == "pull_request" {
        if event.action.as_deref() == Some("review_requested") {
            return Some(LifecycleCommand::ReviewRequested);
        }
        return event
            .action
            .as_deref()
            .filter(|action| IMPLEMENTATION_PULL_REQUEST_ACTIONS.contains(action))
            .map(|_| LifecycleCommand::Implementation);
    }
    (event_name == "pull_request_review"
        && event.action.as_deref() == Some("submitted")
        && event
            .review_state
            .as_deref()
            .is_some_and(|state| state.eq_ignore_ascii_case("changes_requested")))
    .then_some(LifecycleCommand::ChangesRequested)
}

/// Plan one permitted resolving-Issue status transition.
#[must_use]
pub fn next_resolving_issue_status(
    config: &IssuePolicyConfig,
    current_status: Option<&str>,
    command: LifecycleCommand,
    current_status_actor: Option<&str>,
) -> Option<String> {
    let target = match command {
        LifecycleCommand::ReviewRequested => "In review",
        LifecycleCommand::Implementation | LifecycleCommand::ChangesRequested => "In progress",
    };
    if command == LifecycleCommand::ChangesRequested
        && current_status == Some("In review")
        && current_status_actor == Some(config.lifecycle_actor.as_str())
    {
        return Some(target.to_owned());
    }
    let active = config.active_statuses();
    let current_index = active
        .iter()
        .position(|status| Some(*status) == current_status)?;
    let target_index = active.iter().position(|status| *status == target)?;
    (current_index < target_index).then(|| target.to_owned())
}

/// Parse same-repository resolving and informational references outside ignored Markdown.
#[must_use]
pub fn parse_references(body: &str, repository: &str) -> IssueReferences {
    let source = strip_ignored_markdown(body);
    let expected = repository.to_lowercase();
    let mut all = BTreeSet::new();
    let mut resolving = BTreeSet::new();
    collect_references(&REFERENCE, &source, &expected, &mut all, None);
    collect_references(
        &CLOSING_REFERENCE,
        &source,
        &expected,
        &mut all,
        Some(&mut resolving),
    );
    IssueReferences {
        all: all.iter().copied().collect(),
        resolving: resolving.iter().copied().collect(),
        related: all.difference(&resolving).copied().collect(),
    }
}

/// Retain only references that resolve to Issues rather than pull requests.
#[must_use]
pub fn retain_issue_references(
    references: &IssueReferences,
    issues: &BTreeMap<IssueNumber, ReferencedIssue>,
) -> IssueReferences {
    IssueReferences {
        all: references
            .all
            .iter()
            .copied()
            .filter(|number| issues.contains_key(number))
            .collect(),
        resolving: references
            .resolving
            .iter()
            .copied()
            .filter(|number| issues.contains_key(number))
            .collect(),
        related: references
            .related
            .iter()
            .copied()
            .filter(|number| issues.contains_key(number))
            .collect(),
    }
}

/// Validate one Issue with its Project status.
#[must_use]
pub fn validate_issue(config: &IssuePolicyConfig, issue: &IssueSnapshot) -> Vec<String> {
    let mut errors = validate_body(&BodyInput {
        body: issue.body.clone(),
        assignees: issue.assignees.clone(),
        allow_unassigned_owner: config.allow_unassigned_owner,
    });
    let invalid_labels = issue
        .labels
        .iter()
        .filter(|label| label.starts_with("kind/") || LEGACY_LABELS.contains(&label.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if !HAN.is_match(&issue.title) {
        errors.push("Issue 标题必须包含中文".to_owned());
    }
    if !invalid_labels.is_empty() {
        errors.push(format!(
            "Issue 不得使用 PR kind 或旧版标签：{}",
            invalid_labels.join(", ")
        ));
    }
    if TITLE_PREFIX.is_match(&issue.title) {
        errors.push("Issue 标题不得带 Type、Priority、Status、area 或 Owner 前缀".to_owned());
    }
    if issue
        .issue_type
        .as_deref()
        .is_none_or(|value| !TYPES.contains(&value))
    {
        errors.push("Type 必须是五种原生英文 Type 之一".to_owned());
    }
    if issue
        .status
        .as_deref()
        .is_none_or(|status| !config.statuses.iter().any(|candidate| candidate == status))
    {
        errors.push("Issue 必须在 Project 中且具有合法 Status".to_owned());
    }
    if issue
        .priority
        .as_deref()
        .is_some_and(|priority| !PRIORITIES.contains(&priority.to_lowercase().as_str()))
    {
        errors.push("Priority 必须为空或为 P0–P3".to_owned());
    }
    if issue.status.as_deref() == Some("Done")
        && (issue.state != "closed" || issue.state_reason.as_deref() != Some("completed"))
    {
        errors.push("Done 必须对应 Completed 关闭原因".to_owned());
    }
    if issue.status.as_deref() == Some("No action")
        && (issue.state != "closed" || issue.state_reason.as_deref() != Some("not_planned"))
    {
        errors.push("No action 必须对应 Not planned 关闭原因".to_owned());
    }
    if !matches!(issue.status.as_deref(), Some("Done" | "No action")) && issue.state != "open" {
        errors.push(format!(
            "{} 必须对应开放 Issue",
            issue.status.as_deref().unwrap_or("null")
        ));
    }
    errors
}

/// Validate PR labels, references, and resolving-Issue priority agreement.
#[must_use]
pub fn validate_pull_request(pull: &PullRequestSnapshot) -> Vec<String> {
    if !requires_pull_request_policy(pull) {
        return Vec::new();
    }
    let mut errors = Vec::new();
    let kinds = labels_matching(&pull.labels, |label| PR_KINDS.contains(&label));
    let unknown_kinds = labels_matching(&pull.labels, |label| {
        label.starts_with("kind/") && !PR_KINDS.contains(&label) && !LEGACY_LABELS.contains(&label)
    });
    let legacy_labels = labels_matching(&pull.labels, |label| LEGACY_LABELS.contains(&label));
    let source_labels = labels_matching(&pull.labels, |label| label.starts_with("source/"));
    let priorities = labels_matching(&pull.labels, |label| PRIORITIES.contains(&label));
    let areas = labels_matching(&pull.labels, |label| label.starts_with("area/"));
    if pull.references.all.is_empty() {
        errors.push("PR 正文必须引用至少一个同仓库 Issue".to_owned());
    }
    if kinds.len() != 1 {
        errors.push(format!(
            "PR 必须恰好有一个允许的 kind/*，当前为 {}",
            kinds.len()
        ));
    }
    if !unknown_kinds.is_empty() {
        errors.push(format!(
            "PR 含不支持的 kind/*：{}",
            unknown_kinds.join(", ")
        ));
    }
    if !legacy_labels.is_empty() {
        errors.push(format!("PR 含旧版标签：{}", legacy_labels.join(", ")));
    }
    if !source_labels.is_empty() {
        errors.push(format!(
            "source/* 仅用于 Issue：{}",
            source_labels.join(", ")
        ));
    }
    if priorities.len() > 1 {
        errors.push(format!("PR 最多有一个 p0–p3，当前为 {}", priorities.len()));
    }
    if areas.is_empty() {
        errors.push("PR 必须至少有一个 area/*".to_owned());
    }
    for number in &pull.references.all {
        if !pull.issues.contains_key(number) {
            errors.push(format!("#{number} 不是同仓库 Issue"));
        }
    }
    let resolving = pull
        .references
        .resolving
        .iter()
        .filter_map(|number| pull.issues.get(number).map(|issue| (*number, issue)))
        .collect::<Vec<_>>();
    if resolving.is_empty() {
        return errors;
    }
    let mut issue_priorities = resolving
        .iter()
        .filter_map(|(_, issue)| issue.priority.as_deref())
        .map(str::to_lowercase)
        .filter(|priority| PRIORITIES.contains(&priority.as_str()))
        .collect::<Vec<_>>();
    issue_priorities.sort_by_key(|priority| priority_index(priority));
    let highest = issue_priorities.first();
    if priorities.is_empty()
        && let Some(highest) = highest
    {
        errors.push(format!("PR Priority 应为 {highest}"));
    } else if priorities.len() == 1 && issue_priorities.len() != resolving.len() {
        errors.push("有 Priority 的解决型 PR 要求每个被解决 Issue 都设置 Priority".to_owned());
    } else if priorities.len() == 1
        && let Some(highest) = highest
        && priorities[0] != highest.as_str()
    {
        errors.push(format!("PR Priority 应为 {highest}"));
    }
    errors
}

fn strip_ignored_markdown(body: &str) -> String {
    let source = COMMENT.replace_all(body, "");
    let mut kept = Vec::new();
    let mut fence = None;
    let lines = source.split('\n').collect::<Vec<_>>();
    for (index, raw_line) in lines.iter().enumerate() {
        let raw_line = *raw_line;
        let line = if index + 1 < lines.len() {
            raw_line.strip_suffix('\r').unwrap_or(raw_line)
        } else {
            raw_line
        };
        if let Some(marker) = FENCE.captures(line).and_then(|captures| captures.get(1)) {
            let marker = marker.as_str().chars().next().expect("fence marker");
            if fence.is_none() {
                fence = Some(marker);
            } else if fence == Some(marker) {
                fence = None;
            }
            continue;
        }
        if fence.is_none() {
            kept.push(line);
        }
    }
    INLINE_CODE.replace_all(&kept.join("\n"), " ").into_owned()
}

fn collect_references(
    expression: &Regex,
    source: &str,
    expected: &str,
    all: &mut BTreeSet<IssueNumber>,
    mut resolving: Option<&mut BTreeSet<IssueNumber>>,
) {
    for captures in expression.captures_iter(source) {
        let explicit = captures
            .get(1)
            .or_else(|| captures.get(3))
            .map_or("", |value| value.as_str())
            .to_lowercase();
        let number = captures
            .get(2)
            .or_else(|| captures.get(4))
            .and_then(|value| value.as_str().parse::<u64>().ok())
            .map(IssueNumber::new);
        if (explicit.is_empty() || explicit == expected)
            && let Some(number) = number
        {
            all.insert(number);
            if let Some(resolving) = &mut resolving {
                resolving.insert(number);
            }
        }
    }
}

fn labels_matching(labels: &[String], predicate: impl Fn(&str) -> bool) -> Vec<&str> {
    labels
        .iter()
        .map(String::as_str)
        .filter(|label| predicate(label))
        .collect()
}

fn priority_index(priority: &str) -> usize {
    PRIORITIES
        .iter()
        .position(|candidate| *candidate == priority)
        .unwrap_or(PRIORITIES.len())
}
