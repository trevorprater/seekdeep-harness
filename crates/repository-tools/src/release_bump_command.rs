//! Release-family version planning, manifest rewrite, and commit workflow.

use std::{fmt::Write as _, path::Path};

use regex::Regex;

use crate::{
    release_bump::{compare_versions, next_vendor_version},
    release_families::{ReleaseFamily, ReleaseMember},
    release_process::{ReleaseRunOptions, capture},
};

/// One manifest version rewrite and resulting release tag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedVersion {
    /// Repository-relative manifest.
    pub manifest_path: String,
    /// Human log label.
    pub label: String,
    /// Existing version.
    pub from: String,
    /// Target version.
    pub to: String,
    /// Resulting tag, absent for workspace root.
    pub tag: Option<String>,
}

/// Release bump inputs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseBumpOptions {
    /// Family to bump.
    pub family: ReleaseFamily,
    /// Shared-family release type or explicit version.
    pub version: Option<String>,
    /// Vendor rehearsal prerelease identifier.
    pub prerelease: Option<String>,
    /// Report only.
    pub dry_run: bool,
}

/// Runs the bump with real Git and pnpm commands.
///
/// # Errors
///
/// Returns plan, file, pnpm, Git, or commit failures.
pub fn bump_release(root: &Path, options: &ReleaseBumpOptions) -> anyhow::Result<String> {
    bump_release_with(root, options, |command, args| {
        capture(
            command,
            args,
            &ReleaseRunOptions {
                cwd: Some(root.to_owned()),
                env: None,
            },
        )
    })
}

/// Runs the bump through an injected command boundary.
///
/// # Errors
///
/// Returns plan, file, command, or commit failures.
pub fn bump_release_with(
    root: &Path,
    options: &ReleaseBumpOptions,
    mut execute: impl FnMut(&str, &[String]) -> anyhow::Result<String>,
) -> anyhow::Result<String> {
    let members = options.family.members(root)?;
    options.family.verify_versions(&members)?;
    let (planned, shared_version) = match options.family {
        ReleaseFamily::SeekDeep => {
            let request = options.version.as_deref().ok_or_else(|| {
                anyhow::anyhow!("usage: release:seekdeep <major|minor|patch|x.y.z>")
            })?;
            if options.prerelease.is_some() {
                anyhow::bail!(
                    "release:seekdeep takes the prerelease in its version argument, as in 0.0.1-rc.1"
                );
            }
            let (planned, version) = plan_shared(root, options.family, &members, request)?;
            (planned, Some(version))
        }
        ReleaseFamily::Vendor => {
            if options.version.is_some() {
                anyhow::bail!(
                    "release:vendor takes no version: each package increments its own patch"
                );
            }
            if options
                .prerelease
                .as_deref()
                .is_some_and(|value| !valid_prerelease(value))
            {
                anyhow::bail!(
                    "--prerelease must be a semver prerelease identifier, got {}",
                    options.prerelease.as_deref().unwrap_or_default()
                );
            }
            (
                plan_vendor(
                    options.family,
                    &members,
                    options.prerelease.as_deref(),
                    &mut execute,
                )?,
                None,
            )
        }
    };
    if !options.dry_run {
        for entry in &planned {
            write_version(root, entry)?;
        }
        execute(
            "pnpm",
            &["install".to_owned(), "--lockfile-only".to_owned()],
        )?;
    }
    let summary = shared_version.unwrap_or_else(|| {
        planned
            .iter()
            .map(|entry| {
                format!(
                    "{} {}",
                    entry.label.strip_prefix("vendor/").unwrap_or(&entry.label),
                    entry.to
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    });
    let mut output = format!(
        "release bump: family {} -> {summary}\n",
        options.family.identifier()
    );
    for entry in &planned {
        let _ = writeln!(output, "  {}: {} -> {}", entry.label, entry.from, entry.to);
    }
    if options.dry_run {
        output.push_str("release bump: dry run, nothing written\n");
        return Ok(output);
    }
    let mut add = vec!["add".to_owned(), "pnpm-lock.yaml".to_owned()];
    add.extend(planned.iter().map(|entry| entry.manifest_path.clone()));
    execute("git", &add)?;
    execute(
        "git",
        &[
            "commit".to_owned(),
            "-m".to_owned(),
            format!("release({}): {summary}", options.family.identifier()),
        ],
    )?;
    output.push_str("release bump: committed. After this merges to master, tag it:\n");
    let mut tags = std::collections::HashSet::new();
    for tag in planned.iter().filter_map(|entry| entry.tag.as_ref()) {
        if tags.insert(tag.as_str()) {
            let _ = writeln!(
                output,
                "  git tag {tag} <merge commit> && git push origin {tag}"
            );
        }
    }
    Ok(output)
}

fn plan_shared(
    root: &Path,
    family: ReleaseFamily,
    members: &[ReleaseMember],
    request: &str,
) -> anyhow::Result<(Vec<PlannedVersion>, String)> {
    let first = members
        .first()
        .ok_or_else(|| anyhow::anyhow!("release family seekdeep has no members"))?;
    let version = next_shared_version(&first.version, request)?;
    let mut planned = vec![PlannedVersion {
        manifest_path: "package.json".to_owned(),
        label: "package.json".to_owned(),
        from: manifest_version(&root.join("package.json"))?,
        to: version.clone(),
        tag: None,
    }];
    for member in members {
        let mut target = member.clone();
        target.version.clone_from(&version);
        planned.push(PlannedVersion {
            manifest_path: format!("{}/package.json", member.directory),
            label: member.directory.clone(),
            from: member.version.clone(),
            to: version.clone(),
            tag: Some(family.tag_for(&target)),
        });
    }
    Ok((planned, version))
}

fn plan_vendor(
    family: ReleaseFamily,
    members: &[ReleaseMember],
    prerelease: Option<&str>,
    execute: &mut impl FnMut(&str, &[String]) -> anyhow::Result<String>,
) -> anyhow::Result<Vec<PlannedVersion>> {
    let mut planned = Vec::new();
    for member in members {
        let prefix = family.tag_prefix_for(member);
        let tags = execute(
            "git",
            &["tag".to_owned(), "--list".to_owned(), format!("{prefix}*")],
        )?;
        let newest = tags
            .lines()
            .map(|tag| tag.strip_prefix(&prefix).unwrap_or(tag))
            .try_fold(None::<String>, |newest, candidate| {
                Ok::<_, anyhow::Error>(match newest {
                    Some(current)
                        if compare_versions(candidate, &current)?
                            != std::cmp::Ordering::Greater =>
                    {
                        Some(current)
                    }
                    _ => Some(candidate.to_owned()),
                })
            })?;
        let to = next_vendor_version(&member.version, newest.as_deref(), prerelease)?;
        let mut target = member.clone();
        target.version.clone_from(&to);
        planned.push(PlannedVersion {
            manifest_path: format!("{}/package.json", member.directory),
            label: member.directory.clone(),
            from: member.version.clone(),
            to,
            tag: Some(family.tag_for(&target)),
        });
    }
    Ok(planned)
}

fn next_shared_version(current: &str, request: &str) -> anyhow::Result<String> {
    static VERSION: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$").expect("static shared version regex")
    });
    if !matches!(request, "major" | "minor" | "patch") {
        if !VERSION.is_match(request) {
            anyhow::bail!("usage: release:seekdeep <major|minor|patch|x.y.z>, got {request}");
        }
        return Ok(request.to_owned());
    }
    let (major, minor, patch) = release_numbers(current)?;
    Ok(match request {
        "major" => format!("{}.0.0", major + 1),
        "minor" => format!("{major}.{}.0", minor + 1),
        _ => format!("{major}.{minor}.{}", patch + 1),
    })
}

fn release_numbers(version: &str) -> anyhow::Result<(u64, u64, u64)> {
    static VERSION: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"^(\d+)\.(\d+)\.(\d+)(?:-[0-9A-Za-z.-]+)?$").expect("static version regex")
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

fn manifest_version(path: &Path) -> anyhow::Result<String> {
    let value = serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(path)?)?;
    value
        .get("version")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("package.json must declare a string version"))
}

fn write_version(root: &Path, entry: &PlannedVersion) -> anyhow::Result<()> {
    let path = root.join(&entry.manifest_path);
    let source = std::fs::read_to_string(&path)?;
    let needle = format!("\"version\": \"{}\"", entry.from);
    if !source.contains(&needle) {
        anyhow::bail!("{}: cannot locate {needle}", entry.manifest_path);
    }
    std::fs::write(
        path,
        source.replacen(&needle, &format!("\"version\": \"{}\"", entry.to), 1),
    )?;
    Ok(())
}

fn valid_prerelease(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
}
