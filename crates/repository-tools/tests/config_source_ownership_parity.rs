//! Source-oracle coverage for forbidden shipped configuration inlines.

use seekdeep_repository_tools::config_source_ownership::collect_config_source_ownership_violations;

#[test]
fn rejects_inline_endpoints_in_shipped_bundle_patches() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("packages/bundle/base");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("cordis.patch.yml"),
        "config:\n  baseURL: !!js process.env.DEEPSEEK_SEARCH_BASE_URL\n",
    )
    .unwrap();
    assert_eq!(
        collect_config_source_ownership_violations(root.path()).unwrap(),
        [
            "packages/bundle/base/cordis.patch.yml:2: inlines a credential or endpoint from the environment. The adapter resolves apiKeyEnv through ctx.credentials and the endpoint through the environment snapshot; inlining here bypasses both ladders."
        ]
    );
}
