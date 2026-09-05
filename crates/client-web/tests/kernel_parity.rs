//! Kernel signal, status, activation audit, platform, and stylesheet parity.

use std::{cell::Cell, rc::Rc};

use seekdeep_client_web::*;

#[test]
fn signals_keep_snapshot_identity_until_write_and_dispose_exact_listener_generation() {
    let signal = KernelSignal::new("initial".to_owned());
    let first = signal.snapshot();
    assert!(Rc::ptr_eq(&first, &signal.snapshot()));
    let calls = Rc::new(Cell::new(0));
    let observed = calls.clone();
    let subscription = signal.subscribe(Rc::new(move || observed.set(observed.get() + 1)));
    signal.set("next".to_owned());
    assert_eq!(calls.get(), 1);
    assert!(!Rc::ptr_eq(&first, &signal.snapshot()));
    subscription.dispose();
    subscription.dispose();
    signal.set("final".to_owned());
    assert_eq!(calls.get(), 1);
}

#[test]
fn loader_status_is_copy_on_write_and_app_root_switches_loading_failure_settled() {
    let statuses = LoaderStatusStore::new();
    let empty = statuses.signal().snapshot();
    assert_eq!(app_root_view(false, &empty, None), AppRootView::Loading);
    statuses.set("pending".into(), WebFiberState::Pending);
    assert!(!Rc::ptr_eq(&empty, &statuses.signal().snapshot()));
    statuses.set("broken".into(), WebFiberState::Failed);
    assert_eq!(
        app_root_view(false, &statuses.signal().snapshot(), Some("sweep")),
        AppRootView::Failed {
            entries: vec!["broken".into()],
            error: Some("sweep".into()),
        }
    );
    assert_eq!(
        app_root_view(true, &statuses.signal().snapshot(), Some("ignored")),
        AppRootView::Settled
    );
}

#[test]
fn activation_sweep_names_import_pending_failed_and_pluralization_exactly() {
    let active = WebBootEntry {
        id: WebEntryId::new("active"),
        state: Some(WebFiberState::Active),
        missing_services: Vec::new(),
    };
    assert!(assert_web_entries_active(std::slice::from_ref(&active)).is_ok());
    let failures = [
        WebBootEntry {
            id: WebEntryId::new("missing"),
            state: None,
            missing_services: Vec::new(),
        },
        WebBootEntry {
            id: WebEntryId::new("waiting"),
            state: Some(WebFiberState::Pending),
            missing_services: vec!["slots".into(), "layout".into()],
        },
        WebBootEntry {
            id: WebEntryId::new("broken"),
            state: Some(WebFiberState::Failed),
            missing_services: Vec::new(),
        },
    ];
    assert_eq!(
        assert_web_entries_active(&failures).unwrap_err(),
        "web boot: 3 entries did not activate\nmissing: import failed (see console for the import error)\nwaiting: pending (waiting for services: slots, layout)\nbroken: failed"
    );
}

#[test]
fn fiber_labels_platform_words_and_shell_assets_are_exact() {
    assert_eq!(WebFiberState::Pending as u8, 0);
    assert_eq!(WebFiberState::Loading as u8, 1);
    assert_eq!(WebFiberState::Active as u8, 2);
    assert_eq!(WebFiberState::Failed as u8, 3);
    assert_eq!(WebFiberState::Disposed as u8, 4);
    assert_eq!(WebFiberState::Unloading as u8, 5);
    assert_eq!(PLATFORM_MODULES.len(), 11);
    assert_eq!(PLATFORM_MODULES[0], "react");
    assert_eq!(PLATFORM_MODULES[4], "immer");
    assert_eq!(
        PLATFORM_MODULES[7],
        "@seekdeep-ai/seekdeep-client-web-react"
    );
    assert_eq!(APP_SHELL_ID, "@seekdeep-ai/seekdeep-client-app-shell");
    assert_eq!(MODULES_ID, "@seekdeep-ai/seekdeep-client-modules");
    assert!(APP_ROOT_STYLES.contains(".seekdeep-web-boot"));
    assert!(!APP_ROOT_STYLES.contains("Loading plugins"));
    assert!(BASE_STYLES.contains("@seekdeep-ai/seekdeep-client-ui-theme/styles/base.css"));
    assert!(BASE_STYLES.contains("-webkit-font-smoothing"));
    let imports = BASE_STYLES
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("@import '")
                .and_then(|line| line.strip_suffix("';"))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        imports,
        [
            "@seekdeep-ai/seekdeep-client-ui-theme/styles/base.css",
            "@seekdeep-ai/seekdeep-client-ui-theme/styles/design-platform.css",
            "@seekdeep-ai/seekdeep-client-ui-theme/styles/scrollbar.css",
            "@seekdeep-ai/seekdeep-client-ui-theme/styles/gradient-shadow-text.css",
            "@seekdeep-ai/seekdeep-client-ui-theme/styles/shiki.css",
        ]
    );
    assert!(
        imports
            .iter()
            .position(|name| name.ends_with("scrollbar.css"))
            > imports
                .iter()
                .position(|name| name.ends_with("design-platform.css"))
    );
}
