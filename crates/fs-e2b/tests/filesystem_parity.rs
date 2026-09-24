//! Fake-client parity for remote path, read, mutation, and error semantics.

use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use base64::Engine as _;
use futures::{StreamExt as _, stream};
use parking_lot::Mutex;
use seekdeep_fs::{
    FileSystem as _, FsEditRequest, FsError, FsErrorCode, FsKind, FsPathKind, FsTarget,
    FsTargetKey, FsWriteIntent, FsWriteOperation,
};
use seekdeep_fs_e2b::{
    E2bByteStream, E2bCommandExit, E2bCommandResult, E2bCommands, E2bEntryInfo, E2bFileNotFound,
    E2bFileSystem, E2bFileType, E2bFiles, E2bSandbox, E2bSandboxFuture, E2bService,
};
use seekdeep_llm::AbortSignal;

#[derive(Clone, Debug)]
struct Node {
    kind: E2bFileType,
    data: Vec<u8>,
    mode: u32,
    modified: u64,
    symlink: Option<String>,
    metadata: BTreeMap<String, String>,
}

impl Node {
    fn file(data: impl Into<Vec<u8>>, modified: u64) -> Self {
        Self {
            kind: E2bFileType::File,
            data: data.into(),
            mode: 0o644,
            modified,
            symlink: None,
            metadata: BTreeMap::new(),
        }
    }

    fn directory(modified: u64) -> Self {
        Self {
            kind: E2bFileType::Directory,
            data: Vec::new(),
            mode: 0o755,
            modified,
            symlink: None,
            metadata: BTreeMap::new(),
        }
    }

    fn symlink(target: &str, modified: u64) -> Self {
        Self {
            kind: E2bFileType::Other,
            data: Vec::new(),
            mode: 0o777,
            modified,
            symlink: Some(target.to_owned()),
            metadata: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug)]
enum Failure {
    Missing,
    Message(String),
    CommandExit(String),
    Aborted,
}

impl Failure {
    fn error(self) -> anyhow::Error {
        match self {
            Self::Missing => E2bFileNotFound {
                message: "not found".to_owned(),
            }
            .into(),
            Self::Message(message) => anyhow::anyhow!(message),
            Self::CommandExit(stderr) => E2bCommandExit { status: 1, stderr }.into(),
            Self::Aborted => anyhow::anyhow!("AbortError"),
        }
    }
}

#[derive(Default)]
struct RemoteState {
    nodes: HashMap<String, Node>,
    clock: u64,
    stream_chunks: Option<Vec<Vec<u8>>>,
    stream_cancelled: usize,
    reads: Vec<(String, &'static str)>,
    commands: Vec<String>,
    renames: Vec<(String, String)>,
    removals: Vec<String>,
    next_info_error: Option<Failure>,
    next_read_error: Option<Failure>,
    next_list_error: Option<Failure>,
    next_command_error: Option<Failure>,
    next_rename_error: Option<Failure>,
    next_remove_error: Option<Failure>,
    make_dir_result: Option<bool>,
    guarded_output: Option<String>,
    canonical_output: Option<String>,
    competitor: Option<(String, Node)>,
    abort_after_rename: Option<AbortSignal>,
    disappear_on_info: Vec<String>,
}

#[derive(Default)]
struct FakeRemote {
    state: Arc<Mutex<RemoteState>>,
}

impl FakeRemote {
    fn next_modified(state: &mut RemoteState) -> u64 {
        state.clock += 1;
        state.clock
    }

    fn file(&self, path: &str, data: impl Into<Vec<u8>>) {
        let mut state = self.state.lock();
        let modified = Self::next_modified(&mut state);
        state
            .nodes
            .insert(path.to_owned(), Node::file(data, modified));
    }

    fn dir(&self, path: &str) {
        let mut state = self.state.lock();
        let modified = Self::next_modified(&mut state);
        state
            .nodes
            .insert(path.to_owned(), Node::directory(modified));
    }

    fn symlink(&self, path: &str, target: &str) {
        let mut state = self.state.lock();
        let modified = Self::next_modified(&mut state);
        state
            .nodes
            .insert(path.to_owned(), Node::symlink(target, modified));
    }

    fn mutate(&self, path: &str, data: impl Into<Vec<u8>>) {
        self.file(path, data);
    }

    fn info(path: &str, node: &Node) -> E2bEntryInfo {
        E2bEntryInfo {
            name: path.rsplit('/').next().unwrap_or(path).to_owned(),
            path: path.to_owned(),
            kind: node.kind,
            size: node.data.len() as u64,
            mode: node.mode,
            modified_time: Some(format!("t{}", node.modified)),
            symlink_target: node.symlink.clone(),
            metadata: node.metadata.clone(),
        }
    }

    fn resolved_path(state: &RemoteState, path: &str) -> String {
        state
            .nodes
            .get(path)
            .and_then(|node| node.symlink.clone())
            .unwrap_or_else(|| lexical(path))
    }
}

#[async_trait::async_trait]
impl E2bFiles for FakeRemote {
    async fn get_info(
        &self,
        path: &str,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<E2bEntryInfo> {
        let mut state = self.state.lock();
        if let Some(error) = state.next_info_error.take() {
            return Err(error.error());
        }
        if state.disappear_on_info.iter().any(|entry| entry == path) {
            return Err(Failure::Missing.error());
        }
        let node = state
            .nodes
            .get(path)
            .ok_or_else(|| Failure::Missing.error())?;
        Ok(Self::info(path, node))
    }

    async fn read_bytes(
        &self,
        path: &str,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Vec<u8>> {
        let mut state = self.state.lock();
        state.reads.push((path.to_owned(), "bytes"));
        if let Some(error) = state.next_read_error.take() {
            return Err(error.error());
        }
        let node = state
            .nodes
            .get(path)
            .ok_or_else(|| Failure::Missing.error())?;
        Ok(node.data.clone())
    }

    async fn read_stream(
        &self,
        path: &str,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<E2bByteStream> {
        let mut state = self.state.lock();
        state.reads.push((path.to_owned(), "stream"));
        if let Some(error) = state.next_read_error.take() {
            return Err(error.error());
        }
        let chunks = state.stream_chunks.clone().unwrap_or_else(|| {
            vec![
                state
                    .nodes
                    .get(path)
                    .map(|node| node.data.clone())
                    .unwrap_or_default(),
            ]
        });
        let state = self.state.clone();
        Ok(E2bByteStream {
            stream: Box::pin(stream::iter(chunks.into_iter().map(Ok))),
            cancel: Arc::new(move || state.lock().stream_cancelled += 1),
        })
    }

    async fn list(
        &self,
        path: &str,
        _depth: u32,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Vec<E2bEntryInfo>> {
        let mut state = self.state.lock();
        if let Some(error) = state.next_list_error.take() {
            return Err(error.error());
        }
        let prefix = format!("{}/", path.trim_end_matches('/'));
        Ok(state
            .nodes
            .iter()
            .filter(|(candidate, _)| {
                candidate
                    .strip_prefix(&prefix)
                    .is_some_and(|suffix| !suffix.is_empty() && !suffix.contains('/'))
            })
            .map(|(candidate, node)| Self::info(candidate, node))
            .collect())
    }

    async fn make_dir(&self, path: &str, _signal: Option<&AbortSignal>) -> anyhow::Result<bool> {
        let mut state = self.state.lock();
        if let Some(result) = state.make_dir_result.take() {
            return Ok(result);
        }
        if state.nodes.contains_key(path) {
            return Ok(false);
        }
        let modified = Self::next_modified(&mut state);
        state
            .nodes
            .insert(path.to_owned(), Node::directory(modified));
        Ok(true)
    }

    async fn write(
        &self,
        path: &str,
        content: &str,
        metadata: BTreeMap<String, String>,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<()> {
        let mut state = self.state.lock();
        let modified = Self::next_modified(&mut state);
        let mut node = Node::file(content.as_bytes().to_vec(), modified);
        node.metadata = metadata;
        state.nodes.insert(path.to_owned(), node);
        Ok(())
    }

    async fn rename(
        &self,
        from: &str,
        to: &str,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<E2bEntryInfo> {
        let mut state = self.state.lock();
        if let Some(error) = state.next_rename_error.take() {
            return Err(error.error());
        }
        let mut node = state
            .nodes
            .remove(from)
            .ok_or_else(|| Failure::Missing.error())?;
        node.modified = Self::next_modified(&mut state);
        state.nodes.insert(to.to_owned(), node.clone());
        state.renames.push((from.to_owned(), to.to_owned()));
        if let Some(signal) = state.abort_after_rename.take() {
            signal.abort();
        }
        Ok(Self::info(to, &node))
    }

    async fn remove(&self, path: &str) -> anyhow::Result<()> {
        let mut state = self.state.lock();
        if let Some(error) = state.next_remove_error.take() {
            return Err(error.error());
        }
        state.removals.push(path.to_owned());
        let prefix = format!("{}/", path.trim_end_matches('/'));
        state
            .nodes
            .retain(|candidate, _| candidate != path && !candidate.starts_with(&prefix));
        Ok(())
    }
}

#[async_trait::async_trait]
impl E2bCommands for FakeRemote {
    async fn run(
        &self,
        command: &str,
        _env: BTreeMap<String, String>,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<E2bCommandResult> {
        let mut state = self.state.lock();
        state.commands.push(command.to_owned());
        if let Some(error) = state.next_command_error.take() {
            return Err(error.error());
        }
        if command.contains("realpath -mz") {
            if let Some(stdout) = state.canonical_output.take() {
                return Ok(E2bCommandResult {
                    stdout,
                    stderr: String::new(),
                });
            }
            let arguments = shell_arguments(command);
            let path = arguments
                .iter()
                .position(|value| value == "--")
                .and_then(|index| arguments.get(index + 1))
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("missing realpath argument"))?;
            let path = Self::resolved_path(&state, &path);
            let mut framed = path.into_bytes();
            framed.push(0);
            return Ok(E2bCommandResult {
                stdout: base64::engine::general_purpose::STANDARD.encode(framed),
                stderr: String::new(),
            });
        }
        if command.starts_with("chmod ") {
            let arguments = shell_arguments(command);
            let mode = u32::from_str_radix(arguments.get(1).map_or("0", String::as_str), 8)?;
            if let Some(path) = arguments.last()
                && let Some(node) = state.nodes.get_mut(path)
            {
                node.mode = mode;
            }
            return Ok(E2bCommandResult::default());
        }
        if command.contains("if ln -T") {
            let arguments = shell_arguments(command);
            let source = arguments
                .iter()
                .position(|value| value == "--")
                .and_then(|index| arguments.get(index + 1))
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("missing link source"))?;
            let target = arguments
                .iter()
                .position(|value| value == "--")
                .and_then(|index| arguments.get(index + 2))
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("missing link target"))?;
            if let Some((path, node)) = state.competitor.take() {
                state.nodes.insert(path, node);
            }
            let output = if let Some(output) = state.guarded_output.take() {
                output
            } else if state.nodes.contains_key(&target) {
                "exists".to_owned()
            } else {
                let node = state
                    .nodes
                    .get(&source)
                    .cloned()
                    .ok_or_else(|| Failure::Missing.error())?;
                state.nodes.insert(target, node);
                "created".to_owned()
            };
            if let Some(signal) = state.abort_after_rename.take() {
                signal.abort();
            }
            return Ok(E2bCommandResult {
                stdout: output,
                stderr: String::new(),
            });
        }
        Ok(E2bCommandResult::default())
    }
}

struct FakeSandbox(Arc<FakeRemote>);

#[async_trait::async_trait]
impl E2bSandbox for FakeSandbox {
    fn sandbox_id(&self) -> &'static str {
        "fake-sandbox"
    }

    fn files(&self) -> Arc<dyn E2bFiles> {
        self.0.clone()
    }

    fn commands(&self) -> Arc<dyn E2bCommands> {
        self.0.clone()
    }

    async fn kill(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

fn setup(remote: Arc<FakeRemote>) -> Arc<E2bFileSystem> {
    let sandbox: Arc<dyn E2bSandbox> = Arc::new(FakeSandbox(remote));
    E2bFileSystem::new(E2bService::new(
        "/workspace",
        Arc::new(move || {
            let sandbox = sandbox.clone();
            Box::pin(async move { Ok(sandbox) }) as E2bSandboxFuture
        }),
    ))
}

fn code(error: &anyhow::Error) -> Option<FsErrorCode> {
    error.downcast_ref::<FsError>().map(|error| error.code)
}

fn lexical(path: &str) -> String {
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            value => parts.push(value),
        }
    }
    format!("/{}", parts.join("/"))
}

fn shell_arguments(command: &str) -> Vec<String> {
    let mut output = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut chars = command.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\'' {
            quoted = !quoted;
        } else if (character.is_whitespace() || matches!(character, ';' | '|')) && !quoted {
            if !current.is_empty() {
                output.push(std::mem::take(&mut current));
            }
        } else if character == '\\' && !quoted && chars.peek() == Some(&'\'') {
            current.push(chars.next().unwrap());
        } else {
            current.push(character);
        }
    }
    if !current.is_empty() {
        output.push(current);
    }
    output
}

fn target(path: &str) -> FsTarget {
    FsTarget {
        target_key: FsTargetKey::new(path),
        display_path: path.to_owned(),
    }
}

#[tokio::test]
async fn resolves_paths_projects_urls_containment_metadata_and_sorted_listing() {
    let remote = Arc::new(FakeRemote::default());
    remote.dir("/workspace");
    remote.file("/workspace/b file", b"bb".to_vec());
    remote.file("/workspace/a", b"a".to_vec());
    remote.file("/workspace/target", b"target".to_vec());
    remote.file("/workspace/gone", b"gone".to_vec());
    remote.symlink("/workspace/link", "/workspace/target");
    remote.symlink("/workspace/vanished-link", "/workspace/gone");
    remote
        .state
        .lock()
        .disappear_on_info
        .push("/workspace/gone".to_owned());
    let fs = setup(remote.clone());

    let resolved = fs.resolve("./dir/../a", None, None).await.unwrap();
    assert_eq!(resolved.target_key.as_str(), "/workspace/a");
    assert_eq!(resolved.display_path, "/workspace/a");
    assert_eq!(fs.process_path(&resolved), "/workspace/a");
    assert_eq!(
        fs.file_url(&target("/workspace/b file")),
        "file:///workspace/b%20file"
    );
    assert!(fs.contains(&target("/workspace"), &target("/workspace/a")));
    assert!(!fs.contains(&target("/workspace/a"), &target("/workspace/ab")));

    let info = fs.stat(&resolved, None).await.unwrap().unwrap();
    assert_eq!(info.kind, FsKind::File);
    assert_eq!(info.size, Some(1));
    assert_eq!(
        fs.lstat("link", None, None).await.unwrap().unwrap().kind,
        FsPathKind::Symlink
    );
    let listed = fs
        .list_dir(&fs.resolve("/workspace", None, None).await.unwrap(), None)
        .await
        .unwrap();
    assert_eq!(
        listed
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["a", "b file", "gone", "link", "target", "vanished-link"]
    );
    let link = listed.iter().find(|entry| entry.name == "link").unwrap();
    assert_eq!(link.kind, FsKind::File);
    assert_eq!(link.target.target_key.as_str(), "/workspace/target");
    assert_eq!(link.target.display_path, "/workspace/link");
    let vanished = listed
        .iter()
        .find(|entry| entry.name == "vanished-link")
        .unwrap();
    assert_eq!(vanished.kind, FsKind::Other);
    assert!(vanished.version.is_none());
}

#[tokio::test]
async fn canonical_path_transport_preserves_bytes_and_rejects_every_invalid_frame() {
    let remote = Arc::new(FakeRemote::default());
    let fs = setup(remote.clone());
    let encode = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
    let path = "/workspace/line\n€";
    let mut framed = path.as_bytes().to_vec();
    framed.push(0);
    remote.state.lock().canonical_output = Some(encode(&framed));
    assert_eq!(
        fs.resolve("line", None, None)
            .await
            .unwrap()
            .target_key
            .as_str(),
        path
    );

    for invalid in [
        "%%%".to_owned(),
        encode(b"/workspace/no-nul"),
        encode(b"/workspace/a\0b\0"),
        encode(&[b'/', 0xff, 0]),
        encode(b"relative\0"),
    ] {
        remote.state.lock().canonical_output = Some(invalid);
        assert_eq!(
            code(&fs.resolve("bad", None, None).await.unwrap_err()),
            Some(FsErrorCode::FsIoError)
        );
    }
}

#[tokio::test]
async fn reads_text_bytes_and_streams_with_utf8_binary_bounds_and_cancellation() {
    let remote = Arc::new(FakeRemote::default());
    remote.file("/workspace/text.txt", vec![65, 0xe2, 0x82, 0xac, 66]);
    remote.file("/workspace/binary", vec![0, 1]);
    remote.file("/workspace/invalid", vec![0xff]);
    remote.file(
        "/workspace/late-nul",
        [vec![b'a'; 8192], vec![0, b't']].concat(),
    );
    remote.file("/workspace/empty", Vec::new());
    remote.file("/workspace/grow", vec![1, 1, 1, 1]);
    remote.dir("/workspace/directory");
    let fs = setup(remote.clone());
    let text_target = fs.resolve("text.txt", None, None).await.unwrap();
    assert_eq!(fs.read_text(&text_target, None).await.unwrap(), "A€B");

    remote.state.lock().stream_chunks = Some(vec![vec![65, 0xe2], vec![0x82, 0xac, 66]]);
    let mut stream = fs.stream_text(&text_target, None).await.unwrap();
    let mut streamed = String::new();
    while let Some(chunk) = stream.next().await {
        streamed.push_str(&chunk.unwrap());
    }
    assert_eq!(streamed, "A€B");

    remote.state.lock().stream_chunks = Some(vec![b"a".to_vec(), b"b".to_vec()]);
    let mut early = fs.stream_text(&text_target, None).await.unwrap();
    assert_eq!(early.next().await.unwrap().unwrap(), "a");
    drop(early);
    assert_eq!(remote.state.lock().stream_cancelled, 1);

    assert_eq!(
        code(
            &fs.read_text(&fs.resolve("binary", None, None).await.unwrap(), None)
                .await
                .unwrap_err()
        ),
        Some(FsErrorCode::FsNotText)
    );
    assert_eq!(
        code(
            &fs.read_text(&fs.resolve("invalid", None, None).await.unwrap(), None)
                .await
                .unwrap_err()
        ),
        Some(FsErrorCode::FsNotText)
    );
    assert!(
        fs.read_text(&fs.resolve("late-nul", None, None).await.unwrap(), None)
            .await
            .unwrap()
            .contains('\0')
    );
    let empty = fs.resolve("empty", None, None).await.unwrap();
    remote.state.lock().stream_chunks = None;
    assert!(fs.read_bytes(&empty, None, 1).await.unwrap().is_empty());

    remote.state.lock().stream_chunks = Some(vec![vec![1, 1, 1], vec![1, 2, 2]]);
    let grow = fs.resolve("grow", None, None).await.unwrap();
    let error = fs.read_bytes(&grow, None, 4).await.unwrap_err();
    assert_eq!(code(&error), Some(FsErrorCode::FsTooLarge));
    assert_eq!(remote.state.lock().stream_cancelled, 2);
    assert_eq!(
        code(
            &fs.read_bytes(&target("/workspace/directory"), None, 4)
                .await
                .unwrap_err()
        ),
        Some(FsErrorCode::FsNotRegularFile)
    );
}

#[tokio::test]
async fn creates_and_replaces_atomically_with_intent_version_and_commit_boundaries() {
    let remote = Arc::new(FakeRemote::default());
    remote.dir("/workspace");
    let fs = setup(remote.clone());
    let new_target = fs.resolve("new.txt", None, None).await.unwrap();
    let created = fs
        .write_text(
            &new_target,
            "one\r\ntwo\rthree",
            Some(&FsWriteIntent::CreateIfAbsent),
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(created.operation, FsWriteOperation::Create);
    assert_eq!(created.before, None);
    assert_eq!(created.after, "one\ntwo\rthree");
    let node = remote.state.lock().nodes["/workspace/new.txt"].clone();
    assert_eq!(node.mode, 0o600);
    assert!(node.metadata.contains_key("seekdeep-version"));
    assert_eq!(remote.state.lock().renames.len(), 0);

    remote.file("/workspace/file.txt", b"old\r\nline\rlone".to_vec());
    remote
        .state
        .lock()
        .nodes
        .get_mut("/workspace/file.txt")
        .unwrap()
        .mode = 0o640;
    let file = fs.resolve("file.txt", None, None).await.unwrap();
    let version = fs.stat(&file, None).await.unwrap().unwrap().version;
    let updated = fs
        .write_text(
            &file,
            "new",
            Some(&FsWriteIntent::ReplaceIfVersion {
                version: version.clone(),
            }),
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(updated.operation, FsWriteOperation::Update);
    assert_eq!(updated.before.as_deref(), Some("old\nline\rlone"));
    assert_eq!(remote.state.lock().nodes["/workspace/file.txt"].mode, 0o640);
    remote.mutate("/workspace/file.txt", b"external".to_vec());
    assert_ne!(
        fs.stat(&file, None).await.unwrap().unwrap().version,
        updated.version
    );
    assert_eq!(
        code(
            &fs.write_text(
                &file,
                "blind",
                Some(&FsWriteIntent::CreateIfAbsent),
                None,
                None
            )
            .await
            .unwrap_err()
        ),
        Some(FsErrorCode::FsNotObserved)
    );
    assert_eq!(
        code(
            &fs.write_text(
                &file,
                "stale",
                Some(&FsWriteIntent::ReplaceIfVersion { version }),
                None,
                None
            )
            .await
            .unwrap_err()
        ),
        Some(FsErrorCode::FsStaleVersion)
    );

    let committed = fs.resolve("committed", None, None).await.unwrap();
    remote.state.lock().next_remove_error = Some(Failure::Message("cleanup failed".to_owned()));
    assert!(
        fs.write_text(&committed, "yes", None, None, None)
            .await
            .is_ok()
    );
    assert_eq!(
        remote.state.lock().nodes["/workspace/committed"].data,
        b"yes"
    );
}

#[tokio::test]
async fn guarded_create_preserves_competitors_and_rejects_invalid_publication() {
    let remote = Arc::new(FakeRemote::default());
    remote.dir("/workspace");
    let fs = setup(remote.clone());
    let competitor = Node::file(b"competitor".to_vec(), 50);
    remote.state.lock().competitor = Some(("/workspace/race".to_owned(), competitor));
    let error = fs
        .write_text(
            &fs.resolve("race", None, None).await.unwrap(),
            "ours",
            Some(&FsWriteIntent::CreateIfAbsent),
            None,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(code(&error), Some(FsErrorCode::FsNotObserved));
    assert_eq!(
        remote.state.lock().nodes["/workspace/race"].data,
        b"competitor"
    );

    remote.state.lock().guarded_output = Some("unexpected".to_owned());
    let invalid = fs.resolve("invalid", None, None).await.unwrap();
    assert_eq!(
        code(
            &fs.write_text(
                &invalid,
                "ours",
                Some(&FsWriteIntent::CreateIfAbsent),
                None,
                None
            )
            .await
            .unwrap_err()
        ),
        Some(FsErrorCode::FsIoError)
    );
    assert!(!remote.state.lock().nodes.contains_key("/workspace/invalid"));

    let signal = AbortSignal::default();
    remote.state.lock().abort_after_rename = Some(signal.clone());
    let target = fs.resolve("abort-after", None, None).await.unwrap();
    assert!(
        fs.write_text(&target, "yes", None, Some(&signal), None)
            .await
            .is_ok()
    );
    assert!(signal.is_aborted());
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one mutation ledger shares the same target and injected failure sequence"
)]
async fn mutation_failures_preserve_targets_cleanup_staging_and_keep_binary_diff_optional() {
    let remote = Arc::new(FakeRemote::default());
    remote.dir("/workspace");
    remote.file("/workspace/file", vec![0xff]);
    let fs = setup(remote.clone());
    let file = fs.resolve("file", None, None).await.unwrap();
    let outcome = fs
        .write_text(&file, "valid", None, None, None)
        .await
        .unwrap();
    assert_eq!(outcome.before, None);

    remote.mutate("/workspace/file", b"prior".to_vec());
    remote.state.lock().next_read_error =
        Some(Failure::Message("read transport failed".to_owned()));
    assert_eq!(
        code(
            &fs.write_text(&file, "replacement", None, None, None)
                .await
                .unwrap_err()
        ),
        Some(FsErrorCode::FsIoError)
    );
    assert_eq!(remote.state.lock().nodes["/workspace/file"].data, b"prior");

    remote.state.lock().make_dir_result = Some(false);
    assert_eq!(
        code(
            &fs.write_text(
                &fs.resolve("collision", None, None).await.unwrap(),
                "x",
                None,
                None,
                None
            )
            .await
            .unwrap_err()
        ),
        Some(FsErrorCode::FsIoError)
    );
    let removals = remote.state.lock().removals.len();
    let command_target = fs.resolve("command", None, None).await.unwrap();
    remote.state.lock().next_command_error = Some(Failure::CommandExit("chmod failed".to_owned()));
    assert_eq!(
        code(
            &fs.write_text(&command_target, "x", None, None, None)
                .await
                .unwrap_err()
        ),
        Some(FsErrorCode::FsIoError)
    );
    assert!(remote.state.lock().removals.len() > removals);

    remote.state.lock().next_rename_error = Some(Failure::Message("permission denied".to_owned()));
    assert_eq!(
        code(
            &fs.write_text(
                &fs.resolve("permission", None, None).await.unwrap(),
                "x",
                None,
                None,
                None
            )
            .await
            .unwrap_err()
        ),
        Some(FsErrorCode::FsPermissionDenied)
    );
    remote.state.lock().next_rename_error = Some(Failure::Aborted);
    assert_eq!(
        code(
            &fs.write_text(
                &fs.resolve("abort", None, None).await.unwrap(),
                "x",
                None,
                None,
                None
            )
            .await
            .unwrap_err()
        ),
        Some(FsErrorCode::FsAborted)
    );

    remote.mutate(
        "/workspace/late-nul",
        [vec![b'a'; 8192], b"\0tail".to_vec()].concat(),
    );
    assert_eq!(
        code(
            &fs.edit_text(
                &fs.resolve("late-nul", None, None).await.unwrap(),
                &FsEditRequest {
                    old_string: "tail".to_owned(),
                    new_string: "end".to_owned(),
                    replace_all: false,
                },
                None,
                None,
                None
            )
            .await
            .unwrap_err()
        ),
        Some(FsErrorCode::FsNotText)
    );
}

#[tokio::test]
async fn literal_edits_restore_crlf_and_serialize_version_guards() {
    let remote = Arc::new(FakeRemote::default());
    remote.file("/workspace/file.txt", b"one\r\ntwo\r\nthree\n".to_vec());
    let fs = setup(remote.clone());
    let file = fs.resolve("file.txt", None, None).await.unwrap();
    let version = fs.stat(&file, None).await.unwrap().unwrap().version;
    let edited = fs
        .edit_text(
            &file,
            &FsEditRequest {
                old_string: "two\r\n".to_owned(),
                new_string: "TWO\r\n".to_owned(),
                replace_all: false,
            },
            Some(&version),
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(edited.before, "one\ntwo\nthree\n");
    assert_eq!(edited.after, "one\nTWO\nthree\n");
    assert_eq!(
        remote.state.lock().nodes["/workspace/file.txt"].data,
        b"one\r\nTWO\r\nthree\r\n"
    );

    remote.mutate("/workspace/file.txt", b"a a".to_vec());
    for (request, expected) in [
        (
            FsEditRequest {
                old_string: String::new(),
                new_string: "x".to_owned(),
                replace_all: false,
            },
            FsErrorCode::FsEditNotFound,
        ),
        (
            FsEditRequest {
                old_string: "z".to_owned(),
                new_string: "x".to_owned(),
                replace_all: false,
            },
            FsErrorCode::FsEditNotFound,
        ),
        (
            FsEditRequest {
                old_string: "a".to_owned(),
                new_string: "x".to_owned(),
                replace_all: false,
            },
            FsErrorCode::FsAmbiguousEdit,
        ),
    ] {
        assert_eq!(
            code(
                &fs.edit_text(&file, &request, None, None, None)
                    .await
                    .unwrap_err()
            ),
            Some(expected)
        );
    }

    remote.mutate("/workspace/file.txt", b"base".to_vec());
    let version = fs.stat(&file, None).await.unwrap().unwrap().version;
    let write_intent = FsWriteIntent::ReplaceIfVersion {
        version: version.clone(),
    };
    let edit_request = FsEditRequest {
        old_string: "base".to_owned(),
        new_string: "two".to_owned(),
        replace_all: false,
    };
    let write = fs.write_text(&file, "one", Some(&write_intent), None, None);
    let edit = fs.edit_text(&file, &edit_request, Some(&version), None, None);
    let (write, edit) = tokio::join!(write, edit);
    assert_ne!(write.is_ok(), edit.is_ok());
    assert_eq!(
        code(&write.err().or_else(|| edit.err()).unwrap()),
        Some(FsErrorCode::FsStaleVersion)
    );
}

#[tokio::test]
async fn maps_abort_permission_transport_and_listing_failures() {
    let remote = Arc::new(FakeRemote::default());
    remote.dir("/workspace");
    remote.file("/workspace/a", b"a".to_vec());
    let fs = setup(remote.clone());
    let aborted = AbortSignal::default();
    aborted.abort();
    assert_eq!(
        code(&fs.resolve("a", None, Some(&aborted)).await.unwrap_err()),
        Some(FsErrorCode::FsAborted)
    );
    remote.state.lock().next_command_error =
        Some(Failure::CommandExit("not a directory".to_owned()));
    assert_eq!(
        code(&fs.resolve("bad", None, None).await.unwrap_err()),
        Some(FsErrorCode::FsIoError)
    );
    let a = fs.resolve("a", None, None).await.unwrap();
    remote.state.lock().next_read_error =
        Some(Failure::Message("operation not permitted".to_owned()));
    assert_eq!(
        code(&fs.read_text(&a, None).await.unwrap_err()),
        Some(FsErrorCode::FsPermissionDenied)
    );
    remote.state.lock().next_read_error = Some(Failure::Aborted);
    assert_eq!(
        code(&fs.read_text(&a, None).await.unwrap_err()),
        Some(FsErrorCode::FsAborted)
    );
    remote.state.lock().next_list_error =
        Some(Failure::Message("listing transport failed".to_owned()));
    assert_eq!(
        code(&fs.list_dir(&target("/workspace"), None).await.unwrap_err()),
        Some(FsErrorCode::FsIoError)
    );
    assert_eq!(
        code(&fs.resolve("   ", None, None).await.unwrap_err()),
        Some(FsErrorCode::FsNotFound)
    );
}
