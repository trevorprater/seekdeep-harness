//! Measures projection stages against an explicit captured model.

use std::time::Instant;

use seekdeep_typert_generator::{
    catalog::{
        CordisCatalogModel, CordisCatalogPolicy, CordisCatalogProjector, render_page_region,
    },
    model::{FaceModel, SourceDeclarationModel},
};
use serde_json::Value;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os().nth(1).ok_or("capture file required")?;
    let started = Instant::now();
    let payload: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    let face: FaceModel = serde_json::from_value(payload["face"].clone())?;
    let declarations: Vec<SourceDeclarationModel> =
        serde_json::from_value(payload["sourceDeclarations"].clone())?;
    let policy: CordisCatalogPolicy = serde_json::from_value(payload["policy"].clone())?;
    let projector = CordisCatalogProjector::new(&face, &declarations, &policy);
    eprintln!("deserialize/index: {:?}", started.elapsed());
    let started = Instant::now();
    let model = projector.project()?;
    eprintln!("validate/project: {:?}", started.elapsed());
    let started = Instant::now();
    let data = projector.runtime_catalog(&model)?;
    eprintln!(
        "runtime catalog ({} types): {:?}",
        data.types.len(),
        started.elapsed()
    );
    let page = payload["pages"]
        .as_array()
        .ok_or("missing pages")?
        .iter()
        .find(|page| page["page"] == "core.md")
        .ok_or("missing core page")?;
    let selected: CordisCatalogModel = serde_json::from_value(
        serde_json::json!({"services":page["services"],"events":page["events"]}),
    )?;
    let started = Instant::now();
    let region = render_page_region("core.md", &selected.services, &selected.events, &policy)?;
    eprintln!(
        "core region ({} bytes): {:?}",
        region.len(),
        started.elapsed()
    );
    Ok(())
}
