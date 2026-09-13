//! Provider-only files, bounded streams, cancellation, and probe failures.

use std::{collections::HashMap, path::Path, sync::Arc};

use async_trait::async_trait;
use futures::{stream, stream::BoxStream};
use parking_lot::Mutex;
use seekdeep_agent_instructions::{DiscoverOptions, load_baseline_instruction_set};
use seekdeep_fs::{
    FileSystem, FsDirEntry, FsEditOutcome, FsEditRequest, FsInfo, FsKind, FsPathInfo, FsPathKind,
    FsTarget, FsTargetKey, FsVersion, FsWriteIntent, FsWriteOutcome,
};
use seekdeep_llm::AbortSignal;
use seekdeep_sandbox::SandboxExecutionPolicy;

#[derive(Clone)]
struct Entry {
    kind: FsKind,
    content: String,
    version: FsVersion,
    size: Option<u64>,
}

#[derive(Default)]
struct RecordingFs {
    entries: Mutex<HashMap<String, Entry>>,
    stat_failures: Mutex<Vec<String>>,
    read_failures: Mutex<Vec<String>>,
    reads: Mutex<Vec<String>>,
    signals: Mutex<Vec<AbortSignal>>,
}

impl RecordingFs {
    fn insert(&self, path: impl Into<String>, kind: FsKind, content: &str, size: Option<u64>) {
        let path = path.into();
        self.entries.lock().insert(
            path.clone(),
            Entry {
                kind,
                content: content.to_owned(),
                version: FsVersion::new(format!("v:{path}:{content}")),
                size,
            },
        );
    }

    fn absolute(path: &str, cwd: Option<&str>) -> String {
        let path = Path::new(path);
        if path.is_absolute() {
            path.to_string_lossy().into_owned()
        } else {
            Path::new(cwd.unwrap_or("/"))
                .join(path)
                .to_string_lossy()
                .into_owned()
        }
    }

    fn observe_signal(&self, signal: Option<&AbortSignal>) -> anyhow::Result<()> {
        if let Some(signal) = signal {
            self.signals.lock().push(signal.clone());
            if signal.is_aborted() {
                anyhow::bail!("provider operation aborted");
            }
        }
        Ok(())
    }
}

#[async_trait]
impl FileSystem for RecordingFs {
    async fn resolve(
        &self,
        path: &str,
        cwd: Option<&str>,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<FsTarget> {
        self.observe_signal(signal)?;
        let path = Self::absolute(path, cwd);
        Ok(FsTarget {
            target_key: FsTargetKey::new(path.clone()),
            display_path: path,
        })
    }

    fn process_path(&self, target: &FsTarget) -> String {
        target.target_key.to_string()
    }

    fn file_url(&self, target: &FsTarget) -> String {
        format!("file://{}", target.target_key)
    }

    fn contains(&self, parent: &FsTarget, child: &FsTarget) -> bool {
        child.target_key == parent.target_key
            || child
                .target_key
                .as_str()
                .starts_with(&format!("{}/", parent.target_key))
    }

    async fn stat(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Option<FsInfo>> {
        self.observe_signal(signal)?;
        if self
            .stat_failures
            .lock()
            .iter()
            .any(|path| path == target.target_key.as_str())
        {
            anyhow::bail!("stat failed: {}", target.display_path);
        }
        Ok(self
            .entries
            .lock()
            .get(target.target_key.as_str())
            .map(|entry| FsInfo {
                version: entry.version.clone(),
                kind: entry.kind,
                size: entry.size,
            }))
    }

    async fn lstat(
        &self,
        path: &str,
        cwd: Option<&str>,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Option<FsPathInfo>> {
        let target = self.resolve(path, cwd, signal).await?;
        Ok(self
            .entries
            .lock()
            .get(target.target_key.as_str())
            .map(|entry| FsPathInfo {
                version: entry.version.clone(),
                kind: match entry.kind {
                    FsKind::File => FsPathKind::File,
                    FsKind::Directory => FsPathKind::Directory,
                    FsKind::Other => FsPathKind::Other,
                },
                size: entry.size,
            }))
    }

    async fn read_text(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<String> {
        self.observe_signal(signal)?;
        self.reads.lock().push(target.target_key.to_string());
        if self
            .read_failures
            .lock()
            .iter()
            .any(|path| path == target.target_key.as_str())
        {
            anyhow::bail!("read failed: {}", target.display_path);
        }
        Ok(self
            .entries
            .lock()
            .get(target.target_key.as_str())
            .map(|entry| entry.content.clone())
            .unwrap_or_default())
    }

    async fn stream_text(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<BoxStream<'static, anyhow::Result<String>>> {
        let content = self.read_text(target, signal).await?;
        let middle = content.len() / 2;
        let (first, second) = content.split_at(middle);
        Ok(Box::pin(stream::iter([
            Ok(first.to_owned()),
            Ok(second.to_owned()),
        ])))
    }

    async fn read_bytes(
        &self,
        _target: &FsTarget,
        _signal: Option<&AbortSignal>,
        _max_bytes: usize,
    ) -> anyhow::Result<Vec<u8>> {
        anyhow::bail!("unused read_bytes")
    }

    async fn list_dir(
        &self,
        _target: &FsTarget,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Vec<FsDirEntry>> {
        anyhow::bail!("unused list_dir")
    }

    async fn write_text(
        &self,
        _target: &FsTarget,
        _content: &str,
        _expected: Option<&FsWriteIntent>,
        _signal: Option<&AbortSignal>,
        _sandbox_policy: Option<&SandboxExecutionPolicy>,
    ) -> anyhow::Result<FsWriteOutcome> {
        anyhow::bail!("unused write_text")
    }

    async fn edit_text(
        &self,
        _target: &FsTarget,
        _edit: &FsEditRequest,
        _expected: Option<&FsVersion>,
        _signal: Option<&AbortSignal>,
        _sandbox_policy: Option<&SandboxExecutionPolicy>,
    ) -> anyhow::Result<FsEditOutcome> {
        anyhow::bail!("unused edit_text")
    }
}

fn options() -> DiscoverOptions {
    DiscoverOptions {
        cwd: "/virtual/project".to_owned(),
        dsh_home: Some("/virtual/home".to_owned()),
        project_root: Some("/virtual/project".to_owned()),
        ..DiscoverOptions::default()
    }
}

#[tokio::test]
async fn loads_provider_visible_files_absent_from_host_and_reads_through_provider() {
    let fs = Arc::new(RecordingFs::default());
    fs.insert(
        "/virtual/project/AGENTS.md",
        FsKind::File,
        "provider-only instructions",
        Some(26),
    );
    let set = load_baseline_instruction_set(&options(), 4096, 1024, None, Some(fs.as_ref()))
        .await
        .unwrap()
        .unwrap();
    assert!(set.rendered.text.contains("provider-only instructions"));
    assert_eq!(fs.reads.lock().as_slice(), ["/virtual/project/AGENTS.md"]);
    assert!(!Path::new("/virtual/project/AGENTS.md").exists());
}

#[tokio::test]
async fn size_precheck_and_stream_counter_bound_provider_content() {
    let oversized = Arc::new(RecordingFs::default());
    oversized.insert(
        "/virtual/project/AGENTS.md",
        FsKind::File,
        &"x".repeat(100),
        Some(100),
    );
    assert!(
        load_baseline_instruction_set(&options(), 4096, 10, None, Some(oversized.as_ref()))
            .await
            .unwrap()
            .is_none()
    );
    assert!(oversized.reads.lock().is_empty());

    let unreported = Arc::new(RecordingFs::default());
    unreported.insert(
        "/virtual/project/AGENTS.md",
        FsKind::File,
        &"y".repeat(100),
        None,
    );
    assert!(
        load_baseline_instruction_set(&options(), 4096, 10, None, Some(unreported.as_ref()))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(unreported.reads.lock().len(), 1);
}

#[tokio::test]
async fn nonfile_unreadable_and_failed_candidates_do_not_hide_available_sibling() {
    let fs = Arc::new(RecordingFs::default());
    fs.insert("/virtual/project/AGENTS.md", FsKind::Directory, "", None);
    fs.insert(
        "/virtual/project/CLAUDE.md",
        FsKind::File,
        "available sibling",
        Some(17),
    );
    fs.stat_failures
        .lock()
        .push("/virtual/project/BROKEN.md".to_owned());
    let mut configured = options();
    configured.instruction_file_candidates = Some(vec![
        "BROKEN.md".to_owned(),
        "AGENTS.md".to_owned(),
        "CLAUDE.md".to_owned(),
    ]);
    let set = load_baseline_instruction_set(&configured, 4096, 1024, None, Some(fs.as_ref()))
        .await
        .unwrap()
        .unwrap();
    assert!(set.rendered.text.contains("available sibling"));
    assert!(!set.rendered.text.contains("BROKEN"));

    fs.read_failures
        .lock()
        .push("/virtual/project/CLAUDE.md".to_owned());
    assert!(
        load_baseline_instruction_set(&configured, 4096, 1024, None, Some(fs.as_ref()))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn cancellation_propagates_to_provider_resolution_stat_and_stream() {
    let fs = Arc::new(RecordingFs::default());
    fs.insert(
        "/virtual/project/AGENTS.md",
        FsKind::File,
        "cancelled content",
        None,
    );
    let signal = AbortSignal::default();
    signal.abort_with_reason(serde_json::json!({"kind": "cancelled"}));
    let mut configured = options();
    configured.signal = Some(signal.clone());
    let error = load_baseline_instruction_set(&configured, 4096, 1024, None, Some(fs.as_ref()))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("aborted"));
    assert!(fs.signals.lock().iter().all(AbortSignal::is_aborted));
}
