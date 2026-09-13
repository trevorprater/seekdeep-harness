//! Pinned failure-display behavior over persisted JSON and exact string units.

use std::{
    io::Write as _,
    path::PathBuf,
    process::{Command, Stdio},
};

use seekdeep_failure_display::{display_failure_message, display_failure_message_json};
use seekdeep_lossless_json::JsonValue;
use serde_json::Value;

fn source_outputs(inputs: &[String]) -> Vec<Value> {
    let source = std::env::var_os("SEEKDEEP_PARITY_SOURCE")
        .map_or_else(
            || PathBuf::from("/Users/trevor/ws/deepseek-harness"),
            PathBuf::from,
        )
        .join("packages/client/runtime/src/client/sessions/failure-display.ts");
    let script = r"
import { readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';
const { displayFailureMessage } = await import(pathToFileURL(process.argv[1]).href);
const results = JSON.parse(readFileSync(0, 'utf8')).map(raw => {
  const result = displayFailureMessage(JSON.parse(raw));
  return {
    units: Array.from({ length: result.length }, (_, i) => result.charCodeAt(i)),
    json: JSON.stringify(result),
  };
});
process.stdout.write(JSON.stringify(results));
";
    let mut child = Command::new("node")
        .args(["--input-type=module", "--eval", script])
        .arg(source)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(inputs).unwrap())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "source failure display: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn fixtures() -> Vec<String> {
    let mut inputs = [
        "null",
        "false",
        "true",
        "0",
        "-0",
        "1.0",
        "1e-7",
        "1e21",
        "9007199254740993",
        "1e400",
        "-1e400",
        "1e-400",
        r#""""#,
        r#""line\n\u0000\t\u2028\u2029😀""#,
        r#""\ud800\udc00\ud800\ud800\udfff""#,
        r#"{"code":"AUTH","message":"sk-test-\ud800","opaque":{"\udfff":"\ud800"}}"#,
        r#"{"co\u0064e":"A\u0055TH","message":"hidden"}"#,
        r#"{"code":"AUTH","code":"SERVER","message":"shown\udfff"}"#,
        r#"{"code":"SERVER","code":"AUTH","message":"hidden\udfff"}"#,
        r#"{"code":"AUTH\ud800","message":"shown\ud800","opaque":{"\ud800":1}}"#,
        r#"{"message":"","opaque":{"\ud800":"\udfff"}}"#,
        r#"{"message":false,"opaque":{"\ud800":"\udfff"}}"#,
        r#"{"message":null,"opaque":{"\ud800":"\udfff"}}"#,
        r#"{"message":["\ud800"],"opaque":{"\udfff":null}}"#,
        r#"{"message":{"text":"\udfff"},"opaque":{"\ud800":"\udfff"}}"#,
        r#"{"message":"discarded","mess\u0061ge":"retained\ud800"}"#,
        r#"{"message":"discarded","message":1,"opaque":"\udfff"}"#,
        r#"{"__proto__":{"code":"AUTH","message":"hidden"},"value":"\ud800"}"#,
        r#"[{"code":"AUTH","message":"array entry \ud800"},null,-0,1e400,-1e400]"#,
        r#"{"later":1,"10":"ten","2":"two","\u0032":"last","01":"leading","\ud800":"\udfff","nested":{"3":3,"1":1},"large":9007199254740993}"#,
        r#"{"4294967295":"ordinary","4294967294":"index","-0":"negative","0":"zero","00":"leading"}"#,
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    for unit in 0xd800_u16..=0xdfff {
        inputs.push(format!(r#""\u{unit:04x}""#));
        inputs.push(format!(
            r#"{{"message":"before\u{unit:04x}after","opaque":{{"\ud800":"\udfff"}}}}"#
        ));
    }
    inputs
}

#[test]
fn raw_failures_match_source_redaction_messages_and_json_fallback() {
    let inputs = fixtures();
    let expected = source_outputs(&inputs);
    assert_eq!(expected.len(), inputs.len());
    for (input, expected) in inputs.iter().zip(expected) {
        let failure = JsonValue::parse(input.clone()).unwrap();
        let retained = failure.as_raw().to_owned();
        let display = display_failure_message_json(&failure);
        assert_eq!(
            display.utf16_units(),
            serde_json::from_value::<Vec<u16>>(expected["units"].clone()).unwrap(),
            "{input}"
        );
        assert_eq!(
            display.as_raw(),
            expected["json"].as_str().unwrap(),
            "{input}"
        );
        assert_eq!(failure.as_raw(), retained, "failure snapshot changed");
    }
}

#[test]
fn scalar_compatibility_api_matches_source_including_numeric_overflow() {
    let inputs = fixtures()
        .into_iter()
        .filter(|input| serde_json::from_str::<Value>(input).is_ok())
        .collect::<Vec<_>>();
    let expected = source_outputs(&inputs);
    for (input, expected) in inputs.iter().zip(expected) {
        let failure = serde_json::from_str(input).unwrap();
        let display = display_failure_message(&failure);
        assert_eq!(
            display.encode_utf16().collect::<Vec<_>>(),
            serde_json::from_value::<Vec<u16>>(expected["units"].clone()).unwrap(),
            "{input}"
        );
    }
}
