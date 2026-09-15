//! Browser-owned time-zone sampling for outbound operation provenance.

/// Current Client time-zone resolver.
pub trait ClientTimeZoneResolver {
    /// Returns the runtime-provided IANA zone, when readable.
    fn resolve(&self) -> Option<String>;
}

/// Missing or empty browser time-zone failure.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("browser time zone is unavailable")]
pub struct ClientTimeZoneUnavailable;

/// Resolves one current non-empty Client IANA zone.
///
/// # Errors
///
/// Returns [`ClientTimeZoneUnavailable`] when the runtime exposes no readable zone.
pub fn resolved_client_time_zone(
    resolver: &dyn ClientTimeZoneResolver,
) -> Result<String, ClientTimeZoneUnavailable> {
    resolver
        .resolve()
        .filter(|zone| !zone.is_empty())
        .ok_or(ClientTimeZoneUnavailable)
}
