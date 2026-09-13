//! Registry, lifecycle, persistence translation, and renamed-identity parity.

use std::{
    collections::{BTreeMap, HashMap},
    ffi::OsString,
    path::PathBuf,
    sync::Arc,
};

use async_trait::async_trait;
use indexmap::IndexMap;
use seekdeep_agent::{Agent, AgentOptions, Inbox, NoopInboxNotifications};
use seekdeep_cordis::{Context, Plugin};
use seekdeep_core::{
    preparation::SessionPreparation,
    session::{Session, SessionEvent, SessionHeader, SessionId},
    session_store::SessionStore,
};
use seekdeep_llm::{AbortSignal, CallId};
use seekdeep_scope::ScopeKey;
use seekdeep_session_persistence::{
    SessionInspection, SessionLocation, SessionPersistence, SessionPersistenceService,
    SessionPersistenceSnapshot,
};
use seekdeep_shell::SeekDeepEnvironment;
use seekdeep_shell_env::{
    SHELL_ENV, ShellEnvConfig, ShellEnvContributor, ShellEnvRegistry, ShellEnvResolvedValues,
    ShellEnvVariable, ShellEnvVariableInfo, apply, plugin,
};
use seekdeep_tools::{
    ScheduledToolPreparation, ToolExecution, ToolExecutionInput, ToolRuntime, ToolRuntimeConfig,
};
use serde_json::{Value, json};

fn variable(description: &str) -> ShellEnvVariable {
    ShellEnvVariable {
        description: description.to_owned(),
    }
}

fn contributor(
    name: &str,
    variables: impl IntoIterator<Item = (&'static str, &'static str)>,
    resolve: impl Fn(&ToolExecution) -> anyhow::Result<ShellEnvResolvedValues> + Send + Sync + 'static,
) -> ShellEnvContributor {
    ShellEnvContributor {
        name: name.to_owned(),
        variables: variables
            .into_iter()
            .map(|(key, description)| (key.to_owned(), variable(description)))
            .collect(),
        resolve: Arc::new(resolve),
    }
}

fn map(environment: &SeekDeepEnvironment) -> BTreeMap<String, String> {
    environment
        .iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

fn agent(context: &Context, id: &str) -> Arc<Agent> {
    let session = Session::create(&SessionId::new(id), None, None).expect("session");
    let inbox =
        Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).expect("inbox"));
    Arc::new(Agent::new(
        session.id().clone(),
        AgentOptions::default(),
        session,
        inbox,
        context.clone(),
        ScopeKey::new(),
    ))
}

async fn execution(context: &Context, subject: Option<Arc<Agent>>) -> ToolExecution {
    let runtime = ToolRuntime::new(context.clone(), ToolRuntimeConfig::default()).expect("tools");
    let mut input = ToolExecutionInput::new(
        CallId::new("shell-env-call"),
        "missing-fixture",
        json!({"command": "true"}),
        AbortSignal::default(),
    );
    if let Some(subject) = subject {
        input = input.with_agent(subject);
    }
    match runtime.prepare_scheduled(input).await {
        ScheduledToolPreparation::Dispatch { execution }
        | ScheduledToolPreparation::PostResult { execution, .. }
        | ScheduledToolPreparation::FinalResult { execution, .. } => execution,
    }
}

fn registry(home: &str) -> Arc<ShellEnvRegistry> {
    ShellEnvRegistry::new_with_environment(
        &ShellEnvConfig {
            seekdeep_home: Some(home.to_owned()),
        },
        &HashMap::new(),
    )
    .expect("registry")
}

#[tokio::test]
async fn collects_unconditional_facts_and_the_exact_live_agent_session_id() {
    let context = Context::new();
    let registry = registry("./test-seekdeep-home");
    let expected_home = std::env::current_dir()
        .expect("cwd")
        .join("test-seekdeep-home")
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        map(&registry
            .collect(&execution(&context, None).await)
            .expect("agentless")),
        BTreeMap::from([
            ("SEEKDEEP_HOME".to_owned(), expected_home.clone()),
            ("SEEKDEEP_SHELL".to_owned(), "1".to_owned()),
        ])
    );
    assert_eq!(
        map(&registry
            .collect(&execution(&context, Some(agent(&context, "session-a"))).await)
            .expect("agent")),
        BTreeMap::from([
            ("SEEKDEEP_HOME".to_owned(), expected_home),
            ("SEEKDEEP_SESSION_ID".to_owned(), "session-a".to_owned()),
            ("SEEKDEEP_SHELL".to_owned(), "1".to_owned()),
        ])
    );
}

#[tokio::test]
async fn resolves_home_with_explicit_then_ambient_then_default_precedence() {
    let context = Context::new();
    let cwd = std::env::current_dir().expect("cwd");
    let environment = HashMap::from([(
        OsString::from("SEEKDEEP_HOME"),
        OsString::from("./ambient-seekdeep-home"),
    )]);
    let explicit = ShellEnvRegistry::new_with_environment(
        &ShellEnvConfig {
            seekdeep_home: Some("./configured-seekdeep-home".to_owned()),
        },
        &environment,
    )
    .expect("explicit");
    let explicit_values = map(&explicit
        .collect(&execution(&context, None).await)
        .expect("explicit values"));
    assert_eq!(
        explicit_values["SEEKDEEP_HOME"],
        cwd.join("configured-seekdeep-home").to_string_lossy()
    );

    let ambient = ShellEnvRegistry::new_with_environment(&ShellEnvConfig::default(), &environment)
        .expect("ambient");
    let ambient_values = map(&ambient
        .collect(&execution(&context, None).await)
        .expect("ambient values"));
    assert_eq!(
        ambient_values["SEEKDEEP_HOME"],
        cwd.join("ambient-seekdeep-home").to_string_lossy()
    );

    let default =
        ShellEnvRegistry::new_with_environment(&ShellEnvConfig::default(), &HashMap::new())
            .expect("default");
    let default_values = map(&default
        .collect(&execution(&context, None).await)
        .expect("default values"));
    assert_eq!(
        default_values["SEEKDEEP_HOME"],
        seekdeep_util::home_paths::default_seekdeep_home()
            .expect("OS home")
            .to_string_lossy()
    );
}

#[tokio::test]
async fn collects_declared_values_omits_unavailable_ones_and_lists_without_resolving() {
    let context = Context::new();
    let registry = registry("./home");
    registry
        .register(
            &context,
            contributor(
                "optional-session-fact",
                [("SEEKDEEP_SESSION_OPTIONAL", "Optional session fact.")],
                |execution| {
                    Ok(execution
                        .agent
                        .as_ref()
                        .map_or_else(IndexMap::new, |agent| {
                            IndexMap::from([(
                                "SEEKDEEP_SESSION_OPTIONAL".to_owned(),
                                Value::String(agent.session().id().as_str().to_owned()),
                            )])
                        }))
                },
            ),
        )
        .expect("optional");
    registry
        .register(
            &context,
            contributor(
                "always-available-fact",
                [("SEEKDEEP_ALWAYS_AVAILABLE", "Always available fact.")],
                |_| {
                    Ok(IndexMap::from([(
                        "SEEKDEEP_ALWAYS_AVAILABLE".to_owned(),
                        Value::String("yes".to_owned()),
                    )]))
                },
            ),
        )
        .expect("always");

    let agentless = map(&registry
        .collect(&execution(&context, None).await)
        .expect("collect"));
    assert_eq!(agentless["SEEKDEEP_ALWAYS_AVAILABLE"], "yes");
    assert!(!agentless.contains_key("SEEKDEEP_SESSION_OPTIONAL"));
    let with_agent = map(&registry
        .collect(&execution(&context, Some(agent(&context, "session-b"))).await)
        .expect("collect"));
    assert_eq!(with_agent["SEEKDEEP_SESSION_OPTIONAL"], "session-b");
    assert_eq!(
        registry.list(),
        [
            ShellEnvVariableInfo {
                contributor: "always-available-fact".to_owned(),
                key: "SEEKDEEP_ALWAYS_AVAILABLE".to_owned(),
                description: "Always available fact.".to_owned(),
            },
            ShellEnvVariableInfo {
                contributor: "optional-session-fact".to_owned(),
                key: "SEEKDEEP_SESSION_OPTIONAL".to_owned(),
                description: "Optional session fact.".to_owned(),
            },
        ]
    );
}

#[test]
fn rejects_duplicate_key_ownership_atomically() {
    let context = Context::new();
    let registry = registry("./home");
    registry
        .register(
            &context,
            contributor("first", [("SEEKDEEP_SHARED", "First owner.")], |_| {
                Ok(IndexMap::new())
            }),
        )
        .expect("first");
    let error = registry
        .register(
            &context,
            contributor("second", [("SEEKDEEP_SHARED", "Second owner.")], |_| {
                Ok(IndexMap::new())
            }),
        )
        .expect_err("duplicate key");
    assert_eq!(
        format!("{error:#}"),
        "bash env key \"SEEKDEEP_SHARED\" is already owned by contributor \"first\"; contributor \"second\" cannot also own it"
    );
    assert_eq!(registry.list().len(), 1);
}

#[test]
fn rejects_duplicate_names_and_every_malformed_declaration() {
    let context = Context::new();
    let registry = registry("./home");
    registry
        .register(
            &context,
            contributor("declared", [("SEEKDEEP_DECLARED", "Declared.")], |_| {
                Ok(IndexMap::new())
            }),
        )
        .expect("declared");
    let cases = [
        contributor("declared", [("SEEKDEEP_ANOTHER", "Another.")], |_| {
            Ok(IndexMap::new())
        }),
        contributor(" ", [("SEEKDEEP_BLANK_NAME", "Blank.")], |_| {
            Ok(IndexMap::new())
        }),
        contributor("invalid-key", [("seekdeep_invalid", "Invalid.")], |_| {
            Ok(IndexMap::new())
        }),
        contributor("reserved-home", [("SEEKDEEP_HOME", "Reserved.")], |_| {
            Ok(IndexMap::new())
        }),
        contributor("reserved-shell", [("SEEKDEEP_SHELL", "Reserved.")], |_| {
            Ok(IndexMap::new())
        }),
        contributor(
            "reserved-session",
            [("SEEKDEEP_SESSION_ID", "Reserved.")],
            |_| Ok(IndexMap::new()),
        ),
        contributor(
            "blank-description",
            [("SEEKDEEP_BLANK_DESCRIPTION", " ")],
            |_| Ok(IndexMap::new()),
        ),
    ];
    let expected = [
        "already registered",
        "name must be non-empty",
        "invalid key",
        "reserved key",
        "reserved key",
        "reserved key",
        "must describe",
    ];
    for (case, expected) in cases.into_iter().zip(expected) {
        let error = registry.register(&context, case).expect_err("invalid");
        assert!(format!("{error:#}").contains(expected));
    }
}

#[tokio::test]
async fn rejects_undeclared_keys_and_non_string_values_at_collection_time() {
    let context = Context::new();
    let undeclared = registry("./home");
    undeclared
        .register(
            &context,
            contributor("drifted", [("SEEKDEEP_DECLARED", "Declared.")], |_| {
                Ok(IndexMap::from([(
                    "SEEKDEEP_UNDECLARED".to_owned(),
                    Value::String("bad".to_owned()),
                )]))
            }),
        )
        .expect("register");
    let execution = execution(&context, None).await;
    assert_eq!(
        format!(
            "{:#}",
            undeclared.collect(&execution).expect_err("undeclared")
        ),
        "bash env contributor \"drifted\" returned undeclared key \"SEEKDEEP_UNDECLARED\""
    );

    let wrong_type = registry("./home");
    wrong_type
        .register(
            &context,
            contributor("wrong-type", [("SEEKDEEP_STRING", "String.")], |_| {
                Ok(IndexMap::from([("SEEKDEEP_STRING".to_owned(), json!(42))]))
            }),
        )
        .expect("register");
    assert_eq!(
        format!("{:#}", wrong_type.collect(&execution).expect_err("type")),
        "bash env contributor \"wrong-type\" returned a non-string value for \"SEEKDEEP_STRING\""
    );
}

#[tokio::test]
async fn contributor_registration_is_effect_scoped_and_explicitly_disposable() {
    let context = Context::new();
    let registry = registry("./home");
    registry.provide(&context).expect("provide");
    let temporary = context
        .plugin(
            Plugin::new("temporary-env", ["shellEnv"], |inner, _| {
                Box::pin(async move {
                    inner.get(SHELL_ENV).expect("registry").register(
                        &inner,
                        contributor("temporary", [("SEEKDEEP_TEMPORARY", "Temporary.")], |_| {
                            Ok(IndexMap::from([(
                                "SEEKDEEP_TEMPORARY".to_owned(),
                                Value::String("present".to_owned()),
                            )]))
                        }),
                    )?;
                    Ok(())
                })
            }),
            json!({}),
        )
        .expect("mount");
    temporary.await_settled().await.expect("settled");
    let execution = execution(&context, None).await;
    assert_eq!(
        map(&registry.collect(&execution).expect("collect"))["SEEKDEEP_TEMPORARY"],
        "present"
    );
    temporary.dispose().await.expect("dispose plugin");
    assert!(
        !map(&registry.collect(&execution).expect("collect")).contains_key("SEEKDEEP_TEMPORARY")
    );

    let explicit = registry
        .register(
            &context,
            contributor("explicit", [("SEEKDEEP_EXPLICIT", "Explicit.")], |_| {
                Ok(IndexMap::from([(
                    "SEEKDEEP_EXPLICIT".to_owned(),
                    Value::String("present".to_owned()),
                )]))
            }),
        )
        .expect("explicit");
    assert!(map(&registry.collect(&execution).expect("collect")).contains_key("SEEKDEEP_EXPLICIT"));
    explicit.dispose().await.expect("explicit dispose");
    assert!(
        !map(&registry.collect(&execution).expect("collect")).contains_key("SEEKDEEP_EXPLICIT")
    );
}

#[derive(Debug)]
struct LocatedPersistence {
    location: Option<SessionLocation>,
}

#[async_trait]
impl SessionPersistence for LocatedPersistence {
    fn locate(&self, _meta: &SessionHeader) -> Option<SessionLocation> {
        self.location.clone()
    }

    fn supports_raw_artifacts(&self) -> bool {
        false
    }

    async fn create(&self, _meta: &SessionHeader) -> anyhow::Result<()> {
        anyhow::bail!("unused")
    }

    async fn append(&self, _id: &SessionId, _events: &[SessionEvent]) -> anyhow::Result<()> {
        anyhow::bail!("unused")
    }

    async fn load(&self, _id: &SessionId) -> anyhow::Result<SessionInspection> {
        anyhow::bail!("unused")
    }

    async fn inspect(
        &self,
        _id: &SessionId,
        _signal: Option<AbortSignal>,
    ) -> anyhow::Result<SessionInspection> {
        anyhow::bail!("unused")
    }

    async fn read_from(
        &self,
        _id: &SessionId,
        _from_seq: u64,
        _signal: Option<AbortSignal>,
    ) -> anyhow::Result<SessionInspection> {
        anyhow::bail!("unused")
    }

    async fn list(&self, _signal: Option<AbortSignal>) -> anyhow::Result<Vec<SessionHeader>> {
        anyhow::bail!("unused")
    }

    async fn list_snapshots(
        &self,
        _signal: Option<AbortSignal>,
    ) -> anyhow::Result<Vec<SessionPersistenceSnapshot>> {
        anyhow::bail!("unused")
    }

    async fn prepare(
        &self,
        _sessions: &Arc<SessionStore>,
        _id: &SessionId,
        _signal: Option<AbortSignal>,
    ) -> anyhow::Result<SessionPreparation> {
        anyhow::bail!("unused")
    }
}

#[tokio::test]
async fn plugin_service_and_persistence_contributor_follow_live_optional_backend_state() {
    let context = Context::new();
    let mounted = context.plugin(plugin(), Value::Null).expect("mount plugin");
    mounted.await_settled().await.expect("settled");
    assert_eq!(plugin().name(), "shell-env");
    assert!(plugin().inject().is_empty());
    let registry = context.get(SHELL_ENV).expect("shellEnv service");
    assert_eq!(
        registry.list(),
        [ShellEnvVariableInfo {
            contributor: "session-persistence".to_owned(),
            key: "SEEKDEEP_SESSION_JSONL".to_owned(),
            description: "Absolute target path of the current session JSONL when the active persistence backend provides one.".to_owned(),
        }]
    );
    let execution = execution(&context, Some(agent(&context, "sess-p"))).await;
    assert!(
        !map(&registry.collect(&execution).expect("no backend"))
            .contains_key("SEEKDEEP_SESSION_JSONL")
    );

    let sqlite = SessionPersistenceService::new(Arc::new(LocatedPersistence {
        location: Some(SessionLocation {
            kind: "sqlite".to_owned(),
            path: PathBuf::from("C:\\sessions\\s.db"),
        }),
    }));
    let sqlite_effect = sqlite.provide(&context).expect("sqlite provider");
    assert!(
        !map(&registry.collect(&execution).expect("sqlite")).contains_key("SEEKDEEP_SESSION_JSONL")
    );
    sqlite_effect.dispose().await.expect("remove sqlite");

    let jsonl = SessionPersistenceService::new(Arc::new(LocatedPersistence {
        location: Some(SessionLocation {
            kind: "jsonl".to_owned(),
            path: PathBuf::from("C:\\sessions\\s.jsonl"),
        }),
    }));
    jsonl.provide(&context).expect("jsonl provider");
    assert_eq!(
        map(&registry.collect(&execution).expect("jsonl"))["SEEKDEEP_SESSION_JSONL"],
        "C:\\sessions\\s.jsonl"
    );

    mounted.dispose().await.expect("dispose plugin");
    assert!(context.get(SHELL_ENV).is_none());
}

#[test]
fn apply_directly_installs_the_same_service_and_contributor() {
    let context = Context::new();
    let registry = apply(
        &context,
        &ShellEnvConfig {
            seekdeep_home: Some("./direct-home".to_owned()),
        },
    )
    .expect("apply");
    assert!(Arc::ptr_eq(
        &registry,
        &context.get(SHELL_ENV).expect("service")
    ));
    assert_eq!(registry.list()[0].contributor, "session-persistence");
}

#[tokio::test]
async fn loader_config_and_invariant_lifecycles_fail_early_and_rollback() {
    let context = Context::new();
    let invalid = context
        .plugin(plugin(), json!({"seekdeepHome": null}))
        .expect("invalid mount");
    let error = invalid.await_settled().await.expect_err("invalid config");
    assert_eq!(
        format!("{error:#}"),
        "$.seekdeepHome expected string but got null"
    );
    assert!(context.get(SHELL_ENV).is_none());

    let registry = seekdeep_invariants::InvariantRegistry::install(
        &context,
        &seekdeep_invariants::InvariantConfig::default(),
    )
    .expect("invariants");
    let registration =
        seekdeep_shell_env::invariant::register_invariant(&registry).expect("registration");
    registration.await_ready().await.expect("ready");
    assert!(registry.is_registered("seekdeep-shell-env"));
    registration.dispose().await.expect("dispose");
    assert!(!registry.is_registered("seekdeep-shell-env"));
}
