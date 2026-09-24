//! CSS Modules selector semantics for stylesheets the browser crates inject
//! verbatim from the source `*.module.css` files.
//!
//! The source bundler compiles `:global(<selector>)` down to `<selector>`
//! before the rules reach a `<style>` element. A plain `<style>` element does
//! not know the pseudo-class, so every rule that keeps the wrapper is dropped
//! by the browser's parser; unwrapping restores the compiled form.

/// Rewrites every `:global(<selector>)` wrapper to its inner selector,
/// honouring nested parentheses such as `:not(:global(.md-code-block))`.
#[must_use]
pub fn unwrap_global_selectors(css: &str) -> String {
    const WRAPPER: &str = ":global(";
    let mut output = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(start) = rest.find(WRAPPER) {
        output.push_str(&rest[..start]);
        let inner = &rest[start + WRAPPER.len()..];
        let Some(close) = matching_close(inner) else {
            // An unterminated wrapper is left untouched so the defect stays visible.
            output.push_str(&rest[start..]);
            return output;
        };
        output.push_str(&inner[..close]);
        rest = &inner[close + 1..];
    }
    output.push_str(rest);
    output
}

/// Index of the `)` closing the wrapper whose contents start at `inner[0]`.
fn matching_close(inner: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (index, byte) in inner.bytes().enumerate() {
        match byte {
            b'(' => depth += 1,
            b')' if depth == 0 => return Some(index),
            b')' => depth -= 1,
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::unwrap_global_selectors;

    #[test]
    fn unwraps_leading_scoping_and_descendant_wrappers() {
        assert_eq!(
            unwrap_global_selectors(
                ":global([data-conversation-scroll]) .root {\n  flex: 0 0 auto;\n}"
            ),
            "[data-conversation-scroll] .root {\n  flex: 0 0 auto;\n}"
        );
        assert_eq!(
            unwrap_global_selectors(".markdown :global(.katex-display) { max-width: 100%; }"),
            ".markdown .katex-display { max-width: 100%; }"
        );
    }

    #[test]
    fn unwraps_wrappers_nested_inside_functional_pseudo_classes() {
        assert_eq!(
            unwrap_global_selectors(".markdown li > *:last-child:not(:global(.md-code-block)) {}"),
            ".markdown li > *:last-child:not(.md-code-block) {}"
        );
        assert_eq!(
            unwrap_global_selectors(
                ".kindSlot :global([role='tooltip']) {} .a :global(:is(.b, .c)) {}"
            ),
            ".kindSlot [role='tooltip'] {} .a :is(.b, .c) {}"
        );
    }

    #[test]
    fn leaves_plain_css_and_unterminated_wrappers_alone() {
        assert_eq!(
            unwrap_global_selectors(".root { color: red; }"),
            ".root { color: red; }"
        );
        assert_eq!(
            unwrap_global_selectors(".a :global(.b {}"),
            ".a :global(.b {}"
        );
    }
}
