//! Publication view, export failures, concurrency bounds, and stable order parity.

use std::{
    collections::{BTreeMap, HashSet},
    ffi::OsString,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use seekdeep_repository_tools::publint_all::{
    PublintPackage, PublintResult, PublintStatus, publication_files, publint_concurrency,
    render_publint_stderr, render_publint_stdout, run_all, workspace_packages,
};

struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new(export: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let fixture = Self { root };
        fixture.package("core/probe", "@seekdeep-ai/seekdeep-probe", export);
        fixture
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn write(&self, path: &str, content: &str) {
        let path = self.path().join(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    fn package(&self, path: &str, name: &str, export: &str) {
        self.write(
            &format!("packages/{path}/package.json"),
            &format!(
                "{{\n  \"name\": {name:?},\n  \"version\": \"0.0.1\",\n  \"type\": \"module\",\n  \"license\": \"MIT\",\n  \"files\": [\"lib\"],\n  \"exports\": {{\".\": {{\"default\": {export:?}}}}}\n}}\n"
            ),
        );
        self.write(&format!("packages/{path}/README.md"), "# Probe\n");
        self.write(
            &format!("packages/{path}/lib/index.js"),
            "export const probe = true\n",
        );
        self.write(
            &format!("packages/{path}/unpublished.js"),
            "export const hidden = true\n",
        );
    }

    fn packages(&self) -> Vec<PublintPackage> {
        workspace_packages(self.path()).unwrap()
    }
}

fn validate_export(target: PublintPackage) -> PublintResult {
    let files = publication_files(&target).unwrap();
    let names = files
        .iter()
        .map(|file| file.name.as_str())
        .collect::<HashSet<_>>();
    let export = target.manifest["exports"]["."]["default"].as_str().unwrap();
    let published = format!("package/{}", export.trim_start_matches("./"));
    let passed = names.contains(published.as_str());
    PublintResult {
        path: target.path,
        status: if passed {
            PublintStatus::Passed
        } else {
            PublintStatus::Failed
        },
        output: if passed {
            String::new()
        } else {
            format!("{export} is not published\n")
        },
        error_output: String::new(),
        failure: None,
    }
}

#[test]
fn lints_recursively_declared_files_from_exact_publication_view() {
    let fixture = Fixture::new("./lib/index.js");
    let target = fixture.packages().remove(0);
    let files = publication_files(&target).unwrap();
    let names = files
        .iter()
        .map(|file| file.name.as_str())
        .collect::<Vec<_>>();
    assert!(names.contains(&"package/package.json"));
    assert!(names.contains(&"package/README.md"));
    assert!(names.contains(&"package/lib/index.js"));
    assert!(!names.contains(&"package/unpublished.js"));
    let result = run_all(vec![target], 1, validate_export).unwrap();
    assert_eq!(result[0].status, PublintStatus::Passed);
}

#[test]
fn rejects_workspace_files_excluded_from_publication_and_missing_built_exports() {
    for export in ["./unpublished.js", "./lib/missing.js"] {
        let fixture = Fixture::new(export);
        let result = run_all(fixture.packages(), 1, validate_export).unwrap();
        assert_eq!(result[0].status, PublintStatus::Failed);
        assert!(result[0].output.contains(export));
    }
}

#[test]
fn explicit_directories_include_dotfiles_but_globs_exclude_dot_segments() {
    let fixture = Fixture::new("./lib/index.js");
    fixture.write("packages/core/probe/lib/.hidden/value.js", "export {}\n");
    fixture.write(
        "packages/core/probe/lib/types/.hidden/value.d.ts",
        "export {}\n",
    );
    let manifest = fixture.path().join("packages/core/probe/package.json");
    let mut value =
        serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&manifest).unwrap())
            .unwrap();
    value["files"] = serde_json::json!(["lib", "lib/types/**/*.d.ts"]);
    std::fs::write(
        &manifest,
        format!("{}\n", serde_json::to_string_pretty(&value).unwrap()),
    )
    .unwrap();
    let files = publication_files(&fixture.packages().remove(0)).unwrap();
    let names = files
        .iter()
        .map(|file| file.name.as_str())
        .collect::<Vec<_>>();
    assert!(names.contains(&"package/lib/.hidden/value.js"));
    assert!(names.contains(&"package/lib/types/.hidden/value.d.ts"));
    value["files"] = serde_json::json!(["lib/types/**/*.d.ts"]);
    std::fs::write(
        &manifest,
        format!("{}\n", serde_json::to_string_pretty(&value).unwrap()),
    )
    .unwrap();
    let files = publication_files(&fixture.packages().remove(0)).unwrap();
    assert!(
        !files
            .iter()
            .any(|file| file.name == "package/lib/types/.hidden/value.d.ts")
    );
}

#[test]
fn package_discovery_and_parallel_results_remain_sorted() {
    let fixture = Fixture::new("./lib/index.js");
    fixture.package("api/alpha", "@seekdeep-ai/seekdeep-alpha", "./lib/index.js");
    fixture.package("zeta/last", "@seekdeep-ai/seekdeep-last", "./lib/index.js");
    let packages = fixture.packages();
    assert_eq!(
        packages
            .iter()
            .map(|package| package.path.as_str())
            .collect::<Vec<_>>(),
        [
            "packages/api/alpha",
            "packages/core/probe",
            "packages/zeta/last"
        ]
    );
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let active_for_runner = Arc::clone(&active);
    let peak_for_runner = Arc::clone(&peak);
    let results = run_all(packages, 2, move |target| {
        let current = active_for_runner.fetch_add(1, Ordering::SeqCst) + 1;
        peak_for_runner.fetch_max(current, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(10));
        active_for_runner.fetch_sub(1, Ordering::SeqCst);
        validate_export(target)
    })
    .unwrap();
    assert_eq!(peak.load(Ordering::SeqCst), 2);
    assert_eq!(
        results
            .iter()
            .map(|result| result.path.as_str())
            .collect::<Vec<_>>(),
        [
            "packages/api/alpha",
            "packages/core/probe",
            "packages/zeta/last"
        ]
    );
}

#[test]
fn concurrency_override_is_canonical_bounded_and_zero_safe() {
    let mut environment = BTreeMap::<OsString, OsString>::new();
    assert_eq!(publint_concurrency(0, &environment, 8).unwrap(), 0);
    assert_eq!(publint_concurrency(5, &environment, 3).unwrap(), 3);
    environment.insert("SEEKDEEP_PUBLINT_CONCURRENCY".into(), "9".into());
    assert_eq!(publint_concurrency(5, &environment, 3).unwrap(), 5);
    for invalid in ["0", "01", "2x", "-1"] {
        environment.insert("SEEKDEEP_PUBLINT_CONCURRENCY".into(), invalid.into());
        assert!(
            publint_concurrency(5, &environment, 3)
                .unwrap_err()
                .to_string()
                .contains("must be a positive integer")
        );
    }
}

#[test]
fn result_blocks_keep_headers_success_and_failure_streams_attributable() {
    let passed = PublintResult {
        path: "packages/core/probe".to_owned(),
        status: PublintStatus::Passed,
        output: String::new(),
        error_output: String::new(),
        failure: None,
    };
    assert_eq!(
        render_publint_stdout(&passed),
        "Running publint for packages/core/probe...\nAll good!\n"
    );
    assert_eq!(render_publint_stderr(&passed), "");
    let failed = PublintResult {
        status: PublintStatus::Failed,
        output: "export is unpublished\n".to_owned(),
        error_output: "engine stderr\n".to_owned(),
        failure: Some("runner failed".to_owned()),
        ..passed
    };
    assert!(render_publint_stdout(&failed).contains("export is unpublished"));
    assert_eq!(
        render_publint_stderr(&failed),
        "runner failed\nengine stderr\n"
    );
}
