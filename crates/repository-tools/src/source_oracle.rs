//! Read-only access to the original files at the repository's pinned source revision.

use std::{
    collections::HashSet,
    path::{Component, Path, PathBuf},
    process::Command,
};

use anyhow::Context as _;

/// Original source files read directly from Git objects, independent of build output.
pub struct SourceOracle {
    root: PathBuf,
    revision: String,
}

impl SourceOracle {
    /// Checkout containing the pinned Git objects and source tooling dependencies.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolves the oracle override, adjacent checkout, or recorded local checkout.
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
    /// Rejects malformed snapshot metadata, unavailable Git, and revision drift.
    pub fn open_with_root(repository: &Path, source: Option<&Path>) -> anyhow::Result<Self> {
        let revision = crate::doc_source_links::oracle_revision(repository)?;
        let snapshot = std::fs::read_to_string(repository.join("SOURCE_SNAPSHOT"))?;
        let recorded = snapshot
            .lines()
            .find_map(|line| line.strip_prefix("repository="))
            .ok_or_else(|| anyhow::anyhow!("SOURCE_SNAPSHOT has no repository."))?;
        let adjacent = repository.join("../deepseek-harness");
        let root = source.map_or_else(
            || {
                if adjacent.is_dir() {
                    adjacent
                } else {
                    PathBuf::from(recorded)
                }
            },
            Path::to_path_buf,
        );
        let oracle = Self { root, revision };
        let head = oracle.git(&["rev-parse", "HEAD"])?;
        anyhow::ensure!(
            head.trim() == oracle.revision,
            "Source oracle {} has revision {}; expected {} from SOURCE_SNAPSHOT.",
            oracle.root.display(),
            head.trim(),
            oracle.revision
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
        self.git(&["show", &format!("{}:{relative}", self.revision)])
    }

    /// Lists the files and enclosing directories present in the pinned commit.
    ///
    /// # Errors
    /// Returns Git failures and non-UTF-8 path diagnostics.
    pub fn paths(&self) -> anyhow::Result<HashSet<String>> {
        let listing = self.git(&["ls-tree", "-r", "--name-only", "-z", &self.revision])?;
        let mut paths = HashSet::new();
        for file in listing.split('\0').filter(|path| !path.is_empty()) {
            paths.insert(file.to_owned());
            let mut parent = file;
            while let Some((directory, _)) = parent.rsplit_once('/') {
                paths.insert(directory.to_owned());
                parent = directory;
            }
        }
        Ok(paths)
    }

    fn git(&self, args: &[&str]) -> anyhow::Result<String> {
        let output = Command::new("git")
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
