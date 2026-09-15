//! Behavioral mirror of the official Codex `ChatGPT` credential bridge.

use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use seekdeep_llm_pi_ai::codex_auth::{
    Credential, OAuthCredential, OPENAI_CODEX_PROVIDER_ID, create_codex_credential_bridge,
};
use seekdeep_util::launch_environment::{
    LaunchEnvironmentLayerInput, LaunchEnvironmentSource, create_launch_environment_snapshot,
};
use serde_json::{Value, json};
use tempfile::TempDir;

const AUTH_CLAIM: &str = "https://api.openai.com/auth";

#[derive(Debug)]
struct RefreshFailure;

impl std::fmt::Display for RefreshFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("refresh rejected")
    }
}

impl std::error::Error for RefreshFailure {}

fn jwt(account_id: &str, expires: u64) -> String {
    let encode = |value: Value| URL_SAFE_NO_PAD.encode(serde_json::to_vec(&value).unwrap());
    format!(
        "{}.{}.x",
        encode(json!({ "alg": "none" })),
        encode(json!({ "exp": expires, (AUTH_CLAIM): { "chatgpt_account_id": account_id } }))
    )
}

fn future_expiry() -> u64 {
    u64::try_from(chrono::Utc::now().timestamp()).unwrap() + 3600
}

#[allow(clippy::needless_pass_by_value)]
fn auth_document(access: &str, refresh: &str, account_id: Value) -> Value {
    json!({
        "auth_mode": "chatgpt",
        "OPENAI_API_KEY": null,
        "tokens": {
            "id_token": "id-token-must-survive",
            "access_token": access,
            "refresh_token": refresh,
            "account_id": account_id,
            "future_token_field": "kept"
        },
        "last_refresh": "2026-01-01T00:00:00.000Z",
        "future_root_field": { "kept": true }
    })
}

async fn write_auth(home: &TempDir, value: &Value) {
    let path = home.path().join("auth.json");
    tokio::fs::write(&path, serde_json::to_vec_pretty(value).unwrap())
        .await
        .unwrap();
    make_private(&path).await;
}

async fn make_private(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .await
            .unwrap();
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn bridge(home: &std::path::Path) -> seekdeep_llm_pi_ai::codex_auth::CodexCredentialBridge {
    let snapshot = create_launch_environment_snapshot(&[LaunchEnvironmentLayerInput {
        source: LaunchEnvironmentSource::Process,
        path: None,
        values: BTreeMap::from([("CODEX_HOME".to_owned(), home.display().to_string())]),
    }]);
    create_codex_credential_bridge(&snapshot)
}

fn oauth(value: Option<Credential>) -> OAuthCredential {
    match value.unwrap() {
        Credential::OAuth(value) => value,
        Credential::ApiKey { .. } => panic!("expected OAuth"),
    }
}

#[tokio::test]
async fn reads_only_chatgpt_and_reports_scoped_location_and_listing() {
    let home = tempfile::tempdir().unwrap();
    let access = jwt("account-one", future_expiry());
    write_auth(
        &home,
        &auth_document(&access, "refresh-old", json!("account-one")),
    )
    .await;
    let bridge = bridge(home.path());
    let credential = oauth(bridge.store.read(OPENAI_CODEX_PROVIDER_ID).await.unwrap());
    assert_eq!(credential.access, access);
    assert_eq!(credential.refresh, "refresh-old");
    assert_eq!(credential.account_id.as_deref(), Some("account-one"));
    assert_eq!(
        bridge.store.list().await.unwrap()[0].provider_id.as_str(),
        OPENAI_CODEX_PROVIDER_ID
    );
    assert!(bridge.store.read("anthropic").await.unwrap().is_none());
    assert_eq!(bridge.file_path, home.path().join("auth.json"));
    assert_eq!(bridge.display_path, "$CODEX_HOME/auth.json");
}

#[tokio::test]
async fn absence_and_non_chatgpt_modes_are_empty_without_creating_home() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("not-created");
    let missing_bridge = bridge(&missing);
    assert!(
        missing_bridge
            .store
            .read(OPENAI_CODEX_PROVIDER_ID)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        missing_bridge
            .store
            .modify(OPENAI_CODEX_PROVIDER_ID, |_| async { Ok(None) })
            .await
            .unwrap()
            .is_none()
    );
    assert!(!missing.exists());
    write_auth(
        &root,
        &json!({ "auth_mode": "apikey", "OPENAI_API_KEY": "ignored" }),
    )
    .await;
    assert!(
        bridge(root.path())
            .store
            .read(OPENAI_CODEX_PROVIDER_ID)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn rejects_every_consumed_malformed_field_without_exposing_tokens() {
    let home = tempfile::tempdir().unwrap();
    let access = jwt("account-one", future_expiry());
    let valid = auth_document(&access, "secret-refresh", json!("account-one"));
    let malformed = vec![
        (Value::Null, "must contain a JSON object"),
        (json!({ "auth_mode": 1 }), "auth_mode must be a string"),
        (
            json!({ "auth_mode": "chatgpt", "tokens": [] }),
            "must contain ChatGPT tokens",
        ),
        (
            json!({ "auth_mode": "chatgpt", "tokens": { "id_token": "" } }),
            "non-empty id_token",
        ),
        (
            {
                let mut value = valid.clone();
                value["tokens"]["access_token"] = json!("not-a-jwt");
                value
            },
            "invalid access_token JWT",
        ),
        (
            {
                let mut value = valid.clone();
                value["tokens"]["access_token"] = json!(jwt("account-one", 0));
                value
            },
            "no valid exp claim",
        ),
        (
            {
                let mut value = valid.clone();
                value["tokens"]["access_token"] =
                    json!(format!("e30.{}.x", URL_SAFE_NO_PAD.encode(b"{\"exp\":1}")));
                value
            },
            "no ChatGPT account id",
        ),
        (
            {
                let mut value = valid.clone();
                value["tokens"]["account_id"] = json!(1);
                value
            },
            "account_id must be a non-empty string or null",
        ),
        (
            {
                let mut value = valid.clone();
                value["tokens"]["account_id"] = json!("other");
                value
            },
            "account_id does not match",
        ),
    ];
    for (document, expected) in malformed {
        write_auth(&home, &document).await;
        let error = bridge(home.path())
            .store
            .read(OPENAI_CODEX_PROVIDER_ID)
            .await
            .unwrap_err();
        assert!(error.to_string().contains(expected), "{error:#}");
        assert!(!error.to_string().contains("secret-refresh"));
    }
    write_auth(&home, &auth_document(&access, "refresh", Value::Null)).await;
    assert_eq!(
        oauth(
            bridge(home.path())
                .store
                .read(OPENAI_CODEX_PROVIDER_ID)
                .await
                .unwrap()
        )
        .account_id
        .as_deref(),
        Some("account-one")
    );
}

#[tokio::test]
async fn rejects_nonfiles_exposure_oversize_and_invalid_json() {
    let directory_home = tempfile::tempdir().unwrap();
    tokio::fs::create_dir(directory_home.path().join("auth.json"))
        .await
        .unwrap();
    assert!(
        bridge(directory_home.path())
            .store
            .read(OPENAI_CODEX_PROVIDER_ID)
            .await
            .unwrap_err()
            .to_string()
            .contains("not a regular file")
    );

    let oversized = tempfile::tempdir().unwrap();
    tokio::fs::write(
        oversized.path().join("auth.json"),
        vec![b'x'; 1024 * 1024 + 1],
    )
    .await
    .unwrap();
    make_private(&oversized.path().join("auth.json")).await;
    assert!(
        bridge(oversized.path())
            .store
            .read(OPENAI_CODEX_PROVIDER_ID)
            .await
            .unwrap_err()
            .to_string()
            .contains("1048576-byte limit")
    );

    let invalid = tempfile::tempdir().unwrap();
    tokio::fs::write(invalid.path().join("auth.json"), b"{broken")
        .await
        .unwrap();
    make_private(&invalid.path().join("auth.json")).await;
    assert!(
        bridge(invalid.path())
            .store
            .read(OPENAI_CODEX_PROVIDER_ID)
            .await
            .unwrap_err()
            .to_string()
            .contains("not valid JSON")
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let exposed = tempfile::tempdir().unwrap();
        write_auth(
            &exposed,
            &auth_document(
                &jwt("account-one", future_expiry()),
                "secret",
                json!("account-one"),
            ),
        )
        .await;
        tokio::fs::set_permissions(
            exposed.path().join("auth.json"),
            std::fs::Permissions::from_mode(0o644),
        )
        .await
        .unwrap();
        assert!(
            bridge(exposed.path())
                .store
                .read(OPENAI_CODEX_PROVIDER_ID)
                .await
                .unwrap_err()
                .to_string()
                .contains("owner-only")
        );
    }
}

#[tokio::test]
async fn refresh_preserves_unknown_fields_and_exact_account_ownership() {
    let home = tempfile::tempdir().unwrap();
    write_auth(
        &home,
        &auth_document(&jwt("account-one", 1), "refresh-old", json!("account-one")),
    )
    .await;
    let next_access = jwt("account-one", future_expiry());
    let result = bridge(home.path())
        .store
        .modify(OPENAI_CODEX_PROVIDER_ID, |_| {
            let next_access = next_access.clone();
            async move {
                Ok(Some(Credential::OAuth(OAuthCredential {
                    access: next_access,
                    refresh: "refresh-new".into(),
                    expires: 1.0,
                    account_id: Some("account-one".into()),
                })))
            }
        })
        .await
        .unwrap();
    assert_eq!(oauth(result).refresh, "refresh-new");
    let written: Value = serde_json::from_slice(
        &tokio::fs::read(home.path().join("auth.json"))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(written["future_root_field"], json!({ "kept": true }));
    assert_eq!(written["tokens"]["future_token_field"], "kept");
    assert_eq!(written["tokens"]["id_token"], "id-token-must-survive");
    assert_eq!(written["tokens"]["refresh_token"], "refresh-new");
    assert!(written["last_refresh"].as_str().unwrap().ends_with('Z'));
}

#[tokio::test]
async fn account_changes_and_malformed_refresh_results_never_write() {
    let home = tempfile::tempdir().unwrap();
    let original = auth_document(
        &jwt("account-one", future_expiry()),
        "refresh-old",
        json!("account-one"),
    );
    write_auth(&home, &original).await;
    let store = bridge(home.path()).store;
    let invalid = vec![
        Credential::ApiKey { key: "no".into() },
        Credential::OAuth(OAuthCredential {
            access: String::new(),
            refresh: "r".into(),
            expires: 1.0,
            account_id: None,
        }),
        Credential::OAuth(OAuthCredential {
            access: jwt("account-one", future_expiry()),
            refresh: String::new(),
            expires: 1.0,
            account_id: None,
        }),
        Credential::OAuth(OAuthCredential {
            access: jwt("account-one", future_expiry()),
            refresh: "r".into(),
            expires: f64::NAN,
            account_id: None,
        }),
        Credential::OAuth(OAuthCredential {
            access: jwt("account-two", future_expiry()),
            refresh: "r".into(),
            expires: 1.0,
            account_id: Some("account-two".into()),
        }),
    ];
    for credential in invalid {
        assert!(
            store
                .modify(OPENAI_CODEX_PROVIDER_ID, |_| async { Ok(Some(credential)) })
                .await
                .is_err()
        );
    }
    let after: Value = serde_json::from_slice(
        &tokio::fs::read(home.path().join("auth.json"))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(after, original);
}

#[tokio::test]
async fn concurrent_official_rotation_wins_for_success_and_callback_failure() {
    let home = tempfile::tempdir().unwrap();
    write_auth(
        &home,
        &auth_document(&jwt("account-one", 1), "old", json!("account-one")),
    )
    .await;
    let filename = home.path().join("auth.json");
    let external = jwt("account-one", future_expiry());
    let result = bridge(home.path())
        .store
        .modify(OPENAI_CODEX_PROVIDER_ID, |_| {
            let filename = filename.clone();
            let external = external.clone();
            async move {
                tokio::fs::write(
                    &filename,
                    serde_json::to_vec_pretty(&auth_document(
                        &external,
                        "codex",
                        json!("account-one"),
                    ))
                    .unwrap(),
                )
                .await
                .unwrap();
                Ok(Some(Credential::OAuth(OAuthCredential {
                    access: jwt("account-one", future_expiry()),
                    refresh: "harness".into(),
                    expires: 1.0,
                    account_id: None,
                })))
            }
        })
        .await
        .unwrap();
    assert_eq!(oauth(result).refresh, "codex");

    let newer = jwt("account-one", future_expiry() + 1);
    let result = bridge(home.path())
        .store
        .modify(OPENAI_CODEX_PROVIDER_ID, |_| {
            let filename = filename.clone();
            let newer = newer.clone();
            async move {
                tokio::fs::write(
                    &filename,
                    serde_json::to_vec_pretty(&auth_document(
                        &newer,
                        "newer",
                        json!("account-one"),
                    ))
                    .unwrap(),
                )
                .await
                .unwrap();
                anyhow::bail!("refresh token reused")
            }
        })
        .await
        .unwrap();
    assert_eq!(oauth(result).refresh, "newer");
}

#[tokio::test]
async fn no_op_unsupported_login_logout_and_disappearing_login_match_authority() {
    let home = tempfile::tempdir().unwrap();
    write_auth(
        &home,
        &auth_document(
            &jwt("account-one", future_expiry()),
            "old",
            json!("account-one"),
        ),
    )
    .await;
    let store = bridge(home.path()).store;
    assert_eq!(
        oauth(
            store
                .modify(
                    OPENAI_CODEX_PROVIDER_ID,
                    |current| async move { Ok(current) }
                )
                .await
                .unwrap()
        )
        .refresh,
        "old"
    );
    assert!(
        store
            .modify("anthropic", |_| async { Ok(None) })
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .modify("anthropic", |_| async {
                Ok(Some(Credential::ApiKey { key: "x".into() }))
            })
            .await
            .unwrap_err()
            .to_string()
            .contains("does not store provider")
    );
    store.delete("anthropic").await.unwrap();
    assert!(
        store
            .delete(OPENAI_CODEX_PROVIDER_ID)
            .await
            .unwrap_err()
            .to_string()
            .contains("codex logout")
    );
    let failure = store
        .modify(OPENAI_CODEX_PROVIDER_ID, |_| async {
            Err(anyhow::Error::new(RefreshFailure))
        })
        .await
        .unwrap_err();
    assert!(failure.downcast_ref::<RefreshFailure>().is_some());

    let absent = home.path().join("absent");
    let error = bridge(&absent)
        .store
        .modify(OPENAI_CODEX_PROVIDER_ID, |_| async {
            Ok(Some(Credential::OAuth(OAuthCredential {
                access: jwt("account-one", future_expiry()),
                refresh: "r".into(),
                expires: 1.0,
                account_id: None,
            })))
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("codex login"));
    assert!(!absent.exists());

    let lock = home.path().join("auth.json.lock");
    tokio::fs::write(&lock, b"other\n").await.unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let task = tokio::spawn({
        let store = store.clone();
        let calls = calls.clone();
        async move {
            store
                .modify(OPENAI_CODEX_PROVIDER_ID, move |_| {
                    calls.fetch_add(1, Ordering::AcqRel);
                    async { Ok(None) }
                })
                .await
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    tokio::fs::remove_file(home.path().join("auth.json"))
        .await
        .unwrap();
    tokio::fs::remove_file(lock).await.unwrap();
    assert!(task.await.unwrap().unwrap().is_none());
    assert_eq!(calls.load(Ordering::Acquire), 0);
}

#[test]
fn blank_codex_home_uses_official_symbolic_default() {
    let snapshot = create_launch_environment_snapshot(&[LaunchEnvironmentLayerInput {
        source: LaunchEnvironmentSource::Process,
        path: None,
        values: BTreeMap::from([("CODEX_HOME".to_owned(), "   ".to_owned())]),
    }]);
    assert_eq!(
        create_codex_credential_bridge(&snapshot).display_path,
        "~/.codex/auth.json"
    );
}
