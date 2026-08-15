//! Static public product attribution for provider requests.

use std::collections::BTreeMap;

/// Public application identity carried in `User-Agent`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppIdentity {
    /// Lowercase hyphenated product token.
    pub product: &'static str,
    /// Published product version.
    pub version: &'static str,
    /// Public repository URL.
    pub url: &'static str,
}

/// Seekdeep's default static attribution identity.
pub const APP_IDENTITY: AppIdentity = AppIdentity {
    product: "seekdeep",
    version: env!("CARGO_PKG_VERSION"),
    url: "https://github.com/deepseek-ai/seekdeep-harness",
};

/// Renders the standard `product/version (+url)` value.
#[must_use]
pub fn user_agent(identity: &AppIdentity) -> String {
    format!(
        "{}/{} (+{})",
        identity.product, identity.version, identity.url
    )
}

/// Builds the mandatory provider attribution headers.
#[must_use]
pub fn attribution_headers(identity: &AppIdentity) -> BTreeMap<String, String> {
    BTreeMap::from([("user-agent".to_owned(), user_agent(identity))])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_identity_comes_from_package_metadata() {
        assert_eq!(APP_IDENTITY.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(attribution_headers(&APP_IDENTITY).len(), 1);
        assert_eq!(
            user_agent(&APP_IDENTITY),
            format!("seekdeep/{} (+{})", APP_IDENTITY.version, APP_IDENTITY.url)
        );
    }
}
