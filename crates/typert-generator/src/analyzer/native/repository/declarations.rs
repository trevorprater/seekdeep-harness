//! Source declaration and public class projections through the compiler parser.

use serde::{Deserialize, Serialize};

use crate::Result;

use super::{
    super::{
        js::{self, Scope, Val},
        jstext,
    },
    RepositoryCompiler,
};

/// Declaration projection selected by a source-equivalence manifest entry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeclarationProjection {
    /// Complete source declaration, with export modifiers removed.
    #[default]
    Declaration,
    /// Body-stripped class declaration containing only public members.
    PublicApi,
}

impl DeclarationProjection {
    /// Manifest key used by the source verifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Declaration => "declaration",
            Self::PublicApi => "public-api",
        }
    }
}

/// One top-level named declaration and its source-faithful projections.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryDeclaration {
    /// Parser-normalized declared symbol name.
    pub symbol: String,
    /// Complete declaration with original attached `JSDoc`.
    pub declaration: String,
    /// Public ambient declaration for a class, absent for other declarations.
    pub public_api: Option<String>,
}

impl RepositoryDeclaration {
    /// Selects the requested projection, rejecting nonclass public APIs.
    pub fn projection(&self, projection: DeclarationProjection) -> Option<&str> {
        match projection {
            DeclarationProjection::Declaration => Some(&self.declaration),
            DeclarationProjection::PublicApi => self.public_api.as_deref(),
        }
    }
}

impl RepositoryCompiler {
    /// Reads the first interface, type alias, named class, or enum in a snippet.
    ///
    /// # Errors
    /// Returns compiler API failures. Parser recovery matches the source gate.
    pub fn block_symbol(&mut self, code: &str) -> Result<Option<String>> {
        self.compiler.run(|scope, ts| {
            let source = parse_source(scope, ts, "type-equiv.ts", code, false)?;
            for statement in js::get_items(scope, source, "statements")? {
                if named_declaration(scope, ts, statement)?
                    && let Some(name) = js::get_defined(scope, statement, "name")?
                {
                    return js::get_string(scope, name, "text").map(Some);
                }
            }
            Ok(None)
        })
    }

    /// Projects all top-level named declarations from one TypeScript source.
    ///
    /// # Errors
    /// Returns compiler API failures. The source verifier does not reject
    /// syntax recovery diagnostics before extracting declarations.
    pub fn declarations(
        &mut self,
        file_name: &str,
        code: &str,
    ) -> Result<Vec<RepositoryDeclaration>> {
        self.compiler.run(|scope, ts| {
            let source = parse_source(scope, ts, file_name, code, true)?;
            let mut declarations = Vec::new();
            for statement in js::get_items(scope, source, "statements")? {
                if !named_declaration(scope, ts, statement)? {
                    continue;
                }
                let Some(name) = js::get_defined(scope, statement, "name")? else {
                    continue;
                };
                let symbol = js::get_string(scope, name, "text")?;
                let start = node_start(scope, statement, source)?;
                let end = node_end(scope, statement)?;
                let declaration = strip_export(&jstext::utf16_slice(code, start, end));
                let documentation = source_jsdoc(scope, ts, code, statement)?;
                let declaration = with_jsdoc(&documentation, &declaration);
                let public_api = if predicate(scope, ts, "isClassDeclaration", statement)? {
                    Some(public_class(scope, ts, source, code, statement, &symbol)?)
                } else {
                    None
                };
                declarations.push(RepositoryDeclaration {
                    symbol,
                    declaration,
                    public_api,
                });
            }
            Ok(declarations)
        })
    }
}

fn parse_source<'s>(
    scope: &mut Scope<'s, '_>,
    ts: Val<'s>,
    file_name: &str,
    code: &str,
    parents: bool,
) -> Result<Val<'s>> {
    let file_name = js::string(scope, file_name);
    let code = js::string(scope, code);
    let target = js::get_path(scope, ts, "ScriptTarget.Latest")?;
    let parents = js::boolean(scope, parents);
    js::call(
        scope,
        ts,
        "createSourceFile",
        &[file_name, code, target, parents],
    )
}

fn named_declaration<'s>(
    scope: &mut Scope<'s, '_>,
    ts: Val<'s>,
    statement: Val<'s>,
) -> Result<bool> {
    for name in [
        "isInterfaceDeclaration",
        "isTypeAliasDeclaration",
        "isClassDeclaration",
        "isEnumDeclaration",
    ] {
        if predicate(scope, ts, name, statement)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn predicate<'s>(
    scope: &mut Scope<'s, '_>,
    ts: Val<'s>,
    predicate: &str,
    node: Val<'s>,
) -> Result<bool> {
    Ok(js::call(scope, ts, predicate, &[node])?.boolean_value(scope))
}

fn node_start<'s>(scope: &mut Scope<'s, '_>, node: Val<'s>, source: Val<'s>) -> Result<usize> {
    let start = js::call(scope, node, "getStart", &[source])?;
    Ok(js::offset(start.number_value(scope).ok_or_else(|| {
        js::failure("declaration start is not numeric")
    })?))
}

fn node_end<'s>(scope: &mut Scope<'s, '_>, node: Val<'s>) -> Result<usize> {
    Ok(js::offset(js::get_number(scope, node, "end")?))
}

fn source_jsdoc<'s>(
    scope: &mut Scope<'s, '_>,
    ts: Val<'s>,
    code: &str,
    node: Val<'s>,
) -> Result<String> {
    let comments = js::call(scope, ts, "getJSDocCommentsAndTags", &[node])?;
    let mut documentation = Vec::new();
    for comment in js::items(scope, comments)? {
        if predicate(scope, ts, "isJSDoc", comment)? {
            let start = js::offset(js::get_number(scope, comment, "pos")?);
            let end = node_end(scope, comment)?;
            documentation.push(jstext::utf16_slice(code, start, end));
        }
    }
    Ok(documentation.join("\n"))
}

fn with_jsdoc(documentation: &str, declaration: &str) -> String {
    if documentation.is_empty() {
        declaration.to_owned()
    } else {
        format!("{documentation}\n{declaration}")
    }
}

fn strip_export(code: &str) -> String {
    let Some(rest) = code.strip_prefix("export") else {
        return code.to_owned();
    };
    if !rest.starts_with(jstext::is_js_space) {
        return code.to_owned();
    }
    let rest = rest.trim_start_matches(jstext::is_js_space);
    if let Some(default) = rest.strip_prefix("default")
        && default.starts_with(jstext::is_js_space)
    {
        default.trim_start_matches(jstext::is_js_space).to_owned()
    } else {
        rest.to_owned()
    }
}

fn has_modifier<'s>(
    scope: &mut Scope<'s, '_>,
    ts: Val<'s>,
    node: Val<'s>,
    kind: &str,
) -> Result<bool> {
    if !predicate(scope, ts, "canHaveModifiers", node)? {
        return Ok(false);
    }
    let modifiers = js::call(scope, ts, "getModifiers", &[node])?;
    if modifiers.is_null_or_undefined() {
        return Ok(false);
    }
    let expected = js::get_path(scope, ts, &format!("SyntaxKind.{kind}"))?;
    for modifier in js::items(scope, modifiers)? {
        if js::get(scope, modifier, "kind")?.strict_equals(expected) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn public_member<'s>(scope: &mut Scope<'s, '_>, ts: Val<'s>, member: Val<'s>) -> Result<bool> {
    if predicate(scope, ts, "isClassStaticBlockDeclaration", member)? {
        return Ok(false);
    }
    let name = js::call(scope, ts, "getNameOfDeclaration", &[member])?;
    if !name.is_null_or_undefined() && predicate(scope, ts, "isPrivateIdentifier", name)? {
        return Ok(false);
    }
    Ok(!has_modifier(scope, ts, member, "PrivateKeyword")?
        && !has_modifier(scope, ts, member, "ProtectedKeyword")?)
}

fn bodyless_member<'s>(
    scope: &mut Scope<'s, '_>,
    ts: Val<'s>,
    source: Val<'s>,
    code: &str,
    member: Val<'s>,
) -> Result<String> {
    let start = node_start(scope, member, source)?;
    let mut end = node_end(scope, member)?;
    for name in [
        "isConstructorDeclaration",
        "isMethodDeclaration",
        "isGetAccessorDeclaration",
        "isSetAccessorDeclaration",
    ] {
        if predicate(scope, ts, name, member)? {
            if let Some(body) = js::get_defined(scope, member, "body")? {
                end = node_start(scope, body, source)?;
            }
            break;
        }
    }
    if predicate(scope, ts, "isPropertyDeclaration", member)?
        && let Some(initializer) = js::get_defined(scope, member, "initializer")?
    {
        end = node_start(scope, initializer, source)?;
    }
    let signature = jstext::utf16_slice(code, start, end);
    let signature = signature.trim_end_matches(jstext::is_js_space);
    let signature = signature.strip_suffix(';').unwrap_or(signature);
    let signature = signature.trim_end_matches(jstext::is_js_space);
    let signature = signature
        .strip_suffix('=')
        .unwrap_or(signature)
        .trim_end_matches(jstext::is_js_space);
    Ok(format!("{signature};"))
}

fn node_texts<'s>(
    scope: &mut Scope<'s, '_>,
    node: Val<'s>,
    field: &str,
    source: Val<'s>,
    separator: &str,
) -> Result<String> {
    js::get_items(scope, node, field)?
        .into_iter()
        .map(|value| {
            let text = js::call(scope, value, "getText", &[source])?;
            Ok(js::text(scope, text))
        })
        .collect::<Result<Vec<_>>>()
        .map(|texts| texts.join(separator))
}

fn public_class<'s>(
    scope: &mut Scope<'s, '_>,
    ts: Val<'s>,
    source: Val<'s>,
    code: &str,
    class: Val<'s>,
    symbol: &str,
) -> Result<String> {
    let abstract_keyword = if has_modifier(scope, ts, class, "AbstractKeyword")? {
        "abstract "
    } else {
        ""
    };
    let parameters = node_texts(scope, class, "typeParameters", source, ", ")?;
    let heritage = node_texts(scope, class, "heritageClauses", source, " ")?;
    let parameters = if parameters.is_empty() {
        parameters
    } else {
        format!("<{parameters}>")
    };
    let heritage = if heritage.is_empty() {
        heritage
    } else {
        format!(" {heritage}")
    };
    let mut lines = vec![format!(
        "declare {abstract_keyword}class {symbol}{parameters}{heritage} {{"
    )];
    for member in js::get_items(scope, class, "members")? {
        if !public_member(scope, ts, member)? {
            continue;
        }
        let documentation = source_jsdoc(scope, ts, code, member)?;
        let declaration = bodyless_member(scope, ts, source, code, member)?;
        let declaration = with_jsdoc(&documentation, &declaration);
        lines.extend(declaration.split('\n').map(|line| format!("  {line}")));
    }
    lines.push("}".to_owned());
    let documentation = source_jsdoc(scope, ts, code, class)?;
    Ok(with_jsdoc(&documentation, &lines.join("\n")))
}
