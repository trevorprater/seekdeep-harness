//! Pure translation from untrusted LSP wire values into the semantic seam.

use seekdeep_lsp::{
    LSP_MALFORMED_RESPONSE, LspError, LspHover, LspLocation, LspOperation, LspPosition, LspRange,
};
use serde_json::{Map, Value};

use crate::{
    WireProviderCapability, WireServerCapabilities, WireTextDocumentSync, WireTextDocumentSyncKind,
};

/// Returns the `textDocument/*` request method for one closed operation.
#[must_use]
pub const fn request_method(operation: LspOperation) -> &'static str {
    match operation {
        LspOperation::GoToDefinition => "textDocument/definition",
        LspOperation::FindReferences => "textDocument/references",
        LspOperation::GoToImplementation => "textDocument/implementation",
        LspOperation::Hover => "textDocument/hover",
    }
}

/// Whether the server advertises one semantic operation.
#[must_use]
pub fn supports_operation(capabilities: &WireServerCapabilities, operation: LspOperation) -> bool {
    let capability = match operation {
        LspOperation::GoToDefinition => capabilities.definition_provider.as_ref(),
        LspOperation::FindReferences => capabilities.references_provider.as_ref(),
        LspOperation::GoToImplementation => capabilities.implementation_provider.as_ref(),
        LspOperation::Hover => capabilities.hover_provider.as_ref(),
    };
    match capability {
        Some(WireProviderCapability::Bool(value)) => *value,
        Some(WireProviderCapability::Options(_)) => true,
        None => false,
    }
}

/// Whether `textDocumentSync` permits transient didOpen/didClose use.
#[must_use]
pub fn supports_transient_open(sync: Option<&WireTextDocumentSync>) -> bool {
    match sync {
        Some(WireTextDocumentSync::Kind(
            WireTextDocumentSyncKind::Full | WireTextDocumentSyncKind::Incremental,
        )) => true,
        Some(WireTextDocumentSync::Options(options)) => options.open_close == Some(true),
        Some(WireTextDocumentSync::Kind(WireTextDocumentSyncKind::None)) | None => false,
    }
}

/// Normalizes an omitted or UTF-16 position encoding.
///
/// # Errors
///
/// Rejects every explicitly negotiated non-UTF-16 encoding.
pub fn negotiate_position_encoding(encoding: Option<&str>) -> anyhow::Result<&'static str> {
    match encoding {
        None | Some("utf-16") => Ok("utf-16"),
        Some(encoding) => anyhow::bail!(
            "server negotiated unsupported position encoding \"{encoding}\"; this host requires utf-16"
        ),
    }
}

/// Normalizes a navigation payload into semantic locations.
///
/// `None` represents a missing result (`undefined`); JSON null is the protocol
/// no-result value and becomes an empty list.
///
/// # Errors
///
/// Returns `LSP_MALFORMED_RESPONSE` for every invalid entry or coordinate.
pub fn normalize_locations(payload: Option<&Value>) -> anyhow::Result<Vec<LspLocation>> {
    let Some(payload) = payload else {
        return Err(malformed("LSP navigation result was missing"));
    };
    if payload.is_null() {
        return Ok(Vec::new());
    }
    let elements = payload
        .as_array()
        .map_or_else(|| vec![payload], |values| values.iter().collect());
    let mut locations = Vec::with_capacity(elements.len());
    for element in elements {
        let Some(record) = element.as_object() else {
            return Err(malformed(
                "LSP navigation result contained a non-object entry",
            ));
        };
        if let Some(location) = location_link(record).or_else(|| location(record)) {
            locations.push(location);
        } else {
            return Err(malformed(
                "LSP navigation result contained neither a Location nor a LocationLink",
            ));
        }
    }
    Ok(locations)
}

fn location_link(record: &Map<String, Value>) -> Option<LspLocation> {
    Some(LspLocation {
        uri: record.get("targetUri")?.as_str()?.to_owned(),
        range: parse_range(record.get("targetSelectionRange")?)?,
    })
}

fn location(record: &Map<String, Value>) -> Option<LspLocation> {
    Some(LspLocation {
        uri: record.get("uri")?.as_str()?.to_owned(),
        range: parse_range(record.get("range")?)?,
    })
}

fn parse_range(value: &Value) -> Option<LspRange> {
    let range = value.as_object()?;
    Some(LspRange {
        start: parse_position(range.get("start")?)?,
        end: parse_position(range.get("end")?)?,
    })
}

fn parse_position(value: &Value) -> Option<LspPosition> {
    let position = value.as_object()?;
    Some(LspPosition {
        line: protocol_coordinate(position.get("line")?)?,
        character: protocol_coordinate(position.get("character")?)?,
    })
}

fn protocol_coordinate(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .filter(|value| value.is_finite() && *value >= 0.0 && value.fract() == 0.0)
}

/// Normalizes a Hover payload into semantic content.
///
/// # Errors
///
/// Returns `LSP_MALFORMED_RESPONSE` for a missing or invalid hover shape.
pub fn normalize_hover(payload: Option<&Value>) -> anyhow::Result<Option<LspHover>> {
    let Some(payload) = payload else {
        return Err(malformed("LSP hover result was missing"));
    };
    if payload.is_null() {
        return Ok(None);
    }
    let Some(hover) = payload.as_object() else {
        return Err(malformed("LSP hover result was not an object"));
    };
    let contents = render_hover_contents(hover.get("contents"))?;
    if contents.is_empty() {
        return Ok(None);
    }
    let range = match hover.get("range") {
        Some(value) => Some(
            parse_range(value)
                .ok_or_else(|| malformed("LSP hover result contained a malformed range"))?,
        ),
        None => None,
    };
    Ok(Some(LspHover { contents, range }))
}

fn render_hover_contents(contents: Option<&Value>) -> anyhow::Result<String> {
    let Some(contents) = contents.filter(|contents| !contents.is_null()) else {
        return Err(malformed("LSP hover result had no contents"));
    };
    if let Some(contents) = contents.as_str() {
        return Ok(contents.to_owned());
    }
    if let Some(values) = contents.as_array() {
        return values
            .iter()
            .map(|value| {
                render_marked_string(value).ok_or_else(|| {
                    malformed("LSP hover contents contained a malformed MarkedString")
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()
            .map(|values| values.join("\n\n"));
    }
    let Some(record) = contents.as_object() else {
        return Err(malformed(
            "LSP hover contents were not MarkupContent, MarkedString, or an array",
        ));
    };
    if matches!(
        record.get("kind").and_then(Value::as_str),
        Some("markdown" | "plaintext")
    ) {
        return record
            .get("value")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| malformed("LSP hover MarkupContent value was not a string"));
    }
    render_marked_string(contents).ok_or_else(|| {
        malformed("LSP hover contents were not MarkupContent, MarkedString, or an array")
    })
}

fn render_marked_string(value: &Value) -> Option<String> {
    if let Some(value) = value.as_str() {
        return Some(value.to_owned());
    }
    let record = value.as_object()?;
    Some(format!(
        "```{}\n{}\n```",
        record.get("language")?.as_str()?,
        record.get("value")?.as_str()?
    ))
}

fn malformed(message: &str) -> anyhow::Error {
    LspError::new(message, LSP_MALFORMED_RESPONSE).into()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const RANGE: &str = r#"{"start":{"line":1,"character":2},"end":{"line":1,"character":5}}"#;

    fn range() -> Value {
        serde_json::from_str(RANGE).unwrap()
    }

    fn code(error: &anyhow::Error) -> &'static str {
        error.downcast_ref::<LspError>().unwrap().code()
    }

    #[test]
    fn operation_capability_sync_and_encoding_rules_are_exact() {
        assert_eq!(
            request_method(LspOperation::GoToDefinition),
            "textDocument/definition"
        );
        assert_eq!(
            request_method(LspOperation::FindReferences),
            "textDocument/references"
        );
        assert_eq!(
            request_method(LspOperation::GoToImplementation),
            "textDocument/implementation"
        );
        assert_eq!(request_method(LspOperation::Hover), "textDocument/hover");
        let capabilities = serde_json::from_value::<WireServerCapabilities>(json!({
            "definitionProvider": true,
            "referencesProvider": {"workDoneProgress": true},
            "implementationProvider": false,
        }))
        .unwrap();
        assert!(supports_operation(
            &capabilities,
            LspOperation::GoToDefinition
        ));
        assert!(supports_operation(
            &capabilities,
            LspOperation::FindReferences
        ));
        assert!(!supports_operation(
            &capabilities,
            LspOperation::GoToImplementation
        ));
        assert!(!supports_operation(&capabilities, LspOperation::Hover));
        for kind in [
            WireTextDocumentSyncKind::Full,
            WireTextDocumentSyncKind::Incremental,
        ] {
            assert!(supports_transient_open(Some(&WireTextDocumentSync::Kind(
                kind
            ))));
        }
        assert!(!supports_transient_open(Some(&WireTextDocumentSync::Kind(
            WireTextDocumentSyncKind::None
        ))));
        assert!(!supports_transient_open(None));
        assert!(supports_transient_open(Some(
            &WireTextDocumentSync::Options(crate::WireTextDocumentSyncOptions {
                open_close: Some(true),
                change: None
            })
        )));
        assert!(!supports_transient_open(Some(
            &WireTextDocumentSync::Options(crate::WireTextDocumentSyncOptions {
                open_close: None,
                change: Some(WireTextDocumentSyncKind::Incremental)
            })
        )));
        assert_eq!(negotiate_position_encoding(None).unwrap(), "utf-16");
        assert_eq!(
            negotiate_position_encoding(Some("utf-16")).unwrap(),
            "utf-16"
        );
        assert!(negotiate_position_encoding(Some("utf-8")).is_err());
    }

    #[test]
    fn navigation_normalizes_locations_links_and_rejects_every_bad_shape() {
        assert!(normalize_locations(Some(&Value::Null)).unwrap().is_empty());
        assert_eq!(
            code(&normalize_locations(None).unwrap_err()),
            LSP_MALFORMED_RESPONSE
        );
        let location_range = range();
        let single =
            normalize_locations(Some(&json!({"uri": "file:///a", "range": location_range})))
                .unwrap();
        assert_eq!(single[0].uri, "file:///a");
        let linked = normalize_locations(Some(&json!([{
            "targetUri": "file:///c", "targetSelectionRange": range(), "targetRange": range()
        }])))
        .unwrap();
        assert_eq!(linked[0].uri, "file:///c");
        for invalid in [
            json!([42]),
            json!([{"nope": true}]),
            json!([{"uri": "file:///a", "range": "nope"}]),
            json!([{"uri": "file:///a", "range": {"start": null, "end": null}}]),
            json!([{"uri": "file:///a", "range": {"start": {"line": -1, "character": 0}, "end": range()}}]),
            json!([{"uri": "file:///a", "range": {"start": {"line": 1, "character": 0}, "end": {"line": 1.5, "character": 5}}}]),
        ] {
            assert_eq!(
                code(&normalize_locations(Some(&invalid)).unwrap_err()),
                LSP_MALFORMED_RESPONSE
            );
        }
    }

    #[test]
    fn hover_normalizes_every_encoding_and_rejects_malformed_content() {
        assert_eq!(normalize_hover(Some(&Value::Null)).unwrap(), None);
        assert_eq!(
            code(&normalize_hover(None).unwrap_err()),
            LSP_MALFORMED_RESPONSE
        );
        assert_eq!(
            normalize_hover(Some(
                &json!({"contents": {"kind": "markdown", "value": "# H"}, "range": range()})
            ))
            .unwrap()
            .unwrap()
            .contents,
            "# H"
        );
        assert_eq!(
            normalize_hover(Some(&json!({"contents": "plain text"})))
                .unwrap()
                .unwrap()
                .contents,
            "plain text"
        );
        assert_eq!(
            normalize_hover(Some(
                &json!({"contents": {"language": "ts", "value": "const x = 1"}})
            ))
            .unwrap()
            .unwrap()
            .contents,
            "```ts\nconst x = 1\n```"
        );
        assert_eq!(
            normalize_hover(Some(
                &json!({"contents": ["a", {"language": "ts", "value": "b"}]})
            ))
            .unwrap()
            .unwrap()
            .contents,
            "a\n\n```ts\nb\n```"
        );
        assert_eq!(
            normalize_hover(Some(
                &json!({"contents": {"kind": "plaintext", "value": ""}})
            ))
            .unwrap(),
            None
        );
        for invalid in [
            json!(42),
            json!({"contents": {"kind": "markdown", "value": 42}}),
            json!({"contents": {"weird": true}}),
            json!({"contents": 42}),
            json!({"contents": ["ok", {"language": "ts", "value": 42}]}),
            json!({"contents": [null]}),
            json!({"range": range()}),
            json!({"contents": "x", "range": {"start": {"line": 1}}}),
        ] {
            assert_eq!(
                code(&normalize_hover(Some(&invalid)).unwrap_err()),
                LSP_MALFORMED_RESPONSE
            );
        }
    }
}
