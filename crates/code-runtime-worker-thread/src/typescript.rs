//! Evaluable-only TypeScript erasure in async-function-body context.

use std::path::Path;

use oxc_allocator::Allocator;
use oxc_ast::AstKind;
use oxc_ast_visit::Visit;
use oxc_codegen::Codegen;
use oxc_parser::Parser;
use oxc_semantic::SemanticBuilder;
use oxc_span::SourceType;
use oxc_transformer::{TransformOptions, Transformer};

const PREFIX: &str = "async function __seekdeep_program__() {\n";
const SUFFIX: &str = "\n}";

#[derive(Default)]
struct NonErasableSyntax {
    kind: Option<&'static str>,
}

impl<'a> Visit<'a> for NonErasableSyntax {
    fn enter_node(&mut self, kind: AstKind<'a>) {
        if self.kind.is_some() {
            return;
        }
        self.kind = match kind {
            AstKind::TSEnumDeclaration(_) => Some("enum"),
            AstKind::TSModuleDeclaration(_) => Some("namespace/module"),
            AstKind::TSImportEqualsDeclaration(_) => Some("import-equals"),
            AstKind::FormalParameter(parameter)
                if parameter.accessibility.is_some()
                    || parameter.readonly
                    || parameter.r#override =>
            {
                Some("parameter property")
            }
            _ => None,
        };
    }
}

/// Strips erasable TypeScript syntax while preserving the program's async
/// function-body grammar (`return` and top-level `await` remain valid).
///
/// # Errors
///
/// Returns a program diagnostic for parse, semantic, transform, or
/// non-erasable TypeScript syntax.
pub fn strip_typescript(program: &str) -> anyhow::Result<String> {
    let source = format!("{PREFIX}{program}{SUFFIX}");
    let allocator = Allocator::default();
    let source_type = SourceType::ts();
    let parsed = Parser::new(&allocator, &source, source_type).parse();
    if !parsed.errors.is_empty() {
        anyhow::bail!("TypeScript parse failed: {}", diagnostics(&parsed.errors));
    }
    let mut tree = parsed.program;
    let mut non_erasable = NonErasableSyntax::default();
    non_erasable.visit_program(&tree);
    if let Some(kind) = non_erasable.kind {
        anyhow::bail!("TypeScript strip rejected non-erasable {kind} syntax");
    }
    let semantic = SemanticBuilder::new()
        .with_excess_capacity(2.0)
        .build(&tree);
    if !semantic.errors.is_empty() {
        anyhow::bail!(
            "TypeScript semantic analysis failed: {}",
            diagnostics(&semantic.errors)
        );
    }
    let transformed = Transformer::new(
        &allocator,
        Path::new("seekdeep-program.ts"),
        &TransformOptions::default(),
    )
    .build_with_scoping(semantic.semantic.into_scoping(), &mut tree);
    if !transformed.errors.is_empty() {
        anyhow::bail!(
            "TypeScript strip failed: {}",
            diagnostics(&transformed.errors)
        );
    }
    Ok(Codegen::new().build(&tree).code)
}

fn diagnostics(errors: &[oxc_diagnostics::OxcDiagnostic]) -> String {
    errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
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
        assert!(output.contains("async function __seekdeep_program__"));
        assert!(!output.contains("interface Point"));
        assert!(!output.contains(": Point"));
        assert!(output.contains("return await Promise.resolve(p.x)"));
    }

    #[test]
    fn rejects_nonerasable_and_invalid_syntax() {
        let enum_error = strip_typescript("enum E { A }; return E.A").unwrap_err();
        assert!(format!("{enum_error:#}").contains("non-erasable enum"));
        assert!(strip_typescript("return (").is_err());
    }
}
