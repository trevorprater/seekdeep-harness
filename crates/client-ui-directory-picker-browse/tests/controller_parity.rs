//! Deterministic navigation, supersession, timing, and creation parity.

use seekdeep_client_ui_directory_picker_browse::*;

const HOME: &str = "/home/u";
const DOCS: &str = "/home/u/Documents";
const HARNESS: &str = "/home/u/Documents/harness";

fn entry(name: &str, path: &str, hidden: bool) -> DirectoryEntry {
    DirectoryEntry {
        name: name.to_owned(),
        path: path.to_owned(),
        hidden,
    }
}

fn listing(path: &str, entries: Vec<DirectoryEntry>) -> DirectoryListing {
    let mut crumbs = vec![
        entry("/", "/", false),
        entry("home", "/home", false),
        entry("u", HOME, false),
    ];
    if path.starts_with(DOCS) {
        crumbs.push(entry("Documents", DOCS, false));
    }
    if path == HARNESS {
        crumbs.push(entry("harness", HARNESS, false));
    }
    DirectoryListing {
        path: path.to_owned(),
        home: HOME.to_owned(),
        crumbs,
        entries,
        truncated: false,
    }
}

fn home() -> DirectoryListing {
    listing(HOME, vec![entry("Documents", DOCS, false)])
}

fn docs() -> DirectoryListing {
    listing(DOCS, vec![entry("harness", HARNESS, false)])
}

fn harness() -> DirectoryListing {
    listing(HARNESS, Vec::new())
}

#[test]
fn opening_target_parent_wait_and_late_upgrade_preserve_one_frame_landing() {
    let mut controller = DirectoryBrowserController::new();
    let home_launch = controller.open();
    assert_eq!(home_launch.path, None);
    assert!(controller.state().loading);
    assert_eq!(
        controller.target_landed(&home_launch, home(), LandingOptions::SUBMITTED),
        TargetLanding::CommittedSingle
    );
    assert_eq!(controller.state().parent.as_ref().unwrap().path, HOME);

    let target_launch =
        controller.begin_landing(Some(HARNESS.to_owned()), LandingOptions::SUBMITTED);
    let TargetLanding::Parent(parent_leg) =
        controller.target_landed(&target_launch, harness(), LandingOptions::SUBMITTED)
    else {
        panic!("expected parent leg");
    };
    assert_eq!(parent_leg.path, DOCS);
    assert!(parent_leg.bounded_wait);
    assert_eq!(controller.state().parent.as_ref().unwrap().path, HOME);
    assert!(controller.parent_wait_elapsed(parent_leg.seq));
    assert_eq!(controller.state().parent.as_ref().unwrap().path, HARNESS);
    assert_eq!(controller.state().child, None);
    assert!(controller.parent_landed(parent_leg.seq, docs()));
    assert_eq!(controller.state().parent.as_ref().unwrap().path, DOCS);
    assert_eq!(controller.state().selected.as_ref().unwrap().path, HARNESS);
    assert_eq!(controller.state().child.as_ref().unwrap().path, HARNESS);
}

#[test]
fn newer_intent_rejects_stale_target_parent_and_selection_settlements() {
    let mut controller = DirectoryBrowserController::new();
    let first = controller.open();
    let second = controller.begin_landing(Some(DOCS.to_owned()), LandingOptions::SUBMITTED);
    assert_eq!(
        controller.target_landed(&first, home(), LandingOptions::SUBMITTED),
        TargetLanding::Stale
    );
    let TargetLanding::Parent(parent) =
        controller.target_landed(&second, docs(), LandingOptions::SUBMITTED)
    else {
        panic!("expected parent");
    };
    let selection = controller.begin_selection(entry("harness", HARNESS, false));
    assert!(!controller.parent_landed(parent.seq, home()));
    assert!(!controller.selection_landed(second.seq, docs()));
    assert!(controller.selection_landed(selection.seq, harness()));
}

#[test]
fn selection_failure_draft_cancel_and_preview_follow_source_postures() {
    let mut controller = DirectoryBrowserController::new();
    let launch = controller.open();
    controller.target_landed(&launch, home(), LandingOptions::SUBMITTED);
    controller.open_path_editor();
    assert_eq!(controller.state().path_draft.as_deref(), Some("/home/u/"));
    let token = controller.edit_path("/home/u/Documents/har".to_owned());
    let preview = controller
        .preview_elapsed(&token)
        .expect("walk to Documents");
    assert_eq!(preview.path.as_deref(), Some("/home/u/Documents/"));
    let TargetLanding::Parent(parent) =
        controller.target_landed(&preview, docs(), LandingOptions::PREVIEW)
    else {
        panic!("preview parent");
    };
    assert!(!parent.bounded_wait);
    assert!(controller.parent_landed(parent.seq, home()));
    assert_eq!(controller.consume_focus(), FocusRequest::PathInput);
    assert_eq!(
        read_draft(
            controller.state().child.as_ref().unwrap(),
            controller.state().path_draft.as_deref().unwrap(),
            controller.state().scanned.as_ref(),
        )
        .tail,
        Some("har".to_owned())
    );
    let selection = controller.begin_selection(entry("harness", HARNESS, false));
    assert_eq!(controller.consume_focus(), FocusRequest::Selection);
    assert!(controller.selection_failed(selection.seq, "denied".to_owned()));
    assert_eq!(controller.state().selected, None);
    assert_eq!(controller.consume_focus(), FocusRequest::EditZone);

    controller.open_path_editor();
    let token = controller.edit_path("/missing/".to_owned());
    let _ = controller.preview_elapsed(&token);
    assert!(controller.cancel_path_edit(false).is_none());
    assert_eq!(controller.state().path_draft, None);
}

#[test]
fn submitted_path_suspends_preview_and_preserves_untrimmed_text() {
    let mut controller = DirectoryBrowserController::new();
    let launch = controller.open();
    controller.target_landed(&launch, home(), LandingOptions::SUBMITTED);
    controller.open_path_editor();
    let token = controller.edit_path("/home/u/Documents  ".to_owned());
    let submitted = controller.submit_path().unwrap();
    assert_eq!(submitted.path.as_deref(), Some("/home/u/Documents  "));
    assert!(controller.state().preview_suspended);
    assert!(controller.preview_elapsed(&token).is_none());
    assert_eq!(controller.consume_focus(), FocusRequest::EditZone);
    assert!(controller.target_failed(
        submitted.seq,
        LandingOptions::SUBMITTED,
        "unreadable".to_owned(),
    ));
    assert_eq!(
        controller.state().path_draft.as_deref(),
        Some("/home/u/Documents  ")
    );
    assert_eq!(controller.state().error.as_deref(), Some("unreadable"));
}

#[test]
fn creation_generation_relist_selection_and_reopen_fences_are_exact() {
    let mut controller = DirectoryBrowserController::new();
    let launch = controller.open();
    controller.target_landed(&launch, home(), LandingOptions::SUBMITTED);
    assert!(controller.open_create_dialog());
    controller.edit_folder_name(" fresh ".to_owned());
    let create = controller.confirm_create().unwrap();
    assert_eq!(create.path, HOME);
    assert_eq!(create.name, " fresh ");
    let relist = controller
        .creation_succeeded(&create, "/home/u/ fresh ".to_owned())
        .unwrap();
    assert_eq!(relist.listing.path.as_deref(), Some(HOME));
    let level = listing(HOME, vec![entry(" fresh ", "/home/u/ fresh ", false)]);
    let child_launch = controller
        .creation_relist_landed(relist.listing.seq, level)
        .unwrap();
    assert_eq!(
        controller.state().selected.as_ref().unwrap().path,
        "/home/u/ fresh "
    );
    assert_eq!(child_launch.path.as_deref(), Some("/home/u/ fresh "));

    assert!(controller.selection_landed(child_launch.seq, listing("/home/u/ fresh ", Vec::new())));
    let stale = controller.confirm_create();
    assert!(stale.is_none(), "dialog closed after success");

    controller.open_create_dialog();
    controller.edit_folder_name("late".to_owned());
    let stale = controller.confirm_create().unwrap();
    controller.close();
    controller.open();
    assert!(
        controller
            .creation_succeeded(&stale, "/home/u/late".to_owned())
            .is_none()
    );
    assert!(!controller.creation_failed(&stale, "late failure".to_owned()));
}

#[test]
fn slow_scan_window_and_close_reopen_reset_do_not_leak_indicator_state() {
    let mut controller = DirectoryBrowserController::new();
    let first = controller.open();
    assert!(controller.slow_scan_elapsed(first.scan_window));
    assert!(controller.state().slow_scan);
    let second = controller.begin_selection(entry("Documents", DOCS, false));
    assert!(!controller.state().slow_scan);
    assert!(!controller.slow_scan_elapsed(first.scan_window));
    assert!(controller.slow_scan_elapsed(second.scan_window));
    controller.close();
    assert!(!controller.state().loading);
    assert!(!controller.state().slow_scan);
    let reopened = controller.open();
    assert!(!controller.state().slow_scan);
    assert_ne!(reopened.scan_window, second.scan_window);
}
