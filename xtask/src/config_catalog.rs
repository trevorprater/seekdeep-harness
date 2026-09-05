//! Rust-owned plugin configuration catalog generator over pinned TypeScript sources.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
};

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Argument, ArrayExpressionElement, BindingPattern, Class, ClassElement, Declaration,
    ExportDefaultDeclarationKind, Expression, ImportDeclarationSpecifier, MethodDefinitionKind,
    ModuleExportName, Statement, TSLiteral, TSSignature, TSType, TSTypeName,
};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType, Span};
use walkdir::WalkDir;

const OUTPUT: &str = "docs/config-catalog.md";
const FENCE: &str = "ts config-catalog";

const GLOBAL_TYPES: &[&str] = &[
    "Array",
    "ReadonlyArray",
    "Record",
    "Partial",
    "Required",
    "Readonly",
    "Pick",
    "Omit",
    "Promise",
    "Map",
    "Set",
    "Date",
    "Error",
    "RegExp",
    "Exclude",
    "Extract",
    "NonNullable",
    "ReturnType",
    "Parameters",
    "AbortSignal",
    "URL",
    "Buffer",
    "NodeJS",
    "Iterable",
    "AsyncIterable",
];

/// How one package participates in declarative Loader configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogKind {
    /// Loadable plugin with a typed configuration parameter.
    Config,
    /// Loadable plugin with no configuration parameter.
    NoConfig,
    /// Abstract service class that requires a concrete provider.
    Seam,
    /// Package whose entry exports no plugin.
    Library,
}

/// One unresolved imported type used by a pasted declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeRef {
    /// Local source spelling.
    pub alias: String,
    /// Exported source spelling.
    pub imported: String,
    /// Module specifier.
    pub specifier: String,
}

/// One verbatim declaration plus its source pointer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paste {
    /// Leading `JSDoc` through the declaration's closing token.
    pub text: String,
    /// Repository-relative `file:line` pointer.
    pub source: String,
}

/// One package's configuration-catalog record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogEntry {
    /// Source package name.
    pub package: String,
    /// Repository-relative package directory.
    pub directory: String,
    /// Repository-relative entry path.
    pub entry: String,
    /// Loader classification.
    pub kind: CatalogKind,
    /// Required Cordis service keys.
    pub inject: Vec<String>,
    /// Default service class name.
    pub class_name: Option<String>,
    /// Config declaration name.
    pub config_type_name: Option<String>,
    /// Verbatim config closure.
    pub pastes: Option<Vec<Paste>>,
    /// External references left by the closure.
    pub refs: Option<Vec<TypeRef>>,
    /// Statically enumerable runtime schema paths; absent when no schema exists.
    pub schema_keys: Option<Vec<String>>,
    /// Workspace packages whose schemas are intersected.
    pub schema_composes: Vec<String>,
}

#[derive(Clone, Debug)]
struct ImportOrigin {
    imported: String,
    specifier: String,
}

#[derive(Clone, Debug)]
enum TypeExpr {
    Object(Vec<(String, TypeExpr)>),
    Array(Box<TypeExpr>),
    Reference {
        name: String,
        qualified: bool,
        arguments: Vec<TypeExpr>,
    },
    Intersection(Vec<TypeExpr>),
    Union(Vec<TypeExpr>),
    Indexed {
        object: Box<TypeExpr>,
        index: Option<String>,
    },
    Wrapped(Box<TypeExpr>),
    Unknown,
}

#[derive(Clone, Debug)]
enum TypeDeclKind {
    Interface {
        members: Vec<(String, TypeExpr)>,
        bases: Vec<TypeExpr>,
    },
    Alias(TypeExpr),
    Enum,
}

#[derive(Clone, Debug)]
struct TypeDecl {
    name: String,
    kind: TypeDeclKind,
    paste: Paste,
    missing_docs: Vec<String>,
}

#[derive(Clone, Debug)]
struct ConfigParameter {
    type_name: Option<String>,
    pointer: String,
}

#[derive(Clone, Debug)]
struct PluginShape {
    class_name: Option<String>,
    abstract_class: bool,
    config: Option<ConfigParameter>,
    inject: Vec<String>,
    schema_keys: Option<Vec<String>>,
    schema_composes: Vec<String>,
}

#[derive(Clone, Debug)]
struct ReExport {
    specifier: String,
    names: Option<Vec<(String, String)>>,
}

#[derive(Clone, Debug)]
struct FileModel {
    absolute: PathBuf,
    relative: PathBuf,
    imports: HashMap<String, ImportOrigin>,
    declarations: HashMap<String, TypeDecl>,
    reexports: Vec<ReExport>,
    plugin: Option<PluginShape>,
}

#[derive(Clone, Debug)]
enum PathStep {
    Member(String),
    Array,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PathLookup {
    Found,
    Missing,
    Unknown,
}

struct World {
    scan_root: PathBuf,
    files: HashMap<PathBuf, FileModel>,
    package_dirs: HashMap<String, PathBuf>,
}

/// Generates or verifies the checked-in catalog.
///
/// # Errors
///
/// Returns parse, classification, type-resolution, schema, rendering, I/O, or freshness failures.
pub fn run(repo_root: &Path, source_root: &Path, check: bool) -> anyhow::Result<()> {
    let entries = collect_config_catalog(source_root)?;
    let count = entries.len();
    let content = render(&entries);
    let path = repo_root.join(OUTPUT);
    if check {
        let current = std::fs::read_to_string(&path).ok();
        anyhow::ensure!(
            current.as_deref() == Some(content.as_str()),
            "gen-config-catalog: {OUTPUT} is stale. Run `cargo xtask config-catalog` and commit {OUTPUT}."
        );
        println!("gen-config-catalog: {OUTPUT} is up to date ({count} packages).");
        return Ok(());
    }
    std::fs::write(path, content)?;
    println!("gen-config-catalog: wrote {OUTPUT} ({count} packages).");
    Ok(())
}

fn slash(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn pointer(relative: &Path, source: &str, span: Span) -> String {
    let start = usize::try_from(span.start)
        .unwrap_or(usize::MAX)
        .min(source.len());
    let line = source[..start]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1;
    format!("{}:{line}", slash(relative))
}

fn leading_jsdoc(source: &str, node_start: usize) -> Option<(usize, &str)> {
    let prefix = source.get(..node_start)?;
    let start = prefix.rfind("/**")?;
    let tail = source.get(start..node_start)?;
    let close = tail.rfind("*/")? + 2;
    tail.get(close..)?
        .trim()
        .is_empty()
        .then(|| (start, &tail[..close]))
}

fn jsdoc_prose(raw: Option<&str>) -> String {
    let Some(raw) = raw else {
        return String::new();
    };
    raw.strip_prefix("/**")
        .and_then(|body| body.strip_suffix("*/"))
        .unwrap_or(raw)
        .lines()
        .map(str::trim)
        .map(|line| line.strip_prefix('*').map_or(line, str::trim))
        .take_while(|line| !line.starts_with('@'))
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn declaration_text(source: &str, span: Span) -> String {
    let node_start = usize::try_from(span.start)
        .unwrap_or(usize::MAX)
        .min(source.len());
    let end = usize::try_from(span.end)
        .unwrap_or(source.len())
        .min(source.len());
    let start = leading_jsdoc(source, node_start).map_or(node_start, |(start, _)| start);
    source[start..end].trim_end().to_owned()
}

fn module_name(name: &ModuleExportName<'_>) -> String {
    match name {
        ModuleExportName::IdentifierName(name) => name.name.to_string(),
        ModuleExportName::IdentifierReference(name) => name.name.to_string(),
        ModuleExportName::StringLiteral(name) => name.value.to_string(),
    }
}

fn type_head(name: &TSTypeName<'_>) -> Option<(String, bool)> {
    match name {
        TSTypeName::IdentifierReference(name) => Some((name.name.to_string(), false)),
        TSTypeName::QualifiedName(name) => type_head(&name.left).map(|(name, _)| (name, true)),
        TSTypeName::ThisExpression(_) => None,
    }
}

fn literal_string(value: &TSType<'_>) -> Option<String> {
    let TSType::TSLiteralType(literal) = value else {
        return None;
    };
    match &literal.literal {
        TSLiteral::StringLiteral(value) => Some(value.value.to_string()),
        _ => None,
    }
}

fn parse_members(
    source: &str,
    relative: &Path,
    members: &[TSSignature<'_>],
    path: &str,
    missing_docs: &mut Vec<String>,
) -> Vec<(String, TypeExpr)> {
    let mut parsed = Vec::new();
    for member in members {
        let TSSignature::TSPropertySignature(property) = member else {
            continue;
        };
        let Some(name) = property.key.static_name().map(std::borrow::Cow::into_owned) else {
            continue;
        };
        let member_path = format!("{path}.{name}");
        let start = usize::try_from(member.span().start).unwrap_or(0);
        if jsdoc_prose(leading_jsdoc(source, start).map(|(_, raw)| raw)).is_empty() {
            missing_docs.push(format!(
                "config field '{member_path}' ({}) has no JSDoc prose.",
                pointer(relative, source, member.span())
            ));
        }
        let value = property
            .type_annotation
            .as_ref()
            .map_or(TypeExpr::Unknown, |annotation| {
                parse_type_expr(
                    source,
                    relative,
                    &annotation.type_annotation,
                    &member_path,
                    missing_docs,
                )
            });
        parsed.push((name, value));
    }
    parsed
}

fn parse_type_expr(
    source: &str,
    relative: &Path,
    value: &TSType<'_>,
    path: &str,
    missing_docs: &mut Vec<String>,
) -> TypeExpr {
    match value {
        TSType::TSArrayType(array) => TypeExpr::Array(Box::new(parse_type_expr(
            source,
            relative,
            &array.element_type,
            path,
            missing_docs,
        ))),
        TSType::TSTypeLiteral(literal) => TypeExpr::Object(parse_members(
            source,
            relative,
            &literal.members,
            path,
            missing_docs,
        )),
        TSType::TSTypeReference(reference) => {
            let Some((name, qualified)) = type_head(&reference.type_name) else {
                return TypeExpr::Unknown;
            };
            let arguments = reference
                .type_arguments
                .as_ref()
                .map(|arguments| {
                    arguments
                        .params
                        .iter()
                        .map(|argument| {
                            parse_type_expr(source, relative, argument, path, missing_docs)
                        })
                        .collect()
                })
                .unwrap_or_default();
            TypeExpr::Reference {
                name,
                qualified,
                arguments,
            }
        }
        TSType::TSIntersectionType(intersection) => TypeExpr::Intersection(
            intersection
                .types
                .iter()
                .map(|value| parse_type_expr(source, relative, value, path, missing_docs))
                .collect(),
        ),
        TSType::TSUnionType(union) => TypeExpr::Union(
            union
                .types
                .iter()
                .map(|value| parse_type_expr(source, relative, value, path, missing_docs))
                .collect(),
        ),
        TSType::TSIndexedAccessType(indexed) => TypeExpr::Indexed {
            object: Box::new(parse_type_expr(
                source,
                relative,
                &indexed.object_type,
                path,
                missing_docs,
            )),
            index: literal_string(&indexed.index_type),
        },
        TSType::TSParenthesizedType(parenthesized) => TypeExpr::Wrapped(Box::new(parse_type_expr(
            source,
            relative,
            &parenthesized.type_annotation,
            path,
            missing_docs,
        ))),
        TSType::TSTypeOperatorType(operator) => TypeExpr::Wrapped(Box::new(parse_type_expr(
            source,
            relative,
            &operator.type_annotation,
            path,
            missing_docs,
        ))),
        _ => TypeExpr::Unknown,
    }
}

fn interface_decl(
    source: &str,
    relative: &Path,
    declaration: &oxc_ast::ast::TSInterfaceDeclaration<'_>,
    span: Span,
) -> TypeDecl {
    let name = declaration.id.name.to_string();
    let mut missing_docs = Vec::new();
    let members = parse_members(
        source,
        relative,
        &declaration.body.body,
        &name,
        &mut missing_docs,
    );
    let bases = declaration
        .extends
        .iter()
        .map(|base| match &base.expression {
            Expression::Identifier(identifier) => TypeExpr::Reference {
                name: identifier.name.to_string(),
                qualified: false,
                arguments: Vec::new(),
            },
            _ => TypeExpr::Unknown,
        })
        .collect();
    TypeDecl {
        name,
        kind: TypeDeclKind::Interface { members, bases },
        paste: Paste {
            text: declaration_text(source, span),
            source: pointer(relative, source, span),
        },
        missing_docs,
    }
}

fn alias_decl(
    source: &str,
    relative: &Path,
    declaration: &oxc_ast::ast::TSTypeAliasDeclaration<'_>,
    span: Span,
) -> TypeDecl {
    let name = declaration.id.name.to_string();
    let mut missing_docs = Vec::new();
    let value = parse_type_expr(
        source,
        relative,
        &declaration.type_annotation,
        &name,
        &mut missing_docs,
    );
    TypeDecl {
        name,
        kind: TypeDeclKind::Alias(value),
        paste: Paste {
            text: declaration_text(source, span),
            source: pointer(relative, source, span),
        },
        missing_docs,
    }
}

fn enum_decl(
    source: &str,
    relative: &Path,
    declaration: &oxc_ast::ast::TSEnumDeclaration<'_>,
    span: Span,
) -> TypeDecl {
    TypeDecl {
        name: declaration.id.name.to_string(),
        kind: TypeDeclKind::Enum,
        paste: Paste {
            text: declaration_text(source, span),
            source: pointer(relative, source, span),
        },
        missing_docs: Vec::new(),
    }
}

fn type_decl(
    source: &str,
    relative: &Path,
    declaration: &Declaration<'_>,
    span: Span,
) -> Option<TypeDecl> {
    match declaration {
        Declaration::TSInterfaceDeclaration(declaration) => {
            Some(interface_decl(source, relative, declaration, span))
        }
        Declaration::TSTypeAliasDeclaration(declaration) => {
            Some(alias_decl(source, relative, declaration, span))
        }
        Declaration::TSEnumDeclaration(declaration) => {
            Some(enum_decl(source, relative, declaration, span))
        }
        _ => None,
    }
}

fn parameter(
    function: &oxc_ast::ast::Function<'_>,
    relative: &Path,
    source: &str,
) -> Option<ConfigParameter> {
    let parameter = function.params.items.get(1)?;
    let pointer = pointer(relative, source, parameter.span);
    let type_name = parameter.type_annotation.as_ref().and_then(|annotation| {
        match &annotation.type_annotation {
            TSType::TSTypeReference(reference) => match &reference.type_name {
                TSTypeName::IdentifierReference(identifier) => Some(identifier.name.to_string()),
                _ => None,
            },
            _ => None,
        }
    });
    Some(ConfigParameter { type_name, pointer })
}

fn unwrap_expression<'a, 'b>(mut expression: &'b Expression<'a>) -> &'b Expression<'a> {
    loop {
        expression = match expression {
            Expression::TSAsExpression(value) => &value.expression,
            Expression::TSSatisfiesExpression(value) => &value.expression,
            Expression::ParenthesizedExpression(value) => &value.expression,
            Expression::TSNonNullExpression(value) => &value.expression,
            _ => return expression,
        };
    }
}

fn argument_expression<'a, 'b>(argument: &'b Argument<'a>) -> Option<&'b Expression<'a>> {
    argument.as_expression()
}

fn array_expression<'a, 'b>(element: &'b ArrayExpressionElement<'a>) -> Option<&'b Expression<'a>> {
    element.as_expression()
}

fn schema_call<'a, 'b>(
    expression: &'b Expression<'a>,
) -> Option<(
    String,
    &'b oxc_ast::ast::CallExpression<'a>,
    &'b Expression<'a>,
)> {
    let Expression::CallExpression(call) = unwrap_expression(expression) else {
        return None;
    };
    let Expression::StaticMemberExpression(member) = unwrap_expression(&call.callee) else {
        return None;
    };
    Some((member.property.name.to_string(), call, &member.object))
}

fn collect_nested_schema_paths(expression: &Expression<'_>, base: &str, keys: &mut Vec<String>) {
    let Some((method, call, callee_base)) = schema_call(expression) else {
        return;
    };
    match method.as_str() {
        "object" => {
            let Some(Expression::ObjectExpression(object)) = call
                .arguments
                .first()
                .and_then(argument_expression)
                .map(unwrap_expression)
            else {
                return;
            };
            for property in &object.properties {
                let Some(property) = property.as_property() else {
                    continue;
                };
                let Some(name) = property.key.static_name() else {
                    continue;
                };
                let path = format!("{base}.{name}");
                keys.push(path.clone());
                collect_nested_schema_paths(&property.value, &path, keys);
            }
        }
        "array" => {
            if let Some(value) = call.arguments.first().and_then(argument_expression) {
                collect_nested_schema_paths(value, &format!("{base}[]"), keys);
            }
        }
        "union" => {
            let Some(Expression::ArrayExpression(array)) = call
                .arguments
                .first()
                .and_then(argument_expression)
                .map(unwrap_expression)
            else {
                return;
            };
            for value in &array.elements {
                if let Some(value) = array_expression(value) {
                    collect_nested_schema_paths(value, base, keys);
                }
            }
        }
        _ => {
            if matches!(
                unwrap_expression(callee_base),
                Expression::CallExpression(_)
            ) {
                collect_nested_schema_paths(callee_base, base, keys);
            }
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the closed Schemastery call projection is one recursive walk"
)]
fn schema_info(
    expression: &Expression<'_>,
    imports: &HashMap<String, ImportOrigin>,
    where_: &str,
    violations: &mut Vec<String>,
) -> (Vec<String>, Vec<String>) {
    fn visit(
        expression: &Expression<'_>,
        imports: &HashMap<String, ImportOrigin>,
        where_: &str,
        keys: &mut Vec<String>,
        composes: &mut Vec<String>,
        violations: &mut Vec<String>,
    ) {
        let Some((method, call, callee_base)) = schema_call(expression) else {
            violations.push(format!(
                "{where_}: schema is not a walkable call expression."
            ));
            return;
        };
        match method.as_str() {
            "object" => {
                let Some(Expression::ObjectExpression(object)) = call
                    .arguments
                    .first()
                    .and_then(argument_expression)
                    .map(unwrap_expression)
                else {
                    violations.push(format!("{where_}: object schema has no object literal."));
                    return;
                };
                for property in &object.properties {
                    let Some(property) = property.as_property() else {
                        continue;
                    };
                    let Some(name) = property.key.static_name() else {
                        continue;
                    };
                    keys.push(name.to_string());
                    collect_nested_schema_paths(&property.value, &name, keys);
                }
            }
            "intersect" => {
                let Some(Expression::ArrayExpression(array)) = call
                    .arguments
                    .first()
                    .and_then(argument_expression)
                    .map(unwrap_expression)
                else {
                    violations.push(format!("{where_}: intersect schema has no array literal."));
                    return;
                };
                for value in &array.elements {
                    let Some(value) = array_expression(value).map(unwrap_expression) else {
                        continue;
                    };
                    if matches!(value, Expression::CallExpression(_)) {
                        visit(value, imports, where_, keys, composes, violations);
                        continue;
                    }
                    let Expression::StaticMemberExpression(member) = value else {
                        violations.push(format!(
                            "{where_}: intersect member is not a schema call or imported Config."
                        ));
                        continue;
                    };
                    let Expression::Identifier(identifier) = unwrap_expression(&member.object) else {
                        violations.push(format!(
                            "{where_}: intersect Config owner is not an imported identifier."
                        ));
                        continue;
                    };
                    if member.property.name != "Config" {
                        violations.push(format!(
                            "{where_}: intersect member does not select Config."
                        ));
                        continue;
                    }
                    let Some(origin) = imports.get(identifier.name.as_str()) else {
                        violations.push(format!(
                            "{where_}: intersect owner '{}' is not imported.",
                            identifier.name
                        ));
                        continue;
                    };
                    composes.push(origin.specifier.clone());
                }
            }
            "union" => {
                let Some(Expression::ArrayExpression(array)) = call
                    .arguments
                    .first()
                    .and_then(argument_expression)
                    .map(unwrap_expression)
                else {
                    return;
                };
                for value in &array.elements {
                    if let Some(value) = array_expression(value) {
                        visit(value, imports, where_, keys, composes, violations);
                    }
                }
            }
            _ if matches!(unwrap_expression(callee_base), Expression::CallExpression(_)) => {
                visit(callee_base, imports, where_, keys, composes, violations);
            }
            _ => violations.push(format!(
                "{where_}: schema call '{method}' is not object/intersect and hangs off no walkable base call."
            )),
        }
    }
    let mut keys = Vec::new();
    let mut composes = Vec::new();
    visit(
        expression,
        imports,
        where_,
        &mut keys,
        &mut composes,
        violations,
    );
    (keys, composes)
}

fn import_map(program: &oxc_ast::ast::Program<'_>) -> HashMap<String, ImportOrigin> {
    let mut imports = HashMap::new();
    for statement in &program.body {
        let Statement::ImportDeclaration(import) = statement else {
            continue;
        };
        let specifier = import.source.value.to_string();
        for binding in import.specifiers.iter().flatten() {
            let (local, imported) = match binding {
                ImportDeclarationSpecifier::ImportSpecifier(binding) => (
                    binding.local.name.to_string(),
                    module_name(&binding.imported),
                ),
                ImportDeclarationSpecifier::ImportDefaultSpecifier(binding) => {
                    (binding.local.name.to_string(), "default".to_owned())
                }
                ImportDeclarationSpecifier::ImportNamespaceSpecifier(binding) => {
                    (binding.local.name.to_string(), "*".to_owned())
                }
            };
            imports.insert(
                local,
                ImportOrigin {
                    imported,
                    specifier: specifier.clone(),
                },
            );
        }
    }
    imports
}

fn variable_name(declaration: &oxc_ast::ast::VariableDeclarator<'_>) -> Option<String> {
    match &declaration.id {
        BindingPattern::BindingIdentifier(identifier) => Some(identifier.name.to_string()),
        _ => None,
    }
}

fn array_strings(expression: &Expression<'_>) -> Option<Vec<String>> {
    let Expression::ArrayExpression(array) = unwrap_expression(expression) else {
        return None;
    };
    Some(
        array
            .elements
            .iter()
            .filter_map(array_expression)
            .map(|value| match unwrap_expression(value) {
                Expression::StringLiteral(value) => value.value.to_string(),
                other => format!("{:?}", other.span()),
            })
            .collect(),
    )
}

fn class_shape(
    class: &Class<'_>,
    relative: &Path,
    source: &str,
    imports: &HashMap<String, ImportOrigin>,
    violations: &mut Vec<String>,
) -> PluginShape {
    let mut config = None;
    let mut inject = Vec::new();
    let mut schema_keys = None;
    let mut schema_composes = Vec::new();
    for member in &class.body.body {
        match member {
            ClassElement::MethodDefinition(method)
                if method.kind == MethodDefinitionKind::Constructor =>
            {
                config = parameter(&method.value, relative, source);
            }
            ClassElement::PropertyDefinition(property) if property.r#static => {
                let Some(name) = property.key.static_name() else {
                    continue;
                };
                let Some(value) = property.value.as_ref() else {
                    continue;
                };
                match name.as_ref() {
                    "inject" => match array_strings(value) {
                        Some(values) => inject = values,
                        None => violations.push(format!(
                            "{}: inject is not a plain string-array literal; teach the generator the new declaration form.",
                            slash(relative)
                        )),
                    },
                    "Config" => {
                        if !matches!(unwrap_expression(value), Expression::Identifier(identifier) if identifier.name == "Config")
                        {
                            let (keys, composes) = schema_info(
                                value,
                                imports,
                                &format!("{} ({})", class.id.as_ref().map_or("class", |id| id.name.as_str()), slash(relative)),
                                violations,
                            );
                            schema_keys = Some(keys);
                            schema_composes = composes;
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    PluginShape {
        class_name: class.id.as_ref().map(|id| id.name.to_string()),
        abstract_class: class.r#abstract,
        config,
        inject,
        schema_keys,
        schema_composes,
    }
}

fn function_shape(
    function: &oxc_ast::ast::Function<'_>,
    relative: &Path,
    source: &str,
) -> PluginShape {
    PluginShape {
        class_name: None,
        abstract_class: false,
        config: parameter(function, relative, source),
        inject: Vec::new(),
        schema_keys: None,
        schema_composes: Vec::new(),
    }
}

fn merge_namespace_fields(
    mut shape: PluginShape,
    inject: &[String],
    schema: Option<&(Vec<String>, Vec<String>)>,
) -> PluginShape {
    if shape.inject.is_empty() {
        shape.inject = inject.to_vec();
    }
    if shape.schema_keys.is_none()
        && let Some((keys, composes)) = schema
    {
        shape.schema_keys = Some(keys.clone());
        shape.schema_composes.clone_from(composes);
    }
    shape
}

#[allow(
    clippy::too_many_lines,
    reason = "one OXC program is projected into one closed owned file model"
)]
fn parse_file(
    absolute: PathBuf,
    relative: PathBuf,
    source: &str,
    violations: &mut Vec<String>,
) -> anyhow::Result<FileModel> {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, SourceType::ts()).parse();
    anyhow::ensure!(
        parsed.errors.is_empty(),
        "gen-config-catalog: failed to parse {}: {}",
        relative.display(),
        parsed
            .errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    );
    let program = parsed.program;
    let imports = import_map(&program);
    let mut declarations = HashMap::new();
    let mut reexports = Vec::new();
    let mut local_classes = HashMap::<String, PluginShape>::new();
    let mut local_functions = HashMap::<String, PluginShape>::new();
    let mut default_direct = None;
    let mut default_name = None;
    let mut apply_export = None;
    let mut namespace_inject = Vec::new();
    let mut namespace_schema = None;

    for statement in &program.body {
        let (declaration, declaration_span, exported) = match statement {
            Statement::ExportNamedDeclaration(export) => {
                if let Some(specifier) = &export.source {
                    let names = (!export.specifiers.is_empty()).then(|| {
                        export
                            .specifiers
                            .iter()
                            .map(|entry| (module_name(&entry.exported), module_name(&entry.local)))
                            .collect()
                    });
                    reexports.push(ReExport {
                        specifier: specifier.value.to_string(),
                        names,
                    });
                }
                let Some(declaration) = export.declaration.as_ref() else {
                    continue;
                };
                (declaration, export.span, true)
            }
            Statement::ExportAllDeclaration(export) => {
                reexports.push(ReExport {
                    specifier: export.source.value.to_string(),
                    names: None,
                });
                continue;
            }
            Statement::VariableDeclaration(declaration) => {
                for item in &declaration.declarations {
                    if variable_name(item).as_deref() == Some("inject")
                        && let Some(value) = &item.init
                    {
                        namespace_inject = array_strings(value).unwrap_or_else(|| {
                            violations.push(format!(
                                "{}: inject is not a plain string-array literal; teach the generator the new declaration form.",
                                slash(&relative)
                            ));
                            Vec::new()
                        });
                    }
                }
                continue;
            }
            Statement::FunctionDeclaration(function) => {
                if let Some(name) = &function.id {
                    local_functions.insert(
                        name.name.to_string(),
                        function_shape(function, &relative, source),
                    );
                }
                continue;
            }
            Statement::ClassDeclaration(class) => {
                if let Some(name) = &class.id {
                    local_classes.insert(
                        name.name.to_string(),
                        class_shape(class, &relative, source, &imports, violations),
                    );
                }
                continue;
            }
            Statement::TSTypeAliasDeclaration(declaration) => {
                let value = alias_decl(source, &relative, declaration, declaration.span);
                declarations.insert(value.name.clone(), value);
                continue;
            }
            Statement::TSInterfaceDeclaration(declaration) => {
                let value = interface_decl(source, &relative, declaration, declaration.span);
                declarations.insert(value.name.clone(), value);
                continue;
            }
            Statement::TSEnumDeclaration(declaration) => {
                let value = enum_decl(source, &relative, declaration, declaration.span);
                declarations.insert(value.name.clone(), value);
                continue;
            }
            Statement::ExportDefaultDeclaration(export) => {
                match &export.declaration {
                    ExportDefaultDeclarationKind::ClassDeclaration(class) => {
                        default_direct =
                            Some(class_shape(class, &relative, source, &imports, violations));
                    }
                    ExportDefaultDeclarationKind::FunctionDeclaration(function) => {
                        default_direct = Some(function_shape(function, &relative, source));
                    }
                    value => {
                        if let Some(Expression::Identifier(identifier)) = value.as_expression() {
                            default_name = Some(identifier.name.to_string());
                        }
                    }
                }
                continue;
            }
            _ => continue,
        };

        if let Some(value) = type_decl(source, &relative, declaration, declaration_span) {
            declarations.insert(value.name.clone(), value);
        }
        match declaration {
            Declaration::FunctionDeclaration(function) => {
                let shape = function_shape(function, &relative, source);
                if let Some(name) = &function.id {
                    local_functions.insert(name.name.to_string(), shape.clone());
                    if exported && name.name == "apply" {
                        apply_export = Some(shape);
                    }
                }
            }
            Declaration::ClassDeclaration(class) => {
                if let Some(name) = &class.id {
                    local_classes.insert(
                        name.name.to_string(),
                        class_shape(class, &relative, source, &imports, violations),
                    );
                }
            }
            Declaration::VariableDeclaration(variable) => {
                for item in &variable.declarations {
                    let Some(name) = variable_name(item) else {
                        continue;
                    };
                    let Some(value) = &item.init else {
                        continue;
                    };
                    if name == "inject" {
                        namespace_inject = array_strings(value).unwrap_or_else(|| {
                            violations.push(format!(
                                "{}: inject is not a plain string-array literal; teach the generator the new declaration form.",
                                slash(&relative)
                            ));
                            Vec::new()
                        });
                    } else if exported && name == "Config" {
                        namespace_schema = Some(schema_info(
                            value,
                            &imports,
                            &format!("Config ({})", slash(&relative)),
                            violations,
                        ));
                    }
                }
            }
            _ => {}
        }
    }

    let mut plugin = default_direct.or_else(|| {
        default_name.as_ref().and_then(|name| {
            local_classes
                .get(name)
                .or_else(|| local_functions.get(name))
                .cloned()
        })
    });
    if plugin.is_none() {
        plugin = apply_export;
    }
    plugin = plugin
        .map(|plugin| merge_namespace_fields(plugin, &namespace_inject, namespace_schema.as_ref()));
    Ok(FileModel {
        absolute,
        relative,
        imports,
        declarations,
        reexports,
        plugin,
    })
}

#[allow(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "the source convention requires the literal lowercase .ts extension"
)]
impl World {
    fn new(scan_root: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            scan_root: std::fs::canonicalize(scan_root)?,
            files: HashMap::new(),
            package_dirs: HashMap::new(),
        })
    }

    fn load(&mut self, absolute: &Path, violations: &mut Vec<String>) -> anyhow::Result<FileModel> {
        let absolute = std::fs::canonicalize(absolute)?;
        if let Some(model) = self.files.get(&absolute) {
            return Ok(model.clone());
        }
        let relative = absolute.strip_prefix(&self.scan_root)?.to_path_buf();
        let source = std::fs::read_to_string(&absolute)?;
        let model = parse_file(absolute.clone(), relative, &source, violations)?;
        self.files.insert(absolute, model.clone());
        Ok(model)
    }

    fn relative_target(
        &mut self,
        from: &FileModel,
        specifier: &str,
        violations: &mut Vec<String>,
    ) -> anyhow::Result<FileModel> {
        self.load(
            &from
                .absolute
                .parent()
                .ok_or_else(|| anyhow::anyhow!("source file has no parent"))?
                .join(specifier),
            violations,
        )
    }

    fn find_exported(
        &mut self,
        file: &FileModel,
        name: &str,
        seen: &mut HashSet<String>,
        violations: &mut Vec<String>,
    ) -> anyhow::Result<Option<(TypeDecl, FileModel)>> {
        let key = format!("{}#{name}", file.absolute.display());
        if !seen.insert(key) {
            return Ok(None);
        }
        if let Some(declaration) = file.declarations.get(name) {
            return Ok(Some((declaration.clone(), file.clone())));
        }
        for export in &file.reexports {
            if !export.specifier.starts_with('.') || !export.specifier.ends_with(".ts") {
                continue;
            }
            let look_for = match &export.names {
                None => name,
                Some(names) => {
                    let Some((_, local)) = names.iter().find(|(exported, _)| exported == name)
                    else {
                        continue;
                    };
                    local
                }
            };
            let target = self.relative_target(file, &export.specifier, violations)?;
            if let Some(found) = self.find_exported(&target, look_for, seen, violations)? {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }

    fn resolve_type(
        &mut self,
        file: &FileModel,
        name: &str,
        violations: &mut Vec<String>,
    ) -> anyhow::Result<ResolvedType> {
        if let Some(declaration) = file.declarations.get(name) {
            return Ok(ResolvedType::Declaration(
                declaration.clone(),
                Box::new(file.clone()),
            ));
        }
        let Some(import) = file.imports.get(name) else {
            return Ok(ResolvedType::Missing);
        };
        if import.specifier.starts_with('.') {
            if !import.specifier.ends_with(".ts") {
                violations.push(format!(
                    "{}: relative import '{}' lacks the explicit .ts extension the repo convention requires.",
                    slash(&file.relative),
                    import.specifier
                ));
                return Ok(ResolvedType::Missing);
            }
            if import.imported != name {
                violations.push(format!(
                    "{}: '{}' aliases '{}' across a package-local import; the catalog pastes declarations verbatim, so keep package-local config types unaliased.",
                    slash(&file.relative),
                    name,
                    import.imported
                ));
                return Ok(ResolvedType::Missing);
            }
            let target = self.relative_target(file, &import.specifier, violations)?;
            return self.resolve_type(&target, &import.imported, violations);
        }
        Ok(ResolvedType::Reference(TypeRef {
            alias: name.to_owned(),
            imported: import.imported.clone(),
            specifier: import.specifier.clone(),
        }))
    }

    fn declaration_for_reference(
        &mut self,
        file: &FileModel,
        name: &str,
        violations: &mut Vec<String>,
    ) -> anyhow::Result<Option<(TypeDecl, FileModel)>> {
        if let Some(declaration) = file.declarations.get(name) {
            return Ok(Some((declaration.clone(), file.clone())));
        }
        let Some(import) = file.imports.get(name) else {
            return Ok(None);
        };
        if import.specifier.starts_with('.') {
            if !import.specifier.ends_with(".ts") {
                return Ok(None);
            }
            let target = self.relative_target(file, &import.specifier, violations)?;
            return self.find_exported(&target, &import.imported, &mut HashSet::new(), violations);
        }
        let Some(directory) = self.package_dirs.get(&import.specifier).cloned() else {
            return Ok(None);
        };
        let entry = self.load(
            &self.scan_root.join(directory).join("src/index.ts"),
            violations,
        )?;
        self.find_exported(&entry, &import.imported, &mut HashSet::new(), violations)
    }
}

enum ResolvedType {
    Declaration(TypeDecl, Box<FileModel>),
    Reference(TypeRef),
    Missing,
}

fn referenced_names(expression: &TypeExpr, output: &mut Vec<String>) {
    match expression {
        TypeExpr::Object(members) => {
            for (_, value) in members {
                referenced_names(value, output);
            }
        }
        TypeExpr::Array(value) | TypeExpr::Wrapped(value) => referenced_names(value, output),
        TypeExpr::Reference {
            name, arguments, ..
        } => {
            output.push(name.clone());
            for argument in arguments {
                referenced_names(argument, output);
            }
        }
        TypeExpr::Intersection(values) | TypeExpr::Union(values) => {
            for value in values {
                referenced_names(value, output);
            }
        }
        TypeExpr::Indexed { object, .. } => referenced_names(object, output),
        TypeExpr::Unknown => {}
    }
}

fn declaration_references(declaration: &TypeDecl) -> Vec<String> {
    let mut names = Vec::new();
    match &declaration.kind {
        TypeDeclKind::Interface { members, bases } => {
            for base in bases {
                referenced_names(base, &mut names);
            }
            for (_, value) in members {
                referenced_names(value, &mut names);
            }
        }
        TypeDeclKind::Alias(value) => referenced_names(value, &mut names),
        TypeDeclKind::Enum => {}
    }
    names
}

fn parse_path(path: &str) -> Vec<PathStep> {
    let mut steps = Vec::new();
    for segment in path.split('.') {
        let mut member = segment;
        let mut arrays = 0;
        while let Some(stripped) = member.strip_suffix("[]") {
            member = stripped;
            arrays += 1;
        }
        steps.push(PathStep::Member(member.to_owned()));
        steps.extend(std::iter::repeat_n(PathStep::Array, arrays));
    }
    steps
}

fn combine(results: impl IntoIterator<Item = PathLookup>) -> PathLookup {
    let mut unknown = false;
    for result in results {
        match result {
            PathLookup::Found => return PathLookup::Found,
            PathLookup::Unknown => unknown = true,
            PathLookup::Missing => {}
        }
    }
    if unknown {
        PathLookup::Unknown
    } else {
        PathLookup::Missing
    }
}

fn lookup_members(
    world: &mut World,
    file: &FileModel,
    members: &[(String, TypeExpr)],
    steps: &[PathStep],
    seen: &mut HashSet<String>,
    violations: &mut Vec<String>,
) -> anyhow::Result<Option<PathLookup>> {
    let Some(PathStep::Member(name)) = steps.first() else {
        return Ok(None);
    };
    let Some((_, value)) = members.iter().find(|(member, _)| member == name) else {
        return Ok(None);
    };
    if steps.len() == 1 {
        return Ok(Some(PathLookup::Found));
    }
    Ok(Some(lookup_expr(
        world,
        file,
        value,
        &steps[1..],
        seen,
        violations,
    )?))
}

fn lookup_decl(
    world: &mut World,
    file: &FileModel,
    declaration: &TypeDecl,
    steps: &[PathStep],
    seen: &mut HashSet<String>,
    violations: &mut Vec<String>,
) -> anyhow::Result<PathLookup> {
    let key = format!("{}:{}", declaration.paste.source, steps.len());
    if !seen.insert(key) {
        return Ok(PathLookup::Unknown);
    }
    match &declaration.kind {
        TypeDeclKind::Interface { members, bases } => {
            if let Some(result) = lookup_members(world, file, members, steps, seen, violations)? {
                return Ok(result);
            }
            if bases.is_empty() {
                return Ok(PathLookup::Missing);
            }
            let mut results = Vec::new();
            for base in bases {
                results.push(lookup_expr(
                    world,
                    file,
                    base,
                    steps,
                    &mut seen.clone(),
                    violations,
                )?);
            }
            Ok(combine(results))
        }
        TypeDeclKind::Alias(value) => lookup_expr(world, file, value, steps, seen, violations),
        TypeDeclKind::Enum => Ok(PathLookup::Unknown),
    }
}

fn lookup_expr(
    world: &mut World,
    file: &FileModel,
    expression: &TypeExpr,
    steps: &[PathStep],
    seen: &mut HashSet<String>,
    violations: &mut Vec<String>,
) -> anyhow::Result<PathLookup> {
    if steps.is_empty() {
        return Ok(PathLookup::Found);
    }
    match expression {
        TypeExpr::Object(members) => Ok(lookup_members(
            world, file, members, steps, seen, violations,
        )?
        .unwrap_or(PathLookup::Missing)),
        TypeExpr::Array(value) => match steps.first() {
            Some(PathStep::Array) => lookup_expr(world, file, value, &steps[1..], seen, violations),
            _ => Ok(PathLookup::Unknown),
        },
        TypeExpr::Wrapped(value) => lookup_expr(world, file, value, steps, seen, violations),
        TypeExpr::Intersection(values) => {
            let mut results = Vec::new();
            for value in values {
                results.push(lookup_expr(
                    world,
                    file,
                    value,
                    steps,
                    &mut seen.clone(),
                    violations,
                )?);
            }
            Ok(combine(results))
        }
        TypeExpr::Union(values) => {
            let mut results = Vec::new();
            for value in values {
                results.push(lookup_expr(
                    world,
                    file,
                    value,
                    steps,
                    &mut seen.clone(),
                    violations,
                )?);
            }
            if results.iter().all(|result| *result == PathLookup::Found) {
                Ok(PathLookup::Found)
            } else if results.iter().all(|result| *result == PathLookup::Missing) {
                Ok(PathLookup::Missing)
            } else {
                Ok(PathLookup::Unknown)
            }
        }
        TypeExpr::Indexed { object, index } => {
            let Some(index) = index else {
                return Ok(PathLookup::Unknown);
            };
            let mut forwarded = vec![PathStep::Member(index.clone())];
            forwarded.extend_from_slice(steps);
            lookup_expr(world, file, object, &forwarded, seen, violations)
        }
        TypeExpr::Reference {
            name,
            qualified,
            arguments,
        } => {
            if *qualified {
                return Ok(PathLookup::Unknown);
            }
            if matches!(
                name.as_str(),
                "Partial" | "Required" | "Readonly" | "NonNullable"
            ) {
                return arguments
                    .first()
                    .map_or(Ok(PathLookup::Unknown), |argument| {
                        lookup_expr(world, file, argument, steps, seen, violations)
                    });
            }
            if matches!(name.as_str(), "Array" | "ReadonlyArray") {
                return match (steps.first(), arguments.first()) {
                    (Some(PathStep::Array), Some(argument)) => {
                        lookup_expr(world, file, argument, &steps[1..], seen, violations)
                    }
                    _ => Ok(PathLookup::Unknown),
                };
            }
            let Some((declaration, declaring_file)) =
                world.declaration_for_reference(file, name, violations)?
            else {
                return Ok(PathLookup::Unknown);
            };
            lookup_decl(
                world,
                &declaring_file,
                &declaration,
                steps,
                seen,
                violations,
            )
        }
        TypeExpr::Unknown => Ok(PathLookup::Unknown),
    }
}

fn report(violations: &[String]) -> anyhow::Result<()> {
    anyhow::ensure!(
        violations.is_empty(),
        "gen-config-catalog: {} violation(s):\n{}",
        violations.len(),
        violations
            .iter()
            .map(|violation| format!("  {violation}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    Ok(())
}

/// Walks every two-level package and returns the validated catalog.
///
/// # Errors
///
/// Returns aggregate package, declaration, `JSDoc`, schema, or type-resolution violations.
#[allow(
    clippy::items_after_statements,
    clippy::too_many_lines,
    reason = "one closed package/type/schema validation transaction"
)]
pub fn collect_config_catalog(scan_root: &Path) -> anyhow::Result<Vec<CatalogEntry>> {
    let mut world = World::new(scan_root)?;
    let mut violations = Vec::new();
    let mut manifests = Vec::<(PathBuf, String)>::new();
    let mut manifest_paths = WalkDir::new(world.scan_root.join("packages"))
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file() && entry.file_name() == "package.json")
        .filter_map(|entry| {
            let relative = entry.path().strip_prefix(&world.scan_root).ok()?;
            (relative.components().count() == 4).then(|| relative.to_path_buf())
        })
        .collect::<Vec<_>>();
    manifest_paths.sort();
    for manifest in manifest_paths {
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(world.scan_root.join(&manifest))?)?;
        let Some(package) = value.get("name").and_then(serde_json::Value::as_str) else {
            violations.push(format!("{} has no \"name\".", slash(&manifest)));
            continue;
        };
        if value.get("os").is_some() && value.get("cpu").is_some() {
            continue;
        }
        let directory = manifest.parent().unwrap_or(Path::new("")).to_path_buf();
        world
            .package_dirs
            .insert(package.to_owned(), directory.clone());
        manifests.push((directory, package.to_owned()));
    }

    let mut entries = Vec::new();
    for (directory, package) in manifests {
        let entry_relative = directory.join("src/index.ts");
        let entry_absolute = world.scan_root.join(&entry_relative);
        let Ok(file) = world.load(&entry_absolute, &mut violations) else {
            violations.push(format!(
                "{package}: entry {} is missing or unreadable.",
                slash(&entry_relative)
            ));
            continue;
        };
        let Some(plugin) = file.plugin.clone() else {
            entries.push(CatalogEntry {
                package,
                directory: slash(&directory),
                entry: slash(&entry_relative),
                kind: CatalogKind::Library,
                inject: Vec::new(),
                class_name: None,
                config_type_name: None,
                pastes: None,
                refs: None,
                schema_keys: None,
                schema_composes: Vec::new(),
            });
            continue;
        };
        let kind = if plugin.abstract_class {
            CatalogKind::Seam
        } else if plugin.config.is_some() {
            CatalogKind::Config
        } else {
            CatalogKind::NoConfig
        };
        let mut entry = CatalogEntry {
            package: package.clone(),
            directory: slash(&directory),
            entry: slash(&entry_relative),
            kind,
            inject: if matches!(kind, CatalogKind::Library | CatalogKind::Seam) {
                Vec::new()
            } else {
                plugin.inject.clone()
            },
            class_name: plugin.class_name.clone(),
            config_type_name: None,
            pastes: None,
            refs: None,
            schema_keys: plugin.schema_keys.clone(),
            schema_composes: plugin.schema_composes.clone(),
        };
        if kind != CatalogKind::Config {
            entries.push(entry);
            continue;
        }
        let Some(parameter) = plugin.config.as_ref() else {
            entries.push(entry);
            continue;
        };
        let Some(type_name) = parameter.type_name.clone() else {
            violations.push(format!(
                "{package}: config parameter type ({}) is not a plain type-name reference; declare a named config type.",
                parameter.pointer
            ));
            entries.push(entry);
            continue;
        };
        entry.config_type_name = Some(type_name.clone());
        let mut pastes = Vec::new();
        let mut refs = HashMap::<String, TypeRef>::new();
        let mut pasted_sources = HashMap::<String, String>::new();
        let mut queue = VecDeque::from([(type_name.clone(), file.clone())]);
        while let Some((name, from)) = queue.pop_front() {
            match world.resolve_type(&from, &name, &mut violations)? {
                ResolvedType::Missing => {
                    violations.push(format!(
                        "{package}: config declaration references '{name}' (via {}), which is neither declared in the package, imported, nor a known global type.",
                        slash(&from.relative)
                    ));
                }
                ResolvedType::Reference(reference) => {
                    if name == type_name {
                        violations.push(format!(
                            "{package}: config type '{name}' is imported from '{}'; a plugin's config type must live in its own package.",
                            reference.specifier
                        ));
                        continue;
                    }
                    if let Some(local) = pasted_sources.get(&name) {
                        violations.push(format!(
                            "{package}: '{name}' resolves to a package-local declaration ({local}) in one file and an import from '{}' in another; rename one so the fence is unambiguous.",
                            reference.specifier
                        ));
                        continue;
                    }
                    if let Some(existing) = refs.get(&name)
                        && (existing.specifier != reference.specifier
                            || existing.imported != reference.imported)
                    {
                        violations.push(format!(
                            "{package}: '{name}' is imported from both '{}' ({}) and '{}' ({}) across the pasted closure; disambiguate the aliases.",
                            existing.specifier,
                            existing.imported,
                            reference.specifier,
                            reference.imported
                        ));
                        continue;
                    }
                    refs.insert(name, reference);
                }
                ResolvedType::Declaration(declaration, declaring_file) => {
                    let identity = declaration.paste.source.clone();
                    if pasted_sources.get(&name) == Some(&identity) {
                        continue;
                    }
                    if let Some(prior) = pasted_sources.get(&name) {
                        violations.push(format!(
                            "{package}: type name '{name}' resolves to two different declarations ({prior} and {identity}) across the pasted closure; rename one — a verbatim fence cannot carry two same-named declarations."
                        ));
                        continue;
                    }
                    if let Some(reference) = refs.get(&name) {
                        violations.push(format!(
                            "{package}: '{name}' resolves to an import from '{}' in one file and a package-local declaration ({identity}) in another; rename one so the fence is unambiguous.",
                            reference.specifier
                        ));
                        continue;
                    }
                    pasted_sources.insert(name, identity);
                    violations.extend(declaration.missing_docs.clone());
                    pastes.push(declaration.paste.clone());
                    for referenced in declaration_references(&declaration) {
                        if !GLOBAL_TYPES.contains(&referenced.as_str()) {
                            queue.push_back((referenced, (*declaring_file).clone()));
                        }
                    }
                }
            }
        }
        let mut refs = refs.into_values().collect::<Vec<_>>();
        refs.sort_by(|left, right| {
            left.alias
                .to_lowercase()
                .cmp(&right.alias.to_lowercase())
                .then(left.alias.cmp(&right.alias))
        });
        entry.pastes = Some(pastes);
        entry.refs = Some(refs);
        entries.push(entry);
    }

    let by_package = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| (entry.package.clone(), index))
        .collect::<HashMap<_, _>>();
    for index in 0..entries.len() {
        if entries[index].kind != CatalogKind::Config || entries[index].schema_keys.is_none() {
            continue;
        }
        let mut seen = HashSet::new();
        fn folded_keys(
            index: usize,
            entries: &[CatalogEntry],
            by_package: &HashMap<String, usize>,
            seen: &mut HashSet<String>,
            violations: &mut Vec<String>,
        ) -> Vec<String> {
            let entry = &entries[index];
            if !seen.insert(entry.package.clone()) {
                return Vec::new();
            }
            let mut keys = entry.schema_keys.clone().unwrap_or_default();
            for composed in &entry.schema_composes {
                let Some(target) = by_package.get(composed) else {
                    violations.push(format!(
                        "{}: schema intersects '{composed}', which is not a workspace package the walk collected.",
                        entry.package
                    ));
                    continue;
                };
                keys.extend(folded_keys(*target, entries, by_package, seen, violations));
            }
            keys
        }
        let keys = folded_keys(index, &entries, &by_package, &mut seen, &mut violations);
        let Some(type_name) = entries[index].config_type_name.clone() else {
            continue;
        };
        let declaration_path = entries[index]
            .pastes
            .as_ref()
            .and_then(|pastes| pastes.first())
            .and_then(|paste| paste.source.split(':').next())
            .unwrap_or(entries[index].entry.as_str());
        let declaration_absolute = world.scan_root.join(declaration_path);
        let entry_file = world.load(&declaration_absolute, &mut violations)?;
        let Some(declaration) = entry_file.declarations.get(&type_name).cloned() else {
            violations.push(format!(
                "{}: cannot locate config type '{type_name}' for the schema-path check.",
                entries[index].package
            ));
            continue;
        };
        for key in keys {
            if lookup_decl(
                &mut world,
                &entry_file,
                &declaration,
                &parse_path(&key),
                &mut HashSet::new(),
                &mut violations,
            )? == PathLookup::Missing
            {
                violations.push(format!(
                    "{}: schema validates key '{key}' but config type '{type_name}' declares no such member — the catalog paste would hide a loader-accepted field.",
                    entries[index].package
                ));
            }
        }
    }
    report(&violations)?;
    entries.sort_by(|left, right| left.package.cmp(&right.package));
    Ok(entries)
}

fn github_slug(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric() || matches!(character, '_' | ' ' | '-'))
        .flat_map(char::to_lowercase)
        .map(|character| if character == ' ' { '-' } else { character })
        .collect()
}

fn renamed(value: &str) -> String {
    value
        .replace("@deepseek-ai/dsh-", "@seekdeep-ai/seekdeep-")
        .replace("DeepSeek Harness", "SeekDeep Harness")
        .replace("DSH_", "SEEKDEEP_")
        .replace("DSH-", "SEEKDEEP-")
        .replace("dshHome", "seekdeepHome")
        .replace("dsh CLI", "seekdeep CLI")
        .replace("~/.dsh", "~/.seekdeep")
        .replace("dsh-", "seekdeep-")
}

fn requires_line(inject: &[String]) -> String {
    if inject.is_empty() {
        String::new()
    } else {
        format!(
            "Requires: {}",
            inject
                .iter()
                .map(|key| format!("`{key}`"))
                .collect::<Vec<_>>()
                .join(" · ")
        )
    }
}

fn link_page(name: &str) -> Option<&'static str> {
    match name {
        "Agent" | "AgentFactory" | "AgentHandle" | "AgentOptions" | "AgentStatus"
        | "CreateAgentOptions" | "ModelSelection" | "ResumeAgentOptions" | "ScopeKey"
        | "SessionId" => Some("core.md"),
        "ContentBlock"
        | "GenerateOptions"
        | "LlmAdapter"
        | "LlmCallConfig"
        | "LlmFailure"
        | "LlmModelInfo"
        | "LlmProviderInfo"
        | "LlmRuntime"
        | "Message"
        | "MessageId"
        | "MessageSource"
        | "PreparedLlmCall"
        | "ResolvedRetryPolicy"
        | "StreamChunk" => Some("llm-streaming.md"),
        "Session" | "SessionEvent" | "SessionEventMap" | "TurnEndReason" | "TurnTrigger"
        | "UserMessage" => Some("session.md"),
        "CreateSessionOptions"
        | "SessionHeader"
        | "SessionInspection"
        | "SessionLocation"
        | "SessionPersistenceSnapshot"
        | "SessionRawArtifact" => Some("persistence.md"),
        "FsDirEntry" | "FsEditOutcome" | "FsEditRequest" | "FsInfo" | "FsObservation"
        | "FsPathInfo" | "FsTarget" | "FsVersion" | "FsWriteIntent" | "FsWriteOutcome" => {
            Some("filesystem.md")
        }
        "SandboxExecutionPolicy" | "SandboxMode" | "SandboxPolicy" | "SandboxPolicyRequest" => {
            Some("sandbox.md")
        }
        "SkillCatalogSnapshot"
        | "SkillDefinition"
        | "SkillLookupOptions"
        | "SkillProvider"
        | "SkillProviderObservation"
        | "SkillRegistration"
        | "SkillSummary"
        | "SkillViewOptions" => Some("skills.md"),
        "JobId" | "JobRead" | "JobSnapshot" | "JobStart" => Some("jobs.md"),
        "PromptContext" | "PromptSection" | "SystemPrompt" | "AssembleContext" => {
            Some("system-prompt.md")
        }
        "ToolDefinition"
        | "ToolExecution"
        | "ToolExecutionInput"
        | "ToolExecutionResult"
        | "ToolPresentationMode"
        | "ToolRuntime"
        | "ToolSchema" => Some("tools.md"),
        "SettingsNamespace"
        | "SettingsDescriptor"
        | "SettingsPathOp"
        | "SettingsRegisterOptions" => Some("settings.md"),
        "ApprovalPolicy" => Some("approval.md"),
        "CreateGoalRequest" | "EditGoalRequest" | "GoalRef" | "GoalView" => Some("goal.md"),
        "ShellExecRequest" | "ShellExecSpec" | "ShellProcess" | "ShellRunResult" => {
            Some("shell.md")
        }
        "WebFetchRequest" | "WebFetchResult" | "WebSearchRequest" | "WebSearchResult" => {
            Some("web.md")
        }
        "SubagentReportDelivery" => Some("subagent.md"),
        _ => None,
    }
}

fn ref_link(reference: &TypeRef, entries: &HashMap<&str, &CatalogEntry>) -> String {
    if let Some(target) = entries.get(reference.specifier.as_str())
        && target.kind == CatalogKind::Config
        && target.config_type_name.as_deref() == Some(reference.imported.as_str())
    {
        return format!("[`{}`](#{})", reference.alias, github_slug(&target.package));
    }
    if let Some(page) = link_page(&reference.imported) {
        return format!("[`{}`](subsystems/{page})", reference.alias);
    }
    if let Some(target) = entries.get(reference.specifier.as_str()) {
        return format!("[`{}`](../{})", reference.alias, target.entry);
    }
    format!("`{}` (`{}`)", reference.alias, reference.specifier)
}

fn render_config_entry(
    entry: &CatalogEntry,
    entries: &HashMap<&str, &CatalogEntry>,
) -> Vec<String> {
    let mut lines = vec![
        format!("<a id=\"{}\"></a>", github_slug(&entry.package)),
        String::new(),
        format!("## `{}`", entry.package),
        String::new(),
    ];
    let requires = requires_line(&entry.inject);
    if !requires.is_empty() {
        lines.extend([requires, String::new()]);
    }
    lines.push(format!("```{FENCE}"));
    lines.extend(
        entry
            .pastes
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|paste| paste.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
            .lines()
            .map(str::to_owned),
    );
    lines.extend(["```".to_owned(), String::new()]);
    if let Some(references) = &entry.refs
        && !references.is_empty()
    {
        lines.extend([
            format!(
                "Depends on: {}",
                references
                    .iter()
                    .map(|reference| ref_link(reference, entries))
                    .collect::<Vec<_>>()
                    .join(" · ")
            ),
            String::new(),
        ]);
    }
    let source = entry
        .pastes
        .as_ref()
        .and_then(|pastes| pastes.first())
        .map_or(entry.entry.as_str(), |paste| paste.source.as_str());
    lines.extend([
        format!(
            "Source: [`{source}`](../{})",
            source.split(':').next().unwrap_or(source)
        ),
        String::new(),
    ]);
    lines
}

fn terse(entry: &CatalogEntry, detail: &str) -> String {
    let requires = if entry.inject.is_empty() {
        String::new()
    } else {
        format!(
            " — requires {}",
            entry
                .inject
                .iter()
                .map(|key| format!("`{key}`"))
                .collect::<Vec<_>>()
                .join(" · ")
        )
    };
    format!(
        "- `{}`{detail}{requires} ([`{}`](../{}))",
        entry.package, entry.entry, entry.entry
    )
}

/// Renders the deterministic Markdown catalog.
#[must_use]
pub fn render(entries: &[CatalogEntry]) -> String {
    let by_package = entries
        .iter()
        .map(|entry| (entry.package.as_str(), entry))
        .collect::<HashMap<_, _>>();
    let mut lines = vec![
        "<!-- Generated by `cargo xtask config-catalog` — do not edit by hand. -->".to_owned(),
        String::new(),
        "# Plugin Config Catalog".to_owned(),
        String::new(),
        "Every `config:` block a `cordis.yml` entry can set: for each loadable harness package, the verbatim config declaration (JSDoc included) its `apply` function or service constructor receives, with every referenced type pasted alongside (package-local types) or linked (everything else). The paste is the plugin's full declared config type — a field the runtime schema deliberately excludes is a runtime-only seam (its own JSDoc says so) and is not settable from `cordis.yml`. This is the **deployment**-axis reference — the wiring a plugin author works against is the generated Cordis API region on each [subsystem page](subsystems/core.md), the model-facing tool schemas are the [tool catalog](tool-catalog.md), and [subsystems/](subsystems/core.md) documents the types these declarations reference.".to_owned(),
        String::new(),
        "This file is generated from the pinned source by `cargo xtask config-catalog` and freshness-checked by `cargo xtask config-catalog --check`. Declaration blocks use a `ts config-catalog` fence. The generator cross-checks every enumerable runtime schema path against the pasted declaration, including nested paths, so the paste cannot hide a Loader-accepted field.".to_owned(),
        String::new(),
        "A `Requires:` line lists the service keys the plugin `inject`s: its `cordis.yml` tree must also load providers for those services. Scope is the harness tier (`packages/`); the vendored Cordis plugins a config tree may also load (`hmr`, the console logger, …) are pinned upstream source ([vendoring policy](../vendor/README.md)) and not catalogued here.".to_owned(),
        String::new(),
    ];
    for entry in entries
        .iter()
        .filter(|entry| entry.kind == CatalogKind::Config)
    {
        lines.extend(render_config_entry(entry, &by_package));
    }
    lines.extend([
        "## Loadable plugins with no config".to_owned(),
        String::new(),
        "These load from a `cordis.yml` entry with no `config:` block; they declare no configuration API.".to_owned(),
        String::new(),
    ]);
    lines.extend(
        entries
            .iter()
            .filter(|entry| entry.kind == CatalogKind::NoConfig)
            .map(|entry| terse(entry, "")),
    );
    lines.extend([
        String::new(),
        "## Seam packages (not directly loadable)".to_owned(),
        String::new(),
        "Abstract service classes — a deployment loads a concrete implementation package instead ([capability seams](../.agents/notes/implemented/architecture/2026-06-13-capability-seams.md)).".to_owned(),
        String::new(),
    ]);
    lines.extend(
        entries
            .iter()
            .filter(|entry| entry.kind == CatalogKind::Seam)
            .map(|entry| {
                terse(
                    entry,
                    &format!(
                        " — abstract `{}`",
                        entry.class_name.as_deref().unwrap_or("")
                    ),
                )
            }),
    );
    lines.extend([
        String::new(),
        "## Library packages (no plugin entry)".to_owned(),
        String::new(),
        "Imported as libraries by other packages; a `cordis.yml` cannot load them.".to_owned(),
        String::new(),
    ]);
    lines.extend(
        entries
            .iter()
            .filter(|entry| entry.kind == CatalogKind::Library)
            .map(|entry| terse(entry, "")),
    );
    lines.push(String::new());
    renamed(&lines.join("\n"))
}
