//! Matcher shared by both hook dialects.

use std::sync::LazyLock;

use regex::Regex;

use crate::types::MatcherMode;

/// True for an absent / empty / * pattern — the match-all sentinels.
fn is_match_all(matcher: Option<&str>) -> bool {
    matcher.is_none() || matcher == Some("") || matcher == Some("*")
}

/// A Claude-literal pattern is purely word chars + pipe.
static CLAUDE_LITERAL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_|]+$").expect("static literal regex"));

/// Compiles an unanchored matcher regex; invalid patterns return none.
fn compile_regex(pattern: &str) -> Option<Regex> {
    Regex::new(pattern).ok()
}

/// Validates one matcher before a bridge accepts its config group.
#[must_use]
pub fn matcher_diagnostic(matcher: Option<&str>, mode: MatcherMode) -> Option<String> {
    if is_match_all(matcher) {
        return None;
    }
    let Some(pattern) = matcher else {
        return None;
    };
    if mode == MatcherMode::ClaudeCode && CLAUDE_LITERAL.is_match(pattern) {
        return None;
    }
    if compile_regex(pattern).is_none() {
        return Some(format!(
            "invalid {mode:?} regex matcher {}",
            serde_json::to_string(pattern).unwrap_or_default()
        ));
    }
    None
}

/// Whether matcher selects query under the given dialect.
#[must_use]
pub fn matches_matcher(matcher: Option<&str>, query: &str, mode: MatcherMode) -> bool {
    if is_match_all(matcher) {
        return true;
    }
    let Some(pattern) = matcher else {
        return false;
    };
    if mode == MatcherMode::ClaudeCode && CLAUDE_LITERAL.is_match(pattern) {
        return pattern.split('|').any(|alternative| alternative == query);
    }
    compile_regex(pattern).is_some_and(|regex| regex.is_match(query))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_all_sentinels_select_everything() {
        assert!(matches_matcher(None, "tool", MatcherMode::Codex));
        assert!(matches_matcher(Some(""), "tool", MatcherMode::Codex));
        assert!(matches_matcher(Some("*"), "tool", MatcherMode::Codex));
    }

    #[test]
    fn claude_literal_matches_exact_alternatives() {
        assert!(matches_matcher(Some("a|b"), "a", MatcherMode::ClaudeCode));
        assert!(matches_matcher(Some("a|b"), "b", MatcherMode::ClaudeCode));
        assert!(!matches_matcher(Some("a|b"), "c", MatcherMode::ClaudeCode));
    }

    #[test]
    fn codex_treats_pattern_as_regex() {
        assert!(matches_matcher(
            Some("^tool"),
            "tool-name",
            MatcherMode::Codex
        ));
        assert!(!matches_matcher(
            Some("^tool"),
            "notool",
            MatcherMode::Codex
        ));
    }

    #[test]
    fn invalid_regex_is_non_match_and_diagnosed() {
        assert!(!matches_matcher(Some("["), "x", MatcherMode::Codex));
        assert!(matcher_diagnostic(Some("["), MatcherMode::Codex).is_some());
        assert!(matcher_diagnostic(Some("ok"), MatcherMode::Codex).is_none());
    }
}
