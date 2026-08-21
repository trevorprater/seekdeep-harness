//! LSP base-protocol `Content-Length` framing.

use serde_json::Value;

const HEADER_SEPARATOR: &[u8] = b"\r\n\r\n";

/// Maximum header bytes before a missing or late terminator is rejected.
pub const MAX_HEADER_BYTES: usize = 1 << 16;

/// Encodes one JSON-RPC value as an LSP framed message.
///
/// # Errors
///
/// Returns a JSON serialization failure.
pub fn encode_message(message: &Value) -> serde_json::Result<Vec<u8>> {
    let body = serde_json::to_vec(message)?;
    let mut framed = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    framed.extend_from_slice(&body);
    Ok(framed)
}

/// Streaming decoder for `Content-Length` framed JSON-RPC messages.
#[derive(Debug)]
pub struct MessageDecoder {
    buffer: Vec<u8>,
    max_message_bytes: usize,
}

impl MessageDecoder {
    /// Creates a decoder with one body-size bound.
    #[must_use]
    pub const fn new(max_message_bytes: usize) -> Self {
        Self {
            buffer: Vec::new(),
            max_message_bytes,
        }
    }

    /// Appends bytes and returns every complete parsed message in arrival order.
    ///
    /// # Errors
    ///
    /// Returns malformed-header, size-bound, or invalid-JSON failures.
    pub fn push(&mut self, chunk: &[u8]) -> anyhow::Result<Vec<Value>> {
        self.buffer.extend_from_slice(chunk);
        let mut messages = Vec::new();
        while let Some(message) = self.next()? {
            messages.push(message);
        }
        Ok(messages)
    }

    fn next(&mut self) -> anyhow::Result<Option<Value>> {
        let Some(separator) = find_subslice(&self.buffer, HEADER_SEPARATOR) else {
            anyhow::ensure!(
                self.buffer.len() <= MAX_HEADER_BYTES,
                "LSP header exceeded {MAX_HEADER_BYTES} bytes without a terminator"
            );
            return Ok(None);
        };
        anyhow::ensure!(
            separator <= MAX_HEADER_BYTES,
            "LSP header exceeded {MAX_HEADER_BYTES} bytes"
        );
        let header = ascii_decode(&self.buffer[..separator]);
        let content_length = parse_content_length(&header)?;
        anyhow::ensure!(
            content_length <= self.max_message_bytes,
            "LSP message length {content_length} exceeds the {}-byte limit",
            self.max_message_bytes
        );
        let body_start = separator + HEADER_SEPARATOR.len();
        let body_end = body_start
            .checked_add(content_length)
            .ok_or_else(|| anyhow::anyhow!("LSP message length overflow"))?;
        if self.buffer.len() < body_end {
            return Ok(None);
        }
        let body = String::from_utf8_lossy(&self.buffer[body_start..body_end]).into_owned();
        self.buffer.drain(..body_end);
        serde_json::from_str(&body)
            .map(Some)
            .map_err(|error| anyhow::anyhow!("LSP message body was not valid JSON: {error}"))
    }
}

fn parse_content_length(header: &str) -> anyhow::Result<usize> {
    for line in header.split("\r\n") {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if !name.trim().eq_ignore_ascii_case("content-length") {
            continue;
        }
        let number = value.trim().parse::<f64>().ok();
        let Some(number) =
            number.filter(|number| number.is_finite() && *number >= 0.0 && number.fract() == 0.0)
        else {
            anyhow::bail!("invalid Content-Length header: {}", json_string(line));
        };
        if number == 0.0 {
            return Ok(0);
        }
        return format!("{number:.0}")
            .parse::<usize>()
            .map_err(|_| anyhow::anyhow!("invalid Content-Length header: {}", json_string(line)));
    }
    anyhow::bail!(
        "LSP header block missing Content-Length: {}",
        json_string(header)
    )
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).expect("strings always serialize")
}

fn ascii_decode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| char::from(byte & 0x7f)).collect()
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn frame(body: &str) -> Vec<u8> {
        let mut framed = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        framed.extend_from_slice(body.as_bytes());
        framed
    }

    #[test]
    fn encoding_uses_utf8_byte_length() {
        let encoded =
            encode_message(&json!({"jsonrpc": "2.0", "method": "x", "params": {"s": "é"}}))
                .unwrap();
        assert_eq!(
            String::from_utf8(encoded).unwrap(),
            "Content-Length: 50\r\n\r\n{\"jsonrpc\":\"2.0\",\"method\":\"x\",\"params\":{\"s\":\"é\"}}"
        );
    }

    #[test]
    fn decoder_handles_split_coalesced_and_extra_headers() {
        let mut decoder = MessageDecoder::new(1_000);
        let mut coalesced = frame(r#"{"a":1}"#);
        coalesced.extend(frame(r#"{"b":2}"#));
        assert_eq!(
            decoder.push(&coalesced).unwrap(),
            [json!({"a": 1}), json!({"b": 2})]
        );

        let split = frame(r#"{"hello":"world"}"#);
        assert!(decoder.push(&split[..10]).unwrap().is_empty());
        assert_eq!(
            decoder.push(&split[10..]).unwrap(),
            [json!({"hello": "world"})]
        );

        let body = r#"{"ok":true}"#;
        let custom = format!(
            "content-length: {}\r\nContent-Type: x\r\n\r\n{body}",
            body.len()
        );
        assert_eq!(
            decoder.push(custom.as_bytes()).unwrap(),
            [json!({"ok": true})]
        );
    }

    #[test]
    fn decoder_rejects_every_malformed_or_oversized_boundary() {
        let mut small = MessageDecoder::new(4);
        assert!(
            small
                .push(&frame(r#"{"big":true}"#))
                .unwrap_err()
                .to_string()
                .contains("exceeds the 4-byte limit")
        );
        let mut missing = MessageDecoder::new(1_000);
        assert!(
            missing
                .push(b"X: 1\r\n\r\n{}")
                .unwrap_err()
                .to_string()
                .contains("missing Content-Length")
        );
        let mut invalid = MessageDecoder::new(1_000);
        assert!(
            invalid
                .push(b"Content-Length: abc\r\n\r\n{}")
                .unwrap_err()
                .to_string()
                .contains("invalid Content-Length")
        );
        let mut unterminated = MessageDecoder::new(1_000);
        assert!(
            unterminated
                .push(&vec![b'A'; MAX_HEADER_BYTES + 1])
                .unwrap_err()
                .to_string()
                .contains("without a terminator")
        );
        let mut oversized = MessageDecoder::new(1_000);
        let huge = format!(
            "Content-Length: 2\r\nX-Fill: {}\r\n\r\n{{}}",
            "a".repeat(70_000)
        );
        assert!(
            oversized
                .push(huge.as_bytes())
                .unwrap_err()
                .to_string()
                .contains("header exceeded")
        );
        let mut json = MessageDecoder::new(1_000);
        assert!(
            json.push(&frame("not json"))
                .unwrap_err()
                .to_string()
                .contains("not valid JSON")
        );
    }
}
