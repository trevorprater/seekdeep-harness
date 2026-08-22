//! Model-facing whole-list `todo_write` tool and `todos` session projection.

use std::sync::Arc;

use seekdeep_cordis::{
    Context, DispatchMode, EventArgs, EventOptions, EventReply, Plugin, fiber::EffectHandle,
};
use seekdeep_core::{
    session::{AppendOptions, SessionEvent},
    session_store::SESSIONS,
};
use seekdeep_invariants::{
    InvariantFailure, InvariantInstaller, InvariantRegistration, InvariantRegistry,
};
use seekdeep_llm::ContentBlock;
use seekdeep_schemastery::Schema;
use seekdeep_session_projection::{
    ProjectionDefinition, ProjectionTransition, SESSION_PROJECTIONS, SessionProjectionRegistry,
};
use seekdeep_tools::{
    DefineToolOptions, DefineToolOutput, GenericCallView, TOOLS, ToolCallKind, ToolCallView,
    ToolRunContext, ToolRuntime, define_tool,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Stable public tool name.
pub const TOOL_NAME: &str = "todo_write";
/// Cordis plugin name used by Loader diagnostics.
pub const NAME: &str = "tool-todo";
/// Required runtime services for the plugin body.
pub const INJECT: &[&str] = &["tools"];

const DESCRIPTION_HEAD: &str = "Record and update a structured task list for the current work. Send the ENTIRE list every call — it REPLACES the previous list (there are no partial updates, no per-item edits). Use it to plan multi-step work and show progress: add one todo per concrete step before you start. ";
const DESCRIPTION_PARALLEL: &str = "Mark every todo being actively worked on `in_progress` — several at once when work genuinely runs in parallel (e.g. concurrent subagents or background commands), one for sequential work; while work remains, at least one task should be `in_progress`. ";
const DESCRIPTION_SINGLE: &str = "Keep AT MOST ONE todo `in_progress` at a time; while work remains, exactly one active task should be `in_progress`. ";
const DESCRIPTION_TAIL: &str = "Mark a todo `completed` the moment it is done (do not batch completions), and allow no `in_progress` item only once all work is complete. Skip the list for trivial single-step tasks. Statuses: `pending` (not started), `in_progress` (being worked on now), `completed` (finished).";

/// Model-facing todo tool configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    /// Whether several todos may be `in_progress` at once.
    pub allow_parallel_in_progress: bool,
}

/// Source-compatible Loader schema for the required deployment policy.
#[must_use]
pub fn config_schema() -> Schema {
    Schema::object([("allowParallelInProgress", Schema::boolean().required())])
}

/// Loader-facing namespace-style Cordis plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, value| {
        Box::pin(async move {
            let config: Config = serde_json::from_value(value)?;
            let _ = apply(&context, config)?;
            Ok(())
        })
    })
    .with_config_validator(|value| {
        config_schema()
            .resolve(value)
            .map_err(|error| anyhow::anyhow!("{error}"))
    })
}

/// The valid todo statuses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    /// Not started.
    Pending,
    /// Being worked on now.
    InProgress,
    /// Finished.
    Completed,
}

/// One canonical todo item.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    /// Short imperative task line.
    pub content: String,
    /// Current lifecycle state.
    pub status: TodoStatus,
}

/// Model-supplied list shape, schema-checked at the registry boundary.
#[derive(Clone, Debug, Deserialize)]
struct ToolArgs {
    todos: Vec<TodoItemRaw>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TodoItemRaw {
    content: String,
    status: String,
}

/// Canonical successful result.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolOutput {
    todos: Vec<TodoItem>,
    counts: Counts,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Counts {
    pending: usize,
    in_progress: usize,
    completed: usize,
}

/// The active-status clause is the only description part the policy changes.
#[must_use]
fn describe(allow_parallel: bool) -> String {
    format!(
        "{}{}{}",
        DESCRIPTION_HEAD,
        if allow_parallel {
            DESCRIPTION_PARALLEL
        } else {
            DESCRIPTION_SINGLE
        },
        DESCRIPTION_TAIL
    )
}

/// Validates the constraints the schema cannot express and builds the canonical
/// list: trimmed non-empty unique content, and at most one `in_progress` item
/// unless parallel work is allowed.
fn to_todo_list(raw: &[TodoItemRaw], allow_parallel: bool) -> anyhow::Result<Vec<TodoItem>> {
    let mut todos = Vec::with_capacity(raw.len());
    let mut seen = std::collections::HashSet::new();
    let mut active = 0usize;
    for item in raw {
        let content = item.content.trim();
        anyhow::ensure!(
            !content.is_empty(),
            "invalid todo: `content` must be a non-empty string"
        );
        anyhow::ensure!(
            seen.insert(content.to_owned()),
            "invalid todos: duplicate content {}",
            serde_json::to_string(content)?
        );
        let status = match item.status.as_str() {
            "pending" => TodoStatus::Pending,
            "in_progress" => TodoStatus::InProgress,
            "completed" => TodoStatus::Completed,
            other => anyhow::bail!("invalid todo status: {other}"),
        };
        if status == TodoStatus::InProgress {
            active += 1;
        }
        todos.push(TodoItem {
            content: content.to_owned(),
            status,
        });
    }
    anyhow::ensure!(
        allow_parallel || active <= 1,
        "invalid todos: at most one task may be in_progress (got {active})"
    );
    Ok(todos)
}

fn parameter_schema() -> Value {
    json!({
        "todos": {
            "type": "array",
            "required": true,
            "description": "The COMPLETE task list, replacing any previous list.",
            "items": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "content": {
                        "type": "string", "required": true,
                        "description": "What the task is — a short imperative line."
                    },
                    "status": {
                        "type": "string", "required": true,
                        "enum": ["pending", "in_progress", "completed"],
                        "description": "pending (not started) | in_progress (now) | completed (done)."
                    }
                }
            }
        }
    })
}

fn output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "todos": {
                "type": "array", "required": true,
                "items": {
                    "type": "object", "additionalProperties": false,
                    "properties": {
                        "content": {"type": "string", "required": true},
                        "status": {"type": "string", "required": true, "enum": ["pending", "in_progress", "completed"]}
                    }
                }
            },
            "counts": {
                "type": "object", "additionalProperties": false, "required": true,
                "properties": {
                    "pending": {"type": "integer", "required": true},
                    "inProgress": {"type": "integer", "required": true},
                    "completed": {"type": "integer", "required": true}
                }
            }
        }
    })
}

/// Builds the exact model-facing tool definition.
///
/// # Errors
///
/// Returns only author-schema compilation failures.
pub fn definition(config: Config) -> anyhow::Result<seekdeep_tools::ToolDefinition> {
    let allow_parallel = config.allow_parallel_in_progress;
    let output = DefineToolOutput::new(
        output_schema(),
        Arc::new(|_: &ToolArgs, value: &ToolOutput| {
            Ok(vec![ContentBlock::Text {
                text: format!(
                    "Updated todo list: {} pending, {} in progress, {} completed.",
                    value.counts.pending, value.counts.in_progress, value.counts.completed
                ),
            }])
        }),
    );
    let mut options = DefineToolOptions::new(
        TOOL_NAME,
        describe(allow_parallel),
        parameter_schema(),
        output,
        Arc::new(move |args: ToolArgs, run: ToolRunContext| {
            Box::pin(async move {
                let todos = to_todo_list(&args.todos, allow_parallel)?;
                let session = run.session().ok_or_else(|| {
                    anyhow::anyhow!("todo_write requires an owning agent session")
                })?;
                session.append(
                    "todo/write",
                    json!({"todos": todos}),
                    AppendOptions::default(),
                )?;
                let count =
                    |status: TodoStatus| todos.iter().filter(|todo| todo.status == status).count();
                Ok(ToolOutput {
                    counts: Counts {
                        pending: count(TodoStatus::Pending),
                        in_progress: count(TodoStatus::InProgress),
                        completed: count(TodoStatus::Completed),
                    },
                    todos,
                })
            })
        }),
    );
    options.present_call = Some(Arc::new(|args: &ToolArgs| {
        Some(ToolCallView::Generic(GenericCallView {
            title: "Update todo list".to_owned(),
            kind: Some(ToolCallKind::Other),
            raw_input: Some(json!(args.todos)),
            content: None,
            locations: None,
        }))
    }));
    define_tool(options)
}

/// Builds the todos projection: latest whole todo/write list, cleared by the
/// next turn/start, null before the first write.
#[must_use]
pub fn todos_projection() -> ProjectionDefinition {
    ProjectionDefinition::new(
        "todos",
        2,
        || Ok(Value::Null),
        |_state, event: &SessionEvent| {
            if event.event_type == "todo/write" {
                Ok(ProjectionTransition::Changed(event.data["todos"].clone()))
            } else if event.event_type == "turn/start" {
                Ok(ProjectionTransition::Changed(Value::Null))
            } else {
                Ok(ProjectionTransition::Unchanged)
            }
        },
        |state| Ok(state.clone()),
    )
}

/// Registers the `todos` projection and the `todo_write` tool.
///
/// # Errors
///
/// Returns when a dependency is absent or registration fails.
pub fn apply(context: &Context, config: Config) -> anyhow::Result<EffectHandle> {
    if let Some(projections) = context.get::<SessionProjectionRegistry>(SESSION_PROJECTIONS) {
        projections.register(context, todos_projection())?;
    }
    let tools: Arc<ToolRuntime> = context
        .get(TOOLS)
        .ok_or_else(|| anyhow::anyhow!("tool-todo requires tools"))?;
    tools.register(context, definition(config)?)
}

/// Registers the package's durable todo-snapshot invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(
        "seekdeep-tool-todo",
        InvariantInstaller::new(["sessions"], |context, fail| {
            Box::pin(async move {
                install(&context, &fail)?;
                Ok(())
            })
        }),
    )
}

const TODO_STATUSES: [&str; 3] = ["pending", "in_progress", "completed"];

fn install(context: &Context, fail: &InvariantFailure) -> anyhow::Result<()> {
    let sessions = context
        .get(SESSIONS)
        .ok_or_else(|| anyhow::anyhow!("seekdeep-tool-todo invariant requires sessions"))?;
    for session in sessions.list() {
        for event in session.events() {
            validate_event(&event, fail)?;
        }
    }
    let listener_fail = fail.clone();
    context.events().on_sync(
        context,
        "internal/dispatch",
        move |_, args| {
            args.get::<DispatchMode>(0)
                .ok_or_else(|| anyhow::anyhow!("internal/dispatch lacks a dispatch mode"))?;
            let event_name = args
                .get::<String>(1)
                .ok_or_else(|| anyhow::anyhow!("internal/dispatch lacks an event name"))?;
            let event_args = args
                .get::<EventArgs>(2)
                .ok_or_else(|| anyhow::anyhow!("internal/dispatch lacks event arguments"))?;
            if event_name.as_str() != "session/event" {
                return Ok(EventReply::Undefined);
            }
            let event = event_args
                .get::<SessionEvent>(1)
                .ok_or_else(|| anyhow::anyhow!("session/event lacks its event"))?;
            validate_event(event.as_ref(), &listener_fail)?;
            Ok(EventReply::Undefined)
        },
        global_events(),
    )?;
    Ok(())
}

fn validate_event(event: &SessionEvent, fail: &InvariantFailure) -> anyhow::Result<()> {
    if event.event_type == "todo/write" {
        validate_todos(event.data.get("todos"), fail)?;
    }
    Ok(())
}

/// Validates one whole-list todo snapshot before it reaches the durable log. Deliberately silent
/// on how many items are `in_progress`: that is the tool's per-deployment policy, not a durable
/// shape rule.
fn validate_todos(value: Option<&Value>, fail: &InvariantFailure) -> anyhow::Result<()> {
    let Some(array) = value.and_then(Value::as_array) else {
        return Err(fail.fail("todo/write todos must be an array").into());
    };
    let mut seen = std::collections::HashSet::new();
    for item in array {
        let Some(object) = item.as_object() else {
            return Err(fail.fail("todo/write entries must be objects").into());
        };
        let content = object.get("content").and_then(Value::as_str);
        if content.is_none_or(|content| content.is_empty() || content.trim() != content) {
            return Err(fail
                .fail("todo/write content must be non-empty and already trimmed")
                .into());
        }
        let content = content.expect("checked above");
        if !seen.insert(content.to_owned()) {
            return Err(fail
                .fail(format!(
                    "todo/write repeats content {}",
                    serde_json::to_string(content).unwrap_or_default()
                ))
                .into());
        }
        let status = object.get("status").and_then(Value::as_str);
        if status.is_none_or(|status| !TODO_STATUSES.contains(&status)) {
            let rendered = object
                .get("status")
                .map_or_else(|| "null".to_owned(), ToString::to_string);
            return Err(fail
                .fail(format!("todo/write carries unknown status {rendered}"))
                .into());
        }
    }
    Ok(())
}

fn global_events() -> EventOptions {
    EventOptions {
        global: true,
        ..EventOptions::default()
    }
}

#[cfg(test)]
mod tests {
    use seekdeep_invariants::InvariantConfig;

    use super::*;

    #[test]
    fn to_todo_list_trims_rejects_duplicates_and_enforces_active_count() {
        let raw = vec![
            TodoItemRaw {
                content: "  one  ".to_owned(),
                status: "pending".to_owned(),
            },
            TodoItemRaw {
                content: "two".to_owned(),
                status: "in_progress".to_owned(),
            },
        ];
        let todos = to_todo_list(&raw, true).expect("parallel");
        assert_eq!(todos[0].content, "one");
        assert_eq!(todos[1].status, TodoStatus::InProgress);

        let two_active = vec![
            TodoItemRaw {
                content: "a".to_owned(),
                status: "in_progress".to_owned(),
            },
            TodoItemRaw {
                content: "b".to_owned(),
                status: "in_progress".to_owned(),
            },
        ];
        assert!(to_todo_list(&two_active, true).is_ok());
        assert!(to_todo_list(&two_active, false).is_err());

        let empty = vec![TodoItemRaw {
            content: "   ".to_owned(),
            status: "pending".to_owned(),
        }];
        assert!(to_todo_list(&empty, true).is_err());

        let dup = vec![
            TodoItemRaw {
                content: "same".to_owned(),
                status: "pending".to_owned(),
            },
            TodoItemRaw {
                content: "same".to_owned(),
                status: "completed".to_owned(),
            },
        ];
        assert!(to_todo_list(&dup, true).is_err());
    }

    #[test]
    fn description_varies_only_the_active_status_clause() {
        let parallel = describe(true);
        let single = describe(false);
        assert!(parallel.contains("several at once"));
        assert!(single.contains("AT MOST ONE"));
        assert!(parallel.starts_with("Record and update"));
        assert!(parallel.ends_with("(finished)."));
        assert!(single.starts_with("Record and update"));
    }

    #[tokio::test]
    async fn explained_empty_invariant_reserves_and_releases_package_identity() {
        let context = Context::new();
        let registry =
            InvariantRegistry::install(&context, &InvariantConfig::default()).expect("registry");
        let registration = register_invariant(&registry).expect("register");
        assert!(register_invariant(&registry).is_err());
        registration.dispose().await.expect("dispose");
        register_invariant(&registry).expect("replacement");
    }

    fn event(event_type: &str, data: Value) -> SessionEvent {
        SessionEvent {
            event_type: event_type.to_owned(),
            seq: 0,
            time: 1,
            data,
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        }
    }

    #[test]
    fn todos_projection_folds_latest_list_and_clears_on_turn_start() {
        let projection = todos_projection();
        let mut state = projection.initial_state().expect("init");
        assert_eq!(state, Value::Null);

        // Unrelated events leave the standing plan unchanged.
        assert_eq!(
            projection
                .apply_event(&state, &event("turn/end", json!({"turn": 1})))
                .expect("apply"),
            ProjectionTransition::Unchanged
        );

        let first = event(
            "todo/write",
            json!({"todos": [{"content": "a", "status": "pending"}]}),
        );
        let next = projection.apply_event(&state, &first).expect("apply");
        assert_eq!(
            next,
            ProjectionTransition::Changed(json!([{"content": "a", "status": "pending"}]))
        );
        if let ProjectionTransition::Changed(value) = next {
            state = value;
        }

        let second = event(
            "todo/write",
            json!({"todos": [{"content": "a", "status": "completed"}, {"content": "b", "status": "in_progress"}]}),
        );
        assert_eq!(
            projection.apply_event(&state, &second).expect("apply"),
            ProjectionTransition::Changed(json!([
                {"content": "a", "status": "completed"},
                {"content": "b", "status": "in_progress"}
            ]))
        );

        // turn/start clears the standing plan; turn/end keeps it.
        assert_eq!(
            projection
                .apply_event(&state, &event("turn/start", json!({"turn": 2})))
                .expect("apply"),
            ProjectionTransition::Changed(Value::Null)
        );
    }

    #[test]
    fn present_call_uses_a_stable_title_and_the_list_as_raw_input() {
        let definition = definition(Config {
            allow_parallel_in_progress: true,
        })
        .expect("definition");
        let presenter = definition.present_call.as_ref().expect("presenter");
        let view =
            presenter(&json!({"todos": [{"content": "a", "status": "pending"}]})).expect("present");
        match view {
            ToolCallView::Generic(view) => {
                assert_eq!(view.title, "Update todo list");
                assert_eq!(view.kind, Some(ToolCallKind::Other));
                assert_eq!(
                    view.raw_input,
                    Some(json!([{"content": "a", "status": "pending"}]))
                );
            }
            _ => panic!("expected a generic call card"),
        }
    }
}
