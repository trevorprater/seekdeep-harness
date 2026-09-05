//! Portable cross-plugin panel controller and presenter-owned token ledger.

use std::{cell::RefCell, fmt, rc::Rc};

/// Panel action face adopted by the cross-plugin controller.
pub trait PanelActionSink {
    /// Toggles the sidebar.
    fn toggle_sidebar(&self);
    /// Opens details.
    fn open_details(&self);
    /// Closes details.
    fn close_details(&self);
}

/// Error raised when UI code reaches the controller before root-entry wiring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PanelsNotWired;

impl fmt::Display for PanelsNotWired {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("layout: panel actions not wired (root entry not mounted)")
    }
}

impl std::error::Error for PanelsNotWired {}

/// Cross-plugin panel controller with replaceable root-entry wiring.
#[derive(Clone, Default)]
pub struct LayoutController {
    panels: Rc<RefCell<Option<Rc<dyn PanelActionSink>>>>,
}

impl fmt::Debug for LayoutController {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LayoutController")
            .field("wired", &self.panels.borrow().is_some())
            .finish()
    }
}

impl LayoutController {
    /// Creates an unwired service face.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adopts the current root store's bound actions, replacing stale wiring.
    pub fn attach_panels(&self, panels: Rc<dyn PanelActionSink>) {
        *self.panels.borrow_mut() = Some(panels);
    }

    fn panels(&self) -> Result<Rc<dyn PanelActionSink>, PanelsNotWired> {
        self.panels.borrow().clone().ok_or(PanelsNotWired)
    }

    /// Forwards a sidebar toggle.
    ///
    /// # Errors
    ///
    /// Returns [`PanelsNotWired`] before the root entry attaches its actions.
    pub fn toggle_sidebar(&self) -> Result<(), PanelsNotWired> {
        self.panels()?.toggle_sidebar();
        Ok(())
    }

    /// Forwards a details-open request.
    ///
    /// # Errors
    ///
    /// Returns [`PanelsNotWired`] before the root entry attaches its actions.
    pub fn open_details(&self) -> Result<(), PanelsNotWired> {
        self.panels()?.open_details();
        Ok(())
    }

    /// Forwards a details-close request.
    ///
    /// # Errors
    ///
    /// Returns [`PanelsNotWired`] before the root entry attaches its actions.
    pub fn close_details(&self) -> Result<(), PanelsNotWired> {
        self.panels()?.close_details();
        Ok(())
    }
}

/// Presenter-owned CSS variable names from the previous theme application.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThemeTokenLedger {
    applied: Vec<String>,
}

impl ThemeTokenLedger {
    /// Replaces the applied-name set and returns the exact prior retraction set.
    pub fn replace(&mut self, next: Vec<String>) -> Vec<String> {
        std::mem::replace(&mut self.applied, next)
    }

    /// Drains the final retraction set during disposal.
    pub fn drain(&mut self) -> Vec<String> {
        std::mem::take(&mut self.applied)
    }

    /// Current presenter-owned names in source insertion order.
    #[must_use]
    pub fn applied(&self) -> &[String] {
        &self.applied
    }
}
