//! Browser boot-graph wire parsing and normalized consumer views.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Client package identity used by the graph, module table, and Loader.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ClientModuleId(String);

impl ClientModuleId {
    /// Wraps the source-compatible string identity.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrowed wire representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ClientModuleId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// One Host-composed browser entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebBootEntry {
    /// Package and module-table identity.
    pub id: ClientModuleId,
    /// Same-origin bundle endpoint.
    pub url: String,
    /// Bundle content hash.
    pub rev: String,
    /// Informational package dependency edges.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inject: Option<Vec<String>>,
    /// Stage-one prefetch marker.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub immediately: Option<bool>,
}

/// Host-composed Client entry graph.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebBootGraph {
    /// Whole-graph consistency anchor.
    pub rev: String,
    /// Composed rows in source order.
    pub entries: Vec<WebBootEntry>,
}

/// Module-table view of one boot row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BootModuleRow {
    /// Module identity.
    pub id: ClientModuleId,
    /// Bundle endpoint.
    pub url: String,
    /// Bundle content hash.
    pub rev: String,
}

/// Cordis-plugin view of one boot row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BootPluginRow {
    /// Module identity.
    pub id: ClientModuleId,
    /// Normalized dependency edges.
    pub inject: Vec<String>,
    /// Normalized prefetch marker.
    pub immediately: bool,
}

/// One parsed graph projected for its two consumers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BootManifest {
    /// Whole-graph revision.
    pub rev: String,
    /// Module-table rows.
    pub modules: Vec<BootModuleRow>,
    /// Cordis plugin rows.
    pub plugins: Vec<BootPluginRow>,
}

/// Parses the raw `window.__SEEKDEEP_BOOT__` value.
///
/// # Errors
///
/// Returns the source's field-specific diagnostic at the first malformed
/// boundary.
pub fn parse_boot_manifest(wire: &Value) -> anyhow::Result<BootManifest> {
    let graph = wire.as_object().ok_or_else(|| {
        anyhow::anyhow!("client-modules: window.__SEEKDEEP_BOOT__ is missing or not an object")
    })?;
    let rev = graph
        .get("rev")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("client-modules: boot manifest rev must be a string"))?
        .to_owned();
    let entries = graph
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("client-modules: boot manifest entries must be an array"))?;
    let mut modules = Vec::with_capacity(entries.len());
    let mut plugins = Vec::with_capacity(entries.len());
    for value in entries {
        let row = value.as_object().ok_or_else(|| {
            anyhow::anyhow!("client-modules: boot manifest entry is not an object")
        })?;
        let where_ = row
            .get("id")
            .and_then(Value::as_str)
            .map_or_else(|| value.to_string(), |id| format!("{id:?}"));
        let id = row.get("id").and_then(Value::as_str);
        let url = row.get("url").and_then(Value::as_str);
        let row_rev = row.get("rev").and_then(Value::as_str);
        let (Some(id), Some(url), Some(row_rev)) = (id, url, row_rev) else {
            anyhow::bail!(
                "client-modules: boot manifest entry {where_} must carry string id/url/rev"
            );
        };
        let inject = match row.get("inject") {
            None => Vec::new(),
            Some(value) => value
                .as_array()
                .and_then(|values| {
                    values
                        .iter()
                        .map(|value| value.as_str().map(str::to_owned))
                        .collect::<Option<Vec<_>>>()
                })
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "client-modules: boot manifest entry {where_} inject must be a string array"
                    )
                })?,
        };
        let immediately = match row.get("immediately") {
            None => false,
            Some(Value::Bool(value)) => *value,
            Some(_) => anyhow::bail!(
                "client-modules: boot manifest entry {where_} immediately must be a boolean"
            ),
        };
        let id = ClientModuleId::new(id);
        modules.push(BootModuleRow {
            id: id.clone(),
            url: url.to_owned(),
            rev: row_rev.to_owned(),
        });
        plugins.push(BootPluginRow {
            id,
            inject,
            immediately,
        });
    }
    Ok(BootManifest {
        rev,
        modules,
        plugins,
    })
}
