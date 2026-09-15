//! GitHub REST/GraphQL snapshots, audit effects, and event lifecycle handling.

use std::collections::BTreeMap;

use anyhow::{Result, bail, ensure};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};

use crate::{
    ApiMethod, ApiRequest, GitHubTransport, IssueNumber, IssuePolicyConfig, IssueReferences,
    IssueSnapshot, LifecycleCommand, LifecycleEvent, PullRequestSnapshot, ReferencedIssue,
    next_resolving_issue_status, parse_references, resolving_issue_status_command,
    retain_issue_references, validate_issue, validate_pull_request,
};

const AUDIT_MARKER: &str = "<!-- seekdeep-issue-policy -->";

const PROJECT_CONTEXT_QUERY: &str = r#"query(
  $organization: String!
  $repository: String!
  $number: Int!
  $project: Int!
  $includeStatusActor: Boolean!
) {
  organization(login: $organization) {
    projectV2(number: $project) {
      id
      title
      fields(first: 50) {
        nodes {
          ... on ProjectV2SingleSelectField { id name options { id name } }
        }
      }
    }
  }
  repository(owner: $organization, name: $repository) {
    issue(number: $number) {
      id
      timelineItems(last: 100, itemTypes: [PROJECT_V2_ITEM_STATUS_CHANGED_EVENT])
        @include(if: $includeStatusActor) {
        nodes {
          ... on ProjectV2ItemStatusChangedEvent {
            actor { login }
            project { id }
            status
          }
        }
      }
      projectItems(first: 20, includeArchived: true) {
        nodes {
          id
          project { id }
          fieldValueByName(name: "Status") {
            ... on ProjectV2ItemFieldSingleSelectValue { name optionId }
          }
        }
      }
    }
  }
}"#;

const ADD_PROJECT_ITEM_MUTATION: &str = r"mutation($projectId: ID!, $contentId: ID!) {
  addProjectV2ItemById(input: {projectId: $projectId, contentId: $contentId}) {
    item { id }
  }
}";

const UPDATE_STATUS_MUTATION: &str = r"mutation($projectId: ID!, $itemId: ID!, $fieldId: ID!, $optionId: String!) {
  updateProjectV2ItemFieldValue(input: {
    projectId: $projectId,
    itemId: $itemId,
    fieldId: $fieldId,
    value: {singleSelectOptionId: $optionId}
  }) { projectV2Item { id } }
}";

macro_rules! graphql_id {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq)]
        struct $name(String);

        impl $name {
            fn parse(value: &Value, owner: &str) -> Result<Self> {
                value
                    .as_str()
                    .map(|value| Self(value.to_owned()))
                    .ok_or_else(|| anyhow::anyhow!("{owner} is missing an id"))
            }

            fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

graphql_id!(ProjectId);
graphql_id!(IssueNodeId);
graphql_id!(ProjectItemId);
graphql_id!(ProjectFieldId);
graphql_id!(ProjectOptionId);

#[derive(Clone, Debug, PartialEq, Eq)]
struct StatusOption {
    id: ProjectOptionId,
    name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProjectItem {
    id: ProjectItemId,
    status: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProjectContext {
    project_id: ProjectId,
    issue_id: IssueNodeId,
    status_field_id: ProjectFieldId,
    status_options: Vec<StatusOption>,
    item: Option<ProjectItem>,
    status_actor: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StatusSource<'a> {
    Project,
    Supplied(Option<&'a str>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ResolvingSnapshot {
    number: IssueNumber,
    references: IssueReferences,
    issues: BTreeMap<IssueNumber, ReferencedIssue>,
}

/// Result of evaluating one pull request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestCheck {
    /// Validation failures in source order.
    pub errors: Vec<String>,
    /// Whether the PR has entered human-review enforcement.
    pub enforced: bool,
}

/// GitHub Issue-policy runtime over an injected transport.
#[derive(Clone, Debug)]
pub struct IssuePolicyRuntime<T> {
    config: IssuePolicyConfig,
    transport: T,
}

impl<T: GitHubTransport> IssuePolicyRuntime<T> {
    /// Construct one runtime.
    #[must_use]
    pub const fn new(config: IssuePolicyConfig, transport: T) -> Self {
        Self { config, transport }
    }

    /// Fetch and validate the PR named by one webhook event.
    ///
    /// # Errors
    ///
    /// Returns malformed event, REST, GraphQL, or response-shape failures.
    pub async fn check_pull_request_event(&self, event: &Value) -> Result<PullRequestCheck> {
        let number = event_issue_number(event, "pull_request")?;
        let pull = self.pull_request_snapshot(number).await?;
        Ok(PullRequestCheck {
            errors: validate_pull_request(&pull),
            enforced: crate::requires_pull_request_policy(&pull),
        })
    }

    /// Apply Issue or resolving-PR lifecycle effects for one webhook event.
    ///
    /// # Errors
    ///
    /// Returns malformed event, REST, GraphQL, validation, or mutation failures.
    pub async fn handle_lifecycle_event(&self, event_name: &str, event: &Value) -> Result<()> {
        if event_name == "issues" {
            let number = event_issue_number(event, "issue")?;
            let action = event
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if action == "opened" {
                self.set_status(number, "Inbox").await?;
            }
            if action == "closed" {
                let target = if event.pointer("/issue/state_reason").and_then(Value::as_str)
                    == Some("not_planned")
                {
                    "No action"
                } else {
                    "Done"
                };
                self.set_status(number, target).await?;
            }
            if action == "reopened" {
                self.set_status(number, "Inbox").await?;
            }
            self.ensure_project_item(number).await?;
            self.audit_issue(number, &[], StatusSource::Project).await?;
            return Ok(());
        }
        if matches!(event_name, "pull_request" | "pull_request_review") {
            let command = resolving_issue_status_command(
                event_name,
                &LifecycleEvent {
                    action: event
                        .get("action")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    review_state: event
                        .pointer("/review/state")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                },
            );
            let Some(command) = command else {
                return Ok(());
            };
            let number = event_issue_number(event, "pull_request")?;
            let pull = self.lifecycle_pull_request_snapshot(number).await?;
            self.transition_resolving_issues(&pull, command).await?;
        }
        Ok(())
    }

    async fn request<TValue: DeserializeOwned>(
        &self,
        method: ApiMethod,
        path: impl Into<String>,
        body: Option<Value>,
    ) -> Result<TValue> {
        let path = path.into();
        let value = self
            .transport
            .request(ApiRequest { method, path, body })
            .await?
            .ok_or_else(|| anyhow::anyhow!("GitHub API returned no JSON body"))?;
        Ok(serde_json::from_value(value)?)
    }

    async fn request_empty(
        &self,
        method: ApiMethod,
        path: impl Into<String>,
        body: Option<Value>,
    ) -> Result<()> {
        self.transport
            .request(ApiRequest {
                method,
                path: path.into(),
                body,
            })
            .await?;
        Ok(())
    }

    async fn graphql(&self, query: &str, variables: Value) -> Result<Value> {
        let result: Value = self
            .request(
                ApiMethod::Post,
                "/graphql",
                Some(json!({ "query": query, "variables": variables })),
            )
            .await?;
        if let Some(errors) = result.get("errors").and_then(Value::as_array)
            && !errors.is_empty()
        {
            bail!(
                "{}",
                errors
                    .iter()
                    .filter_map(|error| error.get("message").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("; ")
            );
        }
        result
            .get("data")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("GraphQL response is missing data"))
    }

    async fn issue_snapshot(
        &self,
        number: IssueNumber,
        status_source: StatusSource<'_>,
    ) -> Result<Option<IssueSnapshot>> {
        let issue: RestIssue = self
            .request(
                ApiMethod::Get,
                format!(
                    "/repos/{}/{}/issues/{number}",
                    self.config.organization, self.config.repository
                ),
                None,
            )
            .await?;
        if issue.pull_request.is_some() {
            return Ok(None);
        }
        let values: Vec<IssueFieldValue> = self
            .request(
                ApiMethod::Get,
                format!(
                    "/repos/{}/{}/issues/{number}/issue-field-values?per_page=100",
                    self.config.organization, self.config.repository
                ),
                None,
            )
            .await?;
        let priority = values
            .iter()
            .find(|value| value.issue_field_name == self.config.priority_field)
            .and_then(|value| value.single_select_option.as_ref())
            .map(|option| option.name.clone());
        let status = match status_source {
            StatusSource::Project => self.project_status(number).await?,
            StatusSource::Supplied(status) => status.map(str::to_owned),
        };
        Ok(Some(IssueSnapshot {
            number,
            title: issue.title,
            body: issue.body.unwrap_or_default(),
            assignees: issue
                .assignees
                .into_iter()
                .map(|assignee| assignee.login)
                .collect(),
            labels: issue.labels.into_iter().map(|label| label.name).collect(),
            issue_type: issue.issue_type.map(|issue_type| issue_type.name),
            priority,
            status,
            state: issue.state,
            state_reason: issue.state_reason,
        }))
    }

    async fn project_context(
        &self,
        number: IssueNumber,
        include_status_actor: bool,
    ) -> Result<ProjectContext> {
        let data = self
            .graphql(
                PROJECT_CONTEXT_QUERY,
                json!({
                    "organization": self.config.organization.as_str(),
                    "repository": self.config.repository.as_str(),
                    "number": number.get(),
                    "project": self.config.project_number,
                    "includeStatusActor": include_status_actor,
                }),
            )
            .await?;
        parse_project_context(&self.config, number, &data)
    }

    async fn project_status(&self, number: IssueNumber) -> Result<Option<String>> {
        Ok(self
            .project_context(number, false)
            .await?
            .item
            .and_then(|item| item.status))
    }

    async fn ensure_project_item(&self, number: IssueNumber) -> Result<ProjectContext> {
        let mut context = self.project_context(number, false).await?;
        if context.item.is_some() {
            return Ok(context);
        }
        let data = self
            .graphql(
                ADD_PROJECT_ITEM_MUTATION,
                json!({
                    "projectId": context.project_id.as_str(),
                    "contentId": context.issue_id.as_str(),
                }),
            )
            .await?;
        let id = ProjectItemId::parse(
            data.pointer("/addProjectV2ItemById/item/id")
                .unwrap_or(&Value::Null),
            "added Project item",
        )?;
        context.item = Some(ProjectItem { id, status: None });
        Ok(context)
    }

    async fn update_status(&self, context: &ProjectContext, status: &str) -> Result<()> {
        let option = context
            .status_options
            .iter()
            .find(|option| option.name == status)
            .ok_or_else(|| anyhow::anyhow!("Status 不存在：{status}"))?;
        let item = context
            .item
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Issue is not in the target Project"))?;
        if item.status.as_deref() == Some(status) {
            return Ok(());
        }
        self.graphql(
            UPDATE_STATUS_MUTATION,
            json!({
                "projectId": context.project_id.as_str(),
                "itemId": item.id.as_str(),
                "fieldId": context.status_field_id.as_str(),
                "optionId": option.id.as_str(),
            }),
        )
        .await?;
        Ok(())
    }

    async fn set_status(&self, number: IssueNumber, status: &str) -> Result<()> {
        let context = self.ensure_project_item(number).await?;
        self.update_status(&context, status).await
    }

    async fn upsert_audit(&self, number: IssueNumber, errors: &[String]) -> Result<()> {
        let comments: Vec<RestComment> = self
            .request(
                ApiMethod::Get,
                format!(
                    "/repos/{}/{}/issues/{number}/comments?per_page=100",
                    self.config.organization, self.config.repository
                ),
                None,
            )
            .await?;
        let existing = comments.iter().find(|comment| {
            comment.user.as_ref().and_then(|user| user.kind.as_deref()) == Some("Bot")
                && comment
                    .body
                    .as_deref()
                    .is_some_and(|body| body.contains(AUDIT_MARKER))
        });
        if errors.is_empty() {
            if let Some(existing) = existing {
                self.request_empty(
                    ApiMethod::Delete,
                    format!(
                        "/repos/{}/{}/issues/comments/{}",
                        self.config.organization, self.config.repository, existing.id
                    ),
                    None,
                )
                .await?;
            }
            return Ok(());
        }
        let body = format!(
            "{AUDIT_MARKER}\n⚠️ Issue policy 未通过：\n\n{}",
            errors
                .iter()
                .map(|error| format!("- {error}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        if let Some(existing) = existing {
            if existing.body.as_deref() == Some(body.as_str()) {
                return Ok(());
            }
            self.request_empty(
                ApiMethod::Patch,
                format!(
                    "/repos/{}/{}/issues/comments/{}",
                    self.config.organization, self.config.repository, existing.id
                ),
                Some(json!({ "body": body })),
            )
            .await?;
        } else {
            self.request_empty(
                ApiMethod::Post,
                format!(
                    "/repos/{}/{}/issues/{number}/comments",
                    self.config.organization, self.config.repository
                ),
                Some(json!({ "body": body })),
            )
            .await?;
        }
        Ok(())
    }

    async fn audit_issue(
        &self,
        number: IssueNumber,
        extra_errors: &[String],
        status_source: StatusSource<'_>,
    ) -> Result<Vec<String>> {
        let Some(issue) = self.issue_snapshot(number, status_source).await? else {
            return Ok(Vec::new());
        };
        let mut errors = extra_errors.to_vec();
        errors.extend(validate_issue(&self.config, &issue));
        self.upsert_audit(number, &errors).await?;
        Ok(errors)
    }

    async fn resolving_references_snapshot(
        &self,
        number: IssueNumber,
        pull: &RestPull,
    ) -> Result<ResolvingSnapshot> {
        let parsed = parse_references(
            pull.body.as_deref().unwrap_or_default(),
            &format!("{}/{}", self.config.organization, self.config.repository),
        );
        let mut issues = BTreeMap::new();
        for issue_number in &parsed.all {
            if let Some(issue) = self
                .issue_snapshot(*issue_number, StatusSource::Supplied(None))
                .await?
            {
                issues.insert(
                    *issue_number,
                    ReferencedIssue {
                        priority: issue.priority,
                    },
                );
            }
        }
        Ok(ResolvingSnapshot {
            number,
            references: retain_issue_references(&parsed, &issues),
            issues,
        })
    }

    async fn pull_request_snapshot(&self, number: IssueNumber) -> Result<PullRequestSnapshot> {
        let pull_path = format!(
            "/repos/{}/{}/pulls/{number}",
            self.config.organization, self.config.repository
        );
        let review_path = format!("{pull_path}/requested_reviewers");
        let reviews_path = format!("{pull_path}/reviews?per_page=100");
        let (pull, review_requests, reviews): (RestPull, ReviewRequests, Vec<Value>) = tokio::try_join!(
            self.request(ApiMethod::Get, pull_path, None),
            self.request(ApiMethod::Get, review_path, None),
            self.request(ApiMethod::Get, reviews_path, None),
        )?;
        let resolving = self.resolving_references_snapshot(number, &pull).await?;
        Ok(PullRequestSnapshot {
            number: resolving.number,
            is_draft: pull.draft,
            author_type: pull
                .user
                .and_then(|user| user.kind)
                .unwrap_or_else(|| "User".to_owned()),
            review_request_count: review_requests.users.len() + review_requests.teams.len(),
            review_count: reviews.len(),
            labels: pull.labels.into_iter().map(|label| label.name).collect(),
            references: resolving.references,
            issues: resolving.issues,
        })
    }

    async fn lifecycle_pull_request_snapshot(
        &self,
        number: IssueNumber,
    ) -> Result<ResolvingSnapshot> {
        let pull: RestPull = self
            .request(
                ApiMethod::Get,
                format!(
                    "/repos/{}/{}/pulls/{number}",
                    self.config.organization, self.config.repository
                ),
                None,
            )
            .await?;
        self.resolving_references_snapshot(number, &pull).await
    }

    async fn transition_resolving_issues(
        &self,
        pull: &ResolvingSnapshot,
        command: LifecycleCommand,
    ) -> Result<()> {
        for number in &pull.references.resolving {
            let context = self
                .project_context(*number, command == LifecycleCommand::ChangesRequested)
                .await?;
            let target = next_resolving_issue_status(
                &self.config,
                context
                    .item
                    .as_ref()
                    .and_then(|item| item.status.as_deref()),
                command,
                context.status_actor.as_deref(),
            );
            let Some(target) = target else {
                continue;
            };
            self.update_status(&context, &target).await?;
            self.audit_issue(*number, &[], StatusSource::Project)
                .await?;
        }
        Ok(())
    }
}

fn parse_project_context(
    config: &IssuePolicyConfig,
    number: IssueNumber,
    data: &Value,
) -> Result<ProjectContext> {
    let project = data
        .pointer("/organization/projectV2")
        .filter(|value| value.is_object())
        .ok_or_else(|| anyhow::anyhow!("目标 Project 不存在或标题不匹配"))?;
    ensure!(
        project.get("title").and_then(Value::as_str) == Some(config.project_title.as_str()),
        "目标 Project 不存在或标题不匹配"
    );
    let issue = data
        .pointer("/repository/issue")
        .filter(|value| value.is_object())
        .ok_or_else(|| anyhow::anyhow!("#{number} 不存在"))?;
    let project_id = ProjectId::parse(project.get("id").unwrap_or(&Value::Null), "Project")?;
    let issue_id = IssueNodeId::parse(issue.get("id").unwrap_or(&Value::Null), "Issue")?;
    let fields = project
        .pointer("/fields/nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("Project 缺少 Status 字段"))?;
    let status_field = fields
        .iter()
        .find(|field| field.get("name").and_then(Value::as_str) == Some("Status"))
        .ok_or_else(|| anyhow::anyhow!("Project 缺少 Status 字段"))?;
    let status_field_id = ProjectFieldId::parse(
        status_field.get("id").unwrap_or(&Value::Null),
        "Status field",
    )?;
    let status_options = status_field
        .get("options")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("Status field is missing options"))?
        .iter()
        .map(|option| {
            Ok(StatusOption {
                id: ProjectOptionId::parse(
                    option.get("id").unwrap_or(&Value::Null),
                    "Status option",
                )?,
                name: option
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("Status option is missing a name"))?
                    .to_owned(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let items = issue
        .pointer("/projectItems/nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("Issue is missing Project items"))?;
    let item_value = items.iter().find(|item| {
        item.pointer("/project/id").and_then(Value::as_str) == Some(project_id.as_str())
    });
    let item = item_value
        .map(|item| -> Result<ProjectItem> {
            Ok(ProjectItem {
                id: ProjectItemId::parse(item.get("id").unwrap_or(&Value::Null), "Project item")?,
                status: item
                    .pointer("/fieldValueByName/name")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            })
        })
        .transpose()?;
    let latest_status_event = issue
        .pointer("/timelineItems/nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .rfind(|event| {
            event.pointer("/project/id").and_then(Value::as_str) == Some(project_id.as_str())
        });
    let status_actor = latest_status_event
        .filter(|event| {
            event.get("status").and_then(Value::as_str)
                == item.as_ref().and_then(|item| item.status.as_deref())
        })
        .and_then(|event| event.pointer("/actor/login"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok(ProjectContext {
        project_id,
        issue_id,
        status_field_id,
        status_options,
        item,
        status_actor,
    })
}

fn event_issue_number(event: &Value, owner: &str) -> Result<IssueNumber> {
    event
        .pointer(&format!("/{owner}/number"))
        .and_then(Value::as_u64)
        .map(IssueNumber::new)
        .ok_or_else(|| anyhow::anyhow!("event is missing {owner}.number"))
}

#[derive(Debug, Deserialize)]
struct NamedValue {
    name: String,
}

#[derive(Debug, Deserialize)]
struct Login {
    login: String,
}

#[derive(Debug, Deserialize)]
struct RestUser {
    #[serde(rename = "type")]
    kind: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RestIssue {
    title: String,
    body: Option<String>,
    assignees: Vec<Login>,
    labels: Vec<NamedValue>,
    #[serde(rename = "type")]
    issue_type: Option<NamedValue>,
    state: String,
    state_reason: Option<String>,
    pull_request: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct SingleSelectOption {
    name: String,
}

#[derive(Debug, Deserialize)]
struct IssueFieldValue {
    issue_field_name: String,
    single_select_option: Option<SingleSelectOption>,
}

#[derive(Debug, Deserialize)]
struct RestPull {
    draft: bool,
    body: Option<String>,
    user: Option<RestUser>,
    labels: Vec<NamedValue>,
}

#[derive(Debug, Deserialize)]
struct ReviewRequests {
    users: Vec<Value>,
    teams: Vec<Value>,
}

#[derive(Debug, Deserialize)]
struct RestCommentUser {
    #[serde(rename = "type")]
    kind: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RestComment {
    id: u64,
    body: Option<String>,
    user: Option<RestCommentUser>,
}
