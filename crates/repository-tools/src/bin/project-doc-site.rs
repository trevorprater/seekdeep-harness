//! Projects canonical documentation and repository-owned images for the website.

use std::path::PathBuf;

use clap::Parser;
use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root,
    doc_site::{DocsManifest, docs_source_files, prepare_site, project_docs},
};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    root: Option<PathBuf>,
    #[arg(long)]
    manifest: Option<PathBuf>,
    #[arg(long)]
    repository_ref: Option<String>,
    #[arg(long)]
    sources: bool,
    #[arg(long, conflicts_with = "sources")]
    prepare: bool,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let root = args
        .root
        .unwrap_or_else(|| compiled_repository_root().to_path_buf())
        .canonicalize()?;
    let manifest_path = args
        .manifest
        .unwrap_or_else(|| root.join("website/docs.json"));
    let manifest = DocsManifest::read(&manifest_path)?;
    if args.prepare {
        let revision = args
            .repository_ref
            .or_else(|| std::env::var("GITHUB_SHA").ok())
            .map_or_else(|| git_value(&root, &["rev-parse", "HEAD"]), Ok)?;
        let branch = std::env::var("GITHUB_HEAD_REF")
            .ok()
            .filter(|value| !value.is_empty())
            .or_else(|| std::env::var("GITHUB_REF_NAME").ok())
            .map_or_else(
                || git_value(&root, &["symbolic-ref", "--quiet", "--short", "HEAD"]),
                Ok,
            )?;
        prepare_site(&root, &manifest, &revision, &branch)?;
        println!("project-doc-site: Rust/WASM VitePress inputs prepared.");
        return Ok(());
    }
    if args.sources {
        println!(
            "{}",
            serde_json::to_string(&docs_source_files(&root, &manifest)?)?
        );
    } else {
        let output = root.join("website/.generated");
        let revision = args
            .repository_ref
            .or_else(|| std::env::var("GITHUB_SHA").ok())
            .unwrap_or_else(|| "master".to_owned());
        println!(
            "{}",
            serde_json::to_string(&project_docs(&root, &output, &manifest, &revision)?)?
        );
    }
    Ok(())
}

fn git_value(root: &std::path::Path, arguments: &[&str]) -> anyhow::Result<String> {
    let output = std::process::Command::new("git")
        .args(arguments)
        .current_dir(root)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "git {} failed: {}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}
