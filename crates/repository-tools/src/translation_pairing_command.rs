//! Worktree/index bilingual pairing enforcement and recording command.

use std::{
    collections::{HashMap, HashSet},
    fmt::Write as _,
    path::Path,
};

use crate::{
    translation_pairing::{
        TranslationPairingCliRequest, TranslationPairingInput, TranslationPairingMode,
        TranslationPairingScope, is_translation_scope_file, language_switcher_targets, links_to,
        parse_translation_markdown, parse_translation_pairing_cli_args,
        parse_translation_pairing_manifest, partition_generated_regions,
        requires_source_language_switcher, translation_structure_diff,
        translation_structure_signature,
    },
    translation_pairing_git::{git_blob_hash, read_git_index_blob, store_git_blob},
    translation_pairing_record::{
        TranslationPairPaths, TranslationPairingRecord, parse_translation_pairing_record,
        render_translation_pairing_record, translation_pair_paths,
    },
};

/// Captured command result, including conventional process status.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TranslationPairingCommandOutput {
    /// Standard output text.
    pub stdout: String,
    /// Standard error text.
    pub stderr: String,
    /// Process exit code.
    pub exit_code: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PairState {
    Ok,
    OutOfSync,
    Missing,
}

/// Executes the complete pairing command against one repository.
///
/// # Errors
///
/// Returns CLI, manifest, traversal, Git, file, Markdown, or write failures.
pub fn run_translation_pairing(
    root: &Path,
    arguments: &[String],
) -> anyhow::Result<TranslationPairingCommandOutput> {
    let request = parse_translation_pairing_cli_args(arguments)?;
    let mut plane = ContentPlane::new(root, request.input);
    let manifest_bytes = plane
        .read("scripts/translation-pairing.manifest.json")?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "scripts/translation-pairing.manifest.json is missing from the selected content plane"
            )
        })?;
    let manifest = parse_translation_pairing_manifest(&String::from_utf8_lossy(&manifest_bytes))?;
    let excluded = |file: &str| {
        manifest.excluded.iter().any(|entry| {
            if entry.ends_with('/') {
                file.starts_with(entry)
            } else {
                file == entry
            }
        })
    };
    let files = collect_scope_files(root, &request, &mut plane)?;
    let translations = sorted_filter(&files, |file| file.ends_with(".zh.md"));
    let metadata = sorted_filter(&files, |file| file.ends_with(".i18n.yaml"));
    let sources = sorted_filter(&files, |file| {
        file.strip_suffix(".md").is_some() && file.strip_suffix(".zh.md").is_none()
    });

    if request.scope == TranslationPairingScope::Pairs {
        let rejected = request
            .anchors
            .iter()
            .filter(|anchor| !is_translation_scope_file(anchor) || excluded(anchor))
            .cloned()
            .collect::<Vec<_>>();
        let mut absent = Vec::new();
        for anchor in &request.anchors {
            let paths = translation_pair_paths(anchor)?;
            if !plane.exists(&paths.source)?
                && !plane.exists(&paths.zh)?
                && !plane.exists(&paths.metadata)?
            {
                absent.push(anchor.clone());
            }
        }
        if !rejected.is_empty()
            || (request.input != TranslationPairingInput::Index && !absent.is_empty())
        {
            let mut stderr = String::new();
            for anchor in rejected {
                let _ = writeln!(
                    stderr,
                    "verify-translation-pairing: {anchor} is not an in-scope pair (excluded or outside the documentation corpus; see docs/i18n/README.md)"
                );
            }
            for anchor in absent {
                let _ = writeln!(
                    stderr,
                    "verify-translation-pairing: {anchor} names no pair on disk (none of its three files exist)"
                );
            }
            return Ok(TranslationPairingCommandOutput {
                stderr,
                exit_code: 2,
                ..TranslationPairingCommandOutput::default()
            });
        }
    }

    if request.mode == TranslationPairingMode::Write {
        return write_records(root, &request, &sources, &mut plane, &excluded);
    }
    check_records(
        &request,
        &sources,
        &translations,
        &metadata,
        &mut plane,
        &excluded,
    )
}

fn write_records(
    root: &Path,
    request: &TranslationPairingCliRequest,
    sources: &[String],
    plane: &mut ContentPlane<'_>,
    excluded: &impl Fn(&str) -> bool,
) -> anyhow::Result<TranslationPairingCommandOutput> {
    let mut output = TranslationPairingCommandOutput::default();
    let mut written = 0;
    for source in sources {
        if excluded(source) {
            continue;
        }
        let paths = translation_pair_paths(source)?;
        let source_content = plane.read(&paths.source)?;
        let zh_content = plane.read(&paths.zh)?;
        if source_content.is_none() || zh_content.is_none() {
            if request.scope == TranslationPairingScope::Pairs {
                let missing = if source_content.is_some() {
                    &paths.zh
                } else {
                    &paths.source
                };
                let _ = writeln!(
                    output.stderr,
                    "verify-translation-pairing: cannot record {}: missing {missing}",
                    paths.source
                );
                output.exit_code = 2;
                return Ok(output);
            }
            continue;
        }
        let (Some(source_content), Some(zh_content)) = (source_content, zh_content) else {
            continue;
        };
        let record = TranslationPairingRecord {
            source_hash: store_git_blob(root, &source_content)?,
            zh_hash: store_git_blob(root, &zh_content)?,
        };
        let rendered = render_translation_pairing_record(&paths, &record);
        if root.join(&paths.metadata).is_file()
            && std::fs::read_to_string(root.join(&paths.metadata))
                .is_ok_and(|current| current == rendered)
        {
            continue;
        }
        std::fs::write(root.join(&paths.metadata), rendered)?;
        let _ = writeln!(
            output.stdout,
            "verify-translation-pairing: recorded {}",
            paths.metadata
        );
        written += 1;
    }
    let _ = writeln!(
        output.stdout,
        "verify-translation-pairing: {written} record(s) written; run the check to validate the pairs."
    );
    Ok(output)
}

fn check_records(
    request: &TranslationPairingCliRequest,
    sources: &[String],
    translations: &[String],
    metadata: &[String],
    plane: &mut ContentPlane<'_>,
    excluded: &impl Fn(&str) -> bool,
) -> anyhow::Result<TranslationPairingCommandOutput> {
    let mut errors = Vec::new();
    let mut state = HashMap::<String, PairState>::new();
    for source in sources {
        if excluded(source) {
            continue;
        }
        let paths = translation_pair_paths(source)?;
        if !plane.exists(&paths.zh)? {
            errors.push(format!(
                "{source}: in-scope documentation must merge bilingual (docs/i18n/README.md); add the counterpart and record the pair"
            ));
            state.insert(source.clone(), PairState::Missing);
        }
    }

    let mut anchors = translations
        .iter()
        .map(|path| {
            path.strip_suffix(".zh.md")
                .map_or_else(|| path.clone(), |stem| format!("{stem}.md"))
        })
        .chain(metadata.iter().map(|path| {
            path.strip_suffix(".i18n.yaml")
                .map_or_else(|| path.clone(), |stem| format!("{stem}.md"))
        }))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    anchors.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));

    for source in &anchors {
        check_one_pair(source, plane, excluded, &mut errors, &mut state)?;
    }
    for source in sources {
        if !excluded(source) {
            state.entry(source.clone()).or_insert(PairState::Missing);
        }
    }
    if request.mode == TranslationPairingMode::List {
        return Ok(render_list(&state));
    }
    if errors.is_empty() {
        let stdout = if request.scope == TranslationPairingScope::Pairs {
            format!(
                "verify-translation-pairing: {} named {}pair(s) consistent; the corpus-wide check still runs in doc-sync.\n",
                anchors.len(),
                if request.input == TranslationPairingInput::Index {
                    "staged "
                } else {
                    ""
                }
            )
        } else {
            format!(
                "verify-translation-pairing: {} pair(s) checked across all in-scope documentation, all consistent.\n",
                anchors.len()
            )
        };
        return Ok(TranslationPairingCommandOutput {
            stdout,
            ..TranslationPairingCommandOutput::default()
        });
    }
    let mut stderr =
        "verify-translation-pairing: bilingual pairing rules violated (see docs/i18n/README.md):\n"
            .to_owned();
    for error in errors {
        stderr.push_str("  ");
        stderr.push_str(&error);
        stderr.push('\n');
    }
    Ok(TranslationPairingCommandOutput {
        stderr,
        exit_code: 1,
        ..TranslationPairingCommandOutput::default()
    })
}

fn check_one_pair(
    source: &str,
    plane: &mut ContentPlane<'_>,
    excluded: &impl Fn(&str) -> bool,
    errors: &mut Vec<String>,
    state: &mut HashMap<String, PairState>,
) -> anyhow::Result<()> {
    let paths = translation_pair_paths(source)?;
    let have = [
        plane.exists(&paths.source)?,
        plane.exists(&paths.zh)?,
        plane.exists(&paths.metadata)?,
    ];
    if excluded(source) {
        if have[1] {
            errors.push(format!(
                "{}: {} is excluded from pairing (generated or bilingual-by-construction); this translation must not exist",
                paths.zh, paths.source
            ));
        }
        if have[2] {
            errors.push(format!(
                "{}: {} is excluded from pairing; this consistency record must not exist",
                paths.metadata, paths.source
            ));
        }
        return Ok(());
    }
    let all_paths = [&paths.source, &paths.zh, &paths.metadata];
    let missing = all_paths
        .iter()
        .zip(have)
        .filter(|(_, present)| !present)
        .map(|(path, _)| (*path).clone())
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        errors.push(format!(
            "{}: incomplete pair — missing {} (pairs merge whole: both languages plus the .i18n.yaml record)",
            paths.source,
            missing.join(", ")
        ));
        return Ok(());
    }
    let Some(source_content) = plane.read(&paths.source)? else {
        return Ok(());
    };
    let Some(zh_content) = plane.read(&paths.zh)? else {
        return Ok(());
    };
    let Some(metadata_content) = plane.read(&paths.metadata)? else {
        return Ok(());
    };
    let Some(record) =
        parse_translation_pairing_record(&String::from_utf8_lossy(&metadata_content), &paths)
    else {
        errors.push(format!(
            "{}: malformed consistency record (expected exactly `{}: <40-hex>` and `{}: <40-hex>`)",
            paths.metadata,
            basename(&paths.source),
            basename(&paths.zh)
        ));
        return Ok(());
    };
    let mut consistent = true;
    for (file, content, recorded) in [
        (&paths.source, &source_content, &record.source_hash),
        (&paths.zh, &zh_content, &record.zh_hash),
    ] {
        if git_blob_hash(content) != *recorded {
            errors.push(format!(
                "{file}: out of sync — content no longer matches the pair's last confirmed-consistent state in {} (bring the other side along, then re-record with --write)",
                paths.metadata
            ));
            consistent = false;
        }
    }
    if !consistent {
        state.insert(source.to_owned(), PairState::OutOfSync);
        return Ok(());
    }
    check_pair_content(source, &paths, &source_content, &zh_content, errors, state)
}

fn check_pair_content(
    source: &str,
    paths: &TranslationPairPaths,
    source_content: &[u8],
    zh_content: &[u8],
    errors: &mut Vec<String>,
    state: &mut HashMap<String, PairState>,
) -> anyhow::Result<()> {
    let source_text = String::from_utf8_lossy(source_content);
    let zh_text = String::from_utf8_lossy(zh_content);
    let regions = partition_generated_regions(&source_text).and_then(|source_regions| {
        partition_generated_regions(&zh_text).map(|zh_regions| (source_regions, zh_regions))
    });
    let (source_regions, zh_regions) = match regions {
        Ok(regions) => regions,
        Err(error) => {
            errors.push(format!("{} ↔ {}: {error}", paths.source, paths.zh));
            state.insert(source.to_owned(), PairState::OutOfSync);
            return Ok(());
        }
    };
    if source_regions.regions != zh_regions.regions {
        errors.push(format!(
            "{} ↔ {}: generated regions differ between the pair — regenerate (the generator writes both sides byte-identically)",
            paths.source, paths.zh
        ));
        state.insert(source.to_owned(), PairState::OutOfSync);
    }
    let source_tree = parse_translation_markdown(&source_text).map_err(anyhow::Error::msg)?;
    let zh_tree = parse_translation_markdown(&zh_text).map_err(anyhow::Error::msg)?;
    let source_switchers = language_switcher_targets(&paths.source);
    let zh_switchers = language_switcher_targets(&paths.zh);
    if !links_to(&zh_tree, &source_switchers) {
        errors.push(format!(
            "{}: missing language switcher — no link to {}",
            paths.zh,
            basename(&paths.source)
        ));
    }
    if requires_source_language_switcher(&paths.source) && !links_to(&source_tree, &zh_switchers) {
        errors.push(format!(
            "{}: missing language switcher — no link back to {}",
            paths.source,
            basename(&paths.zh)
        ));
    }
    for divergence in translation_structure_diff(
        &translation_structure_signature(&source_tree, &zh_switchers),
        &translation_structure_signature(&zh_tree, &source_switchers),
    ) {
        errors.push(format!("{} ↔ {}: {divergence}", paths.source, paths.zh));
    }
    state.entry(source.to_owned()).or_insert(PairState::Ok);
    Ok(())
}

fn render_list(state: &HashMap<String, PairState>) -> TranslationPairingCommandOutput {
    let mut rows = state.iter().collect::<Vec<_>>();
    rows.sort_by(|(left_path, left), (right_path, right)| {
        state_order(**left)
            .cmp(&state_order(**right))
            .then_with(|| left_path.encode_utf16().cmp(right_path.encode_utf16()))
    });
    let mut stdout = String::new();
    let mut counts = [0_usize; 3];
    for (path, status) in rows {
        counts[state_order(*status)] += 1;
        let label = state_label(*status);
        let _ = writeln!(
            stdout,
            "{label:<11} {path}{}",
            if *status == PairState::Missing {
                "  (required)"
            } else {
                ""
            }
        );
    }
    let _ = writeln!(
        stdout,
        "verify-translation-pairing: {} ok, {} out-of-sync, {} missing (of {} in scope)",
        counts[2],
        counts[0],
        counts[1],
        state.len()
    );
    TranslationPairingCommandOutput {
        stdout,
        ..TranslationPairingCommandOutput::default()
    }
}

fn collect_scope_files(
    root: &Path,
    request: &TranslationPairingCliRequest,
    plane: &mut ContentPlane<'_>,
) -> anyhow::Result<HashSet<String>> {
    let mut files = HashSet::new();
    if request.scope == TranslationPairingScope::Pairs {
        for anchor in &request.anchors {
            let paths = translation_pair_paths(anchor)?;
            for file in [&paths.source, &paths.zh, &paths.metadata] {
                if plane.exists(file)? {
                    files.insert(file.clone());
                }
            }
            if request.input != TranslationPairingInput::Index && !plane.exists(anchor)? {
                files.insert(anchor.clone());
            }
        }
        return Ok(files);
    }
    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        .filter_entry(|entry| {
            entry.depth() == 0
                || !discovery_directory_excluded(entry.file_name().to_string_lossy().as_ref())
        })
    {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(root)?
            .to_string_lossy()
            .replace('\\', "/");
        if is_translation_scope_file(&relative)
            && (relative.strip_suffix(".md").is_some()
                || relative.strip_suffix(".i18n.yaml").is_some())
        {
            files.insert(relative);
        }
    }
    Ok(files)
}

struct ContentPlane<'a> {
    root: &'a Path,
    input: TranslationPairingInput,
    cache: HashMap<String, Option<Vec<u8>>>,
}

impl<'a> ContentPlane<'a> {
    fn new(root: &'a Path, input: TranslationPairingInput) -> Self {
        Self {
            root,
            input,
            cache: HashMap::new(),
        }
    }

    fn read(&mut self, file: &str) -> anyhow::Result<Option<Vec<u8>>> {
        if let Some(content) = self.cache.get(file) {
            return Ok(content.clone());
        }
        let content = if self.input == TranslationPairingInput::Index {
            read_git_index_blob(self.root, file)?.map(|blob| blob.content)
        } else if self.root.join(file).exists() {
            Some(std::fs::read(self.root.join(file))?)
        } else {
            None
        };
        self.cache.insert(file.to_owned(), content.clone());
        Ok(content)
    }

    fn exists(&mut self, file: &str) -> anyhow::Result<bool> {
        Ok(self.read(file)?.is_some())
    }
}

fn sorted_filter(files: &HashSet<String>, predicate: impl Fn(&str) -> bool) -> Vec<String> {
    let mut selected = files
        .iter()
        .filter(|file| predicate(file))
        .cloned()
        .collect::<Vec<_>>();
    selected.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
    selected
}

fn discovery_directory_excluded(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | "target"
            | "node_modules"
            | "lib"
            | ".pnpm-store"
            | ".cache"
            | "coverage"
            | ".sessions"
            | ".storages"
            | "tmp"
            | "dist-exe"
            | "__pycache__"
            | ".pytest_cache"
            | ".artifacts"
            | "vendor"
    ) || name.starts_with(".doc-typecheck-")
        || name.starts_with(".node-next-types-")
}

fn basename(path: &str) -> &str {
    std::path::Path::new(path)
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or(path)
}

fn state_order(state: PairState) -> usize {
    match state {
        PairState::OutOfSync => 0,
        PairState::Missing => 1,
        PairState::Ok => 2,
    }
}

fn state_label(state: PairState) -> &'static str {
    match state {
        PairState::OutOfSync => "out-of-sync",
        PairState::Missing => "missing",
        PairState::Ok => "ok",
    }
}
