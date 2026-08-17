//! Anonymous identity persistence and concurrency contracts.

use std::{collections::HashMap, ffi::OsString, sync::Arc};

use seekdeep_anonymous_user_id::{
    ANONYMOUS_USER_ID_FILE_NAME, AnonymousUserIdOptions, get_or_create_anonymous_user_id,
    invariant::register_invariant,
};
use seekdeep_util::home_paths::SEEKDEEP_HOME_ENV;

fn options(home: &std::path::Path) -> AnonymousUserIdOptions {
    AnonymousUserIdOptions {
        env: Some(HashMap::from([(
            OsString::from(SEEKDEEP_HOME_ENV),
            home.as_os_str().to_owned(),
        )])),
        random_uuid: None,
    }
}

#[test]
fn creates_persists_and_returns_a_bare_uuid_line() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let id = get_or_create_anonymous_user_id(options(temporary.path())).expect("id");
    assert_eq!(id.as_str().len(), 36);
    assert_eq!(
        std::fs::read_to_string(temporary.path().join(ANONYMOUS_USER_ID_FILE_NAME)).expect("file"),
        format!("{id}\n")
    );
}

#[test]
fn creates_a_missing_nested_home() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let home = temporary.path().join("nested/home");
    let id = get_or_create_anonymous_user_id(options(&home)).expect("id");
    assert_eq!(
        std::fs::read_to_string(home.join(ANONYMOUS_USER_ID_FILE_NAME)).expect("file"),
        format!("{id}\n")
    );
}

#[test]
fn accepts_surrounding_whitespace_on_a_persisted_id() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let existing = "01234567-89ab-4cde-8f01-23456789abcd";
    std::fs::write(
        temporary.path().join(ANONYMOUS_USER_ID_FILE_NAME),
        format!("  {existing}\n\n"),
    )
    .expect("seed");
    assert_eq!(
        get_or_create_anonymous_user_id(options(temporary.path()))
            .expect("id")
            .as_str(),
        existing
    );
}

#[test]
fn overwrites_a_corrupt_file_with_the_fresh_id() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let file = temporary.path().join(ANONYMOUS_USER_ID_FILE_NAME);
    std::fs::write(&file, "not-a-uuid\n").expect("seed");
    let fresh = "11111111-2222-4333-8444-555555555555";
    let id = get_or_create_anonymous_user_id(AnonymousUserIdOptions {
        random_uuid: Some(Arc::new(move || fresh.to_owned())),
        ..options(temporary.path())
    })
    .expect("id");
    assert_eq!(id.as_str(), fresh);
    assert_eq!(
        std::fs::read_to_string(file).expect("file"),
        format!("{fresh}\n")
    );
}

#[test]
fn exclusive_create_loser_adopts_the_concurrent_winner() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let home = temporary.path().to_owned();
    let winner = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
    let generator_home = home.clone();
    let id = get_or_create_anonymous_user_id(AnonymousUserIdOptions {
        random_uuid: Some(Arc::new(move || {
            std::fs::write(
                generator_home.join(ANONYMOUS_USER_ID_FILE_NAME),
                format!("{winner}\n"),
            )
            .expect("plant winner");
            "ffffffff-0000-4000-8000-000000000000".to_owned()
        })),
        ..options(&home)
    })
    .expect("id");
    assert_eq!(id.as_str(), winner);
}

#[test]
fn blocked_home_still_returns_a_process_usable_id() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let blocked = temporary.path().join("blocked");
    std::fs::write(&blocked, "occupied\n").expect("block path");
    let id = get_or_create_anonymous_user_id(options(&blocked)).expect("id");
    assert_eq!(id.as_str().len(), 36);
    assert!(!blocked.join(ANONYMOUS_USER_ID_FILE_NAME).exists());
}

#[test]
fn memo_survives_mid_process_deletion() {
    let first_home = tempfile::tempdir().expect("first");
    let first = get_or_create_anonymous_user_id(options(first_home.path())).expect("first id");
    std::fs::remove_file(first_home.path().join(ANONYMOUS_USER_ID_FILE_NAME)).expect("remove");
    assert_eq!(
        get_or_create_anonymous_user_id(options(first_home.path())).expect("cached"),
        first
    );
}

#[test]
fn distinct_homes_receive_distinct_ids() {
    let first_home = tempfile::tempdir().expect("first");
    let second_home = tempfile::tempdir().expect("second");
    let first = get_or_create_anonymous_user_id(options(first_home.path())).expect("first id");
    let second = get_or_create_anonymous_user_id(options(second_home.path())).expect("second id");
    assert_ne!(first, second);
}

#[test]
fn reads_the_process_environment_by_default() {
    const CHILD_MARKER: &str = "SEEKDEEP_ANONYMOUS_ID_ENV_CHILD";
    if std::env::var_os(CHILD_MARKER).is_some() {
        let id = get_or_create_anonymous_user_id(AnonymousUserIdOptions::default()).expect("id");
        let home = std::env::var_os(SEEKDEEP_HOME_ENV).expect("child home");
        assert_eq!(
            std::fs::read_to_string(std::path::Path::new(&home).join(ANONYMOUS_USER_ID_FILE_NAME))
                .expect("persisted"),
            format!("{id}\n")
        );
        return;
    }
    let temporary = tempfile::tempdir().expect("tempdir");
    let status = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .env(CHILD_MARKER, "1")
        .env(SEEKDEEP_HOME_ENV, temporary.path())
        .arg("--exact")
        .arg("reads_the_process_environment_by_default")
        .arg("--nocapture")
        .status()
        .expect("spawn child");
    assert!(status.success());
}

#[tokio::test]
async fn explained_empty_invariant_reserves_and_releases_identity() {
    let context = seekdeep_cordis::Context::new();
    let registry = Arc::new(
        seekdeep_invariants::InvariantRegistry::new(
            &context,
            &seekdeep_invariants::InvariantConfig::default(),
        )
        .expect("registry"),
    );
    let registration = register_invariant(&registry).expect("register");
    registration.await_ready().await.expect("ready");
    assert!(register_invariant(&registry).is_err());
    registration.dispose().await.expect("dispose");
    register_invariant(&registry)
        .expect("replacement")
        .await_ready()
        .await
        .expect("replacement ready");
}
