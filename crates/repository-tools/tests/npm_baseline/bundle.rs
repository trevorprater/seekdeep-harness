use std::path::Path;

use seekdeep_repository_tools::npm_baseline::{
    BaselineCommit, BaselinePackage, BaselinePackageName, BaselineVersion, PackageOrigin,
    ReleaseBundle, ReleaseManifest, WorkspacePackageSet,
};
use serde_json::Value;

use super::support::{NpmFixtureRunner, source_call, tarball, workspace, write_json};

pub(super) fn metadata() -> ReleaseManifest {
    ReleaseManifest {
        schema_version: 1,
        commit: BaselineCommit::new("abcdef0123456789abcdef0123456789abcdef0123"),
        version: BaselineVersion::new("1.2.3-20260911010203-abcdef0123"),
        dist_tag: "dev-1.2.3".to_owned(),
        registry: "http://127.0.0.1:9".to_owned(),
        packages: Vec::new(),
    }
}

pub(super) fn package(name: &str, origin: PackageOrigin) -> BaselinePackage {
    BaselinePackage {
        name: BaselinePackageName::new(name),
        directory: "unused".into(),
        origin,
    }
}

pub(super) fn bundle(directory: &Path) -> ReleaseBundle {
    let manifest = metadata();
    let packages = vec![
        package("@seekdeep-ai/seekdeep", PackageOrigin::Harness),
        package("@seekdeep-ai/vendor", PackageOrigin::Vendor),
    ];
    tarball(
        directory,
        "entry.tgz",
        &serde_json::json!({"name":"@seekdeep-ai/seekdeep","version":manifest.version, "dependencies":{"@seekdeep-ai/vendor":manifest.version}}),
        &[("lib/bin.js", "console.log('fixture');\n")],
    );
    tarball(
        directory,
        "vendor.tgz",
        &serde_json::json!({"name":"@seekdeep-ai/vendor","version":manifest.version}),
        &[("src/index.ts", "export const value = 42;\n")],
    );
    ReleaseBundle::create(
        directory,
        &packages,
        manifest,
        &mut NpmFixtureRunner::default(),
    )
    .unwrap()
}

#[test]
fn tarball_membership_payload_pins_and_manifest_bytes_match_source() {
    let directory = tempfile::tempdir().unwrap();
    let bundle = bundle(directory.path());
    let expected = serde_json::to_value(&bundle.manifest).unwrap();
    let create = source_call(&serde_json::json!({
        "op":"create", "directory":directory.path(),
        "packages":[package("@seekdeep-ai/seekdeep", PackageOrigin::Harness), package("@seekdeep-ai/vendor", PackageOrigin::Vendor)],
        "commit":bundle.manifest.commit, "version":bundle.manifest.version, "distTag":bundle.manifest.dist_tag, "registry":bundle.manifest.registry,
    }));
    assert_eq!(create["value"], expected);
    let persisted = std::fs::read_to_string(directory.path().join("manifest.json")).unwrap();
    assert_eq!(
        persisted,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&bundle.manifest).unwrap()
        )
    );
    let sums = std::fs::read_to_string(directory.path().join("SHA256SUMS")).unwrap();
    assert_eq!(sums.lines().count(), 2);
    for package in &bundle.manifest.packages {
        assert!(sums.contains(&format!("{}  {}\n", package.sha256, package.tarball)));
    }
    let loaded = ReleaseBundle::load(
        &directory.path().join("manifest.json"),
        &mut NpmFixtureRunner::default(),
    )
    .unwrap();
    assert_eq!(loaded, bundle);
    assert_eq!(
        source_call(
            &serde_json::json!({"op":"load", "path":directory.path().join("manifest.json")})
        )["value"],
        expected
    );
}

#[test]
fn source_and_rust_reject_corrupt_packed_artifacts_before_manifest_publication() {
    let mut cases = Vec::new();
    let name = "@seekdeep-ai/seekdeep";
    let version = metadata().version;
    let base = serde_json::json!({"name":name,"version":version});
    cases.push((
        base.clone(),
        vec![("src/index.ts", "source")],
        "publishes source file",
    ));
    cases.push((
        base.clone(),
        vec![("lib/index.js.map", "map")],
        "publishes source map",
    ));
    let mut private = base.clone();
    private["private"] = true.into();
    cases.push((private, vec![], "is still private"));
    let mut bad_version = base.clone();
    bad_version["version"] = "2.0.0".into();
    cases.push((bad_version, vec![], "has version"));
    let mut nested_workspace = base.clone();
    nested_workspace["custom"] = serde_json::json!([{"deep":"workspace:*"}]);
    cases.push((nested_workspace, vec![], "still contains a workspace:"));
    let mut range = base.clone();
    range["peerDependencies"] = serde_json::json!({name:"^1.0.0"});
    cases.push((range, vec![], "has internal peerDependencies"));
    let mut section = base;
    section["optionalDependencies"] = Value::Null;
    cases.push((section, vec![], "optionalDependencies must be an object"));
    for (value, files, fragment) in cases {
        let directory = tempfile::tempdir().unwrap();
        tarball(directory.path(), "entry.tgz", &value, &files);
        let packages = vec![package(name, PackageOrigin::Harness)];
        let manifest = metadata();
        let error = ReleaseBundle::create(
            directory.path(),
            &packages,
            manifest.clone(),
            &mut NpmFixtureRunner::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains(fragment), "{error}");
        assert!(!directory.path().join("manifest.json").exists());
        let oracle = source_call(
            &serde_json::json!({"op":"create","directory":directory.path(),"packages":packages,"commit":manifest.commit,"version":manifest.version,"distTag":manifest.dist_tag,"registry":manifest.registry}),
        );
        assert_eq!(oracle["error"], error);
    }
}

#[test]
fn duplicate_unexpected_and_missing_tarballs_match_source() {
    for mode in ["missing", "unexpected", "duplicate"] {
        let directory = tempfile::tempdir().unwrap();
        let manifest = metadata();
        let packages = vec![package("@seekdeep-ai/seekdeep", PackageOrigin::Harness)];
        if mode != "missing" {
            tarball(
                directory.path(),
                "a.tgz",
                &serde_json::json!({"name":if mode == "unexpected" {"@seekdeep-ai/unknown"}else{"@seekdeep-ai/seekdeep"},"version":manifest.version}),
                &[],
            );
        }
        if mode == "duplicate" {
            std::fs::copy(
                directory.path().join("a.tgz"),
                directory.path().join("b.tgz"),
            )
            .unwrap();
        }
        let error = ReleaseBundle::create(
            directory.path(),
            &packages,
            manifest.clone(),
            &mut NpmFixtureRunner::default(),
        )
        .unwrap_err()
        .to_string();
        let oracle = source_call(
            &serde_json::json!({"op":"create","directory":directory.path(),"packages":packages,"commit":manifest.commit,"version":manifest.version,"distTag":manifest.dist_tag,"registry":manifest.registry}),
        );
        assert_eq!(oracle["error"], error);
    }
}

#[test]
fn persisted_manifest_validates_legacy_origin_paths_duplicates_and_both_checksums() {
    let directory = tempfile::tempdir().unwrap();
    let bundle = bundle(directory.path());
    let baseline = serde_json::to_value(&bundle.manifest).unwrap();
    let path = directory.path().join("manifest.json");
    let mut cases = Vec::new();
    let mut schema = baseline.clone();
    schema["schemaVersion"] = 2.into();
    cases.push(schema);
    let mut missing_schema = baseline.clone();
    missing_schema
        .as_object_mut()
        .unwrap()
        .shift_remove("schemaVersion");
    cases.push(missing_schema);
    let mut empty = baseline.clone();
    empty["packages"] = serde_json::json!([]);
    cases.push(empty);
    let mut duplicate = baseline.clone();
    duplicate["packages"]
        .as_array_mut()
        .unwrap()
        .push(baseline["packages"][0].clone());
    cases.push(duplicate);
    for invalid_path in [
        "../entry.tgz",
        "/tmp/entry.tgz",
        "./entry.tgz",
        "nested/../entry.tgz",
        "entry.tgz//",
    ] {
        let mut invalid = baseline.clone();
        invalid["packages"][0]["tarball"] = invalid_path.into();
        cases.push(invalid);
    }
    for key in ["sha256", "integrity"] {
        let mut bad = baseline.clone();
        bad["packages"][0][key] = "incorrect".into();
        cases.push(bad);
    }
    let mut origin = baseline.clone();
    origin["packages"][0]["origin"] = "foreign".into();
    cases.push(origin);
    let mut name = baseline.clone();
    name["packages"][0]["name"] = "outside-scope".into();
    cases.push(name);
    let mut identity = baseline.clone();
    identity["version"] = "2.0.0".into();
    cases.push(identity);
    for value in cases {
        write_json(&path, &value);
        let error = ReleaseBundle::load(&path, &mut NpmFixtureRunner::default())
            .unwrap_err()
            .to_string();
        assert_eq!(
            source_call(&serde_json::json!({"op":"load","path":path}))["error"],
            error
        );
    }
    let mut legacy = baseline;
    legacy["packages"][0]["tarball"] = "entry.tgz/".into();
    legacy["schemaVersion"] = 1.0.into();
    legacy["packages"][0]
        .as_object_mut()
        .unwrap()
        .shift_remove("origin");
    legacy["registry"] = "http://127.0.0.1:9///".into();
    write_json(&path, &legacy);
    let loaded = ReleaseBundle::load(&path, &mut NpmFixtureRunner::default()).unwrap();
    assert_eq!(loaded.manifest.registry, "http://127.0.0.1:9");
    assert_eq!(loaded.manifest.packages[0].origin, PackageOrigin::Harness);
    assert_eq!(
        source_call(&serde_json::json!({"op":"load","path":path}))["value"],
        serde_json::to_value(loaded.manifest).unwrap()
    );
}

#[test]
fn staging_pins_all_four_sections_and_preserves_external_ranges_like_source() {
    let rust = workspace();
    let source = workspace();
    for root in [rust.path(), source.path()] {
        let path = root.join("apps/cli/package.json");
        let mut manifest: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        manifest["dependencies"]["external"] = "^3.1.4".into();
        write_json(&path, &manifest);
    }
    let set = WorkspacePackageSet::discover(rust.path()).unwrap();
    let version = metadata().version;
    set.stage(rust.path(), &version).unwrap();
    let value = set.packages.iter().map(|package| serde_json::json!({"directory":package.directory,"manifest":serde_json::from_str::<Value>(&std::fs::read_to_string(rust.path().join(&package.directory).join("package.json")).unwrap()).unwrap()})).collect::<Vec<_>>();
    assert_eq!(
        source_call(&serde_json::json!({"op":"stage","root":source.path(),"version":version}))["value"],
        serde_json::json!(value)
    );
    for package in &set.packages {
        let value: Value = serde_json::from_str(
            &std::fs::read_to_string(rust.path().join(&package.directory).join("package.json"))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(value["version"], version.as_str());
        assert!(value.get("private").is_none());
    }
}
