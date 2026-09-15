//! Shared observation identity, mutation visibility, and captured-event lifetime.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use seekdeep_identity::SessionId;
use seekdeep_python_sdk::{
    Client, Error, ErrorKind, EventBacking, HarnessConfig, Host, Notification, NotificationData,
    ObjectHandle, RunEvent, SeededIds,
};
use serde_json::{Map, Value, json};

fn client() -> Arc<Client> {
    let host = Host::native(
        Arc::new(|| Err(Error::new(ErrorKind::FileNotFound, "unused"))),
        Arc::new(|| Err(Error::new(ErrorKind::FileNotFound, "unused"))),
    );
    Client::new(
        HarnessConfig::default(),
        host,
        Arc::new(SeededIds::new([1; 16])),
    )
}

#[test]
fn matching_subscribers_receive_the_same_mutable_notification() {
    let client = client();
    let first = client.subscribe_notifications(None);
    let second = client.subscribe_notifications(None);
    client
        .handle_message(&json!({"method":"tick","params":{"value":1}}))
        .unwrap();
    let first = first.next().unwrap();
    let second = second.next().unwrap();
    assert!(first.same_object(&second));
    let mut value = first.read().unwrap();
    value.payload.insert("value".to_owned(), json!(2));
    first.replace(value).unwrap();
    assert_eq!(second.read().unwrap().payload["value"], 2);
}

#[test]
fn filter_mutations_are_visible_to_later_filters_and_queued_readers() {
    let client = client();
    let first = client.subscribe_notifications(Some(Arc::new(|notification| {
        let mut value = notification.read()?;
        value
            .payload
            .insert("sessionId".to_owned(), json!("changed"));
        notification.replace(value)?;
        Ok(true)
    })));
    let second = client.subscribe_session(SessionId::new("changed"));
    client
        .handle_message(
            &json!({"method":"session.event","params":{"sessionId":"original","event":{}}}),
        )
        .unwrap();
    let left = first.next().unwrap();
    let right = second.next().unwrap();
    assert!(left.same_object(&right));
    assert_eq!(right.read().unwrap().payload["sessionId"], "changed");
    assert_eq!(client.notification_count(), 0);
}

#[test]
fn replacing_the_current_event_does_not_retarget_an_already_captured_event() {
    let notification = Notification::new(NotificationData {
        method: "session.event".to_owned(),
        payload: json!({"event":{"value":"old"}})
            .as_object()
            .unwrap()
            .clone(),
    });
    let captured = notification.event().unwrap().unwrap();
    captured
        .replace(json!({"value":"mutated"}).as_object().unwrap().clone())
        .unwrap();
    assert_eq!(
        notification.read().unwrap().payload["event"]["value"],
        "mutated"
    );
    let mut replacement = notification.read().unwrap();
    replacement
        .payload
        .insert("event".to_owned(), json!({"value":"replacement"}));
    notification.replace(replacement).unwrap();
    assert!(!captured.same_object(&notification.event().unwrap().unwrap()));
    assert_eq!(captured.read().unwrap()["value"], "mutated");
    assert_eq!(
        notification.read().unwrap().payload["event"]["value"],
        "replacement"
    );
}

struct TrackedEvent(Arc<AtomicUsize>);
impl Drop for TrackedEvent {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
impl EventBacking for TrackedEvent {
    fn read(&self) -> seekdeep_python_sdk::Result<Map<String, Value>> {
        Ok(Map::new())
    }
    fn replace(&self, _: Map<String, Value>) -> seekdeep_python_sdk::Result<()> {
        Ok(())
    }
    fn object_handle(&self) -> Option<ObjectHandle> {
        Some(ObjectHandle { owner: 7, value: 8 })
    }
}

#[test]
fn a_foreign_backing_is_released_only_after_its_last_retained_handle() {
    let released = Arc::new(AtomicUsize::new(0));
    let event = RunEvent::from_backing(Arc::new(TrackedEvent(Arc::clone(&released))));
    let retained = event.clone();
    assert_eq!(
        retained.object_handle(),
        Some(ObjectHandle { owner: 7, value: 8 })
    );
    drop(event);
    assert_eq!(released.load(Ordering::SeqCst), 0);
    drop(retained);
    assert_eq!(released.load(Ordering::SeqCst), 1);
}
