//! Bounded model-facing terminal result rendering.

use seekdeep_terminal::{
    TerminalReadResult, TerminalSendRead, TerminalSendResult, TerminalSessionSnapshot,
    TerminalSessionStatus, TerminalSpawnResult,
};
use seekdeep_util::output_retention::{TextRetainer, TextRetentionStrategy};

const TRUNCATED: &str = "\n[output truncated]";

fn retain(text: &str, max_bytes: usize, tail: bool) -> String {
    let strategy = if tail {
        TextRetentionStrategy::Tail { max_bytes }
    } else {
        TextRetentionStrategy::Head { max_bytes }
    };
    let mut retainer = TextRetainer::new(strategy);
    retainer.push_str(text);
    retainer.finish().text
}

fn fit_with_suffix(content: &str, suffix: &str, max_bytes: usize) -> String {
    if suffix.len() >= max_bytes {
        return retain(suffix, max_bytes, true);
    }
    format!(
        "{}{}",
        retain(content, max_bytes - suffix.len(), true),
        suffix
    )
}

fn fit_with_prefix(prefix: &str, content: &str, max_bytes: usize) -> String {
    let fixed = format!("{prefix}{TRUNCATED}");
    if fixed.len() >= max_bytes {
        return retain(&fixed, max_bytes, false);
    }
    format!(
        "{prefix}{}{TRUNCATED}",
        retain(content, max_bytes - fixed.len(), true)
    )
}

fn bound_body_with_suffix(
    content: &str,
    metadata: &str,
    upstream_truncated: bool,
    max_bytes: usize,
) -> String {
    let suffix = format!(
        "{metadata}{}",
        if upstream_truncated { TRUNCATED } else { "" }
    );
    let complete = format!("{content}{suffix}");
    if complete.len() <= max_bytes {
        complete
    } else {
        fit_with_suffix(content, &format!("{metadata}{TRUNCATED}"), max_bytes)
    }
}

/// Bounds one complete acknowledgement on UTF-8 boundaries.
#[must_use]
pub fn bound_terminal_text(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    if TRUNCATED.len() >= max_bytes {
        return retain(TRUNCATED, max_bytes, true);
    }
    format!(
        "{}{TRUNCATED}",
        retain(text, max_bytes - TRUNCATED.len(), false)
    )
}

/// Renders one created session and bounded startup output.
#[must_use]
pub fn render_spawn(result: &TerminalSpawnResult, max_bytes: usize) -> String {
    let label = result.name.as_ref().map_or_else(
        || result.session_id.to_string(),
        |name| format!("{} ({name})", result.session_id),
    );
    let prefix = format!(
        "started terminal session {label} [type: {}]\n",
        result.terminal_type
    );
    let motd = if result.motd.is_empty() {
        "(no startup output)"
    } else {
        result.motd.as_str()
    };
    let complete = format!("{prefix}{motd}");
    if complete.len() <= max_bytes {
        complete
    } else {
        fit_with_prefix(&prefix, motd, max_bytes)
    }
}

fn status_text(status: &TerminalSessionStatus) -> String {
    match status {
        TerminalSessionStatus::Running => "running".to_owned(),
        TerminalSessionStatus::Exited { exit_code, signal } => format!(
            "exited code={} signal={}",
            exit_code.map_or_else(|| "null".to_owned(), |value| value.to_string()),
            signal.as_ref().map_or("null", |signal| signal.as_str())
        ),
    }
}

/// Renders one settled interactive send.
#[must_use]
pub fn render_send(result: &TerminalSendResult, max_bytes: usize) -> String {
    let output = if result.viewport.is_empty() {
        "(no new output)"
    } else {
        result.viewport.as_str()
    };
    bound_body_with_suffix(
        output,
        &format!(
            "\n[wait: {}]\n[session: {}]",
            serde_json::to_value(result.wait_reason)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default(),
            status_text(&result.session_status)
        ),
        result.truncated,
        max_bytes,
    )
}

/// Renders one consuming background-operation read.
#[must_use]
pub fn render_send_read(read: &TerminalSendRead) -> String {
    let separator = if read.delta.ends_with('\n') || read.delta.is_empty() {
        ""
    } else {
        "\n"
    };
    format!(
        "{}{}",
        read.delta,
        if read.truncated {
            format!("{separator}[output truncated]")
        } else {
            String::new()
        }
    )
}

/// Renders one retained-history page.
#[must_use]
pub fn render_read(result: &TerminalReadResult, max_bytes: usize) -> String {
    let output = if result.text.is_empty() {
        "(no retained output)"
    } else {
        result.text.as_str()
    };
    bound_body_with_suffix(
        output,
        &format!(
            "\n[lines: {}-{} of {}]",
            result.line_begin, result.line_end, result.total_lines
        ),
        result.truncated,
        max_bytes,
    )
}

/// Renders owner-visible sessions or the empty marker.
#[must_use]
pub fn render_list(sessions: &[TerminalSessionSnapshot], max_bytes: usize) -> String {
    if sessions.is_empty() {
        return "(no terminal sessions)".to_owned();
    }
    let text = sessions
        .iter()
        .map(|session| {
            let name = session
                .name
                .as_ref()
                .map_or_else(String::new, |name| format!(" ({name})"));
            let pid = session
                .pid
                .map_or_else(String::new, |pid| format!(" pid={}", pid.as_i64()));
            format!(
                "{}{name} [{}] {}{pid}",
                session.session_id,
                session.terminal_type,
                status_text(&session.status)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    bound_body_with_suffix(&text, "", false, max_bytes)
}
