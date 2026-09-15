//! Portable `WorkspaceBrowser` ordering, query, and drop behavior.

use indexmap::IndexMap;
use seekdeep_client_ui_workspace::{
    DropHalf, SEARCH_QUERY_MAX_CODE_UNITS, SessionOrderBy, next_session_order_account,
    reconciled_session_order, resolve_session_drop, resolve_workspace_drop, sanitize_search_query,
};
use seekdeep_identity::{SessionId, WorkspaceId};

fn sid(value: &str) -> SessionId {
    SessionId::new(value)
}

fn wid(value: &str) -> WorkspaceId {
    WorkspaceId::new(value)
}

#[test]
fn search_sanitization_uses_utf16_bound_without_splitting_astral_characters() {
    let query = format!("{}\0😀tail", "a".repeat(499));
    let sanitized = sanitize_search_query(&query);
    assert_eq!(sanitized, "a".repeat(499));
    assert_eq!(sanitized.encode_utf16().count(), 499);
    let exact = format!("{}😀", "a".repeat(498));
    assert_eq!(sanitize_search_query(&exact), exact);
    assert_eq!(exact.encode_utf16().count(), SEARCH_QUERY_MAX_CODE_UNITS);
}

#[test]
fn stored_order_reconciles_stale_duplicate_and_new_account_members() {
    let ids = [sid("one"), sid("two"), sid("three")];
    assert_eq!(
        reconciled_session_order(
            &ids,
            Some(&[
                "three".to_owned(),
                "missing".to_owned(),
                "three".to_owned(),
                "one".to_owned(),
            ]),
        ),
        vec![sid("three"), sid("one"), sid("two")]
    );
}

#[test]
fn updated_order_sorts_on_entry_then_promotes_only_newer_activity() {
    let ids = [sid("one"), sid("two"), sid("three")];
    let timestamps = IndexMap::from([(sid("one"), 3), (sid("two"), 5), (sid("three"), 5)]);
    let sorted = next_session_order_account(
        &ids,
        None,
        &IndexMap::new(),
        &timestamps,
        SessionOrderBy::Updated,
        true,
    );
    assert_eq!(sorted.order, vec![sid("three"), sid("two"), sid("one")]);
    assert!(sorted.changed);

    let previous = ["three".to_owned(), "two".to_owned(), "one".to_owned()];
    let baseline = IndexMap::from([
        ("one".to_owned(), 3),
        ("two".to_owned(), 5),
        ("three".to_owned(), 5),
    ]);
    let promoted_timestamps = IndexMap::from([(sid("one"), 6), (sid("two"), 5), (sid("three"), 5)]);
    let promoted = next_session_order_account(
        &ids,
        Some(&previous),
        &baseline,
        &promoted_timestamps,
        SessionOrderBy::Updated,
        false,
    );
    assert_eq!(promoted.order, vec![sid("one"), sid("three"), sid("two")]);
    assert!(promoted.changed);

    let manual = next_session_order_account(
        &ids,
        Some(&previous),
        &baseline,
        &promoted_timestamps,
        SessionOrderBy::Manual,
        false,
    );
    assert_eq!(manual.order, vec![sid("three"), sid("two"), sid("one")]);
    assert!(
        manual.changed,
        "timestamp baselines still advance in Manual mode"
    );
}

#[test]
fn session_drop_resolves_halves_noops_append_and_vanished_source() {
    let visible = [sid("one"), sid("two"), sid("three")];
    let before = resolve_session_drop(
        &visible,
        &visible,
        &sid("one"),
        &sid("three"),
        DropHalf::Before,
    )
    .unwrap();
    assert_eq!(before.before, Some(sid("three")));
    assert_eq!(before.order, vec![sid("two"), sid("one"), sid("three")]);
    let after = resolve_session_drop(
        &visible,
        &visible,
        &sid("one"),
        &sid("three"),
        DropHalf::After,
    )
    .unwrap();
    assert_eq!(after.before, None);
    assert_eq!(after.order, vec![sid("two"), sid("three"), sid("one")]);
    assert!(
        resolve_session_drop(
            &visible,
            &visible,
            &sid("one"),
            &sid("one"),
            DropHalf::Before,
        )
        .is_none()
    );
    assert!(
        resolve_session_drop(
            &visible,
            &visible,
            &sid("one"),
            &sid("one"),
            DropHalf::After,
        )
        .is_none()
    );

    let vanished = resolve_session_drop(
        &[sid("two")],
        &[sid("two")],
        &sid("one"),
        &sid("two"),
        DropHalf::Before,
    )
    .unwrap();
    assert_eq!(vanished.before, Some(sid("two")));
    assert_eq!(vanished.order, vec![sid("one"), sid("two")]);
}

#[test]
fn workspace_drop_resolves_anchor_and_rejects_adjacent_noops() {
    let ids = [wid("alpha"), wid("beta"), wid("tail")];
    assert_eq!(
        resolve_workspace_drop(&ids, &wid("tail"), &wid("beta"), DropHalf::Before,)
            .unwrap()
            .before,
        Some(wid("beta"))
    );
    assert_eq!(
        resolve_workspace_drop(&ids, &wid("alpha"), &wid("tail"), DropHalf::After,)
            .unwrap()
            .before,
        None
    );
    assert!(resolve_workspace_drop(&ids, &wid("alpha"), &wid("alpha"), DropHalf::After,).is_none());
}
