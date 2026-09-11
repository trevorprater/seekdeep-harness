//! Package-path and manual-topology selection against the pinned setup script.

use std::{
    io::Write as _,
    process::{Command, Stdio},
};

use seekdeep_cordis::{test_invariant_companion_paths, uses_manual_invariant_tree};
use serde_json::{Value, json};

#[test]
fn source_path_selection_matches_across_platforms_nesting_exceptions_and_unicode_order() {
    let source = "/Users/trevor/ws/deepseek-harness";
    let available = [
        "../packages/core/tools/src/invariant.ts",
        "../packages/runtime-diagnostics/invariants/src/invariant.ts",
        "../packages/core/session/src/invariant.ts",
        "../packages/z/\u{10000}/src/invariant.ts",
        "../packages/z/\u{e000}/src/invariant.ts",
    ];
    let mut paths = vec![
        "",
        "test-invariants.spec.ts",
        "/scripts/test-invariants.spec.ts",
        "C:\\repo\\scripts\\test-invariants.spec.ts",
        "/repo/examples/echo-agent/tests/echo.spec.ts",
        "/repo/packages/core/tools/tests/tools.spec.ts",
        "/repo/packages/core/tools/tests/",
        "/repo/packages/core/tools/tests",
        "/repo/packages/core/tools/tests/nested/nested.spec.ts",
        "/repo/packages/core/session/tests/invariant.spec.ts",
        "/repo/packages/core/session/tests/request-invariant-hmr.spec.ts",
        "/repo/packages/core/session/tests/nested/invariant.spec.ts",
        "/repo/packages/core/session/tests/invariant.spec.tsx",
        "/repo/packages/core/session/tests/invariant.spec.ts/trailing",
        "/repo/packages/core/session/tests/INVARIANT.spec.ts",
        "/repo/packages/runtime-diagnostics/invariants/tests/service.spec.ts",
        "C:\\repo\\packages\\runtime-diagnostics\\invariants\\tests\\service.spec.ts",
        "/repo/packages/examples/agent-spine-demo/tests/agent-core.spec.ts",
        "/repo/packages/missing/package/tests/index.spec.ts",
        "/repo/packages/core//tests/index.spec.ts",
        "/repo/packages//tools/tests/index.spec.ts",
        "/repo/packages/a/b/not-tests/packages/core/tools/tests/index.spec.ts",
        "/repo/packages/core/session/tests/packages/core/tools/tests/index.spec.ts",
        "/repo/packages/core/tools/tests//index.spec.ts",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    for file in [
        "invariant.spec.ts",
        "prefix-invariants.spec.ts",
        "ordinary.spec.ts",
        "invariant.test.ts",
    ] {
        for prefix in ["/repo", "C:\\repo", "/repo/with space", "/repo/💻"] {
            paths.push(format!("{prefix}/packages/core/tools/tests/{file}"));
        }
    }
    let expected = oracle(source, &available, &paths);
    for (path, expected) in paths.iter().zip(expected) {
        let selection = match test_invariant_companion_paths(path, available) {
            Ok(paths) => json!({"ok": paths}),
            Err(error) => json!({"error": error}),
        };
        assert_eq!(
            json!({"manual":uses_manual_invariant_tree(path),"selection":selection}),
            expected,
            "{path}"
        );
    }
    assert_eq!(paths.len(), 40);
}

fn oracle(source: &str, available: &[&str], paths: &[String]) -> Vec<Value> {
    let script = r"
const fs=require('node:fs');
const vm=require('node:vm');
const sourceRoot=process.argv[1];
const ts=require(sourceRoot+'/node_modules/typescript/lib/typescript.js');
const path=sourceRoot+'/scripts/test-invariants.ts';
const source=ts.createSourceFile(path,fs.readFileSync(path,'utf8'),ts.ScriptTarget.Latest,true);
const keep=new Set(['usesManualInvariantTree','testInvariantCompanionPaths','MANUAL_INVARIANT_TEST_EXCEPTIONS','ALL_COMPANION_TESTS']);
const selected=source.statements.filter(s=>ts.isFunctionDeclaration(s)?keep.has(s.name?.text):ts.isVariableStatement(s)&&s.declarationList.declarations.some(d=>keep.has(d.name.getText(source))));
const input=JSON.parse(fs.readFileSync(0,'utf8'));
const raw=selected.map(s=>s.getText(source).replace(/^export /,'')).join('\n')+`
JSON.stringify(input.paths.map(path=>{
 let selection; try { selection={ok:testInvariantCompanionPaths(path)}; } catch(error) { selection={error:error.message}; }
 return {manual:usesManualInvariantTree(path),selection};
}));`;
const code=ts.transpileModule(raw,{compilerOptions:{target:ts.ScriptTarget.ES2022}}).outputText;
process.stdout.write(vm.runInNewContext(code,{input,testInvariantCompanions:Object.fromEntries(input.available.map(path=>[path,()=>{}]))}));
";
    let mut child = Command::new("node")
        .args(["-e", script, source])
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
            serde_json::to_string(&json!({"available":available,"paths":paths}))
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
