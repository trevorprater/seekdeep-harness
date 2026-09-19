//! Prose, tag, and completeness contracts of the shared `JSDoc` helpers.

use indexmap::IndexMap;
use seekdeep_repository_tools::jsdoc::{
    DeclaredParameter, Mode, check_params, check_returns, parse_jsdoc, parse_tags, raw_jsdoc,
    report_violations,
};

#[test]
fn prose_collapses_paragraphs_keeps_list_items_and_unlinks() {
    let raw = "/**\n * A thing {@link Other} happened.\n * Continued line.\n *\n * - first item\n *   wraps\n * - second item\n * @mode waterfall\n * @param x - ignored in prose\n */";
    let parsed = parse_jsdoc(raw);
    assert_eq!(
        parsed.doc,
        "A thing Other happened. Continued line.\n\n- first item wraps\n- second item"
    );
    assert_eq!(parsed.mode, Some(Mode::Waterfall));
    assert!(parsed.has_mode);
}

#[test]
fn an_invalid_mode_is_present_but_unparsed_and_intro_lines_flush_above_lists() {
    let parsed = parse_jsdoc("/** Intro line\n * - item\n * @mode sometimes\n */");
    assert_eq!(parsed.doc, "Intro line\n\n- item");
    assert_eq!(parsed.mode, None);
    assert!(parsed.has_mode);
    assert!(!parse_jsdoc("/** No tags at all. */").has_mode);
}

#[test]
fn tags_parse_optional_separators_bracketed_names_and_continuations() {
    let raw = "/**\n * Doc.\n * @param [count] — how many\n *   more words\n * @param name the label\n * @returns the\n *   result\n * @throws never\n */";
    let tags = parse_tags(raw);
    assert_eq!(
        tags.params.get("count").map(String::as_str),
        Some("how many more words")
    );
    assert_eq!(
        tags.params.get("name").map(String::as_str),
        Some("the label")
    );
    assert_eq!(tags.returns.as_deref(), Some("the result"));
    assert_eq!(parse_tags("/** @returns */").returns.as_deref(), Some(""));
    assert_eq!(parse_tags("/** nothing */").returns, None);
}

#[test]
fn completeness_checks_name_every_violation_kind() {
    let parameters = vec![
        DeclaredParameter {
            identifier: Some("this".to_owned()),
            text: "this".to_owned(),
        },
        DeclaredParameter {
            identifier: Some("value".to_owned()),
            text: "value".to_owned(),
        },
        DeclaredParameter {
            identifier: Some("empty".to_owned()),
            text: "empty".to_owned(),
        },
        DeclaredParameter {
            identifier: None,
            text: "{ a, b }".to_owned(),
        },
    ];
    let mut tags = IndexMap::new();
    tags.insert("empty".to_owned(), "  ".to_owned());
    tags.insert("stale".to_owned(), "gone".to_owned());
    let mut violations = Vec::new();
    check_params(
        "event 'x' (f:1)",
        "event",
        &parameters,
        &tags,
        &|parameter| parameter.identifier.as_deref() == Some("this"),
        &mut violations,
    );
    assert_eq!(
        violations,
        [
            "event 'x' (f:1) is missing @param value.",
            "event 'x' (f:1): @param empty has an empty description.",
            "event 'x' (f:1): parameter '{ a, b }' is a binding pattern; the event API needs simple identifier parameters so @param can name them.",
            "event 'x' (f:1): @param stale does not match any parameter (stale tag?).",
        ]
    );
    let mut returns = Vec::new();
    check_returns("m", None, None, &mut returns);
    check_returns("m", Some("void"), None, &mut returns);
    check_returns("m", Some("Promise<\n  void>"), None, &mut returns);
    check_returns("m", Some("Promise<void>"), None, &mut returns);
    check_returns("m", Some("Promise<Result>"), None, &mut returns);
    check_returns("m", Some("string"), Some(" "), &mut returns);
    check_returns("m", Some("string"), Some("ok"), &mut returns);
    assert_eq!(
        returns,
        [
            "m has no return type annotation; annotate it explicitly so the gate can classify the result.",
            "m is missing @returns (return type: Promise< void>).",
            "m is missing @returns (return type: Promise<Result>).",
            "m: @returns has an empty description.",
        ]
    );
    assert!(report_violations("gate", &[]).is_ok());
    let error = report_violations("gate", &["one".to_owned()]).unwrap_err();
    assert!(
        error
            .to_string()
            .starts_with("gate: 1 JSDoc completeness violation(s) (see AGENTS.md):\n  one")
    );
}

#[test]
fn raw_jsdoc_takes_the_last_documentation_comment_before_the_node() {
    let text = "// lead\n/** first */\n/* plain */\n/** second\n */\nexport class A {}";
    let start = text.find("export").unwrap();
    assert_eq!(raw_jsdoc(text, 0), "/** second\n */");
    assert_eq!(raw_jsdoc(text, start), "");
    assert_eq!(raw_jsdoc("export class A {}", 0), "");
}
