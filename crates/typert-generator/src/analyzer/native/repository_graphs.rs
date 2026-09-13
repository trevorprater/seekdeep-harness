//! Semantic package event relations over the repository TypeScript Program.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use indexmap::{IndexMap, IndexSet};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::{Result, text::locale_compare};

use super::{
    js::{self, JsMap, Scope, Val},
    paths,
    repository::TypeScriptProject,
};

/// One event's producer methods and listening packages, in discovery order.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventRelation {
    /// Producing package short name to deduplicated dispatch API names.
    pub dispatchers: IndexMap<String, IndexSet<String>>,
    /// Packages registering an `on` or `once` listener.
    pub listeners: IndexSet<String>,
}

/// Ordered event relations produced by the semantic compiler scan.
pub type EventRelations = IndexMap<String, EventRelation>;

/// One loaded package source selected by the source generator's path grammar.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageSource {
    /// Repository-relative source path.
    pub rel: String,
    /// Package directory name, without its group.
    pub pkg: String,
}

/// Selects loaded package sources in JavaScript locale order.
///
/// # Errors
/// Returns compiler source-file inspection failures.
pub fn collect_package_sources(project: &mut TypeScriptProject) -> Result<Vec<PackageSource>> {
    let mut sources = project
        .source_files()?
        .into_iter()
        .filter_map(|source| {
            package_name(&source.relative_path).map(|pkg| PackageSource {
                rel: source.relative_path,
                pkg,
            })
        })
        .collect::<Vec<_>>();
    sources.sort_by(|left, right| locale_compare(&left.rel, &right.rel));
    Ok(sources)
}

/// Resolves receiver types, literal unions, aliases, and local argument flow.
///
/// # Errors
/// Returns missing core type declarations or compiler query failures.
pub fn collect_event_relations(
    project: &mut TypeScriptProject,
    sources: &[PackageSource],
) -> Result<EventRelations> {
    let root = project.root().to_owned();
    project.with_program(|scope, ts, program, checker| {
        let mut collector = Collector::new(scope, ts, program, checker, root, sources)?;
        collector.collect()
    })
}

fn package_name(path: &str) -> Option<String> {
    static PATTERN: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"^packages/[^/]+/([^/]+)/src/.+\.ts$").expect("package source path")
    });
    PATTERN
        .captures(path)
        .and_then(|capture| capture.get(1))
        .map(|name| name.as_str().to_owned())
}

#[derive(Clone, Copy)]
enum ReceiverKind {
    Context,
    AgentDispatch,
    EventsService,
}

struct Source<'s> {
    package: String,
    file: Val<'s>,
}

struct Collector<'s, 'i, 'a> {
    scope: &'a mut Scope<'s, 'i>,
    ts: Val<'s>,
    checker: Val<'s>,
    root: PathBuf,
    sources: Vec<Source<'s>>,
    kinds: HashMap<&'static str, u32>,
    context_type: Val<'s>,
    agent_dispatch_type: Val<'s>,
    events_service_type: Val<'s>,
    excluded_type_flags: u32,
    string_literal_flag: u32,
    never_flag: u32,
    alias_flag: u32,
    const_flag: u32,
    child_callback: Val<'s>,
    child_holder: v8::Local<'s, v8::Array>,
    file_call_sites: JsMap<'s>,
    local_callee_proofs: JsMap<'s>,
    global_call_sites: Option<JsMap<'s>>,
    relations: EventRelations,
}

impl<'s, 'i, 'a> Collector<'s, 'i, 'a> {
    #[expect(
        clippy::too_many_lines,
        reason = "Capture one compiler scope's immutable kinds, flags, declarations, and identity maps together"
    )]
    fn new(
        scope: &'a mut Scope<'s, 'i>,
        ts: Val<'s>,
        program: Val<'s>,
        checker: Val<'s>,
        root: PathBuf,
        sources: &[PackageSource],
    ) -> Result<Self> {
        let mut kinds = HashMap::new();
        for name in [
            "CallExpression",
            "PropertyAccessExpression",
            "Identifier",
            "ShorthandPropertyAssignment",
            "FunctionDeclaration",
            "VariableDeclaration",
            "Parameter",
            "MethodDeclaration",
            "ObjectLiteralExpression",
            "ArrayLiteralExpression",
            "OmittedExpression",
            "SpreadElement",
            "ConditionalExpression",
            "ParenthesizedExpression",
            "AsExpression",
            "TypeAssertionExpression",
            "NonNullExpression",
            "SatisfiesExpression",
            "StringLiteral",
            "NoSubstitutionTemplateLiteral",
            "ExportKeyword",
            "DefaultKeyword",
        ] {
            kinds.insert(name, enum_value(scope, ts, "SyntaxKind", name)?);
        }
        let excluded_type_flags = enum_value(scope, ts, "TypeFlags", "Any")?
            | enum_value(scope, ts, "TypeFlags", "Unknown")?
            | enum_value(scope, ts, "TypeFlags", "Never")?;
        let string_literal_flag = enum_value(scope, ts, "TypeFlags", "StringLiteral")?;
        let never_flag = enum_value(scope, ts, "TypeFlags", "Never")?;
        let alias_flag = enum_value(scope, ts, "SymbolFlags", "Alias")?;
        let const_flag = enum_value(scope, ts, "NodeFlags", "Const")?;
        let context_type = declared_type(
            scope,
            ts,
            program,
            checker,
            &root,
            "vendor/cordis/src/context.ts",
            "Context",
        )?;
        let agent_dispatch_type = declared_type(
            scope,
            ts,
            program,
            checker,
            &root,
            "packages/core/agent/src/dispatch.ts",
            "AgentEventDispatch",
        )?;
        let events_service_type = declared_type(
            scope,
            ts,
            program,
            checker,
            &root,
            "vendor/cordis/src/events.ts",
            "EventsService",
        )?;
        let mut loaded = Vec::new();
        for source in sources {
            let path = js::string(scope, &paths::slash(&root.join(&source.rel)));
            let file = js::call(scope, program, "getSourceFile", &[path])?;
            if file.is_null_or_undefined() {
                return Err(js::failure(format!(
                    "TypeScript project did not load {}",
                    source.rel
                )));
            }
            loaded.push(Source {
                package: source.pkg.clone(),
                file,
            });
        }
        let child_holder = v8::Array::new(scope, 1);
        let child_callback = v8::Function::builder(collect_child)
            .data(child_holder.into())
            .build(scope)
            .ok_or_else(|| js::failure("event graph child callback allocation"))?
            .into();
        let file_call_sites = JsMap::new(scope);
        let local_callee_proofs = JsMap::new(scope);
        Ok(Self {
            scope,
            ts,
            checker,
            root,
            sources: loaded,
            kinds,
            context_type,
            agent_dispatch_type,
            events_service_type,
            excluded_type_flags,
            string_literal_flag,
            never_flag,
            alias_flag,
            const_flag,
            child_callback,
            child_holder,
            file_call_sites,
            local_callee_proofs,
            global_call_sites: None,
            relations: IndexMap::new(),
        })
    }

    fn collect(&mut self) -> Result<EventRelations> {
        for index in 0..self.sources.len() {
            let package = self.sources[index].package.clone();
            let mut pending = vec![self.sources[index].file];
            while let Some(node) = pending.pop() {
                if self.is(node, "CallExpression")? {
                    self.visit_call(node, &package)?;
                }
                pending.extend(self.children(node)?.into_iter().rev());
            }
        }
        Ok(std::mem::take(&mut self.relations))
    }

    fn children(&mut self, node: Val<'s>) -> Result<Vec<Val<'s>>> {
        let output = v8::Array::new(self.scope, 0);
        self.child_holder.set_index(self.scope, 0, output.into());
        js::call(
            self.scope,
            self.ts,
            "forEachChild",
            &[node, self.child_callback],
        )?;
        js::items(self.scope, output.into())
    }

    fn is(&mut self, node: Val<'s>, kind: &'static str) -> Result<bool> {
        Ok(!node.is_null_or_undefined()
            && js::get_flags(self.scope, node, "kind")? == self.kinds[kind])
    }

    fn predicate(&mut self, name: &str, value: Val<'s>) -> Result<bool> {
        Ok(js::call(self.scope, self.ts, name, &[value])?.boolean_value(self.scope))
    }

    fn source_file(&mut self, node: Val<'s>) -> Result<Val<'s>> {
        js::call(self.scope, node, "getSourceFile", &[])
    }

    fn exported(&mut self, node: Val<'s>) -> Result<bool> {
        if !self.predicate("canHaveModifiers", node)? {
            return Ok(false);
        }
        let modifiers = js::call(self.scope, self.ts, "getModifiers", &[node])?;
        if modifiers.is_null_or_undefined() {
            return Ok(false);
        }
        for modifier in js::items(self.scope, modifiers)? {
            if self.is(modifier, "ExportKeyword")? || self.is(modifier, "DefaultKeyword")? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn build_call_sites(&mut self, files: &[Val<'s>]) -> Result<JsMap<'s>> {
        let index = JsMap::new(self.scope);
        let mut pending = files.iter().copied().rev().collect::<Vec<_>>();
        while let Some(node) = pending.pop() {
            if self.is(node, "CallExpression")? {
                let signature =
                    js::call(self.scope, self.checker, "getResolvedSignature", &[node])?;
                if !signature.is_null_or_undefined() {
                    let declaration = js::get(self.scope, signature, "declaration")?;
                    if !declaration.is_null_or_undefined() {
                        let calls = index
                            .get(self.scope, declaration)
                            .unwrap_or_else(|| v8::Array::new(self.scope, 0).into());
                        let array = v8::Local::<v8::Array>::try_from(calls)
                            .map_err(|_| js::failure("call-site index array"))?;
                        array.set_index(self.scope, array.length(), node);
                        index.set(self.scope, declaration, calls);
                    }
                }
            }
            pending.extend(self.children(node)?.into_iter().rev());
        }
        Ok(index)
    }

    fn call_sites_for(&mut self, owner: Val<'s>) -> Result<Vec<Val<'s>>> {
        if self.global_call_sites.is_none() && !self.proven_local_callee(owner)? {
            let files = self
                .sources
                .iter()
                .map(|source| source.file)
                .collect::<Vec<_>>();
            self.global_call_sites = Some(self.build_call_sites(&files)?);
        }
        let index = if let Some(index) = self.global_call_sites {
            index
        } else {
            let file = self.source_file(owner)?;
            if let Some(index) = self.file_call_sites.get(self.scope, file) {
                JsMap::from_value(index)?
            } else {
                let index = self.build_call_sites(&[file])?;
                self.file_call_sites.set(self.scope, file, index.value());
                index
            }
        };
        index
            .get(self.scope, owner)
            .map_or_else(|| Ok(Vec::new()), |calls| js::items(self.scope, calls))
    }

    fn proven_local_callee(&mut self, owner: Val<'s>) -> Result<bool> {
        if let Some(cached) = self.local_callee_proofs.get(self.scope, owner) {
            return Ok(cached.boolean_value(self.scope));
        }
        let file = self.source_file(owner)?;
        let mut proven = !self.exported(owner)? && self.predicate("isExternalModule", file)?;
        let name = js::get(self.scope, owner, "name")?;
        let owner_symbol = if name.is_null_or_undefined() {
            js::undefined(self.scope)
        } else {
            js::call(self.scope, self.checker, "getSymbolAtLocation", &[name])?
        };
        proven &= !owner_symbol.is_null_or_undefined();
        if proven {
            let name_text = js::get_string(self.scope, name, "text")?;
            let mut pending = vec![file];
            while let Some(node) = pending.pop() {
                if self.is(node, "Identifier")?
                    && !js::same(node, name)
                    && js::get_string(self.scope, node, "text")? == name_text
                    && !self.direct_callee(node)?
                {
                    let parent = js::get(self.scope, node, "parent")?;
                    let local = if self.is(parent, "ShorthandPropertyAssignment")? {
                        js::call(
                            self.scope,
                            self.checker,
                            "getShorthandAssignmentValueSymbol",
                            &[parent],
                        )?
                    } else {
                        js::call(self.scope, self.checker, "getSymbolAtLocation", &[node])?
                    };
                    if !local.is_null_or_undefined() {
                        let symbol = self.unalias(local)?;
                        if js::same(symbol, owner_symbol) {
                            proven = false;
                            break;
                        }
                    }
                }
                pending.extend(self.children(node)?.into_iter().rev());
            }
        }
        let result = js::boolean(self.scope, proven);
        self.local_callee_proofs.set(self.scope, owner, result);
        Ok(proven)
    }

    fn direct_callee(&mut self, identifier: Val<'s>) -> Result<bool> {
        let mut current = identifier;
        loop {
            let parent = js::get(self.scope, current, "parent")?;
            if self.wrapper(parent)? {
                current = parent;
                continue;
            }
            return Ok(self.is(parent, "CallExpression")?
                && js::same(js::get(self.scope, parent, "expression")?, current));
        }
    }

    fn wrapper(&mut self, node: Val<'s>) -> Result<bool> {
        for kind in [
            "ParenthesizedExpression",
            "AsExpression",
            "TypeAssertionExpression",
            "NonNullExpression",
            "SatisfiesExpression",
        ] {
            if self.is(node, kind)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn unwrap(&mut self, mut expression: Val<'s>) -> Result<Val<'s>> {
        while self.wrapper(expression)? {
            expression = js::get(self.scope, expression, "expression")?;
        }
        Ok(expression)
    }

    fn unalias(&mut self, symbol: Val<'s>) -> Result<Val<'s>> {
        if js::get_flags(self.scope, symbol, "flags")? & self.alias_flag != 0 {
            js::call(self.scope, self.checker, "getAliasedSymbol", &[symbol])
        } else {
            Ok(symbol)
        }
    }

    fn visit_call(&mut self, call: Val<'s>, package: &str) -> Result<()> {
        let expression = js::get(self.scope, call, "expression")?;
        let arguments = js::get_items(self.scope, call, "arguments")?;
        if self.agent_event_emitter(expression)? {
            if let Some(event) = arguments.get(2).copied() {
                for name in self.finite_strings(event)?.unwrap_or_default() {
                    self.add_dispatcher(&name, package, "emitAgentEvent");
                }
            }
        } else if self.is(expression, "PropertyAccessExpression")? {
            let name = js::get(self.scope, expression, "name")?;
            let method = js::get_string(self.scope, name, "text")?;
            if ![
                "on",
                "once",
                "emit",
                "parallel",
                "serial",
                "waterfall",
                "dispatch",
            ]
            .contains(&method.as_str())
            {
                return Ok(());
            }
            let receiver = js::get(self.scope, expression, "expression")?;
            match self.receiver_kind(receiver)? {
                Some(ReceiverKind::EventsService) if method == "dispatch" => {
                    if let Some(argument_list) = arguments.get(1).copied() {
                        for event in self.events_from_arguments(argument_list, &[])? {
                            self.add_dispatcher(&event, package, "events.dispatch");
                        }
                    }
                }
                Some(kind @ (ReceiverKind::Context | ReceiverKind::AgentDispatch)) => {
                    let count = match kind {
                        ReceiverKind::Context => 2,
                        ReceiverKind::AgentDispatch => 1,
                        ReceiverKind::EventsService => unreachable!(),
                    };
                    let mut names = IndexSet::new();
                    for candidate in arguments.iter().take(count).copied() {
                        if let Some(values) = self.finite_strings(candidate)? {
                            names = values;
                            break;
                        }
                    }
                    if method == "on" || method == "once" {
                        for event in names {
                            self.relations
                                .entry(event)
                                .or_default()
                                .listeners
                                .insert(package.to_owned());
                        }
                    } else if ["emit", "parallel", "serial", "waterfall"].contains(&method.as_str())
                    {
                        for event in names {
                            self.add_dispatcher(&event, package, &method);
                        }
                    }
                }
                Some(ReceiverKind::EventsService) | None => {}
            }
        }
        Ok(())
    }

    fn agent_event_emitter(&mut self, expression: Val<'s>) -> Result<bool> {
        if !self.is(expression, "Identifier")? {
            return Ok(false);
        }
        let symbol = js::call(
            self.scope,
            self.checker,
            "getSymbolAtLocation",
            &[expression],
        )?;
        if symbol.is_null_or_undefined() {
            return Ok(false);
        }
        let symbol = self.unalias(symbol)?;
        for declaration in js::get_items(self.scope, symbol, "declarations")? {
            if !self.is(declaration, "FunctionDeclaration")? {
                continue;
            }
            let name = js::get(self.scope, declaration, "name")?;
            if name.is_null_or_undefined()
                || js::get_string(self.scope, name, "text")? != "emitAgentEvent"
            {
                continue;
            }
            let source = self.source_file(declaration)?;
            let file_name = js::get_string(self.scope, source, "fileName")?;
            if paths::relative(&self.root, Path::new(&file_name))
                == "packages/core/agent/src/dispatch.ts"
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn assignable(&mut self, source: Val<'s>, target: Val<'s>) -> Result<bool> {
        Ok(js::call(
            self.scope,
            self.checker,
            "isTypeAssignableTo",
            &[source, target],
        )?
        .boolean_value(self.scope))
    }

    fn receiver_kind(&mut self, receiver: Val<'s>) -> Result<Option<ReceiverKind>> {
        let value_type = js::call(self.scope, self.checker, "getTypeAtLocation", &[receiver])?;
        if js::get_flags(self.scope, value_type, "flags")? & self.excluded_type_flags != 0 {
            return Ok(None);
        }
        if self.assignable(value_type, self.events_service_type)? {
            Ok(Some(ReceiverKind::EventsService))
        } else if self.assignable(value_type, self.context_type)? {
            Ok(Some(ReceiverKind::Context))
        } else if self.assignable(value_type, self.agent_dispatch_type)? {
            Ok(Some(ReceiverKind::AgentDispatch))
        } else {
            Ok(None)
        }
    }

    fn events_from_arguments(
        &mut self,
        expression: Val<'s>,
        seen: &[Val<'s>],
    ) -> Result<IndexSet<String>> {
        let current = self.unwrap(expression)?;
        if seen.iter().any(|seen| js::same(*seen, current)) {
            return Ok(IndexSet::new());
        }
        let mut seen = seen.to_vec();
        seen.push(current);
        if self.is(current, "ArrayLiteralExpression")? {
            for element in js::get_items(self.scope, current, "elements")?
                .into_iter()
                .take(2)
            {
                if self.is(element, "OmittedExpression")? || self.is(element, "SpreadElement")? {
                    continue;
                }
                if let Some(values) = self.finite_strings(element)? {
                    return Ok(values);
                }
            }
            return Ok(IndexSet::new());
        }
        if self.is(current, "ConditionalExpression")? {
            let when_true = js::get(self.scope, current, "whenTrue")?;
            let when_false = js::get(self.scope, current, "whenFalse")?;
            let mut values = self.events_from_arguments(when_true, &seen)?;
            values.extend(self.events_from_arguments(when_false, &seen)?);
            return Ok(values);
        }
        if !self.is(current, "Identifier")? {
            return Ok(IndexSet::new());
        }
        let symbol = js::call(self.scope, self.checker, "getSymbolAtLocation", &[current])?;
        if symbol.is_null_or_undefined() {
            return Ok(IndexSet::new());
        }
        let mut events = IndexSet::new();
        for declaration in js::get_items(self.scope, symbol, "declarations")? {
            if self.is(declaration, "VariableDeclaration")? {
                let initializer = js::get(self.scope, declaration, "initializer")?;
                let parent = js::get(self.scope, declaration, "parent")?;
                if !initializer.is_null_or_undefined()
                    && js::get_flags(self.scope, parent, "flags")? & self.const_flag != 0
                {
                    events.extend(self.events_from_arguments(initializer, &seen)?);
                }
            } else if self.is(declaration, "Parameter")? {
                events.extend(self.events_from_parameter(declaration, &seen)?);
            }
        }
        Ok(events)
    }

    fn events_from_parameter(
        &mut self,
        parameter: Val<'s>,
        seen: &[Val<'s>],
    ) -> Result<IndexSet<String>> {
        let owner = js::get(self.scope, parameter, "parent")?;
        if !self.is(owner, "FunctionDeclaration")? || self.exported(owner)? {
            return Ok(IndexSet::new());
        }
        let parameters = js::get_items(self.scope, owner, "parameters")?;
        let Some(index) = parameters
            .iter()
            .position(|value| js::same(*value, parameter))
        else {
            return Ok(IndexSet::new());
        };
        let mut events = IndexSet::new();
        for call in self.call_sites_for(owner)? {
            if let Some(argument) = js::get_items(self.scope, call, "arguments")?
                .get(index)
                .copied()
            {
                events.extend(self.events_from_arguments(argument, seen)?);
            }
        }
        Ok(events)
    }

    fn finite_strings(&mut self, expression: Val<'s>) -> Result<Option<IndexSet<String>>> {
        let current = self.unwrap(expression)?;
        if self.is(current, "StringLiteral")?
            || self.is(current, "NoSubstitutionTemplateLiteral")?
        {
            return Ok(Some(IndexSet::from([js::get_string(
                self.scope, current, "text",
            )?])));
        }
        if self.forwarded_agent_parameter(current)? {
            return Ok(None);
        }
        let value_type = js::call(self.scope, self.checker, "getTypeAtLocation", &[current])?;
        self.finite_type_strings(value_type)
    }

    fn finite_type_strings(&mut self, value_type: Val<'s>) -> Result<Option<IndexSet<String>>> {
        let flags = js::get_flags(self.scope, value_type, "flags")?;
        if flags & self.string_literal_flag != 0 {
            return Ok(Some(IndexSet::from([js::get_string(
                self.scope, value_type, "value",
            )?])));
        }
        if flags & self.never_flag != 0 {
            return Ok(Some(IndexSet::new()));
        }
        if !js::call(self.scope, value_type, "isUnion", &[])?.boolean_value(self.scope) {
            return Ok(None);
        }
        let mut values = IndexSet::new();
        for member in js::get_items(self.scope, value_type, "types")? {
            let Some(member_values) = self.finite_type_strings(member)? else {
                return Ok(None);
            };
            values.extend(member_values);
        }
        Ok(Some(values))
    }

    fn forwarded_agent_parameter(&mut self, expression: Val<'s>) -> Result<bool> {
        if !self.is(expression, "Identifier")? {
            return Ok(false);
        }
        let symbol = js::call(
            self.scope,
            self.checker,
            "getSymbolAtLocation",
            &[expression],
        )?;
        if symbol.is_null_or_undefined() {
            return Ok(false);
        }
        for declaration in js::get_items(self.scope, symbol, "declarations")? {
            if !self.is(declaration, "Parameter")? {
                continue;
            }
            let method = js::get(self.scope, declaration, "parent")?;
            if !self.is(method, "MethodDeclaration")? {
                continue;
            }
            let object = js::get(self.scope, method, "parent")?;
            if !self.is(object, "ObjectLiteralExpression")? {
                continue;
            }
            let contextual = js::call(self.scope, self.checker, "getContextualType", &[object])?;
            if !contextual.is_null_or_undefined()
                && self.assignable(contextual, self.agent_dispatch_type)?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn add_dispatcher(&mut self, event: &str, package: &str, method: &str) {
        self.relations
            .entry(event.to_owned())
            .or_default()
            .dispatchers
            .entry(package.to_owned())
            .or_default()
            .insert(method.to_owned());
    }
}

fn enum_value<'s>(scope: &mut Scope<'s, '_>, ts: Val<'s>, group: &str, name: &str) -> Result<u32> {
    let group = js::get(scope, ts, group)?;
    js::get_flags(scope, group, name)
}

fn declared_type<'s>(
    scope: &mut Scope<'s, '_>,
    ts: Val<'s>,
    program: Val<'s>,
    checker: Val<'s>,
    root: &Path,
    relative: &str,
    name: &str,
) -> Result<Val<'s>> {
    let path = js::string(scope, &paths::slash(&root.join(relative)));
    let file = js::call(scope, program, "getSourceFile", &[path])?;
    if file.is_null_or_undefined() {
        return Err(js::failure(format!(
            "TypeScript project did not load {relative}"
        )));
    }
    for statement in js::get_items(scope, file, "statements")? {
        if !js::call(scope, ts, "isClassDeclaration", &[statement])?.boolean_value(scope)
            && !js::call(scope, ts, "isInterfaceDeclaration", &[statement])?.boolean_value(scope)
        {
            continue;
        }
        let declaration_name = js::get(scope, statement, "name")?;
        if declaration_name.is_null_or_undefined()
            || js::get_string(scope, declaration_name, "text")? != name
        {
            continue;
        }
        let symbol = js::call(scope, checker, "getSymbolAtLocation", &[declaration_name])?;
        if symbol.is_null_or_undefined() {
            break;
        }
        return js::call(scope, checker, "getDeclaredTypeOfSymbol", &[symbol]);
    }
    Err(js::failure(format!(
        "cannot resolve TypeScript type {name} from {relative}"
    )))
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "V8 callback ABI passes its argument view by value"
)]
fn collect_child(
    scope: &mut Scope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    _: v8::ReturnValue,
) {
    let Ok(holder) = v8::Local::<v8::Array>::try_from(args.data()) else {
        return;
    };
    let Some(output) = holder.get_index(scope, 0) else {
        return;
    };
    let Ok(output) = v8::Local::<v8::Array>::try_from(output) else {
        return;
    };
    output.set_index(scope, output.length(), args.get(0));
}
