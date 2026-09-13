//! Pure concession-chain solver for the three-column shell.

/// Center-column floor; only the final fallback may go below it.
pub const CENTER_MIN: f64 = 640.0;
/// Sidebar drag clamp floor.
pub const SIDEBAR_MIN: f64 = 264.0;
/// Sidebar drag clamp ceiling.
pub const SIDEBAR_MAX: f64 = 420.0;
/// Sidebar width before any user drag.
pub const SIDEBAR_DEFAULT: f64 = 280.0;
/// Closed-sidebar compact rail.
pub const SIDEBAR_COLLAPSED: f64 = 56.0;
/// Viewport width below which the sidebar auto-collapses.
pub const SIDEBAR_AUTO_COLLAPSE: f64 = 1_024.0;
/// Details drag clamp floor.
pub const DETAILS_MIN: f64 = 300.0;
/// Details drag clamp ceiling.
pub const DETAILS_MAX: f64 = 520.0;
/// Details width before any user drag.
pub const DETAILS_DEFAULT: f64 = 360.0;

/// Resolved widths for one frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Columns {
    /// Rendered sidebar width.
    pub sidebar: f64,
    /// Rendered center width.
    pub center: f64,
    /// Rendered details width; zero is visually closed but still mounted.
    pub details: f64,
}

/// Clamps and JavaScript-rounds a width into its contract range.
#[must_use]
pub fn clamp_width(px: f64, min: f64, max: f64) -> f64 {
    if px.is_nan() || min.is_nan() || max.is_nan() {
        return f64::NAN;
    }
    let rounded = (px + 0.5).floor();
    max.min(min.max(rounded))
}

/// Resolves the exact sidebar, center, and details concession chain.
#[must_use]
pub fn compute_columns(viewport: f64, sidebar: f64, details: f64) -> Columns {
    let sidebar = if sidebar == 0.0 {
        SIDEBAR_COLLAPSED
    } else {
        clamp_width(sidebar, SIDEBAR_MIN, SIDEBAR_MAX)
    };
    let preferred_details = if details == 0.0 {
        0.0
    } else {
        clamp_width(details, DETAILS_MIN, DETAILS_MAX)
    };

    if sidebar + preferred_details + CENTER_MIN <= viewport {
        return Columns {
            sidebar,
            center: viewport - sidebar - preferred_details,
            details: preferred_details,
        };
    }

    let conceded_details = if preferred_details == 0.0 {
        0.0
    } else {
        js_max(DETAILS_MIN, viewport - sidebar - CENTER_MIN)
    };
    if sidebar + conceded_details + CENTER_MIN <= viewport {
        return Columns {
            sidebar,
            center: CENTER_MIN,
            details: conceded_details,
        };
    }

    Columns {
        sidebar,
        center: js_max(0.0, viewport - sidebar),
        details: 0.0,
    }
}

fn js_max(left: f64, right: f64) -> f64 {
    if left.is_nan() || right.is_nan() {
        f64::NAN
    } else {
        left.max(right)
    }
}
