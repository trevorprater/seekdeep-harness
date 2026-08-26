//! Importer and registry-copy coverage for vendored lockfile integrity.

use seekdeep_repository_tools::vendored_links::inspect_vendored_links;

#[test]
fn reports_non_link_importers_and_registry_package_snapshot_copies() {
    let root = tempfile::tempdir().unwrap();
    let vendor = root.path().join("vendor/cordis");
    std::fs::create_dir_all(&vendor).unwrap();
    std::fs::write(
        vendor.join("package.json"),
        r#"{"name":"@seekdeep-ai/cordis"}"#,
    )
    .unwrap();
    std::fs::write(
        root.path().join("pnpm-lock.yaml"),
        concat!(
            "importers:\n",
            "  app:\n",
            "    dependencies:\n",
            "      '@seekdeep-ai/cordis':\n",
            "        version: 1.2.3\n",
            "  linked:\n",
            "    dependencies:\n",
            "      '@seekdeep-ai/cordis':\n",
            "        version: link:../vendor/cordis\n",
            "packages:\n",
            "  '@seekdeep-ai/cordis@1.2.3': {}\n",
            "snapshots:\n",
            "  '@seekdeep-ai/cordis@1.2.3': {}\n",
        ),
    )
    .unwrap();
    let report = inspect_vendored_links(root.path()).unwrap();
    assert_eq!(report.vendored_packages, 1);
    assert_eq!(
        report.violations,
        [
            "app dependencies.@seekdeep-ai/cordis resolves to \"1.2.3\" (expected link:)",
            "packages entry @seekdeep-ai/cordis@1.2.3 is a registry copy of a vendored package",
            "snapshots entry @seekdeep-ai/cordis@1.2.3 is a registry copy of a vendored package",
        ]
    );
}
