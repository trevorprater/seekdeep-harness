//! Generate detailed Cordis core API pages from pinned vendor declarations:
//! the `Context` class and its module merges, `Events`, `Fiber`, `Registry`,
//! and `Service`, with every public member's `JSDoc` prose, parameters, and
//! return documentation enforced.

#![expect(
    clippy::too_many_lines,
    reason = "one renderer, ported function for function"
)]

use std::{collections::HashMap, path::Path, sync::LazyLock};

use indexmap::IndexMap;
use oxc_allocator::Allocator;
use oxc_ast::ast::{
    BindingPattern, Class, ClassElement, Expression, FormalParameters, Function, MethodDefinition,
    MethodDefinitionKind, ObjectProperty, Program, PropertyKey, PropertyKind, Statement,
    TSAccessibility, TSInterfaceDeclaration, TSLiteral, TSSignature, TSThisParameter, TSType,
    TSTypeAnnotation,
};
use oxc_ast_visit::{Visit, walk};
use oxc_span::{GetSpan, Span};
use oxc_syntax::scope::ScopeFlags;
use regex::Regex;

use crate::{
    cordis_walk::cordis_module_block,
    jsdoc::{
        DeclaredParameter, check_params, check_returns, is_js_space, parse_jsdoc, parse_tags,
        pointer, raw_jsdoc, report_violations,
    },
    ts_lexical::{
        NamedDeclaration, collapse, full_start_after, key_source_text, line_of, named_declaration,
        parse, text_of,
    },
};

/// The fence every pasted declaration and signature block uses.
pub const FENCE: &str = "ts cordis-catalog";

/// One declaration group rendered on a Cordis core API page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CordisCoreApiSection {
    /// A class: prose, source link, then every public instance and static member.
    Class {
        /// Repo-relative vendor file.
        file: &'static str,
        /// Class name.
        symbol: &'static str,
        /// Heading prefix for instance members (defaults to the lower-cased class name).
        prefix: Option<&'static str>,
        /// Optional `##` heading above the section.
        heading: Option<&'static str>,
    },
    /// Every member of the file's `interface Context` merge, as `ctx.` members.
    ContextMerge {
        /// Repo-relative vendor file.
        file: &'static str,
        /// Optional `##` heading above the section.
        heading: Option<&'static str>,
    },
    /// A pasted declaration with bodies stripped.
    Decl {
        /// Repo-relative vendor file.
        file: &'static str,
        /// Declared name.
        symbol: &'static str,
    },
}

/// One generated Cordis core API page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CordisCoreApiPage {
    /// Repo-relative output path.
    pub out: &'static str,
    /// Page title.
    pub title: &'static str,
    /// Intro paragraph.
    pub intro: &'static str,
    /// Sections in reading order.
    pub sections: &'static [CordisCoreApiSection],
}

/// Explicit editorial grouping for the pinned Cordis core API.
pub const CORDIS_CORE_API_PAGES: &[CordisCoreApiPage] = &[
    CordisCoreApiPage {
        out: "docs/cordis-api/context.md",
        title: "Context",
        intro: "The context is the core Cordis object: every service, event, and lifecycle API is reached through `ctx`. Event methods are documented on [Events](events.md), effects and the current fiber on [Fiber](fiber.md), and plugin loading on [Registry](registry.md).",
        sections: &[
            CordisCoreApiSection::Class {
                file: "vendor/cordis/src/context.ts",
                symbol: "Context",
                prefix: Some("ctx."),
                heading: None,
            },
            CordisCoreApiSection::ContextMerge {
                file: "vendor/cordis/src/reflect.ts",
                heading: Some("Service store and mixins"),
            },
        ],
    },
    CordisCoreApiPage {
        out: "docs/cordis-api/events.md",
        title: "Events",
        intro: "The event-dispatch API mixed into every context. Harness event declarations and their dispatch modes are generated into each owning [subsystem page](../subsystems/core.md).",
        sections: &[
            CordisCoreApiSection::ContextMerge {
                file: "vendor/cordis/src/events.ts",
                heading: None,
            },
            CordisCoreApiSection::Decl {
                file: "vendor/cordis/src/events.ts",
                symbol: "EventOptions",
            },
            CordisCoreApiSection::Decl {
                file: "vendor/cordis/src/events.ts",
                symbol: "DispatchMode",
            },
        ],
    },
    CordisCoreApiPage {
        out: "docs/cordis-api/fiber.md",
        title: "Fiber",
        intro: "A fiber is one loaded plugin instance: its lifecycle state, validated config, and registered effects. `ctx.fiber` is the current fiber, and `ctx.effect()` delegates to it.",
        sections: &[
            CordisCoreApiSection::ContextMerge {
                file: "vendor/cordis/src/fiber.ts",
                heading: None,
            },
            CordisCoreApiSection::Class {
                file: "vendor/cordis/src/fiber.ts",
                symbol: "Fiber",
                prefix: None,
                heading: Some("The Fiber class"),
            },
            CordisCoreApiSection::Decl {
                file: "vendor/cordis/src/fiber.ts",
                symbol: "Effect",
            },
            CordisCoreApiSection::Decl {
                file: "vendor/cordis/src/fiber.ts",
                symbol: "Disposable",
            },
            CordisCoreApiSection::Decl {
                file: "vendor/cordis/src/fiber.ts",
                symbol: "EffectMeta",
            },
            CordisCoreApiSection::Decl {
                file: "vendor/cordis/src/fiber.ts",
                symbol: "CordisError",
            },
            CordisCoreApiSection::Decl {
                file: "vendor/cordis/src/fiber.ts",
                symbol: "ValidationError",
            },
        ],
    },
    CordisCoreApiPage {
        out: "docs/cordis-api/registry.md",
        title: "Registry",
        intro: "Plugin loading and dependency injection.",
        sections: &[
            CordisCoreApiSection::ContextMerge {
                file: "vendor/cordis/src/registry.ts",
                heading: None,
            },
            CordisCoreApiSection::Decl {
                file: "vendor/cordis/src/registry.ts",
                symbol: "Plugin",
            },
            CordisCoreApiSection::Decl {
                file: "vendor/cordis/src/registry.ts",
                symbol: "Inject",
            },
        ],
    },
    CordisCoreApiPage {
        out: "docs/cordis-api/service.md",
        title: "Service",
        intro: "The base class for context services. A subclass loaded as a plugin registers itself as `ctx.<name>`.",
        sections: &[CordisCoreApiSection::Class {
            file: "vendor/cordis/src/service.ts",
            symbol: "Service",
            prefix: None,
            heading: None,
        }],
    },
];

struct MemberDoc {
    name: String,
    heading: String,
    signatures: Vec<String>,
    js_doc: String,
    doc: String,
    params: Vec<(String, String)>,
    returns: Option<String>,
    source: String,
}

struct RenderContext<'r> {
    scan_root: &'r Path,
    cache: HashMap<String, String>,
    violations: Vec<String>,
}

impl RenderContext<'_> {
    fn load(&mut self, rel: &str) -> anyhow::Result<String> {
        if let Some(cached) = self.cache.get(rel) {
            return Ok(cached.clone());
        }
        let text = std::fs::read_to_string(self.scan_root.join(rel))?;
        self.cache.insert(rel.to_owned(), text.clone());
        Ok(text)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MemberKind {
    MethodDeclaration,
    MethodSignature,
    Property,
    Accessor,
}

struct ParamInfo {
    text: String,
    identifier: Option<String>,
    rest: bool,
    optional: bool,
}

/// One class element or interface member in the compiler's terms.
struct MemberInfo {
    span: Span,
    full_start: usize,
    kind: MemberKind,
    has_body: bool,
    tail: Option<Span>,
    params: Vec<ParamInfo>,
    return_type: Option<Span>,
}

impl MemberInfo {
    fn is_function(&self) -> bool {
        matches!(
            self.kind,
            MemberKind::MethodDeclaration | MemberKind::MethodSignature
        )
    }
}

fn params_of(
    this_param: Option<&TSThisParameter<'_>>,
    params: &FormalParameters<'_>,
    text: &str,
) -> Vec<ParamInfo> {
    let _ = this_param;
    let mut out = Vec::new();
    for parameter in &params.items {
        let (name_span, identifier, assignment) = match &parameter.pattern {
            BindingPattern::BindingIdentifier(identifier) => {
                (identifier.span, Some(identifier.name.to_string()), false)
            }
            BindingPattern::AssignmentPattern(assignment) => {
                let left = &assignment.left;
                let identifier = match left {
                    BindingPattern::BindingIdentifier(identifier) => {
                        Some(identifier.name.to_string())
                    }
                    _ => None,
                };
                (left.span(), identifier, true)
            }
            other => (other.span(), None, false),
        };
        out.push(ParamInfo {
            text: text_of(text, name_span).to_owned(),
            identifier,
            rest: false,
            optional: parameter.optional || parameter.initializer.is_some() || assignment,
        });
    }
    if let Some(rest) = &params.rest {
        let identifier = match &rest.rest.argument {
            BindingPattern::BindingIdentifier(identifier) => Some(identifier.name.to_string()),
            _ => None,
        };
        out.push(ParamInfo {
            text: text_of(text, rest.rest.argument.span()).to_owned(),
            identifier,
            rest: true,
            optional: false,
        });
    }
    out
}

fn function_member(
    span: Span,
    full_start: usize,
    kind: MemberKind,
    function: &Function<'_>,
    text: &str,
) -> MemberInfo {
    MemberInfo {
        span,
        full_start,
        kind,
        has_body: function.body.is_some(),
        tail: function.body.as_ref().map(|body| body.span),
        params: params_of(function.this_param.as_deref(), &function.params, text),
        return_type: function
            .return_type
            .as_ref()
            .map(|annotation| annotation.type_annotation.span()),
    }
}

fn signature_member(
    span: Span,
    full_start: usize,
    member: &TSSignature<'_>,
    text: &str,
) -> Option<MemberInfo> {
    match member {
        TSSignature::TSMethodSignature(method) => Some(MemberInfo {
            span,
            full_start,
            kind: MemberKind::MethodSignature,
            has_body: false,
            tail: None,
            params: params_of(method.this_param.as_deref(), &method.params, text),
            return_type: method
                .return_type
                .as_ref()
                .map(|annotation| annotation.type_annotation.span()),
        }),
        TSSignature::TSPropertySignature(_) => Some(MemberInfo {
            span,
            full_start,
            kind: MemberKind::Property,
            has_body: false,
            tail: None,
            params: Vec::new(),
            return_type: None,
        }),
        TSSignature::TSIndexSignature(_)
        | TSSignature::TSCallSignatureDeclaration(_)
        | TSSignature::TSConstructSignatureDeclaration(_) => None,
    }
}

fn source_js_doc(text: &str, full_start: usize, start: usize) -> String {
    let raw = raw_jsdoc(text, full_start);
    if raw.is_empty() {
        return raw;
    }
    let line_start = text[..start].rfind('\n').map_or(0, |offset| offset + 1);
    let indent = &text[line_start..start];
    raw.split('\n')
        .enumerate()
        .map(|(index, line)| {
            if index > 0
                && let Some(stripped) = line.strip_prefix(indent)
            {
                stripped
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn signature_of(member: &MemberInfo, text: &str) -> String {
    let full = text_of(text, member.span);
    let signature = match member.tail {
        Some(tail) => {
            let units = full.encode_utf16().collect::<Vec<_>>();
            let tail_units = text_of(text, tail).encode_utf16().count();
            let kept = String::from_utf16_lossy(&units[..units.len().saturating_sub(tail_units)]);
            kept.trim_end_matches(|character: char| character == '=' || is_js_space(character))
                .to_owned()
        }
        None => full.to_owned(),
    };
    let trimmed = signature.trim_end_matches(is_js_space);
    let trimmed = trimmed.strip_suffix(';').unwrap_or(trimmed);
    collapse(trimmed)
}

fn heading_params(params: &[ParamInfo]) -> String {
    let names = params
        .iter()
        .map(|parameter| {
            format!(
                "{}{}{}",
                if parameter.rest { "..." } else { "" },
                parameter.text,
                if parameter.optional { "?" } else { "" }
            )
        })
        .collect::<Vec<_>>();
    format!("({})", names.join(", "))
}

fn public_name(key: &PropertyKey<'_>, computed: bool, text: &str) -> Option<String> {
    if computed || matches!(key, PropertyKey::PrivateIdentifier(_)) {
        return None;
    }
    let name = key_source_text(key, false, text);
    (!name.starts_with('_')).then(|| name.to_owned())
}

fn is_hidden(accessibility: Option<TSAccessibility>) -> bool {
    matches!(
        accessibility,
        Some(TSAccessibility::Private | TSAccessibility::Protected)
    )
}

fn member_doc(
    ctx: &mut RenderContext<'_>,
    where_: &str,
    name: &str,
    group: &[MemberInfo],
    rel: &str,
    text: &str,
) -> anyhow::Result<MemberDoc> {
    let Some(first) = group.first() else {
        anyhow::bail!("cordis-core-api: empty member group for {name}.");
    };
    let raw_docs = group
        .iter()
        .map(|member| source_js_doc(text, member.full_start, member.span.start as usize))
        .collect::<Vec<_>>();
    let doc_index = raw_docs
        .iter()
        .position(|raw| !parse_jsdoc(raw).doc.is_empty());
    let raw = doc_index.map_or_else(String::new, |index| raw_docs[index].clone());
    let doc = parse_jsdoc(&raw).doc;
    if doc.is_empty() {
        ctx.violations.push(format!("{where_} has no JSDoc prose."));
    }
    let tags = parse_tags(&raw);
    let function_members = group
        .iter()
        .filter(|member| member.is_function())
        .collect::<Vec<_>>();
    let doc_carrier = function_members.get(doc_index.unwrap_or(0)).copied();
    let mut params = Vec::new();
    if let Some(carrier) = doc_carrier {
        let declared = carrier
            .params
            .iter()
            .map(|parameter| DeclaredParameter {
                identifier: parameter.identifier.clone(),
                text: parameter.text.clone(),
            })
            .collect::<Vec<_>>();
        check_params(
            where_,
            "cordis-core-api",
            &declared,
            &tags.params,
            &|_| false,
            &mut ctx.violations,
        );
        if let Some(return_type) = carrier.return_type {
            check_returns(
                where_,
                Some(text_of(text, return_type)),
                tags.returns.as_deref(),
                &mut ctx.violations,
            );
        } else if tags.returns.is_none() && carrier.kind == MemberKind::MethodDeclaration {
            ctx.violations.push(format!(
                "{where_} has no return type annotation; document the result with @returns."
            ));
        }
        for parameter in &carrier.params {
            let Some(identifier) = &parameter.identifier else {
                continue;
            };
            if let Some(description) = tags.params.get(identifier) {
                params.push((identifier.clone(), description.clone()));
            }
        }
    }
    let heading_source = doc_carrier.or_else(|| function_members.first().copied());
    let signatures = if first.kind == MemberKind::MethodDeclaration && function_members.len() > 1 {
        function_members
            .iter()
            .filter(|member| member.kind == MemberKind::MethodDeclaration && !member.has_body)
            .map(|member| signature_of(member, text))
            .collect()
    } else {
        group
            .iter()
            .map(|member| signature_of(member, text))
            .collect()
    };
    Ok(MemberDoc {
        name: name.to_owned(),
        heading: heading_source.map_or_else(String::new, |member| heading_params(&member.params)),
        signatures,
        js_doc: raw,
        doc,
        params,
        returns: tags.returns,
        source: pointer(rel, line_of(text, first.span.start as usize)),
    })
}

fn class_named<'b, 'a>(program: &'b Program<'a>, name: &str) -> Option<(&'b Class<'a>, usize)> {
    let mut previous_end = 0;
    for statement in &program.body {
        let full_start = full_start_after(program.source_text, previous_end);
        previous_end = statement.span().end as usize;
        if let Some((NamedDeclaration::Class(class), _)) = named_declaration(statement)
            && class.id.as_ref().is_some_and(|id| id.name == name)
        {
            return Some((class, full_start));
        }
    }
    None
}

fn interface_named<'b, 'a>(
    statements: &'b [Statement<'a>],
    name: &str,
) -> Option<&'b TSInterfaceDeclaration<'a>> {
    statements
        .iter()
        .find_map(|statement| match named_declaration(statement) {
            Some((NamedDeclaration::Interface(interface), _)) if interface.id.name == name => {
                Some(interface)
            }
            _ => None,
        })
}

fn push_class_method_members(
    class: &Class<'_>,
    picked: &[String],
    groups: &mut IndexMap<String, Vec<MemberInfo>>,
    text: &str,
) {
    let mut previous_end = class.body.span.start as usize + 1;
    for element in &class.body.body {
        let full_start = full_start_after(text, previous_end);
        previous_end = element.span().end as usize;
        let ClassElement::MethodDefinition(method) = element else {
            continue;
        };
        if method.kind != MethodDefinitionKind::Method {
            continue;
        }
        let name = key_source_text(&method.key, method.computed, text).to_owned();
        if !picked.contains(&name) {
            continue;
        }
        groups.entry(name).or_default().push(function_member(
            element.span(),
            full_start,
            MemberKind::MethodDeclaration,
            &method.value,
            text,
        ));
    }
}

fn heritage_members(
    interface: &TSInterfaceDeclaration<'_>,
    program: &Program<'_>,
    groups: &mut IndexMap<String, Vec<MemberInfo>>,
    text: &str,
) {
    for heritage in &interface.extends {
        let Expression::Identifier(identifier) = &heritage.expression else {
            continue;
        };
        if identifier.name != "Pick" {
            continue;
        }
        let Some(arguments) = &heritage.type_arguments else {
            continue;
        };
        let (Some(target), Some(keys)) = (arguments.params.first(), arguments.params.get(1)) else {
            continue;
        };
        let TSType::TSTypeReference(target) = target else {
            continue;
        };
        let target_name = text_of(text, target.type_name.span());
        let Some((class, _)) = class_named(program, target_name) else {
            continue;
        };
        let mut picked = Vec::new();
        collect_literal_names(keys, &mut picked);
        push_class_method_members(class, &picked, groups, text);
    }
}

fn collect_literal_names(node: &TSType<'_>, picked: &mut Vec<String>) {
    match node {
        TSType::TSLiteralType(literal) => {
            if let TSLiteral::StringLiteral(value) = &literal.literal {
                picked.push(value.value.to_string());
            }
        }
        TSType::TSUnionType(union) => {
            for member in &union.types {
                collect_literal_names(member, picked);
            }
        }
        _ => {}
    }
}

fn context_merge_members(ctx: &mut RenderContext<'_>, rel: &str) -> anyhow::Result<Vec<MemberDoc>> {
    let text = ctx.load(rel)?;
    let allocator = Allocator::default();
    let program = parse(&allocator, &text, rel);
    let Some(body) = cordis_module_block(&program) else {
        anyhow::bail!("cordis-core-api: {rel} has no Context module merge.");
    };
    let mut groups: IndexMap<String, Vec<MemberInfo>> = IndexMap::new();
    for statement in &body.body {
        let Some((NamedDeclaration::Interface(interface), _)) = named_declaration(statement) else {
            continue;
        };
        if interface.id.name != "Context" {
            continue;
        }
        heritage_members(interface, &program, &mut groups, &text);
        let mut previous_end = interface.body.span.start as usize + 1;
        for member in &interface.body.body {
            let full_start = full_start_after(&text, previous_end);
            let span = member.span();
            previous_end = span.end as usize;
            let (key, computed) = match member {
                TSSignature::TSMethodSignature(method) => (&method.key, method.computed),
                TSSignature::TSPropertySignature(property) => (&property.key, property.computed),
                _ => continue,
            };
            if computed {
                continue;
            }
            let Some(info) = signature_member(span, full_start, member, &text) else {
                continue;
            };
            groups
                .entry(key_source_text(key, false, &text).to_owned())
                .or_default()
                .push(info);
        }
    }
    groups
        .iter()
        .map(|(name, group)| {
            member_doc(ctx, &format!("ctx.{name} ({rel})"), name, group, rel, &text)
        })
        .collect()
}

struct ClassDocs {
    doc: String,
    instance: Vec<MemberDoc>,
    statics: Vec<MemberDoc>,
    source: String,
}

fn class_members(
    ctx: &mut RenderContext<'_>,
    rel: &str,
    class_name: &str,
) -> anyhow::Result<ClassDocs> {
    let text = ctx.load(rel)?;
    let allocator = Allocator::default();
    let program = parse(&allocator, &text, rel);
    let Some((class, class_full_start)) = class_named(&program, class_name) else {
        anyhow::bail!("cordis-core-api: class {class_name} not found in {rel}.");
    };
    let class_start = class.span.start as usize;
    let doc = parse_jsdoc(&raw_jsdoc(&text, class_full_start)).doc;
    let source = pointer(rel, line_of(&text, class_start));
    if doc.is_empty() {
        ctx.violations
            .push(format!("class {class_name} ({source}) has no JSDoc."));
    }
    let mut instance: IndexMap<String, Vec<MemberInfo>> = IndexMap::new();
    let mut statics: IndexMap<String, Vec<MemberInfo>> = IndexMap::new();
    let mut previous_end = class.body.span.start as usize + 1;
    for element in &class.body.body {
        let full_start = full_start_after(&text, previous_end);
        let span = element.span();
        previous_end = span.end as usize;
        let (name, is_static, hidden, info) = match element {
            ClassElement::MethodDefinition(method) => {
                let kind = match method.kind {
                    MethodDefinitionKind::Method => MemberKind::MethodDeclaration,
                    MethodDefinitionKind::Get => MemberKind::Accessor,
                    MethodDefinitionKind::Constructor | MethodDefinitionKind::Set => continue,
                };
                (
                    public_name(&method.key, method.computed, &text),
                    method.r#static,
                    is_hidden(method.accessibility),
                    function_member(span, full_start, kind, &method.value, &text),
                )
            }
            ClassElement::PropertyDefinition(property) => (
                public_name(&property.key, property.computed, &text),
                property.r#static,
                is_hidden(property.accessibility),
                MemberInfo {
                    span,
                    full_start,
                    kind: MemberKind::Property,
                    has_body: false,
                    tail: property.value.as_ref().map(GetSpan::span),
                    params: Vec::new(),
                    return_type: None,
                },
            ),
            ClassElement::StaticBlock(_)
            | ClassElement::AccessorProperty(_)
            | ClassElement::TSIndexSignature(_) => continue,
        };
        let Some(name) = name else {
            continue;
        };
        if hidden {
            continue;
        }
        if !is_static {
            instance.entry(name).or_default().push(info);
        } else if info.kind != MemberKind::Accessor {
            statics.entry(name).or_default().push(info);
        }
    }
    if let Some(declaration) = interface_named(&program.body, class_name) {
        let mut previous_end = declaration.body.span.start as usize + 1;
        for member in &declaration.body.body {
            let full_start = full_start_after(&text, previous_end);
            let span = member.span();
            previous_end = span.end as usize;
            let TSSignature::TSPropertySignature(property) = member else {
                continue;
            };
            if property.computed {
                continue;
            }
            let Some(info) = signature_member(span, full_start, member, &text) else {
                continue;
            };
            instance
                .entry(key_source_text(&property.key, false, &text).to_owned())
                .or_default()
                .push(info);
        }
    }
    let instance = instance
        .iter()
        .map(|(name, group)| {
            member_doc(
                ctx,
                &format!("{class_name}#{name} ({rel})"),
                name,
                group,
                rel,
                &text,
            )
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let statics = statics
        .iter()
        .map(|(name, group)| {
            member_doc(
                ctx,
                &format!("{class_name}.{name} ({rel})"),
                name,
                group,
                rel,
                &text,
            )
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(ClassDocs {
        doc,
        instance,
        statics,
        source,
    })
}

struct BodyCuts {
    cuts: Vec<(usize, usize)>,
}

impl BodyCuts {
    fn record(&mut self, signature_end: usize, body_end: usize) {
        self.cuts.push((signature_end, body_end));
    }

    fn record_function(&mut self, entry_end: usize, function: &Function<'_>) {
        let Some(body) = &function.body else {
            return;
        };
        let signature_end = function
            .return_type
            .as_ref()
            .map(
                |annotation: &oxc_allocator::Box<'_, TSTypeAnnotation<'_>>| {
                    annotation.span.end as usize
                },
            )
            .or_else(|| last_parameter_end(function.this_param.as_deref(), &function.params))
            .unwrap_or(entry_end);
        self.record(signature_end, body.span.end as usize);
    }
}

fn last_parameter_end(
    this_param: Option<&TSThisParameter<'_>>,
    params: &FormalParameters<'_>,
) -> Option<usize> {
    params
        .rest
        .as_ref()
        .map(|rest| rest.span.end as usize)
        .or_else(|| params.items.last().map(|item| item.span.end as usize))
        .or_else(|| this_param.map(|this| this.span.end as usize))
}

impl<'a> Visit<'a> for BodyCuts {
    fn visit_method_definition(&mut self, method: &MethodDefinition<'a>) {
        if method.value.body.is_some() {
            self.record_function(method.span.end as usize, &method.value);
            return;
        }
        walk::walk_method_definition(self, method);
    }

    fn visit_function(&mut self, function: &Function<'a>, flags: ScopeFlags) {
        if function.r#type == oxc_ast::ast::FunctionType::FunctionDeclaration
            && function.body.is_some()
        {
            self.record_function(function.span.end as usize, function);
            return;
        }
        walk::walk_function(self, function, flags);
    }

    fn visit_object_property(&mut self, property: &ObjectProperty<'a>) {
        if (property.method || property.kind != PropertyKind::Init)
            && let Expression::FunctionExpression(function) = &property.value
            && function.body.is_some()
        {
            self.record_function(property.span.end as usize, function);
            return;
        }
        walk::walk_object_property(self, property);
    }
}

fn strip_bodies(statement: &Statement<'_>, text: &str) -> String {
    let mut visitor = BodyCuts { cuts: Vec::new() };
    visitor.visit_statement(statement);
    let span = statement.span();
    let base = span.start as usize;
    let mut output = text_of(text, span).to_owned();
    visitor.cuts.sort_by(|left, right| right.0.cmp(&left.0));
    for (start, end) in visitor.cuts {
        let head = output[..start - base].to_owned();
        let between = output[start - base..end - base].to_owned();
        let kept = if let Some(brace) = between.find('{') {
            between[..brace].to_owned()
        } else {
            let mut characters = between.chars();
            characters.next_back();
            characters.as_str().to_owned()
        };
        let tail = output[end - base..].to_owned();
        output = format!("{head}{}{tail}", kept.trim_end_matches(is_js_space));
    }
    output
}

struct DeclarationPaste {
    doc: String,
    code: String,
    source: String,
}

fn declaration_paste(
    ctx: &mut RenderContext<'_>,
    rel: &str,
    symbol: &str,
) -> anyhow::Result<DeclarationPaste> {
    let text = ctx.load(rel)?;
    let allocator = Allocator::default();
    let program = parse(&allocator, &text, rel);
    let mut matches = Vec::new();
    let mut previous_end = 0;
    for statement in &program.body {
        let full_start = full_start_after(&text, previous_end);
        previous_end = statement.span().end as usize;
        if let Some((declaration, _)) = named_declaration(statement)
            && declaration.name(&text) == symbol
        {
            matches.push((statement, full_start));
        }
    }
    let Some((first, first_full_start)) = matches.first() else {
        anyhow::bail!("cordis-core-api: declaration {symbol} not found in {rel}.");
    };
    let first_start = first.span().start as usize;
    let doc = parse_jsdoc(&source_js_doc(&text, *first_full_start, first_start)).doc;
    let code = matches
        .iter()
        .map(|(statement, full_start)| {
            let js_doc = source_js_doc(&text, *full_start, statement.span().start as usize);
            let declaration = strip_export(&strip_bodies(statement, &text));
            if js_doc.is_empty() {
                declaration
            } else {
                format!("{js_doc}\n{declaration}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    Ok(DeclarationPaste {
        doc,
        code,
        source: pointer(rel, line_of(&text, first_start)),
    })
}

static EXPORT_PREFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^export\s+(default\s+)?").expect("valid export prefix"));

fn strip_export(declaration: &str) -> String {
    EXPORT_PREFIX.replace(declaration, "").into_owned()
}

fn source_link(source: &str) -> String {
    let mut parts = source.splitn(2, ':');
    let file = parts.next().unwrap_or_default();
    let line = parts.next();
    format!(
        "[Source](../../{file}{})",
        line.map_or_else(String::new, |line| format!("#L{line}"))
    )
}

static LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\{@link\s+([^}|\s]+)\s*(?:[|\s]\s*([^}]*))?\}").expect("valid link pattern")
});

fn unlink(text: &str) -> String {
    LINK.replace_all(text, |captures: &regex::Captures<'_>| {
        let target = &captures[1];
        let label = captures
            .get(2)
            .map(|label| label.as_str().trim_matches(is_js_space))
            .filter(|label| !label.is_empty());
        label.map_or_else(|| format!("`{target}`"), str::to_owned)
    })
    .into_owned()
}

static PARAGRAPH_BREAK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\n\s*\n").expect("valid paragraph break"));
static LINE_JOIN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*\n\s*").expect("valid line join"));

fn prose(doc: &str) -> Vec<String> {
    let unlinked = unlink(doc);
    let paragraphs = PARAGRAPH_BREAK
        .split(&unlinked)
        .map(|paragraph| {
            LINE_JOIN
                .replace_all(paragraph, " ")
                .trim_matches(is_js_space)
                .to_owned()
        })
        .filter(|paragraph| !paragraph.is_empty())
        .collect::<Vec<_>>();
    let mut lines = Vec::new();
    for (index, paragraph) in paragraphs.into_iter().enumerate() {
        if index > 0 {
            lines.push(String::new());
        }
        lines.push(paragraph);
    }
    lines
}

fn render_member(prefix: &str, member: &MemberDoc) -> Vec<String> {
    let mut lines = vec![
        format!("### {prefix}{}{}", member.name, member.heading),
        String::new(),
        format!("```{FENCE}"),
    ];
    if !member.js_doc.is_empty() {
        lines.push(member.js_doc.clone());
    }
    lines.extend(member.signatures.iter().cloned());
    lines.push("```".to_owned());
    lines.push(String::new());
    if !member.doc.is_empty() {
        lines.extend(prose(&member.doc));
        lines.push(String::new());
    }
    for (name, text) in &member.params {
        lines.push(format!("- `{name}` — {}", unlink(text)));
    }
    if !member.params.is_empty() {
        lines.push(String::new());
    }
    if let Some(returns) = member
        .returns
        .as_deref()
        .filter(|returns| !returns.is_empty())
    {
        lines.push(format!("**Returns** {}", unlink(returns)));
        lines.push(String::new());
    }
    lines.push(source_link(&member.source));
    lines.push(String::new());
    lines
}

static BLANK_RUNS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\n{3,}").expect("valid blank-run pattern"));

/// Render one detailed Cordis core API page and reject undocumented members.
///
/// # Errors
/// Returns unreadable vendor files, missing declarations, and aggregated
/// `JSDoc` completeness violations.
pub fn render_cordis_core_api_page(
    page: &CordisCoreApiPage,
    scan_root: &Path,
) -> anyhow::Result<String> {
    let mut ctx = RenderContext {
        scan_root,
        cache: HashMap::new(),
        violations: Vec::new(),
    };
    let mut lines = vec![
        "<!-- Generated by scripts/gen-cordis-catalog.ts — do not edit by hand.".to_owned(),
        "     Run `pnpm run gen-cordis-catalog` to regenerate. -->".to_owned(),
        String::new(),
        format!("# {}", page.title),
        String::new(),
        page.intro.to_owned(),
        String::new(),
    ];
    for section in page.sections {
        match *section {
            CordisCoreApiSection::ContextMerge { file, heading } => {
                if let Some(heading) = heading {
                    lines.push(format!("## {heading}"));
                    lines.push(String::new());
                }
                for member in context_merge_members(&mut ctx, file)? {
                    lines.extend(render_member("ctx.", &member));
                }
            }
            CordisCoreApiSection::Class {
                file,
                symbol,
                prefix,
                heading,
            } => {
                if let Some(heading) = heading {
                    lines.push(format!("## {heading}"));
                    lines.push(String::new());
                }
                let class = class_members(&mut ctx, file, symbol)?;
                if !class.doc.is_empty() {
                    lines.extend(prose(&class.doc));
                    lines.push(String::new());
                }
                lines.push(source_link(&class.source));
                lines.push(String::new());
                let prefix =
                    prefix.map_or_else(|| format!("{}.", symbol.to_lowercase()), str::to_owned);
                for member in &class.instance {
                    lines.extend(render_member(&prefix, member));
                }
                if !class.statics.is_empty() {
                    lines.push("## Static members".to_owned());
                    lines.push(String::new());
                    for member in &class.statics {
                        lines.extend(render_member(&format!("{symbol}."), member));
                    }
                }
            }
            CordisCoreApiSection::Decl { file, symbol } => {
                let declaration = declaration_paste(&mut ctx, file, symbol)?;
                lines.push(format!("## {symbol}"));
                lines.push(String::new());
                if !declaration.doc.is_empty() {
                    lines.extend(prose(&declaration.doc));
                    lines.push(String::new());
                }
                lines.push(format!("```{FENCE}"));
                lines.push(declaration.code);
                lines.push("```".to_owned());
                lines.push(String::new());
                lines.push(source_link(&declaration.source));
                lines.push(String::new());
            }
        }
    }
    report_violations("gen-cordis-catalog", &ctx.violations)?;
    let joined = lines.join("\n");
    Ok(format!(
        "{}\n",
        BLANK_RUNS
            .replace_all(&joined, "\n\n")
            .trim_end_matches(is_js_space)
    ))
}

/// Render every detailed Cordis core API page, keyed by output path.
///
/// # Errors
/// Propagates the first page's rendering failure.
pub fn render_cordis_core_api_pages(scan_root: &Path) -> anyhow::Result<IndexMap<String, String>> {
    CORDIS_CORE_API_PAGES
        .iter()
        .map(|page| {
            Ok((
                page.out.to_owned(),
                render_cordis_core_api_page(page, scan_root)?,
            ))
        })
        .collect()
}
