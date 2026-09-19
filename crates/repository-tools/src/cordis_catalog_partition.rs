//! Cordis catalog page regions: exact-marker splicing, the declared-versus-
//! rendered partition backstop, and the guarded bilingual-pair re-record.
//!
//! Mirrors the pure halves of `scripts/gen-cordis-catalog.ts`.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use indexmap::{IndexMap, IndexSet};

use crate::translation_pairing::{
    blob_hash, parse_pair_metadata, partition_generated_regions, render_pair_metadata,
};

/// Opening marker of a page's generated Cordis API region.
pub const REGION_BEGIN: &str =
    "<!-- BEGIN GENERATED cordis-surface (gen-cordis-catalog.ts) — do not edit between markers -->";
/// Closing marker of a page's generated Cordis API region.
pub const REGION_END: &str = "<!-- END GENERATED cordis-surface -->";

/// Splices a page's generated Cordis API region into its Markdown content.
///
/// The page must contain exactly one `cordis-surface` marker region; the
/// match is on this generator's exact markers, so a page carrying only some
/// other generator's region fails loud instead of being overwritten.
///
/// # Errors
/// Returns the marker-count or marker-order diagnostic.
pub fn splice_region(content: &str, region: &str) -> anyhow::Result<String> {
    let lines = content.split('\n').collect::<Vec<_>>();
    let begins = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| **line == REGION_BEGIN)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let ends = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| **line == REGION_END)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if begins.len() != 1 || ends.len() != 1 {
        anyhow::bail!(
            "expected exactly 1 cordis-surface region, found {} BEGIN/{} END; add the BEGIN/END cordis-surface markers once",
            begins.len(),
            ends.len()
        );
    }
    let begin = begins[0];
    let end = ends[0];
    if end < begin {
        anyhow::bail!("cordis-surface END marker precedes its BEGIN");
    }
    let mut output = lines[..begin].to_vec();
    output.extend(region.split('\n'));
    output.extend_from_slice(&lines[end + 1..]);
    Ok(output.join("\n"))
}

/// The declared-versus-rendered inputs [`walk_partition_problems`] judges.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WalkPartitionInput {
    /// Service key to source pointer, as the rendering projection produced them.
    pub rendered_keys: IndexMap<String, String>,
    /// Event scopes the rendering projection produced.
    pub rendered_scopes: IndexSet<String>,
    /// Event names the rendering projection produced.
    pub rendered_event_names: IndexSet<String>,
    /// Context key to first declaring file, from the independent AST scan.
    pub declared_keys: IndexMap<String, String>,
    /// Event name to first declaring file, from the independent AST scan.
    pub declared_events: IndexMap<String, String>,
}

/// The curated partition maps [`walk_partition_problems`] enforces.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WalkPartitionMaps {
    /// Service key to owning subsystems page.
    pub service_page: IndexMap<String, String>,
    /// Declared Context keys the projection cannot render, with their owners.
    pub service_walk_exemptions: IndexMap<String, String>,
    /// Event scope to owning subsystems page.
    pub event_scope_page: IndexMap<String, String>,
    /// Declared events the projection cannot render, with their owners.
    pub event_walk_exemptions: IndexMap<String, String>,
}

/// Judges the rendered API and the independent AST scan against the curated
/// partition maps, fail-closed in every direction.
#[must_use]
pub fn walk_partition_problems(
    input: &WalkPartitionInput,
    maps: &WalkPartitionMaps,
) -> Vec<String> {
    let mut problems = Vec::new();
    for (key, source) in &input.rendered_keys {
        if !maps.service_page.contains_key(key) {
            problems.push(format!(
                "service ctx.{key} ({source}) has no SERVICE_PAGE entry; every service maps to exactly one subsystems page."
            ));
        }
    }
    let mut scopes = input.rendered_scopes.iter().collect::<Vec<_>>();
    scopes.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
    for scope in scopes {
        if !maps.event_scope_page.contains_key(scope) {
            problems.push(format!(
                "event scope '{scope}/*' has no EVENT_SCOPE_PAGE entry; every event scope maps to exactly one subsystems page."
            ));
        }
    }
    for key in maps.service_page.keys() {
        if !input.rendered_keys.contains_key(key) {
            problems.push(format!(
                "SERVICE_PAGE maps 'ctx.{key}' but the projection discovers no such service; remove the stale entry."
            ));
        }
    }
    for scope in maps.event_scope_page.keys() {
        if !input.rendered_scopes.contains(scope) {
            problems.push(format!(
                "EVENT_SCOPE_PAGE maps '{scope}/*' but the projection discovers no such scope; remove the stale entry."
            ));
        }
    }
    for (key, rel) in &input.declared_keys {
        let rendered = input.rendered_keys.contains_key(key);
        let exempt = maps.service_walk_exemptions.contains_key(key);
        if !rendered && !exempt {
            problems.push(format!(
                "ctx.{key} ({rel}) is declared in a Context merge but invisible to the rendering projection; map it in SERVICE_PAGE (after making it renderable) or name it in SERVICE_WALK_EXEMPTIONS with its documentation owner."
            ));
        }
        if rendered && exempt {
            problems.push(format!(
                "ctx.{key} is rendered by the projection but still listed in SERVICE_WALK_EXEMPTIONS; remove the stale exemption."
            ));
        }
    }
    for key in maps.service_walk_exemptions.keys() {
        if !input.declared_keys.contains_key(key) {
            problems.push(format!(
                "SERVICE_WALK_EXEMPTIONS names 'ctx.{key}' but no Context merge declares it; remove the stale exemption."
            ));
        }
    }
    for (name, rel) in &input.declared_events {
        let rendered = input.rendered_event_names.contains(name);
        let exempt = maps.event_walk_exemptions.contains_key(name);
        if !rendered && !exempt {
            problems.push(format!(
                "event '{name}' ({rel}) is declared in an Events merge but invisible to the rendering projection; make it renderable (mapped via EVENT_SCOPE_PAGE) or name it in EVENT_WALK_EXEMPTIONS with its documentation owner."
            ));
        }
        if rendered && exempt {
            problems.push(format!(
                "event '{name}' is rendered by the projection but still listed in EVENT_WALK_EXEMPTIONS; remove the stale exemption."
            ));
        }
    }
    for name in maps.event_walk_exemptions.keys() {
        if !input.declared_events.contains_key(name) {
            problems.push(format!(
                "EVENT_WALK_EXEMPTIONS names '{name}' but no Events merge declares it; remove the stale exemption."
            ));
        }
    }
    for key in input.rendered_keys.keys() {
        if !input.declared_keys.contains_key(key) {
            problems.push(format!(
                "ctx.{key} is rendered by the projection but the independent scan finds no Context merge declaring it; the scan has a blind spot (glob, prefilter, or module-block walk) — fix the scan, not the maps."
            ));
        }
    }
    for name in &input.rendered_event_names {
        if !input.declared_events.contains_key(name) {
            problems.push(format!(
                "event '{name}' is rendered by the projection but the independent scan finds no Events merge declaring it; the scan has a blind spot (glob, prefilter, or module-block walk) — fix the scan, not the maps."
            ));
        }
    }
    problems
}

/// Re-records a pair's `.i18n.yaml` after a region write only when the write
/// is region-confined over a well-formed, previously consistent record.
///
/// Returns whether the record was refreshed. Human-content drift, a missing
/// or malformed record, or a missing pre-write snapshot leaves the record for
/// the pairing gate to report.
///
/// # Errors
/// Returns current-page read, region-partition, and record write failures.
pub fn maybe_record_pair<S: std::hash::BuildHasher>(
    page_rel: &str,
    before: &HashMap<String, Vec<u8>, S>,
    scan_root: &Path,
) -> anyhow::Result<bool> {
    let zh_rel = replace_md_suffix(page_rel, ".zh.md");
    let meta_rel = replace_md_suffix(page_rel, ".i18n.yaml");
    let meta_abs: PathBuf = scan_root.join(&meta_rel);
    let Ok(meta) = std::fs::read_to_string(&meta_abs) else {
        return Ok(false);
    };
    let Some(recorded) = parse_pair_metadata(&meta) else {
        return Ok(false);
    };
    let names = [basename(page_rel), basename(&zh_rel)];
    if recorded.len() != 2 || !names.iter().all(|name| recorded.contains_key(*name)) {
        return Ok(false);
    }
    for rel in [page_rel, zh_rel.as_str()] {
        let Some(previous) = before.get(rel) else {
            return Ok(false);
        };
        if recorded.get(basename(rel)).map(String::as_str) != Some(blob_hash(previous).as_str()) {
            return Ok(false);
        }
        let current = std::fs::read(scan_root.join(rel))?;
        let stripped_before =
            partition_generated_regions(&String::from_utf8_lossy(previous))?.stripped;
        let stripped_after =
            partition_generated_regions(&String::from_utf8_lossy(&current))?.stripped;
        if stripped_before != stripped_after {
            return Ok(false);
        }
    }
    let source = std::fs::read(scan_root.join(page_rel))?;
    let zh = std::fs::read(scan_root.join(&zh_rel))?;
    std::fs::write(
        &meta_abs,
        render_pair_metadata(page_rel, &blob_hash(&source), &zh_rel, &blob_hash(&zh))?,
    )?;
    Ok(true)
}

fn replace_md_suffix(page_rel: &str, suffix: &str) -> String {
    page_rel
        .strip_suffix(".md")
        .map_or_else(|| page_rel.to_owned(), |stem| format!("{stem}{suffix}"))
}

fn basename(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}
