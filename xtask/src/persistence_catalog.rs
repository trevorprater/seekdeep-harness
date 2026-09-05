//! Rust-owned generator for the durable Session event vocabulary.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
};

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Declaration, PropertyKey, Statement, TSInterfaceDeclaration, TSLiteral,
    TSModuleDeclarationBody, TSModuleDeclarationName, TSSignature, TSType,
};
use oxc_codegen::{Codegen, CodegenOptions, CommentOptions};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType, Span};
use walkdir::WalkDir;

const DOC_OUTPUT: &str = "docs/persistence-catalog.md";
const RUST_OUTPUT: &str = "crates/core/src/known_event_types.rs";
const FENCE: &str = "ts persistence-catalog";
const SESSION_PACKAGE: &str = "@deepseek-ai/dsh-session";
const SESSION_TYPES_MODULE: &str = "@deepseek-ai/dsh-session/types";
const ENVELOPE_NAMES: [&str; 4] = [
    "SessionEventType",
    "SurfaceEventType",
    "SurfaceOp",
    "SessionEvent",
];

/// One durable log-event declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogEventEntry {
    /// Scoped event name.
    pub name: String,
    /// Prefix before the first slash.
    pub scope: String,
    /// Single-line payload type.
    pub payload: String,
    /// Complete declaration with leading `JSDoc`.
    pub declaration: String,
    /// Collapsed description prose.
    pub doc: String,
    /// Repository-relative source pointer.
    pub source: String,
}

/// A durable event plus its surface eligibility.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnnotatedLogEventEntry {
    /// Base event declaration.
    pub event: LogEventEntry,
    /// Whether the event contributes to the model-visible surface.
    pub surface: bool,
}

/// One owning event-envelope declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventEnvelopeTypeEntry {
    /// Canonical declaration name.
    pub name: String,
    /// Complete declaration with leading `JSDoc`.
    pub declaration: String,
    /// Repository-relative source pointer.
    pub source: String,
}

#[derive(Clone, Debug)]
struct ParsedDoc {
    doc: String,
    has_mode: bool,
}

/// Generates or checks both persistence-catalog artifacts.
///
/// # Errors
///
/// Returns source parsing, validation, rendering, I/O, or freshness failures.
pub fn run(repo_root: &Path, source_root: &Path, check: bool) -> anyhow::Result<()> {
    let events = annotate_surface(
        collect_log_events(source_root)?,
        &collect_surface_event_types(source_root)?,
    )?;
    let artifacts = [
        (
            DOC_OUTPUT,
            render(&events, &collect_event_envelope_types(source_root)?),
        ),
        (RUST_OUTPUT, render_known_event_types(&events)),
    ];
    if check {
        let stale = artifacts
            .iter()
            .filter_map(|(path, content)| {
                let current = std::fs::read_to_string(repo_root.join(path)).ok();
                (current.as_deref() != Some(content.as_str())).then_some(*path)
            })
            .collect::<Vec<_>>();
        anyhow::ensure!(
            stale.is_empty(),
            "gen-persistence-catalog: {} stale. Run `cargo xtask persistence-catalog` and commit the result.",
            stale.join(", ")
        );
        println!(
            "gen-persistence-catalog: {} are up to date.",
            artifacts
                .iter()
                .map(|(path, _)| *path)
                .collect::<Vec<_>>()
                .join(", ")
        );
        return Ok(());
    }
    for (path, content) in artifacts {
        std::fs::write(repo_root.join(path), content)?;
        println!("gen-persistence-catalog: wrote {path}.");
    }
    Ok(())
}

fn source_files(scan_root: &Path) -> Vec<PathBuf> {
    let mut files = WalkDir::new(scan_root.join("packages"))
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| {
            let relative = entry.path().strip_prefix(scan_root).ok()?;
            let components = relative
                .components()
                .map(|component| component.as_os_str().to_str())
                .collect::<Option<Vec<_>>>()?;
            (components.len() >= 5
                && components.first() == Some(&"packages")
                && components.get(3) == Some(&"src")
                && entry.path().extension().and_then(std::ffi::OsStr::to_str) == Some("ts"))
            .then(|| relative.to_path_buf())
        })
        .collect::<Vec<_>>();
    files.sort();
    files
}

fn slash(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn package_name_for(relative: &Path, scan_root: &Path) -> Option<String> {
    let directory = relative.components().take(3).collect::<PathBuf>();
    let manifest = std::fs::read_to_string(scan_root.join(directory).join("package.json")).ok()?;
    serde_json::from_str::<serde_json::Value>(&manifest)
        .ok()?
        .get("name")?
        .as_str()
        .map(str::to_owned)
}

fn parse_program<'a>(
    allocator: &'a Allocator,
    source: &'a str,
    path: &Path,
) -> anyhow::Result<oxc_ast::ast::Program<'a>> {
    let parsed = Parser::new(allocator, source, SourceType::ts()).parse();
    anyhow::ensure!(
        parsed.errors.is_empty(),
        "gen-persistence-catalog: failed to parse {}: {}",
        path.display(),
        parsed
            .errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    );
    Ok(parsed.program)
}

fn exported_interface<'a>(
    statement: &'a Statement<'a>,
) -> Option<(&'a TSInterfaceDeclaration<'a>, bool, Span)> {
    match statement {
        Statement::TSInterfaceDeclaration(declaration) => {
            Some((declaration, false, declaration.span))
        }
        Statement::ExportNamedDeclaration(export) => match export.declaration.as_ref()? {
            Declaration::TSInterfaceDeclaration(declaration) => {
                Some((declaration, true, export.span))
            }
            _ => None,
        },
        _ => None,
    }
}

fn exported_type_alias<'a>(
    statement: &'a Statement<'a>,
) -> Option<(&'a oxc_ast::ast::TSTypeAliasDeclaration<'a>, bool, Span)> {
    match statement {
        Statement::TSTypeAliasDeclaration(declaration) => {
            Some((declaration, false, declaration.span))
        }
        Statement::ExportNamedDeclaration(export) => match export.declaration.as_ref()? {
            Declaration::TSTypeAliasDeclaration(declaration) => {
                Some((declaration, true, export.span))
            }
            _ => None,
        },
        _ => None,
    }
}

fn module_declaration<'a>(
    statement: &'a Statement<'a>,
) -> Option<&'a oxc_ast::ast::TSModuleDeclaration<'a>> {
    match statement {
        Statement::TSModuleDeclaration(declaration) => Some(declaration),
        Statement::ExportNamedDeclaration(export) => match export.declaration.as_ref()? {
            Declaration::TSModuleDeclaration(declaration) => Some(declaration),
            _ => None,
        },
        _ => None,
    }
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
    if !tail.get(close..)?.trim().is_empty() {
        return None;
    }
    Some((start, tail.get(..close)?))
}

fn joined_doc_lines(parts: &[String]) -> String {
    parts
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn flush_doc_blocks(
    blocks: &mut Vec<String>,
    paragraph: &mut Vec<String>,
    list: &mut Vec<String>,
    item: &mut Vec<String>,
) {
    if !item.is_empty() {
        list.push(joined_doc_lines(item));
        item.clear();
    }
    if !list.is_empty() {
        blocks.push(list.join("\n"));
        list.clear();
    }
    if !paragraph.is_empty() {
        blocks.push(joined_doc_lines(paragraph));
        paragraph.clear();
    }
}

fn parse_jsdoc(raw: Option<&str>) -> ParsedDoc {
    let Some(raw) = raw else {
        return ParsedDoc {
            doc: String::new(),
            has_mode: false,
        };
    };
    let body = raw
        .strip_prefix("/**")
        .and_then(|value| value.strip_suffix("*/"))
        .unwrap_or(raw);
    let lines = body
        .lines()
        .map(|line| {
            let line = line.trim_end();
            let line = line.trim_start();
            line.strip_prefix('*')
                .map_or(line, |line| line.strip_prefix(' ').unwrap_or(line))
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>();
    let mut blocks = Vec::new();
    let mut paragraph = Vec::new();
    let mut list = Vec::new();
    let mut item = Vec::new();
    let mut in_tags = false;
    let mut has_mode = false;
    for line in lines {
        let tag = line.trim_start();
        if tag.starts_with("@mode") {
            has_mode = true;
            flush_doc_blocks(&mut blocks, &mut paragraph, &mut list, &mut item);
            in_tags = true;
            continue;
        }
        if tag.starts_with('@') {
            flush_doc_blocks(&mut blocks, &mut paragraph, &mut list, &mut item);
            in_tags = true;
            continue;
        }
        if in_tags {
            continue;
        }
        if line.trim().is_empty() {
            flush_doc_blocks(&mut blocks, &mut paragraph, &mut list, &mut item);
        } else if line.starts_with("- ") {
            if !item.is_empty() {
                list.push(joined_doc_lines(&item));
                item.clear();
            }
            if !paragraph.is_empty() {
                blocks.push(joined_doc_lines(&paragraph));
                paragraph.clear();
            }
            item.push(line);
        } else if !item.is_empty() {
            item.push(line);
        } else {
            paragraph.push(line);
        }
    }
    flush_doc_blocks(&mut blocks, &mut paragraph, &mut list, &mut item);
    let mut doc = blocks.join("\n\n");
    while let Some(start) = doc.find("{@link ") {
        let Some(relative_end) = doc[start..].find('}') else {
            break;
        };
        let end = start + relative_end;
        let label = doc[start + "{@link ".len()..end].to_owned();
        doc.replace_range(start..=end, &label);
    }
    ParsedDoc {
        doc: doc.trim().to_owned(),
        has_mode,
    }
}

fn declaration_text(source: &str, span: Span) -> String {
    let node_start = usize::try_from(span.start)
        .unwrap_or(usize::MAX)
        .min(source.len());
    let end = usize::try_from(span.end)
        .unwrap_or(source.len())
        .min(source.len());
    let start = leading_jsdoc(source, node_start).map_or(node_start, |(start, _)| start);
    let line_start = source[..start].rfind('\n').map_or(0, |index| index + 1);
    let indent = &source[line_start..start];
    source[line_start..end]
        .lines()
        .map(|line| line.strip_prefix(indent).unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_owned()
}

fn payload_text(raw: &str) -> anyhow::Result<String> {
    let source = format!("type __Payload = {raw};");
    let allocator = Allocator::default();
    let program = parse_program(&allocator, &source, Path::new("payload.ts"))?;
    let printed = Codegen::new()
        .with_options(CodegenOptions {
            comments: CommentOptions::disabled(),
            ..CodegenOptions::default()
        })
        .build(&program)
        .code;
    let (_, payload) = printed
        .split_once('=')
        .ok_or_else(|| anyhow::anyhow!("payload printer omitted its assignment"))?;
    let payload = payload.trim().trim_end_matches(';').trim();
    let mut collapsed = payload.split_whitespace().collect::<Vec<_>>().join(" ");
    while collapsed.contains("; }") {
        collapsed = collapsed.replace("; }", " }");
    }
    Ok(collapsed)
}

fn report_violations(violations: &[String]) -> anyhow::Result<()> {
    anyhow::ensure!(
        violations.is_empty(),
        "gen-persistence-catalog: {} JSDoc completeness violation(s) (see AGENTS.md):\n{}",
        violations.len(),
        violations
            .iter()
            .map(|violation| format!("  {violation}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    Ok(())
}

/// Collects and validates every owning or declaration-merged Session event.
///
/// # Errors
///
/// Returns parse, ownership, shape, documentation, inheritance, or duplicate failures.
#[allow(
    clippy::too_many_lines,
    reason = "the source declaration contract is one closed validation walk"
)]
pub fn collect_log_events(scan_root: &Path) -> anyhow::Result<Vec<LogEventEntry>> {
    let mut entries = Vec::new();
    let mut violations = Vec::new();
    let mut seen = HashMap::<String, String>::new();
    let mut owning_declaration = None::<String>;
    for relative in source_files(scan_root) {
        let absolute = scan_root.join(&relative);
        let source = std::fs::read_to_string(&absolute)?;
        if !source.contains("SessionEventMap") {
            continue;
        }
        let allocator = Allocator::default();
        let program = parse_program(&allocator, &source, &absolute)?;
        let mut declarations = Vec::new();
        for statement in &program.body {
            if let Some((declaration, exported, span)) = exported_interface(statement)
                && declaration.id.name == "SessionEventMap"
            {
                declarations.push((declaration, true, exported, span));
            }
            let Some(module) = module_declaration(statement) else {
                continue;
            };
            let TSModuleDeclarationName::StringLiteral(name) = &module.id else {
                continue;
            };
            if name.value != SESSION_TYPES_MODULE {
                continue;
            }
            let Some(TSModuleDeclarationBody::TSModuleBlock(block)) = module.body.as_ref() else {
                continue;
            };
            for statement in &block.body {
                if let Some((declaration, exported, span)) = exported_interface(statement)
                    && declaration.id.name == "SessionEventMap"
                {
                    declarations.push((declaration, false, exported, span));
                }
            }
        }
        for (declaration, top_level, exported, declaration_span) in declarations {
            let declaration_source = pointer(&relative, &source, declaration_span);
            if top_level {
                let package = package_name_for(&relative, scan_root);
                if package.as_deref() != Some(SESSION_PACKAGE) {
                    violations.push(format!(
                        "top-level interface SessionEventMap ({declaration_source}) is outside {SESSION_PACKAGE} (package {}). Rename the interface, or contribute events via declare module '{SESSION_TYPES_MODULE}'.",
                        package.as_deref().unwrap_or("unknown")
                    ));
                    continue;
                }
                if !exported {
                    violations.push(format!(
                        "top-level interface SessionEventMap ({declaration_source}) is not exported; the owning vocabulary is the single exported declaration — rename a local helper interface."
                    ));
                    continue;
                }
                if let Some(prior) = &owning_declaration {
                    violations.push(format!(
                        "top-level interface SessionEventMap ({declaration_source}) is already declared at {prior}; the owning vocabulary has exactly one home."
                    ));
                    continue;
                }
                owning_declaration = Some(declaration_source.clone());
            }
            if !declaration.extends.is_empty() {
                violations.push(format!(
                    "SessionEventMap declaration ({declaration_source}) uses extends; inherited keys would join keyof SessionEventMap without a catalog row — declare event members directly."
                ));
            }
            for member in &declaration.body.body {
                let member_span = member.span();
                let source_pointer = pointer(&relative, &source, member_span);
                let TSSignature::TSPropertySignature(property) = member else {
                    let label = source
                        .get(
                            usize::try_from(member_span.start).unwrap_or(0)
                                ..usize::try_from(member_span.end).unwrap_or(0),
                        )
                        .unwrap_or("")
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ");
                    violations.push(format!(
                        "SessionEventMap member {label} ({source_pointer}) is not a property signature with an explicit payload type; declare every log event as 'scope/name': <payload>."
                    ));
                    continue;
                };
                let Some(annotation) = property.type_annotation.as_ref() else {
                    let label = source
                        .get(
                            usize::try_from(property.key.span().start).unwrap_or(0)
                                ..usize::try_from(property.key.span().end).unwrap_or(0),
                        )
                        .unwrap_or("");
                    violations.push(format!(
                        "SessionEventMap member {label} ({source_pointer}) is not a property signature with an explicit payload type; declare every log event as 'scope/name': <payload>."
                    ));
                    continue;
                };
                let PropertyKey::StringLiteral(literal) = &property.key else {
                    violations.push(format!(
                        "log event at {source_pointer} has a non-literal name; the catalog needs string-literal event names."
                    ));
                    continue;
                };
                let name = literal.value.to_string();
                let where_ = format!("log event '{name}' ({source_pointer})");
                if let Some(prior) = seen.get(&name) {
                    violations.push(format!(
                        "{where_} is already declared at {prior}; an event type has exactly one declaration."
                    ));
                    continue;
                }
                seen.insert(name.clone(), source_pointer.clone());
                let type_span = annotation.type_annotation.span();
                let raw_type = source
                    .get(
                        usize::try_from(type_span.start).unwrap_or(0)
                            ..usize::try_from(type_span.end).unwrap_or(0),
                    )
                    .unwrap_or("");
                let payload = payload_text(raw_type)?;
                let doc = parse_jsdoc(
                    leading_jsdoc(&source, usize::try_from(member_span.start).unwrap_or(0))
                        .map(|(_, raw)| raw),
                );
                if doc.has_mode {
                    violations.push(format!(
                        "{where_} carries an @mode tag, but a log event has no dispatch mode (it is not a cordis bus event — it rides the 'session/event' emit). Remove the tag."
                    ));
                }
                if doc.doc.is_empty() {
                    violations.push(format!(
                        "{where_} has no description prose. Say what the event records and what its payload means — the JSDoc becomes the catalog entry."
                    ));
                }
                entries.push(LogEventEntry {
                    scope: name.split('/').next().unwrap_or(&name).to_owned(),
                    name,
                    payload,
                    declaration: declaration_text(&source, member_span),
                    doc: doc.doc,
                    source: source_pointer,
                });
            }
        }
    }
    report_violations(&violations)?;
    Ok(entries)
}

/// Collects the four exported event-envelope declarations in canonical order.
///
/// # Errors
///
/// Returns parse, export, documentation, duplicate, or missing-declaration failures.
pub fn collect_event_envelope_types(
    scan_root: &Path,
) -> anyhow::Result<Vec<EventEnvelopeTypeEntry>> {
    let mut found = BTreeMap::<String, EventEnvelopeTypeEntry>::new();
    let mut violations = Vec::new();
    for relative in source_files(scan_root) {
        if package_name_for(&relative, scan_root).as_deref() != Some(SESSION_PACKAGE) {
            continue;
        }
        let absolute = scan_root.join(&relative);
        let source = std::fs::read_to_string(&absolute)?;
        if !ENVELOPE_NAMES.iter().any(|name| source.contains(name)) {
            continue;
        }
        let allocator = Allocator::default();
        let program = parse_program(&allocator, &source, &absolute)?;
        for statement in &program.body {
            let Some((declaration, exported, span)) = exported_type_alias(statement) else {
                continue;
            };
            let name = declaration.id.name.as_str();
            if !ENVELOPE_NAMES.contains(&name) {
                continue;
            }
            let source_pointer = pointer(&relative, &source, span);
            let where_ = format!("event-envelope type '{name}' ({source_pointer})");
            if let Some(prior) = found.get(name) {
                violations.push(format!(
                    "{where_} is already declared at {}; the persisted envelope type has exactly one owner.",
                    prior.source
                ));
                continue;
            }
            if !exported {
                violations.push(format!("{where_} is not exported."));
            }
            let doc = parse_jsdoc(
                leading_jsdoc(&source, usize::try_from(span.start).unwrap_or(0))
                    .map(|(_, raw)| raw),
            );
            if doc.has_mode {
                violations.push(format!(
                    "{where_} carries an @mode tag, but a persisted type has no dispatch mode."
                ));
            }
            if doc.doc.is_empty() {
                violations.push(format!(
                    "{where_} has no description prose. The full JSDoc is part of the generated catalog."
                ));
            }
            found.insert(
                name.to_owned(),
                EventEnvelopeTypeEntry {
                    name: name.to_owned(),
                    declaration: declaration_text(&source, span),
                    source: source_pointer,
                },
            );
        }
    }
    let missing = ENVELOPE_NAMES
        .iter()
        .filter(|name| !found.contains_key(**name))
        .copied()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        violations.push(format!(
            "missing event-envelope declaration(s): {}.",
            missing.join(", ")
        ));
    }
    report_violations(&violations)?;
    ENVELOPE_NAMES
        .iter()
        .map(|name| {
            found.remove(*name).ok_or_else(|| {
                anyhow::anyhow!("missing checked event-envelope declaration '{name}'")
            })
        })
        .collect()
}

fn literal_type_name(type_: &TSType<'_>) -> Option<String> {
    let TSType::TSLiteralType(literal) = type_ else {
        return None;
    };
    let TSLiteral::StringLiteral(value) = &literal.literal else {
        return None;
    };
    Some(value.value.to_string())
}

/// Collects the closed `SurfaceEventType` string-literal union.
///
/// # Errors
///
/// Returns parse, missing, duplicate, or non-string-union failures.
pub fn collect_surface_event_types(scan_root: &Path) -> anyhow::Result<Vec<String>> {
    let mut found = Vec::<(Vec<String>, String)>::new();
    for relative in source_files(scan_root) {
        let absolute = scan_root.join(&relative);
        let source = std::fs::read_to_string(&absolute)?;
        if !source.contains("SurfaceEventType") {
            continue;
        }
        let allocator = Allocator::default();
        let program = parse_program(&allocator, &source, &absolute)?;
        for statement in &program.body {
            let Some((declaration, _, span)) = exported_type_alias(statement) else {
                continue;
            };
            if declaration.id.name != "SurfaceEventType" {
                continue;
            }
            let source_pointer = pointer(&relative, &source, span);
            let types = match &declaration.type_annotation {
                TSType::TSUnionType(union) => union.types.iter().collect::<Vec<_>>(),
                type_ => vec![type_],
            };
            let mut names = Vec::new();
            for type_ in types {
                let Some(name) = literal_type_name(type_) else {
                    anyhow::bail!(
                        "gen-persistence-catalog: SurfaceEventType ({source_pointer}) has a non-string-literal member; the badge derivation needs a closed literal union."
                    );
                };
                names.push(name);
            }
            found.push((names, source_pointer));
        }
    }
    let Some((names, _)) = found.first() else {
        anyhow::bail!(
            "gen-persistence-catalog: no SurfaceEventType union found under packages/*/*/src."
        );
    };
    anyhow::ensure!(
        found.len() == 1,
        "gen-persistence-catalog: SurfaceEventType is declared more than once ({}); the surface subset has exactly one owner.",
        found
            .iter()
            .map(|(_, source)| source.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(names.clone())
}

/// Adds surface/log-only classification and rejects stale union members.
///
/// # Errors
///
/// Returns when a surface member has no event declaration.
pub fn annotate_surface(
    events: Vec<LogEventEntry>,
    surface_types: &[String],
) -> anyhow::Result<Vec<AnnotatedLogEventEntry>> {
    let names = events
        .iter()
        .map(|event| event.name.as_str())
        .collect::<BTreeSet<_>>();
    let stale = surface_types
        .iter()
        .filter(|name| !names.contains(name.as_str()))
        .collect::<Vec<_>>();
    anyhow::ensure!(
        stale.is_empty(),
        "gen-persistence-catalog: SurfaceEventType member(s) {} name no declared log event (stale union member?).",
        stale
            .iter()
            .map(|name| format!("'{name}'"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let surface = surface_types.iter().collect::<BTreeSet<_>>();
    Ok(events
        .into_iter()
        .map(|event| AnnotatedLogEventEntry {
            surface: surface.contains(&event.name),
            event,
        })
        .collect())
}

fn link_map() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        ("CallId", "core.md"),
        ("ContentBlock", "core.md"),
        ("MessageSource", "core.md"),
        ("ScheduleChange", "schedule.md"),
        ("SessionTitleEventData", "session-title.md"),
        ("SessionTitleLlmRequestEventData", "session-title.md"),
        ("SessionTitleModelProvenance", "session-title.md"),
        ("SessionTitleProviderId", "session-title.md"),
        ("SessionTitleSource", "session-title.md"),
        ("StreamChunk", "llm-streaming.md"),
        ("TodoItem", "session.md"),
        ("TokenUsage", "llm-streaming.md"),
        ("TurnEndReason", "session.md"),
        ("TurnTrigger", "session.md"),
    ])
}

fn type_links(payload: &str) -> String {
    let links = link_map()
        .into_iter()
        .filter(|(name, _)| {
            payload
                .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
                .any(|word| word == *name)
        })
        .map(|(name, page)| format!("[{name}](subsystems/{page})"))
        .collect::<Vec<_>>();
    if links.is_empty() {
        String::new()
    } else {
        format!("Types: {}", links.join(" · "))
    }
}

fn github_slug(value: &str) -> String {
    value
        .to_lowercase()
        .chars()
        .filter_map(|character| {
            if character.is_alphanumeric() || character == '-' {
                Some(character)
            } else if character.is_whitespace() {
                Some('-')
            } else {
                None
            }
        })
        .collect()
}

fn source_file(pointer: &str) -> &str {
    pointer.split(':').next().unwrap_or(pointer)
}

fn render_event(entry: &AnnotatedLogEventEntry) -> Vec<String> {
    let event = &entry.event;
    let badge = if entry.surface { "surface" } else { "log-only" };
    let heading = format!("{} — {badge}", event.name);
    let mut output = vec![
        format!("<a id=\"{}\"></a>", github_slug(&heading)),
        String::new(),
        format!("#### `{}` — {badge}", event.name),
        String::new(),
        format!("```{FENCE}"),
        event.declaration.clone(),
        "```".to_owned(),
        String::new(),
    ];
    let links = type_links(&event.payload);
    if !links.is_empty() {
        output.extend([links, String::new()]);
    }
    output.extend([
        format!(
            "Source: [`{}`](../{})",
            event.source,
            source_file(&event.source)
        ),
        String::new(),
    ]);
    output
}

/// Renders the complete Markdown persistence catalog.
#[must_use]
pub fn render(
    events: &[AnnotatedLogEventEntry],
    envelope_types: &[EventEnvelopeTypeEntry],
) -> String {
    let mut lines = vec![
        "<!-- Generated by cargo xtask persistence-catalog — do not edit by hand.".to_owned(),
        "     Run `pnpm run gen-persistence-catalog` to regenerate. -->".to_owned(),
        String::new(),
        "# Session Persistence Event Catalog".to_owned(),
        String::new(),
        "Every event type that can appear in a session's durable event log: the complete persisted `SessionEvent` envelope and each member of the merge-extensible `SessionEventMap` — the owning vocabulary in `@deepseek-ai/dsh-session` plus every plugin declaration merge into `@deepseek-ai/dsh-session/types` in this repo — with source JSDoc, full payload declaration, surface badge, and declaration site. It complements [session.md](subsystems/session.md) (surface ordering and the `deriveMessages()` projection), [persistence.md](subsystems/persistence.md) (how the log is made durable), and the generated region of [session.md](subsystems/session.md#cordis-surface) (the live bus wiring — a log event is NOT a cordis event; it reaches listeners via the single `session/event` emit).".to_owned(),
        String::new(),
        "This file is GENERATED from source (`cargo xtask persistence-catalog`) and verified fresh by `pnpm run verify-persistence-catalog` (part of `doc-sync`) — do not edit it by hand. Declaration blocks retain the source declaration and nested property JSDoc, removing only the indentation imposed by a containing interface/module, and use a `ts persistence-catalog` fence (skipped by doc-typecheck because declarations reference types from their owning modules). Type names in a payload link to the page that documents them. See [the persistence-log-catalog Agent Note](../.agents/notes/archived/process/2026-07-04-persistence-log-catalog.md).".to_owned(),
        String::new(),
        "The envelope declarations below compose each event's `type`, monotonic `seq`, epoch-ms `time`, `data`, the optional `ignorable` unknown-type skip marker, and the conditional `surfaceOp`/`sourceEventSeqs` fields. **surface** marks a `SurfaceEventType` member: it produces an LLM message and declares how it joins the surface list. **log-only** marks everything else: a durable, replayable record with no derived-history contribution. Every payload is JSON-serializable (enforced at `Session.append`), and the whole format is pinned at `SESSION_FORMAT_VERSION = 0` — pre-release, no compatibility implied ([the version stance](subsystems/persistence.md)). Scope: the packages in this repo; a downstream plugin can merge further event types, which are outside this catalog by construction.".to_owned(),
        String::new(),
        "## Event envelope".to_owned(),
        String::new(),
        format!("```{FENCE}"),
        envelope_types
            .iter()
            .map(|entry| entry.declaration.as_str())
            .collect::<Vec<_>>()
            .join("\n\n"),
        "```".to_owned(),
        String::new(),
        format!(
            "Sources: {}",
            envelope_types
                .iter()
                .map(|entry| format!(
                    "[`{}`](../{})",
                    entry.source,
                    source_file(&entry.source)
                ))
                .collect::<Vec<_>>()
                .join(" · ")
        ),
        String::new(),
        "## Events".to_owned(),
        String::new(),
    ];
    let scopes = events
        .iter()
        .map(|entry| entry.event.scope.as_str())
        .collect::<BTreeSet<_>>();
    for scope in scopes {
        lines.extend([format!("### `{scope}/*`"), String::new()]);
        let mut scoped_events = events
            .iter()
            .filter(|entry| entry.event.scope == scope)
            .collect::<Vec<_>>();
        scoped_events.sort_by(|left, right| left.event.name.cmp(&right.event.name));
        for event in scoped_events {
            lines.extend(render_event(event));
        }
    }
    lines
        .join("\n")
        .replace("@deepseek-ai/dsh-", "@seekdeep-ai/seekdeep-")
        .replace("`dsh-", "`seekdeep-")
}

/// Renders the Rust runtime known-event vocabulary.
#[must_use]
pub fn render_known_event_types(events: &[AnnotatedLogEventEntry]) -> String {
    let names = events
        .iter()
        .map(|entry| entry.event.name.as_str())
        .collect::<BTreeSet<_>>();
    let mut lines = vec![
        "//! Repository-wide durable session-event vocabulary.".to_owned(),
        String::new(),
        "use std::{collections::HashSet, sync::LazyLock};".to_owned(),
        String::new(),
        "/// Every session event type understood by this source snapshot.".to_owned(),
        "pub static KNOWN_SESSION_EVENT_TYPES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {".to_owned(),
        "    [".to_owned(),
    ];
    lines.extend(names.into_iter().map(|name| format!("        \"{name}\",")));
    lines.extend([
        "    ]".to_owned(),
        "    .into_iter()".to_owned(),
        "    .collect()".to_owned(),
        "});".to_owned(),
        String::new(),
        "#[cfg(test)]".to_owned(),
        "mod tests {".to_owned(),
        "    use super::*;".to_owned(),
        String::new(),
        "    #[test]".to_owned(),
        "    fn catalog_contains_core_and_plugin_events() {".to_owned(),
        "        assert!(KNOWN_SESSION_EVENT_TYPES.contains(\"turn/start\"));".to_owned(),
        "        assert!(KNOWN_SESSION_EVENT_TYPES.contains(\"compaction/summary\"));".to_owned(),
        format!(
            "        assert_eq!(KNOWN_SESSION_EVENT_TYPES.len(), {});",
            events.len()
        ),
        "    }".to_owned(),
        "}".to_owned(),
        String::new(),
    ]);
    lines.join("\n")
}
