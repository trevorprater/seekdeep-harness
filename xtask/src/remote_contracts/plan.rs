use oxc_allocator::Allocator;
use oxc_ast::ast::{
    BindingPattern, Declaration, Expression, ObjectPropertyKind, PropertyKey, Statement,
    VariableDeclaration,
};
use oxc_parser::Parser;
use oxc_span::{GetSpan as _, SourceType};
use serde_json::{Value, json};

pub(super) fn compile(source: &str) -> anyhow::Result<Value> {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, SourceType::mjs()).parse();
    anyhow::ensure!(
        parsed.errors.is_empty(),
        "invalid Rust-emitted Remote JavaScript: {:?}",
        parsed.errors
    );
    let mut bindings = Vec::new();
    let mut result = None;
    for statement in &parsed.program.body {
        match statement {
            Statement::ImportDeclaration(import) => {
                anyhow::ensure!(import.source.value == "zod", "unexpected Remote dependency");
            }
            Statement::VariableDeclaration(value) => variables(value, &mut bindings)?,
            Statement::ExportNamedDeclaration(export) => {
                let Some(Declaration::VariableDeclaration(value)) = &export.declaration else {
                    anyhow::bail!("unsupported generated export")
                };
                variables(value, &mut bindings)?;
            }
            Statement::ExportDefaultDeclaration(export) => {
                result =
                    Some(expression(export.declaration.as_expression().ok_or_else(
                        || anyhow::anyhow!("default export must be a value"),
                    )?)?);
            }
            _ => anyhow::bail!("unsupported generated statement at {:?}", statement.span()),
        }
    }
    Ok(
        json!({"bindings":bindings,"result":result.ok_or_else(|| anyhow::anyhow!("missing default Remote export"))?}),
    )
}

fn variables(declaration: &VariableDeclaration<'_>, output: &mut Vec<Value>) -> anyhow::Result<()> {
    for variable in &declaration.declarations {
        let BindingPattern::BindingIdentifier(name) = &variable.id else {
            anyhow::bail!("generated binding must have an identifier")
        };
        output.push(json!([
            name.name.as_str(),
            expression(
                variable
                    .init
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("generated binding lacks a value"))?
            )?
        ]));
    }
    Ok(())
}

fn expression(value: &Expression<'_>) -> anyhow::Result<Value> {
    Ok(match value {
        Expression::StringLiteral(value) => json!(["literal", value.value.as_str()]),
        Expression::NumericLiteral(value) => json!(["literal", value.value]),
        Expression::BooleanLiteral(value) => json!(["literal", value.value]),
        Expression::NullLiteral(_) => json!(["literal", null]),
        Expression::Identifier(value) => json!(["name", value.name.as_str()]),
        Expression::ParenthesizedExpression(value) => return expression(&value.expression),
        Expression::ArrayExpression(value) => {
            let items =
                value
                    .elements
                    .iter()
                    .map(|item| {
                        expression(item.as_expression().ok_or_else(|| {
                            anyhow::anyhow!("generated array has a spread or hole")
                        })?)
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?;
            json!(["array", items])
        }
        Expression::ObjectExpression(value) => {
            let entries = value
                .properties
                .iter()
                .map(|property| {
                    let ObjectPropertyKind::ObjectProperty(property) = property else {
                        anyhow::bail!("generated object has a spread")
                    };
                    anyhow::ensure!(
                        !property.computed && !property.method,
                        "unsupported generated property"
                    );
                    let key = match &property.key {
                        PropertyKey::StaticIdentifier(value) => value.name.as_str(),
                        PropertyKey::StringLiteral(value) => value.value.as_str(),
                        _ => anyhow::bail!("unsupported generated property key"),
                    };
                    Ok(json!([key, expression(&property.value)?]))
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            json!(["object", entries])
        }
        Expression::StaticMemberExpression(value) => json!([
            "member",
            expression(&value.object)?,
            value.property.name.as_str()
        ]),
        Expression::CallExpression(value) => {
            let args = value
                .arguments
                .iter()
                .map(|arg| {
                    expression(
                        arg.as_expression()
                            .ok_or_else(|| anyhow::anyhow!("generated call has a spread"))?,
                    )
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            json!(["call", expression(&value.callee)?, args])
        }
        Expression::ArrowFunctionExpression(value) => {
            anyhow::ensure!(
                value.expression && !value.r#async && value.params.rest.is_none(),
                "unsupported generated function"
            );
            let names = value
                .params
                .items
                .iter()
                .map(|arg| match &arg.pattern {
                    BindingPattern::BindingIdentifier(value) => Ok(value.name.as_str()),
                    _ => anyhow::bail!("unsupported generated function parameter"),
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            let [Statement::ExpressionStatement(body)] = value.body.statements.as_slice() else {
                anyhow::bail!("generated arrow body is not an expression")
            };
            json!(["lambda", names, expression(&body.expression)?])
        }
        Expression::UnaryExpression(value) if value.operator.as_str() == "-" => {
            json!(["negate", expression(&value.argument)?])
        }
        _ => anyhow::bail!("unsupported generated expression at {:?}", value.span()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_schema_binding_order_and_lazy_lexical_references() -> anyhow::Result<()> {
        let result = compile(
            r#"import { z } from "zod";
const first = z.lazy(() => second);
const second = z.object({ value: z.number().min(-2), next: first.optional() });
export default { schemas: [first, second], optional: undefined };
"#,
        )?;
        assert_eq!(result["bindings"][0][0], "first");
        assert_eq!(result["bindings"][1][0], "second");
        assert_eq!(
            result["bindings"][0][1],
            json!([
                "call",
                ["member", ["name", "z"], "lazy"],
                [["lambda", [], ["name", "second"]]]
            ])
        );
        assert_eq!(
            result["result"],
            json!([
                "object",
                [
                    [
                        "schemas",
                        ["array", [["name", "first"], ["name", "second"]]]
                    ],
                    ["optional", ["name", "undefined"]]
                ]
            ])
        );
        Ok(())
    }

    #[test]
    fn rejects_construction_outside_the_emitter_grammar() {
        for source in [
            "import { z } from 'other'; export default z;",
            "export default [...values];",
            "export default { ...values };",
            "export default { [key]: value };",
            "export default async () => value;",
            "export default (...args) => args;",
            "export default () => { return value; };",
            "export default value ? one : two;",
            "const value = 1;",
        ] {
            assert!(compile(source).is_err(), "accepted {source}");
        }
    }
}
