//! Pure release version precedence and payload-change judgments.

use regex::Regex;

use crate::{release_families::ReleaseMember, repo_files::repository_glob_matches};

/// Orders two versions by `SemVer` precedence.
///
/// # Errors
///
/// Returns malformed release-number diagnostics.
pub fn compare_versions(left: &str, right: &str) -> anyhow::Result<std::cmp::Ordering> {
    let numbers = release_numbers(left)?.cmp(&release_numbers(right)?);
    if numbers != std::cmp::Ordering::Equal {
        return Ok(numbers);
    }
    let left_pre = prerelease(left);
    let right_pre = prerelease(right);
    match (left_pre, right_pre) {
        (None, None) => return Ok(std::cmp::Ordering::Equal),
        (None, Some(_)) => return Ok(std::cmp::Ordering::Greater),
        (Some(_), None) => return Ok(std::cmp::Ordering::Less),
        (Some(_), Some(_)) => {}
    }
    let left_fields = left_pre.unwrap_or_default().split('.').collect::<Vec<_>>();
    let right_fields = right_pre.unwrap_or_default().split('.').collect::<Vec<_>>();
    for index in 0..left_fields.len().max(right_fields.len()) {
        let (Some(left), Some(right)) = (left_fields.get(index), right_fields.get(index)) else {
            return Ok(left_fields.len().cmp(&right_fields.len()));
        };
        if left == right {
            continue;
        }
        let left_number = numeric_field(left);
        let right_number = numeric_field(right);
        return Ok(match (left_number, right_number) {
            (Some(left), Some(right)) => left.cmp(&right),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => left.cmp(right),
        });
    }
    Ok(std::cmp::Ordering::Equal)
}

/// Computes the next vendor version against manifest and latest-tag baselines.
///
/// # Errors
///
/// Returns malformed version diagnostics.
pub fn next_vendor_version(
    current: &str,
    tagged: Option<&str>,
    prerelease_name: Option<&str>,
) -> anyhow::Result<String> {
    let tagged_order = tagged
        .map(|tagged| compare_release_numbers(tagged, current))
        .transpose()?;
    let ahead = tagged_order.is_some_and(|ordering| ordering == std::cmp::Ordering::Greater);
    let baseline = if ahead {
        tagged.unwrap_or(current)
    } else {
        current
    };
    let (major, minor, patch) = release_numbers(baseline)?;
    let tagged_prerelease = tagged.and_then(prerelease).is_some();
    let same_release_prereleases =
        tagged_order == Some(std::cmp::Ordering::Equal) && prerelease(current).is_some();
    let reuse = tagged_prerelease && (ahead || same_release_prereleases);
    let patch = if reuse { patch } else { patch + 1 };
    let version = format!("{major}.{minor}.{patch}");
    Ok(prerelease_name.map_or(version.clone(), |name| format!("{version}-{name}")))
}

/// Whether a repository path reaches one member's packed payload.
#[must_use]
pub fn reaches_payload(member: &ReleaseMember, path: &str) -> bool {
    let relative = path
        .strip_prefix(&member.directory)
        .and_then(|path| path.strip_prefix('/'))
        .unwrap_or(path);
    let selected = member
        .manifest
        .get("files")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .collect::<Vec<_>>();
    let built = selected.iter().any(|pattern| pattern.starts_with("lib"));
    let mut patterns = vec!["package.json", "README*", "LICENSE*", "LICENCE*"];
    patterns.extend(selected);
    if built {
        patterns.extend([
            "src/**",
            "tsconfig*.json",
            "tsdown.config.*",
            "build.config.*",
        ]);
    }
    patterns.into_iter().any(|pattern| {
        repository_glob_matches(pattern, relative)
            || repository_glob_matches(&format!("{pattern}/**"), relative)
            || relative == pattern
    })
}

fn release_numbers(version: &str) -> anyhow::Result<(u64, u64, u64)> {
    static VERSION: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"^(\d+)\.(\d+)\.(\d+)(?:-[0-9A-Za-z.-]+)?$")
            .expect("static release-number regex")
    });
    let captures = VERSION
        .captures(version)
        .ok_or_else(|| anyhow::anyhow!("cannot read release numbers from version {version}"))?;
    Ok((
        captures[1].parse()?,
        captures[2].parse()?,
        captures[3].parse()?,
    ))
}

fn compare_release_numbers(left: &str, right: &str) -> anyhow::Result<std::cmp::Ordering> {
    Ok(release_numbers(left)?.cmp(&release_numbers(right)?))
}

fn prerelease(version: &str) -> Option<&str> {
    version.split_once('-').map(|(_, prerelease)| prerelease)
}

fn numeric_field(field: &str) -> Option<u64> {
    (!field.is_empty() && field.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| field.parse().ok())
        .flatten()
}
