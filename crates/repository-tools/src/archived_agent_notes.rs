//! Frozen Agent Note archive triplets, content seals, and append-only verification.

use std::{collections::HashSet, path::Path, process::Command, sync::LazyLock};

use chrono::NaiveDate;
use indexmap::IndexMap;
use regex::Regex;
use serde_json::{Map, Value, json};
use sha1::{Digest as _, Sha1};
use sha2::Sha256;

use crate::agent_note_tree::AGENT_NOTE_CLASSES;

static CONTENT_HASH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^sha256:[0-9a-f]{64}$").expect("valid regex"));
static PAIR_META_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([^:#]+\.md): ([0-9a-f]{40})$").expect("valid regex"));
static ARCHIVE_ARTIFACT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([^/]+)/(\d{4}-\d{2}-\d{2}-.+?)(\.zh\.md|\.i18n\.yaml|\.md)$")
        .expect("valid regex")
});
static ARCHIVED_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^Archived: (\d{4}-\d{2}-\d{2})$").expect("valid regex"));
static ARCHIVED_LINE_MULTILINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^Archived: (\d{4}-\d{2}-\d{2})$").expect("valid regex"));

const MANIFEST_REPO_PATH: &str = ".agents/notes/archived/manifest.json";

/// Version-one immutable archive manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchiveManifest {
    /// Closed manifest schema version.
    pub version: u8,
    /// Artifact path to `sha256:<hex>` content seal, in parsed order.
    pub files: IndexMap<String, String>,
}

/// Result of extending an archive manifest with current artifacts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchiveExtension {
    /// Existing seals followed by newly discovered seals.
    pub files: IndexMap<String, String>,
    /// Newly sealed artifact paths in deterministic order.
    pub added: Vec<String>,
    /// Missing or changed artifacts covered by existing seals.
    pub errors: Vec<String>,
}

/// Result of the live archive verifier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchiveVerification {
    /// Frozen artifact count.
    pub artifacts: usize,
    /// Present valid kind-directory count.
    pub kinds: usize,
    /// New seals appended in write mode.
    pub added: usize,
    /// Ordered violations.
    pub errors: Vec<String>,
}

#[derive(Default)]
struct Triplet {
    source: Option<Vec<u8>>,
    chinese: Option<Vec<u8>>,
    metadata: Option<Vec<u8>>,
}

/// Computes the SHA-1 Git blob id used by bilingual consistency sidecars.
#[must_use]
pub fn git_blob_hash(content: &[u8]) -> String {
    let mut hash = Sha1::new();
    hash.update(format!("blob {}\0", content.len()).as_bytes());
    hash.update(content);
    hex::encode(hash.finalize())
}

fn archive_content_hash(content: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(content);
    format!("sha256:{}", hex::encode(hash.finalize()))
}

/// Parses an archive manifest and rejects fields or hashes outside its closed schema.
///
/// # Errors
///
/// Returns JSON, top-level shape, field, version, files-map, or content-hash failures.
pub fn parse_archive_manifest(content: &str) -> anyhow::Result<ArchiveManifest> {
    let value: Value = serde_json::from_str(content)?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("expected a JSON object"))?;
    let mut fields = object.keys().cloned().collect::<Vec<_>>();
    fields.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
    anyhow::ensure!(
        fields == ["files", "version"],
        "expected exactly the fields `version` and `files`"
    );
    anyhow::ensure!(
        object.get("version").and_then(Value::as_u64) == Some(1),
        "unsupported manifest version (expected 1)"
    );
    let file_object = object
        .get("files")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow::anyhow!("`files` must be an object"))?;
    let mut files = IndexMap::new();
    for (path, hash) in file_object {
        let Some(hash) = hash.as_str() else {
            anyhow::bail!("invalid content hash for {path}");
        };
        anyhow::ensure!(
            CONTENT_HASH.is_match(hash),
            "invalid content hash for {path}"
        );
        files.insert(path.clone(), hash.to_owned());
    }
    Ok(ArchiveManifest { version: 1, files })
}

/// Renders an archive manifest with deterministic path ordering.
///
/// # Errors
///
/// Returns JSON serialization failures.
pub fn render_archive_manifest(files: &IndexMap<String, String>) -> anyhow::Result<String> {
    let mut entries = files.iter().collect::<Vec<_>>();
    sort_paths(&mut entries, |entry| entry.0.as_str());
    let mut sorted = Map::new();
    for (path, hash) in entries {
        sorted.insert(path.clone(), Value::String(hash.clone()));
    }
    let mut output = serde_json::to_string_pretty(&json!({"version":1,"files":sorted}))?;
    output.push('\n');
    Ok(output)
}

/// Rejects removals or changes of entries sealed by a baseline manifest.
#[must_use]
pub fn validate_archive_manifest_extension(
    baseline: &ArchiveManifest,
    current: &ArchiveManifest,
) -> Vec<String> {
    let mut errors = Vec::new();
    for (path, expected) in &baseline.files {
        match current.files.get(path) {
            None => errors.push(format!("{path}: sealed manifest entry is missing")),
            Some(actual) if actual != expected => {
                errors.push(format!("{path}: sealed manifest hash changed"));
            }
            Some(_) => {}
        }
    }
    errors
}

/// Validates the closed kind tree, archive headers, and bilingual triplets.
#[must_use]
pub fn validate_archive_artifacts(artifacts: &IndexMap<String, Vec<u8>>) -> Vec<String> {
    let mut errors = Vec::new();
    let mut triplets = IndexMap::<String, Triplet>::new();
    for (path, content) in artifacts {
        let Some(captures) = ARCHIVE_ARTIFACT.captures(path) else {
            errors.push(format!(
                "{path}: expected {{kind}}/yyyy-mm-dd-topic.{{md,zh.md,i18n.yaml}}"
            ));
            continue;
        };
        let (Some(kind), Some(stem), Some(suffix)) =
            (captures.get(1), captures.get(2), captures.get(3))
        else {
            errors.push(format!(
                "{path}: expected {{kind}}/yyyy-mm-dd-topic.{{md,zh.md,i18n.yaml}}"
            ));
            continue;
        };
        let kind = kind.as_str();
        let stem = stem.as_str();
        let suffix = suffix.as_str();
        if !AGENT_NOTE_CLASSES.contains(&kind) {
            let kind = serde_json::to_string(kind).unwrap_or_else(|_| format!("{kind:?}"));
            errors.push(format!("{path}: unknown Agent Note kind {kind}"));
            continue;
        }
        let triplet = triplets.entry(format!("{kind}/{stem}")).or_default();
        match suffix {
            ".md" => triplet.source = Some(content.clone()),
            ".zh.md" => triplet.chinese = Some(content.clone()),
            _ => triplet.metadata = Some(content.clone()),
        }
    }

    let mut entries = triplets.iter().collect::<Vec<_>>();
    sort_paths(&mut entries, |entry| entry.0.as_str());
    for (key, triplet) in entries {
        let source_path = format!("{key}.md");
        let chinese_path = format!("{key}.zh.md");
        let metadata_path = format!("{key}.i18n.yaml");
        let mut missing = Vec::new();
        if triplet.source.is_none() {
            missing.push(source_path.clone());
        }
        if triplet.chinese.is_none() {
            missing.push(chinese_path.clone());
        }
        if triplet.metadata.is_none() {
            missing.push(metadata_path.clone());
        }
        let (Some(source), Some(chinese), Some(metadata)) =
            (&triplet.source, &triplet.chinese, &triplet.metadata)
        else {
            errors.push(format!(
                "{key}: incomplete archived triplet; missing {}",
                missing.join(", ")
            ));
            continue;
        };
        let source_base = key.rsplit('/').next().unwrap_or(key);
        errors.extend(validate_header(&source_path, source, source_base, false));
        errors.extend(validate_header(&chinese_path, chinese, source_base, true));
        let source_text = String::from_utf8_lossy(source);
        let chinese_text = String::from_utf8_lossy(chinese);
        let source_date = archive_date_anywhere(&source_text);
        let chinese_date = archive_date_anywhere(&chinese_text);
        if let (Some(source_date), Some(chinese_date)) = (source_date, chinese_date)
            && source_date != chinese_date
        {
            errors.push(format!(
                "{key}: English and Chinese archive dates differ ({source_date} vs {chinese_date})"
            ));
        }
        let pair = pair_metadata(&String::from_utf8_lossy(metadata));
        if pair.as_ref().is_none_or(|pair| {
            pair.len() != 2
                || pair.get(&format!("{source_base}.md")) != Some(&git_blob_hash(source))
                || pair.get(&format!("{source_base}.zh.md")) != Some(&git_blob_hash(chinese))
        }) {
            errors.push(format!(
                "{metadata_path}: consistency record must contain the current Git blob hashes of both archived sides"
            ));
        }
    }
    errors
}

/// Preserves sealed entries and appends hashes for newly archived artifacts.
#[must_use]
pub fn extend_archive_manifest(
    existing: &ArchiveManifest,
    artifacts: &IndexMap<String, Vec<u8>>,
) -> ArchiveExtension {
    let mut errors = Vec::new();
    let mut files = existing.files.clone();
    for (path, expected) in &existing.files {
        match artifacts.get(path) {
            None => errors.push(format!("{path}: sealed artifact is missing")),
            Some(content) if archive_content_hash(content) != *expected => {
                errors.push(format!("{path}: sealed content hash changed"));
            }
            Some(_) => {}
        }
    }
    let mut entries = artifacts.iter().collect::<Vec<_>>();
    sort_paths(&mut entries, |entry| entry.0.as_str());
    let mut added = Vec::new();
    for (path, content) in entries {
        if files.contains_key(path) {
            continue;
        }
        files.insert(path.clone(), archive_content_hash(content));
        added.push(path.clone());
    }
    ArchiveExtension {
        files,
        added,
        errors,
    }
}

/// Verifies the live archive and optionally appends seals for new artifacts.
///
/// Existing artifact and manifest seals are immutable in both modes.
///
/// # Errors
///
/// Returns archive traversal, file, Git-spawn, or manifest-write failures.
pub fn verify_archived_agent_notes(
    repository_root: &Path,
    write_mode: bool,
    baseline_ref: &str,
) -> anyhow::Result<ArchiveVerification> {
    let archive_root = repository_root.join(".agents/notes/archived");
    let manifest_path = archive_root.join("manifest.json");
    let mut errors = Vec::new();
    let allowed_root_files = ["AGENTS.md", "manifest.json"];
    let mut kinds = HashSet::new();
    if !archive_root.join("AGENTS.md").exists() {
        errors.push("archived/AGENTS.md is required".into());
    }
    let mut artifacts = IndexMap::new();
    for entry in std::fs::read_dir(&archive_root)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let file_type = entry.file_type()?;
        if file_type.is_file() {
            if !allowed_root_files.contains(&name.as_str()) {
                errors.push(format!("archived/{name}: unexpected root file"));
            }
            continue;
        }
        if !file_type.is_dir() {
            errors.push(format!(
                "archived/{name}: only regular files and kind directories are allowed"
            ));
            continue;
        }
        if !AGENT_NOTE_CLASSES.contains(&name.as_str()) {
            errors.push(format!("archived/{name}/: unknown Agent Note kind"));
            continue;
        }
        kinds.insert(name.clone());
        for child in std::fs::read_dir(entry.path())? {
            let child = child?;
            let child_name = child.file_name().to_string_lossy().into_owned();
            let relative = format!("{name}/{child_name}");
            if !child.file_type()?.is_file() {
                errors.push(format!(
                    "{relative}: archived kind directories contain regular files only"
                ));
                continue;
            }
            artifacts.insert(relative, std::fs::read(child.path())?);
        }
    }
    for kind in AGENT_NOTE_CLASSES {
        if !kinds.contains(*kind) {
            errors.push(format!(
                "archived/{kind}/: required kind directory is missing"
            ));
        }
    }
    errors.extend(validate_archive_artifacts(&artifacts));

    let mut manifest = ArchiveManifest {
        version: 1,
        files: IndexMap::new(),
    };
    if manifest_path.exists() {
        match parse_archive_manifest(&std::fs::read_to_string(&manifest_path)?) {
            Ok(parsed) => manifest = parsed,
            Err(error) => errors.push(format!("archived/manifest.json: {error}")),
        }
    } else if !write_mode {
        errors.push(
            "archived/manifest.json is required; seal new artifacts with `pnpm run verify-archived-agent-notes --write`".into(),
        );
    }
    match read_baseline_manifest(repository_root, baseline_ref) {
        Ok(baseline) => errors.extend(validate_archive_manifest_extension(&baseline, &manifest)),
        Err(error) => {
            let baseline = serde_json::to_string(baseline_ref)?;
            errors.push(format!(
                "archived/manifest.json: cannot read baseline {baseline}: {error}"
            ));
        }
    }
    let extension = extend_archive_manifest(&manifest, &artifacts);
    errors.extend(extension.errors.clone());
    if !write_mode {
        errors.extend(
            extension
                .added
                .iter()
                .map(|path| format!("{path}: archived artifact is not sealed in manifest.json")),
        );
    }
    if errors.is_empty() && write_mode {
        let rendered = render_archive_manifest(&extension.files)?;
        if !manifest_path.exists() || std::fs::read_to_string(&manifest_path)? != rendered {
            std::fs::write(&manifest_path, rendered)?;
        }
    }
    Ok(ArchiveVerification {
        artifacts: artifacts.len(),
        kinds: kinds.len(),
        added: extension.added.len(),
        errors,
    })
}

fn validate_header(path: &str, content: &[u8], source_base: &str, chinese: bool) -> Vec<String> {
    let mut errors = Vec::new();
    let text = String::from_utf8_lossy(content);
    let lines = text.split('\n').collect::<Vec<_>>();
    if !lines.first().is_some_and(|line| valid_title(line)) {
        errors.push(format!("{path}: line 1 must be `# Agent Note: <title>`"));
    }
    if lines.get(1).copied() != Some("") {
        errors.push(format!("{path}: line 2 must be blank"));
    }
    if lines.get(2).copied() != Some("Status: implemented") {
        errors.push(format!("{path}: line 3 must be `Status: implemented`"));
    }
    let archived = lines
        .get(3)
        .and_then(|line| ARCHIVED_LINE.captures(line))
        .and_then(|captures| captures.get(1))
        .map(|capture| capture.as_str());
    if archived.is_none_or(|date| !valid_date(date)) {
        errors.push(format!(
            "{path}: line 4 must be `Archived: YYYY-MM-DD` with a valid date"
        ));
    } else if let Some(archived) = archived
        && archived < source_base.get(..10).unwrap_or("")
    {
        errors.push(format!(
            "{path}: archive date {archived} predates the note filename"
        ));
    }
    if lines.get(4).copied() != Some("") {
        errors.push(format!("{path}: line 5 must be blank"));
    }
    let switcher = if chinese {
        format!("[English]({source_base}.md) | 中文")
    } else {
        format!("English | [中文]({source_base}.zh.md)")
    };
    if lines.get(5).copied() != Some(&switcher) {
        let switcher = serde_json::to_string(&switcher).unwrap_or_else(|_| format!("{switcher:?}"));
        errors.push(format!("{path}: line 6 must be {switcher}"));
    }
    errors
}

fn valid_title(line: &str) -> bool {
    line.strip_prefix("# Agent Note: ")
        .and_then(|title| title.chars().next())
        .is_some_and(|character| !character.is_whitespace())
}

fn valid_date(value: &str) -> bool {
    let mut parts = value.split('-');
    let (Some(year), Some(month), Some(day), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    if year.len() != 4 || month.len() != 2 || day.len() != 2 {
        return false;
    }
    let (Ok(year), Ok(month), Ok(day)) = (year.parse(), month.parse(), day.parse()) else {
        return false;
    };
    NaiveDate::from_ymd_opt(year, month, day).is_some()
}

fn pair_metadata(content: &str) -> Option<IndexMap<String, String>> {
    let mut entries = IndexMap::new();
    for line in content.split('\n') {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let captures = PAIR_META_LINE.captures(line)?;
        entries.insert(
            captures.get(1)?.as_str().to_owned(),
            captures.get(2)?.as_str().to_owned(),
        );
    }
    Some(entries)
}

fn archive_date_anywhere(content: &str) -> Option<&str> {
    ARCHIVED_LINE_MULTILINE
        .captures(content)
        .and_then(|captures| captures.get(1))
        .map(|capture| capture.as_str())
}

fn read_baseline_manifest(root: &Path, reference: &str) -> anyhow::Result<ArchiveManifest> {
    run_git(
        root,
        &["cat-file", "-e", &format!("{reference}^{{commit}}")],
    )?;
    let entry = run_git(
        root,
        &[
            "ls-tree",
            "--name-only",
            reference,
            "--",
            MANIFEST_REPO_PATH,
        ],
    )?;
    if entry.trim().is_empty() {
        return Ok(ArchiveManifest {
            version: 1,
            files: IndexMap::new(),
        });
    }
    parse_archive_manifest(&run_git(
        root,
        &["show", &format!("{reference}:{MANIFEST_REPO_PATH}")],
    )?)
}

fn run_git(root: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("git").args(args).current_dir(root).output()?;
    anyhow::ensure!(output.status.success(), "{}", {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        if stderr.is_empty() {
            output.status.code().map_or_else(
                || "git exited with status null".to_owned(),
                |status| format!("git exited with status {status}"),
            )
        } else {
            stderr
        }
    });
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn sort_paths<T, F>(entries: &mut [T], path_of: F)
where
    F: Fn(&T) -> &str,
{
    entries.sort_by(|left, right| {
        path_of(left)
            .encode_utf16()
            .cmp(path_of(right).encode_utf16())
    });
}
