//! Real source-handler comparisons, including its unusual zero-column behavior.

use std::{
    io::Write,
    process::{Command, Stdio},
};

use seekdeep_hmr::error::{code_frame, format_error};
use serde_json::{Value, json};

fn source_errors(cases: &[Value], color: bool) -> Vec<Value> {
    let source_root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../deepseek-harness");
    let mut child = Command::new("node")
        .args(["--input-type=module", "-e", "import fs from 'node:fs'; import { handleError } from './vendor/hmr/src/error.ts'; const cases=JSON.parse(fs.readFileSync(0,'utf8')); const normalize=value=>value instanceof Error ? {name:value.name,message:value.message,...(value.code?{code:value.code}:{})} : value; const output=cases.map(value=>{const warnings=[]; try {handleError({logger:{warn(value){warnings.push(normalize(value))}}}, value); return {warnings};} catch(error){return {error:error.message}}}); process.stdout.write(JSON.stringify(output));"])
        .env("FORCE_COLOR", if color { "1" } else { "0" })
        .env_remove("NO_COLOR")
        .current_dir(source_root)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("start source error handler");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(serde_json::to_vec(cases).unwrap().as_slice())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("source error results")
}

#[test]
fn source_handler_keeps_original_errors_and_orders_compiler_warnings() {
    let temporary = tempfile::tempdir().unwrap();
    let file = temporary.path().join("plugin.ts");
    std::fs::write(
        &file,
        "const good = 1;\nconst broken = ;\nexport default good;\n",
    )
    .unwrap();
    let cases = vec![
        json!(null),
        json!("failure"),
        json!({"name":"Error", "message":"boom"}),
        json!({"errors":[]}),
        json!({"errors":[{"text":""}],"message":"original"}),
        json!({"errors":[null]}),
        json!({"errors":[{"text":"first"},null]}),
        json!({"errors":[{"text":"first"},{"text":"second"}]}),
        json!({"errors":[{"text":"number", "location":false},{"text":23},{"text":true}]}),
        json!({"errors":[{"text":"Expected expression", "location":{"file":file,"line":2,"column":15}}]}),
        json!({"errors":[{"text":"No caret", "location":{"file":file,"line":2,"column":0}}]}),
        json!({"errors":[{"text":"Missing file", "location":{"file":temporary.path().join("missing.ts"),"line":1,"column":1}}, {"text":"continue"}]}),
        json!({"errors":[{"text":"Directory", "location":{"file":temporary.path(),"line":1,"column":1}}]}),
        json!({"errors":[{"text":"No location file", "location":{}}]}),
    ];
    for color in [false, true] {
        let expected = source_errors(&cases, color);
        for (index, case) in cases.iter().enumerate() {
            let actual = match format_error(case, color) {
                Ok(warnings) => json!({"warnings":warnings}),
                Err(error) => json!({"error":error.to_string()}),
            };
            assert_eq!(actual, expected[index], "case={index} color={color}");
        }
    }
}

#[test]
fn source_code_frames_match_at_line_column_unicode_and_color_boundaries() {
    let temporary = tempfile::tempdir().unwrap();
    let sources = [
        "",
        "\n",
        "const x = ;",
        "const x = ;\n",
        "\tconst 名😀 = ;\nnext();\n",
        "a\rb\r\nc\u{2028}d\u{2029}e\n",
        "// comment\nconst Thing = `multi\nline ${1}`;\n/abc/g.test('abc');\n@decorator\n<div foo='a'/>;\n",
        "export async function Example(value) {\n  if (value instanceof Example) return null;\n  const number = 0xff + .25e-2;\n  /* block\n   * comment */\n  return /[a-z]+/gi;\n}\n",
        "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\ntwelve\n",
    ];
    for color in [false, true] {
        let mut cases = Vec::new();
        let mut expected_inputs = Vec::new();
        for (index, source) in sources.iter().enumerate() {
            let file = temporary.path().join(format!("source-{index}.ts"));
            std::fs::write(&file, source).unwrap();
            for (line, column) in [
                (1, 0),
                (1, 1),
                (1, 4),
                (1, 100),
                (2, 1),
                (3, 7),
                (5, 0),
                (10, 2),
                (12, 3),
                (99, 1),
                (0, 0),
                (-1, 1),
            ] {
                cases.push(json!({"errors":[{"text":"test diagnostic", "location":{"file":file,"line":line,"column":column}}]}));
                expected_inputs.push((*source, line, column, file.clone()));
            }
        }
        let expected = source_errors(&cases, color);
        for (index, (source, line, column, file)) in expected_inputs.iter().enumerate() {
            let actual = format!(
                "File: {}:{line}:{column}\n{}",
                file.display(),
                code_frame(source, *line, *column, "test diagnostic", color)
            );
            assert_eq!(
                json!({"warnings":[actual]}),
                expected[index],
                "source={source:?} line={line} column={column} color={color}"
            );
        }
    }
}
