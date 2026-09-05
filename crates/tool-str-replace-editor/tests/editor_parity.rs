//! Real filesystem tool registration, presentation, editing, policy, and errors.

use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;
use parking_lot::Mutex;
use seekdeep_agent::{Agent, AgentOptions, Inbox, NoopInboxNotifications};
use seekdeep_cordis::Context;
use seekdeep_core::session::{Session, SessionId};
use seekdeep_fs::{
    FileSystem, FileSystemService, FsDirEntry, FsEditOutcome, FsEditRequest, FsError, FsErrorCode,
    FsInfo, FsPathInfo, FsTarget, FsVersion, FsWriteIntent, FsWriteOutcome,
};
use seekdeep_fs_local::{Config as LocalConfig, LocalFileSystem};
use seekdeep_llm::{AbortSignal, CallId, ContentBlock};
use seekdeep_sandbox::{SandboxExecutionPolicy, SandboxMode};
use seekdeep_sandbox_policy::{SandboxPolicyConfig, SandboxPolicyService};
use seekdeep_scope::ScopeKey;
use seekdeep_system_prompt::{SystemPromptConfig, install as install_prompt};
use seekdeep_tool_str_replace_editor::{Config, apply, plugin};
use seekdeep_tools::{
    TOOLS, ToolCallView, ToolExecutionInput, ToolExecutionResult, ToolRuntimeConfig,
};
use serde_json::{Value, json};

#[derive(Clone, Copy)]
enum WriteFailure {
    Denied,
    Generic,
}

struct WrappedFileSystem {
    inner: Arc<dyn FileSystem>,
    mode: Option<SandboxMode>,
    failure: Mutex<WriteFailure>,
    policies: Mutex<Vec<Option<SandboxMode>>>,
}

#[async_trait]
impl FileSystem for WrappedFileSystem {
    fn sandbox_mode(&self) -> Option<SandboxMode> {
        self.mode
    }

    async fn resolve(
        &self,
        path: &str,
        cwd: Option<&str>,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<FsTarget> {
        self.inner.resolve(path, cwd, signal).await
    }

    fn process_path(&self, target: &FsTarget) -> String {
        self.inner.process_path(target)
    }

    fn file_url(&self, target: &FsTarget) -> String {
        self.inner.file_url(target)
    }

    fn contains(&self, parent: &FsTarget, child: &FsTarget) -> bool {
        self.inner.contains(parent, child)
    }

    async fn stat(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Option<FsInfo>> {
        self.inner.stat(target, signal).await
    }

    async fn lstat(
        &self,
        path: &str,
        cwd: Option<&str>,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Option<FsPathInfo>> {
        self.inner.lstat(path, cwd, signal).await
    }

    async fn read_text(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<String> {
        self.inner.read_text(target, signal).await
    }

    async fn stream_text(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<BoxStream<'static, anyhow::Result<String>>> {
        self.inner.stream_text(target, signal).await
    }

    async fn read_bytes(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
        max_bytes: usize,
    ) -> anyhow::Result<Vec<u8>> {
        self.inner.read_bytes(target, signal, max_bytes).await
    }

    async fn list_dir(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Vec<FsDirEntry>> {
        self.inner.list_dir(target, signal).await
    }

    async fn write_text(
        &self,
        _target: &FsTarget,
        _content: &str,
        _expected: Option<&FsWriteIntent>,
        _signal: Option<&AbortSignal>,
        policy: Option<&SandboxExecutionPolicy>,
    ) -> anyhow::Result<FsWriteOutcome> {
        self.policies.lock().push(policy.map(|policy| policy.mode));
        let failure = *self.failure.lock();
        match failure {
            WriteFailure::Denied => Err(anyhow::Error::new(FsError::new(
                "provider denied",
                FsErrorCode::FsSandboxDenied,
            ))),
            WriteFailure::Generic => anyhow::bail!("backend write failed"),
        }
    }

    async fn edit_text(
        &self,
        target: &FsTarget,
        edit: &FsEditRequest,
        expected: Option<&FsVersion>,
        signal: Option<&AbortSignal>,
        policy: Option<&SandboxExecutionPolicy>,
    ) -> anyhow::Result<FsEditOutcome> {
        self.inner
            .edit_text(target, edit, expected, signal, policy)
            .await
    }
}

struct Harness {
    context: Context,
    root: tempfile::TempDir,
    owner: Arc<Agent>,
    plugin: Arc<seekdeep_cordis::PluginFiber>,
}

fn build_owner() -> anyhow::Result<Arc<Agent>> {
    let session = Session::create(&SessionId::new("editor-owner"), None, None)?;
    let inbox = Arc::new(Inbox::new(
        session.clone(),
        Arc::new(NoopInboxNotifications),
    )?);
    Ok(Arc::new(Agent::new(
        session.id().clone(),
        AgentOptions::default(),
        session,
        inbox,
        Context::new(),
        ScopeKey::new(),
    )))
}

impl Harness {
    async fn new(config: Value, observation_policy: bool) -> anyhow::Result<Self> {
        let context = Context::new();
        let prompt = install_prompt(&context, SystemPromptConfig::default())?;
        seekdeep_tools::install(&context, &prompt, ToolRuntimeConfig::default())?;
        let root = tempfile::tempdir()?;
        LocalFileSystem::install(
            &context,
            LocalConfig {
                cwd: Some(root.path().to_string_lossy().into_owned()),
                ..LocalConfig::default()
            },
        )?;
        if observation_policy {
            seekdeep_fs_observation_policy::apply(&context)?;
        }
        let plugin = context.plugin(plugin(), config)?;
        plugin.await_settled().await?;
        let owner = build_owner()?;
        Ok(Self {
            context,
            root,
            owner,
            plugin,
        })
    }

    async fn call(&self, args: Value, owner: bool) -> ToolExecutionResult {
        call_editor(&self.context, owner.then(|| self.owner.clone()), args).await
    }
}

async fn call_editor(
    context: &Context,
    owner: Option<Arc<Agent>>,
    args: Value,
) -> ToolExecutionResult {
    let mut input = ToolExecutionInput::new(
        CallId::new("editor-call"),
        "str_replace_editor",
        args,
        AbortSignal::default(),
    );
    if let Some(owner) = owner {
        input = input.with_agent(owner);
    }
    context.get(TOOLS).unwrap().execute(input).await
}

fn text(result: &ToolExecutionResult) -> &str {
    let ContentBlock::Text { text } = &result.content()[0] else {
        panic!("expected text output")
    };
    text
}

fn error_code(result: &ToolExecutionResult) -> Option<&str> {
    result.error()?.info.as_ref().map(|info| info.code.as_str())
}

#[tokio::test]
async fn registers_schema_description_presenters_and_disposes_every_contribution()
-> anyhow::Result<()> {
    let harness = Harness::new(
        json!({"maxOutputChars": 123, "description": "Custom editor"}),
        false,
    )
    .await?;
    let tools = harness.context.get(TOOLS).unwrap();
    let definition = tools.get("str_replace_editor", None).unwrap();
    assert_eq!(definition.description, "Custom editor");
    let schema = tools
        .schemas(None)
        .into_iter()
        .find(|schema| schema.name == "str_replace_editor")
        .unwrap();
    assert_eq!(
        schema.parameters["properties"]["command"]["enum"],
        json!(["view", "create", "str_replace", "insert"])
    );
    let present = definition.present_call.as_ref().unwrap();
    assert!(matches!(
        present(&json!({"command":"view","path":"/workspace/a.txt"})),
        Some(ToolCallView::Generic(_))
    ));
    assert!(matches!(
        present(&json!({"command":"create","path":"/workspace/empty.txt"})),
        Some(ToolCallView::Diff(_))
    ));
    harness.plugin.dispose().await?;
    assert!(tools.get("str_replace_editor", None).is_none());
    assert!(tools.schemas(None).is_empty());
    Ok(())
}

#[tokio::test]
async fn creates_views_replaces_deletes_inserts_and_preserves_literal_tabs() -> anyhow::Result<()> {
    let harness = Harness::new(json!({}), false).await?;
    let sample = harness.root.path().join("sample.txt");
    let sample_text = sample.to_string_lossy();
    let created = harness
        .call(
            json!({"command":"create","path":sample_text,"file_text":"one\ntwo\nthree\n"}),
            true,
        )
        .await;
    assert!(!created.is_error());
    assert_eq!(
        text(&created),
        format!("New file created successfully at: {sample_text}")
    );
    let viewed = harness
        .call(
            json!({"command":"view","path":sample_text,"view_range":[2,-1]}),
            true,
        )
        .await;
    assert_eq!(
        text(&viewed),
        format!(
            "Here's the content of {sample_text} with line numbers (which has a total of 4 lines) with view_range=[2, -1]:\n     2  two\n     3  three\n     4  \n"
        )
    );
    for args in [
        json!({"command":"str_replace","path":sample_text,"old_str":"two","new_str":"TWO"}),
        json!({"command":"str_replace","path":sample_text,"old_str":"TWO"}),
        json!({"command":"insert","path":sample_text,"insert_line":1,"new_str":"between"}),
    ] {
        assert!(!harness.call(args, true).await.is_error());
    }
    assert_eq!(
        tokio::fs::read_to_string(&sample).await?,
        "one\nbetween\n\nthree\n"
    );

    let makefile = harness.root.path().join("Makefile");
    tokio::fs::write(&makefile, "target:\n\told\nremove\n").await?;
    let path = makefile.to_string_lossy();
    assert!(
        text(
            &harness
                .call(json!({"command":"view","path":path}), true)
                .await
        )
        .contains("     2  \told")
    );
    for args in [
        json!({"command":"str_replace","path":path,"old_str":"\told","new_str":"\tnew"}),
        json!({"command":"str_replace","path":path,"old_str":"remove\n"}),
        json!({"command":"insert","path":path,"insert_line":1,"new_str":"\tkept"}),
    ] {
        assert!(!harness.call(args, true).await.is_error());
    }
    assert_eq!(
        tokio::fs::read_to_string(makefile).await?,
        "target:\n\tkept\n\tnew\n"
    );
    Ok(())
}

#[tokio::test]
async fn empty_newline_end_insert_mixed_eol_and_ownerless_calls_match_canonical_edges()
-> anyhow::Result<()> {
    let harness = Harness::new(json!({}), false).await?;
    let empty = harness.root.path().join("empty.txt");
    let newline = harness.root.path().join("newline.txt");
    let plain = harness.root.path().join("plain.txt");
    tokio::fs::write(&empty, "").await?;
    tokio::fs::write(&newline, "\n").await?;
    tokio::fs::write(&plain, "one\ntwo").await?;
    assert!(
        text(
            &harness
                .call(json!({"command":"view","path":empty}), true)
                .await
        )
        .contains("(which has a total of 1 lines):\n     1  \n")
    );
    assert!(
        text(
            &harness
                .call(json!({"command":"view","path":newline}), true)
                .await
        )
        .contains("(which has a total of 2 lines):\n     1  \n     2  \n")
    );
    assert!(
        text(
            &harness
                .call(json!({"command":"view","path":plain}), false)
                .await
        )
        .contains("     1  one")
    );
    let ownerless = harness.root.path().join("ownerless.txt");
    assert!(
        !harness
            .call(
                json!({"command":"create","path":ownerless,"file_text":"ownerless"}),
                false,
            )
            .await
            .is_error()
    );
    assert!(
        !harness
            .call(
                json!({"command":"insert","path":plain,"insert_line":2,"new_str":"three"}),
                true,
            )
            .await
            .is_error()
    );
    assert_eq!(tokio::fs::read_to_string(&plain).await?, "one\ntwo\nthree");
    tokio::fs::write(&newline, "one\n").await?;
    assert!(
        !harness
            .call(
                json!({"command":"insert","path":newline,"insert_line":2,"new_str":"three"}),
                true,
            )
            .await
            .is_error()
    );
    assert_eq!(tokio::fs::read_to_string(&newline).await?, "one\n\nthree");

    let mixed = harness.root.path().join("mixed-eol.txt");
    tokio::fs::write(&mixed, "alpha\r\nbeta\nmiddle\nalpha\nbeta").await?;
    assert!(
        !harness
            .call(
                json!({"command":"str_replace","path":mixed,"old_str":"alpha\r\nbeta","new_str":"replaced"}),
                true,
            )
            .await
            .is_error()
    );
    assert_eq!(
        tokio::fs::read_to_string(mixed).await?,
        "replaced\nmiddle\nalpha\nbeta"
    );
    Ok(())
}

#[tokio::test]
async fn directory_view_filters_depth_sorts_and_clips_file_output() -> anyhow::Result<()> {
    let harness = Harness::new(json!({"maxOutputChars":10000}), false).await?;
    let root = harness.root.path().join("dir");
    for directory in [
        "nested/third",
        "node_modules/pkg",
        "node_modules_old",
        "__pycache__",
        "__pycache__backup",
    ] {
        tokio::fs::create_dir_all(root.join(directory)).await?;
    }
    for (path, content) in [
        ("visible.txt", "ok"),
        (".hidden", "hidden"),
        ("nested/child.txt", "child"),
        ("nested/third/too-deep.txt", "deep"),
        ("node_modules/pkg/index.js", "dependency"),
        ("node_modules_old/kept.js", "source"),
        ("__pycache__/module.pyc", "cache"),
        ("__pycache__backup/kept.py", "source"),
    ] {
        tokio::fs::write(root.join(path), content).await?;
    }
    let listing = harness
        .call(json!({"command":"view","path":root}), true)
        .await;
    let listing = text(&listing);
    assert!(!listing.contains(".hidden"));
    assert!(!listing.contains("too-deep.txt"));
    assert!(!listing.contains("index.js"));
    assert!(!listing.contains("module.pyc"));
    assert!(
        listing.contains(
            &root
                .join("node_modules_old/kept.js")
                .to_string_lossy()
                .to_string()
        )
    );
    assert!(
        listing.contains(
            &root
                .join("__pycache__backup/kept.py")
                .to_string_lossy()
                .to_string()
        )
    );

    let clipped = Harness::new(json!({"maxOutputChars":10}), false).await?;
    let large = clipped.root.path().join("large.txt");
    tokio::fs::write(&large, "x".repeat(100)).await?;
    assert!(
        text(
            &clipped
                .call(json!({"command":"view","path":large}), true)
                .await
        )
        .contains("<response clipped>")
    );
    Ok(())
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn invalid_ranges_commands_targets_and_literal_matches_never_mutate() -> anyhow::Result<()> {
    let harness = Harness::new(json!({}), false).await?;
    let ambiguous = harness.root.path().join("ambiguous.txt");
    let empty = harness.root.path().join("empty.txt");
    let three = harness.root.path().join("three.txt");
    let directory = harness.root.path().join("directory");
    tokio::fs::write(&ambiguous, "same\nother\nsame").await?;
    tokio::fs::write(&empty, "").await?;
    tokio::fs::write(&three, "one\ntwo\nthree").await?;
    tokio::fs::create_dir(&directory).await?;

    let missing = harness
        .call(
            json!({"command":"str_replace","path":ambiguous,"old_str":"absent","new_str":"x"}),
            true,
        )
        .await;
    assert!(missing.is_error());
    assert_eq!(error_code(&missing), Some("FS_EDIT_NOT_FOUND"));
    assert!(text(&missing).contains("old_str `absent`"));
    let repeated = harness
        .call(
            json!({"command":"str_replace","path":ambiguous,"old_str":"same","new_str":"x"}),
            true,
        )
        .await;
    assert_eq!(error_code(&repeated), Some("FS_AMBIGUOUS_EDIT"));
    assert!(text(&repeated).contains("lines [1, 3]"));
    assert_eq!(
        tokio::fs::read_to_string(&ambiguous).await?,
        "same\nother\nsame"
    );

    let invalid = vec![
        json!({"command":"view","path":""}),
        json!({"command":"view","path":"relative.txt"}),
        json!({"command":"view","path":ambiguous,"view_range":[1]}),
        json!({"command":"view","path":ambiguous,"view_range":[0,1]}),
        json!({"command":"view","path":ambiguous,"view_range":[1.5,2]}),
        json!({"command":"view","path":three,"view_range":[1,99]}),
        json!({"command":"view","path":three,"view_range":[2,1]}),
        json!({"command":"view","path":directory,"view_range":[1,1]}),
        json!({"command":"create","path":harness.root.path().join("new.txt")}),
        json!({"command":"create","path":ambiguous,"file_text":"overwrite"}),
        json!({"command":"str_replace","path":ambiguous,"new_str":"x"}),
        json!({"command":"str_replace","path":ambiguous,"old_str":"","new_str":"x"}),
        json!({"command":"insert","path":ambiguous,"new_str":"x"}),
        json!({"command":"insert","path":ambiguous,"insert_line":-1,"new_str":"x"}),
        json!({"command":"insert","path":ambiguous,"insert_line":1.5,"new_str":"x"}),
        json!({"command":"insert","path":ambiguous,"insert_line":99,"new_str":"x"}),
        json!({"command":"insert","path":empty,"insert_line":2,"new_str":"x"}),
        json!({"command":"insert","path":directory,"insert_line":0,"new_str":"x"}),
    ];
    for args in invalid {
        assert!(harness.call(args, true).await.is_error());
    }
    assert_eq!(
        tokio::fs::read_to_string(&ambiguous).await?,
        "same\nother\nsame"
    );
    Ok(())
}

#[tokio::test]
async fn observation_policy_requires_read_but_absence_allows_recovery_create() -> anyhow::Result<()>
{
    let harness = Harness::new(json!({}), true).await?;
    let existing = harness.root.path().join("existing.txt");
    tokio::fs::write(&existing, "before").await?;
    let blind = harness
        .call(
            json!({"command":"str_replace","path":existing,"old_str":"before","new_str":"after"}),
            true,
        )
        .await;
    assert_eq!(error_code(&blind), Some("FS_NOT_OBSERVED"));
    assert_eq!(tokio::fs::read_to_string(&existing).await?, "before");
    assert!(
        !harness
            .call(json!({"command":"view","path":existing}), true)
            .await
            .is_error()
    );
    assert!(!harness.call(json!({"command":"str_replace","path":existing,"old_str":"before","new_str":"after"}), true).await.is_error());
    assert!(
        !harness
            .call(
                json!({"command":"insert","path":existing,"insert_line":1,"new_str":"tail"}),
                true,
            )
            .await
            .is_error()
    );

    tokio::fs::remove_file(&existing).await?;
    let missing = harness
        .call(json!({"command":"view","path":existing}), true)
        .await;
    assert_eq!(error_code(&missing), Some("FS_NOT_FOUND"));
    let created = harness
        .call(
            json!({"command":"create","path":existing,"file_text":"fresh"}),
            true,
        )
        .await;
    assert!(!created.is_error());
    assert_eq!(tokio::fs::read_to_string(existing).await?, "fresh");
    Ok(())
}

#[test]
fn invalid_direct_config_fails_before_service_lookup() {
    let context = Context::new();
    assert!(
        apply(
            &context,
            &Config {
                max_output_chars: Some(0.0),
                ..Config::default()
            }
        )
        .unwrap_err()
        .to_string()
        .contains("positive safe integer")
    );
    assert!(
        apply(
            &context,
            &Config {
                description: Some(" ".to_owned()),
                ..Config::default()
            }
        )
        .unwrap_err()
        .to_string()
        .contains("description must be non-empty")
    );
}

#[tokio::test]
async fn confining_filesystem_requires_policy_and_maps_denials_for_owned_and_ownerless_calls()
-> anyhow::Result<()> {
    let missing = Context::new();
    let prompt = install_prompt(&missing, SystemPromptConfig::default())?;
    seekdeep_tools::install(&missing, &prompt, ToolRuntimeConfig::default())?;
    let root = tempfile::tempdir()?;
    let local = LocalFileSystem::new(LocalConfig {
        cwd: Some(root.path().to_string_lossy().into_owned()),
        ..LocalConfig::default()
    })?;
    let wrapped = Arc::new(WrappedFileSystem {
        inner: local,
        mode: Some(SandboxMode::ReadOnly),
        failure: Mutex::new(WriteFailure::Denied),
        policies: Mutex::new(Vec::new()),
    });
    FileSystemService::new(wrapped.clone()).provide(&missing)?;
    let fiber = missing.plugin(plugin(), json!({}))?;
    assert!(
        fiber
            .await_settled()
            .await
            .unwrap_err()
            .to_string()
            .contains("ctx.sandboxPolicy is missing")
    );
    fiber.dispose().await?;

    let context = Context::new();
    let prompt = install_prompt(&context, SystemPromptConfig::default())?;
    seekdeep_tools::install(&context, &prompt, ToolRuntimeConfig::default())?;
    FileSystemService::new(wrapped.clone()).provide(&context)?;
    SandboxPolicyService::new(SandboxPolicyConfig {
        mode: SandboxMode::ReadOnly,
        workspace_root: Some(root.path().to_owned()),
    })?
    .provide(&context)?;
    let fiber = context.plugin(plugin(), json!({}))?;
    fiber.await_settled().await?;
    let owner = build_owner()?;
    for caller in [Some(owner), None] {
        let result = call_editor(
            &context,
            caller,
            json!({
                "command":"create",
                "path":root.path().join(format!("blocked-{}.txt", wrapped.policies.lock().len())),
                "file_text":"blocked"
            }),
        )
        .await;
        assert_eq!(error_code(&result), Some("FS_SANDBOX_DENIED"));
        assert!(text(&result).contains("[sandbox: file access denied under read-only mode]"));
    }
    let existing = root.path().join("existing.txt");
    tokio::fs::write(&existing, "old\n").await?;
    for args in [
        json!({"command":"str_replace","path":existing,"old_str":"old","new_str":"new"}),
        json!({"command":"insert","path":existing,"insert_line":1,"new_str":"new"}),
    ] {
        let result = call_editor(&context, None, args).await;
        assert_eq!(error_code(&result), Some("FS_SANDBOX_DENIED"));
    }
    assert_eq!(
        wrapped.policies.lock().as_slice(),
        &[
            Some(SandboxMode::ReadOnly),
            Some(SandboxMode::ReadOnly),
            Some(SandboxMode::ReadOnly),
            Some(SandboxMode::ReadOnly),
        ]
    );
    fiber.dispose().await?;
    Ok(())
}

#[tokio::test]
async fn unexpected_backend_write_failures_surface_for_replace_and_insert() -> anyhow::Result<()> {
    let context = Context::new();
    let prompt = install_prompt(&context, SystemPromptConfig::default())?;
    seekdeep_tools::install(&context, &prompt, ToolRuntimeConfig::default())?;
    let root = tempfile::tempdir()?;
    let local = LocalFileSystem::new(LocalConfig {
        cwd: Some(root.path().to_string_lossy().into_owned()),
        ..LocalConfig::default()
    })?;
    let wrapped = Arc::new(WrappedFileSystem {
        inner: local,
        mode: None,
        failure: Mutex::new(WriteFailure::Generic),
        policies: Mutex::new(Vec::new()),
    });
    FileSystemService::new(wrapped).provide(&context)?;
    let fiber = context.plugin(plugin(), json!({}))?;
    fiber.await_settled().await?;
    let path = root.path().join("backend-error.txt");
    tokio::fs::write(&path, "old\n").await?;
    for args in [
        json!({"command":"str_replace","path":path,"old_str":"old","new_str":"new"}),
        json!({"command":"insert","path":path,"insert_line":1,"new_str":"new"}),
    ] {
        let result = call_editor(&context, None, args).await;
        assert!(result.is_error());
        assert!(text(&result).contains("backend write failed"));
    }
    fiber.dispose().await?;
    Ok(())
}
