//! Strict incremental Server-Sent Events decoding for DeepSeek streams.

use std::{pin::Pin, sync::Arc};

use bytes::Bytes;
use futures::{Stream, StreamExt};
use seekdeep_llm::LlmError;

/// Terminal DeepSeek/OpenAI data payload.
pub const DONE: &str = "[DONE]";

/// Fallible byte-stream accepted by the SSE decoder.
pub type ByteStream = Pin<Box<dyn Stream<Item = anyhow::Result<Bytes>> + Send + 'static>>;
/// Fallible payload stream emitted by the SSE decoder.
pub type PayloadStream = Pin<Box<dyn Stream<Item = anyhow::Result<String>> + Send + 'static>>;
/// Out-of-band comment callback used as transport activity.
pub type CommentObserver = Arc<dyn Fn(&str) + Send + Sync + 'static>;

/// Parses strict SSE frames and yields data payloads through `[DONE]`.
///
/// A blank-line terminator is required. EOF before a terminated `[DONE]`
/// event is `STREAM_CLOSED`; comments are observed but never yielded.
#[must_use]
pub fn parse_sse(mut input: ByteStream, on_comment: Option<CommentObserver>) -> PayloadStream {
    Box::pin(async_stream::try_stream! {
        let mut decoder = Utf8Decoder::default();
        let mut lines = String::new();
        let mut data = Vec::<String>::new();
        let mut first_text = true;
        while let Some(item) = input.next().await {
            let bytes = item?;
            let mut text = decoder.push(&bytes);
            if first_text && !text.is_empty() {
                first_text = false;
                if let Some(stripped) = text.strip_prefix('\u{feff}') {
                    text = stripped.to_owned();
                }
            }
            lines.push_str(&text);
            for payload in drain_lines(&mut lines, &mut data, on_comment.as_ref()) {
                let done = payload == DONE;
                yield payload;
                if done {
                    return;
                }
            }
        }
        let mut text = decoder.finish();
        if first_text
            && !text.is_empty()
            && let Some(stripped) = text.strip_prefix('\u{feff}')
        {
            text = stripped.to_owned();
        }
        lines.push_str(&text);
        if lines.ends_with('\r') {
            lines.push('\n');
        }
        for payload in drain_lines(&mut lines, &mut data, on_comment.as_ref()) {
            let done = payload == DONE;
            yield payload;
            if done {
                return;
            }
        }
        Err::<(), anyhow::Error>(LlmError::simple(
            "SSE stream ended without [DONE]",
            "STREAM_CLOSED",
        ).into())?;
    })
}

fn drain_lines(
    text: &mut String,
    data: &mut Vec<String>,
    on_comment: Option<&CommentObserver>,
) -> Vec<String> {
    let mut payloads = Vec::new();
    while let Some((line, consumed)) = take_line(text) {
        text.drain(..consumed);
        if line.is_empty() {
            if !data.is_empty() {
                payloads.push(data.join("\n"));
                data.clear();
            }
            continue;
        }
        if let Some(comment) = line.strip_prefix(':') {
            if let Some(observer) = on_comment {
                observer(comment.strip_prefix(' ').unwrap_or(comment));
            }
            continue;
        }
        let (field, value) = line
            .split_once(':')
            .map_or((line.as_str(), ""), |(field, value)| {
                (field, value.strip_prefix(' ').unwrap_or(value))
            });
        if field == "data" {
            data.push(value.to_owned());
        }
    }
    payloads
}

fn take_line(text: &str) -> Option<(String, usize)> {
    for (index, byte) in text.bytes().enumerate() {
        match byte {
            b'\n' => return Some((text[..index].to_owned(), index + 1)),
            b'\r' => {
                if index + 1 == text.len() {
                    return None;
                }
                let consumed = index + 1 + usize::from(text.as_bytes()[index + 1] == b'\n');
                return Some((text[..index].to_owned(), consumed));
            }
            _ => {}
        }
    }
    None
}

#[derive(Default)]
struct Utf8Decoder {
    pending: Vec<u8>,
}

impl Utf8Decoder {
    fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        self.decode(false)
    }

    fn finish(&mut self) -> String {
        self.decode(true)
    }

    fn decode(&mut self, eof: bool) -> String {
        let mut output = String::new();
        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(valid) => {
                    output.push_str(valid);
                    self.pending.clear();
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    if valid > 0 {
                        output.push_str(
                            std::str::from_utf8(&self.pending[..valid])
                                .expect("UTF-8 validator supplied a valid prefix"),
                        );
                        self.pending.drain(..valid);
                    }
                    match error.error_len() {
                        Some(length) => {
                            output.push('\u{fffd}');
                            self.pending.drain(..length);
                        }
                        None if eof => {
                            output.push('\u{fffd}');
                            self.pending.clear();
                            break;
                        }
                        None => break,
                    }
                }
            }
        }
        output
    }
}
