//! Initial profile-boundary parity for `app-boot/profile.ts`.

use std::{fs, path::PathBuf};

use seekdeep_app_boot::{
    LoadProfileOptions, PROFILE_PATCH_FILENAME, PROFILE_TEMPLATES, compose_entries,
    heal_profiles_module_fallback, init_profile, load_optional_patches, load_profile,
    normalize_shipped_profile, read_profile_manifest, resolve_bundle_dir, resolve_profile_dir,
    write_profile_manifest,
};
use seekdeep_loader::profile_patch::{ProfileNode, parse_patch_list_yaml};
use serde_json::{Value, json};

#[test]
fn profile_names_are_validated_before_path_join() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(
        resolve_profile_dir("web", home.path()).unwrap(),
        home.path().join("profiles/web")
    );
    for invalid in ["", ".", "..", "node_modules", "a/b", "a\\b"] {
        let error = resolve_profile_dir(invalid, home.path()).unwrap_err();
        assert!(error.to_string().contains("invalid profile name"));
    }
}

#[test]
fn initialization_is_idempotent_and_never_rewrites_user_files() {
    let temporary = tempfile::tempdir().unwrap();
    let dir = temporary.path().join("profiles/headless");
    let bundles = PROFILE_TEMPLATES["headless"];
    init_profile(&dir, bundles).unwrap();
    let manifest = fs::read_to_string(dir.join("package.json")).unwrap();
    assert!(manifest.contains("seekdeep-profile-headless"));
    assert!(manifest.contains("@seekdeep-ai/seekdeep-headless"));
    assert!(manifest.ends_with('\n'));
    assert_eq!(
        fs::read_to_string(dir.join(PROFILE_PATCH_FILENAME)).unwrap(),
        concat!(
            "# Your patch layer for this seekdeep profile, applied after every bundle layer:\n",
            "# a top-level YAML array of loader patch entries (id-targeted config\n",
            "# overrides, disables, and insert lists; `!!js` expressions allowed).\n",
            "[]\n",
        )
    );
    fs::write(dir.join(PROFILE_PATCH_FILENAME), "# user owned\n[]\n").unwrap();
    init_profile(&dir, bundles).unwrap();
    assert_eq!(
        fs::read_to_string(dir.join(PROFILE_PATCH_FILENAME)).unwrap(),
        "# user owned\n[]\n"
    );
}

#[test]
fn manifest_round_trip_preserves_unrelated_fields_and_normalizes_only_owned_tuple() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(
        temporary.path().join("package.json"),
        serde_json::to_string_pretty(&json!({
            "name": "custom",
            "private": true,
            "future": {"keep": true},
            "seekdeep": {"profile": {"bundles": [
                "@seekdeep-ai/seekdeep-base",
                "@seekdeep-ai/seekdeep-web-app",
                "@seekdeep-ai/seekdeep-headless"
            ], "futureProfile": 1}, "futureSection": 2}
        }))
        .unwrap(),
    )
    .unwrap();
    let manifest = read_profile_manifest("seekdeep", temporary.path()).unwrap();
    let manifest = normalize_shipped_profile("headless", temporary.path(), manifest).unwrap();
    assert_eq!(manifest.extra["future"], json!({"keep": true}));
    assert_eq!(
        manifest.seekdeep.as_ref().unwrap().extra["futureSection"],
        2
    );
    assert_eq!(
        manifest
            .seekdeep
            .as_ref()
            .unwrap()
            .profile
            .as_ref()
            .unwrap()
            .bundles
            .as_ref()
            .unwrap(),
        &[
            "@seekdeep-ai/seekdeep-base",
            "@seekdeep-ai/seekdeep-headless"
        ]
    );
    write_profile_manifest(temporary.path(), &manifest).unwrap();
    assert!(
        fs::read_to_string(temporary.path().join("package.json"))
            .unwrap()
            .ends_with('\n')
    );

    fs::write(temporary.path().join("package.json"), "[]").unwrap();
    assert!(
        read_profile_manifest("seekdeep", temporary.path())
            .unwrap_err()
            .to_string()
            .contains("must hold a JSON object")
    );
    assert!(
        read_profile_manifest("seekdeep", &temporary.path().join("missing"))
            .unwrap_err()
            .to_string()
            .contains("failed to read profile manifest")
    );
}

#[test]
fn normalization_preserves_every_user_owned_headless_tuple() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(
        temporary.path().join("package.json"),
        serde_json::to_vec(&json!({
            "seekdeep": {"profile": {"bundles": [
                "@seekdeep-ai/seekdeep-base",
                "@seekdeep-ai/seekdeep-web-app",
                "@seekdeep-ai/seekdeep-headless",
                "custom-bundle"
            ]}}
        }))
        .unwrap(),
    )
    .unwrap();
    let manifest = read_profile_manifest("seekdeep", temporary.path()).unwrap();
    let normalized = normalize_shipped_profile("headless", temporary.path(), manifest).unwrap();
    assert_eq!(
        normalized
            .seekdeep
            .unwrap()
            .profile
            .unwrap()
            .bundles
            .unwrap()
            .last()
            .map(String::as_str),
        Some("custom-bundle")
    );
}

#[test]
fn optional_patch_loading_preserves_javascript_and_fails_loud_when_present_but_bad() {
    let temporary = tempfile::tempdir().unwrap();
    let missing = temporary.path().join("missing.yml");
    assert!(
        load_optional_patches("seekdeep", &missing)
            .unwrap()
            .is_none()
    );
    let path = temporary.path().join("patch.yml");
    fs::write(
        &path,
        "- id: agent-loop\n  config:\n    model: !!js process.env.SEEKDEEP_SPEC_MODEL\n",
    )
    .unwrap();
    let patches = load_optional_patches("seekdeep", &path)
        .unwrap()
        .expect("patches");
    let config = patches[0]
        .field("config")
        .and_then(ProfileNode::as_mapping)
        .unwrap();
    assert_eq!(
        config.get("model").and_then(ProfileNode::as_javascript),
        Some(&seekdeep_loader::profile_patch::JavaScriptExpression::new(
            "process.env.SEEKDEEP_SPEC_MODEL"
        ))
    );
    fs::write(&path, "invalid: [unclosed\n").unwrap();
    assert!(
        load_optional_patches("seekdeep", &path)
            .unwrap_err()
            .to_string()
            .starts_with("seekdeep: failed to parse patches")
    );
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(
        load_optional_patches("seekdeep", &path)
            .unwrap_err()
            .to_string()
            .starts_with("seekdeep: failed to read patches")
    );
}

#[test]
fn composition_applies_ordered_layers_over_an_empty_root() {
    let base = parse_patch_list_yaml(
        "- insert:\n    - id: row\n      name: plugin\n      config: {value: base}\n",
    )
    .unwrap();
    let user = parse_patch_list_yaml("- id: row\n  config: {value: user}\n").unwrap();
    let composed = compose_entries(&[base, user]).unwrap();
    assert_eq!(
        composed.entries()[0].config(),
        Some(&ProfileNode::Mapping(indexmap::indexmap! {
            "value".to_owned() => ProfileNode::String("user".to_owned()),
        }))
    );
    let missing = parse_patch_list_yaml("- id: missing\n  config: {}\n").unwrap();
    let warned = compose_entries(&[missing]).unwrap();
    assert_eq!(warned.entries(), []);
    assert!(warned.warnings()[0].to_string().contains("\"missing\""));
}

type BundleFixture<'a> = (&'a str, Option<&'a str>, &'a [(&'a str, &'a str)]);

fn stage_installation(bundles: &[BundleFixture<'_>]) -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let app = root.path().join("app");
    fs::create_dir_all(app.join("node_modules")).unwrap();
    let mut dependencies = serde_json::Map::new();
    for (name, patch, child_dependencies) in bundles {
        dependencies.insert((*name).to_owned(), json!("0.0.0"));
        let dir = app.join("node_modules").join(name);
        fs::create_dir_all(&dir).unwrap();
        let child_dependencies = child_dependencies
            .iter()
            .map(|(name, version)| ((*name).to_owned(), json!(version)))
            .collect::<serde_json::Map<_, _>>();
        let mut manifest = serde_json::Map::from_iter([
            ("name".to_owned(), json!(name)),
            ("version".to_owned(), json!("0.0.0")),
            ("dependencies".to_owned(), Value::Object(child_dependencies)),
        ]);
        if patch.is_some() {
            manifest.insert(
                "seekdeep".to_owned(),
                json!({"bundle": {"patch": "./cordis.patch.yml"}}),
            );
        }
        fs::write(
            dir.join("package.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        if let Some(patch) = patch {
            fs::write(dir.join("cordis.patch.yml"), patch).unwrap();
        }
    }
    let anchor = app.join("package.json");
    fs::write(
        &anchor,
        serde_json::to_vec(&json!({
            "name": "seekdeep-app",
            "dependencies": dependencies,
        }))
        .unwrap(),
    )
    .unwrap();
    (root, anchor)
}

#[test]
fn bundle_resolution_prefers_installation_then_profile_without_exports_assumptions() {
    let (_installation, anchor) = stage_installation(&[("in-box", Some("[]\n"), &[])]);
    let profile = tempfile::tempdir().unwrap();
    fs::write(profile.path().join("package.json"), "{}").unwrap();
    let local = profile.path().join("node_modules/local-only");
    fs::create_dir_all(&local).unwrap();
    fs::write(
        local.join("package.json"),
        r#"{"name":"local-only","exports":{".":"./index.js"}}"#,
    )
    .unwrap();
    assert!(
        resolve_bundle_dir("seekdeep", "in-box", &anchor, profile.path())
            .unwrap()
            .ends_with("in-box")
    );
    assert_eq!(
        resolve_bundle_dir("seekdeep", "local-only", &anchor, profile.path()).unwrap(),
        local
    );
    assert!(
        resolve_bundle_dir("seekdeep", "absent", &anchor, profile.path())
            .unwrap_err()
            .to_string()
            .contains("cannot resolve profile bundle")
    );
}

#[test]
fn load_profile_resolves_layers_and_optional_user_patch() {
    let (_installation, anchor) = stage_installation(&[
        (
            "bundle-a",
            Some("- insert:\n    - id: a\n      name: pkg-a\n"),
            &[],
        ),
        ("bundle-b", Some("- id: a\n  config:\n    v: 2\n"), &[]),
    ]);
    let home = tempfile::tempdir().unwrap();
    let dir = resolve_profile_dir("demo", home.path()).unwrap();
    init_profile(&dir, &["bundle-a", "bundle-b"]).unwrap();
    fs::write(
        dir.join(PROFILE_PATCH_FILENAME),
        "- id: a\n  config:\n    v: 3\n",
    )
    .unwrap();
    let profile = load_profile(
        "seekdeep",
        "demo",
        &anchor,
        home.path(),
        LoadProfileOptions::default(),
    )
    .unwrap();
    assert_eq!(
        profile
            .layers
            .iter()
            .map(|layer| layer.package_name.as_str())
            .collect::<Vec<_>>(),
        ["bundle-a", "bundle-b"]
    );
    let composition = compose_entries(
        &profile
            .layers
            .iter()
            .map(|layer| layer.patches.clone())
            .chain(std::iter::once(profile.patches.clone()))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    assert_eq!(
        composition.entries()[0].config(),
        Some(&ProfileNode::Mapping(indexmap::indexmap! {
            "v".to_owned() => ProfileNode::Number(serde_yml::Number::from(3)),
        }))
    );
    let bundles_only = load_profile(
        "seekdeep",
        "demo",
        &anchor,
        home.path(),
        LoadProfileOptions { user_layer: false },
    )
    .unwrap();
    assert!(bundles_only.patches.is_empty());
    fs::remove_file(dir.join(PROFILE_PATCH_FILENAME)).unwrap();
    assert!(
        load_profile(
            "seekdeep",
            "demo",
            &anchor,
            home.path(),
            LoadProfileOptions::default(),
        )
        .unwrap()
        .patches
        .is_empty()
    );
    let mut bare = read_profile_manifest("seekdeep", &dir).unwrap();
    bare.seekdeep = None;
    write_profile_manifest(&dir, &bare).unwrap();
    assert!(
        load_profile(
            "seekdeep",
            "demo",
            &anchor,
            home.path(),
            LoadProfileOptions::default(),
        )
        .unwrap()
        .layers
        .is_empty()
    );
}

#[test]
fn load_profile_rejects_a_listed_package_without_bundle_metadata() {
    let (_installation, anchor) = stage_installation(&[("not-a-bundle", None, &[])]);
    let home = tempfile::tempdir().unwrap();
    init_profile(
        &resolve_profile_dir("broken", home.path()).unwrap(),
        &["not-a-bundle"],
    )
    .unwrap();
    assert!(
        load_profile(
            "seekdeep",
            "broken",
            &anchor,
            home.path(),
            LoadProfileOptions::default(),
        )
        .unwrap_err()
        .to_string()
        .contains("declares no seekdeep.bundle")
    );
}

#[test]
fn load_profile_auto_initializes_only_shipped_names() {
    let (_installation, anchor) = stage_installation(&[]);
    let home = tempfile::tempdir().unwrap();
    assert!(
        load_profile(
            "seekdeep",
            "custom",
            &anchor,
            home.path(),
            LoadProfileOptions::default(),
        )
        .unwrap_err()
        .to_string()
        .contains("profile \"custom\" does not exist")
    );
    let _ = load_profile(
        "seekdeep",
        "web",
        &anchor,
        home.path(),
        LoadProfileOptions::default(),
    );
    assert_eq!(
        read_profile_manifest(
            "seekdeep",
            &resolve_profile_dir("web", home.path()).unwrap()
        )
        .unwrap()
        .seekdeep
        .unwrap()
        .profile
        .unwrap()
        .bundles
        .unwrap(),
        PROFILE_TEMPLATES["web"]
    );
}

#[cfg(unix)]
#[test]
fn fallback_healing_links_dependency_closure_is_idempotent_and_repairs_wrong_links() {
    let (_installation, anchor) = stage_installation(&[
        (
            "bundle-a",
            Some("[]\n"),
            &[("dep-of-a", "0.0.0"), ("ghost", "0.0.0")],
        ),
        ("plain-lib", None, &[]),
    ]);
    let modules = anchor.parent().unwrap().join("node_modules");
    let dependency = modules.join("dep-of-a");
    fs::create_dir_all(&dependency).unwrap();
    fs::write(
        dependency.join("package.json"),
        r#"{"name":"dep-of-a","version":"0.0.0"}"#,
    )
    .unwrap();
    let home = tempfile::tempdir().unwrap();
    heal_profiles_module_fallback(&anchor, home.path()).unwrap();
    let fallback = home.path().join("profiles/node_modules");
    for name in ["bundle-a", "plain-lib", "dep-of-a", "seekdeep-app"] {
        assert!(
            fs::symlink_metadata(fallback.join(name))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
    assert!(fs::symlink_metadata(fallback.join("ghost")).is_err());
    heal_profiles_module_fallback(&anchor, home.path()).unwrap();
    fs::remove_file(fallback.join("seekdeep-app")).unwrap();
    std::os::unix::fs::symlink(
        tempfile::tempdir().unwrap().path(),
        fallback.join("seekdeep-app"),
    )
    .unwrap();
    heal_profiles_module_fallback(&anchor, home.path()).unwrap();
    assert_eq!(
        fs::read_link(fallback.join("seekdeep-app")).unwrap(),
        anchor.parent().unwrap()
    );

    fs::remove_file(fallback.join("seekdeep-app")).unwrap();
    fs::create_dir(fallback.join("seekdeep-app")).unwrap();
    assert!(
        heal_profiles_module_fallback(&anchor, home.path())
            .unwrap_err()
            .to_string()
            .contains("is not a symlink")
    );
}
