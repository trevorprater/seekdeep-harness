//! Source-map VLQ attribution for built browser chunks.

use std::{fmt::Write as _, sync::LazyLock};

use anyhow::{Context as _, anyhow, bail};
use indexmap::IndexMap;
use regex::Regex;
use serde::Deserialize;

static NODE_MODULE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"node_modules/(@[^/]+/[^/]+|[^@./][^/]*)/").expect("static regex")
});
static PACKAGE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"packages/([^/]+/[^/]+)/").expect("static regex"));
static VENDOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"vendor/([^/]+)/").expect("static regex"));

/// Minimal source-map input consumed by the audit.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct SourceMap {
    /// Source paths indexed by decoded segments.
    pub sources: Vec<String>,
    /// Semicolon-separated generated-line mappings.
    pub mappings: String,
}

/// Attributed generated character units for one source bucket.
#[derive(Clone, Debug, PartialEq)]
pub struct BucketBytes {
    /// npm package, workspace path, or glue label.
    pub name: String,
    /// Source-compatible JavaScript UTF-16 units attributed to the bucket.
    pub bytes: f64,
}

/// Complete attribution result.
#[derive(Clone, Debug, PartialEq)]
pub struct Attribution {
    /// JavaScript UTF-16 length of the minified chunk.
    pub total: f64,
    /// Buckets in descending attributed-size order.
    pub rows: Vec<BucketBytes>,
}

/// Attributes one chunk string through its parsed source map.
///
/// # Errors
///
/// Returns malformed VLQ data or impossible source-index arithmetic.
#[allow(clippy::cast_precision_loss)] // The oracle accumulates every count in JavaScript Number.
pub fn attribute_chunk(code: &str, map: &SourceMap) -> anyhow::Result<Attribution> {
    let code_lines = code.split('\n').collect::<Vec<_>>();
    let mut by_source = vec![0.0_f64; map.sources.len()];
    let mut unmapped = 0.0_f64;
    let mut source_index = 0_i64;
    let mut source_line = 0_i64;
    let mut source_column = 0_i64;
    let mut name_index = 0_i64;

    for (line_index, encoded_line) in map.mappings.split(';').enumerate() {
        let line_len = code_lines.get(line_index).map_or(0_i64, |line| {
            i64::try_from(line.encode_utf16().count() + 1).unwrap_or(i64::MAX)
        });
        if encoded_line.is_empty() {
            unmapped += line_len as f64;
            continue;
        }
        let mut generated_column = 0_i64;
        let mut segments = Vec::<(i64, i64)>::new();
        for encoded_segment in encoded_line.split(',') {
            let fields = decode_vlq(encoded_segment)?;
            let Some(generated_delta) = fields.first() else {
                bail!("source-map segment {encoded_segment:?} decoded no fields");
            };
            generated_column = generated_column
                .checked_add(*generated_delta)
                .ok_or_else(|| anyhow!("generated column overflow"))?;
            if fields.len() > 1 {
                if fields.len() < 4 {
                    bail!(
                        "mapped source-map segment {encoded_segment:?} has {} fields",
                        fields.len()
                    );
                }
                source_index = checked_delta(source_index, fields[1], "source index")?;
                source_line = checked_delta(source_line, fields[2], "source line")?;
                source_column = checked_delta(source_column, fields[3], "source column")?;
                if fields.len() > 4 {
                    name_index = checked_delta(name_index, fields[4], "name index")?;
                }
                segments.push((generated_column, source_index));
            } else {
                segments.push((generated_column, -1));
            }
        }
        if segments[0].0 > 0 {
            unmapped += segments[0].0 as f64;
        }
        for (index, (start, source)) in segments.iter().copied().enumerate() {
            let end = segments
                .get(index + 1)
                .map_or(line_len, |segment| segment.0);
            let span = (end - start).max(0) as f64;
            if source >= 0 {
                if let Some(slot) = usize::try_from(source)
                    .ok()
                    .and_then(|source| by_source.get_mut(source))
                {
                    *slot += span;
                }
            } else {
                unmapped += span;
            }
        }
    }
    let mut buckets = IndexMap::<String, f64>::new();
    for (source, bytes) in map.sources.iter().zip(by_source) {
        if bytes == 0.0 {
            continue;
        }
        *buckets.entry(bucket_of(source)).or_default() += bytes;
    }
    buckets.insert("(unmapped: interop glue/helpers)".to_owned(), unmapped);
    let mut rows = buckets
        .into_iter()
        .map(|(name, bytes)| BucketBytes { name, bytes })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| right.bytes.total_cmp(&left.bytes));
    Ok(Attribution {
        total: code.encode_utf16().count() as f64,
        rows,
    })
}

/// Renders the exact source command-line report.
#[must_use]
#[allow(clippy::cast_precision_loss)] // The oracle accumulates every count in JavaScript Number.
pub fn render_report(chunk_path: &str, attribution: &Attribution, top: f64) -> String {
    let mut output = String::new();
    writeln!(
        output,
        "chunk: {chunk_path}  total {} kB (minified, pre-gzip)",
        javascript_fixed_1(attribution.total / 1024.0)
    )
    .expect("String writes do not fail");
    writeln!(output, "{:>8} {:>5}  package", "kB", "%").expect("String writes do not fail");
    let mut shown = 0.0_f64;
    let mut accounted = 0.0_f64;
    for row in &attribution.rows {
        if shown < top {
            writeln!(
                output,
                "{:>8} {:>5}  {}",
                javascript_fixed_1(row.bytes / 1024.0),
                javascript_fixed_1(row.bytes / attribution.total * 100.0),
                row.name
            )
            .expect("String writes do not fail");
        }
        shown += 1.0;
        accounted += row.bytes;
    }
    if attribution.rows.len() as f64 > top {
        writeln!(
            output,
            "   ... {} more buckets",
            javascript_number(attribution.rows.len() as f64 - top)
        )
        .expect("String writes do not fail");
    }
    writeln!(
        output,
        "accounted: {} kB of {} kB",
        javascript_fixed_1(accounted / 1024.0),
        javascript_fixed_1(attribution.total / 1024.0)
    )
    .expect("String writes do not fail");
    let mut vendor = 0.0_f64;
    let mut workspace = 0.0_f64;
    let mut glue = 0.0_f64;
    for row in &attribution.rows {
        if row.name.starts_with("ws:") {
            workspace += row.bytes;
        } else if row.name.starts_with('(') {
            glue += row.bytes;
        } else {
            vendor += row.bytes;
        }
    }
    writeln!(
        output,
        "\nGROUPS  npm-vendor {} kB | workspace {} kB | glue {} kB",
        javascript_fixed_1(vendor / 1024.0),
        javascript_fixed_1(workspace / 1024.0),
        javascript_fixed_1(glue / 1024.0)
    )
    .expect("String writes do not fail");
    output
}

/// Parses and attributes one chunk plus its adjacent `.map` file.
///
/// # Errors
///
/// Returns file, JSON, or source-map decoding failures.
pub fn run(chunk_path: &str, top: f64) -> anyhow::Result<String> {
    let code = String::from_utf8_lossy(
        &std::fs::read(chunk_path).with_context(|| format!("read chunk {chunk_path:?}"))?,
    )
    .into_owned();
    let map_path = format!("{chunk_path}.map");
    let map: SourceMap = serde_json::from_slice(
        &std::fs::read(&map_path).with_context(|| format!("read source map {map_path:?}"))?,
    )
    .with_context(|| format!("parse source map {map_path:?}"))?;
    Ok(render_report(
        chunk_path,
        &attribute_chunk(&code, &map)?,
        top,
    ))
}

fn decode_vlq(segment: &str) -> anyhow::Result<Vec<i64>> {
    const ALPHABET: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut fields = Vec::new();
    let mut shift = 0_u32;
    let mut value = 0_u64;
    for character in segment.chars() {
        let digit = ALPHABET
            .find(character)
            .ok_or_else(|| anyhow!("invalid base64 VLQ character {character:?}"))?
            as u64;
        value |= (digit & 31) << shift;
        if digit & 32 != 0 {
            shift = shift
                .checked_add(5)
                .ok_or_else(|| anyhow!("VLQ shift overflow"))?;
        } else {
            let magnitude = i64::try_from(value >> 1).context("VLQ magnitude exceeds i64")?;
            fields.push(if value & 1 == 1 {
                -magnitude
            } else {
                magnitude
            });
            shift = 0;
            value = 0;
        }
    }
    if shift != 0 {
        bail!("unterminated base64 VLQ segment {segment:?}");
    }
    Ok(fields)
}

fn checked_delta(value: i64, delta: i64, label: &str) -> anyhow::Result<i64> {
    value
        .checked_add(delta)
        .ok_or_else(|| anyhow!("{label} overflow"))
}

fn bucket_of(source: &str) -> String {
    if source.starts_with('\0') || source.contains("vite/") {
        return "(vite virtual/helpers)".to_owned();
    }
    if let Some(package) = NODE_MODULE
        .captures_iter(source)
        .last()
        .and_then(|capture| capture.get(1))
    {
        return package.as_str().to_owned();
    }
    if let Some(package) = PACKAGE.captures(source).and_then(|capture| capture.get(1)) {
        return format!("ws:packages/{}", package.as_str());
    }
    if let Some(vendor) = VENDOR.captures(source).and_then(|capture| capture.get(1)) {
        return format!("ws:vendor/{}", vendor.as_str());
    }
    if source.contains("apps/web/") {
        return "ws:apps/web".to_owned();
    }
    source.to_owned()
}

fn javascript_number(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_owned()
    } else if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        value.to_string()
    }
}

fn javascript_fixed_1(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_owned()
    } else if value == f64::INFINITY {
        "Infinity".to_owned()
    } else if value == f64::NEG_INFINITY {
        "-Infinity".to_owned()
    } else {
        format!("{:.1}", (value * 10.0).round() / 10.0)
    }
}
