//! Live source differential for the include's patch semantics: the pinned
//! `applyEntryPatches` runs under Node against fixture entry lists, and the
//! Rust composer must produce the same detached entries and the same skipped-
//! patch diagnostics in the same order.

use std::{path::PathBuf, process::Command};

use seekdeep_loader::profile_patch::{
    ProfileEntry, ProfilePatch, apply_entry_patches_with_warning_sink,
};
use serde_json::{Value, json};

const ORACLE: &str = r"
const { resolve } = require('node:path');
const { createRequire } = require('node:module');
const root = resolve(process.argv[1]);
const sourceRequire = createRequire(resolve(root, 'package.json'));
sourceRequire('tsx/cjs');
const { applyEntryPatches } = sourceRequire('./vendor/include/src/index.ts');
const { format } = require('node:util');
const cases = JSON.parse(process.argv[2]);
const out = cases.map(({ data, patches }) => {
  const warnings = [];
  const result = applyEntryPatches(data, patches, (message, ...args) => {
    warnings.push(format(message.replaceAll('%C', '%s'), ...args.map(value => value === undefined ? 'undefined' : JSON.stringify(value))));
  });
  return { result, warnings };
});
process.stdout.write(JSON.stringify(out));
";

fn cases() -> Vec<(Value, Value)> {
    let base = json!([
        { "id": "a", "name": "./a.mjs", "config": { "value": 1 } },
        { "id": "grp", "name": "cordis:group", "group": true, "config": [
            { "id": "nested", "name": "./nested.mjs" }
        ] },
        { "id": "plain", "name": "./plain.mjs" }
    ]);
    vec![
        (base.clone(), json!([])),
        (
            base.clone(),
            json!([
                { "id": "a", "config": { "value": 2 }, "disabled": true },
                { "id": "nested", "name": "./nested.mjs", "inject": ["x"] },
                { "id": "missing", "config": {} },
                { "config": { "orphan": true } },
                { "id": "plain", "name": "./other.mjs", "config": {} }
            ]),
        ),
        (
            base.clone(),
            json!([
                { "insert": [ { "id": "z", "name": "./z.mjs" } ] },
                { "id": "grp", "insert": [ { "id": "deep", "name": "./deep.mjs" } ] },
                { "id": "plain", "insert": [ { "id": "no", "name": "./no.mjs" } ] },
                { "id": "ghost", "insert": [ { "id": "no", "name": "./no.mjs" } ] },
                { "id": "deep", "config": { "patched": "later" } },
                { "id": "z", "disabled": true }
            ]),
        ),
        (
            json!([{ "id": "only", "name": "./only.mjs", "config": { "nested": { "deep": [1, 2] } } }]),
            json!([
                { "id": "only", "config": { "nested": { "deep": [3] } }, "isolate": { "svc": true }, "intercept": { "svc": { "a": 1 } } },
                { "id": "only", "name": "" , "config": { "again": true } }
            ]),
        ),
        (
            json!([{ "id": "dup", "name": "./one.mjs" }, { "id": "dup", "name": "./two.mjs" }]),
            json!([{ "id": "dup", "config": { "hit": true } }]),
        ),
    ]
}

fn to_entries(value: &Value) -> Vec<ProfileEntry> {
    serde_json::from_value(value.clone()).expect("entry list deserializes")
}

fn to_patches(value: &Value) -> Vec<ProfilePatch> {
    serde_json::from_value(value.clone()).expect("patch list deserializes")
}

#[test]
fn rust_composer_matches_the_pinned_include_patch_semantics() {
    let source = std::env::var_os("SEEKDEEP_PARITY_SOURCE").map_or_else(
        || PathBuf::from("/Users/trevor/ws/deepseek-harness"),
        Into::into,
    );
    let cases = cases();
    let payload = serde_json::to_string(
        &cases
            .iter()
            .map(|(data, patches)| json!({ "data": data, "patches": patches }))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let output = Command::new("node")
        .args(["-e", ORACLE])
        .arg(&source)
        .arg(&payload)
        .output()
        .expect("node runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(expected.len(), cases.len());
    for ((data, patches), expected) in cases.iter().zip(&expected) {
        let mut warnings = Vec::new();
        let entries = apply_entry_patches_with_warning_sink(
            &to_entries(data),
            &to_patches(patches),
            |warning| warnings.push(warning.to_string()),
        )
        .expect("composition succeeds");
        let actual = serde_json::to_value(&entries).unwrap();
        assert_eq!(actual, expected["result"], "entries for {patches}");
        assert_eq!(
            json!(warnings),
            expected["warnings"],
            "warnings for {patches}"
        );
    }
}
