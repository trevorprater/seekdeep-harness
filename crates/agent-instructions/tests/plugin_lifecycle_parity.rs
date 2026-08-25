//! Pre-step baseline ordering, optional filesystem, isolation, and disposal.

use std::sync::Arc;

use seekdeep_agent::{
    Agent, AgentEvents, AgentOptions, Inbox, NoopInboxNotifications, PreStepDecision,
};
use seekdeep_agent_instructions::{INJECT, plugin};
use seekdeep_agent_loop::AgentPreStepEvent;
use seekdeep_core::session::{Session, SessionHeader, SessionId};
use seekdeep_fs_local::{Config as LocalFsConfig, LocalFileSystem};
use seekdeep_llm::{AbortSignal, ContentBlock, MessageSource, UserMessage};
use seekdeep_scope::ScopeKey;
use serde_json::json;

fn write(path: &std::path::Path, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn agent(id: &str, cwd: &std::path::Path, context: &seekdeep_cordis::Context) -> Arc<Agent> {
    let id = SessionId::new(id);
    let mut header = SessionHeader::new(id.clone());
    header.cwd = Some(cwd.to_string_lossy().into_owned());
    let session = Session::create(&id, None, Some(header)).unwrap();
    let inbox =
        Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).expect("inbox"));
    Arc::new(Agent::new(
        id,
        AgentOptions::default(),
        session,
        inbox,
        context.clone(),
        ScopeKey::new(),
    ))
}

fn prompt(text: &str) -> UserMessage {
    UserMessage::new(
        vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        MessageSource::user(),
    )
}

async fn pre_step(
    context: &seekdeep_cordis::Context,
    agent: &Arc<Agent>,
    claimed: Vec<UserMessage>,
) -> PreStepDecision {
    let downstream = claimed.clone();
    AgentEvents::new(context.clone(), agent.clone())
        .waterfall(
            "agent/pre-step",
            AgentPreStepEvent {
                messages: claimed,
                turn: 1,
                step: 1,
                signal: AbortSignal::default(),
            },
            move || async move {
                Ok(PreStepDecision::Enter {
                    messages: downstream,
                })
            },
        )
        .await
        .unwrap()
}

fn instruction_text(decision: &PreStepDecision) -> Vec<String> {
    let PreStepDecision::Enter { messages } = decision else {
        return Vec::new();
    };
    messages
        .iter()
        .flat_map(|message| message.content())
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn plugin_has_no_static_fs_dependency() {
    assert!(INJECT.is_empty());
    assert!(plugin().inject().is_empty());
}

#[tokio::test]
async fn providerless_and_zero_budget_paths_leave_request_unchanged() {
    for (name, install_fs, max_bytes) in
        [("providerless", false, 4096_u64), ("disabled", true, 0_u64)]
    {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(".git")).unwrap();
        write(&root.path().join("AGENTS.md"), "must not enter");
        let context = seekdeep_cordis::Context::new();
        if install_fs {
            LocalFileSystem::install(
                &context,
                LocalFsConfig {
                    cwd: Some("/".to_owned()),
                    ..LocalFsConfig::default()
                },
            )
            .unwrap();
        }
        let mounted = context
            .plugin(
                plugin(),
                json!({
                    "dshHome": root.path().join("home"),
                    "maxBytes": max_bytes
                }),
            )
            .unwrap();
        mounted.await_settled().await.unwrap();
        let owner = agent(name, root.path(), &context);
        let claimed = prompt("direct prompt");
        assert_eq!(
            pre_step(&context, &owner, vec![claimed.clone()]).await,
            PreStepDecision::Enter {
                messages: vec![claimed]
            }
        );
        context.fiber().dispose().await.unwrap();
    }
}

#[tokio::test]
async fn baseline_enters_immediately_after_claimed_prompt_and_local_overlay() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".git")).unwrap();
    write(&root.path().join("AGENTS.md"), "base instruction");
    write(&root.path().join("AGENTS.local.md"), "local instruction");
    let context = seekdeep_cordis::Context::new();
    LocalFileSystem::install(
        &context,
        LocalFsConfig {
            cwd: Some("/".to_owned()),
            ..LocalFsConfig::default()
        },
    )
    .unwrap();
    let mounted = context
        .plugin(
            plugin(),
            json!({"dshHome": root.path().join("home"), "maxBytes": 65536}),
        )
        .unwrap();
    mounted.await_settled().await.unwrap();
    let owner = agent("ordered", root.path(), &context);
    let decision = pre_step(&context, &owner, vec![prompt("claimed")]).await;
    let PreStepDecision::Enter { messages } = &decision else {
        panic!("must enter");
    };
    assert_eq!(messages.len(), 2);
    assert_eq!(instruction_text(&decision)[0], "claimed");
    assert!(instruction_text(&decision)[1].contains("base instruction"));
    assert!(instruction_text(&decision)[1].contains("local instruction"));
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn session_cwds_are_isolated_and_listener_disposal_is_exact() {
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("first");
    let second = root.path().join("second");
    for (dir, text) in [(&first, "first-only"), (&second, "second-only")] {
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        write(&dir.join("AGENTS.md"), text);
    }
    let context = seekdeep_cordis::Context::new();
    LocalFileSystem::install(
        &context,
        LocalFsConfig {
            cwd: Some("/".to_owned()),
            ..LocalFsConfig::default()
        },
    )
    .unwrap();
    let mounted = context
        .plugin(
            plugin(),
            json!({"dshHome": root.path().join("home"), "maxBytes": 65536}),
        )
        .unwrap();
    mounted.await_settled().await.unwrap();
    let one = pre_step(
        &context,
        &agent("first", &first, &context),
        vec![prompt("one")],
    )
    .await;
    let two = pre_step(
        &context,
        &agent("second", &second, &context),
        vec![prompt("two")],
    )
    .await;
    let one = instruction_text(&one).join("\n");
    let two = instruction_text(&two).join("\n");
    assert!(one.contains("first-only"));
    assert!(!one.contains("second-only"));
    assert!(two.contains("second-only"));
    assert!(!two.contains("first-only"));

    mounted.dispose().await.unwrap();
    let claimed = prompt("after dispose");
    assert_eq!(
        pre_step(
            &context,
            &agent("disposed", &first, &context),
            vec![claimed.clone()]
        )
        .await,
        PreStepDecision::Enter {
            messages: vec![claimed]
        }
    );
    context.fiber().dispose().await.unwrap();
}
