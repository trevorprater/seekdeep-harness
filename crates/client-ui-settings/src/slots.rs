//! Canonical settings-domain Slot identities and owner currencies.

use std::rc::Rc;

use seekdeep_client_ui_slots::{ListSlot, SingleSlot, SlotScope, TypedSlot};

/// Sidebar-foot trigger content.
pub const SETTINGS_TRIGGER_SLOT: &str = "settings.trigger";
/// Settings-panel title content.
pub const SETTINGS_HEADER_SLOT: &str = "settings.header";
/// Ordered settings-panel header actions.
pub const SETTINGS_ACTION_SLOT: &str = "settings.action";
/// Settings-panel close accessible label.
pub const SETTINGS_CLOSE_SLOT: &str = "settings.close";
/// Ordered feature-owned settings pages.
pub const SETTINGS_SECTION_SLOT: &str = "settings.section";
/// Ordered pages inside the Plugins section.
pub const SETTINGS_PLUGINS_TAB_SLOT: &str = "settings.plugins.tab";
/// Ordered feature-owned onboarding steps.
pub const SETTINGS_ONBOARDING_SLOT: &str = "settings.onboarding";
/// Ordered preference rows inside General.
pub const SETTINGS_GENERAL_ITEM_SLOT: &str = "settings.general.item";

/// Owner currency for the trigger contribution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettingsTriggerOwner {
    /// Whether the sidebar currently renders its wide column.
    pub wide: bool,
}

/// Intentionally empty owner currency for header, action, and close contributions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SettingsHeaderOwner;

/// Owner currency for a settings section.
#[derive(Clone)]
pub struct SettingsSectionOwner {
    /// Closes the settings panel.
    pub close: Rc<dyn Fn()>,
}

/// Intentionally empty owner currency for a Plugins tab.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SettingsPluginsTabOwner;

/// Owner currency for the currently active settings-backed onboarding step.
#[derive(Clone)]
pub struct SettingsOnboardingOwner {
    /// Stable active step identity.
    pub step_id: String,
    /// Completes or skips the step.
    pub complete: Rc<dyn Fn()>,
    /// Opens one settings section by identity.
    pub open_section: Rc<dyn Fn(&str)>,
}

/// Intentionally empty owner currency for a General preference row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SettingsGeneralItemOwner;

/// Typed trigger Slot declaration.
#[must_use]
pub fn settings_trigger_slot() -> TypedSlot<SingleSlot> {
    TypedSlot::<SingleSlot>::new(SETTINGS_TRIGGER_SLOT, SlotScope::Root)
}

/// Typed header Slot declaration.
#[must_use]
pub fn settings_header_slot() -> TypedSlot<SingleSlot> {
    TypedSlot::<SingleSlot>::new(SETTINGS_HEADER_SLOT, SlotScope::Root)
}

/// Typed action Slot declaration.
#[must_use]
pub fn settings_action_slot() -> TypedSlot<ListSlot> {
    TypedSlot::<ListSlot>::new(SETTINGS_ACTION_SLOT, SlotScope::Root)
}

/// Typed close-label Slot declaration.
#[must_use]
pub fn settings_close_slot() -> TypedSlot<SingleSlot> {
    TypedSlot::<SingleSlot>::new(SETTINGS_CLOSE_SLOT, SlotScope::Root)
}

/// Typed settings-section Slot declaration.
#[must_use]
pub fn settings_section_slot() -> TypedSlot<ListSlot> {
    TypedSlot::<ListSlot>::new(SETTINGS_SECTION_SLOT, SlotScope::Root)
}

/// Typed Plugins-tab Slot declaration.
#[must_use]
pub fn settings_plugins_tab_slot() -> TypedSlot<ListSlot> {
    TypedSlot::<ListSlot>::new(SETTINGS_PLUGINS_TAB_SLOT, SlotScope::Root)
}

/// Typed onboarding Slot declaration.
#[must_use]
pub fn settings_onboarding_slot() -> TypedSlot<ListSlot> {
    TypedSlot::<ListSlot>::new(SETTINGS_ONBOARDING_SLOT, SlotScope::Root)
}

/// Typed General-row Slot declaration.
#[must_use]
pub fn settings_general_item_slot() -> TypedSlot<ListSlot> {
    TypedSlot::<ListSlot>::new(SETTINGS_GENERAL_ITEM_SLOT, SlotScope::Root)
}
