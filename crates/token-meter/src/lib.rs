//! Replay-aware request-pressure measurement and bounded session projections.

/// Pure context-composition projection.
pub mod breakdown_projection;
/// Browser-facing projection-only exports.
pub mod client;
/// Fixed-density token estimator.
pub mod estimate;
/// Package-owned invariant registration.
pub mod invariant;
/// Client-safe projection values.
pub mod projection;
/// Replay-owned measurement service and plugin.
pub mod runtime;
/// Positional priced-surface fold.
pub mod surface_fold;
/// Bounded shadow-price surface fold.
pub mod surface_projection;
/// Public service configuration and measurement values.
pub mod types;
/// Provider-usage and context-pressure projections.
pub mod usage_projection;

pub use estimate::{
    ROLE_OVERHEAD, estimate_content, estimate_header, estimate_message, estimate_system_tokens,
    estimate_tools_tokens,
};
pub use projection::{ContextBreakdownProjection, ContextPressureProjection, TokenUsageProjection};
pub use runtime::{NAME, TOKEN_METER, TokenMeter, TokenMeterInstallation, install, plugin};
pub use types::{TokenMeasurement, TokenMeasurementBaseline, TokenMeterConfig, TokenSurfaceNode};
