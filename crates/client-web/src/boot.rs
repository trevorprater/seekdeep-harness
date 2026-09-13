//! Boot-page projection and post-settlement entry audit.

use crate::{LoaderStatus, WebFiberState};

/// Boot-page state derived from kernel signals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppRootView {
    /// Real UI may render.
    Settled,
    /// Loading spinner and hint.
    Loading,
    /// Fail-loud list and optional sweep message.
    Failed {
        /// Entries whose fiber state is failed, in status insertion order.
        entries: Vec<String>,
        /// Boot/sweep failure message.
        error: Option<String>,
    },
}

/// Projects the exact `AppRoot` branch.
#[must_use]
pub fn app_root_view(settled: bool, status: &LoaderStatus, error: Option<&str>) -> AppRootView {
    if settled {
        return AppRootView::Settled;
    }
    let failed = status
        .iter()
        .filter(|(_, state)| **state == WebFiberState::Failed)
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    if error.is_none() && failed.is_empty() {
        AppRootView::Loading
    } else {
        AppRootView::Failed {
            entries: failed,
            error: error.map(str::to_owned),
        }
    }
}

/// Stable entry identity crossing the boot manifest/loader boundary.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct WebEntryId(String);

impl WebEntryId {
    /// Creates an exact manifest identity.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrowed wire spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One post-loader entry snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebBootEntry {
    /// Graph identity.
    pub id: WebEntryId,
    /// Missing fiber means import failure.
    pub state: Option<WebFiberState>,
    /// Injected services absent while pending.
    pub missing_services: Vec<String>,
}

/// Exact fail-loud activation sweep.
///
/// # Errors
///
/// Returns one aggregate naming import failures, pending services, and non-active states.
pub fn assert_web_entries_active(entries: &[WebBootEntry]) -> Result<(), String> {
    let mut failures = Vec::new();
    for entry in entries {
        match entry.state {
            None => failures.push(format!(
                "{}: import failed (see console for the import error)",
                entry.id.as_str()
            )),
            Some(WebFiberState::Active) => {}
            Some(WebFiberState::Pending) => {
                let noun = if entry.missing_services.len() == 1 {
                    "service"
                } else {
                    "services"
                };
                let missing = if entry.missing_services.is_empty() {
                    "unknown".to_owned()
                } else {
                    entry.missing_services.join(", ")
                };
                failures.push(format!(
                    "{}: pending (waiting for {noun}: {missing})",
                    entry.id.as_str()
                ));
            }
            Some(state) => failures.push(format!("{}: {}", entry.id.as_str(), state.label())),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        let noun = if failures.len() == 1 {
            "entry"
        } else {
            "entries"
        };
        Err(format!(
            "web boot: {} {noun} did not activate\n{}",
            failures.len(),
            failures.join("\n")
        ))
    }
}
