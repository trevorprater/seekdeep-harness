//! Verifies fixture provenance against a read-only pinned source checkout.

use std::{error::Error, path::Path, process::Command};

use serde_json::Value;

mod model;
mod schema;

fn main() -> Result<(), Box<dyn Error>> {
    let source_root = std::env::args_os()
        .nth(1)
        .ok_or("usage: oracle_fixtures <pinned-source-root>")?;
    let source_root = Path::new(&source_root);
    let head = Command::new("git")
        .arg("-C")
        .arg(source_root)
        .args(["rev-parse", "HEAD"])
        .output()?;
    let expected_commit = include_str!("../../../../SOURCE_SNAPSHOT")
        .lines()
        .find_map(|line| line.strip_prefix("commit="))
        .ok_or("SOURCE_SNAPSHOT has no pinned commit")?;
    if !head.status.success() || String::from_utf8_lossy(&head.stdout).trim() != expected_commit {
        return Err("oracle checkout does not match SOURCE_SNAPSHOT".into());
    }
    for (name, script, mode, expected) in [
        (
            "source_type_model.json",
            model::SCRIPT,
            "renderer",
            include_str!("../../tests/fixtures/source_type_model.json"),
        ),
        (
            "source_emitter.json",
            model::SCRIPT,
            "emitter",
            include_str!("../../tests/fixtures/source_emitter.json"),
        ),
        (
            "source_schema_matrix.json",
            schema::SCRIPT,
            "schema",
            include_str!("../../tests/fixtures/source_schema_matrix.json"),
        ),
    ] {
        let output = Command::new("node")
            .args(["-e", script])
            .arg(source_root)
            .arg(mode)
            .output()?;
        if !output.status.success() {
            return Err(format!(
                "{name}: source capture failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        let actual: Value = serde_json::from_slice(&output.stdout)?;
        let expected: Value = serde_json::from_str(expected)?;
        if actual != expected {
            return Err(format!("{name}: live pinned source differs from fixture").into());
        }
        println!("{name}: live pinned source matches");
    }
    Ok(())
}
