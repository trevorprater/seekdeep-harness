//! Target-portable directory listing, draft, filter, and landing projection.

use serde::{Deserialize, Serialize};

/// Slow-scan silence window before the loading indicator appears.
pub const SLOW_SCAN_DELAY_MS: u64 = 300;
/// Maximum submitted-navigation wait for the parent listing leg.
pub const PARENT_LEG_WAIT_MS: u64 = 200;
/// Draft-following debounce before scanning an unlisted level.
pub const DRAFT_PREVIEW_DEBOUNCE_MS: u64 = 250;

/// One directory row: a listing child or breadcrumb ancestor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryEntry {
    /// Base name shown in the browser.
    pub name: String,
    /// Absolute Host path.
    pub path: String,
    /// Host-authored hidden marker.
    pub hidden: bool,
}

/// One directory level plus its root-to-level ancestry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryListing {
    /// Listed absolute path.
    pub path: String,
    /// Host account home directory.
    pub home: String,
    /// Filesystem-root-to-level ancestry.
    pub crumbs: Vec<DirectoryEntry>,
    /// Direct child directories.
    pub entries: Vec<DirectoryEntry>,
    /// Whether the backend cut the sorted tail.
    pub truncated: bool,
}

/// Platform separator inferred from the Host-stamped home path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathSeparator {
    /// POSIX `/` paths.
    Posix,
    /// Windows `\` paths, accepting `/` in drafts too.
    Windows,
}

impl PathSeparator {
    /// Canonical separator used to terminate the listed level.
    #[must_use]
    pub const fn as_char(self) -> char {
        match self {
            Self::Posix => '/',
            Self::Windows => '\\',
        }
    }
}

/// Draft directory text and the Host-spelled level it produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScannedDirectory {
    /// Draft directory part sent to the Host.
    pub directory: String,
    /// Listing path returned by the Host.
    pub landed: String,
}

/// One draft interpreted against a specific listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftRead {
    /// Everything through the final platform separator.
    pub directory: Option<String>,
    /// Final segment only when this listing answers the directory part.
    pub tail: Option<String>,
}

/// Selection-anchored landing after the target and optional parent legs settle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryLanding {
    /// Left/single pane listing.
    pub parent: DirectoryListing,
    /// Actual parent-level entry anchoring a two-pane landing.
    pub selected: Option<DirectoryEntry>,
    /// Target listing in the right pane.
    pub child: Option<DirectoryListing>,
}

/// Returns the path platform inferred exactly from the Host-stamped home path.
#[must_use]
pub fn separator_of(listing: &DirectoryListing) -> PathSeparator {
    if listing.home.contains('\\') {
        PathSeparator::Windows
    } else {
        PathSeparator::Posix
    }
}

/// Collapses ancestry inside the home subtree to one localized Home crumb.
#[must_use]
pub fn display_crumbs(listing: &DirectoryListing, home_label: &str) -> Vec<DirectoryEntry> {
    let Some(home_index) = listing
        .crumbs
        .iter()
        .position(|crumb| crumb.path == listing.home)
    else {
        return listing.crumbs.clone();
    };
    let mut crumbs = vec![DirectoryEntry {
        name: home_label.to_owned(),
        path: listing.home.clone(),
        hidden: false,
    }];
    crumbs.extend(listing.crumbs.iter().skip(home_index + 1).cloned());
    crumbs
}

/// Returns the listed level terminated by its canonical platform separator.
#[must_use]
pub fn level_directory(listing: &DirectoryListing) -> String {
    let separator = separator_of(listing).as_char();
    if listing.path.ends_with(separator) {
        listing.path.clone()
    } else {
        format!("{}{separator}", listing.path)
    }
}

/// Returns everything through the draft's final platform separator.
#[must_use]
pub fn draft_directory(listing: &DirectoryListing, draft: &str) -> Option<String> {
    let cut = match separator_of(listing) {
        PathSeparator::Posix => draft.rfind('/'),
        PathSeparator::Windows => match (draft.rfind('\\'), draft.rfind('/')) {
            (Some(left), Some(right)) => Some(left.max(right)),
            (left, right) => left.or(right),
        },
    }?;
    Some(draft[..=cut].to_owned())
}

/// Reads one draft against the level it may address.
#[must_use]
pub fn read_draft(
    listing: &DirectoryListing,
    draft: &str,
    scanned: Option<&ScannedDirectory>,
) -> DraftRead {
    let Some(directory) = draft_directory(listing, draft) else {
        return DraftRead {
            directory: None,
            tail: None,
        };
    };
    let answers = directory == level_directory(listing)
        || scanned.is_some_and(|scanned| {
            scanned.directory == directory && scanned.landed == listing.path
        });
    let tail = answers.then(|| draft[directory.len()..].to_owned());
    DraftRead {
        directory: Some(directory),
        tail,
    }
}

/// Applies hidden and prefix filters while keeping the selected row visible.
#[must_use]
pub fn visible_entries(
    entries: &[DirectoryEntry],
    selected_path: Option<&str>,
    show_hidden: bool,
    filter_prefix: Option<&str>,
) -> Vec<DirectoryEntry> {
    let needle = filter_prefix.unwrap_or_default().to_lowercase();
    let reveals_hidden = needle.starts_with('.');
    let displayable = |entry: &DirectoryEntry| show_hidden || !entry.hidden || reveals_hidden;
    let matches = |entry: &DirectoryEntry| {
        displayable(entry) && entry.name.to_lowercase().starts_with(&needle)
    };
    let narrowing = !needle.is_empty() && entries.iter().any(matches);
    entries
        .iter()
        .filter(|entry| {
            if selected_path == Some(entry.path.as_str()) {
                true
            } else if narrowing {
                matches(entry)
            } else {
                show_hidden || !entry.hidden
            }
        })
        .cloned()
        .collect()
}

/// Whether the target is the collapsed display root and must land single-pane.
#[must_use]
pub fn is_display_root(listing: &DirectoryListing) -> bool {
    display_crumbs(listing, "").len() < 2
}

/// Resolves an actual parent entry with Windows-only case folding.
#[must_use]
pub fn parent_entry(parent: &DirectoryListing, target_path: &str) -> Option<DirectoryEntry> {
    match separator_of(parent) {
        PathSeparator::Posix => parent
            .entries
            .iter()
            .find(|entry| entry.path == target_path)
            .cloned(),
        PathSeparator::Windows => {
            let folded = target_path.to_lowercase();
            parent
                .entries
                .iter()
                .find(|entry| entry.path.to_lowercase() == folded)
                .cloned()
        }
    }
}

/// Resolves the final single- or two-pane landing after a target scan.
#[must_use]
pub fn resolve_landing(
    target: DirectoryListing,
    parent: Option<DirectoryListing>,
) -> DirectoryLanding {
    if is_display_root(&target) {
        return DirectoryLanding {
            parent: target,
            selected: None,
            child: None,
        };
    }
    let Some(parent) = parent else {
        return DirectoryLanding {
            parent: target,
            selected: None,
            child: None,
        };
    };
    let Some(selected) = parent_entry(&parent, &target.path) else {
        return DirectoryLanding {
            parent: target,
            selected: None,
            child: None,
        };
    };
    DirectoryLanding {
        parent,
        selected: Some(selected),
        child: Some(target),
    }
}

/// Directory acted on by Open/create: selection first, then the listed level.
#[must_use]
pub fn target_path<'a>(
    parent: Option<&'a DirectoryListing>,
    selected: Option<&'a DirectoryEntry>,
) -> Option<&'a str> {
    selected
        .map(|entry| entry.path.as_str())
        .or_else(|| parent.map(|listing| listing.path.as_str()))
}

/// Display name for the create target.
#[must_use]
pub fn target_name(
    parent: Option<&DirectoryListing>,
    selected: Option<&DirectoryEntry>,
    home_label: &str,
) -> String {
    if let Some(selected) = selected {
        return selected.name.clone();
    }
    let Some(parent) = parent else {
        return String::new();
    };
    display_crumbs(parent, home_label)
        .last()
        .map_or_else(|| parent.path.clone(), |crumb| crumb.name.clone())
}
