use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use markdown::mdast::Node;
use path_clean::PathClean as _;
use regex::Regex;

use super::{DocsLocale, DocsPage};

const REPOSITORY: &str = "https://github.com/trevorprater/seekdeep-harness";

/// Context for rewriting one canonical Markdown document.
pub struct RewriteOptions<'a> {
    /// Destination route tree.
    pub locale: DocsLocale,
    /// Repository-relative source path.
    pub source_path: &'a str,
    /// Destination Markdown route.
    pub route: &'a str,
    /// Publication allowlist used for local link routing.
    pub pages: &'a [DocsPage],
    /// Absolute repository root.
    pub repo_root: &'a Path,
    /// Revision used for repository links.
    pub repository_ref: &'a str,
}

struct Replacement {
    start: usize,
    end: usize,
    value: String,
}

/// Places one local image and returns its projected destination.
pub type ImagePlacer<'a> = dyn FnMut(&Path) -> anyhow::Result<String> + 'a;

/// Rewrites destinations while preserving every other authored byte.
///
/// # Errors
/// Rejects duplicate aliases, missing targets, malformed escapes, invalid Markdown ranges,
/// and failures from the optional image-placement callback.
pub fn rewrite_markdown(
    source: &str,
    options: &RewriteOptions<'_>,
    mut place_image: Option<&mut ImagePlacer<'_>>,
) -> anyhow::Result<String> {
    let published = source_map(options.pages)?;
    let source_abs = options.repo_root.join(options.source_path).clean();
    map_markdown_destinations(source, &mut |url, image| {
        if external(url) {
            return Ok(None);
        }
        let boundary = url.find(['?', '#']).unwrap_or(url.len());
        let (path, suffix) = url.split_at(boundary);
        if path.is_empty() {
            return Ok(None);
        }
        let (target, line) = resolve_target(&source_abs, path, options.repo_root)?;
        let target_path = relative_path(options.repo_root, &target);
        let locale = if target_path == counterpart(options.source_path) {
            options.locale.other()
        } else {
            options.locale
        };
        let next = if let Some(page) = published.get(&(target_path, locale)) {
            route_target(options.route, &page.route, suffix)
        } else if image && let Some(place) = place_image.as_mut() {
            format!("{}{suffix}", place(&target)?)
        } else {
            github_target(&target, line, suffix, options, image)?
        };
        Ok(Some(next))
    })
}

/// Maps parsed link, image, and definition destinations without reformatting Markdown.
///
/// The callback receives the parsed destination and whether the node is an image.
/// Returning `None` preserves the original token, including its authored escapes.
///
/// # Errors
/// Returns parser, source-range, destination-token, and callback failures.
pub fn map_markdown_destinations(
    source: &str,
    mapper: &mut impl FnMut(&str, bool) -> anyhow::Result<Option<String>>,
) -> anyhow::Result<String> {
    let tree = crate::markdown_util::parse_markdown(source).map_err(anyhow::Error::msg)?;
    let mut replacements = Vec::new();
    let mut failure = None;
    crate::markdown_util::visit_markdown(&tree, &mut |node| {
        let (url, image, definition, position) = match node {
            Node::Link(link) => (&link.url, false, false, &link.position),
            Node::Image(image) => (&image.url, true, false, &image.position),
            Node::Definition(definition) => (&definition.url, false, true, &definition.position),
            _ => return failure.is_none(),
        };
        if failure.is_some() {
            return failure.is_none();
        }
        let rewrite = (|| -> anyhow::Result<()> {
            let Some(next) = mapper(url, image)? else {
                return Ok(());
            };
            let position = position.as_ref().ok_or_else(|| {
                anyhow::anyhow!(
                    "project-doc-site: link {} has no source offsets.",
                    json(url)
                )
            })?;
            let raw = &source[position.start.offset..position.end.offset];
            let (start, end) = destination_range(raw, definition)?;
            replacements.push(Replacement {
                start: position.start.offset + start,
                end: position.start.offset + end,
                value: next,
            });
            Ok(())
        })();
        if let Err(error) = rewrite {
            failure = Some(error);
        }
        failure.is_none()
    });
    if let Some(failure) = failure {
        return Err(failure);
    }
    replacements.sort_by_key(|replacement| std::cmp::Reverse(replacement.start));
    let mut output = source.to_owned();
    for replacement in replacements {
        output.replace_range(replacement.start..replacement.end, &replacement.value);
    }
    Ok(output)
}

fn source_map(pages: &[DocsPage]) -> anyhow::Result<HashMap<(String, DocsLocale), &DocsPage>> {
    let mut map = HashMap::new();
    for page in pages {
        for source in std::iter::once(&page.source).chain(&page.source_aliases) {
            anyhow::ensure!(
                map.insert((source.clone(), page.locale), page).is_none(),
                "project-doc-site: duplicate source or alias {} for locale {}.",
                json(source),
                json(page.locale.name())
            );
        }
    }
    Ok(map)
}

pub(crate) fn external(url: &str) -> bool {
    static SCHEME: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"^[a-zA-Z][a-zA-Z0-9+.-]*:").expect("static scheme regex")
    });
    url.starts_with(['#', '/']) || SCHEME.is_match(url)
}

fn counterpart(source: &str) -> String {
    source.strip_suffix(".zh.md").map_or_else(
        || {
            source
                .strip_suffix(".md")
                .map_or_else(|| source.to_owned(), |base| format!("{base}.zh.md"))
        },
        |base| format!("{base}.md"),
    )
}

fn resolve_target(source: &Path, raw: &str, root: &Path) -> anyhow::Result<(PathBuf, Option<f64>)> {
    let decoded = decode_path(raw)?;
    let parent = source.parent().unwrap_or(root);
    let direct = parent.join(&decoded).clean();
    if direct.exists() {
        return Ok((direct, None));
    }
    if let Some((path, suffix)) = decoded.rsplit_once(':')
        && !suffix.is_empty()
        && suffix.bytes().all(|value| value.is_ascii_digit())
    {
        let target = parent.join(path).clean();
        if target.exists() {
            return Ok((target, Some(suffix.parse()?)));
        }
    }
    if Path::new(&decoded).extension().is_none() {
        let markdown = parent.join(format!("{decoded}.md")).clean();
        if markdown.exists() {
            return Ok((markdown, None));
        }
        let index = parent.join(&decoded).join("index.md").clean();
        if index.exists() {
            return Ok((index, None));
        }
    }
    anyhow::bail!(
        "project-doc-site: {} links to missing path {}.",
        relative_path(root, source),
        json(raw)
    )
}

fn decode_path(path: &str) -> anyhow::Result<String> {
    let bytes = path.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'%'
            && (index + 2 >= bytes.len()
                || !bytes[index + 1].is_ascii_hexdigit()
                || !bytes[index + 2].is_ascii_hexdigit())
        {
            anyhow::bail!(
                "project-doc-site: malformed percent escape in {}.",
                json(path)
            );
        }
    }
    percent_encoding::percent_decode_str(path)
        .decode_utf8()
        .map(std::borrow::Cow::into_owned)
        .map_err(|_| {
            anyhow::anyhow!(
                "project-doc-site: malformed percent escape in {}.",
                json(path)
            )
        })
}

fn route_target(from: &str, to: &str, suffix: &str) -> String {
    let relative = relative_path(
        Path::new(from).parent().unwrap_or_else(|| Path::new("")),
        Path::new(to),
    );
    format!(
        "{}{relative}{suffix}",
        if relative.starts_with('.') { "" } else { "./" }
    )
}

fn github_target(
    target: &Path,
    line: Option<f64>,
    suffix: &str,
    options: &RewriteOptions<'_>,
    image: bool,
) -> anyhow::Result<String> {
    let path = relative_path(options.repo_root, target);
    if image {
        return Ok(format!(
            "https://raw.githubusercontent.com/trevorprater/seekdeep-harness/{}/{path}{suffix}",
            options.repository_ref
        ));
    }
    let kind = if std::fs::symlink_metadata(target)?.is_dir() {
        "tree"
    } else {
        "blob"
    };
    let suffix = line.map_or_else(
        || suffix.to_owned(),
        |line| format!("#L{}", ryu_js::Buffer::new().format(line)),
    );
    Ok(format!(
        "{REPOSITORY}/{kind}/{}/{path}{suffix}",
        options.repository_ref
    ))
}

pub(super) fn relative_path(from: &Path, to: &Path) -> String {
    let from = from.components().collect::<Vec<_>>();
    let to = to.components().collect::<Vec<_>>();
    let shared = from
        .iter()
        .zip(&to)
        .take_while(|(left, right)| left == right)
        .count();
    std::iter::repeat_n("..".to_owned(), from.len() - shared)
        .chain(
            to[shared..]
                .iter()
                .map(|part| part.as_os_str().to_string_lossy().into_owned()),
        )
        .collect::<Vec<_>>()
        .join("/")
}

fn destination_range(raw: &str, definition: bool) -> anyhow::Result<(usize, usize)> {
    let bytes = raw.as_bytes();
    let first = raw.find('[').ok_or_else(|| {
        anyhow::anyhow!(
            "project-doc-site: cannot locate label end in {}.",
            json(raw)
        )
    })?;
    let mut depth = 0;
    let mut index = first;
    let end = loop {
        anyhow::ensure!(
            index < bytes.len(),
            "project-doc-site: cannot locate label end in {}.",
            json(raw)
        );
        match bytes[index] {
            b'\\' => index += 1,
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    break index;
                }
            }
            _ => {}
        }
        index += 1;
    };
    let start = if definition {
        raw[end + 1..]
            .find(':')
            .map(|index| index + end + 2)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "project-doc-site: cannot locate definition separator in {}.",
                    json(raw)
                )
            })?
    } else {
        anyhow::ensure!(
            bytes.get(end + 1) == Some(&b'('),
            "project-doc-site: cannot locate inline destination in {}.",
            json(raw)
        );
        end + 2
    };
    let start = start + raw[start..].len() - raw[start..].trim_start().len();
    if bytes.get(start) == Some(&b'<') {
        let mut index = start + 1;
        while index < bytes.len() {
            if bytes[index] == b'\\' {
                index += 1;
            } else if bytes[index] == b'>' {
                return Ok((start + 1, index));
            }
            index += 1;
        }
        anyhow::bail!(
            "project-doc-site: cannot locate angle-bracket destination end in {}.",
            json(raw)
        );
    }
    let mut index = start;
    let mut depth = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 1,
            b'(' => depth += 1,
            b')' if depth == 0 => return Ok((start, index)),
            b')' => depth -= 1,
            byte if byte.is_ascii_whitespace() && depth == 0 => return Ok((start, index)),
            _ => {}
        }
        index += 1;
    }
    Ok((start, raw.len()))
}

/// Adds projection-owned edit target and outline frontmatter.
#[must_use]
pub fn add_projection_frontmatter(markdown: &str, page: &DocsPage) -> String {
    let mut fields = format!("editSource: {}", json(&page.source));
    if let Some(outline) = &page.outline {
        fields.push_str("\noutline: ");
        fields.push_str(&outline_json(outline));
    }
    markdown.strip_prefix("---\n").map_or_else(
        || format!("---\n{fields}\n---\n\n{markdown}"),
        |body| format!("---\n{fields}\n{body}"),
    )
}

fn outline_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Number(number) => number.as_f64().map_or_else(
            || "null".to_owned(),
            |number| ryu_js::Buffer::new().format(number).to_owned(),
        ),
        serde_json::Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(outline_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::String(_)
        | serde_json::Value::Object(_) => value.to_string(),
    }
}

/// Removes repository chrome or selects a locale home's frontmatter-only content.
///
/// # Errors
/// Rejects a home source without a complete frontmatter block.
pub fn projected_page_content(markdown: &str, page: &DocsPage) -> anyhow::Result<String> {
    static SWITCHER: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"^(?:English \| \[中文\]\([^)]*\)|\[English\]\([^)]*\) \| 中文)$")
            .expect("static switcher regex")
    });
    static BADGE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"^\[!\[[^\]]*\]\(https://img\.shields\.io/[^)]*\)\]\([^)]*\)$")
            .expect("static badge regex")
    });
    if page.sidebar.is_none() {
        anyhow::ensure!(
            markdown.starts_with("---\n"),
            "project-doc-site: locale home source {} must start with YAML frontmatter.",
            json(&page.source)
        );
        let close = markdown[4..]
            .find("\n---\n")
            .map(|index| index + 9)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "project-doc-site: locale home source {} has unclosed YAML frontmatter.",
                    json(&page.source)
                )
            })?;
        return Ok(markdown[..close].to_owned());
    }
    let mut lines = markdown.split('\n').collect::<Vec<_>>();
    if let Some(index) = lines.iter().position(|line| SWITCHER.is_match(line))
        && index < 8
    {
        let count = if lines.get(index + 1) == Some(&"") {
            2
        } else {
            1
        };
        lines.drain(index..index + count);
    }
    if let Some(index) = lines.iter().rposition(|line| BADGE.is_match(line)) {
        let start = if index > 0 && lines[index - 1].is_empty() {
            index - 1
        } else {
            index
        };
        lines.drain(start..=index);
    }
    Ok(lines.join("\n"))
}

fn json(value: &str) -> String {
    serde_json::to_string(value).expect("strings serialize")
}
