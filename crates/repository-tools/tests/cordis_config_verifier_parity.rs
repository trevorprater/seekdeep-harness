//! Recursive metadata, dependency, execution-plane, and Rust ownership parity.

use std::path::Path;

use seekdeep_repository_tools::cordis_config_verifier::{
    CordisConfigReport, inspect_cordis_config, render_cordis_config_report,
};

struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let fixture = Self { root };
        fixture.write(
            "examples/demo/cordis.yml",
            "- id: demo\n  name: '@seekdeep-ai/seekdeep-demo'\n",
        );
        fixture.write(
            "examples/package.json",
            "{\"dependencies\":{\"@seekdeep-ai/seekdeep-demo\":\"workspace:^\"}}\n",
        );
        fixture.write("apps/cli/package.json", "{\"dependencies\":{}}\n");
        fixture.write(
            "packages/bundle/base/package.json",
            "{\"name\":\"@seekdeep-ai/seekdeep-base\",\"dependencies\":{}}\n",
        );
        fixture.write("packages/bundle/base/cordis.patch.yml", "[]\n");
        fixture.write(
            "packages/bundle/web-app/package.json",
            "{\"name\":\"@seekdeep-ai/seekdeep-web-app\",\"dependencies\":{}}\n",
        );
        fixture.write("packages/bundle/web-app/cordis.patch.yml", "[]\n");
        fixture.rust_owner("demo");
        fixture
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn write(&self, path: &str, content: &str) {
        let absolute = self.path().join(path);
        if let Some(parent) = absolute.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(absolute, content).unwrap();
    }

    fn rust_owner(&self, name: &str) {
        self.write(
            &format!("crates/{name}/Cargo.toml"),
            &format!("[package]\nname = \"seekdeep-{name}\"\nversion = \"0.0.0\"\n"),
        );
        self.write(&format!("crates/{name}/src/lib.rs"), "//! Fixture.\n");
    }

    fn report(&self) -> CordisConfigReport {
        inspect_cordis_config(self.path()).unwrap()
    }
}

#[test]
fn valid_repository_passes_with_target_renames_and_rust_ownership() {
    let fixture = Fixture::new();
    let report = fixture.report();
    assert_eq!(report.files, 3);
    assert_eq!(report.errors, Vec::<String>::new());
    assert_eq!(
        render_cordis_config_report(&report),
        "verify-cordis-config: 3 config files passed.\n"
    );
}

#[test]
fn root_entry_and_expression_failures_keep_exact_paths() {
    let fixture = Fixture::new();
    fixture.write("bad-root.cordis.yml", "id: not-a-list\n");
    fixture.write(
        "examples/metadata.cordis.yml",
        r"- 7
- id: !!js process.platform
  name: '@seekdeep-ai/seekdeep-demo'
- id: nested
  name: '@seekdeep-ai/seekdeep-demo'
  disabled:
    when: !!js process.platform
- id: invalid
  name: '@seekdeep-ai/seekdeep-demo'
  disabled: !!js process.platform ===
",
    );
    let report = fixture.report();
    assert!(
        report
            .errors
            .contains(&"bad-root.cordis.yml: root must be a Loader entry array".to_owned())
    );
    assert!(
        report
            .errors
            .contains(&"examples/metadata.cordis.yml[0]: entry must be an object".to_owned())
    );
    assert!(
        report.errors.contains(
            &"examples/metadata.cordis.yml[1].id: !!js is not interpolated here".to_owned()
        ),
        "{:#?}",
        report.errors
    );
    assert!(report.errors.contains(
        &"examples/metadata.cordis.yml[2].disabled.when: !!js is not interpolated here".to_owned()
    ));
    assert!(report.errors.iter().any(|error| {
        error.contains(
            "examples/metadata.cordis.yml[3].disabled: disabled expression does not parse",
        )
    }));
}

#[test]
fn group_insert_and_include_patch_references_are_recursive() {
    let fixture = Fixture::new();
    fixture.write(
        "examples/nested.cordis.yml",
        r"- id: group
  group: true
  config:
    - id: child
      name: '@seekdeep-ai/seekdeep-child'
- insert:
    - id: inserted
      name: '@seekdeep-ai/seekdeep-inserted'
- id: include
  name: '@seekdeep-ai/cordis-plugin-include'
  config:
    patches:
      - name: '@seekdeep-ai/seekdeep-patch'
        disabled:
          when: !!js never_runs
        insert:
          - id: patch-child
            name: '@seekdeep-ai/seekdeep-patch-child'
",
    );
    let report = fixture.report();
    for package in [
        "@seekdeep-ai/seekdeep-child",
        "@seekdeep-ai/seekdeep-inserted",
        "@seekdeep-ai/cordis-plugin-include",
        "@seekdeep-ai/seekdeep-patch",
        "@seekdeep-ai/seekdeep-patch-child",
    ] {
        assert!(report.errors.iter().any(|error| {
            error.contains(package) && error.contains("examples/package.json dependencies")
        }));
    }
    assert!(
        report.errors.contains(
            &"examples/nested.cordis.yml[2].config.patches[0].disabled.when: !!js is not interpolated here"
                .to_owned()
        ),
        "{:#?}",
        report.errors
    );
}

#[test]
fn chooser_runtime_packages_are_required_even_without_yaml_rows() {
    let fixture = Fixture::new();
    fixture.write(
        "examples/chooser.cordis.yml",
        "- id: chooser\n  name: '@seekdeep-ai/seekdeep-host-directory-picker-auto'\n",
    );
    fixture.write(
        "examples/package.json",
        "{\"dependencies\":{\"@seekdeep-ai/seekdeep-demo\":\"workspace:^\",\"@seekdeep-ai/seekdeep-host-directory-picker-auto\":\"workspace:^\"}}\n",
    );
    let report = fixture.report();
    for package in [
        "@seekdeep-ai/seekdeep-host-directory-picker-native",
        "@seekdeep-ai/seekdeep-host-directory-picker-browse",
        "@seekdeep-ai/seekdeep-client-ui-directory-picker-browse",
        "@seekdeep-ai/seekdeep-client-ui-directory-picker-native",
    ] {
        assert!(report.errors.iter().any(|error| error.contains(package)));
    }
}

#[test]
fn app_overlays_and_bundle_layers_use_their_own_dependency_planes() {
    let fixture = Fixture::new();
    fixture.write(
        "examples/web-cordis/cordis.yml",
        "- id: app-only\n  name: '@seekdeep-ai/seekdeep-app-only'\n",
    );
    fixture.rust_owner("app-only");
    let report = fixture.report();
    assert!(report.errors.iter().any(|error| {
        error.contains("examples/web-cordis/cordis.yml")
            && error.contains("apps/cli/package.json or a bundle manifest")
    }));

    fixture.write(
        "packages/bundle/base/cordis.patch.yml",
        "- insert:\n    - id: bundle-only\n      name: '@seekdeep-ai/seekdeep-bundle-only'\n",
    );
    fixture.rust_owner("bundle-only");
    let report = fixture.report();
    assert!(report.errors.iter().any(|error| {
        error.contains("@seekdeep-ai/seekdeep-bundle-only")
            && error.contains("packages/bundle/base/package.json dependencies")
    }));
}

#[test]
fn configured_local_packages_require_rust_source_ownership() {
    let fixture = Fixture::new();
    fixture.write(
        "examples/missing.cordis.yml",
        "- id: missing\n  name: '@seekdeep-ai/seekdeep-missing'\n",
    );
    fixture.write(
        "examples/package.json",
        "{\"dependencies\":{\"@seekdeep-ai/seekdeep-demo\":\"workspace:^\",\"@seekdeep-ai/seekdeep-missing\":\"workspace:^\"}}\n",
    );
    let report = fixture.report();
    assert!(report.errors.iter().any(|error| {
        error.contains("@seekdeep-ai/seekdeep-missing does not resolve to Rust source ownership")
    }));
    fixture.rust_owner("missing");
    assert!(!fixture.report().errors.iter().any(|error| {
        error.contains("@seekdeep-ai/seekdeep-missing does not resolve to Rust source ownership")
    }));
}

#[test]
fn host_and_preset_planes_may_not_repeat_an_active_row() {
    let fixture = Fixture::new();
    fixture.write(
        "packages/bundle/base/cordis.patch.yml",
        "- insert:\n    - id: repeated\n      name: '@seekdeep-ai/seekdeep-demo'\n",
    );
    fixture.write(
        "packages/bundle/base/package.json",
        "{\"name\":\"@seekdeep-ai/seekdeep-base\",\"dependencies\":{\"@seekdeep-ai/seekdeep-demo\":\"workspace:^\"}}\n",
    );
    fixture.write(
        "apps/cli/config/agent-presets/standard/agent.cordis.yml",
        "- id: repeated\n  name: '@seekdeep-ai/seekdeep-demo'\n",
    );
    assert!(fixture.report().errors.iter().any(|error| {
        error.contains("row \"repeated\" is also active in the host composition")
    }));
    fixture.write(
        "packages/bundle/web-app/cordis.patch.yml",
        "- id: repeated\n  disabled: true\n",
    );
    assert!(!fixture.report().errors.iter().any(|error| {
        error.contains("row \"repeated\" is also active in the host composition")
    }));
}

#[test]
fn client_export_and_seekdeep_client_declaration_agree_both_ways() {
    let fixture = Fixture::new();
    fixture.write(
        "packages/client/export-only/package.json",
        "{\"name\":\"@seekdeep-ai/seekdeep-export-only\",\"exports\":{\"./client\":\"./lib/client.js\"}}\n",
    );
    fixture.write(
        "packages/client/declaration-only/package.json",
        "{\"name\":\"@seekdeep-ai/seekdeep-declaration-only\",\"exports\":{},\"seekdeep\":{\"client\":{}}}\n",
    );
    let report = fixture.report();
    assert!(report.errors.contains(
        &"packages/client/export-only/package.json: exports \"./client\" but declares no seekdeep.client, so its browser half is never served"
            .to_owned()
    ));
    assert!(report.errors.contains(
        &"packages/client/declaration-only/package.json: declares seekdeep.client but exports no \"./client\" entry to serve"
            .to_owned()
    ));
}

#[test]
fn external_relative_and_url_plugins_do_not_create_package_requirements() {
    let fixture = Fixture::new();
    fixture.write(
        "examples/external.cordis.yml",
        "- id: relative\n  name: './plugin.mjs'\n- id: url\n  name: 'file:///plugin.mjs'\n",
    );
    let report = fixture.report();
    assert!(!report.errors.iter().any(|error| {
        error.contains("./plugin.mjs must be declared")
            || error.contains("file:///plugin.mjs must be declared")
    }));
}

#[test]
fn js_tag_normalization_excludes_quotes_comments_and_block_scalars() {
    let fixture = Fixture::new();
    fixture.write(
        "examples/literals.cordis.yml",
        r#"- id: literals
  name: '@seekdeep-ai/seekdeep-demo'
  inject:
    - "!!js 中文"
    - '!!js 中文'
    - |
      !!js 中文
  disabled: !!js process.platform === 'win32'
  # !!js comment
"#,
    );
    let report = fixture.report();
    assert!(!report.errors.iter().any(|error| {
        error.contains("examples/literals.cordis.yml") && error.contains("!!js is not interpolated")
    }));
}

#[test]
fn npm_identities_may_map_to_differently_named_cargo_owners() {
    let fixture = Fixture::new();
    fixture.write(
        "examples/aliases.cordis.yml",
        "- id: logger\n  name: '@seekdeep-ai/cordis-plugin-logger-console'\n- id: timeout\n  name: '@seekdeep-ai/seekdeep-tool-call-timeout-policy'\n",
    );
    fixture.write(
        "examples/package.json",
        "{\"dependencies\":{\"@seekdeep-ai/seekdeep-demo\":\"workspace:^\",\"@seekdeep-ai/cordis-plugin-logger-console\":\"workspace:^\",\"@seekdeep-ai/seekdeep-tool-call-timeout-policy\":\"workspace:^\"}}\n",
    );
    fixture.rust_owner("logger-console");
    fixture.rust_owner("tool-timeout-policy");
    let report = fixture.report();
    assert!(!report.errors.iter().any(|error| {
        error.contains("cordis-plugin-logger-console does not resolve")
            || error.contains("seekdeep-tool-call-timeout-policy does not resolve")
    }));
}
