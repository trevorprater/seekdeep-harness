//! Stable declaration-source links for documentation generated from the pinned oracle.

use std::path::Path;

use path_clean::PathClean as _;

use crate::doc_site::map_markdown_destinations;

/// Repository that carries the pinned source revision, including its reviewed changes.
pub const ORACLE_REPOSITORY: &str = "https://github.com/fugue-labs/deepseek-harness";

/// Pins TypeScript and JavaScript declaration links to their actual oracle files.
///
/// Markdown, images, Rust sources, external URLs, and code examples remain untouched.
/// Product renames in source-path labels are resolved against the original oracle spelling.
///
/// # Errors
/// Returns Markdown parsing, invalid source paths, and absent oracle-file diagnostics.
pub fn pin_oracle_source_links(
    markdown: &str,
    document: &Path,
    source_root: &Path,
    revision: &str,
) -> anyhow::Result<String> {
    let parent = document.parent().unwrap_or(Path::new(""));
    map_markdown_destinations(markdown, &mut |url, image| {
        if image || crate::doc_site::external(url) {
            return Ok(None);
        }
        let boundary = url.find(['?', '#']).unwrap_or(url.len());
        let (raw_path, suffix) = url.split_at(boundary);
        let (raw_path, line) = raw_path
            .rsplit_once(':')
            .filter(|(_, line)| !line.is_empty() && line.bytes().all(|byte| byte.is_ascii_digit()))
            .map_or((raw_path, None), |(path, line)| (path, Some(line)));
        if ![".ts", ".tsx", ".js", ".mjs", ".cjs"]
            .iter()
            .any(|extension| raw_path.ends_with(extension))
        {
            return Ok(None);
        }
        let decoded = percent_encoding::percent_decode_str(raw_path).decode_utf8()?;
        let path = parent.join(decoded.as_ref()).clean();
        anyhow::ensure!(
            path.components()
                .all(|part| matches!(part, std::path::Component::Normal(_))),
            "Declaration source {} escapes the oracle repository.",
            path.display()
        );
        let original = path.to_string_lossy().replace("seekdeep-", "dsh-");
        let path = if source_root.join(&path).is_file() {
            path
        } else {
            original.into()
        };
        anyhow::ensure!(
            source_root.join(&path).is_file(),
            "Declaration source {} does not exist in the pinned oracle.",
            path.display()
        );
        let path = path.to_string_lossy().replace('\\', "/");
        let mut destination = url::Url::parse(ORACLE_REPOSITORY)?;
        destination
            .path_segments_mut()
            .map_err(|()| anyhow::anyhow!("Oracle repository URL has no path segments."))?
            .extend(["blob", revision])
            .extend(path.split('/'));
        let suffix = if let Some(line) = line {
            format!("#L{}", ryu_js::Buffer::new().format(line.parse::<f64>()?))
        } else {
            suffix.to_owned()
        };
        Ok(Some(format!("{destination}{suffix}")))
    })
}

/// Reads the revision used by generated documentation source links.
///
/// # Errors
/// Rejects absent or malformed snapshot metadata.
pub fn oracle_revision(repository_root: &Path) -> anyhow::Result<String> {
    let snapshot = std::fs::read_to_string(repository_root.join("SOURCE_SNAPSHOT"))?;
    let revision = snapshot
        .lines()
        .find_map(|line| line.strip_prefix("commit="))
        .ok_or_else(|| anyhow::anyhow!("SOURCE_SNAPSHOT has no commit."))?;
    anyhow::ensure!(
        revision.len() == 40 && revision.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "SOURCE_SNAPSHOT commit must be a full Git object ID."
    );
    Ok(revision.to_owned())
}
