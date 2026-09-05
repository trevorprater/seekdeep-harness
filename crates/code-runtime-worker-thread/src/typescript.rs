//! Source-compatible, position-preserving TypeScript erasure in async-body context.

use std::sync::Arc;

use swc_common::{
    GLOBALS, Globals, SourceMap,
    errors::{DiagnosticBuilder, Emitter, HANDLER, Handler},
    sync::Lrc,
};
use swc_ecma_parser::TsSyntax;
use swc_ts_fast_strip::{Mode, Options};

const PREFIX: &str = "async function __seekdeep_program__() {\n";
const SUFFIX: &str = "\n}";

#[derive(Clone, Default)]
struct FirstDiagnostic(Arc<parking_lot::Mutex<Option<String>>>);

impl Emitter for FirstDiagnostic {
    fn emit(&mut self, diagnostic: &mut DiagnosticBuilder) {
        let mut first = self.0.lock();
        if first.is_none()
            && let Some(message) = diagnostic.message.first()
        {
            *first = Some(message.0.clone());
        }
    }

    fn take_diagnostics(&mut self) -> Vec<String> {
        Vec::new()
    }
}

/// Erases TypeScript with Node's strip-only semantics and source positions.
///
/// The returned code retains the async-function wrapper, so top-level `return`
/// and `await` remain valid. Comments, line endings, and surviving tokens retain
/// their positions; type syntax is replaced with source-width whitespace.
///
/// # Errors
///
/// Returns the first Node-compatible diagnostic for invalid or unsupported syntax.
pub fn strip_typescript(program: &str) -> anyhow::Result<String> {
    let source = format!("{PREFIX}{program}{SUFFIX}");
    let map = Lrc::new(SourceMap::default());
    let diagnostic = FirstDiagnostic::default();
    let handler = Handler::with_emitter(true, false, Box::new(diagnostic.clone()));
    let options = Options {
        mode: Mode::StripOnly,
        filename: Some(String::new()),
        parser: TsSyntax {
            decorators: true,
            ..TsSyntax::default()
        },
        deprecated_ts_module_as_error: Some(true),
        ..Options::default()
    };
    let result = GLOBALS.set(&Globals::new(), || {
        HANDLER.set(&handler, || {
            swc_ts_fast_strip::operate(&map, &handler, source, options)
        })
    });
    if handler.has_errors() {
        // Amaro exposes the first emitted message, not SWC's summary error.
        anyhow::bail!(
            diagnostic
                .0
                .lock()
                .take()
                .unwrap_or_else(|| "Syntax error".to_owned())
        );
    }
    result
        .map(|output| output.code)
        .map_err(|error| anyhow::anyhow!(error.message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_erasable_types_in_async_body_context() {
        let output = strip_typescript(
            "interface Point { x: number }; const p: Point = { x: 1 } as Point; return await Promise.resolve(p.x);",
        )
        .unwrap();
        assert!(output.starts_with(PREFIX));
        assert!(output.ends_with(SUFFIX));
        assert!(!output.contains("interface Point"));
        assert!(!output.contains(": Point"));
        assert!(output.contains("return await Promise.resolve(p.x)"));
    }

    #[test]
    fn rejects_nonerasable_and_invalid_syntax() {
        assert_eq!(
            strip_typescript("enum E { A }; return E.A")
                .unwrap_err()
                .to_string(),
            "TypeScript enum is not supported in strip-only mode"
        );
        assert_eq!(
            strip_typescript("return (").unwrap_err().to_string(),
            "Expression expected"
        );
    }
}
