//! Full generated-Client fake roster and stream-control parity.

#[path = "support/fake_api.rs"]
mod fake_api;

use fake_api::{FakeApiClient, METHOD_ROSTER, StreamItem};
use seekdeep_client_runtime::{ClientRpcError, ClientRpcResult};
use serde_json::json;

#[test]
fn every_source_method_has_a_recorded_ok_default_and_scripted_failures_override_once() {
    futures::executor::block_on(async {
        let fake = FakeApiClient::default();
        for method in METHOD_ROSTER {
            let payload = json!({
                "provider":"provider","model":"model","agentPreset":"preset","sessionId":"s1"
            });
            assert!(
                fake.call(method, payload).await.unwrap().is_ok(),
                "{method}"
            );
        }
        assert_eq!(
            fake.calls_of("session.list"),
            [json!({
                "provider":"provider","model":"model","agentPreset":"preset","sessionId":"s1"
            })]
        );
        let denied = ClientRpcResult::Failure(ClientRpcError {
            code: "denied".to_owned(),
            message: "no".to_owned(),
            details: serde_json::Map::new(),
        });
        fake.script("session.create", Ok(denied.clone()));
        assert_eq!(
            fake.call("session.create", json!({})).await.unwrap(),
            denied
        );
        assert!(
            fake.call("session.create", json!({}))
                .await
                .unwrap()
                .is_ok()
        );
        fake.script("session.create", Err("wire down".to_owned()));
        assert_eq!(
            fake.call("session.create", json!({})).await.unwrap_err(),
            "wire down"
        );
        fake.search(json!({"query":"x"}), Some("signal-1"))
            .await
            .unwrap();
        assert_eq!(
            fake.last_search_signal.borrow().as_deref(),
            Some("signal-1")
        );
    });
}

#[test]
fn stream_hubs_replicate_frames_and_control_open_end_and_failure_paths() {
    let fake = FakeApiClient::default();
    *fake.mux.hold_open.borrow_mut() = true;
    let first = fake.mux.open(true);
    let second = fake.mux.open(true);
    assert_eq!(fake.mux.connection_count(), 2);
    assert_eq!(fake.mux.fired_opens(), 0);
    fake.mux.release_opens();
    assert_eq!(fake.mux.fired_opens(), 2);
    fake.mux.push(&json!({"type":"session/event"}));
    fake.mux.end();
    fake.mux.fail("stream failed");
    for reader in [first, second] {
        let items = reader.borrow().iter().cloned().collect::<Vec<_>>();
        assert!(matches!(&items[0], StreamItem::Frame(frame) if frame["type"] == "session/event"));
        assert_eq!(items[1], StreamItem::End);
        assert_eq!(items[2], StreamItem::Failure("stream failed".to_owned()));
    }
    *fake.host.suppress_open.borrow_mut() = true;
    fake.host.open(true);
    assert_eq!(fake.host.fired_opens(), 0);
}
