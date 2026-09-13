//! AST helpers for the client slot surface: the `SlotMap` declaration merges
//! that type every slot, and the `slots.register` call sites that say who
//! already occupies one. Both readings are lexical (no type-checker program):
//! the client catalog generator consumes them, and the same scan doubles as
//! its own exhaustiveness backstop because it reads every source file rather
//! than a reachable-export closure.

use std::{collections::HashMap, path::Path, sync::LazyLock};

use indexmap::{IndexMap, IndexSet};
use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Argument, CallExpression, Expression, ObjectExpression, ObjectPropertyKind, Program,
    PropertyKind, TSLiteral, TSModuleBlock, TSModuleDeclarationBody, TSModuleDeclarationName,
    TSPropertySignature, TSSignature, TSType, TSTypeLiteral,
};
use oxc_ast_visit::{Visit, walk};
use oxc_span::GetSpan;
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::ts_lexical::{
    NamedDeclaration, collapse, dedent, full_start_after, glob_relative, key_source_text, line_of,
    locale_compare, member_name, named_declaration, package_name_of, parse, start_with_jsdoc,
    text_of,
};

/// The module whose `SlotMap` / standard-kit interfaces every slot owner
/// merges into, in either identity spelling.
pub const SLOTS_MODULES: [&str; 2] = [
    "@deepseek-ai/dsh-client-ui-slots",
    "@seekdeep-ai/seekdeep-client-ui-slots",
];

/// Cheap textual prefilter for a slot-contract merge, quote-style agnostic.
static MERGE_HEAD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"declare module ['"](?:@deepseek-ai/dsh-client-ui-slots|@seekdeep-ai/seekdeep-client-ui-slots)['"]"#,
    )
    .expect("valid merge prefilter")
});

/// Cheap textual prefilter for a registration call site.
static REGISTER_HEAD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\.register\(").expect("valid register prefilter"));

/// One `SlotMap` member: the slot's contract as its owning package declares it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlotDeclaration {
    /// `SlotMap` key, e.g. `settings.section`.
    pub key: String,
    /// Cardinality literal (`single` / `list` / `keyed` / `chain`), or empty when not a literal.
    pub kind: String,
    /// Data-scope literal (`root` / `session` / `session-maybe`), or empty when not a literal.
    pub scope: String,
    /// Type name of the owner-supplied props share, absent when the slot declares none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_type: Option<String>,
    /// Source text of the `keyProps` member (keyed slots), absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_props: Option<String>,
    /// Source text of the `hookContext` member, absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_context: Option<String>,
    /// Type name of the slot-level inject face, absent when the slot declares none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inject_type: Option<String>,
    /// The member's `JSDoc` with container indentation removed, empty when undocumented.
    pub js_doc: String,
    /// Workspace package that declares the contract.
    pub package: String,
    /// Source pointer `packages/…/file.ts:line`.
    pub source: String,
}

/// One `slots.register({ name, … }, Component)` call site.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlotRegistration {
    /// Target `SlotMap` key the entry contributes into.
    pub key: String,
    /// Workspace package that registers the entry.
    pub package: String,
    /// Component argument as written (identifier, or a trimmed expression).
    pub component: String,
    /// `id` literal of a list entry, absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// `key` literal of a keyed entry, absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_key: Option<String>,
    /// `SlotMap` keys this registration declares as children (they exist while it is mounted).
    pub children: Vec<String>,
    /// Source pointer `packages/…/file.ts:line`.
    pub source: String,
}

/// One exported type declaration, retained with its `JSDoc` for catalog projection.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeDeclaration {
    /// Declared name.
    pub name: String,
    /// Full declaration text INCLUDING its `JSDoc` (member docs are the teaching text).
    pub text: String,
    /// Source pointer `packages/…/file.ts:line`.
    pub source: String,
}

/// One scanned source file with the artifacts the catalog reads from it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScannedFile {
    /// Repo-relative, `/`-normalized path.
    pub rel: String,
    /// Workspace package name that owns the file.
    pub package: String,
    /// Source text, parsed on demand.
    pub text: String,
}

/// Parse every file matching `patterns`, keeping the ones that carry a slot
/// contract merge or a registration call. Files without either are skipped so
/// the scan stays cheap over the whole workspace.
///
/// # Errors
/// Returns glob and file-read failures.
pub fn scan_slot_files(scan_root: &Path, patterns: &[&str]) -> anyhow::Result<Vec<ScannedFile>> {
    let mut out = Vec::new();
    let mut names = HashMap::new();
    for rel in glob_relative(scan_root, patterns)? {
        let text = std::fs::read_to_string(scan_root.join(&rel))?;
        if !MERGE_HEAD.is_match(&text) && !REGISTER_HEAD.is_match(&text) {
            continue;
        }
        out.push(ScannedFile {
            package: package_name_of(scan_root, &rel, &mut names),
            rel,
            text,
        });
    }
    Ok(out)
}

/// Index every exported type declaration of the scanned packages, keeping
/// `JSDoc`. Names declared more than once are dropped as ambiguous.
///
/// # Errors
/// Returns glob and file-read failures.
pub fn index_exported_types(
    scan_root: &Path,
    patterns: &[&str],
) -> anyhow::Result<IndexMap<String, TypeDeclaration>> {
    let mut index = IndexMap::new();
    let mut ambiguous = IndexSet::new();
    for rel in glob_relative(scan_root, patterns)? {
        let text = std::fs::read_to_string(scan_root.join(&rel))?;
        index_exported_types_in(&rel, &text, &mut index, &mut ambiguous);
    }
    for name in ambiguous {
        index.shift_remove(&name);
    }
    Ok(index)
}

/// Adds one file's exported interface and type-alias declarations to `index`.
pub fn index_exported_types_in(
    rel: &str,
    text: &str,
    index: &mut IndexMap<String, TypeDeclaration>,
    ambiguous: &mut IndexSet<String>,
) {
    let allocator = Allocator::default();
    let program = parse(&allocator, text, rel);
    let mut previous_end = 0;
    for statement in &program.body {
        let full_start = full_start_after(text, previous_end);
        let span = statement.span();
        previous_end = span.end as usize;
        let Some((declaration, exported)) = named_declaration(statement) else {
            continue;
        };
        if !exported {
            continue;
        }
        let name = match declaration {
            NamedDeclaration::Interface(_) | NamedDeclaration::TypeAlias(_) => {
                declaration.name(text)
            }
            NamedDeclaration::Class(_)
            | NamedDeclaration::Enum(_)
            | NamedDeclaration::Module(_) => {
                continue;
            }
        };
        if index.contains_key(&name) {
            ambiguous.insert(name);
            continue;
        }
        let start = span.start as usize;
        let with_doc = start_with_jsdoc(text, full_start, start);
        index.insert(
            name.clone(),
            TypeDeclaration {
                name,
                text: dedent(&text[with_doc..span.end as usize]),
                source: format!("{rel}:{}", line_of(text, start)),
            },
        );
    }
}

/// Read every `SlotMap` member declared in one scanned file, in source order.
#[must_use]
pub fn slot_declarations(file: &ScannedFile) -> Vec<SlotDeclaration> {
    let allocator = Allocator::default();
    let program = parse(&allocator, &file.text, &file.rel);
    let text = file.text.as_str();
    let mut out = Vec::new();
    for body in slot_module_blocks(&program) {
        for interface in interfaces_named(body, "SlotMap") {
            let mut previous_end = interface.body.span.start as usize + 1;
            for member in &interface.body.body {
                let full_start = full_start_after(text, previous_end);
                let span = member.span();
                previous_end = span.end as usize;
                let TSSignature::TSPropertySignature(property) = member else {
                    continue;
                };
                let Some(annotation) = &property.type_annotation else {
                    continue;
                };
                let entry = match &annotation.type_annotation {
                    TSType::TSTypeLiteral(literal) => Some(&**literal),
                    _ => None,
                };
                let start = span.start as usize;
                out.push(SlotDeclaration {
                    key: member_name(&property.key, property.computed, text),
                    kind: literal_member(entry, "kind", text),
                    scope: literal_member(entry, "scope", text),
                    owner_type: member_type_text(entry, "owner", text),
                    key_props: member_type_text(entry, "keyProps", text),
                    hook_context: member_type_text(entry, "hookContext", text),
                    inject_type: member_type_text(entry, "inject", text),
                    js_doc: js_doc_of(text, full_start, start),
                    package: file.package.clone(),
                    source: format!("{}:{}", file.rel, line_of(text, start)),
                });
            }
        }
    }
    out
}

/// Read every registration call site in one scanned file, in source order. A
/// call whose `name` is not a string literal is skipped — the shipped
/// composition always names its target literally, and a computed name
/// carries no catalog fact.
#[must_use]
pub fn slot_registrations(file: &ScannedFile) -> Vec<SlotRegistration> {
    let allocator = Allocator::default();
    let program = parse(&allocator, &file.text, &file.rel);
    let mut visitor = RegistrationVisitor {
        file,
        out: Vec::new(),
    };
    visitor.visit_program(&program);
    visitor.out
}

struct RegistrationVisitor<'f> {
    file: &'f ScannedFile,
    out: Vec<SlotRegistration>,
}

impl<'a> Visit<'a> for RegistrationVisitor<'_> {
    fn visit_call_expression(&mut self, call: &CallExpression<'a>) {
        let text = self.file.text.as_str();
        if let Expression::StaticMemberExpression(member) = &call.callee
            && member.property.name == "register"
            && is_slots_receiver(text_of(text, member.object.span()))
            && let Some(Argument::ObjectExpression(options)) = call.arguments.first()
            && let Some(key) = string_property(options, "name", text)
        {
            self.out.push(SlotRegistration {
                key,
                package: self.file.package.clone(),
                component: component_text(call.arguments.get(1), text),
                id: string_property(options, "id", text),
                entry_key: string_property(options, "key", text),
                children: child_keys(options, text),
                source: format!(
                    "{}:{}",
                    self.file.rel,
                    line_of(text, call.span.start as usize)
                ),
            });
        }
        walk::walk_call_expression(self, call);
    }
}

/// Read one standard-kit interface's members from the scanned files: the
/// props a slot component receives for free from the framework at a given
/// scope, as `member: type` texts in declaration order merged across files.
#[must_use]
pub fn standard_kit_members(files: &[ScannedFile], interface_name: &str) -> Vec<String> {
    let mut out = Vec::new();
    for file in files {
        let allocator = Allocator::default();
        let program = parse(&allocator, &file.text, &file.rel);
        let text = file.text.as_str();
        for body in slot_module_blocks(&program) {
            for interface in interfaces_named(body, interface_name) {
                for member in &interface.body.body {
                    let TSSignature::TSPropertySignature(property) = member else {
                        continue;
                    };
                    let type_text = property.type_annotation.as_ref().map_or_else(
                        || "unknown".to_owned(),
                        |annotation| text_of(text, annotation.type_annotation.span()).to_owned(),
                    );
                    out.push(format!(
                        "{}{}: {}",
                        key_source_text(&property.key, property.computed, text),
                        if property.optional { "?" } else { "" },
                        collapse(&type_text)
                    ));
                }
            }
        }
    }
    out
}

/// Names in the type index that seed texts mention, word-bounded — ONE level,
/// not a transitive closure — sorted.
#[must_use]
pub fn referenced_type_names(
    seeds: &[String],
    index: &IndexMap<String, TypeDeclaration>,
) -> Vec<String> {
    let mut found = index
        .keys()
        .filter(|name| seeds.iter().any(|seed| mentions_word(seed, name)))
        .cloned()
        .collect::<Vec<_>>();
    found.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
    found
}

/// Resolve declarations by name, dropping names the index does not hold,
/// sorted by name.
#[must_use]
pub fn declared_types(
    names: &[String],
    index: &IndexMap<String, TypeDeclaration>,
) -> Vec<TypeDeclaration> {
    let mut declarations = names
        .iter()
        .filter_map(|name| index.get(name).cloned())
        .collect::<Vec<_>>();
    declarations.sort_by(|left, right| locale_compare(&left.name, &right.name));
    declarations
}

fn mentions_word(text: &str, name: &str) -> bool {
    let is_word = |character: Option<char>| {
        character.is_some_and(|character| character.is_ascii_alphanumeric() || character == '_')
    };
    let mut search = 0;
    while let Some(offset) = text[search..].find(name) {
        let start = search + offset;
        let end = start + name.len();
        let before = text[..start].chars().next_back();
        let after = text[end..].chars().next();
        let boundary_before = is_word(before) != is_word(name.chars().next());
        let boundary_after = is_word(after) != is_word(name.chars().next_back());
        if boundary_before && boundary_after {
            return true;
        }
        search = start + name.chars().next().map_or(1, char::len_utf8);
    }
    false
}

/// Every slot-contract module block in one program, in source order.
fn slot_module_blocks<'b, 'a>(program: &'b Program<'a>) -> Vec<&'b TSModuleBlock<'a>> {
    let mut bodies = Vec::new();
    for statement in &program.body {
        let Some((NamedDeclaration::Module(module), _)) = named_declaration(statement) else {
            continue;
        };
        let TSModuleDeclarationName::StringLiteral(name) = &module.id else {
            continue;
        };
        if !SLOTS_MODULES.contains(&name.value.as_str()) {
            continue;
        }
        if let Some(TSModuleDeclarationBody::TSModuleBlock(block)) = &module.body {
            bodies.push(&**block);
        }
    }
    bodies
}

fn interfaces_named<'b, 'a>(
    body: &'b TSModuleBlock<'a>,
    name: &str,
) -> Vec<&'b oxc_ast::ast::TSInterfaceDeclaration<'a>> {
    body.body
        .iter()
        .filter_map(|statement| match named_declaration(statement) {
            Some((NamedDeclaration::Interface(interface), _)) if interface.id.name == name => {
                Some(interface)
            }
            _ => None,
        })
        .collect()
}

/// Whether a `X.register(...)` receiver is the slots service: every other
/// registry in the repository also takes an options object with a `name`, so
/// the receiver is what separates a slot occupancy fact from an unrelated
/// registration.
#[expect(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "a member-access suffix, not a file extension"
)]
fn is_slots_receiver(receiver: &str) -> bool {
    receiver == "slots" || receiver.ends_with(".slots")
}

/// One member's `JSDoc` comment text, empty when the member has none.
fn js_doc_of(text: &str, full_start: usize, start: usize) -> String {
    let with_doc = start_with_jsdoc(text, full_start, start);
    if with_doc >= start {
        return String::new();
    }
    dedent(text[with_doc..start].trim_end_matches(crate::jsdoc::is_js_space))
}

/// A type-literal member's string-literal type text, empty when absent or computed.
fn literal_member(entry: Option<&TSTypeLiteral<'_>>, name: &str, text: &str) -> String {
    let Some(annotation) =
        named_member(entry, name, text).and_then(|member| member.type_annotation.as_ref())
    else {
        return String::new();
    };
    match &annotation.type_annotation {
        TSType::TSLiteralType(literal) => match &literal.literal {
            TSLiteral::StringLiteral(value) => value.value.to_string(),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

/// A type-literal member's type text on one line, absent when the member is.
fn member_type_text(entry: Option<&TSTypeLiteral<'_>>, name: &str, text: &str) -> Option<String> {
    let annotation = named_member(entry, name, text)?.type_annotation.as_ref()?;
    Some(collapse(text_of(text, annotation.type_annotation.span())))
}

/// One named property signature of a type literal.
fn named_member<'b, 'a>(
    entry: Option<&'b TSTypeLiteral<'a>>,
    name: &str,
    text: &str,
) -> Option<&'b TSPropertySignature<'a>> {
    entry?.members.iter().find_map(|member| match member {
        TSSignature::TSPropertySignature(property)
            if member_name(&property.key, property.computed, text) == name =>
        {
            Some(&**property)
        }
        _ => None,
    })
}

/// One string-literal property of an options object literal.
fn string_property(options: &ObjectExpression<'_>, name: &str, text: &str) -> Option<String> {
    options
        .properties
        .iter()
        .find_map(|property| match property {
            ObjectPropertyKind::ObjectProperty(property)
                if is_property_assignment(property)
                    && member_name(&property.key, property.computed, text) == name =>
            {
                match &property.value {
                    Expression::StringLiteral(literal) => Some(literal.value.to_string()),
                    _ => None,
                }
            }
            _ => None,
        })
}

fn is_property_assignment(property: &oxc_ast::ast::ObjectProperty<'_>) -> bool {
    !property.shorthand && !property.method && property.kind == PropertyKind::Init
}

/// The `SlotMap` keys a registration's `children` table declares.
fn child_keys(options: &ObjectExpression<'_>, text: &str) -> Vec<String> {
    for property in &options.properties {
        let ObjectPropertyKind::ObjectProperty(property) = property else {
            continue;
        };
        if !is_property_assignment(property)
            || member_name(&property.key, property.computed, text) != "children"
        {
            continue;
        }
        let Expression::ObjectExpression(children) = &property.value else {
            return Vec::new();
        };
        return children
            .properties
            .iter()
            .filter_map(|child| match child {
                ObjectPropertyKind::ObjectProperty(child) => {
                    Some(member_name(&child.key, child.computed, text))
                }
                ObjectPropertyKind::SpreadProperty(_) => None,
            })
            .collect();
    }
    Vec::new()
}

/// The component argument as written; a non-identifier expression is collapsed.
fn component_text(argument: Option<&Argument<'_>>, text: &str) -> String {
    let Some(argument) = argument else {
        return "(none)".to_owned();
    };
    let collapsed = collapse(text_of(text, argument.span()));
    let units = collapsed.encode_utf16().collect::<Vec<_>>();
    if units.len() > 60 {
        format!("{}…", String::from_utf16_lossy(&units[..57]))
    } else {
        collapsed
    }
}
