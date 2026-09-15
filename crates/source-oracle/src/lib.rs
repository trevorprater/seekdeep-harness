//! One pinned source checkout shared by native tests and repository tooling.

use std::{
    collections::HashSet,
    path::{Component, Path, PathBuf},
    process::Command,
};

use anyhow::Context as _;

/// Full Git object ID recorded in `SOURCE_SNAPSHOT`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceRevision(String);

impl SourceRevision {
    /// Reads and validates the repository's source revision.
    ///
    /// # Errors
    /// Rejects absent snapshot metadata and malformed Git object IDs.
    pub fn read(repository: &Path) -> anyhow::Result<Self> {
        let snapshot = std::fs::read_to_string(repository.join("SOURCE_SNAPSHOT"))?;
        Self::parse(&snapshot)
    }

    fn parse(snapshot: &str) -> anyhow::Result<Self> {
        let revision = snapshot
            .lines()
            .find_map(|line| line.strip_prefix("commit="))
            .ok_or_else(|| anyhow::anyhow!("SOURCE_SNAPSHOT has no commit."))?;
        anyhow::ensure!(
            revision.len() == 40 && revision.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "SOURCE_SNAPSHOT commit must be a full Git object ID."
        );
        Ok(Self(revision.to_owned()))
    }

    /// Exact object ID used by Git and source-link URLs.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Original files read from Git objects, independent of generated build output.
pub struct SourceOracle {
    root: PathBuf,
    revision: SourceRevision,
}

impl SourceOracle {
    /// Checkout containing the pinned objects and source tooling dependencies.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolves `SEEKDEEP_PARITY_SOURCE`, an adjacent checkout, or the recorded path.
    ///
    /// # Errors
    /// Rejects unavailable checkouts and revisions that differ from `SOURCE_SNAPSHOT`.
    pub fn open(repository: &Path) -> anyhow::Result<Self> {
        let configured = std::env::var_os("SEEKDEEP_PARITY_SOURCE").map(PathBuf::from);
        Self::open_with_root(repository, configured.as_deref())
    }

    /// Opens an explicit oracle or resolves the repository's checkout locations.
    ///
    /// # Errors
    /// Rejects malformed metadata, unavailable Git, and revision drift.
    pub fn open_with_root(repository: &Path, source: Option<&Path>) -> anyhow::Result<Self> {
        let snapshot = std::fs::read_to_string(repository.join("SOURCE_SNAPSHOT"))?;
        let revision = SourceRevision::parse(&snapshot)?;
        let root = source_location(repository, source)?;
        let root = root
            .canonicalize()
            .with_context(|| format!("Resolve source oracle at {}", root.display()))?;
        let oracle = Self { root, revision };
        let head = oracle.git(&["rev-parse", "HEAD"])?;
        anyhow::ensure!(
            head.trim() == oracle.revision.as_str(),
            "Source oracle {} has revision {}; expected {} from SOURCE_SNAPSHOT.",
            oracle.root.display(),
            head.trim(),
            oracle.revision.as_str()
        );
        Ok(oracle)
    }

    /// Reads an original, repository-relative file at the pinned revision.
    ///
    /// # Errors
    /// Rejects paths outside the repository and files absent from the pinned commit.
    pub fn read(&self, relative: &str) -> anyhow::Result<String> {
        anyhow::ensure!(
            !relative.is_empty()
                && Path::new(relative)
                    .components()
                    .all(|part| matches!(part, Component::Normal(_))),
            "Invalid source oracle path: {relative}"
        );
        self.git(&["show", &format!("{}:{relative}", self.revision.as_str())])
    }

    /// Lists the original tracked files in Git tree order.
    ///
    /// # Errors
    /// Returns Git failures and non-UTF-8 path diagnostics.
    pub fn files(&self) -> anyhow::Result<Vec<String>> {
        let listing = self.git(&["ls-tree", "-r", "--name-only", "-z", self.revision.as_str()])?;
        Ok(listing
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .collect())
    }

    /// Lists the files and enclosing directories present in the pinned commit.
    ///
    /// # Errors
    /// Returns Git failures and non-UTF-8 path diagnostics.
    pub fn paths(&self) -> anyhow::Result<HashSet<String>> {
        let mut paths = HashSet::new();
        for file in self.files()? {
            let mut parent = file.as_str();
            while let Some((directory, _)) = parent.rsplit_once('/') {
                paths.insert(directory.to_owned());
                parent = directory;
            }
            paths.insert(file);
        }
        Ok(paths)
    }

    fn git(&self, args: &[&str]) -> anyhow::Result<String> {
        let mut command = Command::new("git");
        // Git hooks export checkout-specific state which must not redirect this reader.
        for variable in [
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_CONFIG",
            "GIT_CONFIG_PARAMETERS",
            "GIT_CONFIG_COUNT",
            "GIT_OBJECT_DIRECTORY",
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_IMPLICIT_WORK_TREE",
            "GIT_GRAFT_FILE",
            "GIT_INDEX_FILE",
            "GIT_NO_REPLACE_OBJECTS",
            "GIT_REPLACE_REF_BASE",
            "GIT_PREFIX",
            "GIT_SHALLOW_FILE",
            "GIT_COMMON_DIR",
        ] {
            command.env_remove(variable);
        }
        let output = command
            .arg("--no-replace-objects")
            .args(args)
            .current_dir(&self.root)
            .output()
            .with_context(|| format!("Read source oracle at {}", self.root.display()))?;
        anyhow::ensure!(
            output.status.success(),
            "Source oracle Git command failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(String::from_utf8(output.stdout)?)
    }
}

/// Chooses the explicit, adjacent, or recorded path without requiring a checkout.
///
/// Commands whose source-dependent branch is optional can resolve their arguments
/// before they need the oracle's files or tools.
///
/// # Errors
/// Rejects absent snapshot metadata when no explicit location was supplied.
pub fn source_location(repository: &Path, source: Option<&Path>) -> anyhow::Result<PathBuf> {
    if let Some(source) = source {
        return Ok(source.to_owned());
    }
    let snapshot = std::fs::read_to_string(repository.join("SOURCE_SNAPSHOT"))?;
    let recorded = snapshot
        .lines()
        .find_map(|line| line.strip_prefix("repository="))
        .ok_or_else(|| anyhow::anyhow!("SOURCE_SNAPSHOT has no repository."))?;
    let adjacent = repository.join("../deepseek-harness");
    Ok(if adjacent.is_dir() {
        adjacent
    } else {
        PathBuf::from(recorded)
    })
}

/// Resolves and validates the source checkout used by an executable parity test.
///
/// # Errors
/// Rejects missing source metadata, unavailable checkouts, and source revision drift.
pub fn source_root(repository: &Path) -> anyhow::Result<PathBuf> {
    Ok(SourceOracle::open(repository)?.root)
}
