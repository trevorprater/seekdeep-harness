//! Frame parsing and serialized reload queue parity.

use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use futures::future::BoxFuture;
use parking_lot::Mutex;
use seekdeep_client_hmr::*;

struct TokioSpawner;

impl ClientHmrSpawner for TokioSpawner {
    fn spawn(&self, future: BoxFuture<'static, ()>) {
        tokio::spawn(future);
    }
}

struct FakePlatform {
    calls: Arc<Mutex<Vec<String>>>,
    gates: Mutex<VecDeque<Option<Arc<tokio::sync::Notify>>>>,
    failures: Mutex<VecDeque<Option<String>>>,
    active: Arc<AtomicUsize>,
    maximum: Arc<AtomicUsize>,
}

impl ClientHmrPlatform for FakePlatform {
    fn reload(&self, id: String) -> BoxFuture<'static, anyhow::Result<()>> {
        self.calls.lock().push(format!("start:{id}"));
        let gate = self.gates.lock().pop_front().flatten();
        let failure = self.failures.lock().pop_front().flatten();
        let calls = self.calls.clone();
        let active = self.active.clone();
        let maximum = self.maximum.clone();
        Box::pin(async move {
            let now = active.fetch_add(1, Ordering::AcqRel) + 1;
            maximum.fetch_max(now, Ordering::AcqRel);
            if let Some(gate) = gate {
                gate.notified().await;
            }
            active.fetch_sub(1, Ordering::AcqRel);
            calls.lock().push(format!("finish:{id}"));
            if let Some(failure) = failure {
                anyhow::bail!(failure);
            }
            Ok(())
        })
    }
}

#[test]
fn protocol_parses_known_frames_preserves_unknown_and_formats_sse() {
    assert!(matches!(
        parse_plugins_event_frame(&serde_json::json!({
            "type": "graph",
            "graph": {"rev": "r", "entries": []}
        }))
        .unwrap(),
        PluginsEventFrame::Graph { .. }
    ));
    assert_eq!(
        parse_plugins_event_frame(&serde_json::json!({
            "type": "rebuilt",
            "id": "a",
            "rev": "r2"
        }))
        .unwrap(),
        PluginsEventFrame::Rebuilt {
            id: "a".to_owned(),
            rev: "r2".to_owned(),
        }
    );
    assert!(matches!(
        parse_plugins_event_frame(&serde_json::json!({"type": "newer", "data": 1})).unwrap(),
        PluginsEventFrame::Unknown { frame_type: Some(ref kind), .. } if kind == "newer"
    ));
    assert!(parse_plugins_event_frame(&serde_json::json!({"type": "rebuilt"})).is_err());
    assert_eq!(
        sse_data(&serde_json::json!({"type": "rebuilt", "id": "a", "rev": "r"})),
        "data: {\"type\":\"rebuilt\",\"id\":\"a\",\"rev\":\"r\"}\n\n"
    );
}

#[tokio::test]
async fn rebuilt_frames_serialize_and_failures_do_not_wedge_later_work() {
    let first_gate = Arc::new(tokio::sync::Notify::new());
    let calls = Arc::new(Mutex::new(Vec::new()));
    let active = Arc::new(AtomicUsize::new(0));
    let maximum = Arc::new(AtomicUsize::new(0));
    let platform = Arc::new(FakePlatform {
        calls: calls.clone(),
        gates: Mutex::new(vec![Some(first_gate.clone()), None, None].into()),
        failures: Mutex::new(vec![Some("first failed".to_owned()), None, None].into()),
        active,
        maximum: maximum.clone(),
    });
    let logs = Arc::new(Mutex::new(Vec::new()));
    let observed = logs.clone();
    let runtime = ClientHmrRuntime::new(
        platform,
        Arc::new(TokioSpawner),
        Arc::new(move |message, error| observed.lock().push((message, error))),
    );
    runtime.handle(PluginsEventFrame::Graph {
        graph: serde_json::json!({}),
    });
    runtime.handle(PluginsEventFrame::Unknown {
        frame_type: Some("future".to_owned()),
        payload: serde_json::json!({}),
    });
    runtime.handle(PluginsEventFrame::Rebuilt {
        id: "a".to_owned(),
        rev: "1".to_owned(),
    });
    runtime.handle(PluginsEventFrame::Rebuilt {
        id: "b".to_owned(),
        rev: "2".to_owned(),
    });
    while calls.lock().is_empty() {
        tokio::task::yield_now().await;
    }
    assert_eq!(calls.lock().as_slice(), &["start:a"]);
    first_gate.notify_one();
    runtime.settled().await;
    assert_eq!(
        calls.lock().as_slice(),
        &["start:a", "finish:a", "start:b", "finish:b"]
    );
    assert_eq!(maximum.load(Ordering::Acquire), 1);
    assert_eq!(logs.lock().len(), 1);
    assert!(logs.lock()[0].0.contains("reload of \"a\" failed"));
    assert_eq!(logs.lock()[0].1.as_deref(), Some("first failed"));

    runtime.handle(PluginsEventFrame::Rebuilt {
        id: "c".to_owned(),
        rev: "3".to_owned(),
    });
    runtime.settled().await;
    assert!(
        calls
            .lock()
            .ends_with(&["start:c".to_owned(), "finish:c".to_owned()])
    );
}
