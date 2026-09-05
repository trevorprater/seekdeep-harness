//! Dynamic scope reconciliation, cache, replay, and budget retry parity.

use std::sync::Arc;

use seekdeep_agent::{Agent, AgentOptions, Inbox, NoopInboxNotifications};
use seekdeep_agent_instructions::{
    AgentInstructionAction, AgentInstructionChange, Config, InstructionVersionCache,
    ReconcileOptions, ReconciledInstructionContext, apply_instruction_version_updates,
    baseline_instruction_state, candidate_scope_key, reconcile_instruction_context, resolve_config,
    retained_instruction_version_updates,
};
use seekdeep_core::session::{AppendOptions, Session, SessionHeader, SessionId, SurfaceOp};
use seekdeep_fs::FileSystem;
use seekdeep_fs::types::FsVersion;
use seekdeep_fs_local::{Config as LocalFsConfig, LocalFileSystem};
use seekdeep_llm::{ContentBlock, Message, MessageSource};
use seekdeep_scope::ScopeKey;

fn write(path: &std::path::Path, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn agent(id: &str, cwd: &std::path::Path) -> Arc<Agent> {
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
        seekdeep_cordis::Context::new(),
        ScopeKey::new(),
    ))
}

fn resolved(home: &std::path::Path, max_bytes: u64) -> seekdeep_agent_instructions::ResolvedConfig {
    resolve_config(&Config {
        seekdeep_home: Some(home.to_string_lossy().into_owned()),
        max_bytes,
        ..Config::default()
    })
    .unwrap()
}

fn changes(update: &ReconciledInstructionContext) -> Vec<AgentInstructionChange> {
    serde_json::from_value(update.context.source().fields["changes"].clone()).unwrap()
}

fn commit(
    agent: &Arc<Agent>,
    update: &ReconciledInstructionContext,
    cache: &InstructionVersionCache,
) {
    agent
        .session()
        .append(
            "user/message",
            serde_json::to_value(update.context.clone()).unwrap(),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .unwrap();
    apply_instruction_version_updates(agent.session(), &update.version_updates, cache);
}

async fn reconcile(
    agent: &Arc<Agent>,
    root: &std::path::Path,
    fs: &dyn FileSystem,
    cache: &InstructionVersionCache,
    max_bytes: u64,
    path: &str,
) -> Option<ReconciledInstructionContext> {
    reconcile_instruction_context(
        agent,
        &resolved(&root.join("home"), max_bytes),
        cache,
        fs,
        &ReconcileOptions {
            touched_paths: vec![path.to_owned()],
            project_root: Some(root.to_string_lossy().into_owned()),
            ..ReconcileOptions::default()
        },
    )
    .await
    .unwrap()
}

fn local_fs() -> Arc<LocalFileSystem> {
    LocalFileSystem::new(LocalFsConfig {
        cwd: Some("/".to_owned()),
        ..LocalFsConfig::default()
    })
    .unwrap()
}

#[tokio::test]
async fn touch_sets_suppresses_replaces_and_removes_one_nested_scope() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".git")).unwrap();
    write(&root.path().join("pkg/AGENTS.md"), "nested v1");
    write(&root.path().join("pkg/file.txt"), "touch");
    let fs = local_fs();
    let cache = InstructionVersionCache::default();
    let owner = agent("dynamic", root.path());

    let first = reconcile(
        &owner,
        root.path(),
        fs.as_ref(),
        &cache,
        65_536,
        "pkg/file.txt",
    )
    .await
    .unwrap();
    assert_eq!(changes(&first)[0].action, AgentInstructionAction::Set);
    assert_eq!(
        changes(&first)[0].scope,
        candidate_scope_key("pkg", "AGENTS.md")
    );
    assert!(
        first.context.content().iter().any(
            |block| matches!(block, ContentBlock::Text { text } if text.contains("nested v1"))
        )
    );
    commit(&owner, &first, &cache);
    assert!(
        reconcile(
            &owner,
            root.path(),
            fs.as_ref(),
            &cache,
            65_536,
            "pkg/file.txt"
        )
        .await
        .is_none()
    );

    write(
        &root.path().join("pkg/AGENTS.md"),
        "nested version two changed",
    );
    let replaced = reconcile(
        &owner,
        root.path(),
        fs.as_ref(),
        &cache,
        65_536,
        "pkg/file.txt",
    )
    .await
    .unwrap();
    assert_eq!(
        changes(&replaced)[0].action,
        AgentInstructionAction::Replace
    );
    commit(&owner, &replaced, &cache);

    std::fs::remove_file(root.path().join("pkg/AGENTS.md")).unwrap();
    let removed = reconcile(
        &owner,
        root.path(),
        fs.as_ref(),
        &cache,
        65_536,
        "pkg/file.txt",
    )
    .await
    .unwrap();
    assert_eq!(changes(&removed)[0].action, AgentInstructionAction::Remove);
    assert!(
        removed
            .context
            .content()
            .iter()
            .any(|block| matches!(block, ContentBlock::Text { text } if text.contains("Instructions removed")))
    );
}

#[tokio::test]
async fn tiny_unrepresentable_update_retries_when_budget_later_allows_content() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".git")).unwrap();
    write(&root.path().join("pkg/AGENTS.md"), "must eventually render");
    write(&root.path().join("pkg/file.txt"), "touch");
    let fs = local_fs();
    let cache = InstructionVersionCache::default();
    let owner = agent("budget-retry", root.path());
    assert!(
        reconcile(&owner, root.path(), fs.as_ref(), &cache, 1, "pkg/file.txt")
            .await
            .is_none()
    );
    let retried = reconcile(
        &owner,
        root.path(),
        fs.as_ref(),
        &cache,
        4096,
        "pkg/file.txt",
    )
    .await
    .unwrap();
    assert_eq!(changes(&retried)[0].action, AgentInstructionAction::Set);
}

#[tokio::test]
async fn cache_and_visible_history_are_isolated_and_replay_suppresses_duplicates() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".git")).unwrap();
    write(&root.path().join("pkg/AGENTS.md"), "shared instruction");
    write(&root.path().join("pkg/file.txt"), "touch");
    let fs = local_fs();
    let cache = InstructionVersionCache::default();
    let first_agent = agent("cache-first", root.path());
    let second_agent = agent("cache-second", root.path());
    let first = reconcile(
        &first_agent,
        root.path(),
        fs.as_ref(),
        &cache,
        4096,
        "pkg/file.txt",
    )
    .await
    .unwrap();
    let second = reconcile(
        &second_agent,
        root.path(),
        fs.as_ref(),
        &cache,
        4096,
        "pkg/file.txt",
    )
    .await
    .unwrap();
    assert_eq!(changes(&first)[0].action, AgentInstructionAction::Set);
    assert_eq!(changes(&second)[0].action, AgentInstructionAction::Set);
    commit(&first_agent, &first, &cache);

    let replay_id = SessionId::new("cache-replay");
    let mut header = SessionHeader::new(replay_id.clone());
    header.cwd = Some(root.path().to_string_lossy().into_owned());
    let replay_session = Session::create(
        &replay_id,
        Some(first_agent.session().events()),
        Some(header),
    )
    .unwrap();
    let replay_inbox =
        Arc::new(Inbox::new(replay_session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
    let replay = Arc::new(Agent::new(
        replay_id,
        AgentOptions::default(),
        replay_session,
        replay_inbox,
        seekdeep_cordis::Context::new(),
        ScopeKey::new(),
    ));
    assert!(
        reconcile(
            &replay,
            root.path(),
            fs.as_ref(),
            &cache,
            4096,
            "pkg/file.txt"
        )
        .await
        .is_none()
    );
}

#[tokio::test]
async fn compacted_visible_instruction_rearms_unchanged_scope() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".git")).unwrap();
    write(&root.path().join("pkg/AGENTS.md"), "rearm instruction");
    write(&root.path().join("pkg/file.txt"), "touch");
    let fs = local_fs();
    let cache = InstructionVersionCache::default();
    let owner = agent("rearm", root.path());
    let first = reconcile(
        &owner,
        root.path(),
        fs.as_ref(),
        &cache,
        4096,
        "pkg/file.txt",
    )
    .await
    .unwrap();
    commit(&owner, &first, &cache);
    let instruction_seq = *owner.session().surface_nodes().last().unwrap();
    owner
        .session()
        .append(
            "user/message",
            serde_json::to_value(Message::user(
                vec![ContentBlock::Text {
                    text: "replacement checkpoint".to_owned(),
                }],
                MessageSource::plugin("compact"),
            ))
            .unwrap(),
            AppendOptions {
                surface_op: Some(SurfaceOp::replace(instruction_seq, instruction_seq)),
                source_event_seqs: Some(vec![instruction_seq]),
                ..AppendOptions::default()
            },
        )
        .unwrap();
    let rearmed = reconcile(
        &owner,
        root.path(),
        fs.as_ref(),
        &cache,
        4096,
        "pkg/file.txt",
    )
    .await
    .unwrap();
    assert_eq!(changes(&rearmed)[0].action, AgentInstructionAction::Set);
}

#[test]
fn baseline_and_version_update_helpers_preserve_only_represented_state() {
    let file = seekdeep_agent_instructions::LoadedInstructionFile {
        absolute_path: "/repo/AGENTS.md".to_owned(),
        display_path: "AGENTS.md".to_owned(),
        content: "rules".to_owned(),
        version: Some(FsVersion::new("v1")),
    };
    let baseline = baseline_instruction_state(std::slice::from_ref(&file));
    let scope = candidate_scope_key(".", "AGENTS.md");
    assert_eq!(baseline.changes[&scope].action, AgentInstructionAction::Set);
    assert_eq!(baseline.versions[&scope].version, FsVersion::new("v1"));
    let update = seekdeep_agent_instructions::InstructionVersionUpdate {
        change: baseline.changes[&scope].clone(),
        state: Some(baseline.versions[&scope].clone()),
    };
    assert_eq!(
        retained_instruction_version_updates(
            std::slice::from_ref(&update),
            std::slice::from_ref(&update.change)
        )
        .len(),
        1
    );
    let unrelated = AgentInstructionChange {
        scope: "other".to_owned(),
        ..update.change.clone()
    };
    assert!(retained_instruction_version_updates(&[update], &[unrelated]).is_empty());
}
