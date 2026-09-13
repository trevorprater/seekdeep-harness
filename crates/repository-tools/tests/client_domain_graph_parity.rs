//! Contract, same-domain, assembly, sibling, top-level, legacy, and import-form fixtures.

use seekdeep_repository_tools::client_domain_graph::inspect_client_domain_graph;
use tempfile::TempDir;

fn source(root: &TempDir, relative: &str, content: &str) {
    let path = root
        .path()
        .join("packages/client/probe/src/client")
        .join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

#[test]
fn contract_same_domain_top_level_and_outside_imports_are_allowed() {
    let root = tempfile::tempdir().unwrap();
    source(
        &root,
        "alpha/value.ts",
        "import { c } from '../contract/api.ts'\nimport { x } from './nested.ts'\nimport { shared } from '../shared.ts'\nimport { outside } from '../../../../outside.ts'\n",
    );
    assert!(inspect_client_domain_graph(root.path()).unwrap().is_empty());
}

#[test]
fn sibling_and_top_level_nonassembly_imports_report_exact_reasons() {
    let root = tempfile::tempdir().unwrap();
    source(
        &root,
        "alpha/value.ts",
        "import { b } from '../beta/value.ts'\n",
    );
    source(
        &root,
        "service.ts",
        "import { a } from './alpha/value.ts'\n",
    );
    let violations = inspect_client_domain_graph(root.path()).unwrap();
    assert_eq!(violations.len(), 2);
    assert_eq!(violations[0].file, "probe/src/client/alpha/value.ts");
    assert_eq!(violations[0].imported, "../beta/value.ts");
    assert_eq!(
        violations[0].reason,
        "domain \"alpha\" imports sibling domain \"beta\" (route shared API through contract/)"
    );
    assert_eq!(violations[1].file, "probe/src/client/service.ts");
    assert_eq!(
        violations[1].reason,
        "top-level non-assembly file imports domain \"alpha\" (only apply/index may assemble)"
    );
}

#[test]
fn apply_and_index_files_may_assemble_domains() {
    let root = tempfile::tempdir().unwrap();
    source(&root, "apply.ts", "import { a } from './alpha/value.ts'\n");
    source(&root, "index.tsx", "export { b } from './beta/value.ts'\n");
    assert!(inspect_client_domain_graph(root.path()).unwrap().is_empty());
}

#[test]
fn legacy_files_and_non_from_import_forms_are_outside_the_gate() {
    let root = tempfile::tempdir().unwrap();
    source(
        &root,
        "alpha/value.legacy.ts",
        "import { b } from '../beta/value.ts'\n",
    );
    source(
        &root,
        "alpha/value.ts",
        "import '../beta/effect.ts'\nconst lazy = import('../beta/lazy.ts')\n",
    );
    assert!(inspect_client_domain_graph(root.path()).unwrap().is_empty());
}

#[test]
fn live_rust_target_has_no_foreign_client_domain_violation() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    assert!(inspect_client_domain_graph(&root).unwrap().is_empty());
}
