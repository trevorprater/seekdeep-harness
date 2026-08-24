//! Spawn-backed model-facing `glob` and `grep` tools.

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    path::{Path, PathBuf},
    sync::Arc,
};

use seekdeep_cordis::{Context, EventOptions, EventReply, Plugin};
use seekdeep_llm::{ContentBlock, HarnessError};
use seekdeep_spill::{SPILL_STORE, SaveTextSpill, SpillOwner, SpillRef, SpillSource};
use seekdeep_subprocess::{
    SUBPROCESS, SubprocessCollect, SubprocessOutputMode, SubprocessSpawnSpec, SubprocessSpill,
    SubprocessStdinMode, SubprocessStdio,
};
use seekdeep_system_prompt::{PromptSection, SYSTEM_PROMPT};
use seekdeep_tools::{
    DefineToolOptions, DefineToolOutput, GenericCallView, PostToolDecision, SearchFileMatches,
    SearchLineMatch, SearchMatchesResultView, SearchPathsResultView, SearchResultView, TOOLS,
    ToolCallKind, ToolCallView, ToolDefinition, ToolExecution, ToolExecutionResult, ToolResult,
    ToolResultView, define_tool,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Loader plugin name.
pub const NAME: &str = "tool-fs-search";
/// Required services; formatted spill remains optional.
pub const INJECT: &[&str] = &["tools", "systemPrompt", "subprocess"];
/// Default glob inline path cap.
pub const GLOB_MAX_RESULTS: usize = 100;
/// Default grep inline match cap.
pub const GREP_MAX_MATCHES: usize = 250;
/// Default matched-line preview cap.
pub const GREP_MAX_LINE_BYTES: usize = 2_000;
/// Default raw ripgrep stdout cap.
pub const RAW_OUTPUT_MAX_BYTES: usize = 20_000_000;
/// Default search timeout declaration.
pub const SEARCH_TIMEOUT_MS: f64 = 30_000.0;
/// Default stderr diagnostic tail.
pub const SEARCH_STDERR_MAX_BYTES: usize = 64 * 1_024;
/// Default subprocess termination grace.
pub const SEARCH_GRACE_MS: f64 = 3_000.0;
/// Default persisted presentation-metadata cap.
pub const SEARCH_META_MAX_BYTES: usize = 65_536;
/// VCS directories excluded at every depth.
pub const GLOB_VCS_EXCLUDES: &[&str] = &[".git", ".svn", ".hg", ".bzr", ".jj", ".sl"];

/// Stable search error codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchErrorCode {
    /// Ripgrep rejected the regex or glob.
    InvalidPattern,
    /// Search launch, execution, or parsing failed.
    Failed,
    /// Complete raw stdout exceeded its transport cap.
    RawOutputOverflow,
    /// Caller cancellation or cooperative timeout stopped the call.
    Aborted,
}

impl SearchErrorCode {
    /// Exact wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidPattern => "SEARCH_INVALID_PATTERN",
            Self::Failed => "SEARCH_FAILED",
            Self::RawOutputOverflow => "SEARCH_RAW_OUTPUT_OVERFLOW",
            Self::Aborted => "SEARCH_ABORTED",
        }
    }
}

/// Typed spawn-backed search failure.
#[derive(Debug, thiserror::Error)]
#[error("{inner}")]
pub struct SearchError {
    #[source]
    inner: HarnessError,
}

impl SearchError {
    fn new(message: impl Into<String>, code: SearchErrorCode) -> Self {
        Self {
            inner: HarnessError::named("SearchError", message, code.as_str()),
        }
    }

    /// Stable route.
    #[must_use]
    pub fn code(&self) -> &str {
        self.inner.code()
    }
}

/// Deployment caps and over-cap sampling choice.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// Required explicit glob sampling choice.
    pub sample_over_cap_glob_results: Option<bool>,
    /// Inline path cap.
    pub glob_max_results: Option<u64>,
    /// Inline match cap.
    pub grep_max_matches: Option<u64>,
    /// Per-line UTF-8 preview cap.
    pub grep_max_line_bytes: Option<u64>,
    /// Persisted metadata cap.
    pub search_meta_max_bytes: Option<u64>,
    /// Complete raw stdout cap.
    pub raw_output_max_bytes: Option<u64>,
    /// Termination grace.
    pub grace_ms: Option<f64>,
    /// Stderr tail cap.
    pub stderr_max_bytes: Option<u64>,
    /// Cooperative timeout declaration.
    pub timeout_ms: Option<f64>,
}

#[derive(Clone, Copy)]
struct ResolvedConfig {
    sample: bool,
    glob_max: usize,
    grep_max: usize,
    line_max: usize,
    meta_max: usize,
    raw_max: usize,
    grace_ms: f64,
    stderr_max: usize,
    timeout_ms: f64,
}

fn positive(name: &str, value: f64) -> anyhow::Result<()> {
    anyhow::ensure!(
        value.is_finite() && value.fract() == 0.0 && value >= 1.0,
        "tool-fs-search: {name} must be a positive integer"
    );
    Ok(())
}

fn resolve_config(config: &Config) -> anyhow::Result<ResolvedConfig> {
    let to_usize = |name: &str, value: u64| -> anyhow::Result<usize> {
        anyhow::ensure!(
            value > 0,
            "tool-fs-search: {name} must be a positive integer"
        );
        usize::try_from(value).map_err(Into::into)
    };
    let grace_ms = config.grace_ms.unwrap_or(SEARCH_GRACE_MS);
    let timeout_ms = config.timeout_ms.unwrap_or(SEARCH_TIMEOUT_MS);
    positive("graceMs", grace_ms)?;
    positive("timeoutMs", timeout_ms)?;
    anyhow::ensure!(
        grace_ms <= seekdeep_util::timeout::MAX_TIMER_DELAY_MS,
        "tool-fs-search: graceMs must be no greater than {}",
        seekdeep_util::timeout::MAX_TIMER_DELAY_MS
    );
    Ok(ResolvedConfig {
        sample: config
            .sample_over_cap_glob_results
            .ok_or_else(|| anyhow::anyhow!("sampleOverCapGlobResults is required"))?,
        glob_max: to_usize(
            "globMaxResults",
            config.glob_max_results.unwrap_or(GLOB_MAX_RESULTS as u64),
        )?,
        grep_max: to_usize(
            "grepMaxMatches",
            config.grep_max_matches.unwrap_or(GREP_MAX_MATCHES as u64),
        )?,
        line_max: to_usize(
            "grepMaxLineBytes",
            config
                .grep_max_line_bytes
                .unwrap_or(GREP_MAX_LINE_BYTES as u64),
        )?,
        meta_max: to_usize(
            "searchMetaMaxBytes",
            config
                .search_meta_max_bytes
                .unwrap_or(SEARCH_META_MAX_BYTES as u64),
        )?,
        raw_max: to_usize(
            "rawOutputMaxBytes",
            config
                .raw_output_max_bytes
                .unwrap_or(RAW_OUTPUT_MAX_BYTES as u64),
        )?,
        grace_ms,
        stderr_max: to_usize(
            "stderrMaxBytes",
            config
                .stderr_max_bytes
                .unwrap_or(SEARCH_STDERR_MAX_BYTES as u64),
        )?,
        timeout_ms,
    })
}

#[derive(Clone, Debug, Deserialize, Serialize)]
/// Validated glob call arguments.
pub struct GlobInput {
    /// Ripgrep glob pattern.
    pub pattern: String,
    /// Optional search root.
    pub path: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
/// Validated grep call arguments.
pub struct GrepInput {
    /// Ripgrep regular expression.
    pub pattern: String,
    /// Optional file or directory target.
    pub path: Option<String>,
    /// Optional positive glob filter.
    pub include: Option<String>,
}

/// Validates glob arguments.
///
/// # Errors
///
/// Rejects blank patterns or supplied blank roots.
pub fn parse_glob_args(input: GlobInput) -> anyhow::Result<GlobInput> {
    anyhow::ensure!(
        !input.pattern.trim().is_empty(),
        "pattern must be a non-empty string"
    );
    anyhow::ensure!(
        input
            .path
            .as_ref()
            .is_none_or(|path| !path.trim().is_empty()),
        "path must be a non-empty string when given"
    );
    Ok(input)
}

/// Validates grep arguments, including one positive include glob.
///
/// # Errors
///
/// Rejects empty patterns, blank roots, negated includes, or include lists.
pub fn parse_grep_args(input: GrepInput) -> anyhow::Result<GrepInput> {
    anyhow::ensure!(
        !input.pattern.is_empty(),
        "pattern must be a non-empty string"
    );
    anyhow::ensure!(
        input
            .path
            .as_ref()
            .is_none_or(|path| !path.trim().is_empty()),
        "path must be a non-empty string when given"
    );
    if let Some(include) = &input.include {
        anyhow::ensure!(
            !include.trim().is_empty(),
            "include must be a non-empty glob when given"
        );
        anyhow::ensure!(
            !include.starts_with('!'),
            "include must be a positive glob filter; negated patterns (\"!…\") are not supported"
        );
        let mut depth = 0_i64;
        for character in include.chars() {
            match character {
                '{' => depth += 1,
                '}' => depth = (depth - 1).max(0),
                ',' if depth == 0 => anyhow::bail!(
                    "include must be one glob, not a comma-separated list (use {{a,b}} alternation instead)"
                ),
                _ => {}
            }
        }
    }
    Ok(input)
}

/// Builds fixed `rg --files` arguments.
#[must_use]
pub fn build_glob_command(input: &GlobInput) -> Vec<String> {
    let mut parts = vec![
        "--files".into(),
        format!("--glob={}", input.pattern),
        "--sort=modified".into(),
        "--no-ignore".into(),
        "--hidden".into(),
    ];
    for name in GLOB_VCS_EXCLUDES {
        parts.extend([
            format!("--glob=!**/{name}"),
            format!("--glob=!**/{name}/**"),
        ]);
    }
    if let Some(path) = &input.path {
        parts.extend(["--".into(), path.clone()]);
    }
    parts
}

/// Builds fixed `rg --json` arguments.
#[must_use]
pub fn build_grep_command(input: &GrepInput) -> Vec<String> {
    let mut parts = vec!["--json".into(), format!("--regexp={}", input.pattern)];
    if let Some(include) = &input.include {
        parts.push(format!("--glob={include}"));
    }
    if let Some(path) = &input.path {
        parts.extend(["--".into(), path.clone()]);
    }
    parts
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
/// One parsed ripgrep content match.
pub struct GrepMatch {
    /// Display path.
    pub path: String,
    /// One-based line number.
    pub line_number: u64,
    /// Matched-line text or non-UTF-8 placeholder.
    pub line: String,
}

/// Parses complete ripgrep NDJSON output.
///
/// # Errors
///
/// Returns `SEARCH_FAILED` for malformed JSON match records.
pub fn parse_grep_matches(stdout: &str) -> anyhow::Result<Vec<GrepMatch>> {
    let malformed = |detail: &str| {
        anyhow::Error::new(SearchError::new(
            format!("grep received malformed ripgrep --json output ({detail})"),
            SearchErrorCode::Failed,
        ))
    };
    let mut matches = Vec::new();
    for line in stdout.lines() {
        if line.is_empty() {
            continue;
        }
        let record: Value =
            serde_json::from_str(line).map_err(|_| malformed("a line is not JSON"))?;
        let Some(object) = record.as_object() else {
            return Err(malformed("a record is not an object"));
        };
        if object.get("type").and_then(Value::as_str) != Some("match") {
            continue;
        }
        let data = object
            .get("data")
            .and_then(Value::as_object)
            .ok_or_else(|| malformed("a match record has no data"))?;
        let path = data
            .get("path")
            .and_then(Value::as_object)
            .and_then(|path| path.get("text"))
            .and_then(Value::as_str)
            .ok_or_else(|| malformed("a match record has no path text"))?;
        let line_number = data
            .get("line_number")
            .and_then(Value::as_u64)
            .ok_or_else(|| malformed("a match record has no line number"))?;
        let lines = data
            .get("lines")
            .and_then(Value::as_object)
            .ok_or_else(|| malformed("a match record has no line content"))?;
        let line = if let Some(text) = lines.get("text").and_then(Value::as_str) {
            text.strip_suffix("\r\n")
                .or_else(|| text.strip_suffix('\n'))
                .unwrap_or(text)
                .to_owned()
        } else if lines.get("bytes").and_then(Value::as_str).is_some() {
            "(line is not valid UTF-8)".to_owned()
        } else {
            return Err(malformed("a match record has neither line text nor bytes"));
        };
        matches.push(GrepMatch {
            path: path.to_owned(),
            line_number,
            line,
        });
    }
    Ok(matches)
}

/// Preserves UTF-8 while bounding one line preview.
#[must_use]
pub fn preview_line(line: &str, max_bytes: usize) -> String {
    if line.len() <= max_bytes {
        return line.to_owned();
    }
    let mut end = max_bytes.min(line.len());
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{} (line truncated)", &line[..end])
}

/// Maps an absolute inside-workdir path to its relative display spelling.
#[must_use]
pub fn to_workdir_relative(path: &str, workdir: &Path) -> String {
    let path_value = Path::new(path);
    if !path_value.is_absolute() {
        return path.to_owned();
    }
    match path_value.strip_prefix(workdir) {
        Ok(relative) if relative.as_os_str().is_empty() => ".".to_owned(),
        Ok(relative) => relative.to_string_lossy().into_owned(),
        Err(_) => path.to_owned(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "shape", rename_all = "lowercase")]
enum SearchMeta {
    Matches {
        files: Vec<SearchFileMatches>,
        truncated: bool,
        total: u64,
    },
    Paths {
        paths: Vec<String>,
        truncated: bool,
        total: u64,
    },
}

fn group_matches(matches: &[GrepMatch]) -> Vec<SearchFileMatches> {
    let mut order = Vec::<String>::new();
    let mut groups = BTreeMap::<String, Vec<SearchLineMatch>>::new();
    for item in matches {
        if !groups.contains_key(&item.path) {
            order.push(item.path.clone());
        }
        groups
            .entry(item.path.clone())
            .or_default()
            .push(SearchLineMatch {
                line_number: item.line_number,
                line: item.line.clone(),
            });
    }
    order
        .into_iter()
        .map(|path| SearchFileMatches {
            matches: groups.remove(&path).unwrap_or_default(),
            path,
        })
        .collect()
}

fn cap_meta(mut meta: SearchMeta, max: usize) -> SearchMeta {
    while serde_json::to_vec(&meta).map_or(0, |bytes| bytes.len()) > max {
        match &mut meta {
            SearchMeta::Matches {
                files, truncated, ..
            } if files.len() > 1 => {
                files.pop();
                *truncated = true;
            }
            SearchMeta::Paths {
                paths, truncated, ..
            } if paths.len() > 1 => {
                paths.pop();
                *truncated = true;
            }
            _ => break,
        }
    }
    meta
}

fn search_view(meta: Option<&Value>) -> Option<SearchResultView> {
    let meta: SearchMeta = serde_json::from_value(meta?.clone()).ok()?;
    Some(match meta {
        SearchMeta::Matches {
            files,
            truncated,
            total,
        } => SearchResultView::Matches(SearchMatchesResultView {
            title: None,
            files,
            truncated,
            total,
        }),
        SearchMeta::Paths {
            paths,
            truncated,
            total,
        } => SearchResultView::Paths(SearchPathsResultView {
            title: None,
            paths,
            truncated,
            total,
        }),
    })
}

fn format_matches(matches: &[GrepMatch]) -> String {
    group_matches(matches)
        .into_iter()
        .map(|file| {
            format!(
                "{}\n{}",
                file.path,
                file.matches
                    .into_iter()
                    .map(|item| format!("Line {}: {}", item.line_number, item.line))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn retained_matches(matches: &[GrepMatch], max_matches: usize, max_line: usize) -> Vec<GrepMatch> {
    matches
        .iter()
        .take(max_matches)
        .map(|item| GrepMatch {
            path: item.path.clone(),
            line_number: item.line_number,
            line: preview_line(&item.line, max_line),
        })
        .collect()
}

fn format_grep(matches: &[GrepMatch], caps: ResolvedConfig, spill: Option<&SpillRef>) -> String {
    if matches.is_empty() {
        return "No matches found".to_owned();
    }
    let retained = retained_matches(matches, caps.grep_max, caps.line_max);
    let header = if matches.len() > retained.len() {
        format!("Found {} of {} matches", retained.len(), matches.len())
    } else {
        format!(
            "Found {} {}",
            matches.len(),
            if matches.len() == 1 {
                "match"
            } else {
                "matches"
            }
        )
    };
    let mut text = format!("{header}\n\n{}", format_matches(&retained));
    if matches.len() > retained.len() {
        let recovery = spill.map_or_else(|| "The complete result could not be saved; narrow pattern, path, or include to see more.".to_owned(),
            |spill| format!("Full grep result stored at: {}. {}", spill.locator.as_str(), spill.retrieval_hint));
        let _ = write!(text, "\n\n({recovery})");
    }
    text
}

fn top_level(path: &str, root: &str) -> String {
    let relative = if root == "." {
        path.strip_prefix(&format!(".{}", std::path::MAIN_SEPARATOR))
            .unwrap_or(path)
    } else {
        Path::new(path)
            .strip_prefix(root)
            .ok()
            .and_then(|path| path.to_str())
            .unwrap_or(path)
    };
    relative
        .trim_start_matches(std::path::MAIN_SEPARATOR)
        .split(std::path::MAIN_SEPARATOR)
        .next()
        .unwrap_or("")
        .to_owned()
}

/// Samples an over-cap path list round-robin across top-level entries.
#[must_use]
pub fn sample_across_top_level(
    paths: &[String],
    max: usize,
    root: &str,
) -> (Vec<String>, usize, usize) {
    let mut order = Vec::new();
    let mut groups = BTreeMap::<String, Vec<String>>::new();
    for path in paths {
        let key = top_level(path, root);
        if !groups.contains_key(&key) {
            order.push(key.clone());
        }
        groups.entry(key).or_default().push(path.clone());
    }
    let total = order.len();
    let mut indices = vec![0_usize; total];
    let mut output = Vec::new();
    while output.len() < max {
        let mut progressed = false;
        for (position, key) in order.iter().enumerate() {
            if output.len() == max {
                break;
            }
            if let Some(item) = groups[key].get(indices[position]) {
                output.push(item.clone());
                indices[position] += 1;
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }
    let shown = indices.iter().filter(|index| **index > 0).count();
    (output, shown, total)
}

fn format_glob(
    paths: &[String],
    root: &str,
    caps: ResolvedConfig,
    spill: Option<&SpillRef>,
) -> String {
    if paths.is_empty() {
        return "No files found".to_owned();
    }
    if paths.len() <= caps.glob_max {
        return paths.join("\n");
    }
    let (items, shown, total) = if caps.sample {
        sample_across_top_level(paths, caps.glob_max, root)
    } else {
        (paths[..caps.glob_max].to_vec(), 0, 0)
    };
    let basis = if !caps.sample || total == paths.len() {
        ".".to_owned()
    } else {
        format!(
            ", sampled across {shown} of the {total} top-level entries this pattern matched instead of taken in modification-time order.{}",
            if shown < total {
                " Narrow path to inspect a specific subtree."
            } else {
                ""
            }
        )
    };
    let recovery = spill.map_or_else(
        || "The complete result could not be saved; narrow pattern or path to see more.".to_owned(),
        |spill| {
            format!(
                "Full sorted result stored at: {}. {}",
                spill.locator.as_str(),
                spill.retrieval_hint
            )
        },
    );
    format!(
        "{}\n\n(Showing {} of {} paths{} {})",
        items.join("\n"),
        items.len(),
        paths.len(),
        basis,
        recovery
    )
}

struct SearchRuntime {
    subprocess: Arc<seekdeep_subprocess::SubprocessService>,
    rg_path: RgPathCache,
}

/// Lazy, rejection-memoizing packaged-ripgrep path cache.
#[derive(Debug, Default)]
pub struct RgPathCache(tokio::sync::OnceCell<Result<String, String>>);

impl RgPathCache {
    /// Resolves once and memoizes both success and failure.
    ///
    /// # Errors
    ///
    /// Maps a missing or corrupt packaged binary to `SEARCH_FAILED` on every call.
    pub async fn resolve_with<F, Fut>(&self, resolver: F) -> anyhow::Result<String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<String, String>>,
    {
        self.0.get_or_init(resolver).await.clone().map_err(|_| {
            anyhow::Error::new(SearchError::new(
                "search could not start its search command (ripgrep launch failed)",
                SearchErrorCode::Failed,
            ))
        })
    }
}

impl SearchRuntime {
    fn bundled_rg() -> Option<String> {
        let executable = std::env::current_exe().ok()?;
        let directory = executable.parent()?;
        let name = if cfg!(windows) { "rg.exe" } else { "rg" };
        [directory.join(name), directory.join("bin").join(name)]
            .into_iter()
            .find(|path| path.is_file())
            .map(|path| path.to_string_lossy().into_owned())
    }

    async fn rg_path(&self) -> anyhow::Result<String> {
        self.rg_path
            .resolve_with(|| async {
                if let Some(path) = Self::bundled_rg() {
                    return Ok(path);
                }
                #[cfg(debug_assertions)]
                {
                    return self
                        .subprocess
                        .resolve_executable("rg", None, None)
                        .await
                        .map_err(|error| error.to_string());
                }
                #[cfg(not(debug_assertions))]
                {
                    Err(
                        "the bundled ripgrep asset is missing beside the SeekDeep executable"
                            .to_owned(),
                    )
                }
            })
            .await
    }

    #[allow(clippy::too_many_lines)]
    async fn run(
        &self,
        exec: &ToolExecution,
        tool: &str,
        argv: Vec<String>,
        caps: ResolvedConfig,
    ) -> anyhow::Result<(String, bool, PathBuf)> {
        let signal = exec.signal();
        if signal.is_aborted() {
            return Err(anyhow::Error::new(SearchError::new(
                format!(
                    "{tool} was aborted before completion (tool timeout or caller cancellation)"
                ),
                SearchErrorCode::Aborted,
            )));
        }
        let workdir = exec.session_cwd().map_or_else(
            || std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            PathBuf::from,
        );
        let mut full_argv = vec![self.rg_path().await?, "--no-config".into()];
        full_argv.extend(argv);
        #[allow(clippy::cast_precision_loss)]
        let collect = |max_bytes| {
            SubprocessOutputMode::Collect(SubprocessCollect {
                max_bytes: max_bytes as f64,
                spill: None::<SubprocessSpill>,
            })
        };
        let handle = self
            .subprocess
            .spawn(SubprocessSpawnSpec {
                argv: full_argv,
                cwd: workdir.clone(),
                stdio: SubprocessStdio {
                    stdin: SubprocessStdinMode::Ignore,
                    stdout: collect(caps.raw_max),
                    stderr: collect(caps.stderr_max),
                },
                grace_ms: caps.grace_ms,
                signal: Some(signal.clone()),
                env: None,
            })
            .map_err(|_| {
                anyhow::Error::new(SearchError::new(
                    format!("{tool} could not start its search command (ripgrep launch failed)"),
                    SearchErrorCode::Failed,
                ))
            })?;
        let outcome = handle.done().await.map_err(|_| {
            anyhow::Error::new(SearchError::new(
                format!("{tool} could not start its search command (ripgrep launch failed)"),
                SearchErrorCode::Failed,
            ))
        })?;
        let collected = handle.collected();
        let stdout = collected
            .stdout
            .ok_or_else(|| {
                anyhow::Error::new(SearchError::new(
                    format!("{tool} search command produced no collected output streams"),
                    SearchErrorCode::Failed,
                ))
            })?
            .read_from(0);
        let stderr = collected
            .stderr
            .ok_or_else(|| {
                anyhow::Error::new(SearchError::new(
                    format!("{tool} search command produced no collected output streams"),
                    SearchErrorCode::Failed,
                ))
            })?
            .read_from(0);
        if signal.is_aborted() {
            return Err(anyhow::Error::new(SearchError::new(
                format!(
                    "{tool} was aborted before completion (tool timeout or caller cancellation)"
                ),
                SearchErrorCode::Aborted,
            )));
        }
        let Some(exit) = outcome.exit_code else {
            return Err(anyhow::Error::new(SearchError::new(
                format!(
                    "{tool} search command was killed by signal {}",
                    outcome
                        .signal
                        .as_ref()
                        .map_or("(unknown)", |signal| signal.as_str())
                ),
                SearchErrorCode::Failed,
            )));
        };
        if exit != 0 && exit != 1 {
            let mut detail = stderr.text.trim().to_owned();
            if stderr.lossy && !detail.is_empty() {
                detail.push_str(" [stderr truncated]");
            }
            let invalid = detail.to_lowercase().contains("regex parse error")
                || detail.to_lowercase().contains("error parsing glob");
            let message = if invalid {
                format!("{tool} pattern rejected by ripgrep: {detail}")
            } else {
                format!(
                    "{tool} search failed (exit {exit}){}",
                    if detail.is_empty() {
                        String::new()
                    } else {
                        format!(": {detail}")
                    }
                )
            };
            return Err(anyhow::Error::new(SearchError::new(
                message,
                if invalid {
                    SearchErrorCode::InvalidPattern
                } else {
                    SearchErrorCode::Failed
                },
            )));
        }
        if stdout.lossy || stdout.text.len() > caps.raw_max {
            return Err(anyhow::Error::new(SearchError::new(
                format!(
                    "{tool} produced more raw output than the subprocess seam retained within the {}-byte cap; narrow pattern, path, or include and retry",
                    caps.raw_max
                ),
                SearchErrorCode::RawOutputOverflow,
            )));
        }
        Ok((stdout.text, exit == 1, workdir))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct GlobValue {
    root: String,
    paths: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct GrepValue {
    matches: Vec<GrepMatch>,
}

async fn save_spill(
    context: &Context,
    execution: &ToolExecution,
    name: &str,
    body: String,
) -> Option<SpillRef> {
    let session = execution.session()?;
    let store = context.get(SPILL_STORE)?;
    store
        .save_text(SaveTextSpill {
            owner: SpillOwner {
                session_id: session.id().clone(),
            },
            source: SpillSource {
                tool_name: execution.name.clone(),
                call_id: execution.call_id.clone(),
                label: "result".into(),
            },
            suggested_name: name.into(),
            content: body,
        })
        .await
        .ok()
}

fn install_post_handler(
    context: &Context,
    tools: Arc<seekdeep_tools::ToolRuntime>,
    definition: Arc<ToolDefinition>,
    caps: ResolvedConfig,
    glob: bool,
) -> anyhow::Result<()> {
    let owner = context.clone();
    context.events().on_waterfall(
        context,
        "tools/post-execute",
        move |_, args, next| {
            let execution = args.get::<ToolExecution>(0);
            let result = args.get::<ToolExecutionResult>(1);
            let owner = owner.clone();
            let tools = tools.clone();
            let definition = definition.clone();
            Box::pin(async move {
                let reply = next.run().await?;
                let decision = reply
                    .downcast::<PostToolDecision>()
                    .map(|value| (*value).clone())
                    .ok_or_else(|| {
                        anyhow::anyhow!("tools/post-execute returned an invalid decision")
                    })?;
                let Some(execution) = execution else {
                    return Ok(EventReply::Value(Arc::new(decision)));
                };
                let Some(result) = result else {
                    return Ok(EventReply::Value(Arc::new(decision)));
                };
                if execution.parent.is_some()
                    || execution.name != definition.name
                    || result.is_error()
                    || !matches!(decision, PostToolDecision::Accept { content: None, .. })
                    || tools
                        .get(&execution.name, execution.scope_key())
                        .is_none_or(|live| !Arc::ptr_eq(&live, &definition))
                {
                    return Ok(EventReply::Value(Arc::new(decision)));
                }
                let Some(value) = result.value() else {
                    return Ok(EventReply::Value(Arc::new(decision)));
                };
                let replacement = if glob {
                    let value: GlobValue = serde_json::from_value(value.clone())?;
                    if value.paths.len() <= caps.glob_max {
                        None
                    } else {
                        let spill = save_spill(
                            &owner,
                            &execution,
                            "glob-results.txt",
                            value.paths.join("\n"),
                        )
                        .await;
                        Some(format_glob(&value.paths, &value.root, caps, spill.as_ref()))
                    }
                } else {
                    let value: GrepValue = serde_json::from_value(value.clone())?;
                    if value.matches.len() <= caps.grep_max {
                        None
                    } else {
                        let previewed = value
                            .matches
                            .iter()
                            .map(|item| GrepMatch {
                                path: item.path.clone(),
                                line_number: item.line_number,
                                line: preview_line(&item.line, caps.line_max),
                            })
                            .collect::<Vec<_>>();
                        let spill = save_spill(
                            &owner,
                            &execution,
                            "grep-results.txt",
                            format!(
                                "Found {} matches\n\n{}",
                                previewed.len(),
                                format_matches(&previewed)
                            ),
                        )
                        .await;
                        Some(format_grep(&value.matches, caps, spill.as_ref()))
                    }
                };
                let additional_contexts = match &decision {
                    PostToolDecision::Accept {
                        additional_contexts,
                        ..
                    } => additional_contexts.clone(),
                    _ => Vec::new(),
                };
                let decision = replacement.map_or(decision, |text| PostToolDecision::Accept {
                    content: Some(vec![ContentBlock::Text { text }]),
                    additional_contexts,
                });
                Ok(EventReply::Value(Arc::new(decision)))
            })
        },
        EventOptions::default(),
    )?;
    Ok(())
}

/// Registers both search tools.
///
/// # Errors
///
/// Returns invalid configuration, missing dependencies, schema, registration,
/// prompt, or event-listener failures.
pub fn apply(context: &Context, config: &Config) -> anyhow::Result<()> {
    let caps = resolve_config(config)?;
    let tools = context
        .get(TOOLS)
        .ok_or_else(|| anyhow::anyhow!("tool-fs-search requires tools"))?;
    let prompt = context
        .get(SYSTEM_PROMPT)
        .ok_or_else(|| anyhow::anyhow!("tool-fs-search requires systemPrompt"))?;
    let subprocess = context
        .get(SUBPROCESS)
        .ok_or_else(|| anyhow::anyhow!("tool-fs-search requires subprocess"))?;
    let runtime = Arc::new(SearchRuntime {
        subprocess,
        rg_path: RgPathCache::default(),
    });
    prompt.section(context, PromptSection::new("tool:glob", 103.0, seekdeep_system_prompt::PromptText::Static("Use the glob tool — not shell find — to discover files by path pattern. A pattern with no \"/\" matches basenames at any depth, so \"*\" matches every file in the tree rather than its top level. Results are files only, never directories, and include hidden and ignored files.".into())))?;
    prompt.section(context, PromptSection::new("tool:grep", 104.0, seekdeep_system_prompt::PromptText::Static("Use the grep tool — not shell grep or rg — to search file contents. Use read on a matched file when you need surrounding context.".into())))?;

    let glob_output = DefineToolOutput::new(
        json!({"type":"object","additionalProperties":false,"properties":{"root":{"type":"string","required":true},"paths":{"type":"array","required":true,"items":{"type":"string"}}}}),
        Arc::new(move |_args: &GlobInput, value: &GlobValue| Ok(vec![ContentBlock::Text { text: format_glob(&value.paths, &value.root, caps, None) }])),
    ).presentation_meta(Arc::new(move |_args, value| {
        let paths = if value.paths.len() <= caps.glob_max { value.paths.clone() } else if caps.sample { sample_across_top_level(&value.paths, caps.glob_max, &value.root).0 } else { value.paths[..caps.glob_max].to_vec() };
        Ok(serde_json::to_value(cap_meta(SearchMeta::Paths { paths, truncated: value.paths.len() > caps.glob_max, total: value.paths.len() as u64 }, caps.meta_max))?)
    }));
    let glob_runtime = runtime.clone();
    let glob_options = DefineToolOptions::new(
        "glob",
        "Find files whose paths match a glob pattern. Returns matching file paths — never directories — including hidden and ignored files (VCS metadata directories are excluded).",
        json!({"pattern":{"type":"string","required":true},"path":{"type":"string"}}),
        glob_output,
        Arc::new(move |args: GlobInput, exec| { let runtime = glob_runtime.clone(); Box::pin(async move {
            let input = parse_glob_args(args)?;
            let (stdout, no_matches, workdir) = runtime.run(&exec, "glob", build_glob_command(&input), caps).await?;
            let root = input.path.as_ref().map_or_else(|| ".".into(), |path| to_workdir_relative(path, &workdir));
            let paths = if no_matches { Vec::new() } else { stdout.lines().filter(|line| !line.is_empty()).map(|line| to_workdir_relative(line, &workdir)).collect() };
            Ok(GlobValue { root, paths })
        }) }),
    ).timeout_ms(caps.timeout_ms)
        .present_call(Arc::new(|args| Some(ToolCallView::Generic(GenericCallView { title: format!("Glob {}{}", args.pattern, args.path.as_ref().map_or(String::new(), |path| format!(" in {path}"))), kind: Some(ToolCallKind::Search), raw_input: Some(json!(args.pattern)), content: None, locations: None }))))
        .present_result(Arc::new(|_, result: &ToolResult| if result.is_error { None } else { search_view(result.meta.as_ref()).map(ToolResultView::Search).filter(|view| matches!(view, ToolResultView::Search(SearchResultView::Paths(_)))) }));
    tools.register(context, define_tool(glob_options)?)?;
    let glob_def = tools
        .get("glob", seekdeep_scope::scope_of(context))
        .ok_or_else(|| anyhow::anyhow!("registered glob disappeared"))?;
    install_post_handler(context, tools.clone(), glob_def, caps, true)?;

    let grep_output = DefineToolOutput::new(
        json!({"type":"object","additionalProperties":false,"properties":{"matches":{"type":"array","required":true,"items":{"type":"object","additionalProperties":false,"properties":{"path":{"type":"string","required":true},"lineNumber":{"type":"integer","required":true},"line":{"type":"string","required":true}}}}}}),
        Arc::new(move |_args: &GrepInput, value: &GrepValue| Ok(vec![ContentBlock::Text { text: format_grep(&value.matches, caps, None) }])),
    ).presentation_meta(Arc::new(move |_args, value| {
        let retained = retained_matches(&value.matches, caps.grep_max, caps.line_max);
        Ok(serde_json::to_value(cap_meta(SearchMeta::Matches { files: group_matches(&retained), truncated: value.matches.len() > caps.grep_max, total: value.matches.len() as u64 }, caps.meta_max))?)
    }));
    let grep_runtime = runtime;
    let grep_options = DefineToolOptions::new(
        "grep",
        "Search file contents with a ripgrep regular expression. Returns matching lines with line numbers, grouped by file. Use read on a matched file for surrounding context.",
        json!({"pattern":{"type":"string","required":true},"path":{"type":"string"},"include":{"type":"string"}}),
        grep_output,
        Arc::new(move |args: GrepInput, exec| { let runtime = grep_runtime.clone(); Box::pin(async move {
            let input = parse_grep_args(args)?;
            let (stdout, no_matches, workdir) = runtime.run(&exec, "grep", build_grep_command(&input), caps).await?;
            let matches = if no_matches { Vec::new() } else { parse_grep_matches(&stdout)?.into_iter().map(|mut item| { item.path = to_workdir_relative(&item.path, &workdir); item }).collect() };
            Ok(GrepValue { matches })
        }) }),
    ).timeout_ms(caps.timeout_ms)
        .present_call(Arc::new(|args| Some(ToolCallView::Generic(GenericCallView { title: format!("Grep {}{}{}", args.pattern, args.path.as_ref().map_or(String::new(), |path| format!(" in {path}")), args.include.as_ref().map_or(String::new(), |include| format!(" ({include})"))), kind: Some(ToolCallKind::Search), raw_input: Some(json!(args.pattern)), content: None, locations: None }))))
        .present_result(Arc::new(|_, result: &ToolResult| if result.is_error { None } else { search_view(result.meta.as_ref()).map(ToolResultView::Search).filter(|view| matches!(view, ToolResultView::Search(SearchResultView::Matches(_)))) }));
    tools.register(context, define_tool(grep_options)?)?;
    let grep_def = tools
        .get("grep", seekdeep_scope::scope_of(context))
        .ok_or_else(|| anyhow::anyhow!("registered grep disappeared"))?;
    install_post_handler(context, tools, grep_def, caps, false)?;
    Ok(())
}

/// Loader-compatible plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, config| {
        Box::pin(async move {
            let config: Config = serde_json::from_value(config)?;
            apply(&context, &config)
        })
    })
    .with_config_validator(|value| {
        let config: Config = serde_json::from_value(value.clone())?;
        resolve_config(&config)?;
        Ok(value.clone())
    })
}
