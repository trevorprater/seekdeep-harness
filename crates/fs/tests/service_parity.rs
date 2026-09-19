//! Behavioral mirror of `packages/fs/fs/tests/service.spec.ts`.

use std::{collections::HashMap, sync::Arc};

use futures::{StreamExt as _, stream};
use parking_lot::Mutex;
use seekdeep_cordis::Context;
use seekdeep_fs::{
    FS, FileSystem, FileSystemService, FsDirEntry, FsEditOutcome, FsEditRequest, FsError,
    FsErrorCode, FsInfo, FsKind, FsPathInfo, FsPathKind, FsTarget, FsTargetKey, FsVersion,
    FsWriteIntent, FsWriteOperation, FsWriteOutcome,
};
use seekdeep_llm::AbortSignal;
use seekdeep_sandbox::SandboxExecutionPolicy;

#[derive(Debug, Default)]
struct FakeFileSystem {
    files: Mutex<HashMap<String, String>>,
}

impl FakeFileSystem {
    fn insert(&self, path: &str, content: &str) {
        self.files
            .lock()
            .insert(path.to_owned(), content.to_owned());
    }
}

#[async_trait::async_trait]
impl FileSystem for FakeFileSystem {
    async fn resolve(
        &self,
        path: &str,
        _cwd: Option<&str>,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<FsTarget> {
        Ok(FsTarget {
            target_key: FsTargetKey::new(path),
            display_path: path.to_owned(),
        })
    }

    fn process_path(&self, target: &FsTarget) -> String {
        target.target_key.to_string()
    }

    fn file_url(&self, target: &FsTarget) -> String {
        format!("file:///{}", target.target_key)
    }

    fn contains(&self, parent: &FsTarget, child: &FsTarget) -> bool {
        child.target_key == parent.target_key
            || child
                .target_key
                .as_str()
                .strip_prefix(parent.target_key.as_str())
                .is_some_and(|suffix| suffix.starts_with('/'))
    }

    async fn stat(
        &self,
        target: &FsTarget,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Option<FsInfo>> {
        Ok(self
            .files
            .lock()
            .get(target.target_key.as_str())
            .map(|value| FsInfo {
                version: FsVersion::new("v1"),
                kind: FsKind::File,
                size: Some(u64::try_from(value.len()).expect("test content length")),
            }))
    }

    async fn lstat(
        &self,
        path: &str,
        _cwd: Option<&str>,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Option<FsPathInfo>> {
        Ok(self.files.lock().get(path).map(|value| FsPathInfo {
            version: FsVersion::new("v1"),
            kind: FsPathKind::File,
            size: Some(u64::try_from(value.len()).expect("test content length")),
        }))
    }

    async fn read_text(
        &self,
        target: &FsTarget,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<String> {
        self.files
            .lock()
            .get(target.target_key.as_str())
            .cloned()
            .ok_or_else(|| {
                FsError::new(
                    format!("not found: {}", target.display_path),
                    FsErrorCode::FsNotFound,
                )
                .into()
            })
    }

    async fn stream_text(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<futures::stream::BoxStream<'static, anyhow::Result<String>>> {
        let content = self.read_text(target, signal).await?;
        Ok(Box::pin(stream::iter([Ok(content)])))
    }

    async fn read_bytes(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
        max_bytes: usize,
    ) -> anyhow::Result<Vec<u8>> {
        let bytes = self.read_text(target, signal).await?.into_bytes();
        if bytes.len() > max_bytes {
            return Err(FsError::new(
                format!("too large: {}", target.display_path),
                FsErrorCode::FsTooLarge,
            )
            .into());
        }
        Ok(bytes)
    }

    async fn list_dir(
        &self,
        target: &FsTarget,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Vec<FsDirEntry>> {
        if target.target_key.as_str() != "skills" {
            return Err(FsError::new(
                format!("not a directory: {}", target.display_path),
                FsErrorCode::FsNotDirectory,
            )
            .into());
        }
        Ok(vec![FsDirEntry {
            name: "alpha.md".to_owned(),
            kind: FsKind::File,
            target: FsTarget {
                target_key: FsTargetKey::new("skills/alpha.md"),
                display_path: "skills/alpha.md".to_owned(),
            },
            version: Some(FsVersion::new("v1")),
            size: Some(2),
        }])
    }

    async fn write_text(
        &self,
        target: &FsTarget,
        content: &str,
        _expected: Option<&FsWriteIntent>,
        _signal: Option<&AbortSignal>,
        _sandbox_policy: Option<&SandboxExecutionPolicy>,
    ) -> anyhow::Result<FsWriteOutcome> {
        let before = self
            .files
            .lock()
            .insert(target.target_key.to_string(), content.to_owned());
        Ok(FsWriteOutcome {
            operation: if before.is_some() {
                FsWriteOperation::Update
            } else {
                FsWriteOperation::Create
            },
            version: FsVersion::new("v2"),
            before,
            after: content.to_owned(),
        })
    }

    async fn edit_text(
        &self,
        target: &FsTarget,
        edit: &FsEditRequest,
        _expected: Option<&FsVersion>,
        _signal: Option<&AbortSignal>,
        _sandbox_policy: Option<&SandboxExecutionPolicy>,
    ) -> anyhow::Result<FsEditOutcome> {
        let before = self
            .files
            .lock()
            .get(target.target_key.as_str())
            .cloned()
            .unwrap_or_default();
        let after = if edit.replace_all {
            before.replace(&edit.old_string, &edit.new_string)
        } else {
            before.replacen(&edit.old_string, &edit.new_string, 1)
        };
        self.files
            .lock()
            .insert(target.target_key.to_string(), after.clone());
        Ok(FsEditOutcome {
            version: FsVersion::new("v3"),
            before,
            after,
        })
    }
}

fn provided(context: &Context) -> (Arc<FakeFileSystem>, Arc<FileSystemService>) {
    let fake = Arc::new(FakeFileSystem::default());
    let service = FileSystemService::new(fake.clone());
    service.provide(context).expect("provide fs");
    (fake, service)
}

#[tokio::test]
async fn provider_registers_serves_primitives_and_disposes() -> anyhow::Result<()> {
    let context = Context::new();
    let (fake, _) = provided(&context);
    let service = context.get(FS).expect("ctx.fs");
    let fs = service.filesystem();
    assert_eq!(fs.sandbox_mode(), None);

    fake.insert("a.txt", "one\ntwo");
    let target = fs.resolve("a.txt", None, None).await.expect("resolve");
    assert_eq!(fs.process_path(&target), "a.txt");
    assert_eq!(fs.file_url(&target), "file:///a.txt");
    assert!(fs.contains(&target, &target));
    assert_eq!(
        fs.stat(&target, None)
            .await
            .expect("stat")
            .map(|info| info.kind),
        Some(FsKind::File)
    );
    assert_eq!(fs.read_text(&target, None).await.expect("read"), "one\ntwo");
    let streamed = fs
        .stream_text(&target, None)
        .await
        .expect("stream")
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<anyhow::Result<Vec<_>>>()?
        .concat();
    assert_eq!(streamed, "one\ntwo");
    assert_eq!(
        fs.read_bytes(&target, None, 7).await.expect("bytes"),
        b"one\ntwo"
    );
    let too_large = fs
        .read_bytes(&target, None, 6)
        .await
        .expect_err("byte bound");
    assert_eq!(
        too_large.downcast_ref::<FsError>().map(|error| error.code),
        Some(FsErrorCode::FsTooLarge)
    );
    assert!(
        fs.stat(&fs.resolve("missing", None, None).await?, None)
            .await?
            .is_none()
    );
    assert_eq!(
        fs.lstat("a.txt", None, None).await?.map(|info| info.kind),
        Some(FsPathKind::File)
    );
    assert!(fs.lstat("missing", None, None).await?.is_none());

    let skills = fs.resolve("skills", None, None).await?;
    assert_eq!(
        fs.list_dir(&skills, None).await?,
        [FsDirEntry {
            name: "alpha.md".to_owned(),
            kind: FsKind::File,
            target: FsTarget {
                target_key: FsTargetKey::new("skills/alpha.md"),
                display_path: "skills/alpha.md".to_owned(),
            },
            version: Some(FsVersion::new("v1")),
            size: Some(2),
        }]
    );

    context.fiber().dispose().await.expect("dispose");
    assert!(context.get(FS).is_none());
    Ok::<_, anyhow::Error>(())
}

#[tokio::test]
async fn duplicate_provider_is_rejected_without_replacing_the_first() {
    let context = Context::new();
    let (_, first) = provided(&context);
    let duplicate = FileSystemService::new(Arc::new(FakeFileSystem::default()));
    assert!(duplicate.provide(&context).is_err());
    assert!(Arc::ptr_eq(
        &context.get(FS).expect("first remains"),
        &first
    ));
    context.fiber().dispose().await.expect("dispose");
}

#[test]
fn branded_ids_preserve_the_wire_string() {
    assert_eq!(FsTargetKey::new("k").as_str(), "k");
    assert_eq!(FsVersion::new("v").as_str(), "v");
}

#[test]
fn fs_error_carries_stable_name_code_and_cause() {
    use std::error::Error as _;

    let error = FsError::new("nope", FsErrorCode::FsNotFound);
    assert_eq!(error.code, FsErrorCode::FsNotFound);
    assert_eq!(error.code.as_str(), "FS_NOT_FOUND");
    assert_eq!(error.name(), "FsError");
    assert!(error.source().is_none());

    let caused = FsError::new("cannot read", FsErrorCode::FsAborted).with_cause(
        std::io::Error::new(std::io::ErrorKind::PermissionDenied, "EACCES"),
    );
    assert_eq!(caused.code, FsErrorCode::FsAborted);
    assert_eq!(
        caused.source().map(ToString::to_string).as_deref(),
        Some("EACCES")
    );
}
