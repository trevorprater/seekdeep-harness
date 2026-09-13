//! Istanbul reporter lifecycle, location fallback, ordering, and CJS binding parity.

use std::{
    io::Write as _,
    process::{Command, Stdio},
};

use seekdeep_repository_tools::coverage_uncovered_locations::UncoveredLocationsReport;
use serde_json::{Value, json};
use tempfile::TempDir;

const SOURCE: &str = "/Users/trevor/ws/deepseek-harness";

fn location(line: i64, column: i64, end_line: i64, end_column: Value) -> Value {
    let mut location = json!({"start":{"line":line,"column":column},"end":{"line":end_line}});
    location["end"]["column"] = end_column;
    location
}

fn file(path: &str) -> Value {
    json!({"path":path,"statementMap":{},"s":{},"fnMap":{},"f":{},"branchMap":{},"b":{}})
}

fn oracle(files: &[Value], adapter: bool, reset: bool) -> Vec<String> {
    let script = r"
const fs = require('node:fs');
const input = JSON.parse(fs.readFileSync(0, 'utf8'), (_key, value) => value?.$seekdeepNumber ? Number(value.$seekdeepNumber) : value);
const Report = require(process.argv[1]);
const report = new Report({projectRoot:'/repo'});
const lines=[];
console.log = (...args) => lines.push(args.join(' '));
report.onStart();
for (const fc of input.files) report.onDetail({getFileCoverage:()=>fc});
if(input.reset) report.onStart();
report.onEnd();
process.stdout.write(JSON.stringify(lines));
";
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let module = if adapter {
        root.join("scripts/coverage-uncovered-locations.cjs")
    } else {
        std::path::Path::new(SOURCE).join("scripts/coverage-uncovered-locations.cjs")
    };
    let mut child = Command::new("node")
        .args(["-e", script])
        .arg(module)
        .env("NODE_PATH", format!("{SOURCE}/node_modules"))
        .env(
            "SEEKDEEP_COVERAGE_REPORT_BIN",
            env!("CARGO_BIN_EXE_coverage-uncovered-locations"),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            serde_json::to_string(&json!({"files":files,"reset":reset}))
                .unwrap()
                .as_bytes(),
        )
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn fixture() -> Vec<Value> {
    let mut first = file("/repo/space and 雪/main.ts");
    first["statementMap"] = json!({"10":location(7,4,9,json!({"$seekdeepNumber":"Infinity"})),"2":location(3,1,3,json!(1)),"0":location(5,2,5,json!(8)),"bad":{},"negative":location(0,1,1,json!(1)),"covered":location(1,0,1,json!(2)),"single":location(6,0,6,json!({"$seekdeepNumber":"Infinity"}))});
    first["s"] = json!({"10":0,"2":0,"0":0,"bad":0,"negative":0,"covered":1,"single":0});
    first["fnMap"] = json!({"0":{"name":"read 雪","decl":location(3,1,4,json!(0)),"loc":location(99,0,100,json!(0))},"1":{"name":"","decl":{},"loc":location(2,0,3,json!(1))},"2":{"name":"skip","decl":{},"loc":{}},"3":{"name":"covered","loc":location(1,0,2,json!(0))}});
    first["f"] = json!({"0":0,"1":0,"2":0,"3":4});
    first["branchMap"] = json!({"0":{"type":"if","loc":location(3,1,3,json!(2)),"locations":[{},location(11,2,12,json!(4)),location(20,0,20,json!(1))]},"1":{"type":"binary-expr","loc":location(4,0,5,json!(1))},"2":{"type":"if","locations":[{}]}});
    first["b"] = json!({"0":[0,0,3],"1":[0,0],"2":[0]});
    let mut second = file("/outside/second.ts");
    second["statementMap"] = json!({"2":location(1,0,1,json!(0)),"1":{"start":{"line":2},"end":{"line":2,"column":null}},"3":{"start":{"line":3,"column":null}},"4":{"start":{"line":4,"column":"2"}},"5":{"start":{"line":{"$seekdeepNumber":"Infinity"},"column":0}}});
    second["s"] = json!({"1":0,"2":0,"3":0,"4":0,"5":0});
    vec![first, file("/repo/green.ts"), second]
}

#[test]
fn exact_records_and_console_output_match_istanbul_source() {
    let files = fixture();
    let mut native = UncoveredLocationsReport {
        project_root: "/repo".to_owned(),
        records: Vec::new(),
    };
    for file in &files {
        native.detail(file).unwrap();
    }
    assert_eq!(native.finish(), oracle(&files, false, false));
    assert_eq!(native.finish(), oracle(&files, true, false));
    assert!(
        native
            .records
            .iter()
            .any(|record| record.contains("(to 9)"))
    );
    assert!(
        native
            .records
            .iter()
            .any(|record| record.contains("uncovered branch (if, path 1/3)"))
    );
}

#[test]
fn green_and_restarted_runs_are_silent() {
    let files = fixture();
    let mut report = UncoveredLocationsReport {
        project_root: "/repo".to_owned(),
        records: Vec::new(),
    };
    report.detail(&file("/repo/green.ts")).unwrap();
    assert!(report.finish().is_empty());
    for file in &files {
        report.detail(file).unwrap();
    }
    report.start();
    assert!(report.finish().is_empty());
    assert_eq!(report.finish(), oracle(&files, false, true));
    assert_eq!(report.finish(), oracle(&files, true, true));
}

#[test]
fn malformed_input_and_unknown_operations_fail_loudly() {
    let mut report = UncoveredLocationsReport {
        project_root: "/repo".to_owned(),
        records: Vec::new(),
    };
    assert!(report.detail(&json!({})).is_err());
    assert!(
        seekdeep_repository_tools::coverage_uncovered_locations::bridge(
            &json!({"operation":"mystery"})
        )
        .is_err()
    );
    let mut missing_counts = file("/repo/a.ts");
    missing_counts["branchMap"] = json!({"0":{"type":"if"}});
    assert!(
        report
            .detail(&missing_counts)
            .unwrap_err()
            .to_string()
            .contains("count array")
    );
}

#[test]
fn large_uncovered_reports_preserve_every_record_across_the_process_bridge() {
    let mut coverage = file(&format!("/repo/{}/a.ts", "directory-".repeat(8)));
    for index in 0..12_000 {
        coverage["statementMap"][index.to_string()] = location(index + 1, 0, index + 1, json!(1));
        coverage["s"][index.to_string()] = json!(0);
    }
    let files = [coverage];
    let expected = oracle(&files, false, false);
    assert!(serde_json::to_vec(&expected).unwrap().len() > 1024 * 1024);
    assert_eq!(oracle(&files, true, false), expected);
}

#[test]
fn installed_istanbul_report_execution_resolves_compiled_target_and_preserves_reuse() {
    let directory = TempDir::new().unwrap();
    let scripts = directory.path().join("scripts");
    std::fs::create_dir(&scripts).unwrap();
    let adapter = scripts.join("coverage-uncovered-locations.cjs");
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/coverage-uncovered-locations.cjs"),
        &adapter,
    )
    .unwrap();
    let program = r"
const assert = require('node:assert/strict');
const coverageRequire = require('node:module').createRequire(require.resolve('@vitest/coverage-v8'));
const { createCoverageMap } = coverageRequire('istanbul-lib-coverage');
const { createContext } = require('istanbul-lib-report');
const reports = coverageRequire('istanbul-reports');
const lines = [];
console.log = (...args) => lines.push(args.join(' '));
const report = reports.create(process.argv[1], { projectRoot: '/repo' });
assert.equal(report.projectRoot, '/repo');
assert.deepEqual(report.records, []);
report.context = { reporter: report };
const location = { start: { line: 3, column: 1 }, end: { line: 4, column: Infinity } };
const coverage = { path: '/repo/a.ts', statementMap: { 0: location }, s: { 0: 0 },
  fnMap: {}, f: {}, branchMap: {}, b: {} };
report.execute(createContext({ coverageMap: createCoverageMap([coverage]) }));
assert.deepEqual(lines, ['\nUncovered locations (per-file 100% gate): 1',
  'a.ts:3:2 uncovered statement (to 4)', '']);
const records = report.records;
report.onDetail({ getFileCoverage: () => coverage });
assert.equal(report.records, records);
assert.equal(records.length, 2);
report.onStart();
assert.notEqual(report.records, records);
assert.deepEqual(report.records, []);
lines.length = 0;
coverage.s[0] = 1;
report.execute(createContext({ coverageMap: createCoverageMap([coverage]) }));
assert.deepEqual(lines, []);
process.stdout.write('Istanbul execute and reporter reuse passed\n');
";
    let run = |target: Option<&std::path::Path>, triple: Option<&str>| {
        let mut command = Command::new("node");
        command
            .args(["-e", program])
            .arg(&adapter)
            .env("NODE_PATH", format!("{SOURCE}/node_modules"))
            .env_remove("SEEKDEEP_COVERAGE_REPORT_BIN")
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("CARGO_BUILD_TARGET");
        if let Some(target) = target {
            command.env("CARGO_TARGET_DIR", target);
        }
        if let Some(triple) = triple {
            command.env("CARGO_BUILD_TARGET", triple);
        }
        command.output().unwrap()
    };
    let missing = run(None, None);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("compiled Rust reporter not found"));
    for (target, triple) in [
        (None, None),
        (Some(directory.path().join("custom target")), None),
        (
            Some(directory.path().join("target with triple")),
            Some("native-test-target"),
        ),
    ] {
        let destination = target
            .clone()
            .unwrap_or_else(|| directory.path().join("target"))
            .join(triple.unwrap_or(""))
            .join("debug")
            .join(format!(
                "coverage-uncovered-locations{}",
                std::env::consts::EXE_SUFFIX
            ));
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::hard_link(
            env!("CARGO_BIN_EXE_coverage-uncovered-locations"),
            &destination,
        )
        .unwrap();
        let output = run(target.as_deref(), triple);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "Istanbul execute and reporter reuse passed\n"
        );
    }
}
