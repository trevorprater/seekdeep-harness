//! Checks or updates declaration links in the canonical documentation publication set.

use std::{collections::BTreeSet, path::PathBuf};

use clap::Parser;
use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root,
    doc_site::DocsManifest,
    doc_source_links::{oracle_revision, pin_oracle_source_links},
};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    write: bool,
    files: Vec<PathBuf>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let root = compiled_repository_root();
    let revision = oracle_revision(root)?;
    let snapshot = std::fs::read_to_string(root.join("SOURCE_SNAPSHOT"))?;
    let source = snapshot
        .lines()
        .find_map(|line| line.strip_prefix("repository="))
        .ok_or_else(|| anyhow::anyhow!("SOURCE_SNAPSHOT has no repository."))?;
    let files = if args.files.is_empty() {
        DocsManifest::read(&root.join("website/docs.json"))?
            .pages
            .into_iter()
            .map(|page| PathBuf::from(page.source))
            .collect::<BTreeSet<_>>()
    } else {
        args.files.into_iter().collect()
    };
    let mut updates = Vec::new();
    for file in files {
        let path = root.join(&file);
        let before = std::fs::read_to_string(&path)?;
        let after = pin_oracle_source_links(&before, &file, &PathBuf::from(source), &revision)?;
        if before != after {
            updates.push((file, after));
        }
    }
    if args.write {
        for (file, content) in &updates {
            std::fs::write(root.join(file), content)?;
        }
        println!(
            "pin-doc-source-links: updated {} canonical files.",
            updates.len()
        );
    } else {
        anyhow::ensure!(
            updates.is_empty(),
            "Unpinned declaration links in {}. Run pin-doc-source-links --write.",
            updates
                .iter()
                .map(|(path, _)| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        println!("pin-doc-source-links: all declaration links are pinned.");
    }
    Ok(())
}
