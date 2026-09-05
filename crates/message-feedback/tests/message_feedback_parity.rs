//! Behavioral mirror of the message-feedback service source suite.

mod support;

use std::sync::Arc;

use futures::FutureExt as _;
use seekdeep_cordis::{EventOptions, EventReply};
use seekdeep_message_feedback::{
    MessageFeedbackDeleteRequest, MessageFeedbackFailure, MessageFeedbackListRequest,
    MessageFeedbackPutRequest, MessageFeedbackRating, MessageFeedbackRow,
    MessageFeedbackSessionIdentity, MessageFeedbackVersion, config_schema,
    validate_message_feedback_row,
};
use seekdeep_typert_protocol::TypertRemoteService;
use serde_json::json;
use tokio::sync::Notify;

use support::{cold_fixture, inspection, live_fixture, setup};

fn put(
    fixture: &support::MessageFixture,
    index: usize,
    rating: MessageFeedbackRating,
    note: Option<&str>,
    version: Option<MessageFeedbackVersion>,
) -> MessageFeedbackPutRequest {
    MessageFeedbackPutRequest {
        session_id: fixture.session.id().clone(),
        message_id: fixture.assistant_message_ids[index].clone(),
        rating,
        note: note.map(str::to_owned),
        if_version: version,
    }
}

#[tokio::test]
async fn plugin_config_service_and_remote_namespace_are_exact() {
    assert!(
        config_schema()
            .resolve(&json!({"maxNoteBytes": 32}))
            .is_ok()
    );
    for value in [
        json!({}),
        json!({"maxNoteBytes": 0}),
        json!({"maxNoteBytes": 1.5}),
        json!({"maxNoteBytes": 9_007_199_254_740_992_u64}),
    ] {
        assert!(config_schema().resolve(&value).is_err(), "{value}");
    }
    let root = tempfile::tempdir().unwrap();
    let harness = setup(root.path(), 32, true).await;
    assert_eq!(seekdeep_message_feedback::NAME, "message-feedback");
    assert_eq!(
        seekdeep_message_feedback::INJECT,
        ["storageDomain", "sessionPersistence", "sessions"]
    );
    let binding = harness.service.typert_remote().unwrap();
    assert_eq!(binding.service_key, "messageFeedback");
    assert_eq!(binding.namespace, "messageFeedback");
    assert_eq!(
        <seekdeep_message_feedback::MessageFeedbackService as TypertRemoteService>::remote_methods(
            &harness.service
        )
        .iter()
        .map(|method| method.export_name.as_deref().unwrap_or(&method.method))
        .collect::<Vec<_>>(),
        ["list", "put", "delete"]
    );
    assert_eq!(
        harness
            .service
            .list_remote(MessageFeedbackListRequest {
                session_id: seekdeep_core::session::SessionId::new("missing"),
            })
            .await
            .unwrap(),
        json!({"ok": false, "error": {"code": "session-not-found", "sessionId": "missing"}})
    );
}

#[tokio::test]
async fn definite_absence_inspection_failures_and_live_ownership_races_stay_distinct() {
    let root = tempfile::tempdir().unwrap();
    let harness = setup(root.path(), 64, true).await;
    let missing = seekdeep_core::session::SessionId::new("missing");
    assert_eq!(
        harness.service.list(&missing).await.unwrap(),
        Err(MessageFeedbackFailure::SessionNotFound {
            session_id: missing.clone()
        })
    );

    let cold = cold_fixture("catalogued", 10, None);
    harness.persistence.set_durable(inspection(&cold));
    harness
        .persistence
        .set_inspect_failure(Some("catalogued inspect failed"));
    let error = harness.service.list(cold.session.id()).await.unwrap_err();
    assert!(error.to_string().contains("catalogued inspect failed"));
    harness.persistence.set_inspect_failure(None);

    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    harness.persistence.on_list_snapshots(Some(Arc::new({
        let started = started.clone();
        let release = release.clone();
        move || {
            let started = started.clone();
            let release = release.clone();
            async move {
                started.notify_one();
                release.notified().await;
                Ok(())
            }
            .boxed()
        }
    })));
    let service = harness.service.clone();
    let raced_id = seekdeep_core::session::SessionId::new("raced-live");
    let task = tokio::spawn({
        let raced_id = raced_id.clone();
        async move { service.list(&raced_id).await }
    });
    started.notified().await;
    let raced = live_fixture(&harness, raced_id.as_str());
    release.notify_one();
    assert_eq!(task.await.unwrap().unwrap().unwrap(), Vec::new());
    assert_eq!(raced.session.id(), &raced_id);
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // grouped source cases share one durable fixture and revision chain
async fn create_noop_update_limits_and_target_validation_match_the_public_contract() {
    let root = tempfile::tempdir().unwrap();
    let harness = setup(root.path(), 8, true).await;
    let fixture = cold_fixture("feedback-basic", 20, Some("/workspace"));
    harness.persistence.set_durable(inspection(&fixture));

    let before_inspects = harness
        .persistence
        .inspect_calls
        .load(std::sync::atomic::Ordering::Acquire);
    assert_eq!(
        harness
            .service
            .put(put(
                &fixture,
                0,
                MessageFeedbackRating::Positive,
                Some("   "),
                None,
            ))
            .await
            .unwrap(),
        Err(MessageFeedbackFailure::NoteBlank)
    );
    assert_eq!(
        harness
            .service
            .put(put(
                &fixture,
                0,
                MessageFeedbackRating::Positive,
                Some("💥💥💥"),
                None,
            ))
            .await
            .unwrap(),
        Err(MessageFeedbackFailure::NoteTooLarge {
            max_bytes: 8,
            actual_bytes: 12,
        })
    );
    assert_eq!(
        harness
            .persistence
            .inspect_calls
            .load(std::sync::atomic::Ordering::Acquire),
        before_inspects
    );

    let created = harness
        .service
        .put(put(
            &fixture,
            0,
            MessageFeedbackRating::Positive,
            Some("good"),
            None,
        ))
        .await
        .unwrap()
        .unwrap();
    let listed = harness
        .service
        .list(fixture.session.id())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(listed.as_slice(), std::slice::from_ref(&created));
    let noop = harness
        .service
        .put(put(
            &fixture,
            0,
            MessageFeedbackRating::Positive,
            Some("good"),
            Some(created.version.clone()),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(noop, created);
    let updated = harness
        .service
        .put(put(
            &fixture,
            0,
            MessageFeedbackRating::Negative,
            None,
            Some(created.version.clone()),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_ne!(updated.version, created.version);
    assert_eq!(updated.created_at, created.created_at);
    assert!(updated.updated_at >= created.updated_at);
    assert_eq!(
        harness
            .service
            .put(put(
                &fixture,
                0,
                MessageFeedbackRating::Positive,
                None,
                Some(created.version),
            ))
            .await
            .unwrap(),
        Err(MessageFeedbackFailure::VersionConflict {
            current: Some(updated)
        })
    );

    for message_id in [
        fixture.user_message_id.clone(),
        fixture.empty_assistant_message_id.clone(),
        fixture.replacement_assistant_message_id.clone(),
    ] {
        let mut invalid = put(&fixture, 1, MessageFeedbackRating::Positive, None, None);
        invalid.message_id = message_id.clone();
        assert_eq!(
            harness.service.put(invalid).await.unwrap(),
            Err(MessageFeedbackFailure::TargetNotFound {
                session_id: fixture.session.id().clone(),
                message_id,
            })
        );
    }
    let observed_absent = MessageFeedbackVersion("observed-absent".to_owned());
    assert_eq!(
        harness
            .service
            .put(put(
                &fixture,
                1,
                MessageFeedbackRating::Positive,
                None,
                Some(observed_absent.clone()),
            ))
            .await
            .unwrap(),
        Err(MessageFeedbackFailure::VersionConflict { current: None })
    );
    harness
        .service
        .delete(MessageFeedbackDeleteRequest {
            session_id: fixture.session.id().clone(),
            message_id: fixture.assistant_message_ids[1].clone(),
            if_version: observed_absent,
        })
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one sequence exposes serialization and both ABA boundaries
async fn per_session_serialisation_versions_and_delete_recreate_aba_are_stable() {
    let root = tempfile::tempdir().unwrap();
    let harness = setup(root.path(), 64, true).await;
    let fixture = cold_fixture("feedback-concurrency", 30, None);
    harness.persistence.set_durable(inspection(&fixture));
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let first = Arc::new(std::sync::atomic::AtomicBool::new(true));
    harness.persistence.on_read_from(Some(Arc::new({
        let started = started.clone();
        let release = release.clone();
        let first = first.clone();
        move || {
            let started = started.clone();
            let release = release.clone();
            let should_block = first.swap(false, std::sync::atomic::Ordering::AcqRel);
            async move {
                if should_block {
                    started.notify_one();
                    release.notified().await;
                }
                Ok(())
            }
            .boxed()
        }
    })));
    let first_put = tokio::spawn({
        let service = harness.service.clone();
        let request = put(&fixture, 0, MessageFeedbackRating::Positive, None, None);
        async move { service.put(request).await }
    });
    started.notified().await;
    let second_put = tokio::spawn({
        let service = harness.service.clone();
        let request = put(&fixture, 1, MessageFeedbackRating::Negative, None, None);
        async move { service.put(request).await }
    });
    release.notify_one();
    let first_item = first_put.await.unwrap().unwrap().unwrap();
    let second_item = second_put.await.unwrap().unwrap().unwrap();
    assert_ne!(first_item.version, second_item.version);
    assert_eq!(
        harness
            .service
            .list(fixture.session.id())
            .await
            .unwrap()
            .unwrap()
            .len(),
        2
    );

    harness
        .service
        .delete(MessageFeedbackDeleteRequest {
            session_id: fixture.session.id().clone(),
            message_id: first_item.message_id.clone(),
            if_version: first_item.version.clone(),
        })
        .await
        .unwrap()
        .unwrap();
    harness
        .service
        .delete(MessageFeedbackDeleteRequest {
            session_id: fixture.session.id().clone(),
            message_id: first_item.message_id.clone(),
            if_version: first_item.version.clone(),
        })
        .await
        .unwrap()
        .unwrap();
    let recreated = harness
        .service
        .put(put(
            &fixture,
            0,
            MessageFeedbackRating::Positive,
            None,
            None,
        ))
        .await
        .unwrap()
        .unwrap();
    assert_ne!(recreated.version, first_item.version);
    let negative = harness
        .service
        .put(put(
            &fixture,
            0,
            MessageFeedbackRating::Negative,
            None,
            Some(recreated.version.clone()),
        ))
        .await
        .unwrap()
        .unwrap();
    let positive_again = harness
        .service
        .put(put(
            &fixture,
            0,
            MessageFeedbackRating::Positive,
            None,
            Some(negative.version),
        ))
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        harness
            .service
            .put(put(
                &fixture,
                0,
                MessageFeedbackRating::Positive,
                None,
                Some(recreated.version.clone()),
            ))
            .await
            .unwrap(),
        Err(MessageFeedbackFailure::VersionConflict { current: Some(current) })
            if current.version == positive_again.version
    ));
    assert!(matches!(
        harness
            .service
            .delete(MessageFeedbackDeleteRequest {
                session_id: fixture.session.id().clone(),
                message_id: first_item.message_id,
                if_version: first_item.version,
            })
            .await
            .unwrap(),
        Err(MessageFeedbackFailure::VersionConflict { .. })
    ));
}

#[tokio::test]
async fn reused_session_identity_hides_stale_rows_and_starts_cleanly() {
    let root = tempfile::tempdir().unwrap();
    let harness = setup(root.path(), 64, true).await;
    let old = cold_fixture("reused-session", 40, Some("/old"));
    harness.persistence.set_durable(inspection(&old));
    harness
        .service
        .put(put(&old, 0, MessageFeedbackRating::Positive, None, None))
        .await
        .unwrap()
        .unwrap();
    let replacement = harness
        .sessions
        .create(
            &harness.context,
            Some(old.session.id().clone()),
            seekdeep_core::session_store::CreateSessionOptions {
                created_at: Some(41),
                cwd: Some("/new".to_owned()),
                ..Default::default()
            },
        )
        .unwrap();
    let replacement = support::append_message_fixture(replacement);
    assert_eq!(
        harness
            .service
            .list(replacement.session.id())
            .await
            .unwrap()
            .unwrap(),
        []
    );
    harness.persistence.persist(&replacement.session);
    let new_item = harness
        .service
        .put(put(
            &replacement,
            0,
            MessageFeedbackRating::Negative,
            None,
            None,
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(new_item.rating, MessageFeedbackRating::Negative);
}

#[tokio::test]
async fn durability_barrier_rejects_missing_physical_targets_and_missing_participants() {
    let root = tempfile::tempdir().unwrap();
    let harness = setup(root.path(), 64, true).await;
    let logical = cold_fixture("logical-only", 50, None);
    harness.persistence.set_logical(inspection(&logical));
    harness
        .persistence
        .set_durable(seekdeep_session_persistence::SessionInspection {
            meta: logical.session.header().clone(),
            events: Vec::new(),
        });
    assert_eq!(
        harness
            .service
            .put(put(
                &logical,
                0,
                MessageFeedbackRating::Positive,
                None,
                None,
            ))
            .await
            .unwrap(),
        Err(MessageFeedbackFailure::TargetNotFound {
            session_id: logical.session.id().clone(),
            message_id: logical.assistant_message_ids[0].clone(),
        })
    );

    let no_flush_root = tempfile::tempdir().unwrap();
    let no_flush = setup(no_flush_root.path(), 64, false).await;
    let live = live_fixture(&no_flush, "no-flush");
    let error = no_flush
        .service
        .put(put(&live, 0, MessageFeedbackRating::Positive, None, None))
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("no durability listener participated")
    );

    let no_physical_root = tempfile::tempdir().unwrap();
    let no_physical = setup(no_physical_root.path(), 64, false).await;
    no_physical
        .context
        .events()
        .on_sync(
            &no_physical.context,
            "session/flush",
            |_, _| Ok(EventReply::Undefined),
            EventOptions::default(),
        )
        .unwrap();
    let live = live_fixture(&no_physical, "not-physical");
    assert!(
        no_physical
            .service
            .put(put(&live, 0, MessageFeedbackRating::Positive, None, None,))
            .await
            .is_err()
    );

    let failed_root = tempfile::tempdir().unwrap();
    let failed = setup(failed_root.path(), 64, false).await;
    failed
        .context
        .events()
        .on_sync(
            &failed.context,
            "session/flush",
            |_, _| anyhow::bail!("disk unavailable"),
            EventOptions::default(),
        )
        .unwrap();
    let live = live_fixture(&failed, "flush-failed");
    let error = failed
        .service
        .put(put(&live, 0, MessageFeedbackRating::Positive, None, None))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("disk unavailable"));
}

#[tokio::test]
async fn captured_live_checkpoint_finishes_after_the_session_detaches_mid_flush() {
    let root = tempfile::tempdir().unwrap();
    let harness = setup(root.path(), 64, false).await;
    let session_fiber = seekdeep_cordis::Fiber::active_child("feedback-live-session");
    let session_context = harness.context.with_fiber(session_fiber.clone());
    let session = harness
        .sessions
        .create(
            &session_context,
            Some(seekdeep_core::session::SessionId::new("detach-mid-flush")),
            seekdeep_core::session_store::CreateSessionOptions::default(),
        )
        .unwrap();
    let fixture = support::append_message_fixture(session.clone());
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let persistence = harness.persistence.clone();
    harness
        .context
        .events()
        .on(
            &harness.context,
            "session/flush",
            {
                let started = started.clone();
                let release = release.clone();
                move |_, args| {
                    let started = started.clone();
                    let release = release.clone();
                    let persistence = persistence.clone();
                    Box::pin(async move {
                        let captured = args
                            .get::<seekdeep_core::session::Session>(0)
                            .ok_or_else(|| anyhow::anyhow!("session/flush lacks session"))?;
                        started.notify_one();
                        release.notified().await;
                        persistence.persist(&captured);
                        Ok(EventReply::Undefined)
                    })
                }
            },
            EventOptions::default(),
        )
        .unwrap();
    let operation = tokio::spawn({
        let service = harness.service.clone();
        let request = put(&fixture, 0, MessageFeedbackRating::Positive, None, None);
        async move { service.put(request).await }
    });
    started.notified().await;
    session_fiber.dispose().await.unwrap();
    assert!(harness.sessions.get(fixture.session.id()).is_none());
    release.notify_one();
    operation.await.unwrap().unwrap().unwrap();
}

#[tokio::test]
async fn disposal_drains_admitted_mutation_rejects_later_admission_and_cold_restart_reopens() {
    let root = tempfile::tempdir().unwrap();
    let harness = setup(root.path(), 64, true).await;
    let fixture = live_fixture(&harness, "restart-feedback");
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    harness.persistence.on_read_from(Some(Arc::new({
        let started = started.clone();
        let release = release.clone();
        move || {
            let started = started.clone();
            let release = release.clone();
            async move {
                started.notify_one();
                release.notified().await;
                Ok(())
            }
            .boxed()
        }
    })));
    let admitted = tokio::spawn({
        let service = harness.service.clone();
        let request = put(
            &fixture,
            0,
            MessageFeedbackRating::Positive,
            Some("survives restart"),
            None,
        );
        async move { service.put(request).await }
    });
    started.notified().await;
    let dispose = tokio::spawn({
        let fiber = harness.feedback_fiber.clone();
        async move { fiber.dispose().await }
    });
    tokio::task::yield_now().await;
    let rejected = harness
        .service
        .put(put(
            &fixture,
            1,
            MessageFeedbackRating::Negative,
            None,
            None,
        ))
        .await
        .unwrap_err();
    assert!(rejected.to_string().contains("service is disposing"));
    release.notify_waiters();
    let item = admitted.await.unwrap().unwrap().unwrap();
    dispose.await.unwrap().unwrap();
    let durable = inspection(&fixture);
    drop(harness);

    let second = setup(root.path(), 64, true).await;
    second.persistence.set_durable(durable);
    assert_eq!(
        second
            .service
            .list(fixture.session.id())
            .await
            .unwrap()
            .unwrap(),
        [item]
    );
}

#[test]
fn durable_row_rejects_duplicate_message_and_version_identities() {
    let item = seekdeep_message_feedback::MessageFeedbackItem {
        message_id: seekdeep_llm::MessageId::new("m1"),
        rating: MessageFeedbackRating::Positive,
        note: None,
        version: MessageFeedbackVersion("v1".to_owned()),
        created_at: 1,
        updated_at: 1,
    };
    let row = |items| MessageFeedbackRow {
        session: MessageFeedbackSessionIdentity {
            created_at: 1,
            cwd: None,
        },
        items,
    };
    assert!(validate_message_feedback_row(&row(vec![item.clone()])).is_ok());
    let mut same_message = item.clone();
    same_message.version = MessageFeedbackVersion("v2".to_owned());
    assert!(validate_message_feedback_row(&row(vec![item.clone(), same_message])).is_err());
    let mut same_version = item.clone();
    same_version.message_id = seekdeep_llm::MessageId::new("m2");
    assert!(validate_message_feedback_row(&row(vec![item, same_version])).is_err());
}
