//! The instrumented Rust coverage lane behind `pnpm run test:coverage`.
//!
//! The source ran its unit suites under V8 instrumentation and held every file under
//! `packages/*/*/src` to a per-file 100% bar, naming each uncovered location through the
//! reporter in the parent module. The port's suites are Rust, so the lane runs them under
//! `cargo llvm-cov`, translates the LLVM export into the reporter's Istanbul shape, and holds
//! the crates that realize those source files to the same bar. `scripts/coverage-roster.json`
//! carries the measured files still below it: the port's counterpart of the exclusion roster
//! the source kept in its Vitest configuration, conditioned on the same host facts.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use path_clean::PathClean as _;
use seekdeep_pwsh_local::{PwshPlatform, resolve_pwsh_path};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{UncoveredLocationsReport, js_number};

/// The `cargo-llvm-cov` release the lane expects and the CI lanes install.
pub const CARGO_LLVM_COV_VERSION: &str = "0.8.5";

/// Repository-relative path of the adoption roster.
pub const ROSTER_PATH: &str = "scripts/coverage-roster.json";

/// Repository-relative path of the parity manifest naming the measured crates.
pub const MANIFEST_PATH: &str = "porting/parity.json";

/// The reason a regenerated roster records for a file it adds.
pub const ADOPTION_REASON: &str = "below the per-file bar when the instrumented lane measured it";

/// The note a regenerated roster carries.
pub const ROSTER_NOTE: &str = "Measured files still below the per-file 100% coverage bar. The instrumented lane (pnpm run test:coverage) holds every other measured file to the bar. An entry may name `platforms` (it applies only there) or `unless: pwsh` (it applies only where no PowerShell answers, as the source exempted its pwsh suites). The gate rejects entries for missing or unmeasured files and names entries whose file is fully covered; remove those.";

/// The crates the lane measures: every crate that realizes a `packages/*/*/src` source file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MeasuredSet {
    crates: BTreeSet<String>,
}

impl MeasuredSet {
    /// Derives the set from the verified rows of a parity manifest.
    ///
    /// # Errors
    ///
    /// Returns a manifest without a `surfaces` list.
    pub fn from_manifest(manifest: &Value) -> anyhow::Result<Self> {
        let surfaces = manifest
            .get("surfaces")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("parity manifest has no surfaces list"))?;
        let mut crates = BTreeSet::new();
        for surface in surfaces {
            let source = surface["source"].as_str().unwrap_or_default();
            if surface["status"] != "verified" || !is_package_source(source) {
                continue;
            }
            let targets = surface["targets"].as_array().into_iter().flatten();
            for target in targets.filter_map(Value::as_str) {
                if let Some(name) = crate_of(target) {
                    crates.insert(name.to_owned());
                }
            }
        }
        Ok(Self { crates })
    }

    /// Reads the manifest at its repository path.
    ///
    /// # Errors
    ///
    /// Returns a missing or malformed manifest.
    pub fn load(repository: &Path) -> anyhow::Result<Self> {
        let path = repository.join(MANIFEST_PATH);
        let bytes = std::fs::read(&path)
            .map_err(|error| anyhow::anyhow!("read parity manifest {}: {error}", path.display()))?;
        Self::from_manifest(&serde_json::from_slice(&bytes)?)
    }

    /// Builds a set from crate directories such as `crates/util`.
    #[must_use]
    pub fn of(crates: &[&str]) -> Self {
        Self {
            crates: crates.iter().map(|name| (*name).to_owned()).collect(),
        }
    }

    /// The measured crate directories, `crates/<name>`, in order.
    pub fn crates(&self) -> impl Iterator<Item = &str> {
        self.crates.iter().map(String::as_str)
    }

    /// Whether a repository-relative source path (with `/` separators) is held to the bar.
    ///
    /// Entry points under `src/bin` and `src/main.rs` execute only as spawned processes, as the
    /// source's `bin.ts` and `worker.ts` did, so process suites cover them instead.
    #[must_use]
    pub fn measures(&self, relative: &str) -> bool {
        let Some(name) = crate_of(relative) else {
            return false;
        };
        if !self.crates.contains(name) {
            return false;
        }
        let inside = &relative[name.len() + "/src/".len()..];
        inside != "main.rs" && !inside.starts_with("bin/")
    }
}

fn is_package_source(source: &str) -> bool {
    let mut parts = source.split('/');
    parts.next() == Some("packages")
        && parts.next().is_some_and(|group| !group.is_empty())
        && parts.next().is_some_and(|package| !package.is_empty())
        && parts.next() == Some("src")
        && parts.next().is_some_and(|tail| !tail.is_empty())
}

/// `crates/<name>` for a path under `crates/<name>/src/`.
fn crate_of(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("crates/")?;
    let name_end = rest.find('/').filter(|end| *end > 0)?;
    rest[name_end..]
        .strip_prefix("/src/")
        .filter(|tail| !tail.is_empty())?;
    Some(&path[.."crates/".len() + name_end])
}

/// One coverage metric's counts for a file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metric {
    /// Measured units.
    pub count: u64,
    /// Units executed at least once.
    pub covered: u64,
}

impl Metric {
    /// Istanbul's percentage: 100 for an empty metric, else rounded to two decimals.
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "coverage counts stay far below 2^53"
    )]
    pub fn percent(self) -> f64 {
        if self.count == 0 {
            100.0
        } else {
            (self.covered as f64 * 10_000.0 / self.count as f64).round() / 100.0
        }
    }

    /// Whether every unit was covered.
    #[must_use]
    pub fn complete(self) -> bool {
        self.covered >= self.count
    }

    fn add(&mut self, other: Self) {
        self.count += other.count;
        self.covered += other.covered;
    }
}

fn metric(summary: &Value, key: &str) -> Metric {
    Metric {
        count: summary[key]["count"].as_u64().unwrap_or(0),
        covered: summary[key]["covered"].as_u64().unwrap_or(0),
    }
}

/// One measured file's merged coverage.
#[derive(Clone, Debug, PartialEq)]
pub struct FileCoverage {
    /// Repository-relative path with `/` separators.
    pub path: String,
    /// Executable lines.
    pub lines: Metric,
    /// Functions, counted once across instantiations.
    pub functions: Metric,
    /// Code regions, the counterpart of Istanbul statements.
    pub regions: Metric,
    /// Branch arms when the run measured branches (`cargo llvm-cov --branch`, nightly only).
    pub branches: Option<Metric>,
    /// The Istanbul-shaped file coverage the source reporter consumes.
    pub istanbul: Value,
}

impl FileCoverage {
    /// The metrics the bar applies to, named as Vitest names its thresholds.
    #[must_use]
    pub fn thresholds(&self) -> Vec<(&'static str, Metric)> {
        let mut metrics = vec![
            ("lines", self.lines),
            ("functions", self.functions),
            ("statements", self.regions),
        ];
        if let Some(branches) = self.branches {
            metrics.push(("branches", branches));
        }
        metrics
    }

    /// The metric names below the bar.
    #[must_use]
    pub fn shortfalls(&self) -> Vec<&'static str> {
        self.thresholds()
            .into_iter()
            .filter(|(_, metric)| !metric.complete())
            .map(|(name, _)| name)
            .collect()
    }
}

type Span = (u64, u64, u64, u64);

#[derive(Default)]
struct Accumulator {
    lines: Metric,
    functions: Metric,
    regions: Metric,
    branches: Option<Metric>,
    /// Code regions by span with counts summed across instantiations.
    statements: BTreeMap<Span, u64>,
    /// Functions by declaration position: demangled name, declaration span, summed count.
    declared: BTreeMap<(u64, u64), (String, Span, u64)>,
    /// Branch arms: span, taken count, skipped count.
    arms: Vec<(Span, u64, u64)>,
}

impl Accumulator {
    fn absorb_file(&mut self, file: &Value) {
        let summary = &file["summary"];
        self.lines = metric(summary, "lines");
        self.functions = metric(summary, "functions");
        self.regions = metric(summary, "regions");
        let branches = metric(summary, "branches");
        self.branches = (branches.count > 0).then_some(branches);
        for arm in file["branches"].as_array().into_iter().flatten() {
            if let Some(numbers) = numbers(arm, 6) {
                self.arms.push((
                    (numbers[0], numbers[1], numbers[2], numbers[3]),
                    numbers[4],
                    numbers[5],
                ));
            }
        }
    }

    fn finish(self, path: String, root: &Path) -> FileCoverage {
        // The reporter derives its clickable path from this absolute form against the same
        // root, so the root as given yields exactly the repository-relative path.
        let absolute = root.join(&path);
        let location = |span: &Span| {
            json!({
                "start": {"line": span.0, "column": span.1.saturating_sub(1)},
                "end": {"line": span.2, "column": span.3.saturating_sub(1)},
            })
        };
        let mut statement_map = serde_json::Map::new();
        let mut statements = serde_json::Map::new();
        for (index, (span, count)) in self.statements.iter().enumerate() {
            statement_map.insert(index.to_string(), location(span));
            statements.insert(index.to_string(), json!(count));
        }
        let mut function_map = serde_json::Map::new();
        let mut functions = serde_json::Map::new();
        for (index, (name, span, count)) in self.declared.values().enumerate() {
            function_map.insert(
                index.to_string(),
                json!({"name": name, "decl": location(span), "loc": location(span)}),
            );
            functions.insert(index.to_string(), json!(count));
        }
        let mut branch_map = serde_json::Map::new();
        let mut branches = serde_json::Map::new();
        for (index, (span, taken, skipped)) in self.arms.iter().enumerate() {
            let arm = location(span);
            branch_map.insert(
                index.to_string(),
                json!({"type": "branch", "loc": arm, "locations": [arm, arm]}),
            );
            branches.insert(index.to_string(), json!([taken, skipped]));
        }
        FileCoverage {
            path,
            lines: self.lines,
            functions: self.functions,
            regions: self.regions,
            branches: self.branches,
            istanbul: json!({
                "path": absolute,
                "statementMap": statement_map,
                "s": statements,
                "fnMap": function_map,
                "f": functions,
                "branchMap": branch_map,
                "b": branches,
            }),
        }
    }
}

fn numbers(value: &Value, minimum: usize) -> Option<Vec<u64>> {
    let values = value
        .as_array()?
        .iter()
        .map(Value::as_u64)
        .collect::<Option<Vec<_>>>()?;
    (values.len() >= minimum).then_some(values)
}

/// The repository-relative form of an exported path, matched against the root as given and
/// as canonicalized (macOS reports `/var` builds under `/private/var`).
fn relative_inside(roots: &[PathBuf], filename: &str) -> Option<String> {
    let path = std::path::absolute(filename).ok()?.clean();
    let inside = |candidate: &Path| {
        roots
            .iter()
            .find_map(|root| candidate.strip_prefix(root).ok())
            .map(|relative| {
                relative
                    .components()
                    .map(|component| component.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/")
            })
    };
    inside(&path).or_else(|| inside(&dunce::canonicalize(&path).ok()?))
}

fn demangle(name: &str) -> String {
    format!("{:#}", rustc_demangle::demangle(name))
}

/// Translates an `llvm-cov export` document into per-file coverage over the measured set.
///
/// Instantiations of one function (generic arguments, separate test binaries) merge by
/// declaration position with summed counts, so a function is uncovered only when no
/// instantiation ran, and a code region is uncovered only when no instantiation reached it.
///
/// # Errors
///
/// Returns a document that is not an `llvm.coverage.json.export`.
pub fn translate_export(
    export: &Value,
    root: &Path,
    measured: &MeasuredSet,
) -> anyhow::Result<Vec<FileCoverage>> {
    anyhow::ensure!(
        export["type"] == "llvm.coverage.json.export",
        "coverage export is not an llvm-cov JSON export"
    );
    let root = std::path::absolute(root)?.clean();
    let mut roots = vec![root.clone()];
    if let Ok(canonical) = dunce::canonicalize(&root)
        && canonical != root
    {
        roots.push(canonical);
    }
    let mut files: BTreeMap<String, Accumulator> = BTreeMap::new();
    let mut keys: BTreeMap<String, String> = BTreeMap::new();
    for data in export["data"].as_array().into_iter().flatten() {
        for file in data["files"].as_array().into_iter().flatten() {
            let Some(filename) = file["filename"].as_str() else {
                continue;
            };
            let Some(relative) = relative_inside(&roots, filename) else {
                continue;
            };
            if !measured.measures(&relative) {
                continue;
            }
            let accumulator = files.entry(relative.clone()).or_default();
            accumulator.absorb_file(file);
            keys.insert(filename.to_owned(), relative);
        }
        for function in data["functions"].as_array().into_iter().flatten() {
            absorb_function(function, &keys, &mut files);
        }
    }
    Ok(files
        .into_iter()
        .map(|(path, accumulator)| accumulator.finish(path, &root))
        .collect())
}

fn absorb_function(
    function: &Value,
    keys: &BTreeMap<String, String>,
    files: &mut BTreeMap<String, Accumulator>,
) {
    let filenames = function["filenames"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    let count = function["count"].as_u64().unwrap_or(0);
    let mut declaration: Option<(String, Span)> = None;
    for region in function["regions"].as_array().into_iter().flatten() {
        let Some(numbers) = numbers(region, 8) else {
            continue;
        };
        // Only code regions carry statement counts; gaps, skips, expansions, and branch
        // regions describe other things.
        if numbers[7] != 0 {
            continue;
        }
        let Some(relative) = usize::try_from(numbers[5])
            .ok()
            .and_then(|index| filenames.get(index))
            .and_then(|filename| keys.get(*filename))
        else {
            continue;
        };
        let span = (numbers[0], numbers[1], numbers[2], numbers[3]);
        if let Some(accumulator) = files.get_mut(relative) {
            *accumulator.statements.entry(span).or_default() += numbers[4];
        }
        let outermost = declaration
            .as_ref()
            .is_none_or(|(_, first)| (span.0, span.1) < (first.0, first.1));
        if numbers[5] == 0 && outermost {
            declaration = Some((relative.clone(), span));
        }
    }
    if let Some((relative, span)) = declaration
        && let Some(accumulator) = files.get_mut(&relative)
    {
        let name = function["name"].as_str().unwrap_or_default();
        let entry = accumulator
            .declared
            .entry((span.0, span.1))
            .or_insert_with(|| (demangle(name), span, 0));
        entry.2 += count;
    }
}

/// One roster entry.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RosterEntry {
    /// Why the file is below the bar.
    pub reason: String,
    /// Platforms (`linux`, `macos`, `windows`) the entry applies to; every platform when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub platforms: Vec<String>,
    /// A host fact that lifts the entry: `pwsh` lifts it wherever `PowerShell` answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unless: Option<String>,
}

impl RosterEntry {
    /// Whether the entry is in force on a host.
    #[must_use]
    pub fn applies(&self, host: &Host) -> bool {
        let platform = self.platforms.is_empty() || self.platforms.contains(&host.platform);
        platform && !(self.unless.as_deref() == Some("pwsh") && host.pwsh)
    }
}

/// Measured files still below the per-file bar.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Roster {
    /// The roster's standing note.
    pub note: String,
    /// Entries by repository-relative path.
    pub files: BTreeMap<String, RosterEntry>,
}

impl Roster {
    /// Reads the roster.
    ///
    /// # Errors
    ///
    /// Returns a missing or malformed roster, or an entry naming an unknown condition or
    /// platform.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let bytes = std::fs::read(path)
            .map_err(|error| anyhow::anyhow!("read coverage roster {}: {error}", path.display()))?;
        let roster: Self = serde_json::from_slice(&bytes)?;
        for (file, entry) in &roster.files {
            anyhow::ensure!(
                entry
                    .unless
                    .as_deref()
                    .is_none_or(|condition| condition == "pwsh"),
                "coverage roster entry {file} names an unknown condition"
            );
            for platform in &entry.platforms {
                anyhow::ensure!(
                    matches!(platform.as_str(), "linux" | "macos" | "windows"),
                    "coverage roster entry {file} names an unknown platform {platform}"
                );
            }
        }
        Ok(roster)
    }

    /// Writes the roster as pretty JSON with a trailing newline.
    ///
    /// # Errors
    ///
    /// Returns serialization or write failures.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let mut text = serde_json::to_string_pretty(self)?;
        text.push('\n');
        std::fs::write(path, text)?;
        Ok(())
    }

    /// Whether an entry lifts the bar for a file on this host.
    #[must_use]
    pub fn applies(&self, file: &str, host: &Host) -> bool {
        self.files
            .get(file)
            .is_some_and(|entry| entry.applies(host))
    }
}

/// Host facts that condition roster entries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Host {
    /// `linux`, `macos`, `windows`, or the Rust OS name.
    pub platform: String,
    /// Whether `PowerShell` answers, resolved as the pwsh executor resolves it.
    pub pwsh: bool,
}

impl Host {
    /// Detects the running host, probing `PowerShell` exactly as the source's coverage
    /// configuration did: the executor's own resolution, then `-NoLogo -NoProfile
    /// -NonInteractive -Command $true`.
    #[must_use]
    pub fn detect() -> Self {
        let environment: BTreeMap<String, String> = std::env::vars().collect();
        let platform = if cfg!(windows) {
            PwshPlatform::Windows
        } else {
            PwshPlatform::Other
        };
        let pwsh = Command::new(resolve_pwsh_path(None, &environment, platform))
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "$true",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        Self {
            platform: std::env::consts::OS.to_owned(),
            pwsh,
        }
    }
}

/// The lane's verdict over one export.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Evaluation {
    /// The source reporter's console lines (uncovered locations); empty when green.
    pub report: Vec<String>,
    /// Totals over the measured files.
    pub summary: Vec<String>,
    /// Observations that do not fail the gate.
    pub notices: Vec<String>,
    /// Gate failures in Vitest's wording.
    pub errors: Vec<String>,
    /// Measured files below the bar with their failing metrics, roster or not.
    pub below_bar: BTreeMap<String, Vec<&'static str>>,
}

/// Evaluates measured files against the bar and the roster.
///
/// # Errors
///
/// Returns reporter failures on malformed translated coverage.
pub fn evaluate(
    files: &[FileCoverage],
    roster: &Roster,
    host: &Host,
    measured: &MeasuredSet,
    root: &Path,
) -> anyhow::Result<Evaluation> {
    let mut evaluation = Evaluation::default();
    let mut reporter = UncoveredLocationsReport {
        project_root: root.to_string_lossy().into_owned(),
        records: Vec::new(),
    };
    reporter.start();
    let mut totals = [Metric::default(); 3];
    let mut lifted_files = 0usize;
    for file in files {
        reporter.detail(&file.istanbul)?;
        totals[0].add(file.lines);
        totals[1].add(file.functions);
        totals[2].add(file.regions);
        let shortfalls = file.shortfalls();
        let lifted = roster.applies(&file.path, host);
        lifted_files += usize::from(lifted);
        if shortfalls.is_empty() {
            if lifted {
                evaluation.notices.push(format!(
                    "coverage roster entry is fully covered; remove it: {}",
                    file.path
                ));
            }
            continue;
        }
        evaluation.below_bar.insert(file.path.clone(), shortfalls);
        if lifted {
            continue;
        }
        for (name, metric) in file.thresholds() {
            if !metric.complete() {
                evaluation.errors.push(format!(
                    "ERROR: Coverage for {name} ({}%) does not meet global threshold (100%) for {}",
                    js_number(metric.percent()),
                    file.path
                ));
            }
        }
    }
    evaluation.report = reporter.finish();
    let instrumented = files
        .iter()
        .map(|file| file.path.as_str())
        .collect::<BTreeSet<_>>();
    for (path, entry) in &roster.files {
        if instrumented.contains(path.as_str()) {
            continue;
        }
        if !root.join(path).is_file() {
            evaluation.errors.push(format!(
                "coverage roster names a file that does not exist: {path}"
            ));
        } else if !measured.measures(path) {
            evaluation.errors.push(format!(
                "coverage roster names a file outside the measured set: {path}"
            ));
        } else if entry.applies(host) {
            evaluation.notices.push(format!(
                "coverage roster entry was not instrumented in this run: {path}"
            ));
        }
    }
    evaluation.summary = vec![format!(
        "coverage: {} measured files ({lifted_files} on the roster); lines {} ({}%), functions {} ({}%), statements {} ({}%)",
        files.len(),
        ratio(totals[0]),
        js_number(totals[0].percent()),
        ratio(totals[1]),
        js_number(totals[1].percent()),
        ratio(totals[2]),
        js_number(totals[2].percent()),
    )];
    Ok(evaluation)
}

fn ratio(metric: Metric) -> String {
    format!("{}/{}", metric.covered, metric.count)
}

/// Regenerates the roster from an evaluation: every measured file below the bar on this host
/// keeps its entry or receives the adoption reason, entries in force whose file now meets the
/// bar are dropped, and entries conditioned on other hosts are kept as they are.
#[must_use]
pub fn regenerate_roster(previous: &Roster, evaluation: &Evaluation, host: &Host) -> Roster {
    let mut files = BTreeMap::new();
    for (path, entry) in &previous.files {
        if !entry.applies(host) || evaluation.below_bar.contains_key(path) {
            files.insert(path.clone(), entry.clone());
        }
    }
    for path in evaluation.below_bar.keys() {
        if let Some(entry) = files.get_mut(path) {
            // An entry conditioned away from this host must widen to it.
            if !entry.platforms.is_empty() && !entry.platforms.contains(&host.platform) {
                entry.platforms.push(host.platform.clone());
            }
            if entry.unless.as_deref() == Some("pwsh") && host.pwsh {
                entry.unless = None;
            }
        } else {
            files.insert(
                path.clone(),
                RosterEntry {
                    reason: ADOPTION_REASON.to_owned(),
                    ..RosterEntry::default()
                },
            );
        }
    }
    Roster {
        note: ROSTER_NOTE.to_owned(),
        files,
    }
}

/// The entries a roster would need for this host's below-bar files that no entry lifts, in
/// the form a maintainer can paste after a lane run on a platform they cannot measure locally.
#[must_use]
pub fn roster_additions(
    evaluation: &Evaluation,
    roster: &Roster,
    host: &Host,
) -> BTreeMap<String, RosterEntry> {
    evaluation
        .below_bar
        .keys()
        .filter(|path| !roster.applies(path, host))
        .map(|path| {
            (
                path.clone(),
                RosterEntry {
                    reason: ADOPTION_REASON.to_owned(),
                    platforms: vec![host.platform.clone()],
                    unless: None,
                },
            )
        })
        .collect()
}
