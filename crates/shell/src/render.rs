//! Shared parsing for shell-tool exit markers.

use std::sync::OnceLock;

use regex::Regex;

/// Exit status recovered from a rendered tool result.
#[derive(Clone, Debug, PartialEq)]
pub enum ParsedExitStatus {
    /// Ordinary exit code plus marker-free body.
    Exit {
        /// Output preceding the consumed marker.
        body: String,
        /// JavaScript-number-compatible exit code.
        exit_code: f64,
    },
    /// Signal termination plus marker-free body.
    Signal {
        /// Output preceding the consumed marker.
        body: String,
        /// Extensible signal spelling.
        signal: String,
    },
}

fn signal_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"\n\[killed by signal: ([^\]\n]+)\]$").expect("static signal marker")
    })
}

fn exit_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"\n\[exit code: (\d+)\]$").expect("static exit marker"))
}

/// Splits the final exit/signal marker from rendered output.
#[must_use]
pub fn parse_exit_status(text: &str) -> ParsedExitStatus {
    if let Some(captures) = signal_pattern().captures(text)
        && let (Some(whole), Some(signal)) = (captures.get(0), captures.get(1))
    {
        return ParsedExitStatus::Signal {
            body: text[..whole.start()].to_owned(),
            signal: signal.as_str().to_owned(),
        };
    }
    if let Some(captures) = exit_pattern().captures(text)
        && let (Some(whole), Some(exit_code)) = (captures.get(0), captures.get(1))
    {
        return ParsedExitStatus::Exit {
            body: text[..whole.start()].to_owned(),
            exit_code: exit_code.as_str().parse().unwrap_or(f64::INFINITY),
        };
    }
    ParsedExitStatus::Exit {
        body: text.to_owned(),
        exit_code: 0.0,
    }
}
