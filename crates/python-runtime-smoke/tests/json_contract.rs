//! Python serialization runs outside the live-listener unit-test process.
//! A concurrent fork can retain another test's descriptors until exec.

#[path = "../src/json.rs"]
mod json;

use serde_json::Value;
use std::{
    io::Write as _,
    process::{Command, Stdio},
};

#[test]
fn all_three_json_forms_match_python_including_unicode_and_number_spelling() {
    let value: Value = serde_json::from_str(r#"{"values":[-0.0,0.0,1e-5,1e-4,1e15,1e16,1e100,1e-100,1.2345678901234567,340282366920938463463374607431768211455],"text":["é🦀","\u007f","\u0001","line\n\"quoted\""],"empty":[[],{}]}"#).unwrap();
    let mut python = Command::new(if cfg!(windows) { "python" } else { "python3" })
        .args(["-c", "import json,sys; value=json.load(sys.stdin); print(json.dumps([json.dumps(value),json.dumps(value,indent=2,ensure_ascii=False),json.dumps(value,separators=(',',':'),ensure_ascii=False)]))"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    python
        .stdin
        .take()
        .unwrap()
        .write_all(value.to_string().as_bytes())
        .unwrap();
    let output = python.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: Vec<String> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        [
            json::dumps(&value, false, true),
            json::dumps(&value, true, false),
            json::compact(&value)
        ],
        expected.as_slice()
    );
}
