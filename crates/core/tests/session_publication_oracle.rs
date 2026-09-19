//! Session publication ordering and admission compared with the pinned source.

use std::{
    path::Path,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use parking_lot::Mutex;
use seekdeep_cordis::{Context, EventOptions, EventReply, fiber::EffectHandle};
use seekdeep_core::{
    session::{AppendOptions, Session, SessionEvent, SessionId},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_source_oracle::SourceOracle;
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum DetachPoint {
    None,
    Dispatch,
    Observer,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Scenario {
    name: &'static str,
    nested_type: &'static str,
    nested_data: Value,
    detach: DetachPoint,
}

#[derive(Clone, Default)]
struct Trace {
    heard: Vec<String>,
    errors: Vec<String>,
    order: Vec<String>,
}

#[derive(Serialize)]
struct Observation {
    events: Vec<SessionEvent>,
    heard: Vec<String>,
    errors: Vec<String>,
    order: Vec<String>,
    clock: u64,
    attached: bool,
}

fn scenarios() -> Vec<Scenario> {
    let mut result = Vec::new();
    for (name, nested_type, nested_data) in [
        ("valid", "todo/write", json!({"todos": []})),
        ("invalid-surface", "assistant/message", json!({})),
        ("legacy-delta", "request/header-delta", json!({})),
        (
            "legacy-fallback",
            "request/header",
            json!({"reason": "fallback"}),
        ),
    ] {
        for detach in [
            DetachPoint::None,
            DetachPoint::Dispatch,
            DetachPoint::Observer,
        ] {
            result.push(Scenario {
                name,
                nested_type,
                nested_data: nested_data.clone(),
                detach,
            });
        }
    }
    result
}

fn live_state(store: &SessionStore) -> &'static str {
    if store.get(&SessionId::new("publication")).is_some() {
        "live"
    } else {
        "detached"
    }
}

fn on(
    context: &Context,
    name: &str,
    observer: impl Fn(Context, seekdeep_cordis::EventArgs) -> anyhow::Result<EventReply>
    + Send
    + Sync
    + 'static,
) {
    context
        .events()
        .on_sync(context, name, observer, EventOptions::default())
        .unwrap();
}

fn register_dispatch(
    context: &Context,
    store: SessionStore,
    detach: EffectHandle,
    point: DetachPoint,
    trace: Arc<Mutex<Trace>>,
) {
    on(context, "internal/dispatch", move |_, args| {
        if args
            .get::<String>(1)
            .is_some_and(|name| name.as_str() == "session/event")
        {
            trace
                .lock()
                .order
                .push(format!("resolve:{}", live_state(&store)));
            if point == DetachPoint::Dispatch {
                futures::executor::block_on(detach.dispose())?;
            }
        }
        Ok(EventReply::Undefined)
    });
}

fn register_observers(
    context: &Context,
    store: &SessionStore,
    detach: &EffectHandle,
    scenario: &Scenario,
    trace: &Arc<Mutex<Trace>>,
) {
    let observer_store = store.clone();
    let observer_trace = trace.clone();
    let nested_type = scenario.nested_type;
    let nested_data = scenario.nested_data.clone();
    on(context, "session/event", move |_, args| {
        if args.get::<SessionEvent>(1).unwrap().event_type == "turn/start" {
            observer_trace
                .lock()
                .order
                .push(format!("attempt:{}", live_state(&observer_store)));
            if let Err(error) = args.get::<Session>(0).unwrap().append(
                nested_type,
                nested_data.clone(),
                AppendOptions::default(),
            ) {
                observer_trace.lock().errors.push(error.to_string());
                return Err(error.into());
            }
        }
        Ok(EventReply::Undefined)
    });

    let observer_store = store.clone();
    let observer_trace = trace.clone();
    let detach = detach.clone();
    let point = scenario.detach;
    on(context, "session/event", move |_, _| {
        if point == DetachPoint::Observer {
            observer_trace
                .lock()
                .order
                .push(format!("request-detach:{}", live_state(&observer_store)));
            futures::executor::block_on(detach.dispose())?;
        }
        Ok(EventReply::Undefined)
    });

    let observer_store = store.clone();
    let observer_trace = trace.clone();
    on(context, "session/event", move |_, args| {
        let mut trace = observer_trace.lock();
        trace
            .heard
            .push(args.get::<SessionEvent>(1).unwrap().event_type.clone());
        trace
            .order
            .push(format!("observe:{}", live_state(&observer_store)));
        Ok(EventReply::Undefined)
    });

    let observer_store = store.clone();
    let observer_trace = trace.clone();
    on(context, "session/disposed", move |_, args| {
        observer_trace
            .lock()
            .order
            .push(format!("dispose:{}", live_state(&observer_store)));
        args.get::<Session>(0).unwrap().append(
            "todo/write",
            json!({"todos": []}),
            AppendOptions::default(),
        )?;
        Ok(EventReply::Undefined)
    });
}

async fn observe(scenario: &Scenario) -> Observation {
    let context = Context::new();
    let clock = Arc::new(AtomicU64::new(100));
    let samples = clock.clone();
    let store = SessionStore::install_with_clock(
        &context,
        Arc::new(move || samples.fetch_add(1, Ordering::SeqCst)),
    )
    .unwrap();
    let session = store
        .prepare(
            Some(SessionId::new("publication")),
            CreateSessionOptions {
                created_at: Some(99),
                ..CreateSessionOptions::default()
            },
        )
        .unwrap();
    let detach = store.enter(&session).unwrap();
    store.announce(&session).unwrap();
    let trace = Arc::new(Mutex::new(Trace::default()));
    register_dispatch(
        &context,
        store.as_ref().clone(),
        detach.clone(),
        scenario.detach,
        trace.clone(),
    );
    register_observers(&context, &store, &detach, scenario, &trace);
    session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .unwrap();
    if scenario.detach == DetachPoint::None {
        session
            .append("todo/write", json!({"todos": []}), AppendOptions::default())
            .unwrap();
    }
    let trace = trace.lock().clone();
    let result = Observation {
        events: session.events(),
        heard: trace.heard,
        errors: trace.errors,
        order: trace.order,
        clock: clock.load(Ordering::SeqCst),
        attached: live_state(&store) == "live",
    };
    detach.dispose().await.unwrap();
    context.fiber().dispose().await.unwrap();
    result
}

fn source_observations(scenarios: &[Scenario]) -> Vec<Value> {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source = SourceOracle::open(&repository).unwrap();
    for file in [
        "packages/core/session/src/index.ts",
        "vendor/cordis/src/events.ts",
    ] {
        assert_eq!(
            source.read(file).unwrap(),
            std::fs::read_to_string(source.root().join(file)).unwrap()
        );
    }
    let fixture = tempfile::tempdir().unwrap();
    let input = fixture.path().join("scenarios.json");
    let output = fixture.path().join("observations.json");
    std::fs::write(&input, serde_json::to_vec(scenarios).unwrap()).unwrap();
    let command = Command::new("node")
        .arg("--experimental-transform-types")
        .arg(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/session-publication-oracle.mjs"),
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
        command.status.success(),
        "source publication oracle: {}",
        String::from_utf8_lossy(&command.stderr)
    );
    serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap()
}

#[tokio::test]
async fn publication_admission_order_and_disposal_match_the_pinned_source() {
    let scenarios = scenarios();
    let expected = source_observations(&scenarios);
    assert_eq!(scenarios.len(), expected.len());
    for (index, (scenario, expected)) in scenarios.iter().zip(expected).enumerate() {
        assert_eq!(
            serde_json::to_value(observe(scenario).await).unwrap(),
            expected,
            "case {index}: {}",
            scenario.name
        );
    }
}
