//! Future-format and unknown-required-event refusal diagnostics.

use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{SESSION_FORMAT_VERSION, SessionEvent, SessionHeader, SessionId},
    session_store::SessionStore,
};
use seekdeep_session_persistence::SessionPersistence as _;
use seekdeep_session_persistence_jsonl::{JsonlCompression, JsonlConfig, JsonlSessionPersistence};
use serde_json::json;

fn event(event_type: &str, seq: u64, data: serde_json::Value) -> SessionEvent {
    SessionEvent {
        event_type: event_type.to_owned(),
        seq,
        time: i64::try_from(seq).expect("fixture sequence fits i64") + 1,
        data,
        source_event_seqs: None,
        surface_op: None,
        ignorable: None,
    }
}

fn backend(
    root: &std::path::Path,
) -> anyhow::Result<(Context, std::sync::Arc<JsonlSessionPersistence>)> {
    let context = Context::new();
    let sessions = SessionStore::install(&context)?;
    let mut config = JsonlConfig::new(root);
    config.compression = JsonlCompression::None;
    let persistence = JsonlSessionPersistence::new(sessions, config)?;
    Ok((context, persistence))
}

fn assert_raw_path(error: &anyhow::Error, expected_path: &std::path::Path) {
    assert!(expected_path.is_file());
    assert!(error.to_string().contains("(raw log: "));
    assert!(
        error
            .to_string()
            .contains(&expected_path.to_string_lossy().to_string())
    );
}

#[tokio::test]
async fn future_format_names_upgrade_direction_and_raw_log() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let (context, persistence) = backend(temporary.path())?;
    let id = SessionId::new("future-format");
    let mut header = SessionHeader::new(id.clone());
    header.cwd = Some(temporary.path().to_string_lossy().into_owned());
    persistence.create(&header).await?;
    persistence
        .append(
            &id,
            &[
                event("turn/start", 0, json!({"turn":1})),
                event(
                    "turn/end",
                    1,
                    json!({"turn":1,"reason":{"kind":"completed"}}),
                ),
            ],
        )
        .await?;
    let path = persistence.locate(&header).expect("JSONL location").path;
    let raw = std::fs::read_to_string(&path)?;
    std::fs::write(
        &path,
        raw.replacen(
            &format!("\"version\":{SESSION_FORMAT_VERSION}"),
            &format!("\"version\":{}", SESSION_FORMAT_VERSION + 99),
            1,
        ),
    )?;
    let error = persistence.load(&id).await.unwrap_err();
    assert!(error.to_string().contains(&format!(
        "session \"future-format\" uses log format v{}, but this harness reads only v{SESSION_FORMAT_VERSION}: the log was written by a newer harness — upgrade the harness to open it",
        SESSION_FORMAT_VERSION + 99
    )));
    assert_raw_path(&error, &path);
    context.fiber().dispose().await?;
    Ok(())
}

#[tokio::test]
async fn unknown_required_event_names_type_sequence_and_raw_log() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let (context, persistence) = backend(temporary.path())?;
    let id = SessionId::new("future-event");
    let mut header = SessionHeader::new(id.clone());
    header.cwd = Some(temporary.path().to_string_lossy().into_owned());
    persistence.create(&header).await?;
    persistence
        .append(
            &id,
            &[
                event("turn/start", 0, json!({"turn":1})),
                event(
                    "turn/end",
                    1,
                    json!({"turn":1,"reason":{"kind":"completed"}}),
                ),
                event("future/event", 2, json!({"payload":1})),
            ],
        )
        .await?;
    let path = persistence.locate(&header).expect("JSONL location").path;
    let error = persistence.load(&id).await.unwrap_err();
    assert!(error.to_string().contains(
        "session \"future-event\" contains event type \"future/event\" (seq 2) unknown to this harness and not marked ignorable; refusing to interpret the log — it was likely written by a newer harness"
    ));
    assert_raw_path(&error, &path);
    context.fiber().dispose().await?;
    Ok(())
}
