//! Differential coverage for filesystem-seam LSP host source access.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use futures::{StreamExt, stream::BoxStream};
use seekdeep_fs::{
    FileSystem, FsDirEntry, FsEditOutcome, FsEditRequest, FsInfo, FsPathInfo, FsTarget, FsVersion,
    FsWriteIntent, FsWriteOutcome,
};
use seekdeep_fs_local::{Config, LocalFileSystem};
use seekdeep_llm::AbortSignal;
use seekdeep_lsp_stdio::{HostWorkspace, canonicalize_workspace, read_host_source};
use seekdeep_sandbox::SandboxExecutionPolicy;
use tempfile::TempDir;

const BIG: usize = 1_000_000;

async fn fixture() -> (TempDir, PathBuf, Arc<LocalFileSystem>) {
    let root = tempfile::tempdir().unwrap();
    let canonical_root = tokio::fs::canonicalize(root.path()).await.unwrap();
    let workspace = canonical_root.join("ws");
    tokio::fs::create_dir(&workspace).await.unwrap();
    let filesystem = LocalFileSystem::new(Config {
        cwd: Some(canonical_root.to_string_lossy().into_owned()),
        diff_basis_max_bytes: None,
    })
    .unwrap();
    (root, workspace, filesystem)
}

async fn workspace(filesystem: &dyn FileSystem, path: &Path) -> HostWorkspace {
    canonicalize_workspace(filesystem, &path.to_string_lossy(), None)
        .await
        .unwrap()
}

#[tokio::test]
async fn workspace_identity_aliases_and_directory_validation_are_exact() {
    let (_root, workspace_path, filesystem) = fixture().await;
    let canonical = workspace(filesystem.as_ref(), &workspace_path).await;
    assert_eq!(canonical.canonical_path, workspace_path.to_string_lossy());

    #[cfg(unix)]
    {
        let link = workspace_path.parent().unwrap().join("ws-link");
        std::os::unix::fs::symlink(&workspace_path, &link).unwrap();
        let alias = canonicalize_workspace(filesystem.as_ref(), &link.to_string_lossy(), None)
            .await
            .unwrap();
        assert_eq!(alias.target.target_key, canonical.target.target_key);
        assert_eq!(alias.canonical_path, canonical.canonical_path);
    }

    let missing = workspace_path.parent().unwrap().join("missing");
    let error = canonicalize_workspace(filesystem.as_ref(), &missing.to_string_lossy(), None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not a directory"), "{error:#}");

    let file = workspace_path.parent().unwrap().join("file.txt");
    tokio::fs::write(&file, "x").await.unwrap();
    let error = canonicalize_workspace(filesystem.as_ref(), &file.to_string_lossy(), None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not a directory"), "{error:#}");
}

#[tokio::test]
async fn source_resolution_containment_and_regular_file_checks_are_exact() {
    let (_root, workspace_path, filesystem) = fixture().await;
    let workspace = workspace(filesystem.as_ref(), &workspace_path).await;
    let relative = workspace_path.join("a.ts");
    tokio::fs::write(&relative, "const x = 1\n").await.unwrap();
    let source = read_host_source(filesystem.as_ref(), "a.ts", &workspace, BIG, None)
        .await
        .unwrap();
    assert_eq!(source.text, "const x = 1\n");
    assert_eq!(
        source.file_url,
        filesystem.file_url(
            &filesystem
                .resolve("a.ts", Some(&workspace.canonical_path), None)
                .await
                .unwrap()
        )
    );

    let absolute = workspace_path.join("b.ts");
    tokio::fs::write(&absolute, "b").await.unwrap();
    assert_eq!(
        read_host_source(
            filesystem.as_ref(),
            &absolute.to_string_lossy(),
            &workspace,
            BIG,
            None,
        )
        .await
        .unwrap()
        .text,
        "b"
    );

    #[cfg(unix)]
    {
        let real = workspace_path.join("real");
        tokio::fs::create_dir(&real).await.unwrap();
        tokio::fs::write(real.join("c.ts"), "c").await.unwrap();
        std::os::unix::fs::symlink(&real, workspace_path.join("linked")).unwrap();
        assert_eq!(
            read_host_source(filesystem.as_ref(), "linked/c.ts", &workspace, BIG, None)
                .await
                .unwrap()
                .text,
            "c"
        );

        let outside = workspace_path.parent().unwrap().join("outside.ts");
        tokio::fs::write(&outside, "secret").await.unwrap();
        std::os::unix::fs::symlink(&outside, workspace_path.join("escape.ts")).unwrap();
        let error = read_host_source(filesystem.as_ref(), "escape.ts", &workspace, BIG, None)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("outside the workspace"));
    }

    let outside = workspace_path.parent().unwrap().join("out.ts");
    tokio::fs::write(&outside, "x").await.unwrap();
    let error = read_host_source(
        filesystem.as_ref(),
        &outside.to_string_lossy(),
        &workspace,
        BIG,
        None,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("outside the workspace"));

    let error = read_host_source(filesystem.as_ref(), "nope.ts", &workspace, BIG, None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not found"), "{error:#}");
    tokio::fs::create_dir(workspace_path.join("dir"))
        .await
        .unwrap();
    for path in [".", "dir"] {
        let error = read_host_source(filesystem.as_ref(), path, &workspace, BIG, None)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("not a regular file"),
            "{error:#}"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn fifo_is_rejected_before_opening_without_a_writer() {
    let (_root, workspace_path, filesystem) = fixture().await;
    let workspace = workspace(filesystem.as_ref(), &workspace_path).await;
    let fifo = workspace_path.join("pipe.ts");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        read_host_source(filesystem.as_ref(), "pipe.ts", &workspace, BIG, None),
    )
    .await
    .expect("FIFO read blocked")
    .unwrap_err();
    assert!(
        error.to_string().contains("not a regular file"),
        "{error:#}"
    );
}

#[tokio::test]
async fn byte_bounds_utf8_and_preabort_are_exact() {
    let (_root, workspace_path, filesystem) = fixture().await;
    let workspace = workspace(filesystem.as_ref(), &workspace_path).await;
    tokio::fs::write(workspace_path.join("big.ts"), "x".repeat(100))
        .await
        .unwrap();
    let error = read_host_source(filesystem.as_ref(), "big.ts", &workspace, 10, None)
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "source \"big.ts\" exceeds the 10-byte limit; reading stopped after 100 bytes"
    );

    tokio::fs::write(workspace_path.join("multibyte.ts"), "€abc")
        .await
        .unwrap();
    assert_eq!(
        read_host_source(filesystem.as_ref(), "multibyte.ts", &workspace, 6, None)
            .await
            .unwrap()
            .text,
        "€abc"
    );
    assert!(
        read_host_source(filesystem.as_ref(), "multibyte.ts", &workspace, 5, None)
            .await
            .unwrap_err()
            .to_string()
            .contains("5-byte limit")
    );

    tokio::fs::write(workspace_path.join("bin.ts"), [0xff, 0xfe, 0x00])
        .await
        .unwrap();
    let error = read_host_source(filesystem.as_ref(), "bin.ts", &workspace, BIG, None)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("invalid UTF-8") || error.to_string().contains("binary file")
    );
    tokio::fs::write(workspace_path.join("replacement.ts"), "const s = \"�\"\n")
        .await
        .unwrap();
    assert_eq!(
        read_host_source(filesystem.as_ref(), "replacement.ts", &workspace, BIG, None)
            .await
            .unwrap()
            .text,
        "const s = \"�\"\n"
    );

    let signal = AbortSignal::default();
    signal.abort();
    let error = read_host_source(
        filesystem.as_ref(),
        "missing.ts",
        &workspace,
        BIG,
        Some(&signal),
    )
    .await
    .unwrap_err();
    assert_eq!(error.to_string(), "LSP query aborted");
}

#[derive(Clone, Copy)]
enum Fault {
    Resolve,
    Stat,
    AbortStat,
    StreamStart,
    StreamItem,
}

struct FaultFilesystem {
    inner: Arc<dyn FileSystem>,
    fault: Fault,
}

#[async_trait]
impl FileSystem for FaultFilesystem {
    async fn resolve(
        &self,
        path: &str,
        cwd: Option<&str>,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<FsTarget> {
        if matches!(self.fault, Fault::Resolve) {
            anyhow::bail!("raw resolve failure");
        }
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
        if matches!(self.fault, Fault::AbortStat) {
            signal.unwrap().abort();
            anyhow::bail!("provider metadata failed");
        }
        if matches!(self.fault, Fault::Stat) {
            anyhow::bail!("workspace metadata failed");
        }
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
        if matches!(self.fault, Fault::StreamStart) {
            anyhow::bail!("stream setup failed");
        }
        if matches!(self.fault, Fault::StreamItem) {
            return Ok(
                futures::stream::once(async { Err(anyhow::anyhow!("stream item failed")) }).boxed(),
            );
        }
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
        target: &FsTarget,
        content: &str,
        expected: Option<&FsWriteIntent>,
        signal: Option<&AbortSignal>,
        sandbox_policy: Option<&SandboxExecutionPolicy>,
    ) -> anyhow::Result<FsWriteOutcome> {
        self.inner
            .write_text(target, content, expected, signal, sandbox_policy)
            .await
    }

    async fn edit_text(
        &self,
        target: &FsTarget,
        edit: &FsEditRequest,
        expected: Option<&FsVersion>,
        signal: Option<&AbortSignal>,
        sandbox_policy: Option<&SandboxExecutionPolicy>,
    ) -> anyhow::Result<FsEditOutcome> {
        self.inner
            .edit_text(target, edit, expected, signal, sandbox_policy)
            .await
    }
}

#[tokio::test]
async fn provider_failures_are_wrapped_once_and_abort_wins() {
    let (_root, workspace_path, local) = fixture().await;
    for (fault, expected) in [
        (Fault::Resolve, "workspace root"),
        (Fault::Stat, "workspace metadata failed"),
    ] {
        let filesystem = FaultFilesystem {
            inner: local.clone(),
            fault,
        };
        let error = canonicalize_workspace(&filesystem, &workspace_path.to_string_lossy(), None)
            .await
            .unwrap_err();
        assert!(error.to_string().contains(expected), "{error:#}");
    }

    let signal = AbortSignal::default();
    let filesystem = FaultFilesystem {
        inner: local.clone(),
        fault: Fault::AbortStat,
    };
    let error = canonicalize_workspace(
        &filesystem,
        &workspace_path.to_string_lossy(),
        Some(&signal),
    )
    .await
    .unwrap_err();
    assert_eq!(error.to_string(), "LSP query aborted");

    let canonical = workspace(local.as_ref(), &workspace_path).await;
    tokio::fs::write(workspace_path.join("source.ts"), "x")
        .await
        .unwrap();
    for (fault, detail) in [
        (Fault::Resolve, "cannot be resolved"),
        (Fault::StreamStart, "stream setup failed"),
        (Fault::StreamItem, "stream item failed"),
    ] {
        let filesystem = FaultFilesystem {
            inner: local.clone(),
            fault,
        };
        let error = read_host_source(&filesystem, "source.ts", &canonical, BIG, None)
            .await
            .unwrap_err();
        assert!(error.to_string().contains(detail), "{error:#}");
    }
}
