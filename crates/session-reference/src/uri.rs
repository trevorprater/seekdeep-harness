//! Canonical session URI and inline mention encoding.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use regress::Regex;
use seekdeep_core::session::SessionId;
use std::sync::LazyLock;

use crate::config::{SessionReferenceError, SessionReferenceErrorCode};
use crate::types::SessionReferenceInput;

/// URI scheme reserved for `SeekDeep` Harness session snapshots.
pub const SESSION_REFERENCE_SCHEME: &str = "dsh-session:";

/// Encodes any session-id string as a canonical lossless URI.
///
/// # Panics
///
/// Panics if the session id cannot be JSON-serialized, which cannot happen.
#[must_use]
pub fn encode_session_reference_uri(session_id: &SessionId) -> String {
    let json = serde_json::to_string(session_id).expect("session id serializes");
    let payload = URL_SAFE_NO_PAD.encode(json.as_bytes());
    format!("{SESSION_REFERENCE_SCHEME}{payload}")
}

/// Decodes and canonicalizes one session-reference URI.
///
/// # Errors
///
/// Returns an invalid-reference failure for a non-canonical or malformed URI.
pub fn decode_session_reference_uri(uri: &str) -> Result<SessionId, SessionReferenceError> {
    let Some(payload) = uri.strip_prefix(SESSION_REFERENCE_SCHEME) else {
        return Err(invalid_uri(uri));
    };
    if payload.is_empty()
        || !payload
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(invalid_uri(uri));
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| invalid_uri(uri))?;
    let text = std::str::from_utf8(&decoded).map_err(|_| invalid_uri(uri))?;
    let parsed: serde_json::Value = serde_json::from_str(text).map_err(|_| invalid_uri(uri))?;
    let Some(raw) = parsed.as_str() else {
        return Err(invalid_uri(uri));
    };
    let session_id = SessionId::new(raw.to_owned());
    if encode_session_reference_uri(&session_id) != uri {
        return Err(invalid_uri(uri));
    }
    Ok(session_id)
}

/// Renders a host-neutral Markdown mention carrying the canonical URI.
#[must_use]
pub fn format_session_reference_mention(reference: &SessionReferenceInput) -> String {
    let label = reference
        .label
        .as_deref()
        .unwrap_or_else(|| reference.session_id.as_str());
    let label = escape_label(label);
    let uri = encode_session_reference_uri(&reference.session_id);
    format!("@[{label}]({uri})")
}

/// Result of extracting canonical mentions from plain text.
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedSessionReferenceText {
    /// Text with opaque tokens replaced by readable at-label spans.
    pub text: String,
    /// Structured references in first-appearance order, before service deduplication.
    pub references: Vec<SessionReferenceInput>,
}

/// Extracts Markdown mentions and bare canonical URIs from one text value.
///
/// # Errors
///
/// Returns an invalid-reference failure for any malformed or non-canonical URI.
pub fn parse_session_reference_text(
    text: &str,
) -> Result<ParsedSessionReferenceText, SessionReferenceError> {
    let regex = mention_regex();
    let mut references = Vec::new();
    let mut rendered = String::with_capacity(text.len());
    let mut last_end = 0;
    for matched in regex.find_iter(text) {
        rendered.push_str(&text[last_end..matched.start()]);
        let raw_label = matched.group(1).map(|range| &text[range]);
        let markdown_uri = matched.group(2).map(|range| &text[range]);
        let bare_uri = matched.group(3).map(|range| &text[range]);
        let uri = markdown_uri.or(bare_uri).ok_or_else(|| {
            SessionReferenceError::new(
                "session reference URI is missing",
                SessionReferenceErrorCode::SessionReferenceInvalidReference,
            )
        })?;
        let session_id = decode_session_reference_uri(uri)?;
        let label = raw_label.map_or_else(|| session_id.as_str().to_owned(), unescape_label);
        references.push(SessionReferenceInput {
            session_id,
            label: Some(label.clone()),
        });
        rendered.push('@');
        rendered.push_str(&label);
        last_end = matched.end();
    }
    rendered.push_str(&text[last_end..]);
    Ok(ParsedSessionReferenceText {
        text: rendered,
        references,
    })
}

fn mention_regex() -> &'static Regex {
    static REGEX: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"@\[((?:\\.|[^\\\]])*)\]\((dsh-session:[^\s)]*)\)|(dsh-session:[A-Za-z0-9_-]+)")
            .expect("session-reference mention regex")
    });
    &REGEX
}

fn escape_label(label: &str) -> String {
    let mut output = String::with_capacity(label.len());
    for character in label.chars() {
        if character == '\\' || character == ']' {
            output.push('\\');
        }
        output.push(character);
    }
    output
}

fn unescape_label(label: &str) -> String {
    let mut output = String::with_capacity(label.len());
    let mut characters = label.chars();
    while let Some(character) = characters.next() {
        if character == '\\' {
            if let Some(next) = characters.next() {
                output.push(next);
            } else {
                output.push(character);
            }
        } else {
            output.push(character);
        }
    }
    output
}

fn invalid_uri(uri: &str) -> SessionReferenceError {
    SessionReferenceError::new(
        format!(
            "invalid session reference URI {}",
            serde_json::to_string(uri).unwrap_or_else(|_| "?".to_owned())
        ),
        SessionReferenceErrorCode::SessionReferenceInvalidReference,
    )
}
