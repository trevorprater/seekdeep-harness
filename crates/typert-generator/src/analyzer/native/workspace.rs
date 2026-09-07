//! Workspace registrations, shared compiler caches, and face orchestration.
#![expect(
    clippy::too_many_lines,
    reason = "Each routine mirrors one source analyzer method so differential review stays line-aligned"
)]

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use v8::MapFnTo as _;

use crate::{
    Result, TypertGeneratorError,
    analyzer::{
        client_export_subpaths, host_export_subpaths, is_dual_face_package, merge_workspace_models,
        package_export_targets, source_path_for_export,
    },
    model::{CrossFaceLink, SourceDeclarationModel, SourceLocation, TypertFace, WorkspaceModel},
    text::locale_compare,
};

use super::{
    engine::{Compiler, locate_library},
    face::{FaceAnalyzer, FaceAnalyzerOptions, Interrupt},
    js::{self, JsMap, Scope, Val},
    paths,
    syntax::{self, Syntax},
};

/// Missing-annotation handling at public business boundaries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnalysisMode {
    /// Fail on a missing public annotation.
    Check,
    /// Write inferred annotations, then re-analyze cleanly.
    Write,
}

/// Workspace analysis configuration.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceAnalyzerOptions {
    /// Workspace root containing the face tsconfigs.
    pub root: PathBuf,
    /// Host aggregate path, relative to `root`; absent files are skipped.
    #[serde(default)]
    pub host_config: Option<String>,
    /// Client aggregate path, relative to `root`; absent files are skipped.
    #[serde(default)]
    pub client_config: Option<String>,
    /// Optional package-name subset for an incremental generation pass.
    #[serde(default)]
    pub packages: Option<Vec<String>>,
    /// Independently compiled faces to materialize; both are analyzed by default.
    #[serde(default)]
    pub faces: Option<Vec<TypertFace>>,
    /// Whether to repeat TypeScript project diagnostics before model extraction.
    #[serde(default)]
    pub check_diagnostics: Option<bool>,
    /// Whether missing annotations fail or are written before a clean re-analysis.
    #[serde(default)]
    pub mode: Option<AnalysisMode>,
    /// Shared workspace memo; supply one instance to reuse parses across analyzers.
    #[serde(skip)]
    pub caches: Option<Rc<RefCell<WorkspaceCaches>>>,
}

/// One package face whose public export graph contains Typert business declarations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredTypertPackage {
    /// Package manifest name.
    pub package: String,
    /// Workspace-relative package root.
    pub root: String,
    /// Faces contributing this package, in stable order.
    pub faces: Vec<TypertFace>,
}

/// One parsed tsconfig, memoizable per workspace snapshot.
#[derive(Debug)]
pub(crate) struct ParsedConfig {
    /// Absolute config path as registered.
    pub(crate) path: PathBuf,
    /// Root file names from the compiler's parse.
    pub(crate) file_names: Vec<String>,
    /// Effective compiler options as plain data.
    pub(crate) options: Value,
    /// Referenced project paths as resolved by the compiler.
    pub(crate) project_references: Vec<String>,
}

/// One package face registration discovered from an aggregate tsconfig.
#[derive(Clone, Debug)]
pub(crate) struct PackageRegistration {
    pub(crate) face: TypertFace,
    pub(crate) name: String,
    pub(crate) root: PathBuf,
    pub(crate) config: Rc<ParsedConfig>,
    pub(crate) manifest: Value,
    pub(crate) export_subpaths: Option<Vec<String>>,
}

/// One inferred annotation waiting to be written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SourceEdit {
    pub(crate) file: PathBuf,
    pub(crate) position: usize,
    pub(crate) text: String,
}

struct FaceProgramHost {
    host: v8::Global<v8::Object>,
    files: v8::Global<v8::Map>,
}

/// Per-face compiler memo living inside one engine.
#[derive(Default)]
pub(crate) struct CacheState {
    configs: HashMap<PathBuf, Rc<ParsedConfig>>,
    registrations: HashMap<String, Rc<Vec<PackageRegistration>>>,
    hosts: HashMap<TypertFace, FaceProgramHost>,
    library_parses: Option<v8::Global<v8::Map>>,
}

/// Shared memo over one immutable workspace snapshot.
///
/// Passing one instance to several analyzers reuses parsed tsconfigs, the
/// registration inventory, and per-face compiler hosts whose parsed and bound
/// source files and module resolutions carry across programs. Callers that
/// mutate workspace files between analyses must start from a fresh instance;
/// write-mode source edits invalidate themselves.
pub struct WorkspaceCaches {
    state: CacheState,
    compiler: Compiler,
}

impl std::fmt::Debug for WorkspaceCaches {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkspaceCaches")
            .field("compiler", &self.compiler)
            .finish_non_exhaustive()
    }
}

impl WorkspaceCaches {
    /// Binds a memo to one loaded compiler.
    pub fn new(compiler: Compiler) -> Self {
        Self {
            state: CacheState::default(),
            compiler,
        }
    }

    /// Loads the compiler library a workspace resolves through `node_modules`.
    ///
    /// # Errors
    /// Reports an unlocatable or unloadable compiler library.
    pub fn for_workspace(root: &Path) -> Result<Self> {
        Ok(Self::new(Compiler::load(&locate_library(root)?)?))
    }

    /// The loaded compiler.
    pub fn compiler(&self) -> &Compiler {
        &self.compiler
    }

    fn parts(&mut self) -> (&mut Compiler, &mut CacheState) {
        (&mut self.compiler, &mut self.state)
    }
}

impl CacheState {
    /// Parses one tsconfig once per workspace snapshot.
    fn config<'s>(
        &mut self,
        scope: &mut Scope<'s, '_>,
        ts: Val<'s>,
        path: &Path,
    ) -> Result<Rc<ParsedConfig>> {
        if let Some(config) = self.configs.get(path) {
            return Ok(config.clone());
        }
        let config = Rc::new(parse_config(scope, ts, path)?);
        self.configs.insert(path.to_owned(), config.clone());
        Ok(config)
    }

    /// Returns the shared compiler host for one face.
    ///
    /// Every program of one face is built from the same aggregate compiler
    /// options (the first call wins), so parsed source files, binder state, and
    /// module resolutions are safe to reuse across the face's batched programs.
    fn program_host<'s>(
        &mut self,
        scope: &mut Scope<'s, '_>,
        ts: Val<'s>,
        face: TypertFace,
        options: Val<'s>,
    ) -> Result<Val<'s>> {
        if let Some(entry) = self.hosts.get(&face) {
            return Ok(v8::Local::new(scope, &entry.host).into());
        }
        let host = js::call(scope, ts, "createCompilerHost", &[options])?;
        let files = JsMap::new(scope);
        let library_parses = if let Some(map) = &self.library_parses {
            v8::Local::new(scope, map)
        } else {
            let map = v8::Map::new(scope);
            self.library_parses = Some(v8::Global::new(scope, map));
            map
        };
        let current_directory = js::call(scope, host, "getCurrentDirectory", &[])?;
        let canonical = js::get(scope, host, "getCanonicalFileName")?;
        let bound = js::call(scope, canonical, "bind", &[host])?;
        let resolution_cache = js::call(
            scope,
            ts,
            "createModuleResolutionCache",
            &[current_directory, bound, options],
        )?;
        let base = js::get(scope, host, "getSourceFile")?;
        let data = js::object(
            scope,
            &[
                ("host", host),
                ("base", base),
                ("files", files.value()),
                ("libraries", library_parses.into()),
            ],
        )?;
        let get_source_file = callback(scope, cached_source_file.map_fn_to(), data)?;
        js::set(scope, host, "getSourceFile", get_source_file)?;
        let cache_data = js::object(scope, &[("cache", resolution_cache)])?;
        let get_cache = callback(scope, module_resolution_cache.map_fn_to(), cache_data)?;
        js::set(scope, host, "getModuleResolutionCache", get_cache)?;
        let host_object = host
            .to_object(scope)
            .ok_or_else(|| js::failure("compiler host is not an object"))?;
        let files_map = v8::Local::<v8::Map>::try_from(files.value())
            .map_err(|_| js::failure("host file cache is not a Map"))?;
        self.hosts.insert(
            face,
            FaceProgramHost {
                host: v8::Global::new(scope, host_object),
                files: v8::Global::new(scope, files_map),
            },
        );
        Ok(host)
    }

    /// Drops cached parses of one edited source file.
    fn invalidate(&mut self, scope: &mut Scope<'_, '_>, file: &Path) -> Result<()> {
        let target = paths::real_path(file);
        for entry in self.hosts.values() {
            let files = JsMap::from_value(v8::Local::new(scope, &entry.files).into())?;
            for (key, _) in files.entries(scope)? {
                let name = js::text(scope, key);
                if paths::real_path(Path::new(&name)) == target {
                    files.delete(scope, key);
                }
            }
        }
        Ok(())
    }
}

fn callback<'s>(
    scope: &mut Scope<'s, '_>,
    function: v8::FunctionCallback,
    data: Val<'s>,
) -> Result<Val<'s>> {
    Ok(v8::Function::builder_raw(function)
        .data(data)
        .build(scope)
        .ok_or_else(|| js::failure("cannot create compiler host callback"))?
        .into())
}

fn default_library_key<'s>(
    scope: &mut Scope<'s, '_>,
    file_name: &str,
    options: Val<'s>,
) -> Result<String> {
    let (language_version, node_format, jsdoc_mode) = if options.is_object() {
        (
            js::get(scope, options, "languageVersion")?,
            js::get(scope, options, "impliedNodeFormat")?,
            js::get(scope, options, "jsDocParsingMode")?,
        )
    } else {
        let undefined = js::undefined(scope);
        (options, undefined, undefined)
    };
    let render = |scope: &mut Scope<'s, '_>, value: Val<'s>| {
        if value.is_null_or_undefined() {
            String::new()
        } else {
            js::text(scope, value)
        }
    };
    let version = js::text(scope, language_version);
    let node_format = render(scope, node_format);
    let jsdoc_mode = render(scope, jsdoc_mode);
    Ok([file_name, &version, &node_format, &jsdoc_mode].join("\0"))
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "V8 callback ABI passes arguments by value"
)]
fn cached_source_file<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut result: v8::ReturnValue<'s>,
) {
    let data = args.data();
    let outcome = (|| -> Result<Val<'s>> {
        let host = js::get(scope, data, "host")?;
        let base = js::get(scope, data, "base")?;
        let file_name = js::text(scope, args.get(0));
        let arguments = (0..args.length())
            .map(|index| args.get(index))
            .collect::<Vec<_>>();
        let (map, key) = if crate::analyzer::is_standard_library_file(&file_name) {
            let key = default_library_key(scope, &file_name, args.get(1))?;
            (
                JsMap::from_value(js::get(scope, data, "libraries")?)?,
                js::string(scope, &key),
            )
        } else {
            (
                JsMap::from_value(js::get(scope, data, "files")?)?,
                args.get(0),
            )
        };
        if !map.has(scope, key) {
            let parsed = js::invoke(scope, base, host, &arguments)?;
            map.set(scope, key, parsed);
        }
        Ok(map.get(scope, key).unwrap_or_else(|| js::undefined(scope)))
    })();
    match outcome {
        Ok(value) => result.set(value),
        Err(error) => {
            let message = js::string(scope, &error.to_string());
            let exception =
                v8::Exception::error(scope, message.try_into().expect("message string"));
            scope.throw_exception(exception);
        }
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "V8 callback ABI passes arguments by value"
)]
fn module_resolution_cache<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut result: v8::ReturnValue<'s>,
) {
    match js::get(scope, args.data(), "cache") {
        Ok(cache) => result.set(cache),
        Err(_) => result.set_undefined(),
    }
}

fn parse_config<'s>(scope: &mut Scope<'s, '_>, ts: Val<'s>, path: &Path) -> Result<ParsedConfig> {
    let compiler_path = paths::slash(path);
    let system = js::get(scope, ts, "sys")?;
    let read_file = js::get(scope, system, "readFile")?;
    let path_value = js::string(scope, &compiler_path);
    let read = js::call(scope, ts, "readConfigFile", &[path_value, read_file])?;
    if let Some(error) = js::get_defined(scope, read, "error")? {
        return Err(TypertGeneratorError::Analysis(format_diagnostic(
            scope, ts, error,
        )?));
    }
    let config = js::get(scope, read, "config")?;
    let directory = js::string(
        scope,
        &paths::slash(&paths::dirname(Path::new(&compiler_path))),
    );
    let undefined = js::undefined(scope);
    let parsed = js::call(
        scope,
        ts,
        "parseJsonConfigFileContent",
        &[config, system, directory, undefined, path_value],
    )?;
    let errors = js::get_items(scope, parsed, "errors")?;
    if !errors.is_empty() {
        let mut messages = Vec::new();
        for error in errors {
            messages.push(format_diagnostic(scope, ts, error)?);
        }
        return Err(TypertGeneratorError::Analysis(messages.join("\n")));
    }
    let file_names = js::get_items(scope, parsed, "fileNames")?
        .into_iter()
        .map(|value| js::text(scope, value))
        .collect();
    let options = js::get(scope, parsed, "options")?;
    let options = js::to_json(scope, options)?;
    let mut project_references = Vec::new();
    for reference in js::get_items(scope, parsed, "projectReferences")? {
        project_references.push(js::get_string(scope, reference, "path")?);
    }
    Ok(ParsedConfig {
        path: path.to_owned(),
        file_names,
        options,
        project_references,
    })
}

pub(crate) fn format_diagnostic<'s>(
    scope: &mut Scope<'s, '_>,
    ts: Val<'s>,
    diagnostic: Val<'s>,
) -> Result<String> {
    let message = js::get(scope, diagnostic, "messageText")?;
    let newline = js::string(scope, "\n");
    let flattened = js::call(
        scope,
        ts,
        "flattenDiagnosticMessageText",
        &[message, newline],
    )?;
    Ok(js::text(scope, flattened))
}

fn project_config_path(path: &str) -> PathBuf {
    let path = Path::new(path);
    if path
        .extension()
        .is_some_and(|extension| extension == "json")
    {
        path.to_owned()
    } else {
        paths::join(path, "tsconfig.json")
    }
}

#[derive(Clone, Debug)]
struct ResolvedOptions {
    root: PathBuf,
    host_config: String,
    client_config: String,
    packages: Option<Vec<String>>,
    faces: Vec<TypertFace>,
    check_diagnostics: bool,
    mode: AnalysisMode,
}

/// Analyze host and client as independent TypeScript programs.
pub struct WorkspaceAnalyzer {
    options: ResolvedOptions,
    checked_projects: HashSet<PathBuf>,
    caches: Rc<RefCell<WorkspaceCaches>>,
}

impl std::fmt::Debug for WorkspaceAnalyzer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkspaceAnalyzer")
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
}

impl WorkspaceAnalyzer {
    /// Binds analysis to one workspace and compiler memo.
    ///
    /// # Errors
    /// Reports an unlocatable compiler library when no shared memo is supplied.
    pub fn new(options: WorkspaceAnalyzerOptions) -> Result<Self> {
        let caches = match options.caches {
            Some(caches) => caches,
            None => Rc::new(RefCell::new(WorkspaceCaches::for_workspace(&options.root)?)),
        };
        Ok(Self {
            options: ResolvedOptions {
                root: paths::real_path(&options.root),
                host_config: options
                    .host_config
                    .unwrap_or_else(|| "tsconfig.host.json".to_owned()),
                client_config: options
                    .client_config
                    .unwrap_or_else(|| "tsconfig.client.json".to_owned()),
                packages: options.packages,
                faces: options
                    .faces
                    .unwrap_or_else(|| vec![TypertFace::Host, TypertFace::Client]),
                check_diagnostics: options.check_diagnostics.unwrap_or(true),
                mode: options.mode.unwrap_or(AnalysisMode::Check),
            },
            checked_projects: HashSet::new(),
            caches,
        })
    }

    /// The shared compiler memo.
    pub fn caches(&self) -> Rc<RefCell<WorkspaceCaches>> {
        self.caches.clone()
    }

    fn derived(&self, mode: AnalysisMode, packages: Option<Vec<String>>) -> Self {
        Self {
            options: ResolvedOptions {
                packages,
                mode,
                ..self.options.clone()
            },
            checked_projects: HashSet::new(),
            caches: self.caches.clone(),
        }
    }

    /// Builds the workspace model. Write mode applies inferred annotations and
    /// then returns a fresh check-mode analysis of the edited projects.
    ///
    /// # Errors
    /// Propagates configuration, TypeScript diagnostic, and analysis violations.
    pub fn analyze(&mut self) -> Result<WorkspaceModel> {
        let registrations = self.load_registrations()?;
        let selected = self
            .options
            .packages
            .as_ref()
            .map(|packages| packages.iter().cloned().collect::<HashSet<_>>());
        let options = self.options.clone();
        let mut cross_face_links = IndexMap::<String, CrossFaceLink>::new();
        let checked = &mut self.checked_projects;
        let mut caches = self.caches.borrow_mut();
        let (compiler, state) = caches.parts();
        let constants = compiler.constants().clone();
        let outcome = compiler.run(|scope, ts| {
            let syntax = Syntax::new(scope, ts, &constants)?;
            let mut faces = Vec::new();
            for face in &options.faces {
                let face = *face;
                let face_registrations = registrations
                    .iter()
                    .filter(|registration| {
                        registration.face == face
                            && selected
                                .as_ref()
                                .is_none_or(|selected| selected.contains(&registration.name))
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                if face_registrations.is_empty() {
                    continue;
                }
                if options.check_diagnostics {
                    for registration in &face_registrations {
                        check_project(scope, &syntax, &options, checked, registration)?;
                    }
                }
                let aggregate = state.config(scope, ts, &options.aggregate_path(face))?;
                let mut root_names = Vec::new();
                let mut seen = HashSet::new();
                for registration in &face_registrations {
                    for file in &registration.config.file_names {
                        if seen.insert(file.clone()) {
                            root_names.push(js::string(scope, file));
                        }
                    }
                }
                let mut program_options = aggregate.options.clone();
                program_options["composite"] = json!(false);
                program_options["incremental"] = json!(false);
                program_options["noEmit"] = json!(true);
                let program_options = js::from_json(scope, &program_options)?;
                let host = state.program_host(scope, ts, face, program_options)?;
                let root_names = js::array(scope, &root_names);
                let arguments = js::object(
                    scope,
                    &[
                        ("rootNames", root_names),
                        ("options", program_options),
                        ("host", host),
                    ],
                )?;
                let program = js::call(scope, ts, "createProgram", &[arguments])?;
                let mut analyzer = FaceAnalyzer::new(
                    scope,
                    &syntax,
                    FaceAnalyzerOptions {
                        root: &options.root,
                        face,
                        program,
                        registrations: &face_registrations,
                        all_registrations: &registrations,
                        mode: options.mode,
                        cross_face_links: &mut cross_face_links,
                    },
                )?;
                match analyzer.analyze() {
                    Ok(model) => faces.push(model),
                    Err(Interrupt::Queued(edit)) => return Ok(Err(edit)),
                    Err(Interrupt::Failed(error)) => return Err(error),
                }
            }
            Ok(Ok(faces))
        })?;
        let faces = match outcome {
            Ok(faces) => faces,
            Err(edit) => {
                if options.mode != AnalysisMode::Write {
                    return Err(TypertGeneratorError::Analysis(
                        "typert: annotation edits are only queued in write mode".to_owned(),
                    ));
                }
                apply_edit(compiler, state, &edit)?;
                drop(caches);
                return self
                    .derived(AnalysisMode::Write, options.packages.clone())
                    .analyze();
            }
        };
        drop(caches);
        if options.mode == AnalysisMode::Write {
            return self
                .derived(AnalysisMode::Check, options.packages.clone())
                .analyze();
        }
        let mut cross_face_links = cross_face_links.into_values().collect::<Vec<_>>();
        cross_face_links.sort_by(compare_cross_face_links);
        Ok(WorkspaceModel {
            faces,
            cross_face_links,
        })
    }

    /// Analyzes an explicit package selection through bounded compiler programs.
    ///
    /// # Errors
    /// Rejects a missing package selection or a non-positive batch size before
    /// propagating analysis failures.
    pub fn analyze_in_batches(&mut self, batch_size: usize) -> Result<WorkspaceModel> {
        let Some(packages) = self.options.packages.clone() else {
            return Err(TypertGeneratorError::Analysis(
                "typert: batched analysis requires an explicit package selection".to_owned(),
            ));
        };
        if batch_size < 1 {
            return Err(TypertGeneratorError::Analysis(format!(
                "typert: batch size must be a positive integer, received {batch_size}"
            )));
        }
        let mut batches = Vec::new();
        for chunk in packages.chunks(batch_size) {
            batches.push(
                self.derived(self.options.mode, Some(chunk.to_vec()))
                    .analyze()?,
            );
        }
        Ok(merge_workspace_models(batches))
    }

    /// Discovers package faces from public-export-reachable Cordis augmentations
    /// and explicit `@typert` roots without constructing a type-checker program.
    ///
    /// # Errors
    /// Propagates configuration parsing failures.
    pub fn discover_packages(&mut self) -> Result<Vec<DiscoveredTypertPackage>> {
        let registrations = self.load_registrations()?;
        let options = self.options.clone();
        let mut caches = self.caches.borrow_mut();
        let (compiler, _) = caches.parts();
        let constants = compiler.constants().clone();
        compiler.run(|scope, ts| {
            let syntax = Syntax::new(scope, ts, &constants)?;
            let mut packages = IndexMap::<String, (String, Vec<TypertFace>)>::new();
            for registration in registrations.iter() {
                if !options.faces.contains(&registration.face)
                    || !registration_has_surface(scope, &syntax, registration)?
                {
                    continue;
                }
                let entry = packages
                    .entry(registration.name.clone())
                    .or_insert_with(|| {
                        (
                            paths::relative(&options.root, &registration.root),
                            Vec::new(),
                        )
                    });
                if !entry.1.contains(&registration.face) {
                    entry.1.push(registration.face);
                }
            }
            let mut discovered = packages
                .into_iter()
                .map(|(package, (root, mut faces))| {
                    faces.sort_by(|left, right| {
                        crate::text::utf16_compare(left.as_str(), right.as_str())
                    });
                    DiscoveredTypertPackage {
                        package,
                        root,
                        faces,
                    }
                })
                .collect::<Vec<_>>();
            discovered.sort_by(|left, right| locale_compare(&left.package, &right.package));
            Ok(discovered)
        })
    }

    /// Indexes top-level exported type declarations without promoting them to graph roots.
    ///
    /// # Errors
    /// Propagates configuration parsing and source reading failures.
    #[expect(
        clippy::case_sensitive_file_extension_comparisons,
        reason = "the source matches TypeScript extensions case-sensitively"
    )]
    pub fn index_source_declarations(&mut self) -> Result<Vec<SourceDeclarationModel>> {
        let registrations = self.load_registrations()?;
        let options = self.options.clone();
        let selected = options
            .packages
            .as_ref()
            .map(|packages| packages.iter().cloned().collect::<HashSet<_>>());
        let mut caches = self.caches.borrow_mut();
        let (compiler, _) = caches.parts();
        let constants = compiler.constants().clone();
        compiler.run(|scope, ts| {
            let syntax = Syntax::new(scope, ts, &constants)?;
            let mut declarations = Vec::new();
            for registration in registrations.iter() {
                if !options.faces.contains(&registration.face)
                    || selected
                        .as_ref()
                        .is_some_and(|selected| !selected.contains(&registration.name))
                {
                    continue;
                }
                for file in &registration.config.file_names {
                    let path = Path::new(file);
                    let relative_file = paths::relative(&options.root, path);
                    if !path.exists()
                        || !paths::is_within(
                            &paths::real_path(path),
                            &registration.root.join("src"),
                        )
                        || !(file.ends_with(".cts")
                            || file.ends_with(".mts")
                            || file.ends_with(".ts"))
                    {
                        continue;
                    }
                    let source = std::fs::read(path).map_err(|error| {
                        TypertGeneratorError::Workspace(format!("{}: {error}", path.display()))
                    })?;
                    let source_file = syntax.create_source_file(
                        scope,
                        file,
                        &super::system::decode_file(&source),
                    )?;
                    for statement in js::get_items(scope, source_file, "statements")? {
                        let Some(kind) = syntax.type_declaration_kind(scope, statement)? else {
                            continue;
                        };
                        let Some(name) = js::get_defined(scope, statement, "name")? else {
                            continue;
                        };
                        if !syntax.has_modifier(scope, statement, syntax.kinds.export_keyword)? {
                            continue;
                        }
                        let start = js::call(scope, statement, "getStart", &[source_file])?;
                        let position = js::call(
                            scope,
                            source_file,
                            "getLineAndCharacterOfPosition",
                            &[start],
                        )?;
                        declarations.push(SourceDeclarationModel {
                            face: registration.face,
                            package: registration.name.clone(),
                            name: js::get_string(scope, name, "text")?,
                            kind,
                            location: SourceLocation {
                                file: relative_file.clone(),
                                line: js::offset(js::get_number(scope, position, "line")?) + 1,
                                column: js::offset(js::get_number(scope, position, "character")?)
                                    + 1,
                            },
                            text: syntax.declaration_text(scope, statement)?,
                        });
                    }
                }
            }
            let mut unique = IndexMap::<String, SourceDeclarationModel>::new();
            for declaration in declarations {
                let key = format!(
                    "{}\0{}\0{}\0{}",
                    declaration.face.as_str(),
                    declaration.location.file,
                    declaration.location.line,
                    declaration.name
                );
                unique.entry(key).or_insert(declaration);
            }
            let mut result = unique.into_values().collect::<Vec<_>>();
            result.sort_by(|left, right| {
                locale_compare(left.face.as_str(), right.face.as_str())
                    .then_with(|| locale_compare(&left.location.file, &right.location.file))
                    .then_with(|| left.location.line.cmp(&right.location.line))
            });
            Ok(result)
        })
    }

    fn load_registrations(&mut self) -> Result<Rc<Vec<PackageRegistration>>> {
        let inventory_key = format!(
            "{}\0{}\0{}",
            self.options.root.display(),
            self.options.host_config,
            self.options.client_config
        );
        let options = self.options.clone();
        let mut caches = self.caches.borrow_mut();
        let (compiler, state) = caches.parts();
        if let Some(cached) = state.registrations.get(&inventory_key) {
            return Ok(cached.clone());
        }
        let inventory = compiler.run(|scope, ts| {
            let mut registrations = Vec::new();
            for face in [TypertFace::Host, TypertFace::Client] {
                let aggregate_path = options.aggregate_path(face);
                if !aggregate_path.exists() {
                    continue;
                }
                let aggregate = state.config(scope, ts, &aggregate_path)?;
                for reference in &aggregate.project_references {
                    let config_path = project_config_path(reference);
                    let package_root = paths::dirname(&config_path);
                    if !paths::is_within(
                        &paths::real_path(&package_root),
                        &paths::join(&options.root, "packages"),
                    ) {
                        continue;
                    }
                    let manifest_path = paths::join(&package_root, "package.json");
                    if !manifest_path.exists() {
                        continue;
                    }
                    let contents = std::fs::read(&manifest_path).map_err(|error| {
                        TypertGeneratorError::Workspace(format!(
                            "{}: {error}",
                            manifest_path.display()
                        ))
                    })?;
                    let manifest: Value = serde_json::from_slice(&contents).map_err(|error| {
                        TypertGeneratorError::Syntax(format!(
                            "{}: {error}",
                            manifest_path.display()
                        ))
                    })?;
                    let Some(name) = manifest["name"].as_str().map(str::to_owned) else {
                        continue;
                    };
                    let registration = PackageRegistration {
                        face,
                        name,
                        root: paths::real_path(&package_root),
                        config: state.config(scope, ts, &config_path)?,
                        manifest: manifest.clone(),
                        export_subpaths: None,
                    };
                    if !is_dual_face_package(&manifest) {
                        registrations.push(registration);
                    } else if config_path == paths::join(&package_root, "tsconfig.json") {
                        registrations.push(PackageRegistration {
                            face: TypertFace::Host,
                            export_subpaths: Some(host_export_subpaths(&manifest)),
                            ..registration.clone()
                        });
                        registrations.push(PackageRegistration {
                            face: TypertFace::Client,
                            export_subpaths: Some(client_export_subpaths(&manifest)),
                            ..registration
                        });
                    } else {
                        registrations.push(PackageRegistration {
                            export_subpaths: Some(match face {
                                TypertFace::Host => host_export_subpaths(&manifest),
                                TypertFace::Client => client_export_subpaths(&manifest),
                            }),
                            ..registration
                        });
                    }
                }
            }
            let mut unique = IndexMap::<String, PackageRegistration>::new();
            for registration in registrations {
                let key = format!("{}\0{}", registration.face.as_str(), registration.name);
                unique.entry(key).or_insert(registration);
            }
            let mut inventory = unique.into_values().collect::<Vec<_>>();
            inventory.sort_by(|left, right| {
                locale_compare(left.face.as_str(), right.face.as_str())
                    .then_with(|| locale_compare(&left.name, &right.name))
            });
            Ok(Rc::new(inventory))
        })?;
        state.registrations.insert(inventory_key, inventory.clone());
        Ok(inventory)
    }
}

impl ResolvedOptions {
    fn aggregate_path(&self, face: TypertFace) -> PathBuf {
        let config = match face {
            TypertFace::Host => &self.host_config,
            TypertFace::Client => &self.client_config,
        };
        paths::join(&self.root, config)
    }
}

#[expect(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "the source compares export targets case-sensitively"
)]
pub(crate) fn entry_source_paths(registration: &PackageRegistration) -> Vec<PathBuf> {
    package_export_targets(&registration.manifest)
        .into_iter()
        .filter(|(subpath, target)| {
            registration
                .export_subpaths
                .as_ref()
                .is_none_or(|subpaths| subpaths.contains(subpath))
                && !target.contains('*')
                && subpath != "./package.json"
                && subpath != "./typert"
                && subpath != "./client/typert"
                && subpath != "./remote"
                && !target.ends_with(".json")
        })
        .filter_map(|(_, target)| source_path_for_export(&registration.root, &target).ok())
        .filter(|path| path.exists())
        .collect()
}

fn registration_has_surface<'s>(
    scope: &mut Scope<'s, '_>,
    syntax: &Syntax<'s>,
    registration: &PackageRegistration,
) -> Result<bool> {
    let mut seen = HashSet::new();
    let mut queue = entry_source_paths(registration);
    let options = js::from_json(scope, &registration.config.options)?;
    let system = js::get(scope, syntax.ts, "sys")?;
    while !queue.is_empty() {
        let file = paths::real_path(&queue.remove(0));
        if seen.contains(&file) || !paths::is_within(&file, &registration.root) {
            continue;
        }
        seen.insert(file.clone());
        let source = std::fs::read(&file).map_err(|error| {
            TypertGeneratorError::Workspace(format!("{}: {error}", file.display()))
        })?;
        let source = super::system::decode_file(&source);
        let file_name = file.to_string_lossy().into_owned();
        let source_file = syntax.create_source_file(scope, &file_name, &source)?;
        if syntax::source_file_has_surface(scope, syntax, source_file)? {
            return Ok(true);
        }
        let source_value = js::string(scope, &source);
        let preprocessed = js::call(scope, syntax.ts, "preProcessFile", &[source_value])?;
        for imported in js::get_items(scope, preprocessed, "importedFiles")? {
            let name = js::get(scope, imported, "fileName")?;
            let file_value = js::string(scope, &file_name);
            let resolution = js::call(
                scope,
                syntax.ts,
                "resolveModuleName",
                &[name, file_value, options, system],
            )?;
            if let Some(resolved) = js::get_defined(scope, resolution, "resolvedModule")? {
                let resolved_file =
                    PathBuf::from(js::get_string(scope, resolved, "resolvedFileName")?);
                if paths::is_within(&resolved_file, &registration.root) {
                    queue.push(resolved_file);
                }
            }
        }
    }
    Ok(false)
}

fn check_project<'s>(
    scope: &mut Scope<'s, '_>,
    syntax: &Syntax<'s>,
    options: &ResolvedOptions,
    checked: &mut HashSet<PathBuf>,
    registration: &PackageRegistration,
) -> Result<()> {
    if !checked.insert(registration.config.path.clone()) {
        return Ok(());
    }
    let mut program_options = registration.config.options.clone();
    program_options["composite"] = json!(false);
    program_options["incremental"] = json!(false);
    program_options["noEmit"] = json!(true);
    // Source-plane workspace aliases resolve referenced packages to source.
    // Widen only this diagnostic program's root so those imports do not
    // produce an artificial TS6059 before Typert checks the public edge.
    program_options["rootDir"] = json!(options.root.to_string_lossy());
    let program_options = js::from_json(scope, &program_options)?;
    let root_names = registration
        .config
        .file_names
        .iter()
        .map(|name| js::string(scope, name))
        .collect::<Vec<_>>();
    let root_names = js::array(scope, &root_names);
    let arguments = js::object(
        scope,
        &[("rootNames", root_names), ("options", program_options)],
    )?;
    let program = js::call(scope, syntax.ts, "createProgram", &[arguments])?;
    let syntactic = js::call(scope, program, "getSyntacticDiagnostics", &[])?;
    let mut diagnostics = js::items(scope, syntactic)?;
    let semantic = js::call(scope, program, "getSemanticDiagnostics", &[])?;
    diagnostics.extend(js::items(scope, semantic)?);
    let mut messages = Vec::new();
    for diagnostic in diagnostics {
        let Some(file) = js::get_defined(scope, diagnostic, "file")? else {
            continue;
        };
        let Some(start) = js::get_defined(scope, diagnostic, "start")? else {
            continue;
        };
        let file_name = js::get_string(scope, file, "fileName")?;
        if !paths::is_within(Path::new(&file_name), &registration.root) {
            continue;
        }
        let position = js::call(scope, file, "getLineAndCharacterOfPosition", &[start])?;
        let message = format_diagnostic(scope, syntax.ts, diagnostic)?;
        let code = js::get_number(scope, diagnostic, "code")?;
        messages.push(format!(
            "typert({}): {}:{}:{}: TypeScript TS{}: {message}",
            registration.face.as_str(),
            paths::relative(&options.root, Path::new(&file_name)),
            js::offset(js::get_number(scope, position, "line")?) + 1,
            js::offset(js::get_number(scope, position, "character")?) + 1,
            super::jstext::number_text(code),
        ));
    }
    if messages.is_empty() {
        return Ok(());
    }
    Err(TypertGeneratorError::Analysis(messages.join("\n")))
}

fn apply_edit(compiler: &mut Compiler, state: &mut CacheState, edit: &SourceEdit) -> Result<()> {
    let source = std::fs::read(&edit.file).map_err(|error| {
        TypertGeneratorError::Workspace(format!("{}: {error}", edit.file.display()))
    })?;
    let source = super::system::decode_file(&source);
    let mut units = source.encode_utf16().collect::<Vec<_>>();
    let position = edit.position.min(units.len());
    let inserted = edit.text.encode_utf16().collect::<Vec<_>>();
    units.splice(position..position, inserted);
    std::fs::write(&edit.file, String::from_utf16_lossy(&units)).map_err(|error| {
        TypertGeneratorError::Workspace(format!("{}: {error}", edit.file.display()))
    })?;
    compiler.run(|scope, _| state.invalidate(scope, &edit.file))
}

pub(crate) fn compare_cross_face_links(
    left: &CrossFaceLink,
    right: &CrossFaceLink,
) -> std::cmp::Ordering {
    locale_compare(left.from_face.as_str(), right.from_face.as_str())
        .then_with(|| locale_compare(&left.from_package, &right.from_package))
        .then_with(|| locale_compare(left.to_face.as_str(), right.to_face.as_str()))
        .then_with(|| locale_compare(&left.to_package, &right.to_package))
        .then_with(|| locale_compare(&left.subpath, &right.subpath))
        .then_with(|| locale_compare(&left.name, &right.name))
}
