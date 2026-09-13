//! AST helpers shared by the Cordis generators: locate the Cordis module
//! merge in a source file and enumerate the `interface Context` keys and
//! `interface Events` names it declares. The core API renderer consumes the
//! merge body; the per-subsystem region generator's exhaustiveness backstop
//! consumes the key and event scans.

use std::{path::Path, sync::LazyLock};

use indexmap::IndexMap;
use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Program, PropertyKey, Statement, TSModuleBlock, TSModuleDeclarationBody,
    TSModuleDeclarationName, TSSignature,
};
use oxc_span::GetSpan;
use regex::Regex;

use crate::ts_lexical::{
    glob_relative, key_source_text, member_name, named_declaration, parse, text_of,
};

/// Module names whose `declare module` blocks merge into the Cordis Context:
/// the harness package (either identity spelling) and the vendor core.
pub const CORDIS_MERGE_MODULES: [&str; 3] =
    ["@deepseek-ai/cordis", "@seekdeep-ai/cordis", "./context.ts"];

/// Cheap textual prefilter for a Cordis module merge, quote-style agnostic.
static MERGE_HEAD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"declare module ['"](?:@deepseek-ai/cordis|@seekdeep-ai/cordis|\./context\.ts)['"]"#,
    )
    .expect("valid merge prefilter")
});

/// One `declare module` block merging into the Cordis Context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CordisMergeBlock {
    /// Repo-relative, `/`-normalized declaring file.
    pub rel: String,
    /// `interface Context` property key → declared type text, in declaration order.
    pub context_keys: IndexMap<String, String>,
    /// `interface Events` member names, in declaration order.
    pub event_names: Vec<String>,
}

/// Every Cordis merge block in the files matching `patterns`, one entry per
/// block: a file may legally hold several `declare module` blocks (the Typert
/// analyzer reads them all), so the exhaustiveness scan must too. Files
/// without a merge are skipped.
///
/// # Errors
/// Returns glob and file-read failures.
pub fn context_merge_blocks(
    scan_root: &Path,
    patterns: &[&str],
) -> anyhow::Result<Vec<CordisMergeBlock>> {
    let mut out = Vec::new();
    for rel in glob_relative(scan_root, patterns)? {
        let text = std::fs::read_to_string(scan_root.join(&rel))?;
        if !MERGE_HEAD.is_match(&text) {
            continue;
        }
        out.extend(merge_blocks_in(&rel, &text));
    }
    Ok(out)
}

/// The merge blocks one source text declares, in source order.
#[must_use]
pub fn merge_blocks_in(rel: &str, text: &str) -> Vec<CordisMergeBlock> {
    let allocator = Allocator::default();
    let program = parse(&allocator, text, rel);
    cordis_module_blocks(&program)
        .into_iter()
        .map(|body| CordisMergeBlock {
            rel: rel.to_owned(),
            context_keys: context_key_map(body, text),
            event_names: event_name_list(body, text),
        })
        .collect()
}

/// Every Cordis module-merge body in a program, in source order.
#[must_use]
pub fn cordis_module_blocks<'b, 'a>(program: &'b Program<'a>) -> Vec<&'b TSModuleBlock<'a>> {
    let mut bodies = Vec::new();
    for statement in &program.body {
        let Some((crate::ts_lexical::NamedDeclaration::Module(module), _)) =
            named_declaration(statement)
        else {
            continue;
        };
        let TSModuleDeclarationName::StringLiteral(name) = &module.id else {
            continue;
        };
        if !CORDIS_MERGE_MODULES.contains(&name.value.as_str()) {
            continue;
        }
        if let Some(TSModuleDeclarationBody::TSModuleBlock(block)) = &module.body {
            bodies.push(&**block);
        }
    }
    bodies
}

/// The first Cordis module-merge body, for inputs that carry exactly one.
#[must_use]
pub fn cordis_module_block<'b, 'a>(program: &'b Program<'a>) -> Option<&'b TSModuleBlock<'a>> {
    cordis_module_blocks(program).into_iter().next()
}

/// Every `key: Type` property a Context merge declares in one module body,
/// key → declared type text in declaration order.
#[must_use]
pub fn context_key_map(body: &TSModuleBlock<'_>, text: &str) -> IndexMap<String, String> {
    let mut keys = IndexMap::new();
    for interface in interfaces_named(body, "Context") {
        for member in &interface.body.body {
            let TSSignature::TSPropertySignature(property) = member else {
                continue;
            };
            let Some(annotation) = &property.type_annotation else {
                continue;
            };
            keys.insert(
                key_source_text(&property.key, property.computed, text).to_owned(),
                text_of(text, annotation.type_annotation.span()).to_owned(),
            );
        }
    }
    keys
}

/// Every event name an Events merge declares in one module body, read from
/// method and property members alike so a declaration form the projector
/// would reject still enters the exhaustiveness scan.
#[must_use]
pub fn event_name_list(body: &TSModuleBlock<'_>, text: &str) -> Vec<String> {
    let mut names = Vec::new();
    for interface in interfaces_named(body, "Events") {
        for member in &interface.body.body {
            let (key, computed): (&PropertyKey<'_>, bool) = match member {
                TSSignature::TSPropertySignature(property) => (&property.key, property.computed),
                TSSignature::TSMethodSignature(method) => (&method.key, method.computed),
                TSSignature::TSIndexSignature(_)
                | TSSignature::TSCallSignatureDeclaration(_)
                | TSSignature::TSConstructSignatureDeclaration(_) => continue,
            };
            names.push(member_name(key, computed, text));
        }
    }
    names
}

fn interfaces_named<'b, 'a>(
    body: &'b TSModuleBlock<'a>,
    name: &str,
) -> Vec<&'b oxc_ast::ast::TSInterfaceDeclaration<'a>> {
    body.body
        .iter()
        .filter_map(|statement| match statement {
            Statement::TSInterfaceDeclaration(interface) if interface.id.name == name => {
                Some(&**interface)
            }
            Statement::ExportNamedDeclaration(export) => match export.declaration.as_ref() {
                Some(oxc_ast::ast::Declaration::TSInterfaceDeclaration(interface))
                    if interface.id.name == name =>
                {
                    Some(&**interface)
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}
