//! One independently compiled face: exports, services, events, and the type graph.
#![expect(
    clippy::too_many_lines,
    reason = "Each routine mirrors one source analyzer method so differential review stays line-aligned"
)]

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use indexmap::IndexMap;

use crate::{
    Result, TypertGeneratorError,
    analyzer::{
        is_remote_segment, is_standard_library_file, module_identity, package_export_targets,
        source_path_for_export,
    },
    model::{
        CrossFaceLink, DeclarationKind, DefinedMember, DefinedTypeNode, DocumentationModel,
        EnumMemberModel, EventModel, ExportModel, FaceModel, MemberBase, MemberId, MemberKind,
        MemberModel, ObjectModel, ObjectPassing, PackageModel, ParameterBinding, ParameterModel,
        SchemaModel, ServiceModel, SignatureModel, SourceLocation, SymbolId, TypeDeclarationModel,
        TypeDeclarationPartModel, TypeGraph, TypeNodeId, TypeNodeKind, TypeNodeModel,
        TypeOperatorName, TypeParameterId, TypeParameterModel, TypeTargetModel, TypertFace,
        Variance,
    },
    text::locale_compare,
};

use super::{
    js::{self, JsSet, Scope, Val},
    jstext, paths,
    syntax::{CORDIS_MODULES, PROTOCOL_MODULES, Syntax},
    workspace::{AnalysisMode, PackageRegistration, SourceEdit},
};

/// Analysis stop: a queued write-mode edit or a hard failure.
pub(crate) enum Interrupt {
    Queued(SourceEdit),
    Failed(TypertGeneratorError),
}

impl From<TypertGeneratorError> for Interrupt {
    fn from(error: TypertGeneratorError) -> Self {
        Self::Failed(error)
    }
}

pub(crate) type FaceResult<T> = std::result::Result<T, Interrupt>;

pub(crate) struct FaceAnalyzerOptions<'a, 's> {
    pub(crate) root: &'a Path,
    pub(crate) face: TypertFace,
    pub(crate) program: Val<'s>,
    pub(crate) registrations: &'a [PackageRegistration],
    pub(crate) all_registrations: &'a [PackageRegistration],
    pub(crate) mode: AnalysisMode,
    pub(crate) cross_face_links: &'a mut IndexMap<String, CrossFaceLink>,
}

#[derive(Clone)]
pub(crate) struct ExportRecord<'s> {
    pub(crate) model: ExportModel,
    pub(crate) symbol: Val<'s>,
    pub(crate) declaration: Val<'s>,
    pub(crate) source_file: Val<'s>,
}

pub(crate) struct StaticLookupDeclaration<'s> {
    pub(crate) key: String,
    pub(crate) host_symbol: SymbolId,
    pub(crate) wire_type: Val<'s>,
}

pub(crate) struct StaticContextDeclaration<'s> {
    pub(crate) key: String,
    pub(crate) wire_type: Val<'s>,
}

/// Annotation purpose named in missing-annotation diagnostics.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TypePurpose {
    Property,
    Parameter,
    Return,
}

impl TypePurpose {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Property => "property",
            Self::Parameter => "parameter",
            Self::Return => "return",
        }
    }
}

pub(crate) struct FaceAnalyzer<'a, 's, 'i> {
    pub(crate) scope: &'a mut Scope<'s, 'i>,
    pub(crate) syntax: &'a Syntax<'s>,
    pub(crate) root: PathBuf,
    pub(crate) face: TypertFace,
    pub(crate) program: Val<'s>,
    pub(crate) checker: Val<'s>,
    pub(crate) registrations: &'a [PackageRegistration],
    pub(crate) all_registrations: &'a [PackageRegistration],
    pub(crate) mode: AnalysisMode,
    cross_face_links: &'a mut IndexMap<String, CrossFaceLink>,
    pub(crate) source_files: HashMap<PathBuf, Val<'s>>,
    pub(crate) declarations: IndexMap<SymbolId, TypeDeclarationModel>,
    declaration_states: HashSet<SymbolId>,
    pub(crate) nodes: IndexMap<TypeNodeId, TypeNodeModel>,
    exports_by_package: HashMap<String, Vec<ExportRecord<'s>>>,
    node_ordinals: HashMap<String, usize>,
    pub(crate) static_lookups: Option<Vec<StaticLookupDeclaration<'s>>>,
    pub(crate) static_contexts: Option<IndexMap<String, StaticContextDeclaration<'s>>>,
}

impl<'a, 's, 'i> FaceAnalyzer<'a, 's, 'i> {
    pub(crate) fn new(
        scope: &'a mut Scope<'s, 'i>,
        syntax: &'a Syntax<'s>,
        options: FaceAnalyzerOptions<'a, 's>,
    ) -> Result<Self> {
        let FaceAnalyzerOptions {
            root,
            face,
            program,
            registrations,
            all_registrations,
            mode,
            cross_face_links,
        } = options;
        let checker = js::call(scope, program, "getTypeChecker", &[])?;
        let mut source_files = HashMap::new();
        let files = js::call(scope, program, "getSourceFiles", &[])?;
        for source_file in js::items(scope, files)? {
            let name = js::get_string(scope, source_file, "fileName")?;
            source_files.insert(paths::real_path(Path::new(&name)), source_file);
        }
        Ok(Self {
            scope,
            syntax,
            root: root.to_owned(),
            face,
            program,
            checker,
            registrations,
            all_registrations,
            mode,
            cross_face_links,
            source_files,
            declarations: IndexMap::new(),
            declaration_states: HashSet::new(),
            nodes: IndexMap::new(),
            exports_by_package: HashMap::new(),
            node_ordinals: HashMap::new(),
            static_lookups: None,
            static_contexts: None,
        })
    }

    pub(crate) fn analyze(&mut self) -> FaceResult<FaceModel> {
        for registration in self.registrations {
            let records = self.collect_exports(registration)?;
            self.exports_by_package
                .insert(registration.name.clone(), records);
        }
        let mut packages = Vec::new();
        for registration in self.registrations {
            let package = self.analyze_package(registration)?;
            if has_package_surface(&package) {
                packages.push(package);
            }
        }
        self.validate_invocation_identity(&packages)?;
        let mut declarations = self.declarations.values().cloned().collect::<Vec<_>>();
        declarations.sort_by(|left, right| locale_compare(left.id.as_str(), right.id.as_str()));
        let mut nodes = self.nodes.values().cloned().collect::<Vec<_>>();
        nodes.sort_by(|left, right| locale_compare(left.id().as_str(), right.id().as_str()));
        Ok(FaceModel {
            face: self.face,
            packages,
            graph: TypeGraph {
                declarations,
                nodes,
            },
        })
    }

    fn analyze_package(&mut self, registration: &PackageRegistration) -> FaceResult<PackageModel> {
        let records = self
            .exports_by_package
            .get(&registration.name)
            .cloned()
            .unwrap_or_default();
        let entry_files = records
            .iter()
            .map(|record| record.source_file)
            .collect::<Vec<_>>();
        let reachable = self.reachable_files(registration, &entry_files)?;
        let mut services = Vec::new();
        let mut events = Vec::new();
        for source_file in &reachable {
            for statement in js::get_items(self.scope, *source_file, "statements")? {
                let Some(body) = self
                    .syntax
                    .module_named(self.scope, statement, CORDIS_MODULES)?
                else {
                    continue;
                };
                for member in js::get_items(self.scope, body, "statements")? {
                    if !self.syntax.is(
                        self.scope,
                        member,
                        self.syntax.kinds.interface_declaration,
                    )? {
                        continue;
                    }
                    let name = js::get(self.scope, member, "name")?;
                    let name = js::get_string(self.scope, name, "text")?;
                    if name == "Context" {
                        services.extend(self.collect_services(member, &records)?);
                    } else if name == "Events" {
                        events.extend(self.collect_events(member)?);
                    }
                }
            }
        }
        let explicit_services = self.collect_explicit_services(&records)?;

        let mut objects = Vec::new();
        let mut schemas = Vec::new();
        let mut seen_business_symbols = HashSet::new();
        for record in &records {
            let declaration = record.declaration;
            if !self.syntax.is_type_declaration(self.scope, declaration)? {
                continue;
            }
            let file = self.syntax.file_name_of(self.scope, declaration)?;
            if self.registration_for_file(&file).is_none() {
                continue;
            }
            let symbol = self.resolve_symbol(record.symbol)?;
            let symbol_id = self.symbol_id(symbol)?;
            if seen_business_symbols.contains(&symbol_id) {
                continue;
            }
            let Some(mode) = self.syntax.typert_mode(self.scope, declaration)? else {
                continue;
            };
            seen_business_symbols.insert(symbol_id.clone());
            self.ensure_declaration(symbol, declaration)?;
            let documentation = self.syntax.documentation_of(self.scope, declaration)?;
            if mode == "object" {
                objects.push(ObjectModel {
                    documentation,
                    export: record.model.clone(),
                    symbol: symbol_id,
                    passing: ObjectPassing::Reference,
                });
            } else {
                let ty = self.reference_node(symbol, declaration)?;
                schemas.push(SchemaModel {
                    documentation,
                    export: record.model.clone(),
                    symbol: symbol_id,
                    ty,
                });
            }
        }

        let mut exports = records
            .iter()
            .map(|record| record.model.clone())
            .collect::<Vec<_>>();
        exports.sort_by(|left, right| {
            locale_compare(&left.subpath, &right.subpath)
                .then_with(|| locale_compare(&left.name, &right.name))
        });
        let mut unique_services = IndexMap::<String, ServiceModel>::new();
        for service in explicit_services.into_iter().chain(services) {
            unique_services
                .entry(service.key.clone())
                .or_insert(service);
        }
        let mut services = unique_services.into_values().collect::<Vec<_>>();
        services.sort_by(|left, right| locale_compare(&left.key, &right.key));
        let mut unique_events = IndexMap::<String, EventModel>::new();
        for event in events {
            unique_events.entry(event.name.clone()).or_insert(event);
        }
        let mut events = unique_events.into_values().collect::<Vec<_>>();
        events.sort_by(|left, right| locale_compare(&left.name, &right.name));
        objects.sort_by(|left, right| locale_compare(&left.export.name, &right.export.name));
        schemas.sort_by(|left, right| locale_compare(&left.export.name, &right.export.name));
        let invocations = if self.face == TypertFace::Host {
            let mut invocations = self.collect_invocations(registration, &reachable)?;
            invocations.sort_by(|left, right| locale_compare(left.id.as_str(), right.id.as_str()));
            invocations
        } else {
            Vec::new()
        };
        Ok(PackageModel {
            name: registration.name.clone(),
            root: paths::relative(&self.root, &registration.root),
            exports,
            services,
            events,
            objects,
            schemas,
            invocations,
        })
    }

    #[expect(
        clippy::case_sensitive_file_extension_comparisons,
        reason = "the source compares export targets case-sensitively"
    )]
    fn collect_exports(
        &mut self,
        registration: &PackageRegistration,
    ) -> FaceResult<Vec<ExportRecord<'s>>> {
        let targets = package_export_targets(&registration.manifest)
            .into_iter()
            .filter(|(subpath, _)| {
                registration
                    .export_subpaths
                    .as_ref()
                    .is_none_or(|subpaths| subpaths.contains(subpath))
            })
            .collect::<Vec<_>>();
        let mut records: Vec<ExportRecord<'s>> = Vec::new();
        for (subpath, target) in targets {
            if target.contains('*')
                || subpath == "./package.json"
                || subpath == "./typert"
                || subpath == "./client/typert"
                || subpath == "./remote"
                // Data exports (bundle patch lists, JSON manifests) carry no TypeScript API.
                || target.ends_with(".json")
                || target.ends_with(".yml")
                || target.ends_with(".yaml")
            {
                continue;
            }
            let source_path = source_path_for_export(&registration.root, &target)?;
            let Some(source_file) = self
                .source_files
                .get(&paths::real_path(&source_path))
                .copied()
            else {
                return Err(TypertGeneratorError::Analysis(format!(
                    "typert({}): {} export {subpath} resolves to missing source {}",
                    self.face.as_str(),
                    registration.name,
                    source_path.display()
                ))
                .into());
            };
            let module_symbol = js::call(
                self.scope,
                self.checker,
                "getSymbolAtLocation",
                &[source_file],
            )?;
            if module_symbol.is_undefined() {
                continue;
            }
            let exported_symbols = js::call(
                self.scope,
                self.checker,
                "getExportsOfModule",
                &[module_symbol],
            )?;
            for exported in js::items(self.scope, exported_symbols)? {
                let symbol = self.resolve_symbol(exported)?;
                let declaration = self
                    .syntax
                    .preferred_declaration(self.scope, symbol)?
                    .unwrap_or_else(|| js::undefined(self.scope));
                let exported_name = js::get_string(self.scope, exported, "name")?;
                let symbol_name = js::get_string(self.scope, symbol, "name")?;
                let aliases = if js::same(exported, symbol) || exported_name == symbol_name {
                    vec![exported_name.clone()]
                } else {
                    vec![exported_name.clone(), symbol_name]
                };
                records.push(ExportRecord {
                    model: ExportModel {
                        subpath: subpath.clone(),
                        name: exported_name,
                        symbol: self.symbol_id(symbol)?,
                        aliases,
                    },
                    symbol,
                    declaration,
                    source_file,
                });
            }
        }
        let mut unique = IndexMap::<String, ExportRecord<'s>>::new();
        for record in records {
            let key = format!("{}\0{}", record.model.subpath, record.model.name);
            unique.entry(key).or_insert(record);
        }
        let unique = unique.into_values().collect::<Vec<_>>();
        self.collect_cross_face_re_exports(registration, &unique)?;
        Ok(unique)
    }

    fn collect_cross_face_re_exports(
        &mut self,
        registration: &PackageRegistration,
        records: &[ExportRecord<'s>],
    ) -> FaceResult<()> {
        let public_symbols = JsSet::new(self.scope);
        for record in records {
            public_symbols.add(self.scope, record.symbol);
        }
        let mut entry_files = Vec::new();
        let mut seen = HashSet::new();
        for record in records {
            let name = js::get_string(self.scope, record.source_file, "fileName")?;
            if seen.insert(name) {
                entry_files.push(record.source_file);
            }
        }
        let kinds = &self.syntax.kinds;
        for source_file in self.reachable_files(registration, &entry_files)? {
            for statement in js::get_items(self.scope, source_file, "statements")? {
                if !self
                    .syntax
                    .is(self.scope, statement, kinds.export_declaration)?
                {
                    continue;
                }
                let Some(module_specifier) =
                    js::get_defined(self.scope, statement, "moduleSpecifier")?
                else {
                    continue;
                };
                if !self
                    .syntax
                    .is(self.scope, module_specifier, kinds.string_literal)?
                {
                    continue;
                }
                let specifier = js::get_string(self.scope, module_specifier, "text")?;
                let Some(module) = module_identity(&specifier) else {
                    continue;
                };
                let Some(to_face) = self
                    .all_registrations
                    .iter()
                    .find(|candidate| {
                        candidate.name == module.package && candidate.face != self.face
                    })
                    .map(|candidate| candidate.face)
                else {
                    continue;
                };
                let export_clause = js::get_defined(self.scope, statement, "exportClause")?;
                if let Some(clause) = export_clause
                    && self.syntax.is(self.scope, clause, kinds.namespace_export)?
                {
                    let name = js::get(self.scope, clause, "name")?;
                    let symbol =
                        js::call(self.scope, self.checker, "getSymbolAtLocation", &[name])?;
                    let namespace = self.resolve_symbol(symbol)?;
                    if public_symbols.has(self.scope, namespace) {
                        return Err(
                            self.fail(clause, "cross-face namespace re-exports are not supported")?
                        );
                    }
                    continue;
                }
                let mut exports = Vec::new();
                match export_clause {
                    None => {
                        for symbol in self.module_exports(module_specifier)? {
                            let resolved = self.resolve_symbol(symbol)?;
                            let requested = js::get_string(self.scope, symbol, "name")?;
                            exports.push((resolved, requested, statement));
                        }
                    }
                    Some(clause) => {
                        for element in js::get_items(self.scope, clause, "elements")? {
                            let name = js::get(self.scope, element, "name")?;
                            let symbol =
                                js::call(self.scope, self.checker, "getSymbolAtLocation", &[name])?;
                            let resolved = self.resolve_symbol(symbol)?;
                            let requested =
                                match js::get_defined(self.scope, element, "propertyName")? {
                                    Some(property) => js::get_string(self.scope, property, "text")?,
                                    None => js::get_string(self.scope, name, "text")?,
                                };
                            exports.push((resolved, requested, element));
                        }
                    }
                }
                for (symbol, requested_name, site) in exports {
                    if !public_symbols.has(self.scope, symbol) {
                        continue;
                    }
                    let Some(name) =
                        self.package_export_name(&module, symbol, to_face, &requested_name)?
                    else {
                        return Err(self.fail(
                            site,
                            &format!(
                                "cross-face re-export {requested_name} is not exported by {} at {}",
                                module.package, module.subpath
                            ),
                        )?);
                    };
                    self.record_cross_face_link(&registration.name, to_face, &module, &name);
                }
            }
        }
        Ok(())
    }

    fn module_exports(&mut self, module_specifier: Val<'s>) -> FaceResult<Vec<Val<'s>>> {
        let module_symbol = js::call(
            self.scope,
            self.checker,
            "getSymbolAtLocation",
            &[module_specifier],
        )?;
        let exports = js::call(
            self.scope,
            self.checker,
            "getExportsOfModule",
            &[module_symbol],
        )?;
        Ok(js::items(self.scope, exports)?)
    }

    fn reachable_files(
        &mut self,
        registration: &PackageRegistration,
        entry_files: &[Val<'s>],
    ) -> FaceResult<Vec<Val<'s>>> {
        let mut reachable = IndexMap::<PathBuf, Val<'s>>::new();
        let mut queue = entry_files.to_vec();
        let options = js::call(self.scope, self.program, "getCompilerOptions", &[])?;
        let system = js::get(self.scope, self.syntax.ts, "sys")?;
        let kinds = &self.syntax.kinds;
        while !queue.is_empty() {
            let source_file = queue.remove(0);
            let name = js::get_string(self.scope, source_file, "fileName")?;
            let file_name = paths::real_path(Path::new(&name));
            if reachable.contains_key(&file_name)
                || !paths::is_within(&file_name, &registration.root)
            {
                continue;
            }
            reachable.insert(file_name, source_file);
            for statement in js::get_items(self.scope, source_file, "statements")? {
                let kind = self.syntax.kind(self.scope, statement)?;
                if kind != kinds.import_declaration && kind != kinds.export_declaration {
                    continue;
                }
                let Some(module_specifier) =
                    js::get_defined(self.scope, statement, "moduleSpecifier")?
                else {
                    continue;
                };
                if !self
                    .syntax
                    .is(self.scope, module_specifier, kinds.string_literal)?
                {
                    continue;
                }
                let specifier = js::get(self.scope, module_specifier, "text")?;
                let file_value = js::get(self.scope, source_file, "fileName")?;
                let resolution = js::call(
                    self.scope,
                    self.syntax.ts,
                    "resolveModuleName",
                    &[specifier, file_value, options, system],
                )?;
                let Some(resolved) = js::get_defined(self.scope, resolution, "resolvedModule")?
                else {
                    continue;
                };
                let resolved_path = paths::real_path(Path::new(&js::get_string(
                    self.scope,
                    resolved,
                    "resolvedFileName",
                )?));
                if !paths::is_within(&resolved_path, &registration.root) {
                    continue;
                }
                if let Some(next) = self.source_files.get(&resolved_path).copied() {
                    queue.push(next);
                }
            }
        }
        let mut files = reachable.into_iter().collect::<Vec<_>>();
        let mut named = Vec::new();
        for (_, file) in &files {
            named.push((js::get_string(self.scope, *file, "fileName")?, *file));
        }
        named.sort_by(|left, right| locale_compare(&left.0, &right.0));
        files.clear();
        Ok(named.into_iter().map(|(_, file)| file).collect())
    }

    fn collect_services(
        &mut self,
        context: Val<'s>,
        records: &[ExportRecord<'s>],
    ) -> FaceResult<Vec<ServiceModel>> {
        let mut by_symbol = HashMap::<SymbolId, Vec<ExportRecord<'s>>>::new();
        for record in records {
            let id = self.symbol_id(record.symbol)?;
            by_symbol.entry(id).or_default().push(record.clone());
        }
        let kinds = &self.syntax.kinds;
        let mut result = Vec::new();
        for member in js::get_items(self.scope, context, "members")? {
            if !self
                .syntax
                .is(self.scope, member, kinds.property_signature)?
            {
                continue;
            }
            let Some(member_type) = js::get_defined(self.scope, member, "type")? else {
                continue;
            };
            // An OPTIONAL key is not a service: `X | undefined` and `key?: X` both mark
            // a value the launcher or boot code installs before the tree mounts (a root
            // accessor, an environment snapshot), which no plugin provides and no
            // consumer can reach with `inject`. Describing one as a service would answer
            // "add the plugin that provides it" for a key where no such plugin exists.
            if js::get_defined(self.scope, member, "questionToken")?.is_some() {
                continue;
            }
            if self.syntax.is(self.scope, member_type, kinds.union_type)? {
                let mut has_undefined = false;
                for node in js::get_items(self.scope, member_type, "types")? {
                    if self.syntax.kind(self.scope, node)? == kinds.undefined_keyword {
                        has_undefined = true;
                    }
                }
                if has_undefined {
                    continue;
                }
            }
            let Some(authored_symbol) = self.symbol_at_type(member_type)? else {
                continue;
            };
            let authored_symbol_id = self.symbol_id(authored_symbol)?;
            let authored_name = js::get_string(self.scope, authored_symbol, "name")?;
            let Some(matches) = by_symbol.get(&authored_symbol_id) else {
                continue;
            };
            let exported = matches
                .iter()
                .find(|record| record.model.name == authored_name)
                .or_else(|| matches.iter().find(|record| record.model.name != "default"))
                .or_else(|| matches.first())
                .cloned();
            let Some(exported) = exported else {
                continue;
            };
            let mut symbol = authored_symbol;
            let mut declaration = self.syntax.preferred_declaration(self.scope, symbol)?;
            let aliases = JsSet::new(self.scope);
            while let Some(current) = declaration
                && self
                    .syntax
                    .is(self.scope, current, kinds.type_alias_declaration)?
            {
                if aliases.has(self.scope, symbol) {
                    break;
                }
                aliases.add(self.scope, symbol);
                let alias_type = js::get(self.scope, current, "type")?;
                let Some(target) = self.symbol_at_type(alias_type)? else {
                    break;
                };
                symbol = target;
                declaration = self.syntax.preferred_declaration(self.scope, symbol)?;
            }
            let declaration = match declaration {
                Some(declaration)
                    if self
                        .syntax
                        .is(self.scope, declaration, kinds.class_declaration)?
                        || self.syntax.is(
                            self.scope,
                            declaration,
                            kinds.interface_declaration,
                        )? =>
                {
                    declaration
                }
                _ => {
                    let name = js::get(self.scope, member, "name")?;
                    let name = self.syntax.member_name(self.scope, name)?;
                    return Err(self.fail(
                        member,
                        &format!(
                            "service {name} does not resolve to an exported class or interface"
                        ),
                    )?);
                }
            };
            let member_file = self.syntax.file_name_of(self.scope, member)?;
            let declaration_file = self.syntax.file_name_of(self.scope, declaration)?;
            let member_owner = self
                .registration_for_file(&member_file)
                .map(|registration| registration.name.clone());
            let declaration_owner = self
                .registration_for_file(&declaration_file)
                .map(|registration| registration.name.clone());
            if member_owner != declaration_owner {
                continue;
            }
            let symbol_id = self.symbol_id(symbol)?;
            let model = self.ensure_declaration(symbol, declaration)?;
            let exposed = model
                .members
                .iter()
                .filter(|member| exposable_member(member))
                .map(MemberModel::id)
                .collect();
            let documentation = self.syntax.documentation_of(self.scope, declaration)?;
            let name = js::get(self.scope, member, "name")?;
            let key = self.syntax.member_name(self.scope, name)?;
            let location = self.location(member)?;
            result.push(ServiceModel {
                documentation,
                key,
                symbol: symbol_id,
                export: exported.model,
                members: exposed,
                location,
            });
        }
        Ok(result)
    }

    fn collect_explicit_services(
        &mut self,
        records: &[ExportRecord<'s>],
    ) -> FaceResult<Vec<ServiceModel>> {
        let mut result = Vec::new();
        let mut seen = HashSet::new();
        for record in records {
            if record.declaration.is_undefined() {
                continue;
            }
            let Some((tag, comment)) = self
                .syntax
                .typert_service_tag(self.scope, record.declaration)?
            else {
                continue;
            };
            let words = jstext::split_words(&comment);
            if words.len() != 2 || !is_remote_segment(words.get(1).map_or("", String::as_str)) {
                return Err(self.fail(
                    tag,
                    "@typert service requires exactly one nonempty Cordis service key without \"/\"",
                )?);
            }
            if !self.syntax.is(
                self.scope,
                record.declaration,
                self.syntax.kinds.class_declaration,
            )? {
                return Err(self.fail(
                    record.declaration,
                    "@typert service requires an exported class",
                )?);
            }
            let symbol = self.resolve_symbol(record.symbol)?;
            let symbol_id = self.symbol_id(symbol)?;
            if !seen.insert(symbol_id.clone()) {
                continue;
            }
            let model = self.ensure_declaration(symbol, record.declaration)?;
            let members = model
                .members
                .iter()
                .filter(|member| exposable_member(member))
                .map(MemberModel::id)
                .collect();
            let documentation = self
                .syntax
                .documentation_of(self.scope, record.declaration)?;
            let location = self.location(record.declaration)?;
            result.push(ServiceModel {
                documentation,
                key: words[1].clone(),
                symbol: symbol_id,
                export: record.model.clone(),
                members,
                location,
            });
        }
        Ok(result)
    }

    fn collect_events(&mut self, events: Val<'s>) -> FaceResult<Vec<EventModel>> {
        let kinds = &self.syntax.kinds;
        let mut result = Vec::new();
        for member in js::get_items(self.scope, events, "members")? {
            let documentation = self.syntax.documentation_of(self.scope, member)?;
            let mode = documentation
                .tags
                .iter()
                .find(|tag| tag.name == "mode")
                .and_then(|tag| tag.comment.as_deref())
                .map(|comment| jstext::trim(comment).to_owned());
            let kind = self.syntax.kind(self.scope, member)?;
            if kind == kinds.method_signature {
                let explicit_return = js::get_defined(self.scope, member, "type")?;
                let signature = self.signature(member, explicit_return)?;
                let name = js::get(self.scope, member, "name")?;
                let name = self.syntax.member_name(self.scope, name)?;
                let signature = self.add_node(member, TypeNodeKind::Function { signature })?;
                let text = self.syntax.member_text(self.scope, member)?;
                let location = self.location(member)?;
                result.push(EventModel {
                    documentation,
                    name,
                    signature,
                    text,
                    mode,
                    location,
                });
            } else if kind == kinds.property_signature
                && let Some(member_type) = js::get_defined(self.scope, member, "type")?
            {
                let name = js::get(self.scope, member, "name")?;
                let name = self.syntax.member_name(self.scope, name)?;
                let signature = self.convert_type(member_type)?;
                let text = self.syntax.member_text(self.scope, member)?;
                let location = self.location(member)?;
                result.push(EventModel {
                    documentation,
                    name,
                    signature,
                    text,
                    mode,
                    location,
                });
            }
        }
        Ok(result)
    }

    pub(crate) fn ensure_declaration(
        &mut self,
        symbol: Val<'s>,
        selected: Val<'s>,
    ) -> FaceResult<TypeDeclarationModel> {
        let resolved = self.resolve_symbol(symbol)?;
        let id = self.symbol_id(resolved)?;
        if let Some(existing) = self.declarations.get(&id) {
            return Ok(existing.clone());
        }
        let kinds = &self.syntax.kinds;
        let mut declaration_parts = Vec::new();
        for declaration in js::get_items(self.scope, resolved, "declarations")? {
            if self.syntax.is_type_declaration(self.scope, declaration)? {
                declaration_parts.push(declaration);
            }
        }
        let resolved_name = js::get_string(self.scope, resolved, "name")?;
        let selected_kind = self.syntax.kind(self.scope, selected)?;
        if declaration_parts.len() > 1 {
            let mut all_interfaces = true;
            for part in &declaration_parts {
                if !self
                    .syntax
                    .is(self.scope, *part, kinds.interface_declaration)?
                {
                    all_interfaces = false;
                }
            }
            if !all_interfaces {
                return Err(self.fail(
                    selected,
                    &format!(
                        "merged {} declaration {resolved_name} is not supported",
                        self.syntax.constants.kind_name(selected_kind)
                    ),
                )?);
            }
        }
        if js::get_defined(self.scope, selected, "name")?.is_none() {
            return Err(self.fail(
                selected,
                &format!(
                    "anonymous {} cannot be represented as a named type declaration",
                    self.syntax.constants.kind_name(selected_kind)
                ),
            )?);
        }
        let selected_file = self.syntax.file_name_of(self.scope, selected)?;
        let owner = self
            .registration_for_file(&selected_file)
            .map(|registration| registration.name.clone())
            .ok_or_else(|| {
                js::failure(format!(
                    "declaration {resolved_name} has no owning registration"
                ))
            })?;

        self.declaration_states.insert(id.clone());
        if declaration_parts.len() > 1 {
            let mut analyzed_parts = Vec::new();
            for part in &declaration_parts {
                let part = *part;
                let part_file = self.syntax.file_name_of(self.scope, part)?;
                let Some(part_owner) = self
                    .registration_for_file(&part_file)
                    .map(|registration| registration.name.clone())
                else {
                    return Err(self.fail(
                        part,
                        &format!("merged interface {resolved_name} contains a declaration outside this face"),
                    )?);
                };
                let part_parameters = js::get_defined(self.scope, part, "typeParameters")?;
                let type_parameters = self.type_parameters(part_parameters)?;
                let heritage = self.heritage(part)?;
                let part_members = js::get(self.scope, part, "members")?;
                let members = self.members(part_members, &id)?;
                let documentation = self.syntax.documentation_of(self.scope, part)?;
                let location = self.location(part)?;
                analyzed_parts.push((
                    type_parameters.clone(),
                    heritage.clone(),
                    members.clone(),
                    TypeDeclarationPartModel {
                        documentation,
                        package: part_owner,
                        location,
                        type_parameters,
                        extends: heritage.0,
                        members: members.iter().map(MemberModel::id).collect(),
                    },
                ));
            }
            let parameters = self.merge_type_parameters(
                &analyzed_parts
                    .iter()
                    .map(|part| part.0.clone())
                    .collect::<Vec<_>>(),
                selected,
                &resolved_name,
            )?;
            let documentation = self.syntax.documentation_of(self.scope, selected)?;
            let name = js::get(self.scope, selected, "name")?;
            let name = js::get_string(self.scope, name, "text")?;
            let exported = self
                .syntax
                .has_modifier(self.scope, selected, kinds.export_keyword)?;
            let location = self.location(selected)?;
            let text = self.syntax.declaration_text(self.scope, selected)?;
            let model = TypeDeclarationModel {
                documentation,
                id: id.clone(),
                package: owner,
                name,
                kind: DeclarationKind::Interface,
                is_abstract: false,
                exported,
                location,
                text,
                type_parameters: parameters,
                extends: analyzed_parts
                    .iter()
                    .flat_map(|part| part.1.0.clone())
                    .collect(),
                implements: Vec::new(),
                members: analyzed_parts
                    .iter()
                    .flat_map(|part| part.2.clone())
                    .collect(),
                parts: Some(analyzed_parts.into_iter().map(|part| part.3).collect()),
                ty: None,
                enum_members: None,
            };
            self.declarations.insert(id.clone(), model.clone());
            self.declaration_states.remove(&id);
            return Ok(model);
        }
        let is_enum = selected_kind == kinds.enum_declaration;
        let is_alias = selected_kind == kinds.type_alias_declaration;
        let parameters = if is_enum {
            Vec::new()
        } else {
            let selected_parameters = js::get_defined(self.scope, selected, "typeParameters")?;
            self.type_parameters(selected_parameters)?
        };
        let (extends, implements) = if is_alias || is_enum {
            (Vec::new(), Vec::new())
        } else {
            self.heritage(selected)?
        };
        let kind = if selected_kind == kinds.class_declaration {
            DeclarationKind::Class
        } else if selected_kind == kinds.interface_declaration {
            DeclarationKind::Interface
        } else if is_alias {
            DeclarationKind::Alias
        } else {
            DeclarationKind::Enum
        };
        let members = if is_alias || is_enum {
            Vec::new()
        } else {
            let selected_members = js::get(self.scope, selected, "members")?;
            self.members(selected_members, &id)?
        };
        let ty = if is_alias {
            let alias_type = js::get(self.scope, selected, "type")?;
            Some(self.convert_type(alias_type)?)
        } else {
            None
        };
        let enum_members = if is_enum {
            Some(self.enum_members(selected)?)
        } else {
            None
        };
        let documentation = self.syntax.documentation_of(self.scope, selected)?;
        let name = js::get(self.scope, selected, "name")?;
        let name = js::get_string(self.scope, name, "text")?;
        let is_abstract = self
            .syntax
            .has_modifier(self.scope, selected, kinds.abstract_keyword)?;
        let exported = self
            .syntax
            .has_modifier(self.scope, selected, kinds.export_keyword)?;
        let location = self.location(selected)?;
        let text = self.syntax.declaration_text(self.scope, selected)?;
        let model = TypeDeclarationModel {
            documentation,
            id: id.clone(),
            package: owner,
            name,
            kind,
            is_abstract,
            exported,
            location,
            text,
            type_parameters: parameters,
            extends,
            implements,
            members,
            parts: None,
            ty,
            enum_members,
        };
        self.declarations.insert(id.clone(), model.clone());
        self.declaration_states.remove(&id);
        Ok(model)
    }

    fn enum_members(&mut self, declaration: Val<'s>) -> FaceResult<Vec<EnumMemberModel>> {
        let mut result = Vec::new();
        for member in js::get_items(self.scope, declaration, "members")? {
            let documentation = self.syntax.documentation_of(self.scope, member)?;
            let name = js::get(self.scope, member, "name")?;
            let name = self.syntax.member_name(self.scope, name)?;
            let initializer = match js::get_defined(self.scope, member, "initializer")? {
                Some(initializer) => Some(self.syntax.node_text(self.scope, initializer)?),
                None => None,
            };
            let location = self.location(member)?;
            result.push(EnumMemberModel {
                documentation,
                name,
                initializer,
                location,
            });
        }
        Ok(result)
    }

    fn heritage(&mut self, declaration: Val<'s>) -> FaceResult<(Vec<TypeNodeId>, Vec<TypeNodeId>)> {
        let mut extends = Vec::new();
        let mut implements = Vec::new();
        for clause in js::get_items(self.scope, declaration, "heritageClauses")? {
            let token = js::integer(js::get_number(self.scope, clause, "token")?);
            for ty in js::get_items(self.scope, clause, "types")? {
                let converted = self.convert_heritage(ty)?;
                if token == self.syntax.kinds.extends_keyword {
                    extends.push(converted);
                } else {
                    implements.push(converted);
                }
            }
        }
        Ok((extends, implements))
    }

    fn convert_heritage(&mut self, node: Val<'s>) -> FaceResult<TypeNodeId> {
        let expression = js::get(self.scope, node, "expression")?;
        let symbol = js::call(
            self.scope,
            self.checker,
            "getSymbolAtLocation",
            &[expression],
        )?;
        let resolved = self.resolve_symbol(symbol)?;
        let name = self.syntax.node_text(self.scope, expression)?;
        let target = self.target_for_reference(resolved, node)?;
        let mut arguments = Vec::new();
        for argument in js::get_items(self.scope, node, "typeArguments")? {
            arguments.push(self.convert_type(argument)?);
        }
        self.add_node(
            node,
            TypeNodeKind::Reference {
                name,
                target,
                arguments,
            },
        )
    }

    pub(crate) fn members(
        &mut self,
        members: Val<'s>,
        owner_id: &SymbolId,
    ) -> FaceResult<Vec<MemberModel>> {
        let kinds = &self.syntax.kinds;
        let all = js::items(self.scope, members)?;
        let mut result = Vec::new();
        for member in &all {
            let member = *member;
            let kind = self.syntax.kind(self.scope, member)?;
            if kind == kinds.property_declaration {
                let name = js::get(self.scope, member, "name")?;
                if self.syntax.member_name(self.scope, name)? == "typertRemote"
                    && let Some(initializer) = js::get_defined(self.scope, member, "initializer")?
                    && self
                        .syntax
                        .is(self.scope, initializer, kinds.call_expression)?
                {
                    let expression = js::get(self.scope, initializer, "expression")?;
                    if self.is_type_meta_symbol(expression, "bindTypertRemote")? {
                        continue;
                    }
                }
            }
            if kind == kinds.method_declaration
                && js::get_defined(self.scope, member, "body")?.is_some()
            {
                let name = js::get(self.scope, member, "name")?;
                let name = self.syntax.member_name(self.scope, name)?;
                let mut overloaded = false;
                for candidate in &all {
                    if js::same(*candidate, member) {
                        continue;
                    }
                    let candidate_kind = self.syntax.kind(self.scope, *candidate)?;
                    if candidate_kind != kinds.method_declaration
                        && candidate_kind != kinds.method_signature
                    {
                        continue;
                    }
                    let candidate_name = js::get(self.scope, *candidate, "name")?;
                    if self.syntax.member_name(self.scope, candidate_name)? != name {
                        continue;
                    }
                    if candidate_kind != kinds.method_declaration
                        || js::get_defined(self.scope, *candidate, "body")?.is_none()
                    {
                        overloaded = true;
                    }
                }
                if overloaded {
                    continue;
                }
            }
            let visibility = self.syntax.visibility_of(self.scope, member)?;
            let is_static = self
                .syntax
                .has_modifier(self.scope, member, kinds.static_keyword)?;
            if visibility != crate::model::MemberVisibility::Public
                || is_static
                || kind == kinds.constructor
            {
                continue;
            }
            let base = self.member_base(member, owner_id, visibility, is_static)?;
            let explicit_type = js::get_defined(self.scope, member, "type")?;
            let member_kind =
                if kind == kinds.property_signature || kind == kinds.property_declaration {
                    let ty = self.required_type(member, explicit_type, TypePurpose::Property)?;
                    MemberKind::Property {
                        ty: self.convert_type(ty)?,
                    }
                } else if kind == kinds.method_signature || kind == kinds.method_declaration {
                    MemberKind::Method {
                        signature: self.signature(member, explicit_type)?,
                    }
                } else if kind == kinds.get_accessor {
                    MemberKind::Getter {
                        signature: self.signature(member, explicit_type)?,
                    }
                } else if kind == kinds.set_accessor {
                    MemberKind::Setter {
                        signature: self.signature(member, explicit_type)?,
                    }
                } else if kind == kinds.call_signature {
                    MemberKind::Call {
                        signature: self.signature(member, explicit_type)?,
                    }
                } else if kind == kinds.construct_signature {
                    MemberKind::Construct {
                        signature: self.signature(member, explicit_type)?,
                    }
                } else if kind == kinds.index_signature {
                    MemberKind::Index {
                        signature: self.signature(member, explicit_type)?,
                    }
                } else {
                    continue;
                };
            result.push(MemberModel::Defined(Box::new(DefinedMember {
                base,
                kind: member_kind,
            })));
        }
        Ok(result)
    }

    fn member_base(
        &mut self,
        member: Val<'s>,
        owner_id: &SymbolId,
        visibility: crate::model::MemberVisibility,
        is_static: bool,
    ) -> FaceResult<MemberBase> {
        let kinds = &self.syntax.kinds;
        let (name, json_name, computed) =
            if let Some(name) = js::get_defined(self.scope, member, "name")? {
                self.member_identity(name)?
            } else {
                let kind = self.syntax.kind(self.scope, member)?;
                let name = if kind == kinds.call_signature {
                    "(call)"
                } else if kind == kinds.construct_signature {
                    "(construct)"
                } else {
                    "(index)"
                };
                (name.to_owned(), None, None)
            };
        let documentation = self.syntax.documentation_of(self.scope, member)?;
        let start = self.syntax.node_start(self.scope, member, None)?;
        let optional = js::get_defined(self.scope, member, "questionToken")?.is_some();
        let read_only = self
            .syntax
            .has_modifier(self.scope, member, kinds.readonly_keyword)?;
        let is_async = self
            .syntax
            .has_modifier(self.scope, member, kinds.async_keyword)?;
        let is_abstract = self
            .syntax
            .has_modifier(self.scope, member, kinds.abstract_keyword)?;
        let location = self.location(member)?;
        let text = self.syntax.member_text(self.scope, member)?;
        Ok(MemberBase {
            documentation,
            id: MemberId::from(format!("{owner_id}#{name}@{}", jstext::number_text(start))),
            name,
            json_name,
            computed,
            optional,
            read_only,
            is_async,
            is_abstract,
            is_static,
            visibility,
            location,
            text,
        })
    }

    fn member_identity(
        &mut self,
        name: Val<'s>,
    ) -> FaceResult<(String, Option<String>, Option<crate::model::ComputedMember>)> {
        let kinds = &self.syntax.kinds;
        if !self
            .syntax
            .is(self.scope, name, kinds.computed_property_name)?
        {
            return Ok((self.syntax.member_name(self.scope, name)?, None, None));
        }
        let expression = js::get(self.scope, name, "expression")?;
        let expression_kind = self.syntax.kind(self.scope, expression)?;
        if expression_kind == kinds.string_literal
            || expression_kind == kinds.numeric_literal
            || expression_kind == kinds.no_substitution_template_literal
        {
            let text = js::get_string(self.scope, expression, "text")?;
            return Ok((self.syntax.member_name(self.scope, name)?, Some(text), None));
        }
        let ty = js::call(self.scope, self.checker, "getTypeAtLocation", &[expression])?;
        let flags = js::get_flags(self.scope, ty, "flags")?;
        let computed = if flags & self.syntax.constants.type_flag("UniqueESSymbol") != 0 {
            crate::model::ComputedMember::Symbol
        } else {
            crate::model::ComputedMember::Dynamic
        };
        Ok((
            self.syntax.member_name(self.scope, name)?,
            None,
            Some(computed),
        ))
    }

    pub(crate) fn signature(
        &mut self,
        node: Val<'s>,
        explicit_return: Option<Val<'s>>,
    ) -> FaceResult<SignatureModel> {
        let kinds = &self.syntax.kinds;
        let mut parameters = Vec::new();
        for parameter in js::get_items(self.scope, node, "parameters")? {
            let name = js::get(self.scope, parameter, "name")?;
            let name_kind = self.syntax.kind(self.scope, name)?;
            let binding = if name_kind == kinds.identifier {
                ParameterBinding::Identifier
            } else if name_kind == kinds.object_binding_pattern {
                ParameterBinding::Object
            } else {
                ParameterBinding::Array
            };
            let explicit = js::get_defined(self.scope, parameter, "type")?;
            let ty = self.required_type(parameter, explicit, TypePurpose::Parameter)?;
            let ty = self.convert_type(ty)?;
            let initializer = js::get_defined(self.scope, parameter, "initializer")?;
            let optional = js::get_defined(self.scope, parameter, "questionToken")?.is_some()
                || initializer.is_some();
            let rest = js::get_defined(self.scope, parameter, "dotDotDotToken")?.is_some();
            let receiver = name_kind == kinds.identifier
                && js::get_string(self.scope, name, "text")? == "this";
            let initializer = match initializer {
                Some(initializer) => Some(self.syntax.node_text(self.scope, initializer)?),
                None => None,
            };
            parameters.push(ParameterModel {
                name: self.syntax.member_name(self.scope, name)?,
                binding,
                ty,
                optional,
                rest,
                receiver,
                initializer,
            });
        }
        let node_parameters = js::get_defined(self.scope, node, "typeParameters")?;
        let type_parameters = self.type_parameters(node_parameters)?;
        let returns = if self.syntax.is(self.scope, node, kinds.set_accessor)? {
            self.add_node(
                node,
                TypeNodeKind::Keyword {
                    name: crate::model::KeywordTypeName::Void,
                },
            )?
        } else {
            let ty = self.required_type(node, explicit_return, TypePurpose::Return)?;
            self.convert_type(ty)?
        };
        Ok(SignatureModel {
            type_parameters,
            parameters,
            returns,
        })
    }

    pub(crate) fn type_parameters(
        &mut self,
        parameters: Option<Val<'s>>,
    ) -> FaceResult<Vec<TypeParameterModel>> {
        let Some(parameters) = parameters.filter(|value| !value.is_null_or_undefined()) else {
            return Ok(Vec::new());
        };
        let mut result = Vec::new();
        for parameter in js::items(self.scope, parameters)? {
            result.push(self.type_parameter(parameter)?);
        }
        Ok(result)
    }

    fn type_parameter(&mut self, parameter: Val<'s>) -> FaceResult<TypeParameterModel> {
        let kinds = &self.syntax.kinds;
        let name = js::get(self.scope, parameter, "name")?;
        let name = js::get_string(self.scope, name, "text")?;
        let key = self.location_key(parameter)?;
        let is_const = self
            .syntax
            .has_modifier(self.scope, parameter, kinds.const_keyword)?;
        let constraint = match js::get_defined(self.scope, parameter, "constraint")? {
            Some(constraint) => Some(self.convert_type(constraint)?),
            None => None,
        };
        let default = match js::get_defined(self.scope, parameter, "default")? {
            Some(default) => Some(self.convert_type(default)?),
            None => None,
        };
        let has_in = self
            .syntax
            .has_modifier(self.scope, parameter, kinds.in_keyword)?;
        let has_out = self
            .syntax
            .has_modifier(self.scope, parameter, kinds.out_keyword)?;
        let variance = match (has_in, has_out) {
            (true, true) => Some(Variance::InOut),
            (true, false) => Some(Variance::In),
            (false, true) => Some(Variance::Out),
            (false, false) => None,
        };
        Ok(TypeParameterModel {
            id: TypeParameterId::from(format!("{key}#{name}")),
            name,
            is_const,
            constraint,
            default,
            variance,
        })
    }

    fn merge_type_parameters(
        &mut self,
        parts: &[Vec<TypeParameterModel>],
        site: Val<'s>,
        declaration_name: &str,
    ) -> FaceResult<Vec<TypeParameterModel>> {
        let first = parts.first().cloned().unwrap_or_default();
        let mut result = Vec::new();
        for (index, parameter) in first.iter().enumerate() {
            let peers = parts
                .iter()
                .filter_map(|part| part.get(index))
                .collect::<Vec<_>>();
            let constraint = peers.iter().find_map(|peer| peer.constraint.clone());
            let fallback = peers.iter().find_map(|peer| peer.default.clone());
            let mut variances = Vec::new();
            for peer in &peers {
                if let Some(variance) = peer.variance
                    && !variances.contains(&variance)
                {
                    variances.push(variance);
                }
            }
            if variances.len() > 1 {
                return Err(self.fail(
                    site,
                    &format!(
                        "merged interface {declaration_name} has incompatible variance modifiers"
                    ),
                )?);
            }
            result.push(TypeParameterModel {
                id: parameter.id.clone(),
                name: parameter.name.clone(),
                is_const: peers.iter().any(|peer| peer.is_const),
                constraint,
                default: fallback,
                variance: variances.first().copied(),
            });
        }
        Ok(result)
    }

    pub(crate) fn required_type(
        &mut self,
        owner: Val<'s>,
        ty: Option<Val<'s>>,
        purpose: TypePurpose,
    ) -> FaceResult<Val<'s>> {
        if let Some(ty) = ty.filter(|value| !value.is_null_or_undefined()) {
            return Ok(ty);
        }
        if self.mode == AnalysisMode::Check {
            return Err(self.fail(
                owner,
                &format!(
                    "public {} is missing an explicit type annotation",
                    purpose.as_str()
                ),
            )?);
        }
        let inferred = self.infer_type(owner, purpose)?;
        let source_file = self.syntax.source_file_of(self.scope, owner)?;
        let rendered = self.syntax.print_node(self.scope, inferred, source_file)?;
        let position = if purpose == TypePurpose::Return {
            let parameters = js::get(self.scope, owner, "parameters")?;
            js::get_number(self.scope, parameters, "end")? + 1.0
        } else {
            let name = js::get(self.scope, owner, "name")?;
            js::get_number(self.scope, name, "end")?
        };
        let file = self.syntax.file_name_of(self.scope, owner)?;
        Err(Interrupt::Queued(SourceEdit {
            file: paths::real_path(Path::new(&file)),
            position: js::offset(position),
            text: format!(": {rendered}"),
        }))
    }

    fn infer_type(&mut self, owner: Val<'s>, purpose: TypePurpose) -> FaceResult<Val<'s>> {
        let ty = if purpose == TypePurpose::Return {
            let signature = js::call(
                self.scope,
                self.checker,
                "getSignatureFromDeclaration",
                &[owner],
            )?;
            js::call(
                self.scope,
                self.checker,
                "getReturnTypeOfSignature",
                &[signature],
            )?
        } else {
            js::call(self.scope, self.checker, "getTypeAtLocation", &[owner])?
        };
        let flags = self.syntax.constants.value("NodeBuilderFlags.NoTruncation")
            | self
                .syntax
                .constants
                .value("NodeBuilderFlags.UseAliasDefinedOutsideCurrentScope");
        let flags = js::number(self.scope, f64::from(flags));
        Ok(js::call(
            self.scope,
            self.checker,
            "typeToTypeNode",
            &[ty, owner, flags],
        )?)
    }

    pub(crate) fn convert_type(&mut self, node: Val<'s>) -> FaceResult<TypeNodeId> {
        let id = self.allocate_node_id(node)?;
        let kinds = self.syntax.kinds.clone();
        let kind = self.syntax.kind(self.scope, node)?;
        let model = if let Some(name) = self.syntax.keyword_name(kind) {
            TypeNodeKind::Keyword { name }
        } else if kind == kinds.parenthesized_type {
            let inner = js::get(self.scope, node, "type")?;
            TypeNodeKind::Parenthesized {
                ty: self.convert_type(inner)?,
            }
        } else if kind == kinds.literal_type {
            self.syntax.literal_model(self.scope, node)?
        } else if kind == kinds.type_reference {
            let type_name = js::get(self.scope, node, "typeName")?;
            let symbol = js::call(
                self.scope,
                self.checker,
                "getSymbolAtLocation",
                &[type_name],
            )?;
            let resolved = self.resolve_symbol(symbol)?;
            let name = self.syntax.node_text(self.scope, type_name)?;
            let target = self.target_for_reference(resolved, node)?;
            let mut arguments = Vec::new();
            for argument in js::get_items(self.scope, node, "typeArguments")? {
                arguments.push(self.convert_type(argument)?);
            }
            TypeNodeKind::Reference {
                name,
                target,
                arguments,
            }
        } else if kind == kinds.union_type || kind == kinds.intersection_type {
            let mut types = Vec::new();
            for member in js::get_items(self.scope, node, "types")? {
                types.push(self.convert_type(member)?);
            }
            if kind == kinds.union_type {
                TypeNodeKind::Union { types }
            } else {
                TypeNodeKind::Intersection { types }
            }
        } else if kind == kinds.array_type {
            let element = js::get(self.scope, node, "elementType")?;
            TypeNodeKind::Array {
                element: self.convert_type(element)?,
            }
        } else if kind == kinds.tuple_type {
            let mut elements = Vec::new();
            for element in js::get_items(self.scope, node, "elements")? {
                let named = if self
                    .syntax
                    .is(self.scope, element, kinds.named_tuple_member)?
                {
                    Some(element)
                } else {
                    None
                };
                let raw = match named {
                    Some(named) => js::get(self.scope, named, "type")?,
                    None => element,
                };
                let raw_kind = self.syntax.kind(self.scope, raw)?;
                let optional = match named {
                    Some(named) => js::get_defined(self.scope, named, "questionToken")?.is_some(),
                    None => false,
                } || raw_kind == kinds.optional_type;
                let rest = match named {
                    Some(named) => js::get_defined(self.scope, named, "dotDotDotToken")?.is_some(),
                    None => false,
                } || raw_kind == kinds.rest_type;
                let ty = if raw_kind == kinds.optional_type || raw_kind == kinds.rest_type {
                    js::get(self.scope, raw, "type")?
                } else {
                    raw
                };
                let name = match named {
                    Some(named) => {
                        let name = js::get(self.scope, named, "name")?;
                        Some(js::get_string(self.scope, name, "text")?)
                    }
                    None => None,
                };
                elements.push(crate::model::TupleElementModel {
                    name,
                    ty: self.convert_type(ty)?,
                    optional,
                    rest,
                });
            }
            TypeNodeKind::Tuple { elements }
        } else if kind == kinds.type_literal {
            let members = js::get(self.scope, node, "members")?;
            let owner = SymbolId::from(id.as_str());
            TypeNodeKind::Object {
                members: self.members(members, &owner)?,
            }
        } else if kind == kinds.function_type {
            let explicit = js::get_defined(self.scope, node, "type")?;
            TypeNodeKind::Function {
                signature: self.signature(node, explicit)?,
            }
        } else if kind == kinds.constructor_type {
            let explicit = js::get_defined(self.scope, node, "type")?;
            TypeNodeKind::Constructor {
                is_abstract: self
                    .syntax
                    .has_modifier(self.scope, node, kinds.abstract_keyword)?,
                signature: self.signature(node, explicit)?,
            }
        } else if kind == kinds.indexed_access_type {
            let object = js::get(self.scope, node, "objectType")?;
            let index = js::get(self.scope, node, "indexType")?;
            TypeNodeKind::IndexedAccess {
                object: self.convert_type(object)?,
                index: self.convert_type(index)?,
            }
        } else if kind == kinds.type_operator {
            let operator = js::get(self.scope, node, "operator")?;
            let spelled = js::call(self.scope, self.syntax.ts, "tokenToString", &[operator])?;
            let operator = match js::text(self.scope, spelled).as_str() {
                "keyof" => TypeOperatorName::Keyof,
                "readonly" => TypeOperatorName::Readonly,
                "unique" => TypeOperatorName::Unique,
                other => TypeOperatorName::Other(other.to_owned()),
            };
            let operand = js::get(self.scope, node, "type")?;
            TypeNodeKind::Operator {
                operator,
                ty: self.convert_type(operand)?,
            }
        } else if kind == kinds.conditional_type {
            let check = js::get(self.scope, node, "checkType")?;
            let extends = js::get(self.scope, node, "extendsType")?;
            let when_true = js::get(self.scope, node, "trueType")?;
            let when_false = js::get(self.scope, node, "falseType")?;
            TypeNodeKind::Conditional {
                check: self.convert_type(check)?,
                extends: self.convert_type(extends)?,
                when_true: self.convert_type(when_true)?,
                when_false: self.convert_type(when_false)?,
            }
        } else if kind == kinds.infer_type {
            let parameter = js::get(self.scope, node, "typeParameter")?;
            TypeNodeKind::Infer {
                parameter: self.type_parameter(parameter)?,
            }
        } else if kind == kinds.mapped_type {
            let parameter = js::get(self.scope, node, "typeParameter")?;
            let parameter = self.type_parameter(parameter)?;
            let name_type = match js::get_defined(self.scope, node, "nameType")? {
                Some(name_type) => Some(self.convert_type(name_type)?),
                None => None,
            };
            let value = match js::get_defined(self.scope, node, "type")? {
                Some(value) => Some(self.convert_type(value)?),
                None => None,
            };
            let readonly_token = js::get_defined(self.scope, node, "readonlyToken")?;
            let question_token = js::get_defined(self.scope, node, "questionToken")?;
            TypeNodeKind::Mapped {
                parameter,
                name_type,
                value,
                read_only: self.syntax.modifier_mode(self.scope, readonly_token)?,
                optional: self.syntax.modifier_mode(self.scope, question_token)?,
            }
        } else if kind == kinds.template_literal_type {
            let head = js::get(self.scope, node, "head")?;
            let head = js::get_string(self.scope, head, "text")?;
            let mut spans = Vec::new();
            for span in js::get_items(self.scope, node, "templateSpans")? {
                let ty = js::get(self.scope, span, "type")?;
                let literal = js::get(self.scope, span, "literal")?;
                spans.push(crate::model::TemplateSpanModel {
                    ty: self.convert_type(ty)?,
                    text: js::get_string(self.scope, literal, "text")?,
                });
            }
            TypeNodeKind::TemplateLiteral { head, spans }
        } else if kind == kinds.type_query {
            let expression = js::get(self.scope, node, "exprName")?;
            let expression = self.syntax.node_text(self.scope, expression)?;
            let mut arguments = Vec::new();
            for argument in js::get_items(self.scope, node, "typeArguments")? {
                arguments.push(self.convert_type(argument)?);
            }
            TypeNodeKind::TypeQuery {
                expression,
                arguments,
            }
        } else if kind == kinds.import_type {
            let argument = js::get(self.scope, node, "argument")?;
            let literal = js::get(self.scope, argument, "literal")?;
            let module = js::get_string(self.scope, literal, "text")?;
            let qualifier = js::get_defined(self.scope, node, "qualifier")?;
            let symbol = match qualifier {
                Some(qualifier) => {
                    let symbol = js::call(
                        self.scope,
                        self.checker,
                        "getSymbolAtLocation",
                        &[qualifier],
                    )?;
                    (!symbol.is_undefined()).then_some(symbol)
                }
                None => None,
            };
            let qualifier_text = match qualifier {
                Some(qualifier) => Some(self.syntax.node_text(self.scope, qualifier)?),
                None => None,
            };
            let mut arguments = Vec::new();
            for argument in js::get_items(self.scope, node, "typeArguments")? {
                arguments.push(self.convert_type(argument)?);
            }
            let is_typeof = js::get_bool(self.scope, node, "isTypeOf")?;
            let attributes = if js::get_defined(self.scope, node, "attributes")?.is_some() {
                Some(self.import_type_attributes_text(node)?)
            } else {
                None
            };
            let target = match symbol {
                Some(symbol) => {
                    let resolved = self.resolve_symbol(symbol)?;
                    Some(self.target_for_reference(resolved, node)?)
                }
                None => None,
            };
            TypeNodeKind::ImportType {
                module,
                qualifier: qualifier_text,
                arguments,
                is_typeof,
                attributes,
                target,
            }
        } else if kind == kinds.type_predicate {
            let asserts = js::get_defined(self.scope, node, "assertsModifier")?.is_some();
            let parameter = js::get(self.scope, node, "parameterName")?;
            let parameter = self.syntax.node_text(self.scope, parameter)?;
            let ty = match js::get_defined(self.scope, node, "type")? {
                Some(ty) => Some(self.convert_type(ty)?),
                None => None,
            };
            TypeNodeKind::Predicate {
                asserts,
                parameter,
                ty,
            }
        } else if kind == kinds.this_type {
            TypeNodeKind::This
        } else {
            return Err(self.fail(
                node,
                &format!(
                    "unsupported TypeScript type node {}",
                    self.syntax.constants.kind_name(kind)
                ),
            )?);
        };
        self.nodes.insert(
            id.clone(),
            TypeNodeModel::Defined(DefinedTypeNode {
                id: id.clone(),
                kind: model,
            }),
        );
        Ok(id)
    }

    fn import_type_attributes_text(&mut self, node: Val<'s>) -> FaceResult<String> {
        let source_file = self.syntax.source_file_of(self.scope, node)?;
        let children = js::call(self.scope, node, "getChildren", &[source_file])?;
        let mut comma = None;
        let mut close = None;
        for child in js::items(self.scope, children)? {
            let kind = self.syntax.kind(self.scope, child)?;
            if kind == self.syntax.kinds.comma_token && comma.is_none() {
                comma = Some(child);
            } else if kind == self.syntax.kinds.close_paren_token && close.is_none() {
                close = Some(child);
            }
        }
        let (Some(comma), Some(close)) = (comma, close) else {
            return Err(js::failure("import type attributes have no delimiters").into());
        };
        let text = js::get(self.scope, source_file, "text")?;
        let start = js::get(self.scope, comma, "end")?;
        let end = js::get(self.scope, close, "pos")?;
        let sliced = js::call(self.scope, text, "slice", &[start, end])?;
        Ok(jstext::trim(&js::text(self.scope, sliced)).to_owned())
    }

    pub(crate) fn add_node(&mut self, site: Val<'s>, kind: TypeNodeKind) -> FaceResult<TypeNodeId> {
        let id = self.allocate_node_id(site)?;
        self.nodes.insert(
            id.clone(),
            TypeNodeModel::Defined(DefinedTypeNode {
                id: id.clone(),
                kind,
            }),
        );
        Ok(id)
    }

    pub(crate) fn reference_node(
        &mut self,
        symbol: Val<'s>,
        site: Val<'s>,
    ) -> FaceResult<TypeNodeId> {
        let name = js::get_string(self.scope, symbol, "name")?;
        let symbol_id = self.symbol_id(symbol)?;
        self.add_node(
            site,
            TypeNodeKind::Reference {
                name,
                target: TypeTargetModel::Declaration { symbol: symbol_id },
                arguments: Vec::new(),
            },
        )
    }

    fn target_for_reference(
        &mut self,
        symbol: Val<'s>,
        site: Val<'s>,
    ) -> FaceResult<TypeTargetModel> {
        let symbol_name = js::get_string(self.scope, symbol, "name")?;
        let Some(declaration) = self.syntax.preferred_declaration(self.scope, symbol)? else {
            return Err(self.fail(
                site,
                &format!("type symbol {symbol_name} has no declaration"),
            )?);
        };
        if self
            .syntax
            .is(self.scope, declaration, self.syntax.kinds.type_parameter)?
        {
            let key = self.location_key(declaration)?;
            let name = js::get(self.scope, declaration, "name")?;
            let name = js::get_string(self.scope, name, "text")?;
            return Ok(TypeTargetModel::TypeParameter {
                parameter: TypeParameterId::from(format!("{key}#{name}")),
            });
        }
        let declaration_file = self.syntax.file_name_of(self.scope, declaration)?;
        if is_standard_library_file(&declaration_file) {
            return Ok(TypeTargetModel::Standard { name: symbol_name });
        }

        let module_specifier = self.module_specifier_of(site)?;
        let module = module_specifier.as_deref().and_then(module_identity);
        let site_file = self.syntax.file_name_of(self.scope, site)?;
        let from = self
            .registration_for_file(&site_file)
            .map(|registration| registration.name.clone())
            .ok_or_else(|| {
                js::failure(format!(
                    "reference site {site_file} has no owning registration"
                ))
            })?;
        let owner = self
            .registration_for_file(&declaration_file)
            .map(|registration| (registration.name.clone(), registration.face));
        if let Some((owner_name, owner_face)) = owner {
            if owner_name != from {
                let Some(module) = &module else {
                    return Err(self.fail(
                        site,
                        &format!("reference to {symbol_name} crosses a package without an explicit package import"),
                    )?);
                };
                let export_name =
                    self.authored_export_name(site, module_specifier.as_deref().unwrap_or(""))?;
                if self
                    .package_export_name(module, symbol, owner_face, &export_name)?
                    .is_none()
                {
                    return Err(self.fail(
                        site,
                        &format!(
                            "package reference {export_name} is not exported by {} at {}",
                            module.package, module.subpath
                        ),
                    )?);
                }
            }
            let symbol_id = self.symbol_id(symbol)?;
            if !self.declaration_states.contains(&symbol_id) {
                self.ensure_declaration(symbol, declaration)?;
            }
            return Ok(TypeTargetModel::Declaration { symbol: symbol_id });
        }

        let mut package_faces = Vec::new();
        if let Some(module) = &module {
            for candidate in self.all_registrations {
                if candidate.name == module.package && !package_faces.contains(&candidate.face) {
                    package_faces.push(candidate.face);
                }
            }
        }
        let other_face = package_faces
            .iter()
            .copied()
            .find(|face| *face != self.face);
        if let (Some(other_face), Some(module)) = (other_face, &module) {
            let requested_name =
                self.authored_export_name(site, module_specifier.as_deref().unwrap_or(""))?;
            let Some(export_name) =
                self.package_export_name(module, symbol, other_face, &requested_name)?
            else {
                return Err(self.fail(
                    site,
                    &format!(
                        "cross-face reference {requested_name} is not exported by {} at {}",
                        module.package, module.subpath
                    ),
                )?);
            };
            self.record_cross_face_link(&from, other_face, module, &export_name);
            return Ok(TypeTargetModel::CrossFace {
                face: other_face,
                package: module.package.clone(),
                subpath: module.subpath.clone(),
                name: export_name,
            });
        }

        if let Some(module) = module {
            return Ok(TypeTargetModel::External {
                module: module.package,
                subpath: module.subpath,
                name: symbol_name,
            });
        }

        if let Some(external) =
            crate::analyzer::external_module_identity_for_file(&declaration_file)
        {
            return Ok(TypeTargetModel::External {
                module: external.package,
                subpath: external.subpath,
                name: symbol_name,
            });
        }

        Err(self.fail(
            site,
            &format!(
                "reference to {symbol_name} crosses a package or face without an explicit import"
            ),
        )?)
    }

    /// The import module specifier that binds a reference site's leading identifier.
    fn module_specifier_of(&mut self, node: Val<'s>) -> FaceResult<Option<String>> {
        let kinds = &self.syntax.kinds;
        if self.syntax.is(self.scope, node, kinds.import_type)? {
            let argument = js::get(self.scope, node, "argument")?;
            let literal = js::get(self.scope, argument, "literal")?;
            return Ok(Some(js::get_string(self.scope, literal, "text")?));
        }
        let symbol = if self.syntax.is(self.scope, node, kinds.type_reference)? {
            js::get(self.scope, node, "typeName")?
        } else {
            js::get(self.scope, node, "expression")?
        };
        let source_file = self.syntax.source_file_of(self.scope, node)?;
        let first = if self.syntax.is(self.scope, symbol, kinds.identifier)? {
            Some(js::get_string(self.scope, symbol, "text")?)
        } else {
            let token = js::call(self.scope, symbol, "getFirstToken", &[source_file])?;
            if token.is_null_or_undefined() {
                None
            } else {
                Some(self.syntax.node_text_in(self.scope, token, source_file)?)
            }
        };
        for statement in js::get_items(self.scope, source_file, "statements")? {
            if !self
                .syntax
                .is(self.scope, statement, kinds.import_declaration)?
            {
                continue;
            }
            let Some(clause) = js::get_defined(self.scope, statement, "importClause")? else {
                continue;
            };
            let module_specifier = js::get(self.scope, statement, "moduleSpecifier")?;
            if !self
                .syntax
                .is(self.scope, module_specifier, kinds.string_literal)?
            {
                continue;
            }
            let specifier = js::get_string(self.scope, module_specifier, "text")?;
            if let Some(name) = js::get_defined(self.scope, clause, "name")?
                && Some(js::get_string(self.scope, name, "text")?) == first
            {
                return Ok(Some(specifier));
            }
            let Some(bindings) = js::get_defined(self.scope, clause, "namedBindings")? else {
                continue;
            };
            if self
                .syntax
                .is(self.scope, bindings, kinds.namespace_import)?
            {
                let name = js::get(self.scope, bindings, "name")?;
                if Some(js::get_string(self.scope, name, "text")?) == first {
                    return Ok(Some(specifier));
                }
            }
            if self.syntax.is(self.scope, bindings, kinds.named_imports)? {
                for element in js::get_items(self.scope, bindings, "elements")? {
                    let name = js::get(self.scope, element, "name")?;
                    if Some(js::get_string(self.scope, name, "text")?) == first {
                        return Ok(Some(specifier));
                    }
                }
            }
        }
        Ok(None)
    }

    fn authored_export_name(
        &mut self,
        node: Val<'s>,
        module_specifier: &str,
    ) -> FaceResult<String> {
        let kinds = &self.syntax.kinds;
        if self.syntax.is(self.scope, node, kinds.import_type)? {
            let qualifier = js::get(self.scope, node, "qualifier")?;
            let text = self.syntax.node_text(self.scope, qualifier)?;
            return Ok(text.split('.').next().unwrap_or_default().to_owned());
        }
        let referenced = if self.syntax.is(self.scope, node, kinds.type_reference)? {
            let type_name = js::get(self.scope, node, "typeName")?;
            self.syntax.node_text(self.scope, type_name)?
        } else {
            let expression = js::get(self.scope, node, "expression")?;
            self.syntax.node_text(self.scope, expression)?
        };
        let referenced = referenced.split('.').map(str::to_owned).collect::<Vec<_>>();
        let local_name = referenced.first().cloned().unwrap_or_default();
        let source_file = self.syntax.source_file_of(self.scope, node)?;
        for statement in js::get_items(self.scope, source_file, "statements")? {
            if !self
                .syntax
                .is(self.scope, statement, kinds.import_declaration)?
            {
                continue;
            }
            let Some(clause) = js::get_defined(self.scope, statement, "importClause")? else {
                continue;
            };
            let specifier = js::get(self.scope, statement, "moduleSpecifier")?;
            if !self
                .syntax
                .is(self.scope, specifier, kinds.string_literal)?
                || js::get_string(self.scope, specifier, "text")? != module_specifier
            {
                continue;
            }
            if let Some(name) = js::get_defined(self.scope, clause, "name")?
                && js::get_string(self.scope, name, "text")? == local_name
            {
                return Ok("default".to_owned());
            }
            let Some(bindings) = js::get_defined(self.scope, clause, "namedBindings")? else {
                continue;
            };
            if self.syntax.is(self.scope, bindings, kinds.named_imports)? {
                for element in js::get_items(self.scope, bindings, "elements")? {
                    let name = js::get(self.scope, element, "name")?;
                    if js::get_string(self.scope, name, "text")? == local_name {
                        return Ok(
                            match js::get_defined(self.scope, element, "propertyName")? {
                                Some(property) => js::get_string(self.scope, property, "text")?,
                                None => local_name,
                            },
                        );
                    }
                }
            }
            if self
                .syntax
                .is(self.scope, bindings, kinds.namespace_import)?
            {
                let name = js::get(self.scope, bindings, "name")?;
                if js::get_string(self.scope, name, "text")? == local_name {
                    return Ok(referenced.get(1).cloned().unwrap_or_default());
                }
            }
        }
        Err(TypertGeneratorError::Analysis(format!(
            "typert: cannot recover export name for {local_name} from {module_specifier}"
        ))
        .into())
    }

    pub(crate) fn record_cross_face_link(
        &mut self,
        from_package: &str,
        to_face: TypertFace,
        module: &crate::analyzer::ModuleIdentity,
        name: &str,
    ) {
        let link = CrossFaceLink {
            from_face: self.face,
            from_package: from_package.to_owned(),
            to_face,
            to_package: module.package.clone(),
            subpath: module.subpath.clone(),
            name: name.to_owned(),
        };
        let key = [
            link.from_face.as_str(),
            &link.from_package,
            link.to_face.as_str(),
            &link.to_package,
            &link.subpath,
            &link.name,
        ]
        .join("\0");
        self.cross_face_links.insert(key, link);
    }

    pub(crate) fn package_export_name(
        &mut self,
        module: &crate::analyzer::ModuleIdentity,
        symbol: Val<'s>,
        face: TypertFace,
        requested_name: &str,
    ) -> FaceResult<Option<String>> {
        let registration = self
            .all_registrations
            .iter()
            .find(|candidate| candidate.face == face && candidate.name == module.package)
            .ok_or_else(|| {
                js::failure(format!(
                    "package {} has no {} registration",
                    module.package,
                    face.as_str()
                ))
            })?;
        let Some((_, target)) = package_export_targets(&registration.manifest)
            .into_iter()
            .find(|(subpath, _)| *subpath == module.subpath)
        else {
            return Ok(None);
        };
        let source_path = paths::real_path(&source_path_for_export(&registration.root, &target)?);
        let source_file = self
            .source_files
            .get(&source_path)
            .copied()
            .ok_or_else(|| {
                js::failure(format!(
                    "export source {} is not part of the program",
                    source_path.display()
                ))
            })?;
        let module_symbol = js::call(
            self.scope,
            self.checker,
            "getSymbolAtLocation",
            &[source_file],
        )?;
        let exports = js::call(
            self.scope,
            self.checker,
            "getExportsOfModule",
            &[module_symbol],
        )?;
        for candidate in js::items(self.scope, exports)? {
            let name = js::get_string(self.scope, candidate, "name")?;
            if name != requested_name {
                continue;
            }
            let resolved = self.resolve_symbol(candidate)?;
            if js::same(resolved, symbol) {
                return Ok(Some(name));
            }
        }
        Ok(None)
    }

    pub(crate) fn symbol_at_type(&mut self, node: Val<'s>) -> FaceResult<Option<Val<'s>>> {
        if self
            .syntax
            .is(self.scope, node, self.syntax.kinds.type_reference)?
        {
            let type_name = js::get(self.scope, node, "typeName")?;
            let symbol = js::call(
                self.scope,
                self.checker,
                "getSymbolAtLocation",
                &[type_name],
            )?;
            return Ok(Some(self.resolve_symbol(symbol)?));
        }
        let ty = js::call(self.scope, self.checker, "getTypeAtLocation", &[node])?;
        let symbol = match js::get_defined(self.scope, ty, "aliasSymbol")? {
            Some(alias) => alias,
            None => js::call(self.scope, ty, "getSymbol", &[])?,
        };
        if symbol.is_undefined() {
            return Ok(None);
        }
        Ok(Some(self.resolve_symbol(symbol)?))
    }

    pub(crate) fn resolve_symbol(&mut self, symbol: Val<'s>) -> FaceResult<Val<'s>> {
        if symbol.is_null_or_undefined() {
            return Err(js::failure("compiler symbol is absent").into());
        }
        let flags = js::get_flags(self.scope, symbol, "flags")?;
        if flags & self.syntax.constants.symbol_flag("Alias") == 0 {
            return Ok(symbol);
        }
        Ok(js::call(
            self.scope,
            self.checker,
            "getAliasedSymbol",
            &[symbol],
        )?)
    }

    pub(crate) fn symbol_id(&mut self, symbol: Val<'s>) -> FaceResult<SymbolId> {
        let name = js::get_string(self.scope, symbol, "name")?;
        let Some(declaration) = self.syntax.preferred_declaration(self.scope, symbol)? else {
            return Ok(SymbolId::from(format!("symbol:{name}")));
        };
        let location = self.location(declaration)?;
        let file = self.syntax.file_name_of(self.scope, declaration)?;
        Ok(SymbolId::from(format!(
            "{}:{}#{name}",
            self.package_name_for_file(&file),
            location.file
        )))
    }

    pub(crate) fn registration_for_file(&self, file: &str) -> Option<&'a PackageRegistration> {
        let path = paths::real_path(Path::new(file));
        self.all_registrations.iter().find(|registration| {
            registration.face == self.face && paths::is_within(&path, &registration.root)
        })
    }

    fn package_name_for_file(&self, file: &str) -> String {
        let path = paths::real_path(Path::new(file));
        self.all_registrations
            .iter()
            .find(|registration| paths::is_within(&path, &registration.root))
            .map_or_else(
                || "<external>".to_owned(),
                |registration| registration.name.clone(),
            )
    }

    pub(crate) fn is_type_meta_symbol(&mut self, node: Val<'s>, name: &str) -> FaceResult<bool> {
        let symbol = js::call(self.scope, self.checker, "getSymbolAtLocation", &[node])?;
        if symbol.is_undefined() {
            return Ok(false);
        }
        let resolved = self.resolve_symbol(symbol)?;
        if js::get_string(self.scope, resolved, "name")? != name {
            return Ok(false);
        }
        let Some(declaration) = self.syntax.preferred_declaration(self.scope, resolved)? else {
            return Ok(false);
        };
        let file = self.syntax.file_name_of(self.scope, declaration)?;
        if self
            .registration_for_file(&file)
            .is_some_and(|registration| PROTOCOL_PACKAGES.contains(&registration.name.as_str()))
        {
            return Ok(true);
        }
        let mut current = Some(declaration);
        while let Some(node) = current {
            if self
                .syntax
                .module_named(self.scope, node, PROTOCOL_MODULES)?
                .is_some()
            {
                return Ok(true);
            }
            if self
                .syntax
                .is(self.scope, node, self.syntax.kinds.module_declaration)?
            {
                let name = js::get(self.scope, node, "name")?;
                if self
                    .syntax
                    .is(self.scope, name, self.syntax.kinds.string_literal)?
                    && PROTOCOL_MODULES
                        .contains(&js::get_string(self.scope, name, "text")?.as_str())
                {
                    return Ok(true);
                }
            }
            current = js::get_defined(self.scope, node, "parent")?;
        }
        Ok(false)
    }

    pub(crate) fn allocate_node_id(&mut self, site: Val<'s>) -> FaceResult<TypeNodeId> {
        let location = self.location_key(site)?;
        let ordinal = self.node_ordinals.entry(location.clone()).or_insert(0);
        *ordinal += 1;
        Ok(TypeNodeId::from(format!("type:{location}#{ordinal}")))
    }

    pub(crate) fn location_key(&mut self, node: Val<'s>) -> FaceResult<String> {
        let location = self.location(node)?;
        Ok(format!(
            "{}:{}:{}",
            location.file, location.line, location.column
        ))
    }

    pub(crate) fn location(&mut self, node: Val<'s>) -> FaceResult<SourceLocation> {
        Ok(self.syntax.location(self.scope, &self.root, node)?)
    }

    pub(crate) fn fail(&mut self, node: Val<'s>, message: &str) -> FaceResult<Interrupt> {
        let location = self.location(node)?;
        Ok(Interrupt::Failed(TypertGeneratorError::Analysis(format!(
            "typert({}): {}:{}:{}: {message}",
            self.face.as_str(),
            location.file,
            location.line,
            location.column
        ))))
    }
}

/// Package identities of the Typert protocol across the rename boundary.
pub(crate) const PROTOCOL_PACKAGES: &[&str] = &[
    "@seekdeep-ai/seekdeep-typert-protocol",
    "@deepseek-ai/dsh-typert-protocol",
];

fn has_package_surface(model: &PackageModel) -> bool {
    !model.services.is_empty()
        || !model.events.is_empty()
        || !model.objects.is_empty()
        || !model.schemas.is_empty()
        || !model.invocations.is_empty()
}

pub(crate) fn exposable_member(member: &MemberModel) -> bool {
    match member {
        MemberModel::Defined(member) => {
            member.base.visibility == crate::model::MemberVisibility::Public
                && !member.base.is_static
        }
        MemberModel::Unsupported(_) => false,
    }
}

impl DocumentationModel {
    pub(crate) fn empty() -> Self {
        Self::default()
    }
}
