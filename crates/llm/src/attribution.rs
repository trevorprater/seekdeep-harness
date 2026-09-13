//! Static public product attribution for provider requests.

use std::collections::BTreeMap;

/// Public application identity carried in `User-Agent`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppIdentity<'a> {
    /// Lowercase hyphenated product token.
    pub product: &'a str,
    /// Published product version.
    pub version: &'a str,
    /// Public repository URL.
    pub url: &'a str,
}

/// `SeekDeep Harness`'s default static attribution identity.
pub const APP_IDENTITY: AppIdentity<'static> = AppIdentity {
    product: "seekdeep-harness",
    version: env!("CARGO_PKG_VERSION"),
    url: "https://github.com/deepseek-ai/seekdeep-harness",
};

/// Renders the standard `product/version (+url)` value.
#[must_use]
pub fn user_agent() -> String {
    user_agent_for(&APP_IDENTITY)
}

/// Renders a white-label `product/version (+url)` value.
#[must_use]
pub fn user_agent_for(identity: &AppIdentity<'_>) -> String {
    format!(
        "{}/{} (+{})",
        identity.product, identity.version, identity.url
    )
}

/// Builds the mandatory provider attribution headers.
#[must_use]
pub fn attribution_headers() -> BTreeMap<String, String> {
    attribution_headers_for(&APP_IDENTITY)
}

/// Builds mandatory provider attribution headers for a white-label identity.
#[must_use]
pub fn attribution_headers_for(identity: &AppIdentity<'_>) -> BTreeMap<String, String> {
    BTreeMap::from([("user-agent".to_owned(), user_agent_for(identity))])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_identity_comes_from_package_metadata() {
        assert_eq!(APP_IDENTITY.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(attribution_headers().len(), 1);
        assert_eq!(
            user_agent(),
            format!(
                "seekdeep-harness/{} (+{})",
                APP_IDENTITY.version, APP_IDENTITY.url
            )
        );
    }
}
