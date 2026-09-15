//! Portable directory-browser path, filter, and landing parity.

use seekdeep_client_ui_directory_picker_browse::*;

const HOME: &str = "/home/u";
const DOCS: &str = "/home/u/Documents";

fn entry(name: &str, path: &str, hidden: bool) -> DirectoryEntry {
    DirectoryEntry {
        name: name.to_owned(),
        path: path.to_owned(),
        hidden,
    }
}

fn listing(
    path: &str,
    home: &str,
    crumbs: Vec<DirectoryEntry>,
    entries: Vec<DirectoryEntry>,
) -> DirectoryListing {
    DirectoryListing {
        path: path.to_owned(),
        home: home.to_owned(),
        crumbs,
        entries,
        truncated: false,
    }
}

fn home() -> DirectoryListing {
    listing(
        HOME,
        HOME,
        vec![
            entry("/", "/", false),
            entry("home", "/home", false),
            entry("u", HOME, false),
        ],
        vec![
            entry(".config", "/home/u/.config", true),
            entry("Documents", DOCS, false),
            entry("Downloads", "/home/u/Downloads", false),
        ],
    )
}

#[test]
fn crumbs_separator_and_draft_read_match_host_spelling_rules() {
    let home = home();
    assert_eq!(
        display_crumbs(&home, "Home"),
        vec![entry("Home", HOME, false)]
    );
    assert_eq!(separator_of(&home), PathSeparator::Posix);
    assert_eq!(level_directory(&home), "/home/u/");
    assert_eq!(
        draft_directory(&home, "/home/u/Doc"),
        Some("/home/u/".to_owned())
    );
    assert_eq!(
        read_draft(&home, "/home/u/Doc", None),
        DraftRead {
            directory: Some("/home/u/".to_owned()),
            tail: Some("Doc".to_owned()),
        }
    );
    assert_eq!(
        read_draft(
            &home,
            "/home/u/../u/Doc",
            Some(&ScannedDirectory {
                directory: "/home/u/../u/".to_owned(),
                landed: HOME.to_owned(),
            }),
        )
        .tail,
        Some("Doc".to_owned())
    );
    assert_eq!(draft_directory(&home, "Documents"), None);

    let windows = listing(
        "C:\\Users\\u",
        "C:\\Users\\u",
        vec![entry("u", "C:\\Users\\u", false)],
        Vec::new(),
    );
    assert_eq!(separator_of(&windows), PathSeparator::Windows);
    assert_eq!(level_directory(&windows), "C:\\Users\\u\\");
    assert_eq!(
        draft_directory(&windows, "C:/Users/u/Documents"),
        Some("C:/Users/u/".to_owned())
    );
}

#[test]
fn visible_entries_preserve_selection_and_dot_reveal_semantics() {
    let home = home();
    assert_eq!(
        visible_entries(&home.entries, None, false, None)
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["Documents", "Downloads"]
    );
    assert_eq!(
        visible_entries(&home.entries, None, false, Some("doc"))
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["Documents"]
    );
    assert_eq!(
        visible_entries(&home.entries, None, false, Some("missing"))
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["Documents", "Downloads"]
    );
    assert_eq!(
        visible_entries(&home.entries, None, false, Some(".c"))
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        [".config"]
    );
    assert_eq!(
        visible_entries(&home.entries, Some("/home/u/.config"), false, None,)
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        [".config", "Documents", "Downloads"]
    );
}

#[test]
fn landing_is_single_at_root_or_missing_anchor_and_two_pane_on_exact_parent_entry() {
    let root = listing(
        "/",
        HOME,
        vec![entry("/", "/", false)],
        vec![entry("home", "/home", false)],
    );
    assert!(is_display_root(&root));
    assert_eq!(resolve_landing(root.clone(), None).child, None);

    let target = listing(
        DOCS,
        HOME,
        vec![
            entry("/", "/", false),
            entry("home", "/home", false),
            entry("u", HOME, false),
            entry("Documents", DOCS, false),
        ],
        Vec::new(),
    );
    let parent = home();
    let landing = resolve_landing(target.clone(), Some(parent.clone()));
    assert_eq!(landing.parent, parent);
    assert_eq!(landing.selected, Some(entry("Documents", DOCS, false)));
    assert_eq!(landing.child, Some(target.clone()));

    let truncated_parent = listing(HOME, HOME, parent.crumbs, Vec::new());
    let single = resolve_landing(target.clone(), Some(truncated_parent));
    assert_eq!(single.parent, target);
    assert_eq!(single.selected, None);
    assert_eq!(single.child, None);
}

#[test]
fn windows_parent_anchor_folds_case_and_targets_prefer_selection() {
    let target = listing(
        "C:\\Users\\U\\Documents",
        "C:\\Users\\U",
        vec![
            entry("C:\\", "C:\\", false),
            entry("Users", "C:\\Users", false),
            entry("U", "C:\\Users\\U", false),
            entry("Documents", "C:\\Users\\U\\Documents", false),
        ],
        Vec::new(),
    );
    let parent = listing(
        "C:\\Users\\U",
        "C:\\Users\\U",
        vec![entry("U", "C:\\Users\\U", false)],
        vec![entry("documents", "c:\\users\\u\\documents", false)],
    );
    let landing = resolve_landing(target.clone(), Some(parent));
    assert_eq!(
        landing.selected,
        Some(entry("documents", "c:\\users\\u\\documents", false))
    );
    assert_eq!(
        target_path(Some(&target), landing.selected.as_ref()),
        Some("c:\\users\\u\\documents")
    );
    assert_eq!(target_name(Some(&target), None, "Home"), "Documents");
    assert_eq!(target_name(None, None, "Home"), "");
}
