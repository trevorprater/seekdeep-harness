//! Define-time sandbox syntax validation parity.

use seekdeep_cordis_host_runner::{
    HOST_BUILTIN_INSPECTION, syntax_error_context, validate_host_code,
};

#[test]
fn valid_runtime_failures_parse_without_executing_side_effects() {
    for body in [
        "return { apply() {} }",
        "throw null",
        "while (true) {}",
        "globalThis.neverRuns = true; return { apply() {} }",
    ] {
        validate_host_code(body).unwrap();
    }
}

#[test]
fn typescript_is_rejected_at_define_time_with_the_plain_javascript_fix() {
    let error = validate_host_code("return { name: 'typed' as const, apply() {} }").unwrap_err();
    assert!(
        error
            .to_string()
            .contains("plain JavaScript, not TypeScript")
    );
}

#[test]
fn syntax_failure_bounds_source_context_and_teaches_bracket_balance() {
    let long = "x".repeat(400);
    let error =
        validate_host_code(&format!("const value = {long};\nreturn {{ apply() {{\n")).unwrap_err();
    assert!(error.to_string().contains("bracket balance"));
    assert!(error.to_string().contains("failed to parse"));
    assert!(error.to_string().contains("BODY of an async function"));
    assert!(!error.to_string().contains("TypeScript"));
    assert!(!error.context().is_empty(), "{error:?}");
    assert!(error.context().chars().count() < 300);
    assert!(error.hint().contains("BODY"));
}

#[test]
fn syntax_error_context_uses_a_stable_fallback_without_a_parser_prelude() {
    assert_eq!(syntax_error_context("boom", None), "SyntaxError: boom");
    assert_eq!(
        syntax_error_context("bang", Some("not-a-parser-stack")),
        "SyntaxError: bang"
    );
    assert_eq!(
        syntax_error_context(
            "ignored",
            Some("dynamic.js:2\nreturn });\n       ^\nSyntaxError: unexpected token\nrest")
        ),
        "dynamic.js:2\nreturn });\n       ^\nSyntaxError: unexpected token"
    );
}

#[test]
fn host_builtin_inspection_has_the_exact_model_visible_directory() {
    assert_eq!(
        HOST_BUILTIN_INSPECTION
            .iter()
            .map(|entry| entry.name)
            .collect::<Vec<_>>(),
        [
            "ctx",
            "harness",
            "console",
            "btoa",
            "atob",
            "TextEncoder",
            "TextDecoder",
        ]
    );
    assert!(
        HOST_BUILTIN_INSPECTION[0]
            .signatures
            .contains(&"ctx.provide(name: string, value: unknown): () => void")
    );
}
