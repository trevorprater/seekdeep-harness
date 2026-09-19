//! URL validation and content-type classification for the local HTTP(S) fetch provider.

use encoding_rs::Encoding;
use seekdeep_web::web_error;
use url::Url;

/// The body kinds this provider decodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FetchableKind {
    /// An HTML document.
    Html,
    /// Plain or structured text.
    Text,
}

/// Validates a request URL against the basic transport hygiene the provider enforces before any
/// network access: http(s) only, no embedded credentials, bounded length.
///
/// # Errors
///
/// Returns a `WEB_INVALID_URL` or `WEB_BLOCKED_URL` error on rejection.
pub fn validate_fetch_url(input: &str, max_url_length: f64) -> anyhow::Result<Url> {
    if crate::numeric::exceeds(input.encode_utf16().count(), max_url_length) {
        anyhow::bail!(web_error(
            format!("URL exceeds the maximum length of {max_url_length}"),
            "WEB_INVALID_URL"
        ));
    }
    let url = Url::parse(input)
        .map_err(|_| web_error(format!("invalid URL: {input}"), "WEB_INVALID_URL"))?;
    if url.scheme() != "http" && url.scheme() != "https" {
        anyhow::bail!(web_error(
            format!(
                "unsupported URL scheme \"{}:\" (only http and https are allowed)",
                url.scheme()
            ),
            "WEB_INVALID_URL"
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        anyhow::bail!(web_error(
            "credentials in URLs are not allowed",
            "WEB_BLOCKED_URL"
        ));
    }
    Ok(url)
}

/// Two URLs are same-origin when scheme, hostname, and port match.
#[must_use]
pub fn is_same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
}

/// Classifies a response Content-Type into a decodable body kind, or None for an unsupported
/// (e.g. binary) type.
#[must_use]
pub fn classify_content_type(content_type: Option<&str>) -> Option<FetchableKind> {
    let mime = content_type
        .unwrap_or_default()
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if mime == "text/html" || mime == "application/xhtml+xml" {
        return Some(FetchableKind::Html);
    }
    if mime.starts_with("text/") {
        return Some(FetchableKind::Text);
    }
    if mime == "application/json"
        || mime == "application/xml"
        || mime.ends_with("+json")
        || mime.ends_with("+xml")
    {
        return Some(FetchableKind::Text);
    }
    None
}

/// Extracts the charset parameter from a response Content-Type, lower-cased.
///
/// Mirrors the source's `/;\s*charset\s*=\s*"?([^";]+)"?/i`: only a parameter whose whole
/// name is `charset` counts, so `foocharset=` is not a charset, and the first such parameter
/// wins.
#[must_use]
pub fn parse_charset(content_type: Option<&str>) -> Option<String> {
    let lower = content_type.unwrap_or_default().to_ascii_lowercase();
    let mut rest = lower.as_str();
    while let Some(separator) = rest.find(';') {
        rest = &rest[separator + 1..];
        let parameter = rest.trim_start();
        let Some(after_name) = parameter.strip_prefix("charset") else {
            continue;
        };
        let Some(after_equals) = after_name.trim_start().strip_prefix('=') else {
            continue;
        };
        let value = after_equals.trim_start();
        let value = value.strip_prefix('"').unwrap_or(value);
        let value = value.split(['"', ';']).next().unwrap_or_default();
        if !value.is_empty() {
            return Some(value.to_owned());
        }
    }
    None
}

/// Resolves a TextDecoder-equivalent encoding for a declared charset, defaulting to UTF-8.
///
/// # Errors
///
/// Returns a `WEB_UNSUPPORTED_CONTENT_TYPE` error when the label is present but unrecognized.
pub fn decoder_for_charset(charset: Option<&str>) -> anyhow::Result<&'static Encoding> {
    let Some(charset) = charset else {
        return Ok(encoding_rs::UTF_8);
    };
    Encoding::for_label(charset.as_bytes()).ok_or_else(|| {
        web_error(
            format!("unsupported charset \"{charset}\""),
            "WEB_UNSUPPORTED_CONTENT_TYPE",
        )
        .into()
    })
}
