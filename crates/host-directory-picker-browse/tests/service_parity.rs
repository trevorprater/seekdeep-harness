//! Real filesystem listing, bounds, cancellation, crumbs, and creation parity.

use std::{fs, sync::Arc};

use seekdeep_cordis::Context;
use seekdeep_host_directory_picker::{
    DIRECTORY_PICKER, DirectoryPickerCapability, DirectoryPickerError, DirectoryPickerErrorCode,
    DirectoryPickerFailure,
};
use seekdeep_host_directory_picker_browse::*;
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use seekdeep_llm::AbortSignal;

#[test]
fn fully_qualified_and_bounded_window_match_platform_rules() {
    assert!(fully_qualified("/home/x", PathPlatform::Posix));
    assert!(!fully_qualified("x/y", PathPlatform::Posix));
    for path in [
        "C:\\projects",
        "C:/projects",
        "\\\\server\\share",
        "//server/share/deep",
    ] {
        assert!(fully_qualified(path, PathPlatform::Windows), "{path}");
    }
    for path in [
        "\\foo",
        "/foo",
        "C:relative",
        "\\\\",
        "\\\\server",
        "\\\\server\\",
        "///server/share",
    ] {
        assert!(!fully_qualified(path, PathPlatform::Windows), "{path}");
    }
    let candidate = |name: &str| ListingCandidate {
        name: name.to_owned(),
        is_directory: true,
        is_symbolic_link: false,
    };
    let mut window = Vec::new();
    assert!(!bounded_insert(&mut window, candidate("m"), 2));
    assert!(!bounded_insert(&mut window, candidate("z"), 2));
    assert!(bounded_insert(&mut window, candidate("a"), 2));
    assert_eq!(
        window
            .iter()
            .map(|row| row.name.as_str())
            .collect::<Vec<_>>(),
        ["a", "m"]
    );
    assert!(bounded_insert(&mut window, candidate("t"), 2));
    assert_eq!(
        window
            .iter()
            .map(|row| row.name.as_str())
            .collect::<Vec<_>>(),
        ["a", "m"]
    );
}

#[test]
#[should_panic(expected = "bounded listing window must retain at least one candidate")]
fn zero_sized_bounded_window_rejects_the_invalid_internal_contract() {
    let mut window = Vec::new();
    bounded_insert(
        &mut window,
        ListingCandidate {
            name: "a".to_owned(),
            is_directory: true,
            is_symbolic_link: false,
        },
        0,
    );
}

#[tokio::test]
async fn lists_only_enterable_directories_sorted_hidden_bounded_and_crumbed() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("projects")).unwrap();
    fs::create_dir(root.path().join("projects/harness")).unwrap();
    fs::create_dir(root.path().join(".hidden-dir")).unwrap();
    fs::write(root.path().join("notes.txt"), "x").unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(root.path().join("projects"), root.path().join("linked"))
            .unwrap();
        std::os::unix::fs::symlink(root.path().join("gone"), root.path().join("broken")).unwrap();
        std::os::unix::fs::symlink(root.path().join("notes.txt"), root.path().join("file-link"))
            .unwrap();
    }
    let listing = list_directory(
        Some(root.path().to_string_lossy().into_owned()),
        AbortSignal::default(),
        1000,
    )
    .await
    .unwrap();
    let names = listing
        .entries
        .iter()
        .map(|entry| entry.name.as_str())
        .collect::<Vec<_>>();
    #[cfg(unix)]
    assert_eq!(names, [".hidden-dir", "linked", "projects"]);
    #[cfg(not(unix))]
    assert_eq!(names, [".hidden-dir", "projects"]);
    assert!(listing.entries[0].hidden);
    assert!(
        listing
            .entries
            .iter()
            .all(|entry| entry.path == root.path().join(&entry.name).to_string_lossy())
    );
    assert!(!listing.truncated);
    assert_eq!(listing.crumbs.last().unwrap().path, listing.path);
    assert_eq!(
        listing.crumbs.last().unwrap().name,
        root.path().file_name().unwrap().to_string_lossy()
    );
    assert_eq!(
        listing.crumbs.first().unwrap().name,
        listing.crumbs.first().unwrap().path
    );

    let cut = list_directory(
        Some(root.path().to_string_lossy().into_owned()),
        AbortSignal::default(),
        1,
    )
    .await
    .unwrap();
    assert_eq!(cut.entries.len(), 1);
    assert_eq!(cut.entries[0].name, ".hidden-dir");
    assert!(cut.truncated);
    let exact = list_directory(
        Some(root.path().join("projects").to_string_lossy().into_owned()),
        AbortSignal::default(),
        1,
    )
    .await
    .unwrap();
    assert_eq!(exact.entries[0].name, "harness");
    assert!(!exact.truncated);
    fs::create_dir(root.path().join("projects/harness/a")).unwrap();
    fs::create_dir(root.path().join("projects/harness/b")).unwrap();
    let in_window = list_directory(
        Some(
            root.path()
                .join("projects/harness")
                .to_string_lossy()
                .into_owned(),
        ),
        AbortSignal::default(),
        1,
    )
    .await
    .unwrap();
    assert_eq!(in_window.entries[0].name, "a");
    assert!(in_window.truncated);

    let home = list_directory(None, AbortSignal::default(), 1000)
        .await
        .unwrap();
    assert_eq!(home.path, dirs::home_dir().unwrap().to_string_lossy());
    assert_eq!(home.home, home.path);
}

#[tokio::test]
async fn cancellation_wins_with_its_reason_and_late_operation_is_drained() {
    #[derive(Debug, thiserror::Error)]
    #[error("caller left")]
    struct CallerLeft;
    let signal = AbortSignal::default();
    assert_eq!(
        race_abort(Box::pin(async { Ok("ok") }), None)
            .await
            .unwrap(),
        "ok"
    );
    assert_eq!(
        race_abort(
            Box::pin(async { Ok::<_, anyhow::Error>("ok") }),
            Some(signal.clone()),
        )
        .await
        .unwrap(),
        "ok"
    );
    let raw = race_abort::<()>(
        Box::pin(async { anyhow::bail!("raw failure") }),
        Some(signal.clone()),
    )
    .await
    .unwrap_err();
    assert_eq!(raw.to_string(), "raw failure");
    let (sender, receiver) = tokio::sync::oneshot::channel::<()>();
    let operation = Box::pin(async move {
        receiver.await?;
        anyhow::bail!("late read failure")
    });
    let raced = race_abort::<()>(operation, Some(signal.clone()));
    signal.abort_with_error(Arc::new(CallerLeft), serde_json::json!("caller left"));
    let error = raced.await.unwrap_err();
    assert_eq!(error.to_string(), "caller left");
    let _ = sender.send(());

    let already_aborted = AbortSignal::default();
    already_aborted.abort_with_error(Arc::new(CallerLeft), serde_json::json!("already gone"));
    let immediate = race_abort(
        Box::pin(async { Ok::<_, anyhow::Error>("operation won") }),
        Some(already_aborted),
    )
    .await
    .unwrap_err();
    assert_eq!(immediate.to_string(), "caller left");

    let string_reason = AbortSignal::default();
    string_reason.abort_with_reason(serde_json::json!("bare reason"));
    let error = race_abort(
        Box::pin(async { Ok::<_, anyhow::Error>(()) }),
        Some(string_reason),
    )
    .await
    .unwrap_err();
    assert_eq!(error.to_string(), "bare reason");

    let number_reason = AbortSignal::default();
    number_reason.abort_with_reason(serde_json::json!(100_000_000_000_000_000_000_f64));
    let error = race_abort(
        Box::pin(async { Ok::<_, anyhow::Error>(()) }),
        Some(number_reason),
    )
    .await
    .unwrap_err();
    assert_eq!(error.to_string(), "100000000000000000000");

    let root = tempfile::tempdir().unwrap();
    let aborted = AbortSignal::default();
    aborted.abort_with_error(Arc::new(CallerLeft), serde_json::json!("caller left"));
    let error = list_directory(
        Some(root.path().to_string_lossy().into_owned()),
        aborted,
        1000,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("caller left"));
    let missing_error = list_directory(
        Some(root.path().join("missing").to_string_lossy().into_owned()),
        AbortSignal::default(),
        1000,
    )
    .await
    .unwrap_err();
    assert_picker_error(
        &missing_error,
        DirectoryPickerErrorCode::DirectoryUnreadable,
        &root.path().join("missing").to_string_lossy(),
    );
}

#[tokio::test]
async fn creation_and_plugin_surface_use_closed_errors_and_stable_capability() {
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().to_string_lossy().into_owned();
    fs::create_dir(root.path().join("existing")).unwrap();
    let normalized_listing = list_directory(
        Some(
            root.path()
                .join("existing/../existing")
                .to_string_lossy()
                .into_owned(),
        ),
        AbortSignal::default(),
        1000,
    )
    .await
    .unwrap();
    assert_eq!(
        normalized_listing.path,
        root.path().join("existing").to_string_lossy()
    );
    let created = create_directory(root_path.clone(), "fresh".to_owned())
        .await
        .unwrap();
    assert_eq!(created, root.path().join("fresh").to_string_lossy());
    let listing = list_directory(Some(root_path.clone()), AbortSignal::default(), 1000)
        .await
        .unwrap();
    assert!(listing.entries.iter().any(|entry| entry.name == "fresh"));
    let exists = create_directory(root_path.clone(), "fresh".to_owned())
        .await
        .unwrap_err();
    assert_picker_error(
        &exists,
        DirectoryPickerErrorCode::DirectoryExists,
        &root.path().join("fresh").to_string_lossy(),
    );
    for name in ["", "  ", ".", "..", "a/b", "a\\b"] {
        let error = create_directory(root_path.clone(), name.to_owned())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            seekdeep_host_directory_picker::DirectoryPickerFailure::Picker(ref error)
                if error.code == DirectoryPickerErrorCode::DirectoryCreateFailed
        ));
    }
    let relative = create_directory("relative".to_owned(), "child".to_owned())
        .await
        .unwrap_err();
    assert_picker_error(
        &relative,
        DirectoryPickerErrorCode::DirectoryCreateFailed,
        "relative",
    );
    let missing_parent = create_directory(
        root.path().join("missing").to_string_lossy().into_owned(),
        "child".to_owned(),
    )
    .await
    .unwrap_err();
    assert_picker_error(
        &missing_parent,
        DirectoryPickerErrorCode::DirectoryCreateFailed,
        &root.path().join("missing/child").to_string_lossy(),
    );

    for relative in ["", "projects", "./projects", ".."] {
        let list_error = list_directory(Some(relative.to_owned()), AbortSignal::default(), 1000)
            .await
            .unwrap_err();
        assert_picker_error(
            &list_error,
            DirectoryPickerErrorCode::DirectoryUnreadable,
            relative,
        );
    }

    let context = Context::new();
    let fiber = context
        .plugin(plugin(), serde_json::json!({"maxEntries": 1}))
        .unwrap();
    fiber.await_settled().await.unwrap();
    let service = context.get(DIRECTORY_PICKER).unwrap();
    let first = service.capability() as *const _;
    let second = service.capability() as *const _;
    assert_eq!(first, second);
    assert!(matches!(
        service.capability(),
        DirectoryPickerCapability::Browse { .. }
    ));
    fiber.dispose().await.unwrap();
    assert!(context.get(DIRECTORY_PICKER).is_none());

    let defaulted = context.plugin(plugin(), serde_json::Value::Null).unwrap();
    defaulted.await_settled().await.unwrap();
    assert!(context.get(DIRECTORY_PICKER).is_some());
    defaulted.dispose().await.unwrap();

    let invalid = context
        .plugin(plugin(), serde_json::json!({"maxEntries": 0}))
        .unwrap();
    assert!(invalid.await_settled().await.is_err());
    invalid.dispose().await.unwrap();
}

#[tokio::test]
async fn invariant_reserves_package_identity() {
    let registry =
        Arc::new(InvariantRegistry::new(&Context::new(), &InvariantConfig::default()).unwrap());
    let registration = register_invariant(&registry).unwrap();
    assert!(register_invariant(&registry).is_err());
    registration.dispose().await.unwrap();
}

fn assert_picker_error(
    failure: &DirectoryPickerFailure,
    code: DirectoryPickerErrorCode,
    path: &str,
) {
    let DirectoryPickerFailure::Picker(DirectoryPickerError {
        code: actual_code,
        path: actual_path,
        ..
    }) = failure
    else {
        panic!("expected typed picker failure, got {failure}");
    };
    assert_eq!(*actual_code, code);
    assert_eq!(actual_path, path);
}
