//! The instrumented coverage lane: argument shaping, the measured set, export translation
//! through the source reporter, the adoption roster, and the public entry end to end.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::Path,
    process::{Command, Output, Stdio},
};

use seekdeep_repository_tools::{
    coverage_exempt::INSTRUMENTED_LANE_EXCLUDED_PACKAGES,
    coverage_uncovered_locations::{
        ADOPTION_REASON, Host, MeasuredSet, Metric, ROSTER_NOTE, ROSTER_PATH, Roster, RosterEntry,
        evaluate, instrumented_exclusions, parse_arguments, regenerate_roster, report_command,
        roster_additions, test_command, translate_export,
    },
};
use serde_json::{Value, json};

fn strings(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

#[test]
fn arguments_map_the_gate_worker_budget_and_the_lane_flags() {
    let parsed = parse_arguments(&strings(&[
        "--maxWorkers=6",
        "-p",
        "seekdeep-util",
        "--",
        "--nocapture",
    ]))
    .unwrap();
    assert_eq!(parsed.workers.map(usize::from), Some(6));
    assert_eq!(parsed.cargo, strings(&["-p", "seekdeep-util"]));
    assert_eq!(parsed.harness, strings(&["--nocapture"]));
    let mut expected = strings(&[
        "llvm-cov",
        "--no-report",
        "--locked",
        "--workspace",
        "--all-features",
        "--no-fail-fast",
    ]);
    for package in INSTRUMENTED_LANE_EXCLUDED_PACKAGES {
        expected.extend(strings(&["--exclude-from-test", package]));
    }
    expected.extend(strings(&[
        "-p",
        "seekdeep-util",
        "--",
        "--test-threads=6",
        "--nocapture",
    ]));
    assert_eq!(
        test_command(&parsed, INSTRUMENTED_LANE_EXCLUDED_PACKAGES),
        expected
    );
    let bare = parse_arguments(&[]).unwrap();
    assert!(!test_command(&bare, &[]).contains(&OsString::from("--")));
    let lane = parse_arguments(&strings(&[
        "--from-json",
        "export.json",
        "--write-roster",
        "--repository=fixture",
        "--maxWorkers",
        "2",
    ]))
    .unwrap();
    assert_eq!(lane.from_json.as_deref(), Some(Path::new("export.json")));
    assert!(lane.write_roster);
    assert_eq!(lane.repository.as_deref(), Some(Path::new("fixture")));
    assert_eq!(lane.workers.map(usize::from), Some(2));
    assert!(parse_arguments(&strings(&["--maxWorkers=0"])).is_err());
    assert!(parse_arguments(&strings(&["--from-json"])).is_err());
    assert_eq!(
        report_command(Path::new("out.json")),
        strings(&["llvm-cov", "report", "--json", "--output-path", "out.json"])
    );
}

#[test]
fn the_instrumented_run_leaves_unmeasured_and_exempt_packages_to_the_other_lanes() {
    let repository = Path::new("/repo");
    let members = BTreeMap::from([
        ("seekdeep-util".to_owned(), repository.join("crates/util")),
        (
            "seekdeep-change-scope".to_owned(),
            repository.join("crates/change-scope"),
        ),
        ("xtask".to_owned(), repository.join("xtask")),
        (
            "seekdeep-landlock-run".to_owned(),
            repository.join("native/landlock-run"),
        ),
        ("seekdeep-lsp".to_owned(), repository.join("crates/lsp")),
    ]);
    let measured = MeasuredSet::of(&["crates/util", "crates/lsp", "crates/change-scope"]);
    assert_eq!(
        instrumented_exclusions(&members, repository, &measured),
        ["seekdeep-change-scope", "seekdeep-landlock-run", "xtask"]
    );
    assert!(instrumented_exclusions(&members, Path::new("/elsewhere"), &measured).len() == 5);
}

#[test]
fn the_measured_set_follows_verified_package_sources_and_skips_entry_points() {
    let manifest = json!({"surfaces": [
        {"source": "packages/core/util/src/index.ts", "status": "verified",
         "targets": ["crates/util/src/lib.rs", "crates/util/tests/lib_parity.rs"]},
        {"source": "packages/core/pending/src/index.ts", "status": "pending",
         "targets": ["crates/pending/src/lib.rs"]},
        {"source": "apps/cli/src/main.ts", "status": "verified", "targets": ["crates/cli/src/main.rs"]},
        {"source": "packages/core/tests/tests/spec.ts", "status": "verified",
         "targets": ["crates/tests-only/src/lib.rs"]},
        {"source": "scripts/tool.ts", "status": "verified", "targets": ["crates/tooling/src/lib.rs"]},
    ]});
    let measured = MeasuredSet::from_manifest(&manifest).unwrap();
    assert_eq!(measured.crates().collect::<Vec<_>>(), ["crates/util"]);
    assert!(measured.measures("crates/util/src/lib.rs"));
    assert!(measured.measures("crates/util/src/nested/deep.rs"));
    assert!(!measured.measures("crates/util/src/main.rs"));
    assert!(!measured.measures("crates/util/src/bin/tool.rs"));
    assert!(!measured.measures("crates/util/tests/lib_parity.rs"));
    assert!(!measured.measures("crates/pending/src/lib.rs"));
    assert!(!measured.measures("crates/cli/src/main.rs"));
    assert!(!measured.measures("xtask/src/main.rs"));
    assert!(MeasuredSet::from_manifest(&json!({})).is_err());
    assert_eq!(
        MeasuredSet::of(&["crates/b", "crates/a"])
            .crates()
            .collect::<Vec<_>>(),
        ["crates/a", "crates/b"]
    );
}

fn summary(lines: (u64, u64), functions: (u64, u64), regions: (u64, u64)) -> Value {
    let zero = json!({"count": 0, "covered": 0, "notcovered": 0, "percent": 0.0});
    json!({
        "branches": zero,
        "mcdc": zero,
        "functions": {"count": functions.0, "covered": functions.1, "percent": 0.0},
        "instantiations": {"count": 0, "covered": 0, "percent": 0.0},
        "lines": {"count": lines.0, "covered": lines.1, "percent": 0.0},
        "regions": {"count": regions.0, "covered": regions.1, "notcovered": regions.0 - regions.1, "percent": 0.0},
    })
}

fn region(span: (u64, u64, u64, u64), count: u64) -> Value {
    json!([span.0, span.1, span.2, span.3, count, 0, 0, 0])
}

/// An `llvm-cov export` document shaped like a real one: two test binaries instantiated the
/// fixture crate, so every function appears twice with counts that only merge to the truth.
fn export(root: &Path) -> Value {
    let lib = root.join("crates/covfix/src/lib.rs");
    let nested = root.join("crates/covfix/src/nested.rs");
    let test = root.join("crates/covfix/tests/smoke.rs");
    let file = |path: &Path, summary: Value| {
        json!({"filename": path, "branches": [], "expansions": [], "mcdc_records": [],
               "segments": [], "summary": summary})
    };
    let function = |name: &str, count: u64, path: &Path, regions: Vec<Value>| json!({"name": name, "count": count, "filenames": [path], "branches": [], "regions": regions});
    let branch = |count: u64| {
        vec![
            region((4, 1, 4, 34), count),
            region((5, 8, 5, 13), count),
            region((6, 9, 6, 10), count),
            region((8, 9, 8, 10), 0),
        ]
    };
    let untouched = || {
        vec![
            region((13, 1, 13, 38), 0),
            region((14, 5, 14, 9), 0),
            region((14, 10, 14, 13), 0),
        ]
    };
    let covered = |count: u64| {
        vec![
            region((2, 1, 2, 23), count),
            region((3, 5, 3, 6), count),
            region((4, 1, 4, 2), count),
        ]
    };
    json!({
        "type": "llvm.coverage.json.export",
        "version": "3.0.1",
        "data": [{
            "files": [
                file(&lib, summary((8, 4), (2, 1), (9, 4))),
                file(&nested, summary((3, 3), (1, 1), (3, 3))),
                file(&test, summary((3, 3), (1, 1), (3, 3))),
            ],
            "functions": [
                function("_RNvCs3upjwVOrb6m_6covfix6branch", 0, &lib, branch(0)),
                function("_RNvCs3upjwVOrb6m_6covfix9untouched", 0, &lib, untouched()),
                function("_RNvNtCs3upjwVOrb6m_6covfix6nested7covered", 0, &nested, covered(0)),
                function("_RNvCscMwAytmYVXk_5smokes_20hits_selected_branch", 1, &test,
                         vec![region((2, 1, 2, 26), 1)]),
                function("_RNvCs1ysyijkj6NZ_6covfix6branch", 1, &lib, branch(1)),
                function("_RNvCs1ysyijkj6NZ_6covfix9untouched", 0, &lib, untouched()),
                function("_RNvNtCs1ysyijkj6NZ_6covfix6nested7covered", 1, &nested, covered(1)),
            ],
            "totals": {},
        }],
    })
}

#[test]
fn export_translation_merges_instantiations_and_feeds_the_source_reporter() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let measured = MeasuredSet::of(&["crates/covfix"]);
    let files = translate_export(&export(root), root, &measured).unwrap();
    assert_eq!(
        files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        ["crates/covfix/src/lib.rs", "crates/covfix/src/nested.rs"]
    );
    let lib = &files[0];
    assert_eq!(
        lib.functions,
        Metric {
            count: 2,
            covered: 1
        }
    );
    assert_eq!(
        lib.regions,
        Metric {
            count: 9,
            covered: 4
        }
    );
    assert_eq!(
        lib.lines,
        Metric {
            count: 8,
            covered: 4
        }
    );
    assert_eq!(lib.branches, None);
    assert_eq!(lib.shortfalls(), ["lines", "functions", "statements"]);
    // Instantiation counts merge by span: the branch body counted 0 in one binary and 1 in
    // the other, and Istanbul columns are 0-based.
    assert_eq!(lib.istanbul["s"]["0"], 1);
    assert_eq!(
        lib.istanbul["statementMap"]["0"],
        json!({"start": {"line": 4, "column": 0}, "end": {"line": 4, "column": 33}})
    );
    assert_eq!(lib.istanbul["fnMap"]["0"]["name"], "covfix::branch");
    assert_eq!(lib.istanbul["f"]["0"], 1);
    assert_eq!(lib.istanbul["fnMap"]["1"]["name"], "covfix::untouched");
    assert_eq!(lib.istanbul["f"]["1"], 0);
    assert!(files[1].shortfalls().is_empty());
    let roster = Roster {
        note: ROSTER_NOTE.to_owned(),
        files: BTreeMap::new(),
    };
    let host = Host {
        platform: "linux".to_owned(),
        pwsh: false,
    };
    let evaluation = evaluate(&files, &roster, &host, &measured, root).unwrap();
    assert_eq!(
        evaluation.report,
        [
            "\nUncovered locations (per-file 100% gate): 5",
            "crates/covfix/src/lib.rs:8:9 uncovered statement (to 8:10)",
            "crates/covfix/src/lib.rs:13:1 uncovered statement (to 13:38)",
            "crates/covfix/src/lib.rs:13:1 uncovered function covfix::untouched",
            "crates/covfix/src/lib.rs:14:5 uncovered statement (to 14:9)",
            "crates/covfix/src/lib.rs:14:10 uncovered statement (to 14:13)",
            "",
        ]
    );
    assert_eq!(
        evaluation.errors,
        [
            "ERROR: Coverage for lines (50%) does not meet global threshold (100%) for crates/covfix/src/lib.rs",
            "ERROR: Coverage for functions (50%) does not meet global threshold (100%) for crates/covfix/src/lib.rs",
            "ERROR: Coverage for statements (44.44%) does not meet global threshold (100%) for crates/covfix/src/lib.rs",
        ]
    );
    assert_eq!(
        evaluation.below_bar.keys().collect::<Vec<_>>(),
        ["crates/covfix/src/lib.rs"]
    );
    assert_eq!(
        evaluation.summary,
        [
            "coverage: 2 measured files (0 on the roster); lines 7/11 (63.64%), functions 2/3 (66.67%), statements 7/12 (58.33%)"
        ]
    );
    assert!(translate_export(&json!({"type": "other"}), root, &measured).is_err());
    assert!(
        translate_export(&export(root), root, &MeasuredSet::of(&["crates/other"]))
            .unwrap()
            .is_empty()
    );
}

fn entry(platforms: &[&str], unless: Option<&str>) -> RosterEntry {
    RosterEntry {
        reason: "pending suites".to_owned(),
        platforms: platforms
            .iter()
            .map(|platform| (*platform).to_owned())
            .collect(),
        unless: unless.map(str::to_owned),
    }
}

fn roster_for_lib(lib: RosterEntry) -> Roster {
    Roster {
        note: ROSTER_NOTE.to_owned(),
        files: BTreeMap::from([("crates/covfix/src/lib.rs".to_owned(), lib)]),
    }
}

#[test]
fn the_roster_lifts_the_bar_by_host_and_reports_stale_entries() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    for file in [
        "crates/covfix/src/lib.rs",
        "crates/covfix/src/nested.rs",
        "crates/covfix/src/bin/tool.rs",
    ] {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }
    let measured = MeasuredSet::of(&["crates/covfix"]);
    let files = translate_export(&export(root), root, &measured).unwrap();
    let linux = Host {
        platform: "linux".to_owned(),
        pwsh: false,
    };
    let lifted = evaluate(
        &files,
        &roster_for_lib(entry(&[], None)),
        &linux,
        &measured,
        root,
    )
    .unwrap();
    assert!(lifted.errors.is_empty());
    assert!(lifted.below_bar.contains_key("crates/covfix/src/lib.rs"));
    assert!(lifted.summary[0].contains("(1 on the roster)"));
    let elsewhere = evaluate(
        &files,
        &roster_for_lib(entry(&["windows"], None)),
        &linux,
        &measured,
        root,
    )
    .unwrap();
    assert_eq!(elsewhere.errors.len(), 3);
    let with_pwsh = Host {
        platform: "linux".to_owned(),
        pwsh: true,
    };
    let pwsh_only = roster_for_lib(entry(&[], Some("pwsh")));
    assert_eq!(
        evaluate(&files, &pwsh_only, &with_pwsh, &measured, root)
            .unwrap()
            .errors
            .len(),
        3
    );
    assert!(
        evaluate(&files, &pwsh_only, &linux, &measured, root)
            .unwrap()
            .errors
            .is_empty()
    );
}

#[test]
fn stale_roster_entries_are_reported_and_regeneration_widens_or_drops_them() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    for file in [
        "crates/covfix/src/lib.rs",
        "crates/covfix/src/nested.rs",
        "crates/covfix/src/bin/tool.rs",
    ] {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }
    let measured = MeasuredSet::of(&["crates/covfix"]);
    let files = translate_export(&export(root), root, &measured).unwrap();
    let linux = Host {
        platform: "linux".to_owned(),
        pwsh: false,
    };
    let with_pwsh = Host {
        platform: "linux".to_owned(),
        pwsh: true,
    };
    let pwsh_only = roster_for_lib(entry(&[], Some("pwsh")));
    let stale = Roster {
        note: String::new(),
        files: BTreeMap::from([
            ("crates/covfix/src/nested.rs".to_owned(), entry(&[], None)),
            ("crates/covfix/src/missing.rs".to_owned(), entry(&[], None)),
            ("crates/covfix/src/bin/tool.rs".to_owned(), entry(&[], None)),
            (
                "crates/covfix/src/lib.rs".to_owned(),
                entry(&["windows"], None),
            ),
        ]),
    };
    let evaluation = evaluate(&files, &stale, &linux, &measured, root).unwrap();
    assert!(
        evaluation.notices.contains(
            &"coverage roster entry is fully covered; remove it: crates/covfix/src/nested.rs"
                .to_owned()
        )
    );
    assert!(
        evaluation.errors.contains(
            &"coverage roster names a file that does not exist: crates/covfix/src/missing.rs"
                .to_owned()
        )
    );
    assert!(
        evaluation.errors.contains(
            &"coverage roster names a file outside the measured set: crates/covfix/src/bin/tool.rs"
                .to_owned()
        )
    );
    let regenerated = regenerate_roster(&stale, &evaluation, &linux);
    assert_eq!(regenerated.note, ROSTER_NOTE);
    assert_eq!(
        regenerated.files.keys().collect::<Vec<_>>(),
        ["crates/covfix/src/lib.rs"]
    );
    let widened = &regenerated.files["crates/covfix/src/lib.rs"];
    assert_eq!(widened.platforms, ["windows", "linux"]);
    assert_eq!(widened.reason, "pending suites");
    let fresh = regenerate_roster(&Roster::default(), &evaluation, &linux);
    assert_eq!(
        fresh.files["crates/covfix/src/lib.rs"],
        RosterEntry {
            reason: ADOPTION_REASON.to_owned(),
            ..RosterEntry::default()
        }
    );
    let relieved = regenerate_roster(&pwsh_only, &evaluation, &with_pwsh);
    assert_eq!(relieved.files["crates/covfix/src/lib.rs"].unless, None);
    // A lane run on a platform nobody can measure locally prints the entries it would need.
    let additions = roster_additions(&evaluation, &stale, &linux);
    assert_eq!(
        additions,
        BTreeMap::from([(
            "crates/covfix/src/lib.rs".to_owned(),
            RosterEntry {
                reason: ADOPTION_REASON.to_owned(),
                platforms: vec!["linux".to_owned()],
                unless: None,
            }
        )])
    );
    assert!(roster_additions(&evaluation, &roster_for_lib(entry(&[], None)), &linux).is_empty());
}

#[test]
fn the_roster_round_trips_and_rejects_unknown_conditions() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(ROSTER_PATH);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let roster = Roster {
        note: ROSTER_NOTE.to_owned(),
        files: BTreeMap::from([
            ("crates/a/src/lib.rs".to_owned(), entry(&["windows"], None)),
            ("crates/b/src/lib.rs".to_owned(), entry(&[], Some("pwsh"))),
        ]),
    };
    roster.save(&path).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.ends_with("}\n"));
    assert!(!text.contains("\"unless\": null"));
    assert_eq!(Roster::load(&path).unwrap(), roster);
    for (body, complaint) in [
        (
            r#"{"note": "", "files": {"crates/a/src/lib.rs": {"reason": "x", "unless": "wine"}}}"#,
            "unknown condition",
        ),
        (
            r#"{"note": "", "files": {"crates/a/src/lib.rs": {"reason": "x", "platforms": ["plan9"]}}}"#,
            "unknown platform",
        ),
        (
            r#"{"note": "", "files": {"crates/a/src/lib.rs": {"reason": "x", "why": "y"}}}"#,
            "unknown field",
        ),
    ] {
        std::fs::write(&path, body).unwrap();
        let error = Roster::load(&path).unwrap_err().to_string();
        assert!(error.contains(complaint), "{error}");
    }
    assert!(Roster::load(&directory.path().join("absent.json")).is_err());
}

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

/// A one-crate workspace with a covered branch, an untouched function, and a covered module,
/// measured through its own parity manifest and an empty roster.
fn fixture_workspace(root: &Path) {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::fs::copy(
        repository.join("rust-toolchain.toml"),
        root.join("rust-toolchain.toml"),
    )
    .unwrap();
    write(
        root,
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/covfix\"]\nresolver = \"2\"\n",
    );
    write(
        root,
        "crates/covfix/Cargo.toml",
        "[package]\nname = \"covfix\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(
        root,
        "crates/covfix/src/lib.rs",
        "pub fn branch(value: bool) -> u32 {\n    if value { 1 } else { 2 }\n}\n\npub fn untouched(text: &str) -> usize {\n    text.len()\n}\n\npub mod nested;\n",
    );
    write(
        root,
        "crates/covfix/src/nested.rs",
        "pub fn covered() -> u8 {\n    7\n}\n",
    );
    write(
        root,
        "crates/covfix/tests/smoke.rs",
        "#[test]\nfn hits_selected_branch() {\n    assert_eq!(covfix::branch(true), 1);\n    assert_eq!(covfix::nested::covered(), 7);\n}\n",
    );
    write(
        root,
        "porting/parity.json",
        &json!({"surfaces": [{
            "source": "packages/fixture/covfix/src/index.ts",
            "status": "verified",
            "targets": ["crates/covfix/src/lib.rs"],
        }]})
        .to_string(),
    );
    write(
        root,
        ROSTER_PATH,
        &serde_json::to_string(&Roster {
            note: ROSTER_NOTE.to_owned(),
            files: BTreeMap::new(),
        })
        .unwrap(),
    );
    // The lane runs `--locked`, as the repository's lanes do, so the fixture carries a lockfile.
    let locked = Command::new("cargo")
        .args(["generate-lockfile", "--offline"])
        .current_dir(root)
        .status()
        .unwrap();
    assert!(locked.success());
}

fn run_entry(root: &Path, extra: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_run-coverage"));
    command
        .current_dir(root)
        .arg("--repository")
        .arg(root)
        .args(extra)
        .env("CARGO_TARGET_DIR", root.join("target"))
        .env("NO_COLOR", "1");
    // An outer instrumented run must not leak its flags into the fixture's own run.
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy().into_owned();
        if name.starts_with("CARGO_LLVM_COV")
            || matches!(
                name.as_str(),
                "LLVM_PROFILE_FILE"
                    | "RUSTFLAGS"
                    | "CARGO_ENCODED_RUSTFLAGS"
                    | "CARGO_BUILD_RUSTFLAGS"
                    | "CARGO_INCREMENTAL"
                    | "RUSTC_WRAPPER"
                    | "RUSTC_WORKSPACE_WRAPPER"
            )
        {
            command.env_remove(&key);
        }
    }
    command.output().unwrap()
}

#[test]
fn the_public_entry_measures_a_fixture_workspace_end_to_end() {
    let available = Command::new("cargo")
        .args(["llvm-cov", "--version"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if !available {
        eprintln!("skipping: cargo-llvm-cov is not installed");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    fixture_workspace(root);
    let failed = run_entry(root, &["--maxWorkers=1"]);
    let stdout = String::from_utf8_lossy(&failed.stdout);
    let stderr = String::from_utf8_lossy(&failed.stderr);
    assert_eq!(
        failed.status.code(),
        Some(1),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("crates/covfix/src/lib.rs:5:1 uncovered function covfix::untouched"),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains(
            "ERROR: Coverage for functions (50%) does not meet global threshold (100%) for crates/covfix/src/lib.rs"
        ),
        "{stderr}"
    );
    assert!(!stdout.contains("nested.rs:"), "{stdout}");
    assert!(
        stdout.contains(
            "run-coverage: roster entries this host would need: {\"crates/covfix/src/lib.rs\":"
        ),
        "{stdout}"
    );
    let adopted = run_entry(root, &["--write-roster"]);
    assert!(
        adopted.status.success(),
        "{}",
        String::from_utf8_lossy(&adopted.stderr)
    );
    let roster = Roster::load(&root.join(ROSTER_PATH)).unwrap();
    assert_eq!(
        roster.files.keys().collect::<Vec<_>>(),
        ["crates/covfix/src/lib.rs"]
    );
    assert_eq!(
        roster.files["crates/covfix/src/lib.rs"].reason,
        ADOPTION_REASON
    );
    let green = run_entry(root, &[]);
    let green_stderr = String::from_utf8_lossy(&green.stderr);
    assert!(green.status.success(), "{green_stderr}");
    assert!(!green_stderr.contains("ERROR:"), "{green_stderr}");
    println!(
        "real cargo llvm-cov lane passed: uncovered locations, Vitest-worded threshold failure, roster adoption, green rerun"
    );
}
