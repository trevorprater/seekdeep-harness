//! Behavioral mirror of `packages/core/tools/tests/scoped.spec.ts`.
//!
//! JavaScript accessor/proxy/class-instance hazards are represented at the
//! Rust boundary by the closed, owned `serde_json::Value` input type. The
//! runtime cases below test the corresponding snapshot and notification
//! guarantees after that structural boundary.

use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use parking_lot::Mutex;
use seekdeep_cordis::{Context, EventOptions};
use seekdeep_llm::{AbortSignal, CallId, ContentBlock};
use seekdeep_scope::{Scope, ScopeKey, create_scope};
use seekdeep_system_prompt::{AssembleContext, SystemPrompt, SystemPromptConfig};
use seekdeep_tools::{
    PreToolDecision, ScheduledToolPreparation, ToolDefinition, ToolExecutionInput,
    ToolExecutionToken, ToolOutputDefinition, ToolRestriction, ToolRuntime, ToolRuntimeConfig,
    assert_supported_json_schema,
};
use serde_json::{Map, Value, json};

struct Mounted {
    root: Context,
    prompt: Arc<SystemPrompt>,
    tools: Arc<ToolRuntime>,
}

fn mount() -> Mounted {
    let root = Context::new();
    let prompt = SystemPrompt::new(&root, SystemPromptConfig::default()).expect("system prompt");
    let tools = ToolRuntime::new_with_system_prompt(&root, &prompt, ToolRuntimeConfig::default())
        .expect("tools");
    Mounted {
        root,
        prompt,
        tools,
    }
}

fn mint_scope(root: &Context, parent: Option<ScopeKey>) -> (Scope, ScopeKey) {
    let key = ScopeKey::new();
    (
        create_scope(root, key, parent).expect("scope creation"),
        key,
    )
}

fn tool(name: &str, reply: &str) -> ToolDefinition {
    let output_schema =
        Arc::new(assert_supported_json_schema(json!({"type": "string"})).expect("output schema"));
    let rendered = reply.to_owned();
    ToolDefinition::new(
        name,
        format!("tool {name}"),
        Map::from_iter([
            ("type".to_owned(), json!("object")),
            ("properties".to_owned(), json!({})),
        ]),
        ToolOutputDefinition::new(
            output_schema,
            Arc::new(|_, value| {
                Ok(vec![ContentBlock::Text {
                    text: value.as_str().unwrap_or_default().to_owned(),
                }])
            }),
        ),
        Arc::new(move |_, _| {
            let rendered = rendered.clone();
            Box::pin(async move { Ok(Value::String(rendered)) })
        }),
    )
}

fn input(name: &str, agent: Option<ScopeKey>) -> ToolExecutionInput {
    let input = ToolExecutionInput::new(
        CallId::new(format!("call-{name}")),
        name,
        json!({}),
        AbortSignal::default(),
    );
    agent.map_or(input.clone(), |agent| input.with_agent_scope(agent))
}

async fn run(tools: &Arc<ToolRuntime>, name: &str, agent: Option<ScopeKey>) -> String {
    let result = tools.execute(input(name, agent)).await;
    match result.content().first() {
        Some(ContentBlock::Text { text }) => text.clone(),
        other => format!("{other:?}"),
    }
}

fn names(tools: &ToolRuntime, scope: Option<ScopeKey>) -> Vec<String> {
    let mut names = tools
        .schemas(scope)
        .into_iter()
        .map(|schema| schema.name)
        .collect::<Vec<_>>();
    names.sort();
    names
}

#[tokio::test]
async fn keeps_final_result_observers_synchronous() {
    let mounted = mount();
    mounted
        .tools
        .register(&mounted.root, tool("t", "ran:t"))
        .expect("register");
    let observed = Arc::new(AtomicUsize::new(0));
    let listener_observed = observed.clone();
    mounted
        .tools
        .on_result(
            &mounted.root,
            move |_, _| {
                listener_observed.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            EventOptions::default(),
        )
        .expect("observer");
    let result = mounted.tools.execute(input("t", None)).await;
    assert!(!result.is_error());
    assert_eq!(observed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn files_a_scoped_tool_in_its_layer_only() {
    let mounted = mount();
    let (scope, key) = mint_scope(&mounted.root, None);
    let (_, other) = mint_scope(&mounted.root, None);
    mounted
        .tools
        .register(&mounted.root, tool("shared", "ran:shared"))
        .expect("global");
    mounted
        .tools
        .register(&scope.context, tool("mine", "ran:mine"))
        .expect("scoped");
    assert_eq!(names(&mounted.tools, Some(key)), ["mine", "shared"]);
    assert_eq!(names(&mounted.tools, None), ["shared"]);
    assert_eq!(names(&mounted.tools, Some(other)), ["shared"]);
    assert_eq!(run(&mounted.tools, "mine", Some(key)).await, "ran:mine");
    assert_eq!(
        run(&mounted.tools, "mine", Some(other)).await,
        "Error: unknown tool \"mine\""
    );
    assert_eq!(
        run(&mounted.tools, "mine", None).await,
        "Error: unknown tool \"mine\""
    );
}

#[tokio::test]
async fn scoped_shadows_global_in_either_registration_order() {
    let mounted = mount();
    let (scope, key) = mint_scope(&mounted.root, None);
    mounted
        .tools
        .register(&scope.context, tool("bash", "restricted-bash"))
        .expect("scoped first");
    mounted
        .tools
        .register(&mounted.root, tool("bash", "global-bash"))
        .expect("global second");
    assert_eq!(
        run(&mounted.tools, "bash", Some(key)).await,
        "restricted-bash"
    );
    assert_eq!(run(&mounted.tools, "bash", None).await, "global-bash");
    assert_eq!(
        mounted
            .tools
            .schemas(Some(key))
            .iter()
            .filter(|schema| schema.name == "bash")
            .count(),
        1
    );
}

#[tokio::test]
async fn rejects_duplicate_names_with_layer_specific_diagnostics() {
    let mounted = mount();
    let (scope, _) = mint_scope(&mounted.root, None);
    mounted
        .tools
        .register(&mounted.root, tool("x", "x"))
        .expect("first global");
    let global = mounted
        .tools
        .register(&mounted.root, tool("x", "x"))
        .expect_err("duplicate global");
    assert!(global.to_string().contains("agent.ctx"));
    mounted
        .tools
        .register(&scope.context, tool("y", "y"))
        .expect("first scoped");
    let scoped = mounted
        .tools
        .register(&scope.context, tool("y", "y"))
        .expect_err("duplicate scoped");
    assert!(
        scoped
            .to_string()
            .contains("already registered in this scope")
    );
}

#[tokio::test]
async fn disposing_scope_unwinds_registrations_without_residue() {
    let mounted = mount();
    let (scope, key) = mint_scope(&mounted.root, None);
    mounted
        .tools
        .register(&scope.context, tool("mine", "mine"))
        .expect("scoped");
    assert!(mounted.tools.get("mine", Some(key)).is_some());
    scope.dispose().await.expect("dispose");
    assert!(mounted.tools.get("mine", Some(key)).is_none());
    assert!(mounted.tools.schemas(Some(key)).is_empty());
}

#[tokio::test]
async fn restriction_masks_inherited_then_merges_local_and_keeps_prompt_and_execution_aligned() {
    let mounted = mount();
    let (scope, key) = mint_scope(&mounted.root, None);
    for name in ["read", "bash"] {
        mounted
            .tools
            .register(&mounted.root, tool(name, &format!("ran:{name}")))
            .expect("global");
    }
    mounted
        .tools
        .register(&scope.context, tool("capture", "ran:capture"))
        .expect("local");
    mounted
        .tools
        .restrict(
            &scope.context,
            ToolRestriction {
                allow: Some(vec!["read".to_owned()]),
                deny: None,
            },
        )
        .expect("restrict");
    assert_eq!(names(&mounted.tools, Some(key)), ["capture", "read"]);
    let assembly = mounted
        .prompt
        .assemble(AssembleContext {
            scope: Some(key),
            ..AssembleContext::default()
        })
        .await
        .expect("assembly");
    let mut assembled = assembly
        .tools
        .into_iter()
        .map(|schema| schema.name)
        .collect::<Vec<_>>();
    assembled.sort();
    assert_eq!(assembled, ["capture", "read"]);
    assert_eq!(
        run(&mounted.tools, "bash", Some(key)).await,
        "Error: unknown tool \"bash\""
    );
    assert_eq!(run(&mounted.tools, "read", Some(key)).await, "ran:read");
    assert_eq!(
        run(&mounted.tools, "capture", Some(key)).await,
        "ran:capture"
    );
    assert_eq!(names(&mounted.tools, None), ["bash", "read"]);
}

#[tokio::test]
async fn snapshotted_filters_apply_to_live_global_registry_before_later_local_merge() {
    let mounted = mount();
    let (denied_scope, denied) = mint_scope(&mounted.root, None);
    let (allowed_scope, allowed) = mint_scope(&mounted.root, None);
    for name in ["read", "bash"] {
        mounted
            .tools
            .register(&mounted.root, tool(name, &format!("ran:{name}")))
            .expect("global");
    }
    mounted
        .tools
        .restrict(
            &denied_scope.context,
            ToolRestriction {
                allow: None,
                deny: Some(vec!["bash".to_owned()]),
            },
        )
        .expect("deny");
    mounted
        .tools
        .restrict(
            &allowed_scope.context,
            ToolRestriction {
                allow: Some(vec!["read".to_owned()]),
                deny: None,
            },
        )
        .expect("allow");
    mounted
        .tools
        .register(&mounted.root, tool("web", "ran:web"))
        .expect("late global");
    mounted
        .tools
        .register(
            &denied_scope.context,
            tool("denied-local", "ran:denied-local"),
        )
        .expect("denied local");
    mounted
        .tools
        .register(
            &allowed_scope.context,
            tool("allowed-local", "ran:allowed-local"),
        )
        .expect("allowed local");
    assert_eq!(
        names(&mounted.tools, Some(denied)),
        ["denied-local", "read", "web"]
    );
    assert_eq!(
        names(&mounted.tools, Some(allowed)),
        ["allowed-local", "read"]
    );
}

#[tokio::test]
async fn multiple_restrictions_intersect_and_lift_independently() {
    let mounted = mount();
    let (scope, key) = mint_scope(&mounted.root, None);
    for name in ["a", "b", "c"] {
        mounted
            .tools
            .register(&mounted.root, tool(name, name))
            .expect("global");
    }
    let lift_allow = mounted
        .tools
        .restrict(
            &scope.context,
            ToolRestriction {
                allow: Some(vec!["a".to_owned(), "b".to_owned()]),
                deny: None,
            },
        )
        .expect("allow");
    mounted
        .tools
        .restrict(
            &scope.context,
            ToolRestriction {
                allow: None,
                deny: Some(vec!["b".to_owned()]),
            },
        )
        .expect("deny");
    assert_eq!(names(&mounted.tools, Some(key)), ["a"]);
    lift_allow.dispose().await.expect("lift allow");
    assert_eq!(names(&mounted.tools, Some(key)), ["a", "c"]);
}

#[tokio::test]
async fn restriction_values_are_compiled_at_registration() {
    let mounted = mount();
    let (scope, key) = mint_scope(&mounted.root, None);
    for name in ["a", "b"] {
        mounted
            .tools
            .register(&mounted.root, tool(name, name))
            .expect("global");
    }
    let mut filter = ToolRestriction {
        allow: None,
        deny: Some(vec!["a".to_owned()]),
    };
    mounted
        .tools
        .restrict(&scope.context, filter.clone())
        .expect("restrict");
    filter.deny.as_mut().expect("deny").push("b".to_owned());
    assert_eq!(names(&mounted.tools, Some(key)), ["b"]);
}

#[tokio::test]
async fn restriction_fails_loud_for_invalid_context_filter_and_names() {
    let mounted = mount();
    let (scope, _) = mint_scope(&mounted.root, None);
    mounted
        .tools
        .register(&mounted.root, tool("real", "real"))
        .expect("global");
    mounted
        .tools
        .register(&scope.context, tool("local", "local"))
        .expect("local");
    let unscoped = mounted
        .tools
        .restrict(
            &mounted.root,
            ToolRestriction {
                allow: None,
                deny: Some(vec!["real".to_owned()]),
            },
        )
        .expect_err("unscoped");
    assert!(unscoped.to_string().contains("requires a scoped context"));
    let empty = mounted
        .tools
        .restrict(&scope.context, ToolRestriction::default())
        .expect_err("no-op");
    assert!(empty.to_string().contains("no-op"));
    for (filter, fragment) in [
        (
            ToolRestriction {
                allow: Some(vec!["local".to_owned()]),
                deny: None,
            },
            "unknown global tool \"local\"",
        ),
        (
            ToolRestriction {
                allow: Some(vec!["reall".to_owned()]),
                deny: None,
            },
            "known global tools: real",
        ),
        (
            ToolRestriction {
                allow: None,
                deny: Some(vec!["ghost".to_owned(), "wraith".to_owned()]),
            },
            "unknown global tools \"ghost\", \"wraith\"",
        ),
    ] {
        let error = mounted
            .tools
            .restrict(&scope.context, filter)
            .expect_err("unknown name");
        assert!(error.to_string().contains(fragment), "{error:#}");
    }

    let empty_mount = mount();
    let (empty_scope, _) = mint_scope(&empty_mount.root, None);
    let error = empty_mount
        .tools
        .restrict(
            &empty_scope.context,
            ToolRestriction {
                allow: None,
                deny: Some(vec!["ghost".to_owned()]),
            },
        )
        .expect_err("unknown in empty runtime");
    assert!(error.to_string().contains("known global tools: (none)"));
}

#[tokio::test]
async fn child_filters_tools_inherited_from_an_ancestor_scope() {
    let mounted = mount();
    let (parent_scope, parent) = mint_scope(&mounted.root, None);
    for name in ["bash", "read"] {
        mounted
            .tools
            .register(&parent_scope.context, tool(name, &format!("ran:{name}")))
            .expect("parent tool");
    }
    let (child_scope, child) = mint_scope(&mounted.root, Some(parent));
    assert_eq!(names(&mounted.tools, Some(child)), ["bash", "read"]);
    mounted
        .tools
        .restrict(
            &child_scope.context,
            ToolRestriction {
                allow: None,
                deny: Some(vec!["bash".to_owned()]),
            },
        )
        .expect("child deny");
    assert_eq!(names(&mounted.tools, Some(child)), ["read"]);
    assert_eq!(names(&mounted.tools, Some(parent)), ["bash", "read"]);
}

#[tokio::test]
async fn child_own_registrations_stay_outside_its_filter() {
    let mounted = mount();
    let (parent_scope, parent) = mint_scope(&mounted.root, None);
    for name in ["bash", "read"] {
        mounted
            .tools
            .register(&parent_scope.context, tool(name, &format!("ran:{name}")))
            .expect("parent tool");
    }
    let (child_scope, child) = mint_scope(&mounted.root, Some(parent));
    mounted
        .tools
        .register(&child_scope.context, tool("report", "ran:report"))
        .expect("child local");
    mounted
        .tools
        .restrict(
            &child_scope.context,
            ToolRestriction {
                allow: Some(vec!["read".to_owned()]),
                deny: None,
            },
        )
        .expect("child allow");
    assert_eq!(names(&mounted.tools, Some(child)), ["read", "report"]);
    assert_eq!(
        run(&mounted.tools, "report", Some(child)).await,
        "ran:report"
    );
}

#[tokio::test]
async fn ancestor_restriction_reaches_every_nested_scope() {
    let mounted = mount();
    mounted
        .tools
        .register(&mounted.root, tool("web", "web"))
        .expect("global");
    let (parent_scope, parent) = mint_scope(&mounted.root, None);
    mounted
        .tools
        .register(&parent_scope.context, tool("bash", "bash"))
        .expect("parent");
    let (_, child) = mint_scope(&mounted.root, Some(parent));
    mounted
        .tools
        .restrict(
            &parent_scope.context,
            ToolRestriction {
                allow: None,
                deny: Some(vec!["web".to_owned()]),
            },
        )
        .expect("parent deny");
    assert_eq!(names(&mounted.tools, Some(child)), ["bash"]);
    assert_eq!(names(&mounted.tools, Some(parent)), ["bash"]);
}

#[tokio::test]
async fn scoped_pre_execute_listener_gates_only_its_scope() {
    let mounted = mount();
    let (scope, key) = mint_scope(&mounted.root, None);
    let (_, other) = mint_scope(&mounted.root, None);
    mounted
        .tools
        .register(&mounted.root, tool("t", "ran:t"))
        .expect("tool");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let listener_seen = seen.clone();
    mounted
        .tools
        .on_pre_execute(
            &scope.context,
            move |execution, _| {
                listener_seen.lock().push(execution.scope_key());
                async {
                    Ok(PreToolDecision::Deny {
                        reason: "scoped veto".to_owned(),
                    })
                }
            },
            EventOptions::default(),
        )
        .expect("scoped pre");
    assert_eq!(
        run(&mounted.tools, "t", Some(key)).await,
        "Error: scoped veto"
    );
    assert_eq!(run(&mounted.tools, "t", Some(other)).await, "ran:t");
    assert_eq!(run(&mounted.tools, "t", None).await, "ran:t");
    assert_eq!(*seen.lock(), [Some(key)]);
}

#[tokio::test]
async fn scoped_guards_run_after_pre_and_unwind_independently() {
    let mounted = mount();
    let (scope, key) = mint_scope(&mounted.root, None);
    let (_, other) = mint_scope(&mounted.root, None);
    let body_calls = Arc::new(AtomicUsize::new(0));
    let tool_calls = body_calls.clone();
    let mut definition = tool("t", "ran:t");
    definition.execute = Arc::new(move |_, _| {
        tool_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(json!("ran:t")) })
    });
    mounted
        .tools
        .register(&mounted.root, definition)
        .expect("tool");
    let guard = Arc::new(|_: &_| Some("terminal policy".to_owned()));
    let lift_first = mounted
        .tools
        .guard(&scope.context, guard.clone())
        .expect("first guard");
    mounted
        .tools
        .guard(&scope.context, guard)
        .expect("second guard");
    mounted
        .tools
        .on_pre_execute(
            &scope.context,
            |_, _| async { Ok(PreToolDecision::Allow) },
            EventOptions {
                prepend: true,
                ..EventOptions::default()
            },
        )
        .expect("forced allow");
    assert_eq!(
        run(&mounted.tools, "t", Some(key)).await,
        "Error: terminal policy"
    );
    assert_eq!(run(&mounted.tools, "t", Some(other)).await, "ran:t");
    assert_eq!(body_calls.load(Ordering::SeqCst), 1);
    lift_first.dispose().await.expect("lift first");
    assert_eq!(
        run(&mounted.tools, "t", Some(key)).await,
        "Error: terminal policy"
    );
    scope.dispose().await.expect("scope dispose");
    assert_eq!(run(&mounted.tools, "t", Some(key)).await, "ran:t");
    assert_eq!(body_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn global_guards_compose_monotonically() {
    let mounted = mount();
    let body_calls = Arc::new(AtomicUsize::new(0));
    let tool_calls = body_calls.clone();
    let mut definition = tool("t", "ran:t");
    definition.execute = Arc::new(move |_, _| {
        tool_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(json!("ran:t")) })
    });
    mounted
        .tools
        .register(&mounted.root, definition)
        .expect("tool");
    mounted
        .tools
        .guard(&mounted.root, Arc::new(|_| None))
        .expect("abstain");
    mounted
        .tools
        .guard(
            &mounted.root,
            Arc::new(|_| Some("global denial".to_owned())),
        )
        .expect("deny");
    assert_eq!(run(&mounted.tools, "t", None).await, "Error: global denial");
    assert_eq!(body_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn guard_iteration_is_live_for_later_insertions() {
    let mounted = mount();
    mounted
        .tools
        .register(&mounted.root, tool("t", "ran:t"))
        .expect("tool");
    let calls = Arc::new(Mutex::new(Vec::new()));
    let added = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let weak = Arc::downgrade(&mounted.tools);
    let root = mounted.root.clone();
    let guard_calls = calls.clone();
    let guard_added = added.clone();
    mounted
        .tools
        .guard(
            &mounted.root,
            Arc::new(move |_| {
                guard_calls.lock().push("first");
                if !guard_added.swap(true, Ordering::SeqCst) {
                    let late_calls = guard_calls.clone();
                    weak.upgrade()
                        .expect("runtime")
                        .guard(
                            &root,
                            Arc::new(move |_| {
                                late_calls.lock().push("late");
                                Some("late denial".to_owned())
                            }),
                        )
                        .expect("late guard");
                }
                None
            }),
        )
        .expect("first guard");
    assert_eq!(run(&mounted.tools, "t", None).await, "Error: late denial");
    assert_eq!(*calls.lock(), ["first", "late"]);
}

#[tokio::test]
async fn replacement_of_last_guard_is_deferred_to_next_generation() {
    let mounted = mount();
    let (scope, key) = mint_scope(&mounted.root, None);
    mounted
        .tools
        .register(&mounted.root, tool("t", "ran:t"))
        .expect("tool");
    mounted
        .tools
        .register(&scope.context, tool("scope_sibling", "sibling"))
        .expect("keep layer");
    let calls = Arc::new(Mutex::new(Vec::new()));
    let holder = Arc::new(Mutex::new(None::<seekdeep_cordis::fiber::EffectHandle>));
    let weak = Arc::downgrade(&mounted.tools);
    let scoped_context = scope.context.clone();
    let guard_calls = calls.clone();
    let guard_holder = holder.clone();
    let lift = mounted
        .tools
        .guard(
            &scope.context,
            Arc::new(move |_| {
                guard_calls.lock().push("first");
                guard_holder
                    .lock()
                    .take()
                    .expect("lift installed")
                    .dispose_now_for_test();
                let replacement_calls = guard_calls.clone();
                weak.upgrade()
                    .expect("runtime")
                    .guard(
                        &scoped_context,
                        Arc::new(move |_| {
                            replacement_calls.lock().push("replacement");
                            Some("replacement denial".to_owned())
                        }),
                    )
                    .expect("replacement");
                None
            }),
        )
        .expect("first guard");
    *holder.lock() = Some(lift);
    assert_eq!(run(&mounted.tools, "t", Some(key)).await, "ran:t");
    assert_eq!(*calls.lock(), ["first"]);
    assert_eq!(
        run(&mounted.tools, "t", Some(key)).await,
        "Error: replacement denial"
    );
    assert_eq!(*calls.lock(), ["first", "replacement"]);
}

#[tokio::test]
async fn one_token_and_one_structural_argument_snapshot_flow_through_pipeline() {
    let mounted = mount();
    let (scope, key) = mint_scope(&mounted.root, None);
    let safe_calls = Arc::new(AtomicUsize::new(0));
    let observed_args = Arc::new(Mutex::new(None));
    let body_calls = safe_calls.clone();
    let body_args = observed_args.clone();
    let mut safe = tool("safe", "safe");
    safe.execute = Arc::new(move |arguments, _| {
        body_calls.fetch_add(1, Ordering::SeqCst);
        *body_args.lock() = Some(arguments);
        Box::pin(async { Ok(json!("safe")) })
    });
    mounted.tools.register(&mounted.root, safe).expect("safe");
    mounted
        .tools
        .register(&mounted.root, tool("danger", "danger"))
        .expect("danger");
    mounted
        .tools
        .guard(
            &scope.context,
            Arc::new(|execution| {
                (execution.name == "danger").then_some("danger denied".to_owned())
            }),
        )
        .expect("guard");
    let tokens = Arc::new(Mutex::new(HashSet::<ToolExecutionToken>::new()));
    let pre_tokens = tokens.clone();
    mounted
        .tools
        .on_pre_execute(
            &mounted.root,
            move |execution, next| {
                pre_tokens.lock().insert(execution.token);
                async move { next.run().await }
            },
            EventOptions::default(),
        )
        .expect("pre");
    let around_tokens = tokens.clone();
    mounted
        .tools
        .on_execute(
            &mounted.root,
            move |execution, next| {
                around_tokens.lock().insert(execution.token);
                async move { next.run().await }
            },
            EventOptions::default(),
        )
        .expect("around");
    let post_tokens = tokens.clone();
    mounted
        .tools
        .on_post_execute(
            &mounted.root,
            move |execution, _, next| {
                post_tokens.lock().insert(execution.token);
                async move { next.run().await }
            },
            EventOptions::default(),
        )
        .expect("post");
    assert_eq!(
        run(&mounted.tools, "danger", Some(key)).await,
        "Error: danger denied"
    );
    let caller_arguments = json!({"source": true});
    let result = mounted
        .tools
        .execute(
            ToolExecutionInput::new(
                CallId::new("safe-call"),
                "safe",
                caller_arguments.clone(),
                AbortSignal::default(),
            )
            .with_agent_scope(key),
        )
        .await;
    assert!(!result.is_error());
    assert_eq!(*observed_args.lock(), Some(caller_arguments));
    assert_eq!(tokens.lock().len(), 2);
    assert_eq!(safe_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn non_cloneable_arguments_are_unrepresentable_and_structural_input_publishes_once() {
    let mounted = mount();
    let (scope, key) = mint_scope(&mounted.root, None);
    let policy_calls = Arc::new(AtomicUsize::new(0));
    let pre_calls = policy_calls.clone();
    mounted
        .tools
        .on_pre_execute(
            &mounted.root,
            move |_, next| {
                pre_calls.fetch_add(1, Ordering::SeqCst);
                async move { next.run().await }
            },
            EventOptions::default(),
        )
        .expect("pre");
    let body_calls = Arc::new(AtomicUsize::new(0));
    let tool_calls = body_calls.clone();
    let mut definition = tool("t", "ran:t");
    definition.execute = Arc::new(move |_, _| {
        tool_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(json!("ran:t")) })
    });
    mounted
        .tools
        .register(&mounted.root, definition)
        .expect("tool");
    let scoped_observed = Arc::new(AtomicUsize::new(0));
    let scoped_count = scoped_observed.clone();
    mounted
        .tools
        .on_result(
            &scope.context,
            move |_, _| {
                scoped_count.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            EventOptions::default(),
        )
        .expect("scoped observer");
    let global_observed = Arc::new(AtomicUsize::new(0));
    let global_count = global_observed.clone();
    mounted
        .tools
        .on_result(
            &mounted.root,
            move |_, _| {
                global_count.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            EventOptions::default(),
        )
        .expect("global observer");

    let result = mounted
        .tools
        .execute(
            ToolExecutionInput::new(
                CallId::new("structural"),
                "t",
                json!({"nested": [true, null, 3]}),
                AbortSignal::default(),
            )
            .with_agent_scope(key),
        )
        .await;
    assert!(!result.is_error());
    assert_eq!(policy_calls.load(Ordering::SeqCst), 1);
    assert_eq!(body_calls.load(Ordering::SeqCst), 1);
    assert_eq!(scoped_observed.load(Ordering::SeqCst), 1);
    assert_eq!(global_observed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn structural_value_boundary_excludes_javascript_map_arguments() {
    let value = json!({"entries": [["mutable", true]]});
    let input = ToolExecutionInput::new(
        CallId::new("structural-map"),
        "t",
        value.clone(),
        AbortSignal::default(),
    );
    assert_eq!(input.arguments, value);
}

#[tokio::test]
async fn structural_value_boundary_excludes_javascript_class_instances() {
    let value = json!({"value": 1});
    let input = ToolExecutionInput::new(
        CallId::new("structural-instance"),
        "t",
        value.clone(),
        AbortSignal::default(),
    );
    assert_eq!(input.arguments, value);
}

#[tokio::test]
async fn parent_and_identity_snapshot_are_consistent_across_every_observer() {
    let mounted = mount();
    mounted
        .tools
        .register(&mounted.root, tool("t", "ran:t"))
        .expect("tool");
    let ScheduledToolPreparation::Dispatch {
        execution: parent_execution,
    } = mounted.tools.prepare_scheduled(input("t", None)).await
    else {
        panic!("registered tool should prepare for dispatch")
    };
    let parent = parent_execution.token;
    let observed = Arc::new(Mutex::new(Vec::new()));
    let pre = observed.clone();
    mounted
        .tools
        .on_pre_execute(
            &mounted.root,
            move |execution, next| {
                pre.lock().push(execution.parent);
                async move { next.run().await }
            },
            EventOptions::default(),
        )
        .expect("pre");
    let around = observed.clone();
    mounted
        .tools
        .on_execute(
            &mounted.root,
            move |execution, next| {
                around.lock().push(execution.parent);
                async move { next.run().await }
            },
            EventOptions::default(),
        )
        .expect("around");
    let result_seen = observed.clone();
    mounted
        .tools
        .on_result(
            &mounted.root,
            move |execution, _| {
                result_seen.lock().push(execution.parent);
                Ok(())
            },
            EventOptions::default(),
        )
        .expect("result");
    let result = mounted
        .tools
        .execute(
            ToolExecutionInput::new(
                CallId::new("stateful-parent"),
                "t",
                json!({}),
                AbortSignal::default(),
            )
            .with_parent(parent),
        )
        .await;
    assert!(!result.is_error());
    assert_eq!(*observed.lock(), [Some(parent), Some(parent), Some(parent)]);
}

#[tokio::test]
async fn normalized_error_shell_uses_the_owned_input_snapshot() {
    let mounted = mount();
    let (scope, key) = mint_scope(&mounted.root, None);
    let observed = Arc::new(Mutex::new(None));
    let result_observed = observed.clone();
    mounted
        .tools
        .on_result(
            &scope.context,
            move |execution, _| {
                *result_observed.lock() = Some((
                    execution.call_id.clone(),
                    execution.name.clone(),
                    execution.scope_key(),
                    execution.arguments.clone(),
                ));
                Ok(())
            },
            EventOptions::default(),
        )
        .expect("observer");
    let arguments = json!({"stable": true});
    let result = mounted
        .tools
        .execute(
            ToolExecutionInput::new(
                CallId::new("stable-error"),
                "missing",
                arguments.clone(),
                AbortSignal::default(),
            )
            .with_agent_scope(key),
        )
        .await;
    assert!(result.is_error());
    assert_eq!(
        *observed.lock(),
        Some((
            CallId::new("stable-error"),
            "missing".to_owned(),
            Some(key),
            arguments
        ))
    );
}

#[tokio::test]
async fn throwing_argument_accessors_are_unrepresentable_after_structural_materialization() {
    let value = json!({"nested": {"safe": true}});
    let cloned = value.clone();
    assert_eq!(cloned, value);
}

#[tokio::test]
async fn nested_arguments_are_snapshotted_into_the_executed_value() {
    let mounted = mount();
    let seen = Arc::new(Mutex::new(None));
    let body_seen = seen.clone();
    let mut definition = tool("t", "ran:t");
    definition.execute = Arc::new(move |arguments, _| {
        *body_seen.lock() = Some(arguments);
        Box::pin(async { Ok(json!("ran:t")) })
    });
    mounted
        .tools
        .register(&mounted.root, definition)
        .expect("tool");
    let arguments = json!({"nested": {"value": "safe"}});
    let result = mounted
        .tools
        .execute(ToolExecutionInput::new(
            CallId::new("nested"),
            "t",
            arguments.clone(),
            AbortSignal::default(),
        ))
        .await;
    assert!(!result.is_error());
    assert_eq!(*seen.lock(), Some(arguments));
}

#[tokio::test]
async fn every_result_observer_runs_synchronously_and_failures_are_contained() {
    let mounted = mount();
    let (scope, key) = mint_scope(&mounted.root, None);
    mounted
        .tools
        .register(&mounted.root, tool("t", "ran:t"))
        .expect("tool");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let scoped_seen = seen.clone();
    mounted
        .tools
        .on_result(
            &scope.context,
            move |_, result| {
                scoped_seen.lock().push(result.is_error());
                Ok(())
            },
            EventOptions::default(),
        )
        .expect("scoped observer");
    mounted
        .tools
        .on_result(
            &mounted.root,
            |_, _| anyhow::bail!("observer failure"),
            EventOptions::default(),
        )
        .expect("failing observer");
    mounted
        .tools
        .on_result(
            &mounted.root,
            |_, _| panic!("observer panic"),
            EventOptions::default(),
        )
        .expect("panicking observer");
    let final_seen = seen.clone();
    mounted
        .tools
        .on_result(
            &mounted.root,
            move |_, result| {
                final_seen.lock().push(result.is_error());
                Ok(())
            },
            EventOptions::default(),
        )
        .expect("final observer");
    let result = mounted.tools.execute(input("t", Some(key))).await;
    assert!(!result.is_error());
    assert_eq!(*seen.lock(), [false, false]);
}

trait DisposeNowForTest {
    fn dispose_now_for_test(&self);
}

impl DisposeNowForTest for seekdeep_cordis::fiber::EffectHandle {
    fn dispose_now_for_test(&self) {
        futures::executor::block_on(self.dispose()).expect("synchronous guard disposer");
    }
}
