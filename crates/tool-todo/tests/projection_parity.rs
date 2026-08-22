//! Live registry and plugin-lifecycle parity for the `todos` projection.

use std::sync::Arc;

use seekdeep_cordis::{Context, PluginFiber};
use seekdeep_core::{
    session::{AppendOptions, Session, SurfaceOp},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_llm::{ContentBlock, MessageSource, UserMessage};
use seekdeep_session_projection::SessionProjectionRegistry;
use seekdeep_system_prompt::SystemPromptConfig;
use seekdeep_tool_todo::plugin;
use seekdeep_tools::ToolRuntimeConfig;
use serde_json::{Value, json};

struct Bench {
    context: Context,
    session: Arc<Session>,
    projections: Arc<SessionProjectionRegistry>,
    todo: Option<Arc<PluginFiber>>,
}

impl Bench {
    async fn new(with_todo: bool) -> Self {
        let context = Context::new();
        let sessions = SessionStore::install(&context).expect("sessions");
        let prompt = seekdeep_system_prompt::install(&context, SystemPromptConfig::default())
            .expect("system prompt");
        seekdeep_tools::install(&context, &prompt, ToolRuntimeConfig::default()).expect("tools");
        let projections = SessionProjectionRegistry::install(&context).expect("projections");
        let todo = if with_todo {
            let fiber = context
                .plugin(plugin(), json!({"allowParallelInProgress": true}))
                .expect("mount todo");
            fiber.await_settled().await.expect("settle todo");
            Some(fiber)
        } else {
            None
        };
        let session = sessions
            .create(&context, None, CreateSessionOptions::default())
            .expect("session");
        Self {
            context,
            session,
            projections,
            todo,
        }
    }

    fn seed_message(&self) {
        self.session
            .append(
                "user/message",
                serde_json::to_value(UserMessage::new(
                    vec![ContentBlock::Text {
                        text: "hi".to_owned(),
                    }],
                    MessageSource::user(),
                ))
                .expect("serialize user message"),
                AppendOptions {
                    surface_op: Some(SurfaceOp::append()),
                    ..AppendOptions::default()
                },
            )
            .expect("seed message");
    }

    fn snapshot(&self) -> seekdeep_session_projection::ProjectionSnapshot {
        self.projections.snapshot(&self.session).expect("snapshot")
    }
}

#[tokio::test]
async fn projection_is_null_before_first_write_and_absent_without_the_plugin() {
    let with_todo = Bench::new(true).await;
    with_todo.seed_message();
    let snapshot = with_todo.snapshot();
    assert_eq!(snapshot.values.get("todos"), Some(&Value::Null));
    assert_eq!(
        snapshot.as_of_seq,
        i64::try_from(with_todo.session.seq()).unwrap() - 1
    );
    with_todo.context.fiber().dispose().await.expect("dispose");

    let without_todo = Bench::new(false).await;
    without_todo.seed_message();
    assert!(!without_todo.snapshot().values.contains_key("todos"));
    without_todo
        .context
        .fiber()
        .dispose()
        .await
        .expect("dispose");
}

#[tokio::test]
async fn projection_is_last_write_wins_and_clears_only_on_turn_start() {
    let bench = Bench::new(true).await;
    bench.seed_message();
    let first = json!([{"content": "a", "status": "pending"}]);
    let second = json!([
        {"content": "a", "status": "completed"},
        {"content": "b", "status": "in_progress"},
    ]);
    bench
        .session
        .append(
            "todo/write",
            json!({"todos": first}),
            AppendOptions::default(),
        )
        .expect("first write");
    bench
        .session
        .append(
            "todo/write",
            json!({"todos": second.clone()}),
            AppendOptions::default(),
        )
        .expect("second write");
    assert_eq!(bench.snapshot().values["todos"], second);

    bench
        .session
        .append(
            "turn/end",
            json!({"turn": 1, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .expect("turn end");
    assert_eq!(bench.snapshot().values["todos"], second);
    bench
        .session
        .append("turn/start", json!({"turn": 2}), AppendOptions::default())
        .expect("turn start");
    let cleared = bench.snapshot();
    assert_eq!(cleared.values["todos"], Value::Null);
    assert_eq!(
        cleared.as_of_seq,
        i64::try_from(bench.session.seq()).unwrap() - 1
    );

    bench.context.fiber().dispose().await.expect("dispose");
}

#[tokio::test]
async fn disposing_the_plugin_fiber_removes_the_projection_key() {
    let bench = Bench::new(true).await;
    bench.seed_message();
    assert_eq!(bench.snapshot().values.get("todos"), Some(&Value::Null));
    bench
        .todo
        .as_ref()
        .expect("todo fiber")
        .dispose()
        .await
        .expect("dispose todo");
    assert!(!bench.snapshot().values.contains_key("todos"));
    bench.context.fiber().dispose().await.expect("dispose root");
}
