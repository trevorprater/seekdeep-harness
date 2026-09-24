//! Fragment validation against built `VitePress` HTML.

use std::{
    collections::{HashMap, HashSet},
    fmt::Write as _,
    path::Path,
};

use percent_encoding::percent_decode_str;
use scraper::{Html, Selector};
use url::Url;

/// One internal fragment reference that does not resolve in the built site.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrokenSiteFragment {
    /// HTML file containing the link.
    pub source: String,
    /// Link value emitted by `VitePress`.
    pub href: String,
    /// Built HTML target, absent when no route was emitted.
    pub target: Option<String>,
    /// Decoded fragment ID requested by the link.
    pub fragment: String,
}

/// Result of checking every fragment-bearing anchor in a built site.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SiteFragmentReport {
    /// Number of internal, nonempty fragment references inspected.
    pub checked: usize,
    /// References whose route or fragment ID is absent.
    pub broken: Vec<BrokenSiteFragment>,
}

#[derive(Debug)]
struct BuiltPage {
    file: String,
    route: String,
    ids: HashSet<String>,
    hrefs: Vec<String>,
}

/// Checks fragment-bearing links in one `VitePress` output directory.
///
/// # Errors
///
/// Returns missing output, traversal, file, HTML-selector, ambiguous-route, or
/// invalid-URL diagnostics.
pub fn inspect_site_fragments(dist_root: &Path) -> anyhow::Result<SiteFragmentReport> {
    let pages = load_pages(dist_root)?;
    let by_route = route_map(&pages)?;
    let origin = Url::parse("https://dsh-docs.invalid/")?;
    let mut report = SiteFragmentReport::default();
    for page in &pages {
        let base = origin.join(&page.route)?;
        for href in &page.hrefs {
            if !href.contains('#') {
                continue;
            }
            let target_url = base.join(href).map_err(|error| {
                anyhow::Error::new(error).context(format!(
                    "verify-doc-site-fragments: {} has invalid fragment href {}.",
                    page.file,
                    json_string(href)
                ))
            })?;
            let Some(raw_fragment) = target_url.fragment() else {
                continue;
            };
            if target_url.origin() != origin.origin() || raw_fragment.is_empty() {
                continue;
            }
            let fragment = decoded_fragment(raw_fragment);
            if fragment.is_empty() {
                continue;
            }
            report.checked += 1;
            let target = by_route.get(target_url.path()).map(|index| &pages[*index]);
            if target.is_none_or(|target| !target.ids.contains(&fragment)) {
                report.broken.push(BrokenSiteFragment {
                    source: page.file.clone(),
                    href: href.clone(),
                    target: target.map(|target| target.file.clone()),
                    fragment,
                });
            }
        }
    }
    Ok(report)
}

fn load_pages(dist_root: &Path) -> anyhow::Result<Vec<BuiltPage>> {
    if !dist_root.is_dir() {
        anyhow::bail!(
            "verify-doc-site-fragments: no HTML files found under {}; run docs:build first.",
            dist_root.display()
        );
    }
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(dist_root)
        .into_iter()
        .filter_entry(|entry| {
            entry.depth() == 0
                || entry.path().strip_prefix(dist_root).is_ok_and(|relative| {
                    !relative
                        .components()
                        .any(|component| component.as_os_str().to_string_lossy().starts_with('.'))
                })
        })
    {
        let entry = entry?;
        if !entry.file_type().is_file()
            || entry.path().extension().and_then(std::ffi::OsStr::to_str) != Some("html")
        {
            continue;
        }
        files.push(slash_path(entry.path().strip_prefix(dist_root)?));
    }
    files.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
    if files.is_empty() {
        anyhow::bail!(
            "verify-doc-site-fragments: no HTML files found under {}; run docs:build first.",
            dist_root.display()
        );
    }

    let id_selector = Selector::parse("[id]")
        .map_err(|error| anyhow::anyhow!("invalid built-in selector [id]: {error:?}"))?;
    let named_anchor_selector = Selector::parse("a[name]")
        .map_err(|error| anyhow::anyhow!("invalid built-in selector a[name]: {error:?}"))?;
    let href_selector = Selector::parse("a[href]")
        .map_err(|error| anyhow::anyhow!("invalid built-in selector a[href]: {error:?}"))?;

    let mut pages = Vec::new();
    for file in files {
        let document = Html::parse_document(&std::fs::read_to_string(dist_root.join(&file))?);
        let mut ids = HashSet::new();
        for element in document.select(&id_selector) {
            if let Some(id) = element.value().attr("id") {
                ids.insert(id.to_owned());
            }
        }
        for element in document.select(&named_anchor_selector) {
            if let Some(name) = element.value().attr("name") {
                ids.insert(name.to_owned());
            }
        }
        let hrefs = document
            .select(&href_selector)
            .filter_map(|element| element.value().attr("href").map(str::to_owned))
            .collect();
        pages.push(BuiltPage {
            route: route_for(&file),
            file,
            ids,
            hrefs,
        });
    }

    Ok(pages)
}

fn route_map(pages: &[BuiltPage]) -> anyhow::Result<HashMap<String, usize>> {
    let mut by_route = HashMap::<String, usize>::new();
    for (index, page) in pages.iter().enumerate() {
        for alias in aliases_for(page) {
            if let Some(existing) = by_route.get(&alias).copied()
                && existing != index
            {
                anyhow::bail!(
                    "verify-doc-site-fragments: built pages {} and {} share route {}.",
                    pages[existing].file,
                    page.file,
                    json_string(&alias)
                );
            }
            by_route.insert(alias, index);
        }
    }

    Ok(by_route)
}

/// Renders the source-compatible success or failure report.
#[must_use]
pub fn render_site_fragment_report(report: &SiteFragmentReport) -> String {
    if report.broken.is_empty() {
        return format!(
            "verify-doc-site-fragments: {} internal fragment reference(s) resolve.\n",
            report.checked
        );
    }
    let mut output = format!(
        "verify-doc-site-fragments: {} broken fragment reference(s):\n",
        report.broken.len()
    );
    for item in &report.broken {
        let target = item.target.as_ref().map_or_else(
            || "target route was not built".to_owned(),
            |target| format!("{} has no id {}", target, json_string(&item.fragment)),
        );
        let _ = writeln!(
            output,
            "  {}: {} ({})",
            item.source,
            json_string(&item.href),
            target
        );
    }
    output
}

fn route_for(file: &str) -> String {
    if file == "index.html" {
        return "/".to_owned();
    }
    if let Some(stem) = file.strip_suffix("/index.html") {
        return format!("/{stem}/");
    }
    format!(
        "/{}",
        file.strip_suffix(".html")
            .expect("HTML inventory contains only .html files")
    )
}

fn aliases_for(page: &BuiltPage) -> Vec<String> {
    if page.route == "/" {
        return vec![
            "/".to_owned(),
            "/index".to_owned(),
            "/index.html".to_owned(),
        ];
    }
    if let Some(stem) = page.route.strip_suffix('/') {
        return vec![
            page.route.clone(),
            stem.to_owned(),
            format!("{stem}/index"),
            format!("{stem}/index.html"),
        ];
    }
    vec![page.route.clone(), format!("{}.html", page.route)]
}

fn decoded_fragment(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len()
            || !bytes[index + 1].is_ascii_hexdigit()
            || !bytes[index + 2].is_ascii_hexdigit()
        {
            return raw.to_owned();
        }
        index += 3;
    }
    percent_decode_str(raw)
        .decode_utf8()
        .map_or_else(|_| raw.to_owned(), std::borrow::Cow::into_owned)
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned())
}

fn slash_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
