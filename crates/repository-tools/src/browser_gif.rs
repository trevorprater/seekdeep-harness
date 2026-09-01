//! Browser screenshot GIF encoding through an injected media backend.

use std::{
    env,
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context as _, Result, bail};
use glob::{MatchOptions, Pattern, glob_with};
use num_traits::ToPrimitive as _;
use path_clean::PathClean as _;
use serde::Serialize;
use serde_json::{Map, Value};

/// Default maximum encoded GIF size: five mebibytes.
pub const DEFAULT_MAX_BYTES: u64 = 5 * 1024 * 1024;

/// Validated command input for one GIF encoding run.
#[derive(Clone, Debug, PartialEq)]
pub struct EncodeGifOptions {
    /// Directory containing the source screenshots.
    pub frames: PathBuf,
    /// Destination `.gif` path.
    pub output: PathBuf,
    /// Glob evaluated relative to `frames`.
    pub pattern: String,
    /// One duration or one comma-separated duration per source frame.
    pub durations: String,
    /// Encoded frames per second.
    pub fps: u64,
    /// Maximum output width.
    pub max_width: u64,
    /// Palette color count.
    pub colors: u64,
    /// Maximum output byte size.
    pub max_bytes: u64,
    /// Whether an existing destination may be replaced.
    pub force: bool,
}

/// Verified machine-readable result from one encoding run.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EncodeGifSummary {
    /// Encoded file size.
    pub bytes: u64,
    /// Probed output duration.
    pub duration_seconds: f64,
    /// Probed encoded frame count.
    pub encoded_frames: u64,
    /// Requested encoded frame rate.
    pub fps: u64,
    /// Probed output height.
    pub height: u64,
    /// Absolute output path.
    pub output: String,
    /// Number of source screenshots.
    pub source_frames: usize,
    /// Probed output width.
    pub width: u64,
}

/// Complete ffmpeg invocation derived from validated input.
#[derive(Clone, Debug, PartialEq)]
pub struct MediaEncodeRequest {
    /// Temporary ffconcat manifest.
    pub manifest: PathBuf,
    /// Palette and scaling filter graph.
    pub filters: String,
    /// Exact requested duration.
    pub expected_duration: f64,
    /// Whether ffmpeg may replace an existing destination.
    pub force: bool,
    /// Destination GIF.
    pub output: PathBuf,
}

#[derive(Clone, Debug, PartialEq)]
struct PreparedGif {
    output: PathBuf,
    frames: Vec<PathBuf>,
    durations: Vec<f64>,
    expected_duration: f64,
}

/// Media boundary used by the deterministic validation core.
pub trait GifMediaBackend {
    /// Probe the first video stream for one path.
    ///
    /// # Errors
    ///
    /// Returns process, probe-format, or stream-shape failures.
    fn probe(&mut self, path: &Path) -> Result<Map<String, Value>>;

    /// Encode one already-materialized ffconcat manifest.
    ///
    /// # Errors
    ///
    /// Returns process-launch or nonzero ffmpeg failures.
    fn encode(&mut self, request: &MediaEncodeRequest) -> Result<()>;
}

/// Installed ffmpeg and ffprobe process backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FfmpegBackend {
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
}

impl FfmpegBackend {
    /// Resolve both required media binaries from `PATH` without installing software.
    ///
    /// # Errors
    ///
    /// Returns when `ffmpeg` or `ffprobe` is not executable through `PATH`.
    pub fn discover() -> Result<Self> {
        Ok(Self {
            ffmpeg: require_binary("ffmpeg")?,
            ffprobe: require_binary("ffprobe")?,
        })
    }

    /// Construct a backend from explicit binaries.
    #[must_use]
    pub const fn new(ffmpeg: PathBuf, ffprobe: PathBuf) -> Self {
        Self { ffmpeg, ffprobe }
    }
}

impl GifMediaBackend for FfmpegBackend {
    fn probe(&mut self, path: &Path) -> Result<Map<String, Value>> {
        let output = Command::new(&self.ffprobe)
            .args([
                OsStr::new("-v"),
                OsStr::new("error"),
                OsStr::new("-select_streams"),
                OsStr::new("v:0"),
                OsStr::new("-show_entries"),
                OsStr::new("stream=width,height,nb_frames,duration,r_frame_rate"),
                OsStr::new("-of"),
                OsStr::new("json"),
            ])
            .arg(path)
            .output()
            .with_context(|| format!("failed to run {}", self.ffprobe.display()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if !stderr.is_empty() {
                bail!(stderr);
            }
            if !stdout.is_empty() {
                bail!(stdout);
            }
            bail!(
                "media probe failed with exit code {}",
                exit_code(output.status)
            );
        }
        parse_probe_stream(&output.stdout, path)
    }

    fn encode(&mut self, request: &MediaEncodeRequest) -> Result<()> {
        let status = Command::new(&self.ffmpeg)
            .args([
                OsStr::new("-hide_banner"),
                OsStr::new("-loglevel"),
                OsStr::new("error"),
                OsStr::new("-f"),
                OsStr::new("concat"),
                OsStr::new("-safe"),
                OsStr::new("0"),
                OsStr::new("-i"),
            ])
            .arg(&request.manifest)
            .args([OsStr::new("-vf"), request.filters.as_ref()])
            .args([OsStr::new("-loop"), OsStr::new("0"), OsStr::new("-t")])
            .arg(format!("{:.6}", request.expected_duration))
            .arg(if request.force { "-y" } else { "-n" })
            .arg(&request.output)
            .status()
            .with_context(|| format!("failed to run {}", self.ffmpeg.display()))?;
        if !status.success() {
            bail!("ffmpeg failed with exit code {}", exit_code(status));
        }
        Ok(())
    }
}

/// Parse one finite positive duration.
///
/// # Errors
///
/// Returns when `value` is not numeric, finite, and greater than zero.
pub fn positive_float(value: &str) -> Result<f64> {
    let parsed = value
        .parse::<f64>()
        .map_err(|_| anyhow::anyhow!("expected a number, got {value:?}"))?;
    if !parsed.is_finite() || parsed <= 0.0 {
        bail!("expected a positive finite number, got {value:?}");
    }
    Ok(parsed)
}

/// Expand one hold duration or validate one duration per source frame.
///
/// # Errors
///
/// Returns for empty, invalid, non-positive, or count-mismatched durations.
pub fn parse_durations(value: &str, frame_count: usize) -> Result<Vec<f64>> {
    let parts = value.split(',').map(str::trim).collect::<Vec<_>>();
    if parts.is_empty() || parts.iter().any(|part| part.is_empty()) {
        bail!("--durations must be a number or a comma-separated list of numbers");
    }
    let durations = parts
        .into_iter()
        .map(positive_float)
        .collect::<Result<Vec<_>>>()?;
    if durations.len() == 1 {
        return Ok(vec![durations[0]; frame_count]);
    }
    if durations.len() != frame_count {
        bail!(
            "--durations supplied {} values for {frame_count} frames",
            durations.len()
        );
    }
    Ok(durations)
}

/// Quote one ffconcat path while preserving literal backslashes.
///
/// # Errors
///
/// Returns for non-UTF-8 paths or paths containing a newline.
pub fn ffconcat_quote(path: &Path) -> Result<String> {
    let value = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("frame path is not valid UTF-8: {}", path.display()))?;
    if value.contains(['\n', '\r']) {
        bail!("frame path contains a newline: {}", path.display());
    }
    Ok(format!("'{}'", value.replace('\'', "'\\''")))
}

/// Render an ffconcat manifest, repeating the final frame to materialize its hold.
///
/// # Errors
///
/// Returns when the frame and duration counts are empty or unequal, or a path cannot be quoted.
pub fn render_concat_manifest(frames: &[PathBuf], durations: &[f64]) -> Result<String> {
    if frames.is_empty() || frames.len() != durations.len() {
        bail!("frame and duration counts must match and be nonzero");
    }
    let mut lines = vec!["ffconcat version 1.0".to_owned()];
    for (frame, duration) in frames.iter().zip(durations) {
        lines.push(format!("file {}", ffconcat_quote(frame)?));
        lines.push(format!("duration {duration:.6}"));
    }
    lines.push(format!(
        "file {}",
        ffconcat_quote(&frames[frames.len() - 1])?
    ));
    Ok(format!("{}\n", lines.join("\n")))
}

/// Encode and verify one GIF using binaries resolved from `PATH`.
///
/// # Errors
///
/// Returns input, dependency, media-process, probe, or output-bound failures.
pub fn encode_gif(options: &EncodeGifOptions) -> Result<EncodeGifSummary> {
    let prepared = prepare(options)?;
    let mut backend = FfmpegBackend::discover()?;
    encode_prepared(options, prepared, &mut backend)
}

/// Encode and verify one GIF through an injected media boundary.
///
/// # Errors
///
/// Returns input, backend, probe, filesystem, or output-bound failures.
pub fn encode_gif_with_backend(
    options: &EncodeGifOptions,
    backend: &mut impl GifMediaBackend,
) -> Result<EncodeGifSummary> {
    let prepared = prepare(options)?;
    encode_prepared(options, prepared, backend)
}

fn prepare(options: &EncodeGifOptions) -> Result<PreparedGif> {
    let frame_dir = resolve_path(&options.frames)?;
    let output = resolve_path(&options.output)?;
    if !frame_dir.is_dir() {
        bail!("frame directory does not exist: {}", frame_dir.display());
    }
    if output
        .extension()
        .and_then(OsStr::to_str)
        .is_none_or(|extension| !extension.eq_ignore_ascii_case("gif"))
    {
        bail!("output must end in .gif: {}", output.display());
    }
    if output.exists() && !options.force {
        bail!(
            "output already exists (pass --force to replace it): {}",
            output.display()
        );
    }
    if !(4..=256).contains(&options.colors) {
        bail!("--colors must be between 4 and 256");
    }
    if options.fps > 30 {
        bail!("--fps must not exceed 30");
    }
    if options.fps == 0 {
        bail!("--fps must be a positive integer");
    }
    if options.max_width == 0 {
        bail!("--max-width must be a positive integer");
    }
    if options.max_bytes == 0 {
        bail!("--max-bytes must be a positive integer");
    }

    let frames = discover_frames(&frame_dir, &options.pattern)?;
    if frames.len() < 2 {
        bail!(
            "expected at least two frames matching {:?} in {}",
            options.pattern,
            frame_dir.display()
        );
    }
    if frames.iter().any(|frame| frame == &output) {
        bail!("output path must not match an input frame");
    }
    let durations = parse_durations(&options.durations, frames.len())?;
    let expected_duration = durations.iter().sum::<f64>();

    Ok(PreparedGif {
        output,
        frames,
        durations,
        expected_duration,
    })
}

fn encode_prepared(
    options: &EncodeGifOptions,
    prepared: PreparedGif,
    backend: &mut impl GifMediaBackend,
) -> Result<EncodeGifSummary> {
    let PreparedGif {
        output,
        frames,
        durations,
        expected_duration,
    } = prepared;

    let mut dimensions = frames
        .iter()
        .map(|frame| {
            let stream = backend.probe(frame)?;
            Ok((
                stream_positive_int(&stream, "width", frame)?,
                stream_positive_int(&stream, "height", frame)?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    dimensions.sort_unstable();
    dimensions.dedup();
    if dimensions.len() != 1 {
        bail!("all frames must have identical dimensions, got {dimensions:?}");
    }

    let parent = output
        .parent()
        .ok_or_else(|| anyhow::anyhow!("output has no parent: {}", output.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create output directory {}", parent.display()))?;
    let temporary = tempfile::Builder::new()
        .prefix("record-browser-gif-")
        .tempdir()
        .context("failed to create GIF encoding scratch directory")?;
    let manifest = temporary.path().join("frames.ffconcat");
    fs::write(&manifest, render_concat_manifest(&frames, &durations)?)
        .with_context(|| format!("failed to write {}", manifest.display()))?;
    let filters = format!(
        "fps={},scale='min({},iw)':-2:flags=lanczos,split[base][palette_input];[palette_input]palettegen=max_colors={}:stats_mode=full[palette];[base][palette]paletteuse=dither=bayer:bayer_scale=3:diff_mode=rectangle",
        options.fps, options.max_width, options.colors
    );
    backend.encode(&MediaEncodeRequest {
        manifest,
        filters,
        expected_duration,
        force: options.force,
        output: output.clone(),
    })?;

    let stream = backend.probe(&output)?;
    let width = stream_positive_int(&stream, "width", &output)?;
    let height = stream_positive_int(&stream, "height", &output)?;
    let encoded_frames = stream_positive_int(&stream, "nb_frames", &output)?;
    let actual_duration = stream_float(&stream, "duration", &output)?;
    let fps = u32::try_from(options.fps).expect("fps is validated at no more than 30");
    let tolerance = 0.2_f64.max(2.0 / f64::from(fps));
    if (actual_duration - expected_duration).abs() > tolerance {
        bail!("expected about {expected_duration:.3}s, encoded {actual_duration:.3}s");
    }
    if width > options.max_width {
        bail!(
            "expected width at most {}, encoded {width}",
            options.max_width
        );
    }
    if encoded_frames < 2 {
        bail!("expected an animated GIF, encoded {encoded_frames} frame");
    }
    let bytes = fs::metadata(&output)
        .with_context(|| format!("failed to read output metadata for {}", output.display()))?
        .len();
    if bytes > options.max_bytes {
        bail!(
            "output is {bytes} bytes, above --max-bytes {}",
            options.max_bytes
        );
    }

    Ok(EncodeGifSummary {
        bytes,
        duration_seconds: actual_duration,
        encoded_frames,
        fps: options.fps,
        height,
        output: output.to_string_lossy().into_owned(),
        source_frames: frames.len(),
        width,
    })
}

/// Render the stable, alphabetically keyed JSON summary.
///
/// # Errors
///
/// Returns when the supplied summary contains a non-JSON numeric value.
pub fn render_summary(summary: &EncodeGifSummary) -> Result<String> {
    Ok(format!(
        "{}\n",
        escape_non_ascii(&serde_json::to_string_pretty(summary)?)
    ))
}

fn escape_non_ascii(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value.chars() {
        let code = u32::from(character);
        if character.is_ascii() {
            output.push(character);
        } else if code <= 0xffff {
            use std::fmt::Write as _;
            write!(output, "\\u{code:04x}").expect("writing to String cannot fail");
        } else {
            use std::fmt::Write as _;
            let scalar = code - 0x1_0000;
            let high = 0xd800 + (scalar >> 10);
            let low = 0xdc00 + (scalar & 0x3ff);
            write!(output, "\\u{high:04x}\\u{low:04x}").expect("writing to String cannot fail");
        }
    }
    output
}

fn discover_frames(frame_dir: &Path, pattern: &str) -> Result<Vec<PathBuf>> {
    if Path::new(pattern).is_absolute() {
        bail!("frame pattern must be relative to the input directory: {pattern:?}");
    }
    let directory = frame_dir.to_str().ok_or_else(|| {
        anyhow::anyhow!(
            "frame directory is not valid UTF-8: {}",
            frame_dir.display()
        )
    })?;
    let pattern = format!(
        "{}{}{}",
        Pattern::escape(directory),
        std::path::MAIN_SEPARATOR,
        pattern
    );
    let mut frames = glob_with(
        &pattern,
        MatchOptions {
            case_sensitive: !cfg!(windows),
            require_literal_separator: true,
            require_literal_leading_dot: false,
        },
    )
    .map_err(|error| anyhow::anyhow!("invalid frame pattern: {error}"))?
    .filter_map(std::result::Result::ok)
    .filter(|path| path.is_file())
    .map(|path| resolve_path(&path))
    .collect::<Result<Vec<_>>>()?;
    frames.sort();
    Ok(frames)
}

fn resolve_path(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return fs::canonicalize(path)
            .with_context(|| format!("failed to resolve {}", path.display()));
    }
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        env::current_dir()?.join(path)
    }
    .clean();
    let ancestor = absolute
        .ancestors()
        .find(|candidate| candidate.exists())
        .ok_or_else(|| anyhow::anyhow!("path has no existing ancestor: {}", absolute.display()))?;
    let canonical = fs::canonicalize(ancestor)
        .with_context(|| format!("failed to resolve {}", ancestor.display()))?;
    Ok(canonical.join(absolute.strip_prefix(ancestor)?))
}

fn parse_probe_stream(bytes: &[u8], path: &Path) -> Result<Map<String, Value>> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| anyhow::anyhow!("media probe returned invalid JSON: {error}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("media probe returned a non-object JSON value"))?;
    let streams = object
        .get("streams")
        .and_then(Value::as_array)
        .filter(|streams| streams.len() == 1)
        .ok_or_else(|| anyhow::anyhow!("expected one video stream in {}", path.display()))?;
    streams[0]
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("expected one video stream in {}", path.display()))
}

fn stream_positive_int(stream: &Map<String, Value>, key: &str, path: &Path) -> Result<u64> {
    let value = stream.get(key).ok_or_else(|| {
        anyhow::anyhow!(
            "missing integer {key:?} in media probe for {}",
            path.display()
        )
    })?;
    let parsed = match value {
        Value::String(value) => value.parse::<i128>().ok(),
        Value::Number(value) => value
            .as_i64()
            .map(i128::from)
            .or_else(|| value.as_u64().map(i128::from))
            .or_else(|| value.as_f64().and_then(|value| value.trunc().to_i128())),
        _ => None,
    }
    .filter(|value| *value > 0)
    .and_then(|value| u64::try_from(value).ok())
    .ok_or_else(|| {
        anyhow::anyhow!(
            "missing integer {key:?} in media probe for {}",
            path.display()
        )
    })?;
    Ok(parsed)
}

fn stream_float(stream: &Map<String, Value>, key: &str, path: &Path) -> Result<f64> {
    let value = stream.get(key).and_then(|value| match value {
        Value::String(value) => value.parse::<f64>().ok(),
        Value::Number(value) => value.as_f64(),
        _ => None,
    });
    value
        .filter(|value| value.is_finite())
        .ok_or_else(|| anyhow::anyhow!("missing {key} in media probe for {}", path.display()))
}

fn require_binary(name: &str) -> Result<PathBuf> {
    let search_path = env::var_os("PATH").unwrap_or_default();
    for directory in env::split_paths(&search_path) {
        for candidate_name in executable_names(name) {
            let candidate = directory.join(candidate_name);
            if is_executable(&candidate) {
                return Ok(candidate);
            }
        }
    }
    bail!("required binary {name:?} is not available on PATH")
}

#[cfg(windows)]
fn executable_names(name: &str) -> Vec<OsString> {
    let extensions = env::var_os("PATHEXT").unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into());
    let mut names = vec![OsString::from(name)];
    names.extend(
        extensions
            .to_string_lossy()
            .split(';')
            .filter(|extension| !extension.is_empty())
            .map(|extension| OsString::from(format!("{name}{extension}"))),
    );
    names
}

#[cfg(not(windows))]
fn executable_names(name: &str) -> Vec<OsString> {
    vec![OsString::from(name)]
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;

    fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

fn exit_code(status: std::process::ExitStatus) -> String {
    if let Some(code) = status.code() {
        return code.to_string();
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;

        if let Some(signal) = status.signal() {
            return (-signal).to_string();
        }
    }
    "unknown".to_owned()
}
