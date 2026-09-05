//! Durable projection state for dynamic runtime context.

use std::{collections::HashSet, sync::Arc};

use parking_lot::Mutex;
use seekdeep_cordis::{Context, EventOptions, EventReply, fiber::EffectHandle};
use seekdeep_core::session::{Session, SessionEvent, is_replacement_surface_event};
use seekdeep_llm::{ContentBlock, ContextSnapshotSection, MessageSource, UserMessage};
use serde_json::Value;

const SOURCE: &str = "@seekdeep-ai/seekdeep-system-prompt";
const CLEARED: &str =
    "Current runtime context: none. Earlier runtime-context snapshots no longer apply.";

#[derive(Clone, Debug, PartialEq, Eq)]
struct Retained {
    seq: u64,
    text: Option<String>,
}

/// `None` means no snapshot ever existed; `Some(None)` means none is retained.
type RetainedState = Option<Option<Retained>>;

/// Tracks the last retained runtime-context snapshot without owning its commit.
#[derive(Debug)]
pub struct RuntimeContextProjection {
    retained: Arc<Mutex<RetainedState>>,
    _listener: EffectHandle,
}

impl RuntimeContextProjection {
    /// Restores projection state once, then follows authoritative session events.
    ///
    /// # Errors
    ///
    /// Returns when the session-event listener cannot be registered.
    pub fn new(context: &Context, session: &Arc<Session>) -> anyhow::Result<Self> {
        let surface = session.surface_nodes().into_iter().collect::<HashSet<_>>();
        let mut retained: RetainedState = None;
        for event in session.events().into_iter().rev() {
            if event.event_type != "user/message" {
                continue;
            }
            let Ok(message) = serde_json::from_value::<UserMessage>(event.data) else {
                continue;
            };
            if !is_owned(&message) {
                continue;
            }
            retained.get_or_insert(None);
            if surface.contains(&event.seq) {
                retained = Some(Some(Retained {
                    seq: event.seq,
                    text: text_of(&message),
                }));
                break;
            }
        }
        let retained = Arc::new(Mutex::new(retained));
        let listener_retained = retained.clone();
        let expected_session = session.clone();
        let listener = context.events().on_sync(
            context,
            "session/event",
            move |_, args| {
                let Some(subject) = args.get::<Session>(0) else {
                    return Ok(EventReply::Undefined);
                };
                if !Arc::ptr_eq(&subject, &expected_session) {
                    return Ok(EventReply::Undefined);
                }
                let Some(event) = args.get::<SessionEvent>(1) else {
                    return Ok(EventReply::Undefined);
                };
                update_retained(&mut listener_retained.lock(), &event);
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )?;
        Ok(Self {
            retained,
            _listener: listener,
        })
    }

    /// Creates an uncommitted snapshot only when the retained value differs.
    #[must_use]
    pub fn project(
        &self,
        current: &str,
        sections: &[ContextSnapshotSection],
    ) -> Option<UserMessage> {
        let retained = self.retained.lock();
        if retained.is_none() && current.is_empty() {
            return None;
        }
        let snapshot = if current.is_empty() { CLEARED } else { current };
        if retained
            .as_ref()
            .and_then(Option::as_ref)
            .and_then(|retained| retained.text.as_deref())
            == Some(snapshot)
        {
            return None;
        }
        let mut source = MessageSource::plugin(SOURCE);
        if !sections.is_empty() {
            source
                .fields
                .insert("form".to_owned(), Value::String("snapshot".to_owned()));
            if let Ok(value) = serde_json::to_value(sections) {
                source.fields.insert("sections".to_owned(), value);
            }
        }
        Some(UserMessage::new(
            vec![ContentBlock::Text {
                text: snapshot.to_owned(),
            }],
            source,
        ))
    }
}

fn update_retained(retained: &mut RetainedState, event: &SessionEvent) {
    if event.event_type == "user/message"
        && let Ok(message) = serde_json::from_value::<UserMessage>(event.data.clone())
        && is_owned(&message)
    {
        *retained = Some(Some(Retained {
            seq: event.seq,
            text: text_of(&message),
        }));
        return;
    }
    let Some(Some(current)) = retained.as_ref() else {
        return;
    };
    if is_replacement_surface_event(event)
        && event
            .source_event_seqs
            .as_ref()
            .is_some_and(|sources| sources.contains(&current.seq))
    {
        *retained = Some(None);
    }
}

fn is_owned(message: &UserMessage) -> bool {
    message.source().kind == "plugin"
        && message
            .source()
            .fields
            .get("plugin")
            .and_then(Value::as_str)
            == Some(SOURCE)
}

fn text_of(message: &UserMessage) -> Option<String> {
    match message.content() {
        [ContentBlock::Text { text }] => Some(text.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use seekdeep_core::{
        session::{AppendOptions, SessionId, SurfaceOp},
        session_store::{CreateSessionOptions, SessionStore},
    };

    use super::*;

    fn append_snapshot(session: &Session, text: &str) -> SessionEvent {
        let message = UserMessage::new(
            vec![ContentBlock::Text {
                text: text.to_owned(),
            }],
            MessageSource::plugin(SOURCE),
        );
        session
            .append(
                "user/message",
                serde_json::to_value(message).expect("message"),
                AppendOptions {
                    surface_op: Some(SurfaceOp::append()),
                    ..AppendOptions::default()
                },
            )
            .expect("append")
    }

    #[tokio::test]
    async fn projects_changes_clear_marker_and_live_retention() {
        let context = Context::new();
        let store = SessionStore::install(&context).expect("store");
        let session = store
            .prepare(
                Some(SessionId::new("runtime-context")),
                CreateSessionOptions::default(),
            )
            .expect("prepare");
        let detach = store.enter(&session).expect("enter");
        store.announce(&session).expect("announce");
        let projection = RuntimeContextProjection::new(&context, &session).expect("projection");

        assert!(projection.project("", &[]).is_none());
        let first = projection.project("cwd: /tmp", &[]).expect("first");
        assert_eq!(text_of(&first).as_deref(), Some("cwd: /tmp"));
        session
            .append(
                "user/message",
                serde_json::to_value(first).expect("first"),
                AppendOptions {
                    surface_op: Some(SurfaceOp::append()),
                    ..AppendOptions::default()
                },
            )
            .expect("commit first");
        assert!(projection.project("cwd: /tmp", &[]).is_none());
        let cleared = projection.project("", &[]).expect("cleared");
        assert_eq!(text_of(&cleared).as_deref(), Some(CLEARED));
        detach.dispose().await.expect("detach");
    }

    #[tokio::test]
    async fn reconstructs_retained_snapshot_and_notices_replacement() {
        let context = Context::new();
        let store = SessionStore::install(&context).expect("store");
        let session = store
            .prepare(
                Some(SessionId::new("replacement")),
                CreateSessionOptions::default(),
            )
            .expect("prepare");
        let prior = append_snapshot(&session, "old");
        let detach = store.enter(&session).expect("enter");
        store.announce(&session).expect("announce");
        let projection = RuntimeContextProjection::new(&context, &session).expect("projection");
        assert!(projection.project("old", &[]).is_none());

        let replacement = UserMessage::new(
            vec![ContentBlock::Text {
                text: "ordinary".to_owned(),
            }],
            MessageSource::user(),
        );
        session
            .append(
                "user/message",
                serde_json::to_value(replacement).expect("replacement"),
                AppendOptions {
                    surface_op: Some(SurfaceOp::replace(prior.seq, prior.seq)),
                    source_event_seqs: Some(vec![prior.seq]),
                    ..AppendOptions::default()
                },
            )
            .expect("replace");
        assert!(projection.project("", &[]).is_some());
        detach.dispose().await.expect("detach");
    }
}
