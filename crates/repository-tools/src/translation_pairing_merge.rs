//! Fail-closed composition of bilingual pairing records during Git merges.

use std::{
    collections::HashMap,
    fs,
    path::Path,
    process::{Command, ExitStatus, Stdio},
};

use path_clean::PathClean as _;
use regex::Regex;

use crate::{
    translation_pairing::{
        is_translation_scope_file, language_switcher_targets, links_to, parse_translation_markdown,
        requires_source_language_switcher, translation_structure_diff,
        translation_structure_signature,
    },
    translation_pairing_git::{
        GIT_COMMAND_MAX_BUFFER, git_blob_hash, read_git_index_blob, run_git, store_git_blob,
    },
    translation_pairing_record::{
        TranslationPairPaths, TranslationPairingRecord, parse_translation_pairing_record,
        render_translation_pairing_record, translation_pair_paths_from_metadata,
    },
};

/// A mechanically composed record and the exact merged owner contents it names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranslationPairingMergeResult {
    /// Canonical generated sidecar text.
    pub record: String,
    /// Clean three-way merge of the English owner.
    pub source_content: Vec<u8>,
    /// Git blob hash of `source_content`.
    pub source_hash: String,
    /// Clean three-way merge of the Simplified Chinese owner.
    pub zh_content: Vec<u8>,
    /// Git blob hash of `zh_content`.
    pub zh_hash: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct UnmergedStages {
    ancestor: Option<String>,
    current: Option<String>,
    other: Option<String>,
}

#[derive(Debug)]
struct CapturedCommand {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn run_git_captured(
    root: &Path,
    args: &[String],
    operation: &str,
) -> anyhow::Result<CapturedCommand> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| anyhow::anyhow!("{operation} failed: {error}"))?;
    if output.stdout.len() > GIT_COMMAND_MAX_BUFFER || output.stderr.len() > GIT_COMMAND_MAX_BUFFER
    {
        anyhow::bail!("{operation} failed: Git output exceeded {GIT_COMMAND_MAX_BUFFER} bytes");
    }
    Ok(CapturedCommand {
        status: output.status,
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

fn read_git_blob(root: &Path, object_id: &str, owner: &str) -> anyhow::Result<Vec<u8>> {
    let content = run_git(
        root,
        &[
            "cat-file".to_owned(),
            "blob".to_owned(),
            object_id.to_owned(),
        ],
        &format!("reading {owner} blob {object_id}"),
        None,
    )?;
    if git_blob_hash(&content) != object_id {
        anyhow::bail!("{owner} record names {object_id}, which is not its SHA-1 git blob hash");
    }
    Ok(content)
}

fn read_merge_default(root: &Path) -> anyhow::Result<Option<String>> {
    let output = run_git_captured(
        root,
        &[
            "config".to_owned(),
            "--get".to_owned(),
            "merge.default".to_owned(),
        ],
        "reading merge.default",
    )?;
    if output.status.code() == Some(1) {
        return Ok(None);
    }
    if !output.status.success() {
        anyhow::bail!(
            "reading merge.default failed with status {}: {}",
            status_text(output.status),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(Some(
        String::from_utf8_lossy(&output.stdout).trim().to_owned(),
    ))
}

fn assert_default_text_merge(root: &Path, paths: &TranslationPairPaths) -> anyhow::Result<()> {
    let output = run_git(
        root,
        &[
            "check-attr".to_owned(),
            "-z".to_owned(),
            "merge".to_owned(),
            "--".to_owned(),
            paths.source.clone(),
            paths.zh.clone(),
        ],
        "checking bilingual owner merge attributes",
        None,
    )?;
    let output = String::from_utf8_lossy(&output);
    let mut fields = output.split('\0').collect::<Vec<_>>();
    if fields.last() == Some(&"") {
        fields.pop();
    }
    let mut merge_default = None;
    for fields in fields.chunks(3) {
        let Some(path) = fields.first() else {
            anyhow::bail!("git check-attr returned a malformed result");
        };
        let Some(value) = fields.get(2) else {
            anyhow::bail!("git check-attr returned a malformed result");
        };
        if !matches!(*value, "unspecified" | "set" | "text") {
            anyhow::bail!(
                "{path} uses merge={value}; the pairing driver only composes Git's default text merge"
            );
        }
        if *value == "unspecified" {
            if merge_default.is_none() {
                merge_default = Some(read_merge_default(root)?);
            }
            if let Some(value) = merge_default.as_ref().and_then(Option::as_ref)
                && value != "text"
            {
                anyhow::bail!(
                    "{path} inherits merge.default={value}; the pairing driver only composes Git's default text merge"
                );
            }
        }
    }
    Ok(())
}

fn run_text_merge(
    root: &Path,
    label: &str,
    ancestor: &[u8],
    current: &[u8],
    other: &[u8],
) -> anyhow::Result<CapturedCommand> {
    let temporary = tempfile::Builder::new()
        .prefix("seekdeep-translation-pairing-merge-")
        .tempdir()?;
    let ancestor_path = temporary.path().join("ancestor");
    let current_path = temporary.path().join("current");
    let other_path = temporary.path().join("other");
    fs::write(&ancestor_path, ancestor)?;
    fs::write(&current_path, current)?;
    fs::write(&other_path, other)?;
    run_git_captured(
        root,
        &[
            "merge-file".to_owned(),
            "-p".to_owned(),
            "-L".to_owned(),
            format!("{label}:current"),
            "-L".to_owned(),
            format!("{label}:ancestor"),
            "-L".to_owned(),
            format!("{label}:other"),
            current_path.to_string_lossy().into_owned(),
            ancestor_path.to_string_lossy().into_owned(),
            other_path.to_string_lossy().into_owned(),
        ],
        &format!("merging {label}"),
    )
}

fn merge_blob_triplet(
    root: &Path,
    owner: &str,
    ancestor: &[u8],
    current: &[u8],
    other: &[u8],
) -> anyhow::Result<Vec<u8>> {
    let result = run_text_merge(root, owner, ancestor, current, other)?;
    if !result.status.success() {
        let status = result.status.code();
        let kind = if status.is_some_and(|status| (1..=127).contains(&status)) {
            "has content conflicts".to_owned()
        } else {
            format!("failed with status {}", status_text(result.status))
        };
        anyhow::bail!("{owner} {kind}");
    }
    Ok(result.stdout)
}

fn load_record_owners(
    root: &Path,
    label: &str,
    content: &str,
    paths: &TranslationPairPaths,
) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    let record = parse_translation_pairing_record(content, paths).ok_or_else(|| {
        anyhow::anyhow!(
            "{label} {} is not a valid two-hash pairing record",
            paths.metadata
        )
    })?;
    Ok((
        read_git_blob(
            root,
            &record.source_hash,
            &format!("{label} {}", paths.source),
        )?,
        read_git_blob(root, &record.zh_hash, &format!("{label} {}", paths.zh))?,
    ))
}

fn assert_merged_pair_structure(
    paths: &TranslationPairPaths,
    source: &[u8],
    zh: &[u8],
) -> anyhow::Result<()> {
    let source_tree =
        parse_translation_markdown(&String::from_utf8_lossy(source)).map_err(anyhow::Error::msg)?;
    let zh_tree =
        parse_translation_markdown(&String::from_utf8_lossy(zh)).map_err(anyhow::Error::msg)?;
    let source_switcher_targets = language_switcher_targets(&paths.source);
    let zh_switcher_targets = language_switcher_targets(&paths.zh);
    if requires_source_language_switcher(&paths.source)
        && !links_to(&source_tree, &zh_switcher_targets)
    {
        anyhow::bail!(
            "{} clean merge lost its language-switcher link to {}",
            paths.source,
            basename(&paths.zh)
        );
    }
    if !links_to(&zh_tree, &source_switcher_targets) {
        anyhow::bail!(
            "{} clean merge lost its language-switcher link to {}",
            paths.zh,
            basename(&paths.source)
        );
    }
    let divergences = translation_structure_diff(
        &translation_structure_signature(&source_tree, &zh_switcher_targets),
        &translation_structure_signature(&zh_tree, &source_switcher_targets),
    );
    if !divergences.is_empty() {
        anyhow::bail!(
            "{} and {} clean merges diverge structurally: {}",
            paths.source,
            paths.zh,
            divergences.join("; ")
        );
    }
    Ok(())
}

fn normalize_metadata_path(root: &Path, metadata: &str) -> anyhow::Result<String> {
    let metadata_path = Path::new(metadata);
    if metadata_path.is_absolute() {
        anyhow::bail!(
            "pairing record must be repository-relative: {}",
            serde_json::to_string(metadata)?
        );
    }
    let root = if root.is_absolute() {
        root.clean()
    } else {
        std::env::current_dir()?.join(root).clean()
    };
    let absolute = root.join(metadata_path).clean();
    let relative = absolute.strip_prefix(&root).map_err(|_| {
        anyhow::anyhow!(
            "pairing record escapes the repository: {}",
            serde_json::to_string(metadata).unwrap_or_else(|_| "\"\"".to_owned())
        )
    })?;
    if relative.as_os_str().is_empty() {
        anyhow::bail!(
            "pairing record escapes the repository: {}",
            serde_json::to_string(metadata)?
        );
    }
    relative_to_slashes(relative).ok_or_else(|| {
        anyhow::anyhow!(
            "pairing record is not valid UTF-8: {}",
            serde_json::to_string(metadata).unwrap_or_else(|_| "\"\"".to_owned())
        )
    })
}

/// Composes one generated sidecar from ancestor, current, and other records.
///
/// Each input record confirms its two owner blobs. A result exists only when
/// Git's default text merge succeeds independently for both languages and the
/// composed documents retain the pairing structure.
///
/// # Errors
///
/// Returns path, record, Git-object, merge-strategy, content-conflict,
/// language-switcher, structural-divergence, or object-persistence failures.
pub fn merge_translation_pairing_records(
    root: &Path,
    metadata_path: &str,
    ancestor_record: &str,
    current_record: &str,
    other_record: &str,
) -> anyhow::Result<TranslationPairingMergeResult> {
    let normalized_metadata = normalize_metadata_path(root, metadata_path)?;
    if !is_translation_scope_file(&normalized_metadata) {
        anyhow::bail!("{normalized_metadata} is outside the active bilingual documentation corpus");
    }
    let paths = translation_pair_paths_from_metadata(&normalized_metadata)?;
    assert_default_text_merge(root, &paths)?;
    let ancestor = load_record_owners(root, "ancestor", ancestor_record, &paths)?;
    let current = load_record_owners(root, "current", current_record, &paths)?;
    let other = load_record_owners(root, "other", other_record, &paths)?;
    let source_content =
        merge_blob_triplet(root, &paths.source, &ancestor.0, &current.0, &other.0)?;
    let zh_content = merge_blob_triplet(root, &paths.zh, &ancestor.1, &current.1, &other.1)?;
    assert_merged_pair_structure(&paths, &source_content, &zh_content)?;
    let source_hash = store_git_blob(root, &source_content)?;
    let zh_hash = store_git_blob(root, &zh_content)?;
    let record = render_translation_pairing_record(
        &paths,
        &TranslationPairingRecord {
            source_hash: source_hash.clone(),
            zh_hash: zh_hash.clone(),
        },
    );
    Ok(TranslationPairingMergeResult {
        record,
        source_content,
        source_hash,
        zh_content,
        zh_hash,
    })
}

fn unmerged_sidecars(root: &Path) -> anyhow::Result<HashMap<String, UnmergedStages>> {
    static UNMERGED_ENTRY: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"^(\d+) ([0-9a-f]+) ([123])\t([\s\S]+)$").expect("static unmerged-entry regex")
    });
    let output = run_git(
        root,
        &[
            "ls-files".to_owned(),
            "--unmerged".to_owned(),
            "-z".to_owned(),
        ],
        "listing unresolved merge entries",
        None,
    )?;
    let output = String::from_utf8_lossy(&output);
    let mut records = HashMap::<String, UnmergedStages>::new();
    for entry in output.split('\0').filter(|entry| !entry.is_empty()) {
        let captures = UNMERGED_ENTRY.captures(entry).ok_or_else(|| {
            anyhow::anyhow!(
                "git ls-files returned a malformed unmerged entry: {}",
                serde_json::to_string(entry).unwrap_or_else(|_| "\"\"".to_owned())
            )
        })?;
        let object_id = captures
            .get(2)
            .ok_or_else(|| malformed_unmerged_entry(entry))?
            .as_str()
            .to_owned();
        let stage = captures
            .get(3)
            .ok_or_else(|| malformed_unmerged_entry(entry))?
            .as_str();
        let path = captures
            .get(4)
            .ok_or_else(|| malformed_unmerged_entry(entry))?
            .as_str();
        if !path.ends_with(".i18n.yaml") {
            continue;
        }
        let stages = records.entry(path.to_owned()).or_default();
        match stage {
            "1" => stages.ancestor = Some(object_id),
            "2" => stages.current = Some(object_id),
            "3" => stages.other = Some(object_id),
            _ => unreachable!("regex limits stages"),
        }
    }
    Ok(records)
}

fn malformed_unmerged_entry(entry: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "git ls-files returned a malformed unmerged entry: {}",
        serde_json::to_string(entry).unwrap_or_else(|_| "\"\"".to_owned())
    )
}

fn assert_unedited_sidecar(
    root: &Path,
    metadata_path: &str,
    ancestor_record: &str,
    current_record: &str,
    other_record: &str,
) -> anyhow::Result<()> {
    let worktree_record = fs::read(root.join(metadata_path))?;
    if worktree_record == current_record.as_bytes() || worktree_record == other_record.as_bytes() {
        return Ok(());
    }
    let text_merge = run_text_merge(
        root,
        metadata_path,
        ancestor_record.as_bytes(),
        current_record.as_bytes(),
        other_record.as_bytes(),
    )?;
    if text_merge.status.success() && text_merge.stdout == worktree_record {
        return Ok(());
    }
    let stage_data_lines = [current_record, other_record]
        .into_iter()
        .flat_map(str::lines)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect::<Vec<_>>();
    let worktree_record = String::from_utf8_lossy(&worktree_record);
    let has_unedited_conflict = worktree_record.contains("<<<<<<<")
        && worktree_record.contains("=======")
        && worktree_record.contains(">>>>>>>")
        && stage_data_lines
            .iter()
            .all(|line| worktree_record.contains(line));
    if !has_unedited_conflict {
        anyhow::bail!(
            "{metadata_path} has edited conflict content; refusing to overwrite manual work"
        );
    }
    Ok(())
}

/// Resolves every mechanically composable `.i18n.yaml` conflict in the index.
///
/// Owner merges are independently recomputed and must match both the stage-zero
/// index and worktree before any safe sidecars are written and staged as a
/// batch. Other conflicts remain untouched and are reported together.
///
/// # Errors
///
/// Returns Git/index/file failures or an aggregate diagnostic after staging
/// every safe record when one or more pairing conflicts require manual work.
pub fn resolve_translation_pairing_conflicts(root: &Path) -> anyhow::Result<Vec<String>> {
    let mut sidecars = unmerged_sidecars(root)?.into_iter().collect::<Vec<_>>();
    sidecars.sort_by(|left, right| left.0.cmp(&right.0));
    let mut resolutions = Vec::<(String, String)>::new();
    let mut failures = Vec::<(String, String)>::new();
    for (metadata_path, stages) in sidecars {
        match resolve_one_sidecar(root, &metadata_path, &stages) {
            Ok(record) => resolutions.push((metadata_path, record)),
            Err(error) => failures.push((metadata_path, error.to_string())),
        }
    }
    for (path, record) in &resolutions {
        fs::write(root.join(path), record)?;
    }
    if !resolutions.is_empty() {
        let mut args = vec!["add".to_owned(), "--".to_owned()];
        args.extend(resolutions.iter().map(|(path, _)| path.clone()));
        run_git(root, &args, "staging resolved pairing records", None)?;
    }
    if !failures.is_empty() {
        let resolved = if resolutions.is_empty() {
            String::new()
        } else {
            format!(
                "resolved and staged {}; ",
                resolutions
                    .iter()
                    .map(|(path, _)| path.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let details = failures
            .iter()
            .map(|(path, reason)| format!("- {path}: {reason}"))
            .collect::<Vec<_>>()
            .join("\n");
        anyhow::bail!(
            "{resolved}left {} pairing conflict(s) unresolved:\n{details}",
            failures.len()
        );
    }
    Ok(resolutions.into_iter().map(|(path, _)| path).collect())
}

fn resolve_one_sidecar(
    root: &Path,
    metadata_path: &str,
    stages: &UnmergedStages,
) -> anyhow::Result<String> {
    let (Some(ancestor_id), Some(current_id), Some(other_id)) = (
        stages.ancestor.as_deref(),
        stages.current.as_deref(),
        stages.other.as_deref(),
    ) else {
        anyhow::bail!(
            "is an add/delete or incomplete-stage conflict and requires manual resolution"
        );
    };
    let ancestor_record = String::from_utf8_lossy(&read_git_blob(
        root,
        ancestor_id,
        &format!("ancestor {metadata_path}"),
    )?)
    .into_owned();
    let current_record = String::from_utf8_lossy(&read_git_blob(
        root,
        current_id,
        &format!("current {metadata_path}"),
    )?)
    .into_owned();
    let other_record = String::from_utf8_lossy(&read_git_blob(
        root,
        other_id,
        &format!("other {metadata_path}"),
    )?)
    .into_owned();
    assert_unedited_sidecar(
        root,
        metadata_path,
        &ancestor_record,
        &current_record,
        &other_record,
    )?;
    let merged = merge_translation_pairing_records(
        root,
        metadata_path,
        &ancestor_record,
        &current_record,
        &other_record,
    )?;
    let paths = translation_pair_paths_from_metadata(metadata_path)?;
    assert_indexed_owner(root, &paths.source, &merged.source_hash)?;
    assert_indexed_owner(root, &paths.zh, &merged.zh_hash)?;
    for (path, expected) in [
        (&paths.source, &merged.source_hash),
        (&paths.zh, &merged.zh_hash),
    ] {
        if git_blob_hash(&fs::read(root.join(path))?) != *expected {
            anyhow::bail!(
                "{path} has unstaged content; refusing to confirm bytes outside the merge result"
            );
        }
    }
    Ok(merged.record)
}

fn assert_indexed_owner(root: &Path, path: &str, expected: &str) -> anyhow::Result<()> {
    if read_git_index_blob(root, path)?.is_none_or(|blob| blob.object_id != expected) {
        anyhow::bail!("{path} staged merge does not match the pairing driver's clean merge");
    }
    Ok(())
}

fn basename(path: &str) -> &str {
    Path::new(path)
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or(path)
}

fn relative_to_slashes(path: &Path) -> Option<String> {
    let value = path.to_str()?;
    #[cfg(windows)]
    {
        Some(value.replace('\\', "/"))
    }
    #[cfg(not(windows))]
    {
        Some(value.to_owned())
    }
}

fn status_text(status: ExitStatus) -> String {
    status
        .code()
        .map_or_else(|| "null".to_owned(), |code| code.to_string())
}
