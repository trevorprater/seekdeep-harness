//! One test binary per crate: every top-level test file is a module here.

mod context_service_boundary_audit;
mod fiber_ownership_boundary_audit;
mod lifecycle_deferral;
mod logger_oracle;
mod logger_utils_priority_oracle;
mod native_failure_identity;
mod plugin_lifecycle_events;
mod reflect_oracle;
mod registry_oracle;
mod service_change_watch;
mod source_semantic_oracle;
mod test_invariant_paths_parity;
mod utils_oracle;
