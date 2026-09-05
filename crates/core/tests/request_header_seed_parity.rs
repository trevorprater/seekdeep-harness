//! Durable request-header validation against the pinned Session boundary.

use seekdeep_core::session::{Session, SessionEvent, SessionId};
use serde_json::{Value, json};

fn cases() -> Vec<Value> {
    let base = json!({ "config": { "provider": "source", "model": "model" } });
    let mut cases = vec![
        base.clone(),
        json!({ "config": { "provider": "source", "model": "model" }, "tools": "{{tools}}", "messagePrefix": ["{{messagePrefix}}"] }),
        json!({ "config": { "provider": "source", "model": "model", "extension": { "opaque": true } }, "extension": 42 }),
        Value::Null,
        json!({ "config": null }),
        json!({ "config": { "provider": "", "model": "model" } }),
        json!({ "config": { "provider": "source", "model": 42 } }),
    ];
    for effort in [
        Value::Null,
        json!(false),
        json!(0),
        json!(""),
        json!("high"),
    ] {
        let mut header = base.clone();
        header["config"]["reasoningEffort"] = effort;
        cases.push(header);
    }
    for defaults in [
        Value::Null,
        json!([]),
        json!({}),
        json!({ "maxTokens": false }),
        json!({ "unknown": true }),
        json!({ "maxTokens": true }),
        json!({ "reasoningEffort": true }),
    ] {
        let mut header = base.clone();
        header["adapterDefaults"] = defaults;
        cases.push(header);
    }
    cases.push(json!({ "config": { "provider": "source", "model": "model", "maxTokens": null }, "adapterDefaults": { "maxTokens": true } }));
    cases.push(json!({ "config": { "provider": "source", "model": "model", "reasoningEffort": "high" }, "adapterDefaults": { "reasoningEffort": true } }));
    cases
}

fn observe(header: &Value) -> Value {
    let event: SessionEvent = serde_json::from_value(json!({
        "type": "request/header", "seq": 0, "time": 0,
        "data": { "header": header, "reason": "initial" },
    }))
    .unwrap();
    match Session::create(&SessionId::new("header-seed"), Some(vec![event]), None) {
        Ok(session) => json!({ "ok": true, "header": session.events()[0].data["header"] }),
        Err(error) => json!({ "ok": false, "error": error.to_string() }),
    }
}

#[test]
fn opaque_header_fields_survive_without_bypassing_source_configuration_guards() {
    let inputs = cases();
    for index in [0, 1, 2, 11, 14, 19, 20] {
        assert_eq!(
            observe(&inputs[index]),
            json!({ "ok": true, "header": inputs[index] }),
            "case {index}",
        );
    }
    for index in [3, 4, 5, 6, 7, 8, 9, 10, 12, 13, 15, 16, 17, 18] {
        assert_eq!(observe(&inputs[index])["ok"], false, "case {index}");
    }
}

#[test]
#[ignore = "requires the pinned source checkout and its installed Session package"]
fn seed_acceptance_rejections_and_round_trips_match_source() {
    let source = std::env::var("SEEKDEEP_PARITY_SOURCE").expect("SEEKDEEP_PARITY_SOURCE");
    let output = std::process::Command::new("node")
        .args(["--input-type=module", "-e"])
        .arg(
            r"import { pathToFileURL } from 'node:url';
const { Session } = await import(pathToFileURL(process.argv[1] + '/packages/core/session/lib/index.js'));
const results = JSON.parse(process.argv[2]).map(header => {
  try {
    const session = Session.create('header-seed', [{ type: 'request/header', seq: 0, time: 0, data: { header, reason: 'initial' } }]);
    return { ok: true, header: session.events[0].data.header };
  } catch (error) { return { ok: false, error: error.message }; }
});
console.log(JSON.stringify(results));",
        )
        .arg(source)
        .arg(serde_json::to_string(&cases()).unwrap())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let oracle: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    let actual = cases().iter().map(observe).collect::<Vec<_>>();
    assert_eq!(actual, oracle);
}
