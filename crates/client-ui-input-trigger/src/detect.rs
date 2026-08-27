//! Pure backward-scanning trigger detection over JavaScript UTF-16 offsets.

use crate::{TokenSpan, TriggerChar, TriggerGuard, TriggerPosition, TriggerTier};

/// A detected trigger token under the caret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TriggerHit {
    /// Trigger character.
    pub trigger: TriggerChar,
    /// Text between trigger and caret.
    pub query: String,
    /// Leading or inline placement.
    pub position: TriggerPosition,
    /// UTF-16 token span with placeholder revision zero.
    pub span: TokenSpan,
}

fn is_js_whitespace(unit: u16) -> bool {
    matches!(
        unit,
        0x0009..=0x000d
            | 0x0020
            | 0x00a0
            | 0x1680
            | 0x2000..=0x200a
            | 0x2028
            | 0x2029
            | 0x202f
            | 0x205f
            | 0x3000
            | 0xfeff
    )
}

fn is_word_unit(unit: u16) -> bool {
    unit == u16::from(b'_')
        || char::from_u32(u32::from(unit))
            .is_some_and(|character| character.is_alphabetic() || character.is_numeric())
}

fn boundary_ok(draft: &[u16], index: usize, trigger: TriggerChar) -> bool {
    if index == 0 {
        return true;
    }
    let previous = draft[index - 1];
    if is_js_whitespace(previous) {
        return true;
    }
    if is_word_unit(previous) {
        return false;
    }
    if trigger == TriggerChar::Slash {
        if previous == u16::from(b'/') {
            return false;
        }
        if previous == u16::from(b':') && index >= 2 && !is_js_whitespace(draft[index - 2]) {
            return false;
        }
    }
    true
}

/// Detects the nearest live trigger left of one UTF-16 caret offset.
#[must_use]
pub fn detect_trigger(
    draft: &str,
    caret: usize,
    guard: impl Into<TriggerGuard>,
) -> Option<TriggerHit> {
    let tier = guard.into().tier;
    if tier == TriggerTier::Frozen || caret == 0 {
        return None;
    }
    let units = draft.encode_utf16().collect::<Vec<_>>();
    let scan_end = caret.min(units.len());
    for index in (0..scan_end).rev() {
        let unit = units[index];
        if is_js_whitespace(unit) {
            return None;
        }
        let trigger = match unit {
            value if value == u16::from(b'/') => TriggerChar::Slash,
            value if value == u16::from(b'@') => TriggerChar::At,
            _ => continue,
        };
        if tier == TriggerTier::Claimed && trigger == TriggerChar::Slash {
            continue;
        }
        if !boundary_ok(&units, index, trigger) {
            continue;
        }
        let first_non_whitespace = units.iter().position(|unit| !is_js_whitespace(*unit));
        return Some(TriggerHit {
            trigger,
            query: String::from_utf16_lossy(&units[index + 1..scan_end]),
            position: if first_non_whitespace == Some(index) {
                TriggerPosition::Leading
            } else {
                TriggerPosition::Inline
            },
            span: TokenSpan {
                start: index,
                end: caret,
                draft_rev: 0,
            },
        });
    }
    None
}
