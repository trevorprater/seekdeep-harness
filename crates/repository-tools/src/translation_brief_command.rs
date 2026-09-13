//! The `gen-translation-brief` command: print the minimal-update briefing
//! for out-of-sync translation pairs. With no arguments it discovers every
//! out-of-sync pair; with arguments (any file of a pair) it briefs exactly
//! those pairs and fails loud on in-sync, incomplete, or out-of-scope
//! requests. Each briefing maps the change at the narrowest safe granularity
//! — code-fence-only splice, changed Markdown units, heading sections, whole
//! document — and `--apply` writes the computed counterpart for pairs whose
//! change is code-fence-only.

use std::{
    collections::{BTreeSet, HashMap},
    path::Path,
    process::Command,
};

use regex::Regex;

use crate::{
    translation_brief::{
        BriefBundle, BriefDirection, BriefScope, BundleReason, MarkdownSpan, TranslationBriefInput,
        changed_span_indices, compute_mechanical_update, first_occurrence_context, markdown_units,
        relevant_terminology_rows, render_translation_brief, section_spans, spans_aligned,
    },
    translation_pairing::{
        is_translation_scope_file, language_switcher_targets, pair_anchor_of_argument,
        parse_translation_markdown, parse_translation_pairing_manifest, translation_structure_diff,
        translation_structure_signature,
    },
    translation_pairing_git::run_git,
};

/// Directory names the source's scope glob excludes at any depth.
const EXCLUDED_DIRECTORIES: &[&str] = &[
    "node_modules",
    "lib",
    ".pnpm-store",
    ".cache",
    "coverage",
    ".sessions",
    ".storages",
    "tmp",
    "dist-exe",
    "__pycache__",
    ".pytest_cache",
    ".git",
    "target",
];

/// Repo-relative directory prefixes the source's scope glob excludes.
const EXCLUDED_PREFIXES: &[&str] = &[".agents/notes/archived/", "apps/web/dist/", ".artifacts/"];

fn directory_excluded(name: &str) -> bool {
    EXCLUDED_DIRECTORIES.contains(&name)
        || name.starts_with(".doc-typecheck-")
        || name.starts_with(".node-next-types-")
}

/// Recorded hashes of one consistency record: basename → blob hash.
fn parse_meta(content: &str) -> Option<HashMap<String, String>> {
    static LINE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"^([^:#]+\.md): ([0-9a-f]{40})$").expect("static record regex")
    });
    let mut out = HashMap::new();
    for line in content.split('\n') {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let captures = LINE.captures(line)?;
        out.insert(captures[1].to_owned(), captures[2].to_owned());
    }
    Some(out)
}

fn blob_text(root: &Path, hash: &str) -> anyhow::Result<String> {
    let bytes = run_git(
        root,
        &["cat-file".to_owned(), "-p".to_owned(), hash.to_owned()],
        "gen-translation-brief blob read",
        None,
    )?;
    Ok(String::from_utf8(bytes)?)
}

/// Unified diff between two texts, headers stripped, via `git diff --no-index`.
///
/// # Errors
/// Returns temporary-file or Git failures.
pub fn diff_texts(root: &Path, before: &str, after: &str) -> anyhow::Result<String> {
    let directory = tempfile::Builder::new()
        .prefix("translation-brief-")
        .tempdir()?;
    let last = directory.path().join("last-confirmed.md");
    let current = directory.path().join("current.md");
    std::fs::write(&last, before)?;
    std::fs::write(&current, after)?;
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["diff", "--no-index", "--unified=2"])
        .arg(&last)
        .arg(&current)
        .output()?;
    if !matches!(output.status.code(), Some(0 | 1)) {
        anyhow::bail!(
            "git diff --no-index failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let raw = String::from_utf8(output.stdout)?;
    Ok(raw
        .split('\n')
        .filter(|line| {
            !line.starts_with("diff --git")
                && !line.starts_with("index ")
                && !line.starts_with("--- ")
                && !line.starts_with("+++ ")
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim_matches(crate::jsdoc::is_js_space)
        .to_owned())
}

/// One pair's recorded and current state.
#[derive(Clone, Debug)]
pub struct PairState {
    /// The English anchor path.
    pub anchor: String,
    /// The Chinese counterpart path.
    pub zh: String,
    /// The consistency record path.
    pub meta: String,
    /// Whether the English side differs from its recorded blob.
    pub en_drifted: bool,
    /// Whether the Chinese side differs from its recorded blob.
    pub zh_drifted: bool,
    /// The recorded English text.
    pub en_last: String,
    /// The recorded Chinese text.
    pub zh_last: String,
}

fn is_excluded(excluded: &[String], file: &str) -> bool {
    excluded.iter().any(|entry| {
        if entry.ends_with('/') {
            file.starts_with(entry.as_str())
        } else {
            file == entry
        }
    })
}

/// Load one pair's recorded and current state, or explain why it cannot be briefed.
///
/// # Errors
/// Returns file-read and Git failures.
pub fn load_pair(
    root: &Path,
    excluded: &[String],
    anchor: &str,
) -> anyhow::Result<Result<PairState, String>> {
    let stem = anchor.strip_suffix(".md").unwrap_or(anchor);
    let zh = format!("{stem}.zh.md");
    let meta = format!("{stem}.i18n.yaml");
    if !is_translation_scope_file(anchor) || is_excluded(excluded, anchor) {
        return Ok(Err(format!(
            "{anchor}: not an in-scope documentation pair (docs/i18n/README.md)"
        )));
    }
    let missing = [anchor, zh.as_str(), meta.as_str()]
        .into_iter()
        .filter(|file| !root.join(file).exists())
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Ok(Err(format!(
            "{anchor}: incomplete pair (missing {}) — a new counterpart is whole-document translation work, not a minimal update",
            missing.join(", ")
        )));
    }
    let record = parse_meta(&std::fs::read_to_string(root.join(&meta))?);
    let basename = |path: &str| path.rsplit('/').next().unwrap_or(path).to_owned();
    let (Some(record), true) = (record.as_ref(), true) else {
        return Ok(Err(format!("{meta}: malformed consistency record")));
    };
    let (Some(en_recorded), Some(zh_recorded)) =
        (record.get(&basename(anchor)), record.get(&basename(&zh)))
    else {
        return Ok(Err(format!("{meta}: malformed consistency record")));
    };
    let en_current = std::fs::read_to_string(root.join(anchor))?;
    let zh_current = std::fs::read_to_string(root.join(&zh))?;
    let en_last = blob_text(root, en_recorded)?;
    let zh_last = blob_text(root, zh_recorded)?;
    Ok(Ok(PairState {
        anchor: anchor.to_owned(),
        en_drifted: en_current != en_last,
        zh_drifted: zh_current != zh_last,
        zh,
        meta,
        en_last,
        zh_last,
    }))
}

/// Assemble bundles for the given changed + first-occurrence span indices.
///
/// # Errors
/// Returns an index that is unmapped despite alignment.
pub fn bundles_for(
    indices: &[usize],
    extra_indices: &[usize],
    confirmed: &[MarkdownSpan],
    current: &[MarkdownSpan],
    counterpart: &[MarkdownSpan],
) -> anyhow::Result<Vec<BriefBundle>> {
    let extras = extra_indices.iter().copied().collect::<BTreeSet<_>>();
    indices
        .iter()
        .chain(extra_indices)
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|index| {
            let (Some(confirmed_span), Some(current_span), Some(counterpart_span)) = (
                confirmed.get(index),
                current.get(index),
                counterpart.get(index),
            ) else {
                anyhow::bail!("gen-translation-brief: span {index} is unmapped despite alignment");
            };
            Ok(BriefBundle {
                index,
                label: current_span.label.clone(),
                reason: (extras.contains(&index) && confirmed_span.text == current_span.text)
                    .then_some(BundleReason::FirstOccurrence),
                confirmed_source_text: confirmed_span.text.clone(),
                current_source_text: current_span.text.clone(),
                counterpart_text: counterpart_span.text.clone(),
                counterpart_start_line: counterpart_span.start_line,
            })
        })
        .collect()
}

type SpansOf = fn(&str) -> anyhow::Result<Vec<MarkdownSpan>>;

/// One planned briefing scope with its terminology text and mechanical result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedBrief {
    /// The mapped scope.
    pub scope: BriefScope,
    /// Old + new text of the changed spans, for terminology matching.
    pub changed_text: String,
    /// Computed counterpart for a mechanical scope, for `--apply`.
    pub mechanical_result: Option<String>,
}

/// Choose the narrowest safely mapped granularity for one drifted side.
///
/// # Errors
/// Returns Markdown parser diagnostics and unmapped spans.
pub fn plan_scope(
    terminology: &str,
    source_last: &str,
    source_current: &str,
    counterpart_current: &str,
    direction: BriefDirection,
    both_drifted: bool,
) -> anyhow::Result<PlannedBrief> {
    let whole_changed_text = format!("{source_last}\n{source_current}");
    if both_drifted {
        return Ok(PlannedBrief {
            scope: BriefScope::Document {
                reason: "BOTH sides changed since the pair was last confirmed consistent, so no side is a trustworthy mapping anchor; decide which side owns each divergence.".to_owned(),
            },
            changed_text: whole_changed_text,
            mechanical_result: None,
        });
    }
    if let Some(mechanical) =
        compute_mechanical_update(source_last, source_current, counterpart_current)?
    {
        return Ok(PlannedBrief {
            scope: BriefScope::Mechanical,
            changed_text: whole_changed_text,
            mechanical_result: Some(mechanical),
        });
    }
    let granularities: [(bool, SpansOf); 2] = [(true, markdown_units), (false, section_spans)];
    for (units, spans_of) in granularities {
        let confirmed = spans_of(source_last)?;
        let current = spans_of(source_current)?;
        let counterpart = spans_of(counterpart_current)?;
        if !spans_aligned(&confirmed, &current) || !spans_aligned(&confirmed, &counterpart) {
            continue;
        }
        let changed = changed_span_indices(&confirmed, &current);
        if changed.is_empty() {
            continue;
        }
        let changed_text = changed
            .iter()
            .map(|index| {
                format!(
                    "{}\n{}",
                    confirmed.get(*index).map_or("", |span| span.text.as_str()),
                    current.get(*index).map_or("", |span| span.text.as_str())
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let rows = relevant_terminology_rows(terminology, direction, &changed_text);
        let occurrence = if direction == BriefDirection::EnToZh {
            first_occurrence_context(
                source_last,
                source_current,
                &confirmed,
                &current,
                &rows,
                &changed.iter().copied().collect(),
            )
        } else {
            crate::translation_brief::FirstOccurrenceContext::default()
        };
        let bundles = bundles_for(
            &changed,
            &occurrence.extra_span_indices,
            &confirmed,
            &current,
            &counterpart,
        )?;
        let scope = if units {
            BriefScope::Units {
                bundles,
                first_occurrence_notes: occurrence.notes,
            }
        } else {
            BriefScope::Sections {
                bundles,
                first_occurrence_notes: occurrence.notes,
            }
        };
        return Ok(PlannedBrief {
            scope,
            changed_text,
            mechanical_result: None,
        });
    }
    Ok(PlannedBrief {
        scope: BriefScope::Document {
            reason: "Neither fine-grained units nor heading sections align one to one across the last-confirmed source, current source, and current counterpart.".to_owned(),
        },
        changed_text: whole_changed_text,
        mechanical_result: None,
    })
}

/// Validate a computed mechanical counterpart and write it.
///
/// # Errors
/// Returns a structure violation or write failure.
pub fn apply_mechanical(
    root: &Path,
    counterpart_path: &str,
    source_current: &str,
    result: &str,
) -> anyhow::Result<String> {
    let counterpart_base = counterpart_path
        .rsplit('/')
        .next()
        .unwrap_or(counterpart_path);
    let source_base = counterpart_base.strip_suffix(".zh.md").map_or_else(
        || {
            counterpart_base.strip_suffix(".md").map_or_else(
                || counterpart_base.to_owned(),
                |stem| format!("{stem}.zh.md"),
            )
        },
        |stem| format!("{stem}.md"),
    );
    let source_tree = parse_translation_markdown(source_current).map_err(anyhow::Error::msg)?;
    let result_tree = parse_translation_markdown(result).map_err(anyhow::Error::msg)?;
    let errors = translation_structure_diff(
        &translation_structure_signature(
            &source_tree,
            &language_switcher_targets(counterpart_base),
        ),
        &translation_structure_signature(&result_tree, &language_switcher_targets(&source_base)),
    );
    if !errors.is_empty() {
        anyhow::bail!(
            "gen-translation-brief: computed mechanical update for {counterpart_path} violates the pair structure: {}",
            errors.join("; ")
        );
    }
    std::fs::write(root.join(counterpart_path), result)?;
    Ok(format!(
        "gen-translation-brief: applied code-fence splice to {counterpart_path}; review the diff, then record the pair."
    ))
}

/// Render (and under `--apply`, apply) the briefing for one drifted side.
fn brief_direction(
    root: &Path,
    terminology: &str,
    pair: &PairState,
    direction: BriefDirection,
    apply: bool,
    notices: &mut Vec<String>,
) -> anyhow::Result<String> {
    let source_is_english = direction == BriefDirection::EnToZh;
    let (source_path, counterpart_path, source_last) = if source_is_english {
        (&pair.anchor, &pair.zh, &pair.en_last)
    } else {
        (&pair.zh, &pair.anchor, &pair.zh_last)
    };
    let source_current = std::fs::read_to_string(root.join(source_path))?;
    let counterpart_current = std::fs::read_to_string(root.join(counterpart_path))?;
    let diff = diff_texts(root, source_last, &source_current)?;
    let planned = plan_scope(
        terminology,
        source_last,
        &source_current,
        &counterpart_current,
        direction,
        pair.en_drifted && pair.zh_drifted,
    )?;
    if let (true, Some(result)) = (apply, &planned.mechanical_result) {
        notices.push(apply_mechanical(
            root,
            counterpart_path,
            &source_current,
            result,
        )?);
    }
    Ok(render_translation_brief(&TranslationBriefInput {
        source_path: source_path.clone(),
        counterpart_path: counterpart_path.clone(),
        direction,
        diff,
        scope: planned.scope,
        terminology: relevant_terminology_rows(terminology, direction, &planned.changed_text),
    }))
}

/// The command's outcome, mapped to the source's exit codes by the binary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BriefOutcome {
    /// Unknown flags (exit 2).
    UnknownFlags(Vec<String>),
    /// Requested pairs that could not or need not be briefed (exit 2).
    Problems(Vec<String>),
    /// Every recorded pair matches its consistency record (exit 0).
    Nothing,
    /// The briefings, joined for stdout (exit 0).
    Briefs(String),
}

/// The command's outcome plus the `--apply` notices written to stderr.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BriefRun {
    /// Outcome for the exit code and stdout.
    pub outcome: BriefOutcome,
    /// Apply notices (stderr), in order.
    pub notices: Vec<String>,
}

fn discover_anchors(root: &Path) -> anyhow::Result<BTreeSet<String>> {
    let mut discovered = BTreeSet::new();
    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        .filter_entry(|entry| {
            entry.depth() == 0 || !directory_excluded(entry.file_name().to_string_lossy().as_ref())
        })
    {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(root)?
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        if EXCLUDED_PREFIXES
            .iter()
            .any(|prefix| relative.starts_with(prefix))
        {
            continue;
        }
        if let Some(stem) = relative.strip_suffix(".i18n.yaml")
            && is_translation_scope_file(&relative)
        {
            discovered.insert(format!("{stem}.md"));
        }
    }
    Ok(discovered)
}

/// Run the command over `root` with the source CLI's arguments.
///
/// # Errors
/// Returns manifest, terminology, file, Git, and Markdown failures.
pub fn run_translation_brief(root: &Path, arguments: &[String]) -> anyhow::Result<BriefRun> {
    let flags = arguments
        .iter()
        .filter(|argument| argument.starts_with("--"))
        .cloned()
        .collect::<Vec<_>>();
    let unknown = flags
        .iter()
        .filter(|flag| *flag != "--apply")
        .cloned()
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        return Ok(BriefRun {
            outcome: BriefOutcome::UnknownFlags(unknown),
            notices: Vec::new(),
        });
    }
    let apply = flags.iter().any(|flag| flag == "--apply");
    let manifest = parse_translation_pairing_manifest(&std::fs::read_to_string(
        root.join("scripts/translation-pairing.manifest.json"),
    )?)?;
    let terminology = std::fs::read_to_string(root.join("docs/i18n/terminology.md"))?;
    let requested = arguments
        .iter()
        .filter(|argument| !argument.starts_with("--"))
        .map(|argument| pair_anchor_of_argument(argument))
        .collect::<BTreeSet<_>>();
    let anchors = if requested.is_empty() {
        discover_anchors(root)?
    } else {
        requested.clone()
    };

    let mut briefs = Vec::new();
    let mut problems = Vec::new();
    let mut skipped = Vec::new();
    let mut notices = Vec::new();
    for anchor in &anchors {
        let pair = match load_pair(root, &manifest.excluded, anchor)? {
            Ok(pair) => pair,
            Err(problem) => {
                if !requested.is_empty() {
                    problems.push(problem);
                }
                continue;
            }
        };
        if !pair.en_drifted && !pair.zh_drifted {
            if !requested.is_empty() {
                skipped.push(format!(
                    "{anchor}: pair is consistent with its record — nothing to brief"
                ));
            }
            continue;
        }
        if pair.en_drifted {
            briefs.push(brief_direction(
                root,
                &terminology,
                &pair,
                BriefDirection::EnToZh,
                apply,
                &mut notices,
            )?);
        }
        if pair.zh_drifted {
            briefs.push(brief_direction(
                root,
                &terminology,
                &pair,
                BriefDirection::ZhToEn,
                apply,
                &mut notices,
            )?);
        }
    }
    if !problems.is_empty() || !skipped.is_empty() {
        problems.extend(skipped);
        return Ok(BriefRun {
            outcome: BriefOutcome::Problems(problems),
            notices,
        });
    }
    if briefs.is_empty() {
        return Ok(BriefRun {
            outcome: BriefOutcome::Nothing,
            notices,
        });
    }
    Ok(BriefRun {
        outcome: BriefOutcome::Briefs(briefs.join("\n\n---\n\n")),
        notices,
    })
}
