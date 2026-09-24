//! Canonical publication metadata and source-preserving documentation projection.

mod config;
mod prepare;
mod projection;
mod rewrite;

use std::{collections::BTreeMap, path::Path};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use config::site_configuration;
pub use prepare::prepare_site;
pub use projection::{ProjectionReport, docs_source_files, project_docs, publishable_image};
pub(crate) use rewrite::external;
pub use rewrite::{
    ImagePlacer, RewriteOptions, add_projection_frontmatter, map_markdown_destinations,
    projected_page_content, rewrite_markdown,
};

/// Public documentation route tree.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DocsLocale {
    /// Chinese route tree at the site root.
    Root,
    /// English route tree below `/en/`.
    En,
}

impl DocsLocale {
    pub(super) const fn name(self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::En => "en",
        }
    }
    pub(super) const fn other(self) -> Self {
        match self {
            Self::Root => Self::En,
            Self::En => Self::Root,
        }
    }
}

/// Language of a canonical document in the publication manifest.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum DocsContentLocale {
    /// Simplified Chinese.
    #[serde(rename = "zh-CN")]
    ZhCn,
    /// English.
    #[serde(rename = "en-US")]
    EnUs,
}

/// Closed collection names shared by the manifest and site navigation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DocsSidebar {
    /// Chinese product guides.
    ZhGuide,
    /// Chinese development tutorials.
    ZhDevelop,
    /// Chinese reference pages.
    ZhReference,
    /// English product guides.
    EnGuide,
    /// English development tutorials.
    EnDevelop,
    /// English reference pages.
    EnReference,
}

impl DocsSidebar {
    const fn name(self) -> &'static str {
        match self {
            Self::ZhGuide => "zh-guide",
            Self::ZhDevelop => "zh-develop",
            Self::ZhReference => "zh-reference",
            Self::EnGuide => "en-guide",
            Self::EnDevelop => "en-develop",
            Self::EnReference => "en-reference",
        }
    }
}

fn required_sidebar<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<DocsSidebar>, D::Error> {
    Option::deserialize(deserializer)
}

/// One canonical Markdown page exposed by the website.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DocsPage {
    /// Route tree receiving this page.
    pub locale: DocsLocale,
    /// Language of the source content.
    pub content_locale: DocsContentLocale,
    /// Repository-relative canonical Markdown path.
    pub source: String,
    /// Public route with its `.md` suffix.
    pub route: String,
    /// Sidebar label.
    pub label: String,
    /// Sidebar collection, absent for a locale home page.
    #[serde(deserialize_with = "required_sidebar")]
    pub sidebar: Option<DocsSidebar>,
    /// Sidebar group name.
    pub section: String,
    /// Position inside the group.
    pub order: f64,
    /// Optional heading-depth configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outline: Option<Value>,
    /// Additional repository paths resolving to this page.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_aliases: Vec<String>,
}

/// Ordered sidebar section and optional collapsed state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocsSection {
    /// Group label shared with its pages.
    pub label: String,
    /// Whether a collapsible group starts closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collapsed: Option<bool>,
}

/// Publication allowlist and per-locale section ordering.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DocsManifest {
    /// Pinned oracle used to establish the original manifest.
    pub source_commit: String,
    /// Every published locale route.
    pub pages: Vec<DocsPage>,
    /// Sidebar sections in display order.
    pub sections: BTreeMap<DocsLocale, Vec<DocsSection>>,
}

impl DocsManifest {
    /// Reads a publication manifest from disk.
    ///
    /// # Errors
    /// Returns file or schema errors.
    pub fn read(path: &Path) -> anyhow::Result<Self> {
        Ok(serde_json::from_slice(&std::fs::read(path)?)?)
    }

    /// Finds a section's placement and collapse policy.
    ///
    /// # Errors
    /// Rejects undeclared sections.
    pub fn section_spec(
        &self,
        locale: DocsLocale,
        label: &str,
    ) -> anyhow::Result<(usize, &DocsSection)> {
        self.sections
            .get(&locale)
            .and_then(|sections| {
                sections
                    .iter()
                    .enumerate()
                    .find(|(_, section)| section.label == label)
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Sidebar section \"{label}\" has no placement in the {} locale.",
                    locale.name()
                )
            })
    }

    /// Selects a collection in source-stable section and page order.
    ///
    /// # Errors
    /// Rejects a page whose section has no placement.
    pub fn ordered_pages(
        &self,
        locale: DocsLocale,
        collection: &str,
    ) -> anyhow::Result<Vec<&DocsPage>> {
        let mut pages = self
            .pages
            .iter()
            .filter(|page| {
                page.locale == locale && page.sidebar.map(DocsSidebar::name) == Some(collection)
            })
            .map(|page| Ok((self.section_spec(locale, &page.section)?.0, page)))
            .collect::<anyhow::Result<Vec<_>>>()?;
        pages.sort_by(|(left_section, left), (right_section, right)| {
            left_section.cmp(right_section).then_with(|| {
                left.order
                    .partial_cmp(&right.order)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        });
        Ok(pages.into_iter().map(|(_, page)| page).collect())
    }

    /// Derives a navigation destination from the first page in a collection.
    ///
    /// # Errors
    /// Rejects an empty collection or undeclared section.
    pub fn landing_link(&self, locale: DocsLocale, collection: &str) -> anyhow::Result<String> {
        let pages = self.ordered_pages(locale, collection)?;
        let first = pages.first().ok_or_else(|| {
            anyhow::anyhow!("Sidebar collection \"{collection}\" publishes no page.")
        })?;
        Ok(route_link(&first.route))
    }
}

/// Converts a Markdown route to its clean site URL.
#[must_use]
pub fn route_link(route: &str) -> String {
    let route = route
        .strip_suffix("index.md")
        .or_else(|| route.strip_suffix(".md"))
        .unwrap_or(route);
    format!("/{route}")
}
