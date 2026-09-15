//! Real-filesystem policy, mutation, escalation, and lifecycle parity.

use std::{path::Path, sync::Arc};

use seekdeep_cordis::Context;
use seekdeep_fs::{
    FS, FileSystem,
    types::{FsEditRequest, FsError, FsErrorCode, FsTarget, FsTargetKey},
};
use seekdeep_fs_sandbox::{SandboxedFileSystem, apply};
use seekdeep_sandbox::{SandboxExecutionPolicy, SandboxMode};
use seekdeep_sandbox_policy::{SandboxPolicyConfig, SandboxPolicyService};

struct Harness {
    _context: Context,
    _base: tempfile::TempDir,
    workspace: std::path::PathBuf,
    outside: std::path::PathBuf,
    filesystem: Arc<SandboxedFileSystem>,
}

fn harness(mode: SandboxMode) -> Harness {
    let home = std::env::var_os("HOME").expect("home");
    let base = tempfile::Builder::new()
        .prefix(".seekdeep-fs-sandbox-")
        .tempdir_in(home)
        .expect("base");
    let workspace = base.path().join("workspace");
    let outside = base.path().join("outside");
    std::fs::create_dir(&workspace).expect("workspace");
    std::fs::create_dir(&outside).expect("outside");
    let context = Context::new();
    SandboxPolicyService::new(SandboxPolicyConfig {
        mode,
        workspace_root: Some(workspace.clone()),
    })
    .expect("policy")
    .provide(&context)
    .expect("provide policy");
    let filesystem = apply(
        &context,
        seekdeep_fs_local::Config {
            cwd: Some(workspace.to_string_lossy().into_owned()),
            ..seekdeep_fs_local::Config::default()
        },
    )
    .expect("filesystem");
    Harness {
        _context: context,
        _base: base,
        workspace,
        outside,
        filesystem,
    }
}

async fn target(filesystem: &SandboxedFileSystem, path: &Path) -> FsTarget {
    filesystem
        .resolve(&path.to_string_lossy(), None, None)
        .await
        .expect("resolve")
}

fn assert_denied(error: &anyhow::Error, mode: &str) {
    let error = error.downcast_ref::<FsError>().expect("typed fs error");
    assert_eq!(error.code, FsErrorCode::FsSandboxDenied);
    assert!(error.message.contains(mode), "{}", error.message);
}

fn policy(mode: SandboxMode, workspace: &Path) -> SandboxExecutionPolicy {
    SandboxExecutionPolicy {
        mode,
        workspace_root: workspace.to_owned(),
        session_id: None,
    }
}

#[tokio::test]
async fn read_only_denies_mutations_but_allows_reads() {
    let harness = harness(SandboxMode::ReadOnly);
    assert_eq!(
        harness.filesystem.sandbox_mode(),
        Some(SandboxMode::ReadOnly)
    );
    let denied = harness.workspace.join("denied.txt");
    let error = harness
        .filesystem
        .write_text(
            &target(&harness.filesystem, &denied).await,
            "x",
            None,
            None,
            None,
        )
        .await
        .unwrap_err();
    assert_denied(&error, "read-only");
    assert!(!denied.exists());

    let editable = harness.workspace.join("editable.txt");
    std::fs::write(&editable, "original").expect("editable");
    let editable_target = target(&harness.filesystem, &editable).await;
    let error = harness
        .filesystem
        .edit_text(
            &editable_target,
            &FsEditRequest {
                old_string: "original".to_owned(),
                new_string: "changed".to_owned(),
                replace_all: false,
            },
            None,
            None,
            None,
        )
        .await
        .unwrap_err();
    assert_denied(&error, "read-only");
    assert_eq!(std::fs::read_to_string(&editable).unwrap(), "original");
    assert_eq!(
        harness
            .filesystem
            .read_text(&editable_target, None)
            .await
            .unwrap(),
        "original"
    );
}

#[tokio::test]
async fn workspace_write_allows_workspace_and_temp_but_denies_escape_and_symlinks() {
    let harness = harness(SandboxMode::WorkspaceWrite);
    let inside = harness.workspace.join("nested/inside.txt");
    harness
        .filesystem
        .write_text(
            &target(&harness.filesystem, &inside).await,
            "inside",
            None,
            None,
            None,
        )
        .await
        .expect("inside write");
    assert_eq!(std::fs::read_to_string(&inside).unwrap(), "inside");

    let error = harness
        .filesystem
        .write_text(
            &target(&harness.filesystem, &harness.workspace).await,
            "not a file",
            None,
            None,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<FsError>().expect("typed error").code,
        FsErrorCode::FsNotRegularFile
    );

    let temp = tempfile::tempdir().expect("temp");
    let temp_file = temp.path().join("temp.txt");
    harness
        .filesystem
        .write_text(
            &target(&harness.filesystem, &temp_file).await,
            "temp",
            None,
            None,
            None,
        )
        .await
        .expect("temp write");

    for escaped in [
        harness.outside.join("absolute.txt"),
        harness.workspace.join("../traversal.txt"),
    ] {
        let error = harness
            .filesystem
            .write_text(
                &target(&harness.filesystem, &escaped).await,
                "escape",
                None,
                None,
                None,
            )
            .await
            .unwrap_err();
        assert_denied(&error, "workspace-write");
        assert!(!escaped.exists());
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let link = harness.workspace.join("link");
        symlink(&harness.outside, &link).expect("symlink");
        let escaped = link.join("new/deep.txt");
        let error = harness
            .filesystem
            .write_text(
                &target(&harness.filesystem, &escaped).await,
                "escape",
                None,
                None,
                None,
            )
            .await
            .unwrap_err();
        assert_denied(&error, "workspace-write");
        assert!(!harness.outside.join("new").exists());
    }
}

#[tokio::test]
async fn filesystem_root_workspace_grants_targets_on_that_volume() {
    let base = tempfile::tempdir().expect("base");
    let context = Context::new();
    let root = base
        .path()
        .ancestors()
        .last()
        .expect("filesystem root")
        .to_owned();
    SandboxPolicyService::new(SandboxPolicyConfig {
        mode: SandboxMode::WorkspaceWrite,
        workspace_root: Some(root),
    })
    .expect("policy")
    .provide(&context)
    .expect("provide policy");
    let filesystem = apply(
        &context,
        seekdeep_fs_local::Config {
            cwd: Some(base.path().to_string_lossy().into_owned()),
            ..seekdeep_fs_local::Config::default()
        },
    )
    .expect("filesystem");
    let path = base.path().join("anywhere.txt");
    filesystem
        .write_text(
            &target(&filesystem, &path).await,
            "anywhere",
            None,
            None,
            None,
        )
        .await
        .expect("root write");
    assert_eq!(std::fs::read_to_string(path).unwrap(), "anywhere");
}

#[tokio::test]
async fn edits_and_fresh_target_identity_follow_the_checked_path() {
    let harness = harness(SandboxMode::WorkspaceWrite);
    let inside = harness.workspace.join("edit.txt");
    std::fs::write(&inside, "original").expect("inside");
    let outcome = harness
        .filesystem
        .edit_text(
            &target(&harness.filesystem, &inside).await,
            &FsEditRequest {
                old_string: "original".to_owned(),
                new_string: "changed".to_owned(),
                replace_all: false,
            },
            None,
            None,
            None,
        )
        .await
        .expect("edit");
    assert_eq!(outcome.after, "changed");

    let outside = harness.outside.join("escaped.txt");
    let stale = FsTarget {
        display_path: harness
            .workspace
            .join("landed.txt")
            .to_string_lossy()
            .into_owned(),
        target_key: FsTargetKey::new(outside.to_string_lossy().into_owned()),
    };
    harness
        .filesystem
        .write_text(&stale, "landed", None, None, None)
        .await
        .expect("fresh identity write");
    assert_eq!(
        std::fs::read_to_string(harness.workspace.join("landed.txt")).unwrap(),
        "landed"
    );
    assert!(!outside.exists());
}

#[tokio::test]
async fn full_access_and_per_call_escalation_preserve_default_policy() {
    let full = harness(SandboxMode::DangerFullAccess);
    let outside = full.outside.join("full.txt");
    full.filesystem
        .write_text(
            &target(&full.filesystem, &outside).await,
            "full",
            None,
            None,
            None,
        )
        .await
        .expect("full access");

    let read_only = harness(SandboxMode::ReadOnly);
    let escalated = read_only.workspace.join("escalated.txt");
    read_only
        .filesystem
        .write_text(
            &target(&read_only.filesystem, &escalated).await,
            "granted",
            None,
            None,
            Some(&policy(SandboxMode::WorkspaceWrite, &read_only.workspace)),
        )
        .await
        .expect("workspace escalation");
    let plain = read_only.workspace.join("plain.txt");
    assert_denied(
        &read_only
            .filesystem
            .write_text(
                &target(&read_only.filesystem, &plain).await,
                "denied",
                None,
                None,
                None,
            )
            .await
            .unwrap_err(),
        "read-only",
    );

    let granted_full = read_only.outside.join("granted-full.txt");
    read_only
        .filesystem
        .write_text(
            &target(&read_only.filesystem, &granted_full).await,
            "full",
            None,
            None,
            Some(&policy(SandboxMode::DangerFullAccess, &read_only.workspace)),
        )
        .await
        .expect("full escalation");
}

#[tokio::test]
async fn plugin_activation_and_disposal_own_the_fs_registration() {
    let base = tempfile::tempdir().expect("base");
    let context = Context::new();
    let plugin = context
        .plugin(
            seekdeep_fs_sandbox::plugin(),
            serde_json::json!({"cwd": base.path()}),
        )
        .expect("plugin");
    plugin.await_settled().await.expect("pending");
    assert!(context.get(FS).is_none());
    SandboxPolicyService::new(SandboxPolicyConfig {
        mode: SandboxMode::WorkspaceWrite,
        workspace_root: Some(base.path().to_owned()),
    })
    .expect("policy")
    .provide(&context)
    .expect("provide policy");
    plugin.await_settled().await.expect("active");
    assert!(context.get(FS).is_some());
    plugin.dispose().await.expect("dispose");
    assert!(context.get(FS).is_none());
}
