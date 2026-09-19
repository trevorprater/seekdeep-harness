//! Reference-stable settings section and onboarding ledger projections.

use std::{cell::RefCell, cmp::Ordering, rc::Rc};

/// One settings-section registration projected into shell navigation.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingsSectionEntry {
    /// Optional list-cell identity; Slot registration makes it mandatory.
    pub id: Option<String>,
    /// Optional display order, defaulting to zero.
    pub order: Option<f64>,
    /// Resolved static or locale-following label.
    pub label: Option<String>,
}

/// One shell navigation row.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingsSectionRow {
    /// Section identity.
    pub id: String,
    /// Stable display order.
    pub order: f64,
    /// Current resolved label.
    pub label: String,
}

/// One onboarding registration projected into coordinator order.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingsOnboardingEntry {
    /// Optional list-cell identity; Slot registration makes it mandatory.
    pub id: Option<String>,
    /// Optional display order, defaulting to zero.
    pub order: Option<f64>,
}

/// One ordered onboarding coordinator step.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingsOnboardingStep {
    /// Step identity.
    pub id: String,
    /// Stable display order.
    pub order: f64,
}

#[derive(Default)]
struct ProjectionState {
    sections: Option<(u64, u64, Rc<Vec<SettingsSectionRow>>)>,
    onboarding: Option<(u64, Rc<Vec<SettingsOnboardingStep>>)>,
}

/// Caches ledger projections until their owning Slot or locale revision moves.
#[derive(Default)]
pub struct SettingsLedgerProjection {
    state: RefCell<ProjectionState>,
}

impl SettingsLedgerProjection {
    /// Projects section entries and retains exact snapshot identity between version changes.
    #[must_use]
    pub fn sections(
        &self,
        version: u64,
        locale_revision: u64,
        entries: impl IntoIterator<Item = SettingsSectionEntry>,
    ) -> Rc<Vec<SettingsSectionRow>> {
        if let Some((current_version, current_revision, rows)) =
            self.state.borrow().sections.as_ref()
            && *current_version == version
            && *current_revision == locale_revision
        {
            return rows.clone();
        }
        let mut rows = entries
            .into_iter()
            .map(|entry| SettingsSectionRow {
                id: entry.id.unwrap_or_default(),
                order: entry.order.unwrap_or_default(),
                label: entry.label.unwrap_or_default(),
            })
            .collect::<Vec<_>>();
        rows.sort_by(|left, right| {
            left.order
                .partial_cmp(&right.order)
                .unwrap_or(Ordering::Equal)
        });
        let rows = Rc::new(rows);
        self.state.borrow_mut().sections = Some((version, locale_revision, rows.clone()));
        rows
    }

    /// Projects onboarding entries and retains exact snapshot identity between Slot versions.
    #[must_use]
    pub fn onboarding(
        &self,
        version: u64,
        entries: impl IntoIterator<Item = SettingsOnboardingEntry>,
    ) -> Rc<Vec<SettingsOnboardingStep>> {
        if let Some((current_version, steps)) = self.state.borrow().onboarding.as_ref()
            && *current_version == version
        {
            return steps.clone();
        }
        let mut steps = entries
            .into_iter()
            .map(|entry| SettingsOnboardingStep {
                id: entry.id.unwrap_or_default(),
                order: entry.order.unwrap_or_default(),
            })
            .collect::<Vec<_>>();
        steps.sort_by(|left, right| {
            left.order
                .partial_cmp(&right.order)
                .unwrap_or(Ordering::Equal)
        });
        let steps = Rc::new(steps);
        self.state.borrow_mut().onboarding = Some((version, steps.clone()));
        steps
    }
}
