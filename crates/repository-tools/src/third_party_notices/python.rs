use std::{collections::HashSet, sync::OnceLock};

use anyhow::{Context as _, bail};
use indexmap::IndexMap;
use regex::Regex;

use super::{JS_SPACE_BODY, PythonDependency, PythonMetadata, js_entries, locale_compare, policy};

fn table<'a>(
    value: Option<&'a toml::Value>,
    location: &str,
) -> anyhow::Result<Option<&'a toml::Table>> {
    value
        .map(|value| {
            value
                .as_table()
                .with_context(|| format!("gen-third-party-notices: {location} must be a table."))
        })
        .transpose()
}

fn requirement_name(requirement: &str) -> anyhow::Result<String> {
    static NAME: OnceLock<Regex> = OnceLock::new();
    let pattern = NAME.get_or_init(|| Regex::new(&r"^\s*([a-zA-Z][a-zA-Z0-9._-]*)\s*(?:\[[^\]]*\])?\s*(?:[<>=!~;@][^\r\n\u{2028}\u{2029}]*)?$".replace(r"\s", &format!("[{JS_SPACE_BODY}]"))).expect("static Python requirement pattern"));
    pattern
        .captures(requirement)
        .map(|capture| capture[1].to_owned())
        .with_context(|| {
            format!(
                "gen-third-party-notices: cannot read a distribution name from the requirement {}.",
                serde_json::to_string(requirement).expect("string serialization")
            )
        })
}

fn collect_array(
    names: &mut Vec<String>,
    value: Option<&toml::Value>,
    location: &str,
    includes: bool,
) -> anyhow::Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    let values = value
        .as_array()
        .with_context(|| format!("gen-third-party-notices: {location} must be an array."))?;
    for value in values {
        if let Some(requirement) = value.as_str() {
            names.push(requirement_name(requirement)?);
            continue;
        }
        if includes
            && value.as_table().is_some_and(|table| {
                table.len() == 1 && table.get("include-group").is_some_and(toml::Value::is_str)
            })
        {
            continue;
        }
        bail!("gen-third-party-notices: {location} contains an unsupported requirement entry.");
    }
    Ok(())
}

fn parse_pyproject(text: &str) -> anyhow::Result<(Option<String>, Vec<String>)> {
    let document = text.parse::<toml::Table>()?;
    let build_system = table(document.get("build-system"), "[build-system]")?;
    let project = table(document.get("project"), "[project]")?;
    let project_name = project.and_then(|project| project.get("name"));
    if project_name.is_some_and(|value| !value.is_str()) {
        bail!("gen-third-party-notices: [project].name must be a string.");
    }
    let mut names = Vec::new();
    collect_array(
        &mut names,
        build_system.and_then(|table| table.get("requires")),
        "[build-system].requires",
        false,
    )?;
    collect_array(
        &mut names,
        project.and_then(|table| table.get("dependencies")),
        "[project].dependencies",
        false,
    )?;
    if let Some(optional) = table(
        project.and_then(|table| table.get("optional-dependencies")),
        "[project.optional-dependencies]",
    )? {
        for (group, requirements) in
            js_entries(optional.iter().map(|(name, value)| (name.as_str(), value)))
        {
            collect_array(
                &mut names,
                Some(requirements),
                &format!("[project.optional-dependencies].{group}"),
                false,
            )?;
        }
    }
    if let Some(groups) = table(document.get("dependency-groups"), "[dependency-groups]")? {
        for (group, requirements) in
            js_entries(groups.iter().map(|(name, value)| (name.as_str(), value)))
        {
            collect_array(
                &mut names,
                Some(requirements),
                &format!("[dependency-groups].{group}"),
                true,
            )?;
        }
    }
    Ok((
        project_name
            .and_then(toml::Value::as_str)
            .map(str::to_owned),
        names,
    ))
}

/// Reads all declared PEP 508 requirement names, including optional and named groups.
///
/// # Errors
/// Returns TOML parse, unsupported-shape, and malformed-requirement errors.
pub fn parse_pyproject_requirements(text: &str) -> anyhow::Result<Vec<String>> {
    parse_pyproject(text).map(|(_, requirements)| requirements)
}

fn normalize(name: &str) -> String {
    static SEPARATORS: OnceLock<Regex> = OnceLock::new();
    SEPARATORS
        .get_or_init(|| Regex::new("[-_.]+").expect("static Python name separators"))
        .replace_all(&name.to_lowercase(), "-")
        .into_owned()
}

/// Resolves external Python requirements after excluding normalized local identities.
///
/// # Errors
/// Returns TOML/requirement failures and missing disclosure metadata.
pub fn collect_python_dependencies(
    pyprojects: &[String],
    metadata: Option<&IndexMap<String, PythonMetadata>>,
) -> anyhow::Result<Vec<PythonDependency>> {
    let parsed = pyprojects
        .iter()
        .map(|text| parse_pyproject(text))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let first_party = parsed
        .iter()
        .filter_map(|(name, _)| name.as_ref())
        .map(|name| normalize(name))
        .collect::<HashSet<_>>();
    let mut names = parsed
        .into_iter()
        .flat_map(|(_, requirements)| requirements)
        .map(|name| normalize(&name))
        .filter(|name| !first_party.contains(name))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    names.sort_by(|a, b| locale_compare(a, b));
    let metadata = metadata.unwrap_or(&policy().python_metadata);
    names.into_iter().map(|name| {
        let entry = metadata.get(&name).with_context(|| format!("gen-third-party-notices: python dependency {name} is missing from PYTHON_METADATA."))?;
        Ok(PythonDependency { name, license: entry.license.clone(), repo: entry.repo.clone(), role: entry.role.clone() })
    }).collect()
}
