//! Pure column, transient store, Host, and stylesheet parity.

#![cfg(not(target_arch = "wasm32"))]
#![allow(clippy::float_cmp)] // The source contract produces exact integral pixel widths.

use std::{cell::Cell, rc::Rc};

use seekdeep_client_ui_layout::*;
use seekdeep_cordis::{Context, FiberState};
use serde_json::Value;

#[test]
#[allow(clippy::too_many_lines)]
fn column_solver_preserves_every_concession_seam_and_recovery_rule() {
    assert_eq!(clamp_width(250.4, 240.0, 420.0), 250.0);
    assert_eq!(clamp_width(100.0, 240.0, 420.0), 240.0);
    assert_eq!(clamp_width(9_999.0, 240.0, 420.0), 420.0);
    assert!(clamp_width(f64::NAN, 240.0, 420.0).is_nan());

    assert_eq!(
        compute_columns(1_920.0, SIDEBAR_DEFAULT, DETAILS_DEFAULT),
        Columns {
            sidebar: 280.0,
            center: 1_280.0,
            details: 360.0,
        }
    );
    assert_eq!(
        compute_columns(1_920.0, 0.0, 0.0),
        Columns {
            sidebar: SIDEBAR_COLLAPSED,
            center: 1_864.0,
            details: 0.0,
        }
    );
    assert_eq!(
        compute_columns(1_250.0, SIDEBAR_DEFAULT, DETAILS_DEFAULT),
        Columns {
            sidebar: 280.0,
            center: CENTER_MIN,
            details: 330.0,
        }
    );
    assert_eq!(
        compute_columns(1_300.0, 300.0, 360.0),
        Columns {
            sidebar: 300.0,
            center: CENTER_MIN,
            details: 360.0,
        }
    );
    assert_eq!(
        compute_columns(1_299.0, 300.0, 360.0),
        Columns {
            sidebar: 300.0,
            center: CENTER_MIN,
            details: 359.0,
        }
    );
    assert_eq!(
        compute_columns(1_210.0, SIDEBAR_DEFAULT, DETAILS_DEFAULT),
        Columns {
            sidebar: 280.0,
            center: 930.0,
            details: 0.0,
        }
    );
    assert_eq!(
        compute_columns(700.0, SIDEBAR_DEFAULT, 0.0),
        Columns {
            sidebar: SIDEBAR_DEFAULT,
            center: 420.0,
            details: 0.0,
        }
    );
    assert_eq!(
        compute_columns(
            SIDEBAR_COLLAPSED + DETAILS_MIN + CENTER_MIN,
            0.0,
            DETAILS_DEFAULT
        ),
        Columns {
            sidebar: SIDEBAR_COLLAPSED,
            center: CENTER_MIN,
            details: DETAILS_MIN,
        }
    );
    assert_eq!(
        compute_columns(
            SIDEBAR_COLLAPSED + DETAILS_MIN + CENTER_MIN - 1.0,
            0.0,
            DETAILS_DEFAULT,
        ),
        Columns {
            sidebar: SIDEBAR_COLLAPSED,
            center: DETAILS_MIN + CENTER_MIN - 1.0,
            details: 0.0,
        }
    );
    assert_eq!(
        compute_columns(400.0, SIDEBAR_DEFAULT, DETAILS_DEFAULT),
        Columns {
            sidebar: SIDEBAR_DEFAULT,
            center: 120.0,
            details: 0.0,
        }
    );
    assert_eq!(
        compute_columns(500.0, 0.0, DETAILS_DEFAULT),
        Columns {
            sidebar: SIDEBAR_COLLAPSED,
            center: 500.0 - SIDEBAR_COLLAPSED,
            details: 0.0,
        }
    );
    assert_eq!(
        compute_columns(1_920.0, 9_999.0, 1.0),
        Columns {
            sidebar: SIDEBAR_MAX,
            center: 1_200.0,
            details: DETAILS_MIN,
        }
    );
    assert_eq!(
        compute_columns(1_920.0, SIDEBAR_DEFAULT, DETAILS_DEFAULT).details,
        DETAILS_DEFAULT
    );
    let invalid = compute_columns(f64::NAN, SIDEBAR_DEFAULT, DETAILS_DEFAULT);
    assert!(invalid.center.is_nan());
    let invalid = compute_columns(1_920.0, f64::NAN, DETAILS_DEFAULT);
    assert!(invalid.sidebar.is_nan());
    assert!(invalid.center.is_nan());
}

#[test]
fn layout_state_actions_are_transient_independent_and_exact() {
    let mut state = LayoutState::default();
    assert_eq!(
        state,
        LayoutState {
            sidebar: SIDEBAR_DEFAULT,
            details: 0.0,
            narrow: false,
            narrow_expanded: false,
        }
    );
    state.set_sidebar(9_999.0);
    state.set_details(1.0);
    assert_eq!(state.sidebar, SIDEBAR_MAX);
    assert_eq!(state.details, DETAILS_MIN);
    state.toggle_sidebar();
    assert_eq!(state.sidebar, 0.0);
    state.toggle_sidebar();
    assert_eq!(state.sidebar, SIDEBAR_DEFAULT);

    state.set_sidebar(400.0);
    state.set_narrow(true);
    state.toggle_sidebar();
    assert!(state.narrow_expanded);
    assert_eq!(state.sidebar, 400.0);
    state.set_narrow(true);
    assert!(state.narrow_expanded);
    state.set_narrow(false);
    assert!(!state.narrow_expanded);
    state.close_details();
    state.open_details();
    assert_eq!(state.details, DETAILS_DEFAULT);
    state.set_details(500.0);
    state.open_details();
    assert_eq!(state.details, 500.0);
    state.close_details();
    assert_eq!(state.details, 0.0);
    assert_eq!(LayoutState::default().sidebar, SIDEBAR_DEFAULT);
}

#[derive(Default)]
struct RecordingPanels {
    toggles: Cell<u32>,
    opens: Cell<u32>,
    closes: Cell<u32>,
}

impl PanelActionSink for RecordingPanels {
    fn toggle_sidebar(&self) {
        self.toggles.set(self.toggles.get() + 1);
    }

    fn open_details(&self) {
        self.opens.set(self.opens.get() + 1);
    }

    fn close_details(&self) {
        self.closes.set(self.closes.get() + 1);
    }
}

#[test]
fn controller_fails_loud_forwards_exactly_and_replaces_stale_actions() {
    let controller = LayoutController::new();
    for error in [
        controller.toggle_sidebar().unwrap_err(),
        controller.open_details().unwrap_err(),
        controller.close_details().unwrap_err(),
    ] {
        assert_eq!(
            error.to_string(),
            "layout: panel actions not wired (root entry not mounted)"
        );
    }

    let stale = Rc::new(RecordingPanels::default());
    let fresh = Rc::new(RecordingPanels::default());
    controller.attach_panels(stale.clone());
    controller.attach_panels(fresh.clone());
    controller.toggle_sidebar().unwrap();
    controller.open_details().unwrap();
    controller.close_details().unwrap();
    assert_eq!(stale.toggles.get(), 0);
    assert_eq!(
        (fresh.toggles.get(), fresh.opens.get(), fresh.closes.get()),
        (1, 1, 1)
    );
}

#[test]
fn theme_token_ledger_retracts_only_the_presenters_previous_set() {
    let mut ledger = ThemeTokenLedger::default();
    assert!(
        ledger
            .replace(vec!["--one".into(), "--two".into()])
            .is_empty()
    );
    assert_eq!(ledger.applied(), ["--one", "--two"]);
    assert_eq!(ledger.replace(vec!["--one".into()]), ["--one", "--two"]);
    assert_eq!(ledger.drain(), ["--one"]);
    assert!(ledger.applied().is_empty());
}

#[test]
fn stylesheet_and_public_constants_preserve_the_shell_contract() {
    assert_eq!(SIDEBAR_AUTO_COLLAPSE, 1_024.0);
    assert_eq!(INJECT, ["slots", "theme"]);
    assert_eq!(DARK_ATTRIBUTE, "data-ds-dark-theme");
    assert_eq!(INVARIANT_NAME, "client-ui-layout-invariant");
    for expected in [
        "grid-template-rows: 100%",
        "transition: grid-template-columns var(--ds-transition-duration-slow)",
        ".seekdeep-layout-frame[data-dragging]",
        "width: 8px",
        "margin-left: -4px",
        "touch-action: none",
        "pointer-events: none",
        ".seekdeep-layout-overlay-layer > *",
    ] {
        assert!(LAYOUT_STYLES.contains(expected), "{expected:?}");
    }
}

#[tokio::test]
async fn host_half_is_dependency_free_and_behaviorless() {
    let plugin = host_plugin();
    assert!(plugin.inject().is_empty());
    let context = Context::new();
    let fiber = context.plugin(plugin, Value::Null).unwrap();
    fiber.await_settled().await.unwrap();
    assert_eq!(fiber.fiber().state(), FiberState::Active);
    fiber.dispose().await.unwrap();
}
