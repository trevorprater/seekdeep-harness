//! Compare mutable observation identities with the pinned Python implementation.

use seekdeep_python_sdk::{
    Client, Error, ErrorKind, HarnessConfig, Host, Notification, NotificationData, SeededIds,
};
use serde_json::{Value, json};
use std::{path::PathBuf, process::Command, sync::Arc};

const ORACLE: &str = r#"
import json
from deepseek_harness import HarnessClient, Notification
client = HarnessClient()
with client.subscribe_notifications() as a, client.subscribe_notifications() as b:
    client._handle_message({"method":"tick","params":{"value":1}})
    left, right = a.next(), b.next()
    shared = left is right
    left.payload["value"] = 2
    value = right.payload["value"]
n = Notification("session.event", {"event":{"value":"old"}})
captured = n.payload["event"]
captured["value"] = "mutated"
before = n.payload["event"]["value"]
n.payload["event"] = {"value":"replacement"}
print(json.dumps({"shared":shared,"value":value,"before":before,"captured":captured["value"],"current":n.payload["event"]["value"]}))
"#;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let source = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("pinned source root required")?,
    )
    .canonicalize()?;
    let head = Command::new("git")
        .arg("-C")
        .arg(&source)
        .args(["rev-parse", "HEAD"])
        .output()?;
    let pin = include_str!("../../../SOURCE_SNAPSHOT")
        .lines()
        .find_map(|line| line.strip_prefix("commit="))
        .ok_or("source pin absent")?;
    if !head.status.success() || String::from_utf8_lossy(&head.stdout).trim() != pin {
        return Err("oracle differs from SOURCE_SNAPSHOT".into());
    }
    let output = Command::new(source.join("python/sdk/.venv/bin/python"))
        .args(["-c", ORACLE])
        .env("PYTHONPATH", source.join("python/sdk/src"))
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    let expected: Value = serde_json::from_slice(&output.stdout)?;
    let host = Host::native(
        Arc::new(|| Err(Error::new(ErrorKind::FileNotFound, "unused"))),
        Arc::new(|| Err(Error::new(ErrorKind::FileNotFound, "unused"))),
    );
    let client = Client::new(
        HarnessConfig::default(),
        host,
        Arc::new(SeededIds::new([1; 16])),
    );
    let a = client.subscribe_notifications(None);
    let b = client.subscribe_notifications(None);
    client.handle_message(&json!({"method":"tick","params":{"value":1}}))?;
    let left = a.next()?;
    let right = b.next()?;
    let shared = left.same_object(&right);
    let mut data = left.read()?;
    data.payload.insert("value".to_owned(), json!(2));
    left.replace(data)?;
    let value = right.read()?.payload["value"].clone();
    let n = Notification::new(NotificationData {
        method: "session.event".to_owned(),
        payload: json!({"event":{"value":"old"}})
            .as_object()
            .unwrap()
            .clone(),
    });
    let captured = n.event()?.ok_or("event absent")?;
    captured.replace(json!({"value":"mutated"}).as_object().unwrap().clone())?;
    let before = n.read()?.payload["event"]["value"].clone();
    let mut data = n.read()?;
    data.payload
        .insert("event".to_owned(), json!({"value":"replacement"}));
    n.replace(data)?;
    let actual = json!({"shared":shared,"value":value,"before":before,"captured":captured.read()?["value"],"current":n.read()?.payload["event"]["value"]});
    if actual != expected {
        return Err(format!("observation difference: native={actual}, source={expected}").into());
    }
    println!("notification identity and captured-event mutations match the pinned Python source");
    Ok(())
}
