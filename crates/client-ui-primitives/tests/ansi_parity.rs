//! Source fixtures for terminal replay, SGR styling, Unicode columns, and caps.

use std::fmt::Write as _;

use seekdeep_client_ui_primitives::{
    AnsiLine, AnsiSpan, AnsiStyle, head_tail_cap, parse_ansi_lines,
};

const ESC: &str = "\u{1b}";
const BS: &str = "\u{8}";

fn sgr(codes: &str, text: &str) -> String {
    format!("{ESC}[{codes}m{text}{ESC}[0m")
}

fn style(entries: &[(&str, &str)]) -> AnsiStyle {
    let mut style = AnsiStyle::default();
    for (key, value) in entries {
        match *key {
            "color" => style.color = Some((*value).to_owned()),
            "backgroundColor" => style.background_color = Some((*value).to_owned()),
            "fontStyle" => style.font_style = Some((*value).to_owned()),
            "textDecoration" => style.text_decoration = Some((*value).to_owned()),
            "visibility" => style.visibility = Some((*value).to_owned()),
            _ => panic!("unknown style field"),
        }
    }
    style
}

fn styled(text: &str, style: AnsiStyle) -> AnsiSpan {
    AnsiSpan {
        text: text.to_owned(),
        style: Some(style),
    }
}

fn plain(text: &str) -> AnsiSpan {
    AnsiSpan {
        text: text.to_owned(),
        style: None,
    }
}

fn only(text: &str) -> AnsiSpan {
    let lines = parse_ansi_lines(text);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].len(), 1);
    lines[0][0].clone()
}

#[test]
fn plain_text_lines_and_tabs_are_preserved() {
    assert_eq!(parse_ansi_lines(""), vec![AnsiLine::new()]);
    assert_eq!(parse_ansi_lines("hello"), vec![vec![plain("hello")]]);
    assert_eq!(
        parse_ansi_lines("a\n\nb"),
        vec![vec![plain("a")], vec![], vec![plain("b")]]
    );
    assert_eq!(only("a\tb"), plain("a\tb"));
}

#[test]
fn basic_foregrounds_map_to_exact_theme_tokens() {
    for (code, token) in [
        ("30", "var(--dsw-alias-label-primary)"),
        ("37", "var(--dsw-alias-label-primary)"),
        ("90", "var(--dsw-alias-label-tertiary)"),
        ("31", "var(--dsw-alias-state-error-primary)"),
        ("91", "var(--dsw-alias-state-error-secondary)"),
        ("32", "var(--dsw-alias-state-success-primary)"),
        ("92", "var(--dsw-alias-state-success-secondary)"),
        ("33", "var(--dsw-alias-state-warn-primary)"),
        ("93", "var(--dsw-alias-state-warn-secondary)"),
        ("34", "var(--dsw-alias-state-business-primary)"),
        ("94", "var(--dsw-static-blue-400)"),
    ] {
        assert_eq!(
            only(&sgr(code, "x")),
            styled("x", style(&[("color", token)])),
            "SGR {code}"
        );
    }
}

#[test]
fn literal_palette_truecolor_background_and_reverse_match_anser() {
    for (code, literal) in [
        ("35", "rgb(187, 0, 187)"),
        ("36", "rgb(0, 187, 187)"),
        ("38;5;208", "rgb(255, 135, 0)"),
        ("38;2;10;20;30", "rgb(10, 20, 30)"),
    ] {
        assert_eq!(
            only(&sgr(code, "x")),
            styled("x", style(&[("color", literal)]))
        );
    }
    assert_eq!(
        only(&sgr("44", "x")),
        styled("x", style(&[("backgroundColor", "rgb(0, 0, 187)"),]))
    );
    assert_eq!(
        only(&sgr("41;37", "x")),
        styled(
            "x",
            style(&[
                ("backgroundColor", "rgb(187, 0, 0)"),
                ("color", "rgb(255,255,255)"),
            ])
        )
    );
    assert_eq!(
        only(&sgr("31;7", "x")),
        styled(
            "x",
            style(&[
                ("backgroundColor", "rgb(187, 0, 0)"),
                ("color", "rgb(0, 0, 0)"),
            ])
        )
    );
}

#[test]
fn decorations_combine_close_and_resolve_in_declaration_order() {
    let cases = [
        (
            "1",
            AnsiStyle {
                font_weight: Some(700),
                ..AnsiStyle::default()
            },
        ),
        (
            "2",
            AnsiStyle {
                opacity: Some(0.7),
                ..AnsiStyle::default()
            },
        ),
        ("3", style(&[("fontStyle", "italic")])),
        ("4", style(&[("textDecoration", "underline")])),
        ("9", style(&[("textDecoration", "line-through")])),
        ("8", style(&[("visibility", "hidden")])),
    ];
    for (code, expected) in cases {
        assert_eq!(only(&sgr(code, "x")), styled("x", expected));
    }
    assert_eq!(
        only(&sgr("4;9", "x")),
        styled("x", style(&[("textDecoration", "line-through")]))
    );
    assert_eq!(
        only(&sgr("9;4", "x")),
        styled("x", style(&[("textDecoration", "underline")]))
    );
    let mut combined = style(&[
        ("color", "var(--dsw-alias-state-error-primary)"),
        ("fontStyle", "italic"),
    ]);
    combined.font_weight = Some(700);
    assert_eq!(only(&sgr("1;3;31", "x")), styled("x", combined));
    assert_eq!(only(&sgr("5", "x")), plain("x"));
}

#[test]
fn non_color_escape_and_inert_control_sequences_are_removed() {
    assert_eq!(only(&format!("a{ESC}]0;window title\u{7}b")), plain("ab"));
    assert_eq!(
        only(&format!("a{ESC}]8;;https://example.com{ESC}\\b")),
        plain("ab")
    );
    assert_eq!(only(&format!("x{ESC}(By{ESC}cz")), plain("xyz"));
    assert_eq!(only("\u{0}ab\u{1f}c\u{7f}"), plain("abc"));
    assert_eq!(only(&format!("{ESC}[2K{ESC}[1Adone")), plain("done"));
    assert_eq!(
        parse_ansi_lines(&format!("a{ESC}[0mb")),
        vec![vec![plain("a"), plain("b")]]
    );
    assert_eq!(
        parse_ansi_lines(&format!("plain{ESC}[5mA{ESC}[25mB")),
        vec![vec![plain("plain"), plain("A"), plain("B")]]
    );
}

#[test]
fn carriage_returns_and_crlf_repaint_each_line_without_erasing_tails() {
    assert_eq!(only("10%\r55%\r100%"), plain("100%"));
    assert_eq!(only("100%\rOK"), plain("OK0%"));
    assert_eq!(only("abcdef\rXY"), plain("XYcdef"));
    assert_eq!(only(&format!("ab{BS}{BS}{BS}{BS}xyz")), plain("xyz"));
    assert_eq!(
        only(&format!("{ESC}[31mgone\rkept")),
        styled(
            "kept",
            style(&[("color", "var(--dsw-alias-state-error-primary)")])
        )
    );
    assert_eq!(
        parse_ansi_lines("a\r\r\nb\r\n"),
        vec![vec![plain("a")], vec![plain("b")], vec![]]
    );
    assert_eq!(
        parse_ansi_lines("one\rtwo\nthree"),
        vec![vec![plain("two")], vec![plain("three")]]
    );
}

#[test]
fn backspace_moves_the_cursor_and_preserves_unreached_cell_styles() {
    assert_eq!(only(&format!("abc{BS}{BS}XY")), plain("aXY"));
    assert_eq!(only(&format!("abc{BS}")), plain("abc"));
    assert_eq!(
        parse_ansi_lines(&format!("{}{}{}XY", sgr("31", "abc"), BS, BS)),
        vec![vec![
            styled(
                "a",
                style(&[("color", "var(--dsw-alias-state-error-primary)")])
            ),
            plain("XY"),
        ]]
    );
    assert_eq!(
        parse_ansi_lines(&format!("{}{ESC}[31m{BS}bad", sgr("32", "ok"))),
        vec![vec![
            styled(
                "o",
                style(&[("color", "var(--dsw-alias-state-success-primary)")])
            ),
            styled(
                "bad",
                style(&[("color", "var(--dsw-alias-state-error-primary)")])
            ),
        ]]
    );
    assert_eq!(only(&format!("old\rnew{BS}")), plain("new"));
    assert_eq!(
        parse_ansi_lines(&format!("{}{BS}{BS}{BS}ok", sgr("31", "bad"))),
        vec![vec![
            plain("ok"),
            styled(
                "d",
                style(&[("color", "var(--dsw-alias-state-error-primary)")])
            ),
        ]]
    );
}

#[test]
fn erase_tab_wide_and_combining_column_arithmetic_matches_terminal_painting() {
    assert_eq!(only(&format!("100%\r{ESC}[KOK")), plain("OK"));
    assert_eq!(only(&format!("ab\r{ESC}[2Kxy")), plain("xy"));
    assert_eq!(only(&format!("abcd{ESC}[1K|")), plain("    |"));
    assert_eq!(only(&format!("abcd{ESC}[2Kx")), plain("    x"));
    assert_eq!(only("a\tb\rXY"), plain("XY      b"));
    assert_eq!(only("中x\rab"), plain("abx"));
    assert_eq!(only("e\u{301}x\rYZ"), plain("YZ"));
    assert_eq!(only("ab\r\u{301}x"), plain("xb"));
    assert_eq!(only("中x\rA"), plain("A x"));
    assert_eq!(only(&format!("中x{BS}{BS}A")), plain(" Ax"));
    assert_eq!(only(&format!("中x{ESC}[1K|")), plain("   |"));
    assert_eq!(only("A\u{2713}B\rXY"), plain("XYB"));
    assert_eq!(only("A\u{1f600}B\rXY"), plain("XY B"));
}

#[test]
fn state_at_line_end_and_across_lines_uses_the_scan_not_last_cell() {
    assert_eq!(
        parse_ansi_lines(&format!("{ESC}[32mdone\rok{ESC}[0m\nplain")),
        vec![
            vec![styled(
                "okne",
                style(&[("color", "var(--dsw-alias-state-success-primary)")])
            )],
            vec![plain("plain")],
        ]
    );
    assert_eq!(
        parse_ansi_lines(&format!("ab\rX{ESC}[31m\nnext")),
        vec![
            vec![plain("Xb")],
            vec![styled(
                "next",
                style(&[("color", "var(--dsw-alias-state-error-primary)")])
            )],
        ]
    );
    assert_eq!(
        parse_ansi_lines(&format!("{ESC}[31mabc\rX\nnext")),
        vec![
            vec![styled(
                "Xbc",
                style(&[("color", "var(--dsw-alias-state-error-primary)")])
            )],
            vec![styled(
                "next",
                style(&[("color", "var(--dsw-alias-state-error-primary)")])
            )],
        ]
    );
    assert_eq!(
        parse_ansi_lines(&sgr("32", "first\nsecond")),
        vec![
            vec![styled(
                "first",
                style(&[("color", "var(--dsw-alias-state-success-primary)")])
            )],
            vec![styled(
                "second",
                style(&[("color", "var(--dsw-alias-state-success-primary)")])
            )],
        ]
    );
}

#[test]
fn normalized_state_stays_linear_and_every_closer_is_effective() {
    let mut input = String::new();
    for index in 0..2_000 {
        write!(input, "{ESC}[3{}mx", index % 6 + 1).unwrap();
    }
    input.push_str("\rz");
    let emitted = parse_ansi_lines(&input);
    assert_eq!(
        emitted[0].iter().map(|span| span.text.len()).sum::<usize>(),
        2_000
    );
    assert_eq!(
        parse_ansi_lines(&format!("{ESC}[1mA{ESC}[22mB")),
        vec![vec![
            styled(
                "A",
                AnsiStyle {
                    font_weight: Some(700),
                    ..AnsiStyle::default()
                }
            ),
            plain("B"),
        ]]
    );
    assert_eq!(
        parse_ansi_lines(&format!("{ESC}[38;5;208mA\r{ESC}[KB")),
        vec![vec![styled("B", style(&[("color", "rgb(255, 135, 0)")]))]]
    );
    assert_eq!(
        parse_ansi_lines(&format!("{ESC}[48;2;1;2;3mA\r{ESC}[KB")),
        vec![vec![styled(
            "B",
            style(&[("backgroundColor", "rgb(1, 2, 3)")])
        )]]
    );
    assert_eq!(
        parse_ansi_lines(&format!("{ESC}[3;4mA{ESC}[24mB\r{ESC}[KC")),
        vec![vec![styled("C", style(&[("fontStyle", "italic")]))]]
    );
    assert_eq!(
        parse_ansi_lines(&format!("{ESC}[31;42mA{ESC}[39mB\r{ESC}[KC")),
        vec![vec![styled(
            "C",
            style(&[("backgroundColor", "rgb(0, 187, 0)")])
        )]]
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn remaining_source_cursor_width_and_sgr_edges_are_exact() {
    assert_eq!(
        parse_ansi_lines(&format!("ab\n{BS}{BS}{BS}cd")),
        vec![vec![plain("ab")], vec![plain("cd")]]
    );
    assert_eq!(only(&format!("abcd{BS}{ESC}[1K|")), plain("   |"));
    assert_eq!(only("\u{301}abc"), plain("\u{301}abc"));
    assert_eq!(only(&format!("abcd{ESC}[1;2K|")), plain("    |"));
    assert_eq!(only(&format!("中x\r{BS}A")), plain("A x"));
    assert_eq!(only("中中\rA"), plain("A 中"));
    assert_eq!(only(&format!("中中{BS}{BS}{BS}好")), plain(" 好 "));
    assert_eq!(
        parse_ansi_lines(&format!("a\r{ESC}[32mb\nplain\nc")),
        vec![
            vec![styled(
                "b",
                style(&[("color", "var(--dsw-alias-state-success-primary)")])
            )],
            vec![styled(
                "plain",
                style(&[("color", "var(--dsw-alias-state-success-primary)")])
            )],
            vec![styled(
                "c",
                style(&[("color", "var(--dsw-alias-state-success-primary)")])
            )],
        ]
    );
    assert_eq!(
        parse_ansi_lines(&format!("plain{}tail", sgr("31", "red"))),
        vec![vec![
            plain("plain"),
            styled(
                "red",
                style(&[("color", "var(--dsw-alias-state-error-primary)")])
            ),
            plain("tail"),
        ]]
    );
    assert_eq!(
        parse_ansi_lines(&format!("{ESC}[101mA\r{ESC}[KB")),
        vec![vec![styled(
            "B",
            style(&[("backgroundColor", "rgb(255, 85, 85)")])
        )]]
    );
    assert_eq!(
        parse_ansi_lines(&format!("{ESC}[38mA\r{ESC}[KB")),
        vec![vec![plain("B")]]
    );
    assert_eq!(
        parse_ansi_lines(&format!("{ESC}[1m{ESC}[1mA{ESC}[mB\r{ESC}[KC")),
        vec![vec![plain("C")]]
    );
    assert_eq!(
        parse_ansi_lines(&format!("{ESC}[31ma\r{ESC}[Kb")),
        vec![vec![styled(
            "b",
            style(&[("color", "var(--dsw-alias-state-error-primary)")])
        )]]
    );
}

#[test]
fn head_tail_cap_preserves_source_arithmetic() {
    assert_eq!(
        head_tail_cap(17.0, 16.0, false),
        seekdeep_client_ui_primitives::HeadTailCap {
            hidden: 1.0,
            capped: true,
            head_lines: 8.0,
            tail_lines: 8.0,
        }
    );
    assert_eq!(
        head_tail_cap(20.0, 5.0, false),
        seekdeep_client_ui_primitives::HeadTailCap {
            hidden: 15.0,
            capped: true,
            head_lines: 3.0,
            tail_lines: 2.0,
        }
    );
    assert!(!head_tail_cap(17.0, 16.0, true).capped);
    assert!(!head_tail_cap(16.0, 16.0, false).capped);
    let infinite = head_tail_cap(100.0, f64::INFINITY, false);
    assert!(infinite.hidden.is_infinite() && infinite.hidden.is_sign_negative());
    assert!(!infinite.capped);
}
