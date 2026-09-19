//! Behavioral mirror of the local credential provider's read/write suites.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use parking_lot::Mutex;
use seekdeep_cordis::{Context, EventOptions, EventReply, PluginFiber};
use seekdeep_credentials::{CREDENTIALS, CredentialRef, credential_ref};
use seekdeep_credentials_local::{
    CREDENTIALS_FILENAME, LocalCredentialConfig, install, parse_credentials_document, resolve_spec,
};
use seekdeep_util::launch_environment::{
    LaunchEnvironmentLayerInput, LaunchEnvironmentSource, SEEKDEEP_LAUNCH_ENVIRONMENT,
    create_launch_environment_snapshot,
};
use tempfile::TempDir;

fn config(path: PathBuf) -> LocalCredentialConfig {
    LocalCredentialConfig {
        path: Some(path),
        watch: false,
        ..LocalCredentialConfig::default()
    }
}

async fn boot(context: &Context, config: LocalCredentialConfig) -> Arc<PluginFiber> {
    let fiber = install(context, config).expect("mount provider");
    fiber.await_settled().await.expect("provider ready");
    fiber
}

fn values(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

fn provide_layers(context: &Context, layers: &[LaunchEnvironmentLayerInput]) {
    context
        .provide(
            SEEKDEEP_LAUNCH_ENVIRONMENT,
            Arc::new(create_launch_environment_snapshot(layers)),
        )
        .expect("launch environment");
}

async fn write_owner_only(path: &Path, text: &str) {
    tokio::fs::write(path, text).await.unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .await
            .unwrap();
    }
}

fn updates(context: &Context) -> Arc<Mutex<Vec<CredentialRef>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorded = seen.clone();
    context
        .events()
        .on_sync(
            context,
            "credentials/updated",
            move |_, args| {
                recorded
                    .lock()
                    .push((*args.get::<CredentialRef>(0).expect("reference")).clone());
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    seen
}

#[test]
fn resolves_default_and_explicit_specs() {
    let default = resolve_spec(&LocalCredentialConfig {
        seekdeep_home: Some(PathBuf::from("/custom/home")),
        ..LocalCredentialConfig::default()
    })
    .unwrap();
    assert_eq!(
        default.filename,
        Path::new("/custom/home").join(CREDENTIALS_FILENAME)
    );
    assert!(default.watch);
    assert!((default.debounce_ms - 100.0).abs() < f64::EPSILON);

    let explicit = resolve_spec(&LocalCredentialConfig {
        path: Some(PathBuf::from("/etc/seekdeep/creds.yaml")),
        seekdeep_home: Some(PathBuf::from("/ignored")),
        watch: false,
        debounce_ms: 5.0,
    })
    .unwrap();
    assert_eq!(explicit.filename, Path::new("/etc/seekdeep/creds.yaml"));
    assert!(!explicit.watch);
    assert!((explicit.debounce_ms - 5.0).abs() < f64::EPSILON);
}

#[tokio::test]
async fn absent_file_is_an_empty_writable_store() {
    let temporary = TempDir::new().unwrap();
    let context = Context::new();
    let fiber = boot(
        &context,
        config(temporary.path().join(CREDENTIALS_FILENAME)),
    )
    .await;
    let reference = credential_ref("SEEKDEEP_CRED_TEST").unwrap();
    let credentials = context.get(CREDENTIALS).unwrap();

    assert_eq!(credentials.resolve(&reference).await.unwrap(), None);
    let info = credentials.describe(&reference).await.unwrap();
    assert!(!info.configured);
    assert!(info.writable);
    assert_eq!(info.source, None);
    fiber.dispose().await.unwrap();
}

#[tokio::test]
async fn serves_comments_plain_and_quoted_file_values() {
    let temporary = TempDir::new().unwrap();
    let path = temporary.path().join(CREDENTIALS_FILENAME);
    write_owner_only(
        &path,
        "# notes\nSEEKDEEP_CRED_TEST: plain\nSEEKDEEP_CRED_OTHER: \"with space\"\n",
    )
    .await;
    let context = Context::new();
    let fiber = boot(&context, config(path)).await;
    let credentials = context.get(CREDENTIALS).unwrap();
    let key = credential_ref("SEEKDEEP_CRED_TEST").unwrap();
    let other = credential_ref("SEEKDEEP_CRED_OTHER").unwrap();

    assert_eq!(
        credentials.resolve(&key).await.unwrap().unwrap().value,
        "plain"
    );
    assert_eq!(
        credentials.resolve(&other).await.unwrap().unwrap().value,
        "with space"
    );
    assert_eq!(
        credentials.describe(&key).await.unwrap().source.as_deref(),
        Some("file")
    );
    fiber.dispose().await.unwrap();
}

#[tokio::test]
async fn applies_process_file_project_and_user_precedence() {
    let temporary = TempDir::new().unwrap();
    let path = temporary.path().join(CREDENTIALS_FILENAME);
    write_owner_only(&path, "SEEKDEEP_CRED_FILE: stored\n").await;
    let context = Context::new();
    provide_layers(
        &context,
        &[
            LaunchEnvironmentLayerInput {
                source: LaunchEnvironmentSource::Process,
                path: None,
                values: values(&[("SEEKDEEP_CRED_SHARED", "from-process")]),
            },
            LaunchEnvironmentLayerInput {
                source: LaunchEnvironmentSource::ProjectEnv,
                path: Some(PathBuf::from("/work/.env")),
                values: values(&[
                    ("SEEKDEEP_CRED_SHARED", "from-project"),
                    ("SEEKDEEP_CRED_PROJECT", "from-project"),
                ]),
            },
            LaunchEnvironmentLayerInput {
                source: LaunchEnvironmentSource::UserEnv,
                path: Some(PathBuf::from("/home/.seekdeep/.env")),
                values: values(&[
                    ("SEEKDEEP_CRED_SHARED", "from-user"),
                    ("SEEKDEEP_CRED_PROJECT", "from-user"),
                    ("SEEKDEEP_CRED_USER", "from-user"),
                    ("SEEKDEEP_CRED_FILE", "older-user-value"),
                ]),
            },
        ],
    );
    let fiber = boot(&context, config(path)).await;
    let credentials = context.get(CREDENTIALS).unwrap();

    for (name, value, source, writable) in [
        ("SEEKDEEP_CRED_SHARED", "from-process", "env", false),
        ("SEEKDEEP_CRED_FILE", "stored", "file", true),
        ("SEEKDEEP_CRED_PROJECT", "from-project", "project-env", true),
        ("SEEKDEEP_CRED_USER", "from-user", "user-env", true),
    ] {
        let reference = credential_ref(name).unwrap();
        let resolved = credentials.resolve(&reference).await.unwrap().unwrap();
        assert_eq!(resolved.value, value);
        assert_eq!(resolved.source, source);
        let info = credentials.describe(&reference).await.unwrap();
        assert_eq!(info.source.as_deref(), Some(source));
        assert_eq!(info.writable, writable);
    }
    fiber.dispose().await.unwrap();
}

#[tokio::test]
async fn empty_environment_values_fall_through_and_only_nonempty_process_values_shadow() {
    let temporary = TempDir::new().unwrap();
    let path = temporary.path().join(CREDENTIALS_FILENAME);
    write_owner_only(&path, "SEEKDEEP_CRED_TEST: stored\n").await;
    let context = Context::new();
    provide_layers(
        &context,
        &[LaunchEnvironmentLayerInput {
            source: LaunchEnvironmentSource::Process,
            path: None,
            values: values(&[("SEEKDEEP_CRED_TEST", "")]),
        }],
    );
    let fiber = boot(&context, config(path)).await;
    let credentials = context.get(CREDENTIALS).unwrap();
    let key = credential_ref("SEEKDEEP_CRED_TEST").unwrap();
    assert_eq!(
        credentials.resolve(&key).await.unwrap().unwrap().value,
        "stored"
    );
    assert!(credentials.describe(&key).await.unwrap().writable);
    fiber.dispose().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn rejects_a_document_readable_beyond_its_owner_before_parsing() {
    use std::os::unix::fs::PermissionsExt as _;

    let temporary = TempDir::new().unwrap();
    let path = temporary.path().join(CREDENTIALS_FILENAME);
    tokio::fs::write(&path, "SEEKDEEP_CRED_TEST: secret\n")
        .await
        .unwrap();
    tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
        .await
        .unwrap();
    let context = Context::new();
    let fiber = install(&context, config(path)).unwrap();
    let error = fiber.await_settled().await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("readable beyond its owner (mode 644)")
    );
}

#[tokio::test]
async fn rejects_unreachable_or_non_file_paths_as_misconfiguration() {
    let temporary = TempDir::new().unwrap();
    let occupied = temporary.path().join("occupied");
    tokio::fs::write(&occupied, "regular file").await.unwrap();
    let context = Context::new();
    let fiber = install(&context, config(occupied.join(CREDENTIALS_FILENAME))).unwrap();
    assert!(fiber.await_settled().await.is_err());

    let directory = temporary.path().join("document-is-directory");
    tokio::fs::create_dir(&directory).await.unwrap();
    let context = Context::new();
    let fiber = install(&context, config(directory)).unwrap();
    assert!(fiber.await_settled().await.is_err());
}

#[test]
fn strict_document_validation_rejects_every_invalid_shape_without_leaking_values() {
    let filename = Path::new("/private/.credentials.yaml");
    for (text, expected) in [
        ("just a string\n", "must be a mapping"),
        ("- SEEKDEEP_CRED_TEST\n", "must be a mapping"),
        ("not-a-ref: value\n", "credential ref"),
        ("SEEKDEEP_CRED_TEST: 123\n", "must be a string"),
        ("SEEKDEEP_CRED_TEST: \"\"\n", "is empty"),
        (
            "SEEKDEEP_CRED_TEST: one\nSEEKDEEP_CRED_TEST: two\n",
            "invalid document",
        ),
        ("SEEKDEEP_CRED_TEST: \"unterminated\n", "invalid document"),
    ] {
        let error = parse_credentials_document(text, filename).unwrap_err();
        assert!(error.to_string().contains(expected), "{error:#}");
    }

    let secret = "sk-live-DO-NOT-LOG-abcdef123456";
    let error = parse_credentials_document(&format!("SEEKDEEP_CRED_TEST: \"{secret}\n"), filename)
        .unwrap_err();
    let rendered = format!("{error:#}");
    assert!(rendered.contains("invalid document"));
    assert!(rendered.contains("line 2, column 1"));
    assert!(!rendered.contains(secret));
}

#[test]
fn empty_document_is_an_empty_store() {
    assert!(
        parse_credentials_document("# nothing stored yet\n", Path::new("doc.yaml"))
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn creates_owner_only_document_and_emits_after_commit() {
    let temporary = TempDir::new().unwrap();
    let path = temporary.path().join("private").join(CREDENTIALS_FILENAME);
    let context = Context::new();
    let fiber = boot(&context, config(path.clone())).await;
    let seen = updates(&context);
    let credentials = context.get(CREDENTIALS).unwrap();
    let key = credential_ref("SEEKDEEP_CRED_TEST").unwrap();

    credentials.set(&key, "sk-fresh").await.unwrap();
    assert_eq!(
        tokio::fs::read_to_string(&path).await.unwrap(),
        "SEEKDEEP_CRED_TEST: sk-fresh\n"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    assert_eq!(*seen.lock(), vec![key]);
    fiber.dispose().await.unwrap();
}

#[tokio::test]
async fn patches_one_entry_preserving_comments_and_untouched_values() {
    let temporary = TempDir::new().unwrap();
    let path = temporary.path().join(CREDENTIALS_FILENAME);
    write_owner_only(
        &path,
        "# deployment notes\nSEEKDEEP_CRED_OTHER: keep\n\n# the one under edit\nSEEKDEEP_CRED_TEST: old\n",
    )
    .await;
    let context = Context::new();
    let fiber = boot(&context, config(path.clone())).await;
    let credentials = context.get(CREDENTIALS).unwrap();
    credentials
        .set(&credential_ref("SEEKDEEP_CRED_TEST").unwrap(), "new value!")
        .await
        .unwrap();
    assert_eq!(
        tokio::fs::read_to_string(path).await.unwrap(),
        "# deployment notes\nSEEKDEEP_CRED_OTHER: keep\n\n# the one under edit\nSEEKDEEP_CRED_TEST: new value!\n"
    );
    fiber.dispose().await.unwrap();
}

#[tokio::test]
async fn appends_with_source_yaml_flow_comment_and_document_marker_semantics() {
    let cases = [
        ("A: one\n\n", "A: one\nB: two\n"),
        ("A: one\n# footer\n", "A: one\nB: two\n\n# footer\n"),
        ("{}\n", "{ B: two }\n"),
        ("# empty note\n", "# empty note\n\nB: two\n"),
        ("---\nA: one\n...\n", "---\nA: one\nB: two\n...\n"),
        ("{ A: one }\n", "{ A: one, B: two }\n"),
    ];
    for (source, expected) in cases {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join(CREDENTIALS_FILENAME);
        write_owner_only(&path, source).await;
        let context = Context::new();
        let fiber = boot(&context, config(path.clone())).await;
        context
            .get(CREDENTIALS)
            .unwrap()
            .set(&credential_ref("B").unwrap(), "two")
            .await
            .unwrap();
        assert_eq!(tokio::fs::read_to_string(path).await.unwrap(), expected);
        fiber.dispose().await.unwrap();
    }
}

#[tokio::test]
async fn updates_preserve_scalar_style_inline_comments_and_semantic_newlines() {
    let cases = [
        (
            "KEY: old # inline\nOTHER: x\n",
            "new value!",
            "KEY: new value! # inline\nOTHER: x\n",
        ),
        (
            "'KEY': old\nOTHER: x\n",
            "new value!",
            "'KEY': new value!\nOTHER: x\n",
        ),
        (
            "KEY: \"old\"\n",
            "new ' \" value",
            "KEY: \"new ' \\\" value\"\n",
        ),
        ("KEY: 'old'\n", "new ' \" value", "KEY: 'new '' \" value'\n"),
        (
            "KEY: >-\n  old value\n",
            "line one\nline two",
            "KEY: >-\n  line one\n\n  line two\n",
        ),
    ];
    for (source, value, expected) in cases {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join(CREDENTIALS_FILENAME);
        write_owner_only(&path, source).await;
        let context = Context::new();
        let fiber = boot(&context, config(path.clone())).await;
        let credentials = context.get(CREDENTIALS).unwrap();
        let key = credential_ref("KEY").unwrap();
        credentials.set(&key, value).await.unwrap();
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), expected);
        assert_eq!(
            credentials.resolve(&key).await.unwrap().unwrap().value,
            value
        );
        fiber.dispose().await.unwrap();
    }
}

#[tokio::test]
async fn round_trips_multiline_quotes_and_entry_like_values() {
    let temporary = TempDir::new().unwrap();
    let path = temporary.path().join(CREDENTIALS_FILENAME);
    let context = Context::new();
    let fiber = boot(&context, config(path.clone())).await;
    let credentials = context.get(CREDENTIALS).unwrap();
    let alpha = credential_ref("SEEKDEEP_CRED_ALPHA").unwrap();
    let beta = credential_ref("SEEKDEEP_CRED_BETA").unwrap();
    let gamma = credential_ref("SEEKDEEP_CRED_GAMMA").unwrap();
    let injected = credential_ref("SEEKDEEP_CRED_INNER").unwrap();
    let multiline = "line one\nline two";
    let mixed = "both ' and \"";
    credentials.set(&alpha, multiline).await.unwrap();
    credentials.set(&beta, mixed).await.unwrap();
    credentials
        .set(&gamma, &format!("{injected}: injected"))
        .await
        .unwrap();
    assert_eq!(
        tokio::fs::read_to_string(&path).await.unwrap(),
        concat!(
            "SEEKDEEP_CRED_ALPHA: |-\n",
            "  line one\n",
            "  line two\n",
            "SEEKDEEP_CRED_BETA: both ' and \"\n",
            "SEEKDEEP_CRED_GAMMA: \"SEEKDEEP_CRED_INNER: injected\"\n",
        )
    );
    fiber.dispose().await.unwrap();

    let reread_context = Context::new();
    let reread = boot(&reread_context, config(path)).await;
    let credentials = reread_context.get(CREDENTIALS).unwrap();
    assert_eq!(
        credentials.resolve(&gamma).await.unwrap().unwrap().value,
        format!("{injected}: injected")
    );
    assert_eq!(
        credentials.resolve(&alpha).await.unwrap().unwrap().value,
        multiline
    );
    assert_eq!(
        credentials.resolve(&beta).await.unwrap().unwrap().value,
        mixed
    );
    assert_eq!(credentials.resolve(&injected).await.unwrap(), None);
    reread.dispose().await.unwrap();
}

#[tokio::test]
async fn leaves_sibling_multiline_scalar_byte_identical_while_patching_an_entry() {
    let temporary = TempDir::new().unwrap();
    let path = temporary.path().join(CREDENTIALS_FILENAME);
    let original = "SEEKDEEP_WRAPPED: |-\n  line1\n  line2\nSEEKDEEP_REVIEW_ALPHA: a\n";
    write_owner_only(&path, original).await;
    let context = Context::new();
    let fiber = boot(&context, config(path.clone())).await;
    let credentials = context.get(CREDENTIALS).unwrap();
    credentials
        .set(&credential_ref("SEEKDEEP_REVIEW_ALPHA").unwrap(), "b")
        .await
        .unwrap();
    assert_eq!(
        tokio::fs::read_to_string(path).await.unwrap(),
        "SEEKDEEP_WRAPPED: |-\n  line1\n  line2\nSEEKDEEP_REVIEW_ALPHA: b\n"
    );
    let wrapped = credential_ref("SEEKDEEP_WRAPPED").unwrap();
    assert_eq!(
        credentials.resolve(&wrapped).await.unwrap().unwrap().value,
        "line1\nline2"
    );
    fiber.dispose().await.unwrap();
}

#[tokio::test]
async fn unsets_only_the_entry_and_keeps_absent_unset_silent() {
    let temporary = TempDir::new().unwrap();
    let path = temporary.path().join(CREDENTIALS_FILENAME);
    write_owner_only(
        &path,
        "# about the doomed one\nSEEKDEEP_CRED_TEST: gone\n# about the survivor\nSEEKDEEP_CRED_OTHER: stays\n",
    )
    .await;
    let context = Context::new();
    let fiber = boot(&context, config(path.clone())).await;
    let seen = updates(&context);
    let credentials = context.get(CREDENTIALS).unwrap();
    let key = credential_ref("SEEKDEEP_CRED_TEST").unwrap();
    credentials.unset(&key).await.unwrap();
    credentials.unset(&key).await.unwrap();
    assert_eq!(
        tokio::fs::read_to_string(path).await.unwrap(),
        "# about the survivor\nSEEKDEEP_CRED_OTHER: stays\n"
    );
    assert_eq!(*seen.lock(), vec![key]);
    fiber.dispose().await.unwrap();
}

#[tokio::test]
async fn unsetting_only_entry_leaves_an_empty_mapping() {
    let temporary = TempDir::new().unwrap();
    let path = temporary.path().join(CREDENTIALS_FILENAME);
    write_owner_only(&path, "SEEKDEEP_CRED_TEST: only\n").await;
    let context = Context::new();
    let fiber = boot(&context, config(path.clone())).await;
    context
        .get(CREDENTIALS)
        .unwrap()
        .unset(&credential_ref("SEEKDEEP_CRED_TEST").unwrap())
        .await
        .unwrap();
    assert_eq!(tokio::fs::read_to_string(path).await.unwrap(), "{}\n");
    fiber.dispose().await.unwrap();
}

#[tokio::test]
async fn rejects_empty_and_shadowed_writes() {
    let temporary = TempDir::new().unwrap();
    let path = temporary.path().join(CREDENTIALS_FILENAME);
    let context = Context::new();
    provide_layers(
        &context,
        &[LaunchEnvironmentLayerInput {
            source: LaunchEnvironmentSource::Process,
            path: None,
            values: values(&[("SEEKDEEP_CRED_SHADOW", "ambient")]),
        }],
    );
    let fiber = boot(&context, config(path)).await;
    let credentials = context.get(CREDENTIALS).unwrap();
    let key = credential_ref("SEEKDEEP_CRED_TEST").unwrap();
    let shadow = credential_ref("SEEKDEEP_CRED_SHADOW").unwrap();
    assert!(
        credentials
            .set(&key, "")
            .await
            .unwrap_err()
            .to_string()
            .contains("empty value")
    );
    assert!(
        credentials
            .set(&shadow, "next")
            .await
            .unwrap_err()
            .to_string()
            .contains("shadowed")
    );
    assert!(
        credentials
            .unset(&shadow)
            .await
            .unwrap_err()
            .to_string()
            .contains("shadowed")
    );
    fiber.dispose().await.unwrap();
}

#[tokio::test]
async fn folds_unobserved_external_edit_into_write_and_publishes_it_first() {
    let temporary = TempDir::new().unwrap();
    let path = temporary.path().join(CREDENTIALS_FILENAME);
    let context = Context::new();
    let fiber = boot(&context, config(path.clone())).await;
    let seen = updates(&context);
    let credentials = context.get(CREDENTIALS).unwrap();
    let alpha = credential_ref("SEEKDEEP_REVIEW_ALPHA").unwrap();
    let beta = credential_ref("SEEKDEEP_REVIEW_BETA").unwrap();
    credentials.set(&alpha, "one").await.unwrap();
    write_owner_only(&path, &format!("{alpha}: one\n{beta}: external\n")).await;
    credentials.set(&alpha, "two").await.unwrap();
    let text = tokio::fs::read_to_string(path).await.unwrap();
    assert!(text.contains(&format!("{beta}: external")));
    assert!(text.contains(&format!("{alpha}: two")));
    assert_eq!(*seen.lock(), vec![alpha.clone(), beta, alpha]);
    fiber.dispose().await.unwrap();
}

#[tokio::test]
async fn concurrent_providers_preserve_both_refs_under_writer_lock() {
    let temporary = TempDir::new().unwrap();
    let path = temporary.path().join(CREDENTIALS_FILENAME);
    let first_context = Context::new();
    let second_context = Context::new();
    let first = boot(&first_context, config(path.clone())).await;
    let second = boot(&second_context, config(path.clone())).await;
    let alpha = credential_ref("SEEKDEEP_REVIEW_ALPHA").unwrap();
    let beta = credential_ref("SEEKDEEP_REVIEW_BETA").unwrap();
    let first_credentials = first_context.get(CREDENTIALS).unwrap();
    let second_credentials = second_context.get(CREDENTIALS).unwrap();

    let (left, right) = tokio::join!(
        async {
            for value in ["1", "2", "3"] {
                first_credentials.set(&alpha, value).await.unwrap();
            }
        },
        async {
            for value in ["1", "2", "3"] {
                second_credentials.set(&beta, value).await.unwrap();
            }
        }
    );
    let _ = (left, right);
    first.dispose().await.unwrap();
    second.dispose().await.unwrap();

    let check_context = Context::new();
    let check = boot(&check_context, config(path)).await;
    let credentials = check_context.get(CREDENTIALS).unwrap();
    assert_eq!(
        credentials.resolve(&alpha).await.unwrap().unwrap().value,
        "3"
    );
    assert_eq!(
        credentials.resolve(&beta).await.unwrap().unwrap().value,
        "3"
    );
    check.dispose().await.unwrap();
}

#[tokio::test]
async fn observer_failure_is_contained_and_later_listener_runs() {
    let temporary = TempDir::new().unwrap();
    let context = Context::new();
    let fiber = boot(
        &context,
        config(temporary.path().join(CREDENTIALS_FILENAME)),
    )
    .await;
    let calls = Arc::new(Mutex::new(0_u32));
    context
        .events()
        .on_sync(
            &context,
            "credentials/updated",
            |_, _| anyhow::bail!("observer boom"),
            EventOptions::default(),
        )
        .unwrap();
    let calls_after = calls.clone();
    context
        .events()
        .on_sync(
            &context,
            "credentials/updated",
            move |_, _| {
                *calls_after.lock() += 1;
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    context
        .get(CREDENTIALS)
        .unwrap()
        .set(&credential_ref("SEEKDEEP_CRED_TEST").unwrap(), "one")
        .await
        .unwrap();
    assert_eq!(*calls.lock(), 1);
    fiber.dispose().await.unwrap();
}

#[tokio::test]
async fn async_observer_rejection_is_contained() {
    let temporary = TempDir::new().unwrap();
    let context = Context::new();
    let fiber = boot(
        &context,
        config(temporary.path().join(CREDENTIALS_FILENAME)),
    )
    .await;
    context
        .events()
        .on(
            &context,
            "credentials/updated",
            |_, _| {
                Box::pin(async {
                    tokio::task::yield_now().await;
                    anyhow::bail!("async observer boom")
                })
            },
            EventOptions::default(),
        )
        .unwrap();
    context
        .get(CREDENTIALS)
        .unwrap()
        .set(&credential_ref("SEEKDEEP_CRED_TEST").unwrap(), "one")
        .await
        .unwrap();
    tokio::task::yield_now().await;
    fiber.dispose().await.unwrap();
}

#[tokio::test]
async fn invariant_failure_rethrows_after_commit_and_remaining_listeners() {
    let temporary = TempDir::new().unwrap();
    let path = temporary.path().join(CREDENTIALS_FILENAME);
    let context = Context::new();
    let fiber = boot(&context, config(path.clone())).await;
    context
        .events()
        .on_sync(
            &context,
            "credentials/updated",
            |_, _| {
                Err(seekdeep_invariants::InvariantError::new(
                    "forged-credentials-relation",
                    "forged relation",
                )
                .into())
            },
            EventOptions::default(),
        )
        .unwrap();
    let later = Arc::new(Mutex::new(0_u32));
    let later_listener = later.clone();
    context
        .events()
        .on_sync(
            &context,
            "credentials/updated",
            move |_, _| {
                *later_listener.lock() += 1;
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    let reference = credential_ref("SEEKDEEP_REVIEW_ALPHA").unwrap();
    let credentials = context.get(CREDENTIALS).unwrap();
    let error = credentials.set(&reference, "one").await.unwrap_err();
    assert!(error.to_string().contains("forged relation"));
    assert_eq!(*later.lock(), 1);
    assert!(
        tokio::fs::read_to_string(path)
            .await
            .unwrap()
            .contains("SEEKDEEP_REVIEW_ALPHA: one")
    );
    assert_eq!(
        credentials
            .resolve(&reference)
            .await
            .unwrap()
            .unwrap()
            .value,
        "one"
    );
    fiber.dispose().await.unwrap();
}

#[tokio::test]
async fn rejected_and_concurrent_writes_do_not_poison_serial_operation_queue() {
    let temporary = TempDir::new().unwrap();
    let path = temporary.path().join(CREDENTIALS_FILENAME);
    let context = Context::new();
    let fiber = boot(&context, config(path.clone())).await;
    let credentials = context.get(CREDENTIALS).unwrap();
    let alpha = credential_ref("SEEKDEEP_QUEUE_ALPHA").unwrap();
    let beta = credential_ref("SEEKDEEP_QUEUE_BETA").unwrap();
    assert!(credentials.set(&alpha, "").await.is_err());
    let (alpha_result, beta_result) = tokio::join!(
        credentials.set(&alpha, "one"),
        credentials.set(&beta, "two")
    );
    alpha_result.unwrap();
    beta_result.unwrap();
    let text = tokio::fs::read_to_string(path).await.unwrap();
    assert_eq!(
        text,
        "SEEKDEEP_QUEUE_ALPHA: one\nSEEKDEEP_QUEUE_BETA: two\n"
    );
    fiber.dispose().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn nul_path_fails_before_os_lookup() {
    use std::os::unix::ffi::OsStringExt as _;

    let temporary = TempDir::new().unwrap();
    let mut bytes = temporary.path().as_os_str().as_encoded_bytes().to_vec();
    bytes.extend_from_slice(b"/.credentials\0.yaml");
    let path = PathBuf::from(std::ffi::OsString::from_vec(bytes));
    let context = Context::new();
    let fiber = install(&context, config(path)).unwrap();
    assert!(fiber.await_settled().await.is_err());
}

#[tokio::test]
async fn invalid_external_document_fails_write_without_overwriting_it() {
    let temporary = TempDir::new().unwrap();
    let path = temporary.path().join(CREDENTIALS_FILENAME);
    let context = Context::new();
    let fiber = boot(&context, config(path.clone())).await;
    write_owner_only(&path, "SEEKDEEP_CRED_TEST: \"unterminated\n").await;
    let error = context
        .get(CREDENTIALS)
        .unwrap()
        .set(&credential_ref("SEEKDEEP_CRED_OTHER").unwrap(), "lands")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("invalid document"));
    assert!(
        tokio::fs::read_to_string(path)
            .await
            .unwrap()
            .contains("unterminated")
    );
    fiber.dispose().await.unwrap();
}

#[tokio::test]
async fn disposal_refuses_late_and_queued_writes_but_drains_in_flight_write() {
    let temporary = TempDir::new().unwrap();
    let path = temporary.path().join(CREDENTIALS_FILENAME);
    let lock_path = PathBuf::from(format!("{}.lock", path.display()));
    let context = Context::new();
    let fiber = boot(&context, config(path)).await;
    let credentials = context.get(CREDENTIALS).unwrap();
    let first_ref = credential_ref("SEEKDEEP_DRAIN_A").unwrap();
    let second_ref = credential_ref("SEEKDEEP_DRAIN_B").unwrap();
    tokio::fs::write(&lock_path, "held\n").await.unwrap();

    let first_service = credentials.clone();
    let first_key = first_ref.clone();
    let first = tokio::spawn(async move { first_service.set(&first_key, "one").await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let second_service = credentials.clone();
    let second_key = second_ref.clone();
    let second = tokio::spawn(async move { second_service.set(&second_key, "two").await });
    let disposal_fiber = fiber.clone();
    let disposal = tokio::spawn(async move { disposal_fiber.dispose().await });
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    tokio::fs::remove_file(lock_path).await.unwrap();

    first.await.unwrap().unwrap();
    let second_error = second.await.unwrap().unwrap_err();
    assert!(
        second_error
            .to_string()
            .contains("disposed before the queued")
    );
    disposal.await.unwrap().unwrap();
    assert_eq!(
        credentials
            .resolve(&first_ref)
            .await
            .unwrap()
            .unwrap()
            .value,
        "one"
    );
    assert_eq!(credentials.resolve(&second_ref).await.unwrap(), None);
    assert!(
        credentials
            .set(&first_ref, "late")
            .await
            .unwrap_err()
            .to_string()
            .contains("disposed")
    );
}
