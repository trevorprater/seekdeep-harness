//! Private local-filesystem implementation of the spill storage seam.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use path_clean::PathClean as _;
use seekdeep_cordis::Context;
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use seekdeep_spill::{SaveTextSpill, SpillBackend, SpillLocator, SpillRef, SpillStore};
use serde::{Deserialize, Serialize};

pub mod store;

pub use store::{
    SaveTextOptions, SavedText, encode_segment, private_root, save_text_file, session_dir,
};

/// Exact local-path retrieval guidance shown to the model.
pub const RETRIEVAL_HINT: &str =
    "Use read with offset/limit, or grep this path to search within it.";

/// Local spill backend configuration.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalSpillConfig {
    /// Configured spill root; omitted uses a private per-process temp directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<PathBuf>,
}

/// Local-filesystem spill backend.
#[derive(Clone, Debug)]
pub struct LocalSpillStore {
    /// Absolute root fixed at construction.
    pub root: PathBuf,
}

impl LocalSpillStore {
    /// Resolves a configured root or creates/reuses the private default root.
    ///
    /// # Errors
    ///
    /// Returns current-directory or private-temp-directory creation failures.
    pub fn new(config: &LocalSpillConfig) -> anyhow::Result<Self> {
        let root = if let Some(root) = &config.root {
            absolute(root)?
        } else {
            private_root()?
        };
        Ok(Self { root })
    }
}

fn absolute(path: &Path) -> std::io::Result<PathBuf> {
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    }
    .clean())
}

#[async_trait]
impl SpillBackend for LocalSpillStore {
    async fn save_text(&self, input: SaveTextSpill) -> anyhow::Result<SpillRef> {
        let saved = save_text_file(SaveTextOptions {
            root: self.root.clone(),
            session_id: input.owner.session_id.into_string(),
            suggested_name: input.suggested_name,
            content: input.content,
        })
        .await?;
        Ok(SpillRef {
            locator: SpillLocator::new(saved.path.to_string_lossy().into_owned()),
            bytes: saved.bytes,
            retrieval_hint: RETRIEVAL_HINT.to_owned(),
        })
    }
}

/// Installs the local backend as the lifecycle-owned `spillStore` service.
///
/// # Errors
///
/// Returns path, temp-directory, or Cordis service-registration failures.
pub fn install(
    context: &Context,
    config: &LocalSpillConfig,
) -> anyhow::Result<Arc<LocalSpillStore>> {
    let backend = Arc::new(LocalSpillStore::new(config)?);
    Arc::new(SpillStore::new(backend.clone())).provide(context)?;
    Ok(backend)
}

/// Registers the local spill package's explained empty invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register("seekdeep-spill-local", InvariantInstaller::noop())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use seekdeep_core::session::SessionId;
    use seekdeep_llm::CallId;
    use seekdeep_spill::{SPILL_STORE, SpillOwner, SpillSource};
    use tempfile::TempDir;

    use super::*;

    fn request(content: &str) -> SaveTextSpill {
        SaveTextSpill {
            owner: SpillOwner {
                session_id: SessionId::new("sess-1"),
            },
            source: SpillSource {
                tool_name: "web_fetch".to_owned(),
                call_id: CallId::new("call-1"),
                label: "result".to_owned(),
            },
            suggested_name: "web_fetch.txt".to_owned(),
            content: content.to_owned(),
        }
    }

    #[test]
    fn session_directory_is_stable_distinct_and_below_root() {
        let first = session_dir("/spill", "sess-1");
        assert_eq!(first, session_dir("/spill", "sess-1"));
        assert_eq!(first.parent(), Some(Path::new("/spill")));
        assert!(
            first
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("session-")
        );
        assert_eq!(first.file_name().unwrap().len(), "session-".len() + 12);
        assert_ne!(first, session_dir("/spill", "sess-2"));
    }

    #[tokio::test]
    async fn writes_verbatim_under_session_with_safe_distinct_owner_only_names() {
        let temp = TempDir::new().unwrap();
        let first = save_text_file(SaveTextOptions {
            root: temp.path().to_path_buf(),
            session_id: "sess-1".to_owned(),
            suggested_name: "r.txt".to_owned(),
            content: "héllo".to_owned(),
        })
        .await
        .unwrap();
        let second = save_text_file(SaveTextOptions {
            root: temp.path().to_path_buf(),
            session_id: "sess-1".to_owned(),
            suggested_name: "../../evil".to_owned(),
            content: "x".to_owned(),
        })
        .await
        .unwrap();
        assert_eq!(fs::read_to_string(&first.path).unwrap(), "héllo");
        assert_eq!(first.bytes, "héllo".len() as u64);
        assert_eq!(
            first.path.parent(),
            Some(session_dir(temp.path(), "sess-1").as_path())
        );
        assert_ne!(first.path, second.path);
        assert_eq!(second.path.parent(), first.path.parent());
        assert!(
            !second
                .path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .contains('/')
        );
        let prefix = first
            .path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .split('-')
            .next()
            .unwrap()
            .to_owned();
        assert_eq!(prefix.len(), 12);
        assert!(prefix.bytes().all(|byte| byte.is_ascii_hexdigit()));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(first.path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(&first.path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[tokio::test]
    async fn service_honors_root_returns_exact_reference_and_propagates_storage_failure() {
        let temp = TempDir::new().unwrap();
        let context = Context::new();
        let backend = install(
            &context,
            &LocalSpillConfig {
                root: Some(temp.path().to_path_buf()),
            },
        )
        .unwrap();
        let reference = context
            .get(SPILL_STORE)
            .unwrap()
            .save_text(request("the full body"))
            .await
            .unwrap();
        let path = PathBuf::from(reference.locator.as_str());
        assert_eq!(
            path.parent(),
            Some(session_dir(temp.path(), "sess-1").as_path())
        );
        assert_eq!(fs::read_to_string(path).unwrap(), "the full body");
        assert_eq!(reference.bytes, "the full body".len() as u64);
        assert_eq!(reference.retrieval_hint, RETRIEVAL_HINT);
        assert_eq!(backend.root, temp.path());

        let bad_context = Context::new();
        let file_root = temp.path().join("not-a-directory");
        fs::write(&file_root, "file").unwrap();
        install(
            &bad_context,
            &LocalSpillConfig {
                root: Some(file_root),
            },
        )
        .unwrap();
        assert!(
            bad_context
                .get(SPILL_STORE)
                .unwrap()
                .save_text(request("body"))
                .await
                .is_err()
        );
    }

    #[test]
    fn configured_and_default_roots_are_absolute_stable_and_private() {
        let configured = LocalSpillStore::new(&LocalSpillConfig {
            root: Some(PathBuf::from(".")),
        })
        .unwrap();
        assert!(configured.root.is_absolute());
        let first = LocalSpillStore::new(&LocalSpillConfig::default()).unwrap();
        let second = LocalSpillStore::new(&LocalSpillConfig::default()).unwrap();
        assert_eq!(first.root, second.root);
        assert!(first.root.is_absolute());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(&first.root).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
    }

    #[test]
    fn invariant_companion_reserves_package() {
        let context = Context::new();
        let registry = Arc::new(
            InvariantRegistry::new(&context, &seekdeep_invariants::InvariantConfig::default())
                .unwrap(),
        );
        let _registration = register_invariant(&registry).unwrap();
        assert!(registry.is_registered("seekdeep-spill-local"));
    }
}
