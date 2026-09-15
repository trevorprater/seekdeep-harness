//! Live oracle for compiler-independent workspace rules.

use std::{
    error::Error,
    io::Write as _,
    path::Path,
    process::{Command, Stdio},
};

use seekdeep_typert_generator::{
    analyzer::{
        client_export_subpaths, external_module_identity_for_file, host_export_subpaths,
        is_dual_face_package, is_remote_segment, is_standard_library_file, merge_workspace_models,
        module_identity, package_export_targets, source_path_for_export,
    },
    model::WorkspaceModel,
};
use serde_json::{Value, json};

const ORACLE: &str = r"
const { readFileSync } = require('node:fs');
const { resolve } = require('node:path');
const { createRequire } = require('node:module');
const root = resolve(process.argv[1]);
const sourceRequire = createRequire(resolve(root, 'package.json'));
const ts = sourceRequire('typescript');
const path = resolve(root, 'packages/typert/generator/src/analyzer.ts');
const source = readFileSync(path, 'utf8');
const file = ts.createSourceFile(path, source, ts.ScriptTarget.Latest, true);
const names = new Set(['packageExportTargets', 'exportTarget', 'hostExportSubpaths', 'clientExportSubpaths', 'isDualFacePackage', 'sourcePathForExport', 'moduleIdentity', 'externalModuleIdentityForFile', 'isStandardLibraryFile', 'isRemoteSegment', 'mergeWorkspaceModels', 'compareCrossFaceLinks', 'slash']);
const selected = file.statements.filter(statement => ts.isFunctionDeclaration(statement) && names.has(statement.name?.text))
  .map(statement => statement.getText(file)).join('\n');
const functions = new Function('resolve', ts.transpileModule(selected + '\nreturn {' + [...names].join(',') + '};', {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None },
}).outputText)(resolve);
let input = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', chunk => { input += chunk });
process.stdin.on('end', () => {
  const results = JSON.parse(input).map(item => {
    if (item.kind === 'manifest') {
      const manifest = { ...item.value };
      if (Object.hasOwn(manifest, 'seekdeep')) { manifest.dsh = manifest.seekdeep; delete manifest.seekdeep; }
      return { targets: functions.packageExportTargets(manifest), host: functions.hostExportSubpaths(manifest), client: functions.clientExportSubpaths(manifest), dual: functions.isDualFacePackage(manifest) };
    }
    if (item.kind === 'path') return functions.sourcePathForExport(item.root, item.value);
    if (item.kind === 'identity') return { module: functions.moduleIdentity(item.value) ?? null, external: functions.externalModuleIdentityForFile(item.value) ?? null, standard: functions.isStandardLibraryFile(item.value), remote: functions.isRemoteSegment(item.value) };
    if (item.kind === 'merge') return functions.mergeWorkspaceModels(item.value);
    throw new Error('unknown case');
  });
  process.stdout.write(JSON.stringify(results));
});
";

fn main() -> Result<(), Box<dyn Error>> {
    let source = std::env::args_os()
        .nth(1)
        .ok_or("pinned source root required")?;
    let head = Command::new("git")
        .arg("-C")
        .arg(&source)
        .args(["rev-parse", "HEAD"])
        .output()?;
    let pin = include_str!("../../../SOURCE_SNAPSHOT")
        .lines()
        .find_map(|line| line.strip_prefix("commit="))
        .ok_or("missing source pin")?;
    if !head.status.success() || String::from_utf8_lossy(&head.stdout).trim() != pin {
        return Err("oracle differs from SOURCE_SNAPSHOT".into());
    }
    let input = cases();
    let mut child = Command::new("node")
        .args(["-e", ORACLE])
        .arg(source)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or("missing oracle stdin")?
        .write_all(&serde_json::to_vec(&input)?)?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    let expected: Vec<Value> = serde_json::from_slice(&output.stdout)?;
    if expected.len() != input.len() {
        return Err("oracle case count differs".into());
    }
    for (index, (case, expected)) in input.iter().zip(expected).enumerate() {
        let actual = native(case)?;
        if actual != expected {
            return Err(format!("case {index} differs: {actual} != {expected}").into());
        }
    }
    println!(
        "{} analyzer workspace-rule cases match the pinned source",
        input.len()
    );
    Ok(())
}

fn native(case: &Value) -> Result<Value, Box<dyn Error>> {
    let value = &case["value"];
    Ok(match case["kind"].as_str().ok_or("missing kind")? {
        "manifest" => {
            json!({"targets":package_export_targets(value),"host":host_export_subpaths(value),"client":client_export_subpaths(value),"dual":is_dual_face_package(value)})
        }
        "path" => json!(source_path_for_export(
            Path::new(case["root"].as_str().ok_or("missing root")?),
            value.as_str().ok_or("missing target")?
        )?),
        "identity" => {
            let value = value.as_str().ok_or("missing identity")?;
            json!({"module":module_identity(value),"external":external_module_identity_for_file(value),"standard":is_standard_library_file(value),"remote":is_remote_segment(value)})
        }
        "merge" => serde_json::to_value(merge_workspace_models(serde_json::from_value::<
            Vec<WorkspaceModel>,
        >(value.clone())?))?,
        _ => return Err("unknown case".into()),
    })
}

fn cases() -> Vec<Value> {
    let mut cases = vec![
        json!({"kind":"manifest","value":{}}),
        json!({"kind":"manifest","value":{"exports":"./lib/index.js"}}),
        json!({"kind":"manifest","value":{"types":"./types.d.ts"}}),
        json!({"kind":"manifest","value":{"exports":{"browser":null,"development":"./dev.js"}}}),
        json!({"kind":"manifest","value":{"exports":{"default":"default","types":"types","import":"import"}}}),
        json!({"kind":"manifest","value":{"exports":[null,false,{},[false,"first"],"second"]}}),
        json!({"kind":"manifest","value":{"exports":{".":"root","./client":"client","./client/extra":"extra","./remote":"remote","ignored":"skip","./none":[null,false]}}}),
        json!({"kind":"manifest","value":{"seekdeep":{"client":{}},"exports":{".":"root","./client":"client"}}}),
        json!({"kind":"manifest","value":{"seekdeep":{"client":[]},"exports":{"./client":"client"}}}),
        json!({"kind":"manifest","value":{"seekdeep":{"client":false},"exports":{"./client":"client"}}}),
        json!({"kind":"manifest","value":{"exports":true,"types":"fallback"}}),
        json!({"kind":"manifest","value":serde_json::from_str::<Value>(r#"{"exports":{"2":"two","1":"one","01":"leading-zero"}}"#).unwrap()}),
    ];
    for root in ["/workspace/package", "relative-package"] {
        for value in [
            "./lib/types/index.d.ts",
            "lib/types/index.d.mts",
            "lib/types/index.d.cts",
            "./lib/index.js",
            "lib/index.mjs",
            "lib/index.cjs",
            "lib/index.d.ts",
            "lib/index.D.TS",
            "lib/index.d.ts\n",
            "./src/../direct.ts",
            "lib/types//absolute.d.ts",
            "/absolute.ts",
            "../outside.ts",
        ] {
            cases.push(json!({"kind":"path","root":root,"value":value}));
        }
    }
    for value in [
        "",
        ".",
        "..",
        "a",
        "a.b",
        "a-b",
        "$name",
        "a b",
        "a/b",
        "é",
        "@scope/package/types",
        "@scope",
        "node:fs",
        "./relative",
        "/root/node_modules/pkg/file.d.ts",
        "/root/node_modules/.pnpm/pkg/node_modules/@types/node/index.d.ts",
        "C:\\root\\node_modules\\typescript\\lib\\lib.esnext.d.ts",
        "/root/typescript/lib/lib.d.ts",
        "/root/typescript/lib/lib.esnext.d.ts\n",
    ] {
        cases.push(json!({"kind":"identity","value":value}));
    }
    let fixture: Value =
        serde_json::from_str(include_str!("../tests/fixtures/source_type_model.json"))
            .expect("model fixture");
    let mut modified = fixture["workspace"].clone();
    modified["faces"].as_array_mut().expect("faces").reverse();
    modified["faces"][0]["packages"][0]["root"] = json!("replacement");
    modified["faces"][0]["graph"]["declarations"][0]["text"] = json!("not selected");
    cases.push(json!({"kind":"merge","value":[]}));
    cases.push(json!({"kind":"merge","value":[fixture["workspace"],modified]}));
    cases
}
