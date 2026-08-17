//! Behavioral mirror of `packages/interaction/commands/tests/commands.spec.ts`.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use parking_lot::Mutex;
use seekdeep_agent::{Agent, AgentOptions, Inbox, NoopInboxNotifications};
use seekdeep_commands::{
    CommandDefinition, CommandInvocation, CommandResult, CommandRuntime, CommandSource, install,
    parse_command, typert_remote_contribution,
};
use seekdeep_cordis::{Context, EventOptions, EventReply};
use seekdeep_core::session::{AppendOptions, Session, SessionId};
use seekdeep_llm::AbortSignal;
use seekdeep_scope::{Scope, ScopeKey, create_scope};
use seekdeep_typert_protocol::{
    InvocationParameterSource, InvocationReceiver, RemoteInvocationMarker, TypertBoundaryValue,
    TypertCodec, TypertRemoteService,
};
use serde_json::json;
use tokio::sync::{Notify, oneshot};

fn command(name: &str, text: &str) -> CommandDefinition {
    let text = text.to_owned();
    CommandDefinition::new(
        name,
        format!("command {name}"),
        Arc::new(move |_| {
            let text = text.clone();
            Box::pin(async move { Ok(CommandResult::success(Some(text))) })
        }),
    )
}

fn mount() -> (Context, Arc<CommandRuntime>) {
    let context = Context::new();
    let commands = install(&context).expect("commands");
    (context, commands)
}

#[test]
fn exposes_list_and_execute_as_direct_typert_remote_methods() {
    let (context, commands) = mount();
    let binding = commands.typert_remote().unwrap();
    assert!(Arc::ptr_eq(&binding.service, &commands));
    assert_eq!(binding.service_key, "commands");
    assert_eq!(binding.namespace, "commands");
    let methods = commands.remote_methods();
    assert_eq!(
        methods
            .iter()
            .map(|method| method.method.as_str())
            .collect::<Vec<_>>(),
        ["list", "execute"]
    );
    assert!(
        methods
            .iter()
            .all(|method| method.invocation == RemoteInvocationMarker::Direct)
    );
    drop(context);
}

#[test]
fn generated_typert_descriptors_match_strict_agent_projected_artifact() {
    let contribution = typert_remote_contribution();
    assert_eq!(contribution.package, "@deepseek-ai/seekdeep-commands");
    assert_eq!(
        contribution
            .descriptors
            .iter()
            .map(|descriptor| descriptor.method.as_str())
            .collect::<Vec<_>>(),
        ["execute", "list"]
    );
    let execute = &contribution.descriptors[0];
    assert_eq!(
        execute.id,
        "@deepseek-ai/seekdeep-commands#commands/execute"
    );
    assert!(matches!(execute.invocation, InvocationReceiver::Direct));
    assert_eq!(execute.scope.as_ref().unwrap().context, "agent");
    assert_eq!(execute.scope.as_ref().unwrap().wire, "agentId");
    assert_eq!(
        execute.parameters[0].source,
        InvocationParameterSource::Lookup
    );
    assert_eq!(execute.parameters[0].lookup.as_deref(), Some("agent"));
    assert_eq!(execute.parameters[1].wire, "line");
    assert!(execute.cancellation);
    assert_eq!(execute.source_location.as_ref().unwrap().line, 297);
    let TypertCodec::Strict { schema, .. } = &execute.result else {
        panic!("execute result must be strict");
    };
    assert_eq!(
        schema.parse(TypertBoundaryValue::Undefined).unwrap(),
        TypertBoundaryValue::Undefined
    );
    assert!(
        schema
            .parse(TypertBoundaryValue::json(json!(null)))
            .is_err()
    );
    let list = &contribution.descriptors[1];
    assert!(!list.cancellation);
    assert_eq!(list.source_location.as_ref().unwrap().line, 260);
    let TypertCodec::Strict { schema, .. } = &list.result else {
        panic!("list result must be strict");
    };
    assert!(
        schema
            .parse(TypertBoundaryValue::json(json!([{
                "name": "goal", "description": "Set the goal"
            }])))
            .is_ok()
    );
}

fn scoped_agent(context: &Context, id: &str) -> (Scope, Arc<Agent>) {
    let key = ScopeKey::new();
    let scope = create_scope(context, key, None).expect("scope");
    let id = SessionId::new(id);
    let session = Session::create(&id, None, None).expect("session");
    let inbox =
        Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).expect("inbox"));
    let agent = Arc::new(Agent::new(
        id,
        AgentOptions::default(),
        session,
        inbox,
        scope.context.clone(),
        key,
    ));
    (scope, agent)
}

fn event_types(agent: &Agent) -> Vec<String> {
    agent
        .session()
        .events()
        .into_iter()
        .map(|event| event.event_type)
        .collect()
}

#[test]
fn parses_without_normalizing_trailing_input_and_rejects_boundaries() {
    for (line, name, raw) in [
        ("/goal", "goal", ""),
        ("/goal create the thing", "goal", " create the thing"),
        ("/goal\ncreate the thing", "goal", "\ncreate the thing"),
        ("/goal_name-2\t x ", "goal_name-2", "\t x "),
    ] {
        let parsed = parse_command(line).expect("parsed");
        assert_eq!(parsed.name, name);
        assert_eq!(parsed.raw_input, raw);
    }
    for line in ["goal", " /goal", "/", "/Goal", "/goal/path", "/goal🔥"] {
        assert_eq!(parse_command(line), None, "{line:?}");
    }
    let future: CommandSource = serde_json::from_value(json!({
        "kind": "remote-user", "surface": {"kept": true}
    }))
    .unwrap();
    assert_eq!(
        serde_json::to_value(future).unwrap(),
        json!({
            "kind": "remote-user", "surface": {"kept": true}
        })
    );
}

#[test]
fn lists_sorted_handler_free_descriptors_with_input_metadata() {
    let (context, commands) = mount();
    let (_, agent) = scoped_agent(&context, "a");
    commands
        .register(&context, command("inspect", "done").with_input("<target>"))
        .unwrap();
    commands.register(&context, command("zeta", "z")).unwrap();
    commands.register(&context, command("alpha", "a")).unwrap();
    let listed = commands.list(&agent);
    assert_eq!(
        listed
            .iter()
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>(),
        ["alpha", "inspect", "zeta"]
    );
    assert_eq!(listed[1].input.as_ref().unwrap().hint, "<target>");
    assert!(commands.find(&agent, "inspect").is_some());
    assert!(commands.find(&agent, "missing").is_none());
}

#[tokio::test]
async fn scoped_shadow_executes_then_scope_disposal_restores_global() {
    let (context, commands) = mount();
    let (scope, agent) = scoped_agent(&context, "a");
    let (_, other) = scoped_agent(&context, "b");
    commands
        .register(&context, command("shared", "global"))
        .unwrap();
    commands
        .register(&scope.context, command("shared", "scoped"))
        .unwrap();
    assert_eq!(commands.list(&agent).len(), 1);
    assert_eq!(commands.list(&other).len(), 1);
    let result = commands
        .execute(agent.clone(), "/shared", AbortSignal::default())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.result.text(), Some("scoped"));
    scope.dispose().await.unwrap();
    let result = commands
        .execute(agent, "/shared", AbortSignal::default())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.result.text(), Some("global"));
}

#[tokio::test]
async fn registration_effect_disposal_is_exact_and_idempotent() {
    let (context, commands) = mount();
    let (_, agent) = scoped_agent(&context, "a");
    let effect = commands
        .register(&context, command("temporary", "done"))
        .unwrap();
    assert!(commands.find(&agent, "temporary").is_some());
    effect.dispose().await.unwrap();
    effect.dispose().await.unwrap();
    assert!(commands.find(&agent, "temporary").is_none());
}

#[test]
fn duplicate_diagnostics_are_layer_specific() {
    let (context, commands) = mount();
    let (scope, _) = scoped_agent(&context, "a");
    commands.register(&context, command("same", "one")).unwrap();
    let error = commands
        .register(&context, command("same", "two"))
        .unwrap_err();
    assert!(error.to_string().contains("agent.ctx"));
    commands
        .register(&scope.context, command("same", "scoped"))
        .unwrap();
    let error = commands
        .register(&scope.context, command("same", "again"))
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("already registered in this scope")
    );
}

#[tokio::test]
async fn change_notification_runs_on_insert_and_disposal_and_contains_panics() {
    let (context, commands) = mount();
    let changes = Arc::new(AtomicUsize::new(0));
    let seen = changes.clone();
    context
        .events()
        .on_sync(
            &context,
            "commands/change",
            move |_, _| {
                seen.fetch_add(1, Ordering::SeqCst);
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    context
        .events()
        .on_sync(
            &context,
            "commands/change",
            |_, _| panic!("observer threw"),
            EventOptions::default(),
        )
        .unwrap();
    let after = changes.clone();
    context
        .events()
        .on_sync(
            &context,
            "commands/change",
            move |_, _| {
                after.fetch_add(10, Ordering::SeqCst);
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    let effect = commands
        .register(&context, command("live", "done"))
        .unwrap();
    effect.dispose().await.unwrap();
    assert_eq!(changes.load(Ordering::SeqCst), 22);
}

#[test]
fn invalid_metadata_is_rejected_before_registration() {
    let (context, commands) = mount();
    for (definition, fragment) in [
        (command("Bad", "x"), "command name"),
        (
            CommandDefinition::new(
                "empty-description",
                " ",
                Arc::new(|_| Box::pin(async { Ok(CommandResult::success(None::<String>)) })),
            ),
            "description",
        ),
        (command("empty-hint", "x").with_input(""), "input hint"),
    ] {
        let error = commands.register(&context, definition).unwrap_err();
        assert!(error.to_string().contains(fragment));
    }
}

#[tokio::test]
async fn passes_exact_invocation_and_returns_detached_result() {
    let (context, commands) = mount();
    let (_, agent) = scoped_agent(&context, "a");
    let seen = Arc::new(Mutex::new(None::<CommandInvocation>));
    let handler_seen = seen.clone();
    commands
        .register(
            &context,
            CommandDefinition::new(
                "run",
                "Run it",
                Arc::new(move |invocation| {
                    *handler_seen.lock() = Some(invocation);
                    Box::pin(async { Ok(CommandResult::success(Some("ok"))) })
                }),
            ),
        )
        .unwrap();
    let signal = AbortSignal::default();
    let execution = commands
        .execute(agent.clone(), "/run  untouched ", signal.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(execution.result.text(), Some("ok"));
    let invocation = seen.lock().clone().unwrap();
    assert!(Arc::ptr_eq(&invocation.agent, &agent));
    assert_eq!(invocation.raw_input, "  untouched ");
    assert_eq!(invocation.signal, signal);
    assert!(
        commands
            .execute(agent.clone(), "run", AbortSignal::default())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        commands
            .execute(agent, "/missing", AbortSignal::default())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn running_and_preaborted_signals_stop_waiting_and_late_answer_is_contained() {
    let (context, commands) = mount();
    let (_, agent) = scoped_agent(&context, "a");
    let release = Arc::new(Mutex::new(None));
    let handler_release = release.clone();
    let entered = Arc::new(Notify::new());
    let handler_entered = entered.clone();
    commands
        .register(
            &context,
            CommandDefinition::new(
                "wait",
                "Wait",
                Arc::new(move |_| {
                    let (sender, receiver) = oneshot::channel();
                    *handler_release.lock() = Some(sender);
                    handler_entered.notify_one();
                    Box::pin(async move { receiver.await.map_err(Into::into) })
                }),
            ),
        )
        .unwrap();
    let signal = AbortSignal::default();
    let task_commands = commands.clone();
    let task_agent = agent.clone();
    let task_signal = signal.clone();
    let task = tokio::spawn(async move {
        task_commands
            .execute(task_agent, "/wait", task_signal)
            .await
    });
    entered.notified().await;
    signal.abort_with_reason(json!("operator cancelled command"));
    let error = task.await.unwrap().unwrap_err();
    assert_eq!(error.to_string(), "operator cancelled command");
    release
        .lock()
        .take()
        .unwrap()
        .send(CommandResult::success(Some("late")))
        .ok();

    let already = AbortSignal::default();
    already.abort_with_reason(json!("already gone"));
    let before = agent.session().events().len();
    let error = commands.execute(agent, "/wait", already).await.unwrap_err();
    assert_eq!(error.to_string(), "already gone");
    assert_eq!(before, 2);
}

#[tokio::test]
async fn handler_error_and_panic_are_logged_and_propagated() {
    let (context, commands) = mount();
    let (_, agent) = scoped_agent(&context, "a");
    commands
        .register(
            &context,
            CommandDefinition::new(
                "reject",
                "Reject",
                Arc::new(|_| Box::pin(async { anyhow::bail!("handler rejected") })),
            ),
        )
        .unwrap();
    let error = commands
        .execute(agent.clone(), "/reject", AbortSignal::default())
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "handler rejected");
    commands
        .register(
            &context,
            CommandDefinition::new("boom", "Boom", Arc::new(|_| panic!("handler exploded"))),
        )
        .unwrap();
    let error = commands
        .execute(agent.clone(), "/boom", AbortSignal::default())
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "handler exploded");
    let events = agent.session().events();
    assert_eq!(events[1].data["kind"], "error");
    assert_eq!(events[1].data["text"], "handler rejected");
    assert_eq!(events[3].data["kind"], "error");
    assert_eq!(events[3].data["text"], "handler exploded");
}

#[tokio::test]
async fn synchronous_self_abort_wins_over_ready_handler_result() {
    let (context, commands) = mount();
    let (_, agent) = scoped_agent(&context, "a");
    let signal = AbortSignal::default();
    let handler_signal = signal.clone();
    commands
        .register(
            &context,
            CommandDefinition::new(
                "self-abort",
                "Abort",
                Arc::new(move |_| {
                    handler_signal.abort_with_reason(json!("aborted in handler"));
                    Box::pin(async { Ok(CommandResult::success(None::<String>)) })
                }),
            ),
        )
        .unwrap();
    let error = commands
        .execute(agent, "/self-abort", signal)
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "aborted in handler");
}

#[tokio::test]
async fn expected_error_and_silent_success_are_returned_and_logged() {
    let (context, commands) = mount();
    let (_, agent) = scoped_agent(&context, "a");
    commands
        .register(
            &context,
            CommandDefinition::new(
                "denied",
                "Denied",
                Arc::new(|_| Box::pin(async { Ok(CommandResult::error("not now")) })),
            ),
        )
        .unwrap();
    commands
        .register(
            &context,
            CommandDefinition::new(
                "silent",
                "Silent",
                Arc::new(|_| Box::pin(async { Ok(CommandResult::success(None::<String>)) })),
            ),
        )
        .unwrap();
    let denied = commands
        .execute(agent.clone(), "/denied", AbortSignal::default())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(denied.result, CommandResult::error("not now"));
    let silent = commands
        .execute(agent.clone(), "/silent", AbortSignal::default())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(silent.result, CommandResult::success(None::<String>));
    assert_eq!(agent.session().events()[1].data["kind"], "error");
}

#[tokio::test]
async fn successful_lifecycle_pair_is_direct_and_id_matches_execution() {
    let (context, commands) = mount();
    let (_, agent) = scoped_agent(&context, "a");
    commands
        .register(&context, command("deploy", "deployed"))
        .unwrap();
    let execution = commands
        .execute(agent.clone(), "/deploy now", AbortSignal::default())
        .await
        .unwrap()
        .unwrap();
    let events = agent.session().events();
    assert_eq!(event_types(&agent), ["command/run", "command/done"]);
    assert_eq!(events[0].data["name"], "deploy");
    assert_eq!(events[0].data["args"], " now");
    assert_eq!(events[0].data["source"], json!({"kind": "user"}));
    assert_eq!(events[1].data["text"], "deployed");
    assert_eq!(events[0].data["commandId"], events[1].data["commandId"]);
    assert_eq!(events[0].data["commandId"], execution.command_id.as_str());
}

#[tokio::test]
async fn authoritative_source_reference_and_private_input_are_preserved() {
    let (context, commands) = mount();
    let (_, agent) = scoped_agent(&context, "a");
    let source = agent
        .session()
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .unwrap();
    let sequence = source.seq;
    commands
        .register(
            &context,
            CommandDefinition::new(
                "linked",
                "Link",
                Arc::new(move |_| {
                    Box::pin(
                        async move { Ok(CommandResult::success_linked(Some("linked"), sequence)) },
                    )
                }),
            )
            .record_input(false),
        )
        .unwrap();
    let result = commands
        .execute(agent.clone(), "/linked secret", AbortSignal::default())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.result.source_event_seq(), Some(sequence));
    let events = agent.session().events();
    assert!(events[1].data.get("args").is_none());
    assert_eq!(events[2].data["sourceEventSeq"], sequence);
    assert_eq!(
        event_types(&agent),
        ["turn/start", "command/run", "command/done"]
    );
}

#[tokio::test]
async fn command_ids_are_distinct_monotonic_and_admission_misses_log_nothing() {
    let (context, commands) = mount();
    let (_, agent) = scoped_agent(&context, "a");
    commands.register(&context, command("first", "1")).unwrap();
    commands.register(&context, command("second", "2")).unwrap();
    assert!(
        commands
            .execute(agent.clone(), "not a command", AbortSignal::default())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        commands
            .execute(agent.clone(), "/missing", AbortSignal::default())
            .await
            .unwrap()
            .is_none()
    );
    assert!(agent.session().events().is_empty());
    let first = commands
        .execute(agent.clone(), "/first", AbortSignal::default())
        .await
        .unwrap()
        .unwrap();
    let second = commands
        .execute(agent, "/second", AbortSignal::default())
        .await
        .unwrap()
        .unwrap();
    assert_ne!(first.command_id, second.command_id);
    assert!(first.command_id.as_str().ends_with("-1"));
    assert!(second.command_id.as_str().ends_with("-2"));
}

#[tokio::test]
async fn malformed_representable_results_fail_and_still_pair_the_log() {
    let (context, commands) = mount();
    let (_, agent) = scoped_agent(&context, "a");
    let cases = [
        ("empty", CommandResult::error(""), "error text"),
        (
            "unsafe-seq",
            CommandResult::success_linked(None::<String>, 9_007_199_254_740_992),
            "sourceEventSeq",
        ),
    ];
    for (name, result, fragment) in cases {
        commands
            .register(
                &context,
                CommandDefinition::new(
                    name,
                    "Broken",
                    Arc::new(move |_| {
                        let result = result.clone();
                        Box::pin(async move { Ok(result) })
                    }),
                ),
            )
            .unwrap();
        let error = commands
            .execute(agent.clone(), &format!("/{name}"), AbortSignal::default())
            .await
            .unwrap_err();
        assert!(error.to_string().contains(fragment));
    }
    assert_eq!(agent.session().events().len(), 4);
}
