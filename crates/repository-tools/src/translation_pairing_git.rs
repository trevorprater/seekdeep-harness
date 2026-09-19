//! Git-blob operations for bilingual pairing snapshots and staged content.

use std::{
    io::Write as _,
    path::Path,
    process::{Command, Stdio},
};

use regex::Regex;
use sha1::{Digest as _, Sha1};

const SNAPSHOT_REF_PREFIX: &str = "refs/seekdeep/translation-pairing/snapshots";

/// Maximum captured stdout or stderr for repository-owned Git subprocesses.
pub const GIT_COMMAND_MAX_BUFFER: usize = 1 << 26;

/// One regular stage-zero Git index entry and its exact blob bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitIndexBlob {
    /// Object ID recorded in the index.
    pub object_id: String,
    /// Exact bytes stored under the object ID.
    pub content: Vec<u8>,
}

/// Computes the full SHA-1 Git blob hash used by pairing records.
#[must_use]
pub fn git_blob_hash(content: &[u8]) -> String {
    let mut hash = Sha1::new();
    hash.update(format!("blob {}\0", content.len()).as_bytes());
    hash.update(content);
    hex::encode(hash.finalize())
}

/// Runs one Git subprocess and returns exact stdout bytes.
///
/// # Errors
///
/// Returns spawn, stdin, wait, output-cap, or nonzero-status diagnostics.
pub fn run_git(
    root: &Path,
    args: &[String],
    operation: &str,
    input: Option<&[u8]>,
) -> anyhow::Result<Vec<u8>> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = if let Some(input) = input {
        let mut child = command
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|error| anyhow::anyhow!("{operation} failed: {error}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(input)?;
        }
        child.wait_with_output()?
    } else {
        command
            .output()
            .map_err(|error| anyhow::anyhow!("{operation} failed: {error}"))?
    };
    if output.stdout.len() > GIT_COMMAND_MAX_BUFFER || output.stderr.len() > GIT_COMMAND_MAX_BUFFER
    {
        anyhow::bail!("{operation} failed: Git output exceeded {GIT_COMMAND_MAX_BUFFER} bytes");
    }
    if !output.status.success() {
        let status = output
            .status
            .code()
            .map_or_else(|| "null".to_owned(), |status| status.to_string());
        anyhow::bail!(
            "{operation} failed with status {status}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

/// Reads one path from the Git index without consulting working-tree bytes.
///
/// # Errors
///
/// Returns Git failures, invalid index states, or unresolved stages.
pub fn read_git_index_blob(root: &Path, path: &str) -> anyhow::Result<Option<GitIndexBlob>> {
    static INDEX_ENTRY: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"^(?:\d+) ([0-9a-f]+) 0\t[\s\S]+$").expect("static index-entry regex")
    });
    let output = run_git(
        root,
        &[
            "ls-files".to_owned(),
            "--stage".to_owned(),
            "-z".to_owned(),
            "--".to_owned(),
            path.to_owned(),
        ],
        &format!("git ls-files --stage for {path}"),
        None,
    )?;
    let output = String::from_utf8_lossy(&output);
    let entries = output
        .split('\0')
        .filter(|entry| !entry.is_empty())
        .collect::<Vec<_>>();
    if entries.is_empty() {
        return Ok(None);
    }
    if entries.len() != 1 {
        anyhow::bail!("{path} does not have exactly one resolved index entry");
    }
    let Some(object_id) = INDEX_ENTRY
        .captures(entries[0])
        .and_then(|captures| captures.get(1))
        .map(|capture| capture.as_str().to_owned())
    else {
        anyhow::bail!("{path} remains unmerged or has an invalid index entry");
    };
    let content = run_git(
        root,
        &["cat-file".to_owned(), "blob".to_owned(), object_id.clone()],
        &format!("reading staged {path}"),
        None,
    )?;
    Ok(Some(GitIndexBlob { object_id, content }))
}

/// Persists exact bytes and pins them under a content-addressed snapshot ref.
///
/// # Errors
///
/// Returns Git failures or a non-SHA-1/unexpected stored object ID.
pub fn store_git_blob(root: &Path, content: &[u8]) -> anyhow::Result<String> {
    let expected = git_blob_hash(content);
    let stored = run_git(
        root,
        &[
            "hash-object".to_owned(),
            "-w".to_owned(),
            "--stdin".to_owned(),
        ],
        "git hash-object -w --stdin",
        Some(content),
    )?;
    let stored = String::from_utf8_lossy(&stored).trim().to_owned();
    if stored != expected {
        anyhow::bail!(
            "git hash-object -w --stdin returned unexpected object ID {}; expected {expected}",
            serde_json::to_string(&stored)?
        );
    }
    run_git(
        root,
        &[
            "update-ref".to_owned(),
            format!("{SNAPSHOT_REF_PREFIX}/{stored}"),
            stored.clone(),
        ],
        "git update-ref for translation snapshot",
        None,
    )?;
    Ok(stored)
}
