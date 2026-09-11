use std::{
    collections::{HashMap, HashSet},
    path::{Component, Path, PathBuf},
};

use path_clean::PathClean as _;
use serde::Serialize;

use super::rewrite::relative_path;
use super::{
    DocsManifest, RewriteOptions, add_projection_frontmatter, projected_page_content,
    rewrite_markdown,
};

/// Files emitted by one successful source projection.
#[derive(Debug, Serialize)]
pub struct ProjectionReport {
    /// Number of published locale pages.
    pub pages: usize,
    /// Unique image destinations written beside pages.
    pub images: usize,
    /// Generated directory used for this build.
    pub output: PathBuf,
}

/// Resolves a regular image file while refusing files outside the repository.
///
/// # Errors
/// Propagates missing-file and filesystem inspection failures.
pub fn publishable_image(path: &Path, repo_root: &Path) -> anyhow::Result<Option<PathBuf>> {
    let real = std::fs::canonicalize(path)?;
    Ok((real.starts_with(repo_root) && std::fs::metadata(&real)?.is_file()).then_some(real))
}

/// Lists canonical sources and publishable referenced images in source order.
///
/// # Errors
/// Returns invalid-link, image, and filesystem diagnostics.
pub fn docs_source_files(root: &Path, manifest: &DocsManifest) -> anyhow::Result<Vec<PathBuf>> {
    let mut output = Vec::new();
    let mut found = HashSet::new();
    for page in &manifest.pages {
        let source = root.join(&page.source).clean();
        if found.insert(source.clone()) {
            output.push(source);
        }
    }
    for page in &manifest.pages {
        let source = root.join(&page.source).clean();
        if !source.exists() {
            continue;
        }
        let mut place = |path: &Path| -> anyhow::Result<String> {
            if let Some(real) = publishable_image(path, root)?
                && found.insert(real.clone())
            {
                output.push(real);
            }
            Ok(String::new())
        };
        rewrite_markdown(
            &std::fs::read_to_string(source)?,
            &RewriteOptions {
                locale: page.locale,
                source_path: &page.source,
                route: &page.route,
                pages: &manifest.pages,
                repo_root: root,
                repository_ref: "master",
            },
            Some(&mut place),
        )?;
    }
    Ok(output)
}

/// Rebuilds the disposable Markdown tree and its repository-owned image assets.
///
/// # Errors
/// Rejects duplicate routes, unsafe destination paths, absent or non-file sources,
/// conflicting output claims, invalid images, and link-rewrite failures.
pub fn project_docs(
    root: &Path,
    output: &Path,
    manifest: &DocsManifest,
    revision: &str,
) -> anyhow::Result<ProjectionReport> {
    anyhow::ensure!(
        output.clean() == root.join("website/.generated").clean(),
        "project-doc-site: output must be the disposable website/.generated directory."
    );
    if let Ok(metadata) = std::fs::symlink_metadata(output) {
        if metadata.is_dir() {
            std::fs::remove_dir_all(output)?;
        } else {
            std::fs::remove_file(output)?;
        }
    }
    let mut routes = HashSet::new();
    let mut claimed = HashMap::new();
    let mut images = HashSet::new();
    for page in &manifest.pages {
        anyhow::ensure!(
            routes.insert(&page.route),
            "project-doc-site: duplicate route {}.",
            json(&page.route)
        );
        anyhow::ensure!(
            Path::new(&page.route)
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
            "project-doc-site: route {} must remain inside the generated tree.",
            json(&page.route)
        );
        let source = root.join(&page.source).clean();
        anyhow::ensure!(
            std::fs::symlink_metadata(&source).is_ok_and(|metadata| metadata.is_file()),
            "project-doc-site: source {} does not exist or is not a file.",
            json(&page.source)
        );
        let destination = output.join(&page.route).clean();
        claim(&mut claimed, &destination, &source, root, output)?;
        let directory = destination
            .parent()
            .ok_or_else(|| anyhow::anyhow!("project-doc-site: page route has no output parent."))?;
        std::fs::create_dir_all(directory)?;
        let mut place = |path: &Path| -> anyhow::Result<String> {
            let real = publishable_image(path, root)?.ok_or_else(|| anyhow::anyhow!(
                "project-doc-site: {} references image {}, which is not a regular file inside the repository.", page.source, relative_path(root, path)))?;
            let name = real
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("project-doc-site: image has no filename."))?;
            let target = directory.join(name);
            claim(&mut claimed, &target, &real, root, output)?;
            std::fs::copy(&real, &target)?;
            images.insert(target);
            Ok(format!("./{}", encode_uri(&name.to_string_lossy())))
        };
        let rewritten = rewrite_markdown(
            &std::fs::read_to_string(&source)?,
            &RewriteOptions {
                locale: page.locale,
                source_path: &page.source,
                route: &page.route,
                pages: &manifest.pages,
                repo_root: root,
                repository_ref: revision,
            },
            Some(&mut place),
        )?;
        std::fs::write(
            destination,
            add_projection_frontmatter(&projected_page_content(&rewritten, page)?, page),
        )?;
    }
    Ok(ProjectionReport {
        pages: manifest.pages.len(),
        images: images.len(),
        output: output.to_owned(),
    })
}

fn claim(
    claimed: &mut HashMap<PathBuf, PathBuf>,
    target: &Path,
    source: &Path,
    root: &Path,
    output: &Path,
) -> anyhow::Result<()> {
    if let Some(holder) = claimed.get(target)
        && holder != source
    {
        anyhow::bail!(
            "project-doc-site: {} and {} both project to {}.",
            relative_path(root, source),
            relative_path(root, holder),
            relative_path(output, target)
        );
    }
    claimed.insert(target.to_owned(), source.to_owned());
    Ok(())
}

fn encode_uri(value: &str) -> String {
    use std::fmt::Write as _;
    let mut output = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b";,/?:@&=+$#-_.!~*'()".contains(&byte) {
            output.push(char::from(byte));
        } else {
            write!(output, "%{byte:02X}").expect("string formatting succeeds");
        }
    }
    output
}

fn json(value: &str) -> String {
    serde_json::to_string(value).expect("strings serialize")
}
