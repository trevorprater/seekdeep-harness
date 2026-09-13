//! Release-family version, publishability, and workflow-tag verification.

use std::path::Path;

use crate::release_families::{ReleaseFamily, ReleaseMember};

/// Verifies one release family and renders its source-compatible summary.
///
/// # Errors
///
/// Returns discovery, version, private-package, ref, family-tag, or member-tag
/// diagnostics.
pub fn verify_release(
    root: &Path,
    family: ReleaseFamily,
    publishing: bool,
    github_ref: &str,
) -> anyhow::Result<String> {
    let members = family.members(root)?;
    family.verify_versions(&members)?;
    if publishing {
        verify_publishable(&members)?;
        verify_tag(family, &members, github_ref)?;
    }
    let mut versions = Vec::new();
    for member in &members {
        if !versions.contains(&member.version) {
            versions.push(member.version.clone());
        }
    }
    let summary = if versions.len() == 1 {
        versions[0].clone()
    } else {
        format!("{} versions", versions.len())
    };
    Ok(format!(
        "release verify: family {}, {} member(s), {summary}{}\n",
        family.identifier(),
        members.len(),
        if publishing {
            ", publish gates passed"
        } else {
            ""
        }
    ))
}

fn verify_publishable(members: &[ReleaseMember]) -> anyhow::Result<()> {
    let private = members
        .iter()
        .filter(|member| {
            member
                .manifest
                .get("private")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
        })
        .map(|member| member.directory.clone())
        .collect::<Vec<_>>();
    if !private.is_empty() {
        anyhow::bail!(
            "publishing requires removing \"private\": true from:\n{}",
            private.join("\n")
        );
    }
    Ok(())
}

fn verify_tag(
    family: ReleaseFamily,
    members: &[ReleaseMember],
    github_ref: &str,
) -> anyhow::Result<()> {
    let Some(tag) = github_ref.strip_prefix("refs/tags/") else {
        anyhow::bail!(
            "publishing release family {} requires running from a {}* tag, got {}",
            family.identifier(),
            family_tag_prefix(family),
            if github_ref.is_empty() {
                "(no ref)"
            } else {
                github_ref
            }
        );
    };
    let prefix = family_tag_prefix(family);
    if !tag.starts_with(prefix) {
        anyhow::bail!(
            "tag {tag} does not belong to release family {} (expected {prefix}*)",
            family.identifier()
        );
    }
    let expected = members
        .iter()
        .map(|member| family.tag_for(member))
        .collect::<Vec<_>>();
    if !expected.iter().any(|candidate| candidate == tag) {
        let mut unique = Vec::new();
        for tag in expected {
            if !unique.contains(&tag) {
                unique.push(tag);
            }
        }
        anyhow::bail!(
            "tag {tag} names no version this family carries; its members would tag as:\n{}",
            unique.join("\n")
        );
    }
    Ok(())
}

fn family_tag_prefix(family: ReleaseFamily) -> &'static str {
    match family {
        ReleaseFamily::SeekDeep => "seekdeep-v",
        ReleaseFamily::Vendor => "vendor-",
    }
}
