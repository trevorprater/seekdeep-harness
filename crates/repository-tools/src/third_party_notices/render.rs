use std::path::Path;

use anyhow::bail;

use super::{
    CLAUDE_AGENT_SDK_PACKAGE, CLAUDE_PLATFORM_DECLARED_LICENSE, ClaudeDistribution, ExternalDep,
    NoticeCollection, collect, is_owner_authorized_runtime, is_permissive,
};

fn npm_table(dependencies: &[&ExternalDep]) -> String {
    let mut lines = vec![
        "| Package | License |".to_owned(),
        "| --- | --- |".to_owned(),
    ];
    lines.extend(dependencies.iter().map(|dependency| {
        format!(
            "| [`{}`]({}) | {} |",
            dependency.name, dependency.repo, dependency.license
        )
    }));
    lines.join("\n")
}

fn non_permissive_note(dependencies: &[&ExternalDep]) -> String {
    if dependencies.is_empty() {
        return String::new();
    }
    let named = dependencies
        .iter()
        .map(|dependency| format!("`{}` ({})", dependency.name, dependency.license))
        .collect::<Vec<_>>();
    let subject = if named.len() == 1 {
        named[0].clone()
    } else {
        format!(
            "{} and {}",
            named[..named.len() - 1].join(", "),
            named[named.len() - 1]
        )
    };
    format!(
        "\n{subject} {} only as development tooling; their code is not linked into or distributed with any SeekDeep Harness artifact.\n",
        if named.len() == 1 { "runs" } else { "run" }
    )
}

fn claude_distribution(distribution: Option<&ClaudeDistribution>) -> String {
    let Some(distribution) = distribution else {
        return String::new();
    };
    let rows = distribution.payloads.iter().map(|payload| format!("| [`{}`](https://www.npmjs.com/package/{}) | {} | {CLAUDE_PLATFORM_DECLARED_LICENSE} |", payload.name, payload.name, payload.version)).collect::<Vec<_>>().join("\n");
    format!(
        r"
## Official Claude Code platform payloads

The project owner authorizes distribution of every version of the official `{CLAUDE_AGENT_SDK_PACKAGE}` package and the official Claude Code CLI/platform payloads that each version declares through `optionalDependencies`. This identity-scoped authorization does not classify their declared terms as permissive and does not cover any unrelated runtime package; version, declared-license, and payload-set changes still require the ordinary dependency, lockfile, compatibility, terms, and notices review.

The installed SDK {} declares the following optional platform packages. Each carries the official Claude Code {} executable; the package identities and versions come from the SDK manifest, while the declared license field is verified against the platform payload installed for the current host.

| Optional platform package | Version | Declared license |
| --- | --- | --- |
{rows}
",
        distribution.sdk_version, distribution.claude_code_version
    )
}

/// Renders validated discovery results and enforces runtime license policy.
///
/// # Errors
/// Rejects any non-permissive runtime dependency outside the exact owner authorization.
pub fn render_collection(collection: &NoticeCollection) -> anyhow::Result<String> {
    let runtime = collection
        .npm
        .iter()
        .filter(|dependency| dependency.runtime)
        .collect::<Vec<_>>();
    let development = collection
        .npm
        .iter()
        .filter(|dependency| !dependency.runtime)
        .collect::<Vec<_>>();
    let non_permissive_development = development
        .iter()
        .copied()
        .filter(|dependency| !is_permissive(&dependency.license))
        .collect::<Vec<_>>();
    let non_permissive_runtime = runtime
        .iter()
        .filter(|dependency| {
            !is_permissive(&dependency.license) && !is_owner_authorized_runtime(&dependency.name)
        })
        .map(|dependency| format!("{} ({})", dependency.name, dependency.license))
        .collect::<Vec<_>>();
    if !non_permissive_runtime.is_empty() {
        bail!(
            "gen-third-party-notices: runtime {} is not a permissive license; review the distribution terms and record the decision before regenerating.",
            non_permissive_runtime.join(", ")
        );
    }
    let substitutions = [
        (
            "VENDORED_ROWS",
            collection
                .vendored
                .iter()
                .map(|row| {
                    format!(
                        "| `{}` | `{}` | [{}]({}) | MIT |",
                        row.npm_name,
                        row.upstream_name,
                        row.upstream.replacen("https://", "", 1),
                        row.upstream
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        ("RUNTIME_TABLE", npm_table(&runtime)),
        (
            "PATCHED_ROWS",
            collection
                .patched
                .iter()
                .map(|patch| format!("- `{}` — [`{}`]({})", patch.spec, patch.patch, patch.patch))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        (
            "CLAUDE_DISTRIBUTION",
            claude_distribution(collection.claude_distribution.as_ref()),
        ),
        ("DEVELOPMENT_TABLE", npm_table(&development)),
        (
            "NONPERMISSIVE_DEVELOPMENT",
            non_permissive_note(&non_permissive_development),
        ),
        (
            "PYTHON_ROWS",
            collection
                .python
                .iter()
                .map(|dependency| {
                    format!(
                        "| [`{}`]({}) | {} | {} |",
                        dependency.name, dependency.repo, dependency.license, dependency.role
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        (
            "BUILD_TOOL_ROWS",
            collection
                .build_time_tools
                .iter()
                .map(|tool| {
                    format!(
                        "| [`{}`]({}) | {} | {} |",
                        tool.name, tool.repo, tool.license, tool.role
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"),
        ),
    ];
    fill_template(&substitutions)
}

fn fill_template(substitutions: &[(&str, String)]) -> anyhow::Result<String> {
    let placeholders = regex::Regex::new(r"\{\{([A-Z_]+)\}\}")?;
    Ok(placeholders
        .replace_all(
            include_str!("document.txt"),
            |capture: &regex::Captures<'_>| {
                substitutions
                    .iter()
                    .find(|(name, _)| *name == &capture[1])
                    .map_or_else(|| capture[0].to_owned(), |(_, content)| content.clone())
            },
        )
        .into_owned())
}

/// Discovers the current workspace and renders its exact notices bytes.
///
/// # Errors
/// Returns dependency-discovery, metadata, and runtime-license policy failures.
pub fn render(root: &Path) -> anyhow::Result<String> {
    render_collection(&collect(root)?)
}
