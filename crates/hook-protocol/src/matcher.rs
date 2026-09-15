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

/// The dialect wire spelling used in diagnostics.
fn mode_str(mode: MatcherMode) -> &'static str {
    match mode {
        MatcherMode::ClaudeCode => "claude-code",
        MatcherMode::Codex => "codex",
    }
}

/// Validates one matcher before a bridge accepts its config group.
#[must_use]
pub fn matcher_diagnostic(matcher: Option<&str>, mode: MatcherMode) -> Option<String> {
    if is_match_all(matcher) {
        return None;
    }
    let pattern = matcher?;
    if mode == MatcherMode::ClaudeCode && CLAUDE_LITERAL.is_match(pattern) {
        return None;
    }
    if compile_regex(pattern).is_none() {
        return Some(format!(
            "invalid {} regex matcher {}",
            mode_str(mode),
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
    fn match_all_sentinels_select_everything_in_both_dialects() {
        for mode in [MatcherMode::ClaudeCode, MatcherMode::Codex] {
            assert!(matches_matcher(None, "Bash", mode));
            assert!(matches_matcher(Some(""), "anything", mode));
            assert!(matches_matcher(Some("*"), "whatever", mode));
        }
    }

    #[test]
    fn claude_literal_is_exact_not_substring() {
        assert!(matches_matcher(
            Some("Bash"),
            "Bash",
            MatcherMode::ClaudeCode
        ));
        assert!(!matches_matcher(
            Some("Bash"),
            "BashOutput",
            MatcherMode::ClaudeCode
        ));
    }

    #[test]
    fn claude_pipe_pattern_is_literal_alternation() {
        assert!(matches_matcher(
            Some("Edit|Write"),
            "Edit",
            MatcherMode::ClaudeCode
        ));
        assert!(matches_matcher(
            Some("Edit|Write"),
            "Write",
            MatcherMode::ClaudeCode
        ));
        assert!(!matches_matcher(
            Some("Edit|Write"),
            "Read",
            MatcherMode::ClaudeCode
        ));
        assert!(!matches_matcher(
            Some("Edit|Write"),
            "EditFile",
            MatcherMode::ClaudeCode
        ));
    }

    #[test]
    fn claude_non_word_pattern_falls_through_to_regex() {
        assert!(matches_matcher(
            Some("^Bash$"),
            "Bash",
            MatcherMode::ClaudeCode
        ));
        assert!(matches_matcher(
            Some("Bash.*"),
            "BashOutput",
            MatcherMode::ClaudeCode
        ));
        assert!(matches_matcher(
            Some(".*[.]ts$"),
            "foo.ts",
            MatcherMode::ClaudeCode
        ));
        assert!(!matches_matcher(
            Some(".*[.]ts$"),
            "foo.js",
            MatcherMode::ClaudeCode
        ));
    }

    #[test]
    fn codex_treats_word_pattern_as_unanchored_regex() {
        assert!(matches_matcher(Some("Bash"), "Bash", MatcherMode::Codex));
        assert!(matches_matcher(
            Some("Bash"),
            "BashOutput",
            MatcherMode::Codex
        ));
    }

    #[test]
    fn codex_regex_alternation_and_anchors() {
        assert!(matches_matcher(
            Some("Edit|Write"),
            "Edit",
            MatcherMode::Codex
        ));
        assert!(matches_matcher(Some("^Bash$"), "Bash", MatcherMode::Codex));
        assert!(!matches_matcher(
            Some("^Bash$"),
            "BashOutput",
            MatcherMode::Codex
        ));
    }

    #[test]
    fn invalid_regex_is_a_non_match() {
        assert!(!matches_matcher(Some("("), "x", MatcherMode::ClaudeCode));
        assert!(!matches_matcher(Some("["), "x", MatcherMode::Codex));
    }

    #[test]
    fn diagnostic_accepts_sentinels_literals_and_valid_regexes() {
        assert_eq!(matcher_diagnostic(None, MatcherMode::ClaudeCode), None);
        assert_eq!(matcher_diagnostic(Some(""), MatcherMode::Codex), None);
        assert_eq!(matcher_diagnostic(Some("*"), MatcherMode::Codex), None);
        assert_eq!(
            matcher_diagnostic(Some("Edit|Write"), MatcherMode::ClaudeCode),
            None
        );
        assert_eq!(
            matcher_diagnostic(Some("^Bash$"), MatcherMode::ClaudeCode),
            None
        );
        assert_eq!(
            matcher_diagnostic(Some("Edit|Write"), MatcherMode::Codex),
            None
        );
    }

    #[test]
    fn diagnostic_returns_stable_strings_for_invalid_regexes() {
        assert_eq!(
            matcher_diagnostic(Some("("), MatcherMode::ClaudeCode).as_deref(),
            Some(r#"invalid claude-code regex matcher "(""#)
        );
        assert_eq!(
            matcher_diagnostic(Some("["), MatcherMode::Codex).as_deref(),
            Some(r#"invalid codex regex matcher "[""#)
        );
    }
}
