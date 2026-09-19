//! Streaming sanitizer parity for split controls, prompt markers, and bounds.

use seekdeep_terminal_bash::{TerminalSanitizer, normalize_terminal_text};

fn chunk(
    text: &str,
    prompt: bool,
    prompt_tail: Option<&str>,
) -> seekdeep_terminal_bash::SanitizedChunk {
    seekdeep_terminal_bash::SanitizedChunk {
        text: text.to_owned(),
        prompt,
        prompt_tail: prompt_tail.map(str::to_owned),
    }
}

#[test]
fn removes_split_csi_and_owned_osc_prompt_markers() {
    let mut sanitizer = TerminalSanitizer::new(64);
    assert_eq!(sanitizer.push("red\x1b[3"), chunk("red", false, None));
    assert_eq!(
        sanitizer.push("1m text\x1b[0m\r\n"),
        chunk(" text\n", false, None)
    );
    assert_eq!(sanitizer.push("\x1b]133;"), chunk("", false, None));
    assert_eq!(
        sanitizer.push("D;0\x07seekdeep> "),
        chunk("seekdeep> ", true, Some("seekdeep> "))
    );
}

#[test]
fn drops_unrelated_controls_bel_and_incomplete_trailing_escape() {
    let mut sanitizer = TerminalSanitizer::new(64);
    assert_eq!(
        sanitizer.push("a\x1b]0;title\x1b\\b\x1b7c\x07"),
        chunk("abc", false, None)
    );
    assert_eq!(sanitizer.push("tail\x1b"), chunk("tail", false, None));
    assert_eq!(sanitizer.flush(), "");
    assert_eq!(sanitizer.flush(), "");
    assert_eq!(
        sanitizer.push("\x1b]0;one\x07middle\x1b\\"),
        chunk("middle", false, None)
    );
    assert_eq!(
        sanitizer.push("\x1b]0;one\x1b\\middle\x07"),
        chunk("middle", false, None)
    );
}

#[test]
fn normalizes_carriage_returns_across_chunks_and_flush() {
    assert_eq!(normalize_terminal_text("a\r\nb\rc\x07"), "a\nb\nc");
    let mut sanitizer = TerminalSanitizer::new(64);
    assert_eq!(sanitizer.push("a\r"), chunk("a", false, None));
    assert_eq!(sanitizer.push("\nb"), chunk("\nb", false, None));
    assert_eq!(sanitizer.push("\r"), chunk("", false, None));
    assert_eq!(sanitizer.flush(), "\n");
}

#[test]
fn reports_prompt_text_following_a_marker_in_a_later_chunk() {
    let mut sanitizer = TerminalSanitizer::new(64);
    assert_eq!(
        sanitizer.push("\x1b]133;D;0\x07"),
        chunk("", true, Some(""))
    );
    assert_eq!(
        sanitizer.push("seekdeep> "),
        chunk("seekdeep> ", false, Some("seekdeep> "))
    );
}

#[test]
fn bounds_unterminated_sequences_through_their_terminators() {
    let long_osc = format!("\x1b]0;{}", "x".repeat(16));
    let mut osc_bel = TerminalSanitizer::new(8);
    assert_eq!(osc_bel.push(&long_osc), chunk("", false, None));
    assert_eq!(osc_bel.push("more\x07tail"), chunk("tail", false, None));

    let mut osc_st = TerminalSanitizer::new(8);
    osc_st.push(&long_osc);
    assert_eq!(osc_st.push("more\x1b"), chunk("", false, None));
    assert_eq!(osc_st.push("\\tail"), chunk("tail", false, None));

    let mut osc_direct = TerminalSanitizer::new(8);
    osc_direct.push(&long_osc);
    assert_eq!(
        osc_direct.push("more\x1b\\tail"),
        chunk("tail", false, None)
    );

    let mut osc_false = TerminalSanitizer::new(8);
    osc_false.push(&long_osc);
    osc_false.push("\x1b");
    assert_eq!(osc_false.push("more"), chunk("", false, None));
    assert_eq!(osc_false.push("\x07tail"), chunk("tail", false, None));

    let mut nonterminating = TerminalSanitizer::new(8);
    nonterminating.push(&long_osc);
    assert_eq!(
        nonterminating.push("more\x1bxmore\x07tail"),
        chunk("tail", false, None)
    );

    let mut csi = TerminalSanitizer::new(8);
    assert_eq!(
        csi.push(&format!("\x1b[{}", "1".repeat(16))),
        chunk("", false, None)
    );
    assert_eq!(csi.push("123"), chunk("", false, None));
    assert_eq!(csi.push("mtext"), chunk("text", false, None));

    let mut flushed = TerminalSanitizer::new(8);
    flushed.push(&long_osc);
    assert_eq!(flushed.flush(), "");
    assert_eq!(flushed.push("text"), chunk("text", false, None));
}
