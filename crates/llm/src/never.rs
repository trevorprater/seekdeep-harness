//! Runtime diagnostics for values that escape a closed wire union.

use serde_json::Value;

/// Panics with the source-compatible diagnostic for an unreachable variant.
///
/// `None` represents JavaScript `undefined`; wire-facing callers pass the
/// offending lossless JSON value as `Some`.
///
/// # Panics
///
/// Always panics, exactly like the source `assertNever` helper always throws.
pub fn assert_never(value: Option<&Value>, context: Option<&str>) -> ! {
    let rendered = value.map_or_else(
        || "undefined".to_owned(),
        |value| serde_json::to_string(value).expect("serde_json::Value always serializes"),
    );
    match context {
        Some(context) => panic!("unreachable variant in {context}: {rendered}"),
        None => panic!("unreachable variant: {rendered}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reports_context_json_and_undefined() {
        let rogue = std::panic::catch_unwind(|| {
            assert_never(Some(&json!({"type": "rogue"})), Some("test-context"));
        })
        .expect_err("always throws");
        let message = rogue
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| rogue.downcast_ref::<&str>().copied())
            .expect("panic message");
        assert_eq!(
            message,
            "unreachable variant in test-context: {\"type\":\"rogue\"}"
        );

        let undefined =
            std::panic::catch_unwind(|| assert_never(None, None)).expect_err("always throws");
        let message = undefined
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| undefined.downcast_ref::<&str>().copied())
            .expect("panic message");
        assert_eq!(message, "unreachable variant: undefined");
    }
}
