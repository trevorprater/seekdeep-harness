//! Live event admission and durable seed validation against the pinned source.

use std::{
    path::Path,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use seekdeep_core::session::{AppendOptions, Session, SessionEvent, SessionHeader, SessionId};
use seekdeep_lossless_json::JsonValue;
use seekdeep_source_oracle::SourceOracle;
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Mode {
    Append,
    Seed,
}

#[derive(Serialize)]
struct Scenario {
    name: String,
    mode: Mode,
    event: SessionEvent,
    header: Option<SessionHeader>,
}

#[derive(Serialize)]
struct Observation {
    error: Option<String>,
    events: Option<Vec<SessionEvent>>,
    nodes: Option<Vec<u64>>,
    clock: u64,
}

fn payload_cases() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        ("header-empty", "request/header", json!({})),
        (
            "header-config-empty",
            "request/header",
            json!({"header":{"config":{}}}),
        ),
        (
            "header-invalid-reasoning",
            "request/header",
            json!({"header":{"config":{"provider":"p","model":"m","reasoningEffort":null}}}),
        ),
        (
            "header-invalid-defaults",
            "request/header",
            json!({"header":{"config":{"provider":"p","model":"m"},"adapterDefaults":[]}}),
        ),
        (
            "header-fallback-empty",
            "request/header",
            json!({"reason":"fallback"}),
        ),
        (
            "header-fallback-valid",
            "request/header",
            json!({"header":{"config":{"provider":"p","model":"m"}},"reason":"fallback"}),
        ),
        ("legacy-delta", "request/header-delta", json!({})),
        ("context-empty", "request/context", json!({})),
        ("user-empty", "user/message", json!({})),
        ("assistant-empty", "assistant/message", json!({})),
        ("tool-empty", "tool/result", json!({})),
        (
            "user-invalid-role",
            "user/message",
            json!({"id":"u","role":"assistant","source":{"kind":"user"},"content":[]}),
        ),
        (
            "user-invalid-content",
            "user/message",
            json!({"id":"u","role":"user","source":{"kind":"user"},"content":"bad"}),
        ),
        (
            "user-unknown-content",
            "user/message",
            json!({"id":"u","role":"user","source":{"kind":"user"},"content":[null,42,{"type":"future-block","nested":true}]}),
        ),
        (
            "user-extra-content-fields",
            "user/message",
            json!({"id":"u","role":"user","source":{"kind":"user"},"content":[{"type":"text","text":"x","extra":true}]}),
        ),
    ]
}

fn candidates() -> Vec<(String, SessionEvent)> {
    let mut events = Vec::new();
    for (name, event_type, data) in payload_cases() {
        let mut value = json!({"type":event_type,"seq":0,"time":90,"data":data});
        if matches!(
            event_type,
            "user/message" | "assistant/message" | "tool/result"
        ) {
            value["surfaceOp"] = json!("append");
        }
        events.push((name.to_owned(), serde_json::from_value(value).unwrap()));
    }
    let user = json!({"id":"u","role":"user","source":{"kind":"user"},"content":[]});
    for (name, metadata) in [
        ("missing-marker", json!({})),
        (
            "invalid-marker-invalid-provenance",
            json!({"surfaceOp":"invalid","sourceEventSeqs":[0]}),
        ),
        (
            "invalid-range-invalid-provenance",
            json!({"surfaceOp":{"op":"replace","start":5,"end":6},"sourceEventSeqs":[0]}),
        ),
        (
            "invalid-replace-marker-invalid-provenance",
            json!({"surfaceOp":{"op":"invalid","start":5,"end":6},"sourceEventSeqs":[0]}),
        ),
    ] {
        let mut value = json!({"type":"user/message","seq":0,"time":90,"data":user});
        value
            .as_object_mut()
            .unwrap()
            .extend(metadata.as_object().unwrap().clone());
        events.push((name.to_owned(), serde_json::from_value(value).unwrap()));
    }
    let mut plugin: SessionEvent =
        serde_json::from_value(json!({"type":"test/plugin","seq":0,"time":90,"data":null}))
            .unwrap();
    for (name, raw) in [
        ("negative-zero", "-0"),
        ("nonfinite", "1e999"),
        ("lossless-string", r#""\ud800""#),
    ] {
        plugin.data = seekdeep_lossless_json::JsonValue::parse(raw.to_owned()).unwrap();
        events.push((name.to_owned(), plugin.clone()));
    }
    plugin.data = json!({}).into();
    plugin.ignorable = Some(true);
    events.push(("ignorable-option".to_owned(), plugin));
    events
}

fn scenarios() -> Vec<Scenario> {
    let mut scenarios = Vec::new();
    for (name, event) in candidates() {
        for mode in [Mode::Append, Mode::Seed] {
            // The source exposes ignorable only on durable events, not append options.
            if event.ignorable.is_some() && mode == Mode::Append {
                continue;
            }
            scenarios.push(Scenario {
                name: name.clone(),
                mode,
                event: event.clone(),
                header: Some(SessionHeader::new_with_created_at(
                    SessionId::new("admission"),
                    99,
                )),
            });
        }
        for header in [
            None,
            Some(SessionHeader {
                version: 3,
                ..SessionHeader::new_with_created_at(SessionId::new("admission"), 99)
            }),
        ] {
            scenarios.push(Scenario {
                name: name.clone(),
                mode: Mode::Seed,
                event: event.clone(),
                header,
            });
        }
    }
    scenarios
}

fn observe(scenario: &Scenario) -> JsonValue {
    let clock = Arc::new(AtomicU64::new(100));
    let samples = clock.clone();
    let mut events = None;
    let mut nodes = None;
    let result = Session::create_with_clock(
        &SessionId::new("admission"),
        (scenario.mode == Mode::Seed).then(|| vec![scenario.event.clone()]),
        scenario.header.clone(),
        Arc::new(move || samples.fetch_add(1, Ordering::SeqCst)),
    )
    .and_then(|session| {
        let result = if scenario.mode == Mode::Append {
            session
                .append_json(
                    scenario.event.event_type.clone(),
                    scenario.event.data.clone(),
                    AppendOptions {
                        surface_op: scenario.event.surface_op.clone(),
                        source_event_seqs: scenario.event.source_event_seqs.clone(),
                        ignorable: scenario.event.ignorable.unwrap_or(false),
                    },
                )
                .map(|_| ())
        } else {
            Ok(())
        };
        events = Some(session.events());
        nodes = Some(session.surface_nodes());
        result
    });
    JsonValue::parse(
        serde_json::to_string(&Observation {
            error: result.err().map(|error| error.to_string()),
            events,
            nodes,
            clock: clock.load(Ordering::SeqCst),
        })
        .unwrap(),
    )
    .unwrap()
}

fn source_observations(scenarios: &[Scenario]) -> Vec<JsonValue> {
    let source = SourceOracle::open(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap();
    for file in [
        "packages/core/session/src/index.ts",
        "packages/core/session/src/surface.ts",
    ] {
        assert_eq!(
            source.read(file).unwrap(),
            std::fs::read_to_string(source.root().join(file)).unwrap()
        );
    }
    let temporary = tempfile::tempdir().unwrap();
    let input = temporary.path().join("scenarios.json");
    let output = temporary.path().join("observations.json");
    std::fs::write(&input, serde_json::to_vec(scenarios).unwrap()).unwrap();
    let result = Command::new("node")
        .arg("--experimental-transform-types")
        .arg(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/session-admission-oracle.mjs"),
        )
        .arg(source.root())
        .arg(input)
        .arg(&output)
        .env_remove("NODE_OPTIONS")
        .env_remove("NODE_PATH")
        .current_dir(source.root())
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "source admission oracle: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap()
}

#[test]
fn live_and_seed_admission_match_the_pinned_source() {
    let scenarios = scenarios();
    let expected = source_observations(&scenarios);
    assert_eq!(scenarios.len(), expected.len());
    let differences = scenarios
        .iter()
        .zip(expected)
        .enumerate()
        .filter_map(|(index, (scenario, expected))| {
            let actual = observe(scenario);
            (actual != expected).then(|| {
                format!(
                    "case {index} {}:\nactual {}\nsource {}",
                    scenario.name,
                    actual.as_raw(),
                    expected.as_raw()
                )
            })
        })
        .collect::<Vec<_>>();
    assert!(
        differences.is_empty(),
        "{} differences:\n{}",
        differences.len(),
        differences.join("\n")
    );
}
