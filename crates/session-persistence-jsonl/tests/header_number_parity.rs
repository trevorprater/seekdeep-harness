//! Signed numeric header tokens, duplicate keys, and the pinned metadata reader.

use seekdeep_session_persistence_jsonl::parse_header_meta;
use serde_json::{Value, json};

fn cases() -> Vec<String> {
    let mut lines = Vec::new();
    for field in ["createdAt", "delegationDepth"] {
        for number in ["0", "-0", "-0.0", "-0e0", "-1e-9999", "1"] {
            let other = if field == "createdAt" {
                "delegationDepth"
            } else {
                "createdAt"
            };
            lines.push(format!(
                r#"{{"type":"session","version":0,"id":"x","{field}":{number},"{other}":0}}"#,
            ));
        }
    }
    lines.extend([
        r#"{"type":"session","version":0,"id":"x","createdAt":-0,"createdAt":0,"delegationDepth":0}"#.to_owned(),
        r#"{"type":"session","version":0,"id":"x","createdAt":0,"createdAt":-0,"delegationDepth":0}"#.to_owned(),
        r#"{"type":"session","version":0,"id":"x","\u0063reatedAt":-0,"delegationDepth":0}"#.to_owned(),
        r#"{"type":"session","version":0,"id":"x","createdAt":0,"delegationDepth":0,"cwd":"/project/-0"}"#.to_owned(),
    ]);
    lines
}

#[test]
fn metadata_preserves_signed_zero_refusal_without_changing_last_key_wins() {
    for (index, line) in cases().iter().enumerate() {
        let accepted = parse_header_meta(line).unwrap().is_some();
        assert_eq!(
            accepted,
            matches!(index, 0 | 5 | 6 | 11 | 12 | 15),
            "{line}"
        );
    }
}

#[test]
#[ignore = "requires the pinned source checkout and Node TypeScript stripping"]
fn numeric_metadata_results_match_source() {
    let source = std::env::var("SEEKDEEP_PARITY_SOURCE").expect("SEEKDEEP_PARITY_SOURCE");
    let output = std::process::Command::new("node")
        .args(["--input-type=module", "-e"])
        .arg(r"import { pathToFileURL } from 'node:url';
const { parseHeaderMeta } = await import(pathToFileURL(process.argv[1] + '/packages/session/session-persistence-jsonl/src/format.ts'));
console.log(JSON.stringify(JSON.parse(process.argv[2]).map(line => parseHeaderMeta(line) ?? null)));" )
        .arg(source)
        .arg(serde_json::to_string(&cases()).unwrap())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let source: Value = serde_json::from_slice(&output.stdout).unwrap();
    let native = cases()
        .iter()
        .map(|line| parse_header_meta(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(json!(native), source);
}
