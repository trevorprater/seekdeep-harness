//! Complete pinned-source graph inputs through the native scoped projection.

use std::{path::Path, process::Command};

use seekdeep_repository_tools::doc_graphs::{render_source_doc_graphs, target_identity};
use seekdeep_typert_generator::{analyzer::run_with_stack, catalog::CordisCatalogModel};

#[test]
fn source_scoped_discovery_and_native_rendering_match_all_eight_source_artifacts() {
    let source = Path::new("/Users/trevor/ws/deepseek-harness");
    let script = r"
import { projectCordisCatalog } from '@deepseek-ai/dsh-typert-generator';
import { CORDIS_CATALOG_POLICY } from './scripts/gen-cordis-catalog.ts';
process.stdout.write(JSON.stringify(projectCordisCatalog(process.cwd(), CORDIS_CATALOG_POLICY).model));
";
    let output = Command::new("node")
        .args(["--import", "tsx", "--input-type=module", "-e", script])
        .current_dir(source)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let model: CordisCatalogModel = serde_json::from_slice(&output.stdout).unwrap();
    run_with_stack(move || {
        let docs = render_source_doc_graphs(source, source, &model).unwrap();
        assert_eq!(docs.len(), 8);
        for doc in docs {
            assert_eq!(
                doc.content,
                target_identity(&std::fs::read_to_string(source.join(&doc.rel)).unwrap()),
                "{}",
                doc.rel
            );
        }
    });
}
