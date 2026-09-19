//! Streaming terminal-control sanitizer for the line-oriented first release.

/// OSC marker emitted by the controlled shell before each prompt.
pub const PROMPT_MARKER_PREFIX: &str = "133;D;";
/// Exact printable prompt emitted after the private marker.
pub const CONTROLLED_PROMPT: &str = "seekdeep> ";

/// One sanitized chunk plus owned-prompt facts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SanitizedChunk {
    /// Printable line-normalized text.
    pub text: String,
    /// Whether an owned prompt marker completed.
    pub prompt: bool,
    /// Printable text after the latest owned marker when tracking is active.
    pub prompt_tail: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DiscardMode {
    Osc,
    Csi,
}

/// Stateful remover for CSI, OSC, and short escape sequences.
#[derive(Debug)]
pub struct TerminalSanitizer {
    max_pending_bytes: usize,
    pending: String,
    discard_mode: Option<DiscardMode>,
    discard_osc_escape: bool,
    trailing_carriage_return: bool,
    tracking_prompt_tail: bool,
}

impl TerminalSanitizer {
    /// Creates a sanitizer with a byte bound for incomplete control sequences.
    #[must_use]
    pub fn new(max_pending_bytes: usize) -> Self {
        Self {
            max_pending_bytes,
            pending: String::new(),
            discard_mode: None,
            discard_osc_escape: false,
            trailing_carriage_return: false,
            tracking_prompt_tail: false,
        }
    }

    /// Consumes one decoded terminal data chunk.
    pub fn push(&mut self, chunk: &str) -> SanitizedChunk {
        let prefix = self.discard_prefix(chunk);
        self.pending.push_str(&prefix);
        let mut text = String::new();
        let mut prompt = false;
        let mut include_prompt_tail = self.tracking_prompt_tail;
        let mut prompt_tail = String::new();
        let mut index = 0;
        while index < self.pending.len() {
            let Some(relative_escape) = self.pending[index..].find('\u{1b}') else {
                append_text(
                    &self.pending[index..],
                    &mut text,
                    self.tracking_prompt_tail,
                    &mut prompt_tail,
                );
                index = self.pending.len();
                break;
            };
            let escape = index + relative_escape;
            append_text(
                &self.pending[index..escape],
                &mut text,
                self.tracking_prompt_tail,
                &mut prompt_tail,
            );
            if escape + 1 >= self.pending.len() {
                index = escape;
                break;
            }
            let kind = self.pending.as_bytes()[escape + 1];
            if kind == b']' {
                let content_start = escape + 2;
                let bel = self.pending[content_start..]
                    .find('\u{7}')
                    .map(|offset| content_start + offset);
                let string_terminator = self.pending[content_start..]
                    .find("\u{1b}\\")
                    .map(|offset| content_start + offset);
                let (end, terminator_bytes) = match (bel, string_terminator) {
                    (Some(bel), Some(st)) if bel < st => (bel + 1, 1),
                    (Some(_) | None, Some(st)) => (st + 2, 2),
                    (Some(bel), None) => (bel + 1, 1),
                    (None, None) => {
                        index = escape;
                        break;
                    }
                };
                let content = &self.pending[content_start..end - terminator_bytes];
                if content.starts_with(PROMPT_MARKER_PREFIX) {
                    prompt = true;
                    self.tracking_prompt_tail = true;
                    include_prompt_tail = true;
                    prompt_tail.clear();
                }
                index = end;
                continue;
            }
            if kind == b'[' {
                let mut end = escape + 2;
                while end < self.pending.len() {
                    let code = self.pending.as_bytes()[end];
                    if (0x40..=0x7e).contains(&code) {
                        break;
                    }
                    end += 1;
                }
                if end >= self.pending.len() {
                    index = escape;
                    break;
                }
                index = end + 1;
                continue;
            }
            index = escape + 2;
        }
        self.pending.drain(..index);
        self.enforce_pending_bound();
        SanitizedChunk {
            text: self.normalize_text(&text),
            prompt,
            prompt_tail: include_prompt_tail.then_some(prompt_tail),
        }
    }

    /// Flushes trailing printable text and discards incomplete escapes.
    pub fn flush(&mut self) -> String {
        let text = if self.pending.starts_with('\u{1b}') {
            String::new()
        } else {
            std::mem::take(&mut self.pending)
        };
        self.pending.clear();
        self.discard_mode = None;
        self.discard_osc_escape = false;
        self.tracking_prompt_tail = false;
        let normalized = self.normalize_text(&text);
        if !self.trailing_carriage_return {
            return normalized;
        }
        self.trailing_carriage_return = false;
        format!("{normalized}\n")
    }

    fn normalize_text(&mut self, text: &str) -> String {
        let mut complete = if self.trailing_carriage_return {
            format!("\r{text}")
        } else {
            text.to_owned()
        };
        self.trailing_carriage_return = false;
        if complete.ends_with('\r') {
            complete.pop();
            self.trailing_carriage_return = true;
        }
        normalize_terminal_text(&complete)
    }

    fn enforce_pending_bound(&mut self) {
        if self.pending.len() <= self.max_pending_bytes {
            return;
        }
        self.discard_mode = Some(if self.pending.as_bytes().get(1) == Some(&b']') {
            DiscardMode::Osc
        } else {
            DiscardMode::Csi
        });
        self.pending.clear();
    }

    fn discard_prefix(&mut self, chunk: &str) -> String {
        match self.discard_mode {
            None => chunk.to_owned(),
            Some(DiscardMode::Csi) => {
                for (index, code) in chunk.bytes().enumerate() {
                    if (0x40..=0x7e).contains(&code) {
                        self.discard_mode = None;
                        return chunk[index + 1..].to_owned();
                    }
                }
                String::new()
            }
            Some(DiscardMode::Osc) => self.discard_osc_prefix(chunk),
        }
    }

    fn discard_osc_prefix(&mut self, chunk: &str) -> String {
        if self.discard_osc_escape {
            self.discard_osc_escape = false;
            if let Some(rest) = chunk.strip_prefix('\\') {
                self.discard_mode = None;
                return rest.to_owned();
            }
        }
        let bytes = chunk.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] == 0x07 {
                self.discard_mode = None;
                return chunk[index + 1..].to_owned();
            }
            if bytes[index] == 0x1b {
                if bytes.get(index + 1) == Some(&b'\\') {
                    self.discard_mode = None;
                    return chunk[index + 2..].to_owned();
                }
                if index + 1 == bytes.len() {
                    self.discard_osc_escape = true;
                }
            }
            index += 1;
        }
        String::new()
    }
}

fn append_text(value: &str, text: &mut String, tracking: bool, prompt_tail: &mut String) {
    text.push_str(value);
    if tracking {
        prompt_tail.push_str(value);
    }
}

/// Normalizes CRLF and standalone carriage returns and removes BEL.
#[must_use]
pub fn normalize_terminal_text(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\u{7}', "")
}
