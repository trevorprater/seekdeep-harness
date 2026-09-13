//! Behavioral parity tests for the abstract settings seam.

use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use parking_lot::Mutex;
use seekdeep_cordis::{Context, EventOptions, EventReply, Fiber, fiber::EffectHandle};
use seekdeep_invariants::InvariantError;
use seekdeep_schemastery::Schema;
use seekdeep_settings::{
    RedactedSecret, SETTINGS, SettingsApplies, SettingsConflictError, SettingsDocument,
    SettingsPathOp, SettingsRegisterOptions, SettingsService, SettingsStorage,
    SettingsUpdateSource, deep_equal_json, install_settings_section, redact_secrets,
    settings_namespace,
};
use serde_json::{Map, Value, json};

#[derive(Default)]
struct MemoryStorage {
    document: Mutex<SettingsDocument>,
    persisted: Mutex<Vec<(String, Map<String, Value>)>>,
    writable: AtomicBool,
    delay: Mutex<Duration>,
}

impl MemoryStorage {
    fn new(document: Value) -> Arc<Self> {
        let Value::Object(document) = document else {
            panic!("memory settings fixture requires an object document");
        };
        Arc::new(Self {
            document: Mutex::new(document),
            persisted: Mutex::new(Vec::new()),
            writable: AtomicBool::new(true),
            delay: Mutex::new(Duration::ZERO),
        })
    }
}

#[async_trait]
impl SettingsStorage for MemoryStorage {
    fn writable(&self) -> bool {
        self.writable.load(Ordering::Acquire)
    }

    fn document_path(&self) -> Option<&Path> {
        None
    }

    async fn load(&self) -> anyhow::Result<SettingsDocument> {
        Ok(self.document.lock().clone())
    }

    async fn persist(
        &self,
        namespace: &seekdeep_settings::SettingsNamespace,
        section: &Map<String, Value>,
    ) -> anyhow::Result<()> {
        let delay = *self.delay.lock();
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        self.persisted
            .lock()
            .push((namespace.to_string(), section.clone()));
        self.document
            .lock()
            .insert(namespace.to_string(), Value::Object(section.clone()));
        Ok(())
    }
}

async fn boot(document: Value) -> (Context, Arc<MemoryStorage>, Arc<SettingsService>) {
    let context = Context::new();
    let storage = MemoryStorage::new(document);
    let settings = SettingsService::install(&context, storage.clone())
        .await
        .unwrap();
    (context, storage, settings)
}

#[tokio::test]
async fn installation_rolls_back_scoped_effects_on_duplicate_or_inactive_ownership() {
    let context = Context::new();
    let first_storage = MemoryStorage::new(json!({}));
    let first = SettingsService::install(&context, first_storage)
        .await
        .unwrap();
    let duplicate = SettingsService::install(&context, MemoryStorage::new(json!({})))
        .await
        .unwrap_err();
    assert!(duplicate.to_string().contains("already provided"));
    assert!(Arc::ptr_eq(&context.get(SETTINGS).unwrap(), &first));

    context.fiber().restart().await.unwrap();
    assert!(context.get(SETTINGS).is_none());

    let inactive = Fiber::active_child("inactive-settings-owner");
    inactive.dispose().await.unwrap();
    let inactive_context = context.with_fiber(inactive);
    let error = SettingsService::install(&inactive_context, MemoryStorage::new(json!({})))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("inactive context"));
    assert!(context.get(SETTINGS).is_none());
}

fn theme_schema() -> Schema {
    Schema::object([
        (
            "theme",
            Schema::union([Schema::constant("dark"), Schema::constant("light")])
                .with_default("dark"),
        ),
        ("fontSize", Schema::number().with_default(14)),
    ])
}

fn nested_schema() -> Schema {
    Schema::object([
        (
            "retry",
            Schema::object([
                ("attempts", Schema::number().with_default(2)),
                ("delayMs", Schema::number().with_default(100)),
            ]),
        ),
        (
            "tags",
            Schema::array(Schema::string()).with_default(json!(["default"])),
        ),
    ])
}

#[test]
fn namespace_brand_matches_lowercase_kebab_contract() {
    assert_eq!(settings_namespace("ui-theme").unwrap().as_str(), "ui-theme");
    for invalid in ["", "UI", "9lives", "a_b", "-lead"] {
        assert!(settings_namespace(invalid).is_err(), "accepted {invalid:?}");
    }
}

#[test]
fn deep_json_equality_matches_javascript_numbers_arrays_and_objects() {
    assert!(deep_equal_json(&json!(1), &json!(1.0)));
    assert!(deep_equal_json(
        &json!({ "a": [1, 2], "b": null }),
        &json!({ "b": null, "a": [1.0, 2.0] })
    ));
    assert!(!deep_equal_json(
        &json!({ "a": [1, 2] }),
        &json!({ "a": [1] })
    ));
    assert!(!deep_equal_json(
        &json!({ "a": [1] }),
        &json!({ "a": { "0": 1 } })
    ));
    assert!(!deep_equal_json(&json!({ "a": 1 }), &json!({ "b": 1 })));
}

#[tokio::test]
async fn registration_layers_defaults_base_and_user_and_describes_wire_schema() {
    let (context, _, settings) = boot(json!({
        "ui-theme": { "theme": "light" }
    }))
    .await;
    let ns = settings_namespace("ui-theme").unwrap();
    let scope = settings
        .register(
            &context,
            &ns,
            theme_schema(),
            SettingsRegisterOptions {
                base: Some(json!({ "fontSize": 16 })),
                ..SettingsRegisterOptions::default()
            },
        )
        .unwrap();
    assert_eq!(scope.get(), json!({ "theme": "light", "fontSize": 16 }));
    let descriptor = settings.describe(false).pop().unwrap();
    assert_eq!(descriptor.ns, ns);
    assert_eq!(descriptor.base, Some(json!({ "fontSize": 16 })));
    assert_eq!(descriptor.user, Some(json!({ "theme": "light" })));
    assert_eq!(descriptor.applies, SettingsApplies::Live);
    let uid = descriptor.schema["uid"].as_u64().unwrap();
    assert_eq!(descriptor.schema["refs"][uid.to_string()]["type"], "object");
    assert!(settings.document_path().is_none());
    assert!(settings.prepare_document().await.unwrap().is_none());
}

#[tokio::test]
async fn registration_rejects_duplicates_malformed_storage_and_owner_validation() {
    let (context, _, settings) = boot(json!({ "bad": "scalar" })).await;
    let bad = settings_namespace("bad").unwrap();
    assert!(
        settings
            .register(
                &context,
                &bad,
                theme_schema(),
                SettingsRegisterOptions::default()
            )
            .unwrap_err()
            .to_string()
            .contains("must be an object")
    );
    let ns = settings_namespace("validated").unwrap();
    let options = SettingsRegisterOptions {
        validate: Some(Arc::new(|value| {
            anyhow::ensure!(value["fontSize"].as_f64().unwrap() >= 10.0, "unreadable");
            Ok(())
        })),
        ..SettingsRegisterOptions::default()
    };
    let scope = settings
        .register(&context, &ns, theme_schema(), options)
        .unwrap();
    assert!(scope.update(json!({ "fontSize": 4 })).await.is_err());
    assert_eq!(scope.get()["fontSize"], 14);
    assert!(
        settings
            .register(
                &context,
                &ns,
                theme_schema(),
                SettingsRegisterOptions::default()
            )
            .unwrap_err()
            .to_string()
            .contains("already registered")
    );

    let reentrant = settings_namespace("reentrant").unwrap();
    settings
        .register(
            &context,
            &reentrant,
            theme_schema(),
            SettingsRegisterOptions {
                validate: Some({
                    let settings = settings.clone();
                    Arc::new(move |_| {
                        let _ = settings.describe(false);
                        Ok(())
                    })
                }),
                ..SettingsRegisterOptions::default()
            },
        )
        .unwrap();

    let (invalid_context, _, invalid_at_registration) = boot(json!({
        "validated": { "fontSize": 4 }
    }))
    .await;
    let invalid_ns = settings_namespace("validated").unwrap();
    assert!(
        invalid_at_registration
            .register(
                &invalid_context,
                &invalid_ns,
                theme_schema(),
                SettingsRegisterOptions {
                    validate: Some(Arc::new(|value| {
                        anyhow::ensure!(value["fontSize"].as_f64().unwrap() >= 10.0, "unreadable");
                        Ok(())
                    })),
                    ..SettingsRegisterOptions::default()
                },
            )
            .is_err()
    );
}

#[tokio::test]
async fn update_deep_merges_persists_before_commit_and_emits_once() {
    let (context, storage, settings) = boot(json!({
        "workspace": { "retry": { "attempts": 5 }, "tags": ["old"] }
    }))
    .await;
    let ns = settings_namespace("workspace").unwrap();
    let scope = settings
        .register(
            &context,
            &ns,
            nested_schema(),
            SettingsRegisterOptions::default(),
        )
        .unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let capture = seen.clone();
    context
        .events()
        .on_sync(
            &context,
            "settings/updated",
            move |_, args| {
                capture.lock().push((
                    (*args.get::<Value>(1).unwrap()).clone(),
                    (*args.get::<Value>(2).unwrap()).clone(),
                    *args.get::<SettingsUpdateSource>(3).unwrap(),
                ));
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    scope
        .update(json!({ "retry": { "delayMs": 900 }, "tags": ["new"] }))
        .await
        .unwrap();
    assert_eq!(
        scope.get(),
        json!({
            "retry": { "attempts": 5, "delayMs": 900 },
            "tags": ["new"]
        })
    );
    assert_eq!(storage.persisted.lock().len(), 1);
    assert_eq!(seen.lock().len(), 1);
    assert_eq!(seen.lock()[0].2, SettingsUpdateSource::Update);
}

#[tokio::test]
async fn writes_are_serialized_failures_do_not_poison_queue_and_conflicts_are_typed() {
    let (context, storage, settings) = boot(json!({})).await;
    *storage.delay.lock() = Duration::from_millis(15);
    let ns = settings_namespace("workspace").unwrap();
    settings
        .register(
            &context,
            &ns,
            nested_schema(),
            SettingsRegisterOptions::default(),
        )
        .unwrap();
    let first = settings.update(&ns, json!({ "retry": { "attempts": 7 } }), None);
    let second = settings.update(&ns, json!({ "retry": { "delayMs": 250 } }), None);
    tokio::try_join!(first, second).unwrap();
    assert_eq!(
        settings.get(&ns).unwrap()["retry"],
        json!({ "attempts": 7, "delayMs": 250 })
    );
    let revision = settings.describe(false)[0].revision;
    settings
        .update(&ns, json!({ "tags": ["winner"] }), Some(revision))
        .await
        .unwrap();
    let error = settings
        .update(&ns, json!({ "tags": ["stale"] }), Some(revision))
        .await
        .unwrap_err();
    let conflict = error.downcast_ref::<SettingsConflictError>().unwrap();
    assert_eq!(conflict.code, "SETTINGS_CONFLICT");
    assert_eq!(
        (conflict.expected, conflict.actual),
        (revision, revision + 1)
    );
    assert!(settings.update(&ns, json!(5), None).await.is_err());
    settings
        .update(&ns, json!({ "tags": ["after-failure"] }), None)
        .await
        .unwrap();
}

#[tokio::test]
async fn replace_and_path_mutation_preserve_unseen_secrets_and_apply_in_order() {
    let (context, _, settings) = boot(json!({
        "adapter": {
            "apiKey": "stored",
            "baseURL": "https://old",
            "nested": { "left": 1, "right": 2 }
        }
    }))
    .await;
    let ns = settings_namespace("adapter").unwrap();
    let schema = Schema::object([
        ("apiKey", Schema::string().role("secret")),
        ("baseURL", Schema::string()),
        (
            "nested",
            Schema::object([("left", Schema::number()), ("right", Schema::number())]),
        ),
    ]);
    let scope = settings
        .register(&context, &ns, schema, SettingsRegisterOptions::default())
        .unwrap();
    settings
        .mutate(
            &ns,
            vec![
                SettingsPathOp::Unset {
                    path: vec!["baseURL".to_owned()],
                },
                SettingsPathOp::Set {
                    path: vec!["nested".to_owned(), "left".to_owned()],
                    value: json!(9),
                },
            ],
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        settings.describe(false)[0].user,
        Some(json!({
            "apiKey": "stored",
            "nested": { "left": 9, "right": 2 }
        }))
    );
    scope.replace(json!({})).await.unwrap();
    assert_eq!(settings.describe(false)[0].user, Some(json!({})));
    assert!(
        settings
            .mutate(
                &ns,
                vec![SettingsPathOp::Set {
                    path: Vec::new(),
                    value: json!("scalar")
                }],
                None
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn provider_publication_keeps_last_good_recovers_and_tracks_raw_revisions() {
    let (context, _, settings) = boot(json!({})).await;
    let ns = settings_namespace("ui-theme").unwrap();
    let scope = settings
        .register(
            &context,
            &ns,
            theme_schema(),
            SettingsRegisterOptions::default(),
        )
        .unwrap();
    settings
        .publish(
            json!({ "ui-theme": { "fontSize": "large" } })
                .as_object()
                .cloned()
                .unwrap(),
            SettingsUpdateSource::Provider,
        )
        .unwrap();
    assert_eq!(scope.get(), json!({ "theme": "dark", "fontSize": 14 }));
    assert_eq!(settings.describe(false)[0].revision, 0);
    settings
        .publish(
            json!({ "ui-theme": { "fontSize": 18 } })
                .as_object()
                .cloned()
                .unwrap(),
            SettingsUpdateSource::Provider,
        )
        .unwrap();
    assert_eq!(scope.get()["fontSize"], 18);
    assert_eq!(settings.describe(false)[0].revision, 1);
    settings
        .update(&ns, json!({ "fontSize": 18 }), None)
        .await
        .unwrap();
    assert_eq!(settings.describe(false)[0].revision, 1);

    let validated = settings_namespace("validated").unwrap();
    let validated_scope = settings
        .register(
            &context,
            &validated,
            theme_schema(),
            SettingsRegisterOptions {
                validate: Some(Arc::new(|value| {
                    anyhow::ensure!(value["fontSize"].as_f64().unwrap() >= 10.0, "unreadable");
                    Ok(())
                })),
                ..SettingsRegisterOptions::default()
            },
        )
        .unwrap();
    settings
        .publish(
            json!({
                "ui-theme": { "fontSize": 19 },
                "validated": { "fontSize": 4 }
            })
            .as_object()
            .unwrap()
            .clone(),
            SettingsUpdateSource::Provider,
        )
        .unwrap();
    assert_eq!(scope.get()["fontSize"], 19);
    assert_eq!(validated_scope.get()["fontSize"], 14);
    settings
        .publish(
            json!({
                "ui-theme": { "fontSize": 19 },
                "validated": { "fontSize": 20 }
            })
            .as_object()
            .unwrap()
            .clone(),
            SettingsUpdateSource::Provider,
        )
        .unwrap();
    assert_eq!(validated_scope.get()["fontSize"], 20);
}

#[tokio::test]
async fn watchers_are_serial_per_callback_contained_and_skipped_after_disposal() {
    let (context, _, settings) = boot(json!({})).await;
    let ns = settings_namespace("ui-theme").unwrap();
    let scope = settings
        .register(
            &context,
            &ns,
            theme_schema(),
            SettingsRegisterOptions::default(),
        )
        .unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let capture = seen.clone();
    let watcher = scope.watch(move |next, _| {
        let capture = capture.clone();
        async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            capture.lock().push(next["fontSize"].as_u64().unwrap());
            Ok(())
        }
    });
    let failing = scope.watch(|_, _| async { anyhow::bail!("watcher boom") });
    let one = settings.update(&ns, json!({ "fontSize": 15 }), None);
    let two = settings.update(&ns, json!({ "fontSize": 16 }), None);
    tokio::try_join!(one, two).unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if seen.lock().len() == 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(*seen.lock(), vec![15, 16]);
    watcher.dispose();
    failing.dispose();
    settings
        .update(&ns, json!({ "fontSize": 17 }), None)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(*seen.lock(), vec![15, 16]);
}

#[tokio::test]
async fn watcher_disposal_skips_queued_work_and_service_disposal_drains_started_work() {
    let (context, _, settings) = boot(json!({})).await;
    let ns = settings_namespace("ui-theme").unwrap();
    let scope = settings
        .register(
            &context,
            &ns,
            theme_schema(),
            SettingsRegisterOptions::default(),
        )
        .unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let watcher = scope.watch({
        let entered = entered.clone();
        let release = release.clone();
        let seen = seen.clone();
        move |next, _| {
            let entered = entered.clone();
            let release = release.clone();
            let seen = seen.clone();
            async move {
                entered.notify_one();
                release.notified().await;
                seen.lock().push(next["fontSize"].as_u64().unwrap());
                Ok(())
            }
        }
    });
    settings
        .update(&ns, json!({ "fontSize": 15 }), None)
        .await
        .unwrap();
    entered.notified().await;
    settings
        .update(&ns, json!({ "fontSize": 16 }), None)
        .await
        .unwrap();
    watcher.dispose();

    let disposed = Arc::new(AtomicBool::new(false));
    let dispose_task = tokio::spawn({
        let disposed = disposed.clone();
        let fiber = context.fiber().clone();
        async move {
            fiber.restart().await.unwrap();
            disposed.store(true, Ordering::Release);
        }
    });
    tokio::task::yield_now().await;
    assert!(!disposed.load(Ordering::Acquire));
    release.notify_one();
    dispose_task.await.unwrap();
    assert_eq!(*seen.lock(), vec![15]);
}

#[tokio::test]
async fn updated_and_document_events_fan_out_contain_ordinary_failures_and_rethrow_invariants() {
    let (context, _, settings) = boot(json!({})).await;
    let ns = settings_namespace("ui-theme").unwrap();
    settings
        .register(
            &context,
            &ns,
            theme_schema(),
            SettingsRegisterOptions::default(),
        )
        .unwrap();
    let updated_late = Arc::new(AtomicUsize::new(0));
    context
        .events()
        .on_sync(
            &context,
            "settings/updated",
            |_, _| anyhow::bail!("ordinary listener failure"),
            EventOptions::default(),
        )
        .unwrap();
    context
        .events()
        .on_sync(
            &context,
            "settings/updated",
            |_, _| Err(InvariantError::new("test-settings", "updated invariant").into()),
            EventOptions::default(),
        )
        .unwrap();
    context
        .events()
        .on_sync(
            &context,
            "settings/updated",
            {
                let updated_late = updated_late.clone();
                move |_, _| {
                    updated_late.fetch_add(1, Ordering::AcqRel);
                    Ok(EventReply::Undefined)
                }
            },
            EventOptions::default(),
        )
        .unwrap();
    let error = settings
        .update(&ns, json!({ "fontSize": 15 }), None)
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<InvariantError>().is_some());
    assert_eq!(updated_late.load(Ordering::Acquire), 1);
    assert_eq!(settings.get(&ns).unwrap()["fontSize"], 15);

    let document_late = Arc::new(AtomicUsize::new(0));
    context
        .events()
        .on_sync(
            &context,
            "settings/document-updated",
            |_, _| anyhow::bail!("ordinary document listener failure"),
            EventOptions::default(),
        )
        .unwrap();
    context
        .events()
        .on_sync(
            &context,
            "settings/document-updated",
            |_, _| Err(InvariantError::new("test-settings", "document invariant").into()),
            EventOptions::default(),
        )
        .unwrap();
    context
        .events()
        .on_sync(
            &context,
            "settings/document-updated",
            {
                let document_late = document_late.clone();
                move |_, _| {
                    document_late.fetch_add(1, Ordering::AcqRel);
                    Ok(EventReply::Undefined)
                }
            },
            EventOptions::default(),
        )
        .unwrap();
    let error = settings
        .update(&ns, json!({ "fontSize": 16 }), None)
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<InvariantError>().is_some());
    assert_eq!(document_late.load(Ordering::Acquire), 1);
    assert_eq!(settings.get(&ns).unwrap()["fontSize"], 15);
    assert_eq!(settings.describe(false)[0].revision, 2);
    assert_eq!(
        settings.describe(false)[0].user.as_ref().unwrap()["fontSize"],
        16
    );
}

#[tokio::test]
async fn raw_revisions_announce_equal_overrides_ignore_identical_writes_and_recover_after_malformed_storage()
 {
    let (context, _, settings) = boot(json!({})).await;
    let ns = settings_namespace("ui-theme").unwrap();
    settings
        .register(
            &context,
            &ns,
            theme_schema(),
            SettingsRegisterOptions {
                base: Some(json!({ "fontSize": 16 })),
                applies: SettingsApplies::Restart,
                ..SettingsRegisterOptions::default()
            },
        )
        .unwrap();
    let documents = Arc::new(Mutex::new(Vec::new()));
    context
        .events()
        .on_sync(
            &context,
            "settings/document-updated",
            {
                let documents = documents.clone();
                move |_, args| {
                    documents.lock().push(*args.get::<u64>(1).unwrap());
                    Ok(EventReply::Undefined)
                }
            },
            EventOptions::default(),
        )
        .unwrap();
    let updated = Arc::new(AtomicUsize::new(0));
    context
        .events()
        .on_sync(
            &context,
            "settings/updated",
            {
                let updated = updated.clone();
                move |_, _| {
                    updated.fetch_add(1, Ordering::AcqRel);
                    Ok(EventReply::Undefined)
                }
            },
            EventOptions::default(),
        )
        .unwrap();

    settings
        .replace(&ns, json!({ "fontSize": 16 }), None)
        .await
        .unwrap();
    assert_eq!(*documents.lock(), vec![1]);
    assert_eq!(updated.load(Ordering::Acquire), 0);
    assert_eq!(
        settings.describe(false)[0].applies,
        SettingsApplies::Restart
    );
    settings
        .replace(&ns, json!({ "fontSize": 16 }), None)
        .await
        .unwrap();
    assert_eq!(*documents.lock(), vec![1]);

    settings
        .publish(
            json!({ "ui-theme": "malformed" })
                .as_object()
                .unwrap()
                .clone(),
            SettingsUpdateSource::Provider,
        )
        .unwrap();
    assert!(settings.describe(false)[0].user.is_none());
    settings
        .publish(
            json!({ "ui-theme": { "fontSize": 18 } })
                .as_object()
                .unwrap()
                .clone(),
            SettingsUpdateSource::Provider,
        )
        .unwrap();
    assert_eq!(*documents.lock(), vec![1, 2]);
    assert_eq!(settings.get(&ns).unwrap()["fontSize"], 18);
}

#[tokio::test]
async fn queued_mutation_reads_front_of_queue_and_root_and_intermediate_edits_match_source() {
    let (context, storage, settings) = boot(json!({})).await;
    *storage.delay.lock() = Duration::from_millis(15);
    let ns = settings_namespace("workspace").unwrap();
    settings
        .register(
            &context,
            &ns,
            nested_schema(),
            SettingsRegisterOptions::default(),
        )
        .unwrap();
    let first = settings.update(&ns, json!({ "retry": { "attempts": 7 } }), None);
    let second = settings.mutate(
        &ns,
        vec![SettingsPathOp::Set {
            path: vec!["retry".to_owned(), "delayMs".to_owned()],
            value: json!(250),
        }],
        None,
    );
    tokio::try_join!(first, second).unwrap();
    assert_eq!(
        settings.get(&ns).unwrap()["retry"],
        json!({ "attempts": 7, "delayMs": 250 })
    );

    settings
        .mutate(
            &ns,
            vec![
                SettingsPathOp::Unset {
                    path: vec!["absent".to_owned(), "leaf".to_owned()],
                },
                SettingsPathOp::Set {
                    path: vec!["created".to_owned(), "leaf".to_owned()],
                    value: json!(true),
                },
            ],
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        settings.describe(false)[0].user.as_ref().unwrap()["created"]["leaf"],
        true
    );
    settings
        .mutate(
            &ns,
            vec![SettingsPathOp::Set {
                path: Vec::new(),
                value: json!({ "retry": { "attempts": 3 } }),
            }],
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        settings.describe(false)[0].user,
        Some(json!({ "retry": { "attempts": 3 } }))
    );
    settings
        .mutate(&ns, vec![SettingsPathOp::Unset { path: Vec::new() }], None)
        .await
        .unwrap();
    assert_eq!(settings.describe(false)[0].user, Some(json!({})));
}

#[tokio::test]
async fn registration_and_service_disposal_are_reversible_and_drain_inflight_writes() {
    let (context, storage, settings) = boot(json!({})).await;
    *storage.delay.lock() = Duration::from_millis(40);
    let owner_fiber = Fiber::active_child("settings-owner");
    let owner = context.with_fiber(owner_fiber.clone());
    let ns = settings_namespace("ui-theme").unwrap();
    settings
        .register(
            &owner,
            &ns,
            theme_schema(),
            SettingsRegisterOptions::default(),
        )
        .unwrap();
    assert!(settings.get(&ns).is_some());
    owner_fiber.dispose().await.unwrap();
    assert!(settings.get(&ns).is_none());

    let scope = settings
        .register(
            &context,
            &ns,
            theme_schema(),
            SettingsRegisterOptions::default(),
        )
        .unwrap();
    let write = tokio::spawn({
        let settings = settings.clone();
        let ns = ns.clone();
        async move { settings.update(&ns, json!({ "fontSize": 20 }), None).await }
    });
    tokio::time::sleep(Duration::from_millis(5)).await;
    context.fiber().restart().await.unwrap();
    write.await.unwrap().unwrap();
    assert!(context.get(SETTINGS).is_none());
    assert_eq!(scope.get()["fontSize"], 14);
    assert!(scope.update(json!({ "fontSize": 21 })).await.is_err());
}

#[tokio::test]
async fn registrant_disposal_during_persist_allows_storage_but_suppresses_commit_and_watchers() {
    let (context, storage, settings) = boot(json!({})).await;
    *storage.delay.lock() = Duration::from_millis(40);
    let owner_fiber = Fiber::active_child("inflight-settings-owner");
    let owner = context.with_fiber(owner_fiber.clone());
    let ns = settings_namespace("ui-theme").unwrap();
    let scope = settings
        .register(
            &owner,
            &ns,
            theme_schema(),
            SettingsRegisterOptions::default(),
        )
        .unwrap();
    let watcher_calls = Arc::new(AtomicUsize::new(0));
    let _watcher = scope.watch({
        let watcher_calls = watcher_calls.clone();
        move |_, _| {
            watcher_calls.fetch_add(1, Ordering::AcqRel);
            async { Ok(()) }
        }
    });
    let write = tokio::spawn({
        let settings = settings.clone();
        let ns = ns.clone();
        async move { settings.update(&ns, json!({ "fontSize": 20 }), None).await }
    });
    tokio::time::sleep(Duration::from_millis(5)).await;
    owner_fiber.dispose().await.unwrap();
    write.await.unwrap().unwrap();
    assert_eq!(storage.persisted.lock().len(), 1);
    assert!(settings.get(&ns).is_none());
    assert_eq!(scope.get()["fontSize"], 14);
    assert_eq!(watcher_calls.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn optional_section_helper_attaches_updates_and_falls_back_on_provider_detach() {
    let context = Context::new();
    let ns = settings_namespace("helper-ns").unwrap();
    let changes = Arc::new(Mutex::new(Vec::new()));
    let capture = changes.clone();
    let installed = install_settings_section(
        &context,
        &ns,
        Schema::object([("theme", Schema::string().with_default("default"))]),
        json!({ "theme": "entry" }),
        None,
        Arc::new(move || {
            capture.lock().push(());
            Ok(())
        }),
    )
    .unwrap();
    assert_eq!(installed.source.get(), json!({ "theme": "entry" }));
    assert!(changes.lock().is_empty());

    let provider_fiber = Fiber::active_child("settings-provider");
    let provider_context = context.with_fiber(provider_fiber.clone());
    let storage = MemoryStorage::new(json!({
        "helper-ns": { "theme": "user" }
    }));
    SettingsService::install(&provider_context, storage)
        .await
        .unwrap();
    installed.fiber.await_settled().await.unwrap();
    assert_eq!(installed.source.get(), json!({ "theme": "user" }));
    assert_eq!(changes.lock().len(), 1);

    context
        .get(SETTINGS)
        .unwrap()
        .update(&ns, json!({ "theme": "live" }), None)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if installed.source.get() == json!({ "theme": "live" }) && changes.lock().len() == 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(changes.lock().len(), 2);

    provider_fiber.dispose().await.unwrap();
    installed.fiber.await_settled().await.unwrap();
    assert_eq!(installed.source.get(), json!({ "theme": "entry" }));
    assert_eq!(changes.lock().len(), 3);
}

#[tokio::test]
async fn optional_section_helper_stays_silent_when_its_consumer_unloads() {
    let context = Context::new();
    let provider_fiber = Fiber::active_child("settings-provider");
    let provider_context = context.with_fiber(provider_fiber);
    SettingsService::install(&provider_context, MemoryStorage::new(json!({})))
        .await
        .unwrap();
    let consumer_fiber = Fiber::active_child("consumer");
    let consumer = context.with_fiber(consumer_fiber.clone());
    let changes = Arc::new(Mutex::new(0_u64));
    let capture = changes.clone();
    let installed = install_settings_section(
        &consumer,
        &settings_namespace("helper-ns").unwrap(),
        Schema::object([("theme", Schema::string().with_default("default"))]),
        json!({ "theme": "entry" }),
        None,
        Arc::new(move || {
            *capture.lock() += 1;
            Ok(())
        }),
    )
    .unwrap();
    installed.fiber.await_settled().await.unwrap();
    assert_eq!(*changes.lock(), 1);
    let settings = context.get(SETTINGS).unwrap();
    let publish_ns = settings_namespace("helper-ns").unwrap();
    consumer
        .own(EffectHandle::synchronous(
            "publish while settings consumer unloads",
            move || {
                settings.publish(
                    json!({ (publish_ns.as_str()): { "theme": "late" } })
                        .as_object()
                        .unwrap()
                        .clone(),
                    SettingsUpdateSource::Provider,
                )
            },
        ))
        .unwrap();
    consumer_fiber.dispose().await.unwrap();
    tokio::task::yield_now().await;
    assert_eq!(*changes.lock(), 1);
}

#[test]
fn redaction_walks_object_dict_array_and_opaque_secret_containers() {
    let profile = Schema::object([
        ("apiKey", Schema::string().role("secret")),
        ("baseURL", Schema::string()),
    ]);
    let schema = Schema::object([
        ("apiKey", Schema::string().role("secret")),
        ("providers", Schema::dict(profile.clone())),
        ("fallbacks", Schema::array(profile)),
        (
            "blob",
            Schema::object([("inner", Schema::string())]).role("secret"),
        ),
    ]);
    let redacted = redact_secrets(
        &schema,
        Some(&json!({
            "apiKey": "top",
            "providers": {
                "a": { "apiKey": "one", "baseURL": "x" },
                "b": { "baseURL": "y" }
            },
            "fallbacks": [{ "apiKey": "two" }],
            "blob": { "inner": "three" },
            "extra": true
        })),
    );
    assert_eq!(
        redacted.value,
        Some(json!({
            "providers": {
                "a": { "baseURL": "x" },
                "b": { "baseURL": "y" }
            },
            "fallbacks": [{}],
            "extra": true
        }))
    );
    assert_eq!(
        redacted.secrets,
        vec![
            RedactedSecret {
                path: vec!["apiKey".to_owned()],
                set: true
            },
            RedactedSecret {
                path: vec!["providers".to_owned(), "a".to_owned(), "apiKey".to_owned()],
                set: true
            },
            RedactedSecret {
                path: vec!["providers".to_owned(), "b".to_owned(), "apiKey".to_owned()],
                set: false
            },
            RedactedSecret {
                path: vec!["fallbacks".to_owned(), "0".to_owned(), "apiKey".to_owned()],
                set: true
            },
            RedactedSecret {
                path: vec!["blob".to_owned()],
                set: true
            },
        ]
    );
}

#[test]
fn redaction_enumerates_unset_slots_preserves_malformed_containers_and_drops_secret_dict_entries() {
    let schema = Schema::object([
        ("apiKey", Schema::string().role("secret")),
        (
            "providers",
            Schema::dict(Schema::object([(
                "apiKey",
                Schema::string().role("secret"),
            )])),
        ),
        (
            "fallbacks",
            Schema::array(Schema::object([(
                "apiKey",
                Schema::string().role("secret"),
            )])),
        ),
        (
            "nested",
            Schema::object([("token", Schema::string().role("secret"))]),
        ),
    ]);
    assert_eq!(
        redact_secrets(&schema, None),
        seekdeep_settings::RedactedValue {
            value: None,
            secrets: vec![
                RedactedSecret {
                    path: vec!["apiKey".to_owned()],
                    set: false,
                },
                RedactedSecret {
                    path: vec!["nested".to_owned(), "token".to_owned()],
                    set: false,
                },
            ],
        }
    );
    let malformed = redact_secrets(
        &schema,
        Some(&json!({
            "providers": "not-a-dict",
            "fallbacks": "not-an-array",
            "extra": { "keep": true }
        })),
    );
    assert_eq!(
        malformed.value,
        Some(json!({
            "providers": "not-a-dict",
            "fallbacks": "not-an-array",
            "extra": { "keep": true }
        }))
    );
    assert_eq!(
        malformed.secrets,
        vec![
            RedactedSecret {
                path: vec!["apiKey".to_owned()],
                set: false,
            },
            RedactedSecret {
                path: vec!["nested".to_owned(), "token".to_owned()],
                set: false,
            },
        ]
    );
    let tokens = Schema::object([("tokens", Schema::dict(Schema::string().role("secret")))]);
    let stripped = redact_secrets(&tokens, Some(&json!({ "tokens": { "a": "x", "b": "y" } })));
    assert_eq!(stripped.value, Some(json!({ "tokens": {} })));
    assert_eq!(
        stripped.secrets,
        vec![
            RedactedSecret {
                path: vec!["tokens".to_owned(), "a".to_owned()],
                set: true,
            },
            RedactedSecret {
                path: vec!["tokens".to_owned(), "b".to_owned()],
                set: true,
            },
        ]
    );
}

#[tokio::test]
async fn descriptors_detach_and_redact_value_base_and_user_layers() {
    let (context, _storage, settings) = boot(json!({
        "adapter": { "apiKey": "user-key", "baseURL": "https://user" }
    }))
    .await;
    let ns = settings_namespace("adapter").unwrap();
    settings
        .register(
            &context,
            &ns,
            Schema::object([
                ("apiKey", Schema::string().role("secret")),
                ("baseURL", Schema::string()),
            ]),
            SettingsRegisterOptions {
                base: Some(json!({ "apiKey": "entry-key", "baseURL": "https://base" })),
                ..SettingsRegisterOptions::default()
            },
        )
        .unwrap();
    let redacted = settings.describe(true).pop().unwrap();
    assert_eq!(redacted.value, json!({ "baseURL": "https://user" }));
    assert_eq!(redacted.base, Some(json!({ "baseURL": "https://base" })));
    assert_eq!(redacted.user, Some(json!({ "baseURL": "https://user" })));
    assert_eq!(
        redacted.secrets,
        Some(vec![RedactedSecret {
            path: vec!["apiKey".to_owned()],
            set: true,
        }])
    );
    let verbatim = settings.describe(false).pop().unwrap();
    assert_eq!(
        verbatim.value,
        json!({ "apiKey": "user-key", "baseURL": "https://user" })
    );
    let mut detached_user = verbatim.user.unwrap();
    detached_user["baseURL"] = json!("mutated");
    assert_eq!(
        settings.describe(false)[0].user,
        Some(json!({ "apiKey": "user-key", "baseURL": "https://user" }))
    );
}

#[tokio::test]
async fn read_only_provider_rejects_before_persist_and_listener_failures_are_contained() {
    let (context, storage, settings) = boot(json!({})).await;
    let ns = settings_namespace("ui-theme").unwrap();
    settings
        .register(
            &context,
            &ns,
            theme_schema(),
            SettingsRegisterOptions::default(),
        )
        .unwrap();
    storage.writable.store(false, Ordering::Release);
    assert!(
        settings
            .update(&ns, json!({ "fontSize": 20 }), None)
            .await
            .is_err()
    );
    assert!(storage.persisted.lock().is_empty());
    storage.writable.store(true, Ordering::Release);
    context
        .events()
        .on_sync(
            &context,
            "settings/updated",
            |_, _| anyhow::bail!("listener boom"),
            EventOptions::default(),
        )
        .unwrap();
    settings
        .update(&ns, json!({ "fontSize": 20 }), None)
        .await
        .unwrap();
    assert_eq!(settings.get(&ns).unwrap()["fontSize"], 20);
}

#[test]
fn registration_options_debug_never_exposes_validator_internals() {
    let options = SettingsRegisterOptions {
        validate: Some(Arc::new(|_| Ok(()))),
        ..SettingsRegisterOptions::default()
    };
    assert!(format!("{options:?}").contains("<validator>"));
}
