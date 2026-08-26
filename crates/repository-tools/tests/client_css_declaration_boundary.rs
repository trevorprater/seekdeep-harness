//! Rust/WASM client CSS owns no TypeScript source declaration aggregate.

#[test]
fn compiled_client_tree_has_no_typescript_css_module_inputs() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut declarations = Vec::new();
    let mut foreign_sources = Vec::new();
    for family in ["packages/client", "packages/extensions"] {
        for entry in walkdir::WalkDir::new(root.join(family)) {
            let entry = entry.unwrap();
            if !entry.file_type().is_file() {
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if relative.ends_with("/src/css-modules.d.ts") {
                declarations.push(relative.clone());
            }
            let in_build_output = relative.contains("/lib/");
            let extension = entry.path().extension().and_then(std::ffi::OsStr::to_str);
            if !in_build_output && matches!(extension, Some("ts" | "tsx" | "mts" | "cts")) {
                foreign_sources.push(relative);
            }
        }
    }
    assert_eq!(declarations, Vec::<String>::new());
    assert_eq!(foreign_sources, Vec::<String>::new());

    let rust =
        std::fs::read_to_string(root.join("crates/client-ui-primitives/src/browser_blocks.rs"))
            .unwrap();
    assert!(rust.contains(
        "include_str!(\"../../../packages/client/ui-primitives/src/DiffBlock.module.css\")"
    ));
}
