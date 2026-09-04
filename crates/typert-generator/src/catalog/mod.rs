//! Cordis documentation validation and projections over independently analyzed faces.

use std::{collections::HashSet, sync::LazyLock};

use indexmap::{IndexMap, IndexSet};
use seekdeep_cordis_api_catalog::RuntimeApiCatalog;
use serde::{Deserialize, Serialize};

use crate::{
    Result, TypertGeneratorError,
    model::{
        DeclarationKind, FaceModel, MemberKind, MemberModel, ParameterBinding, ParameterModel,
        ServiceModel, SignatureModel, SourceDeclarationModel, SourceLocation, TypeNodeId,
        TypeNodeKind, TypeNodeModel, TypeTargetModel, TypertFace, child_type_node_ids,
    },
    renderer::TypeGraphRenderer,
    text::{locale_compare, utf16_compare},
};

mod jsdoc;
mod markdown;
mod runtime;
mod runtime_text;

pub use markdown::{REGION_BEGIN, REGION_END, render_inherited_page, render_page_region};

/// Validated source dispatch mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Synchronous broadcast.
    Emit,
    /// First nonempty result.
    Bail,
    /// Listener-controlled continuation.
    Waterfall,
    /// Concurrent awaited listeners.
    Parallel,
    /// Sequential awaited listeners.
    Serial,
}

impl Mode {
    /// Source documentation spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Emit => "emit",
            Self::Bail => "bail",
            Self::Waterfall => "waterfall",
            Self::Parallel => "parallel",
            Self::Serial => "serial",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "emit" => Some(Self::Emit),
            "bail" => Some(Self::Bail),
            "waterfall" => Some(Self::Waterfall),
            "parallel" => Some(Self::Parallel),
            "serial" => Some(Self::Serial),
            _ => None,
        }
    }
}

/// One validated listener declaration and its source documentation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventEntry {
    /// Scoped event name.
    pub name: String,
    /// Prefix before the first slash.
    pub scope: String,
    /// Exact authored body-free signature.
    pub signature: String,
    /// Dedented original documentation.
    pub js_doc: String,
    /// Validated dispatch mode.
    pub mode: Mode,
    /// Description paragraphs without block tags.
    pub doc: String,
    /// Workspace-relative file and one-based line.
    pub source: String,
}

/// Public catalog member category.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceMemberKind {
    /// Authored method contract.
    Method,
    /// Documented property retained only in the runtime catalog.
    Property,
}

/// One service member and its retained contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceMethodEntry {
    /// Source category; curated framework entries may omit it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<ServiceMemberKind>,
    /// Body-free signature.
    pub signature: String,
    /// Dedented original documentation.
    pub js_doc: String,
}

/// One selected service declaration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceEntry {
    /// Cordis Context property name.
    pub key: String,
    /// Declaring class or interface name.
    #[serde(rename = "type")]
    pub type_name: String,
    /// Abstract class marker.
    #[serde(rename = "abstract")]
    pub is_abstract: bool,
    /// Complete class-level description.
    pub doc: String,
    /// Public member contracts in authored order.
    pub methods: Vec<ServiceMethodEntry>,
    /// Source declaration pointer.
    pub source: String,
}

/// Curated inherited Cordis entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InheritedEntry {
    /// Display name.
    pub name: String,
    /// One-line description.
    pub summary: String,
    /// Source file and line.
    pub source: String,
}

/// Repository-owned classifications and inherited catalog entries.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CordisCatalogPolicy {
    /// Type names mapped to their documentation pages.
    pub linked_type_pages: IndexMap<String, String>,
    /// TypeScript or framework names that need no repository link.
    pub foundation_type_names: IndexSet<String>,
    /// Types documented elsewhere, with their owner descriptions.
    pub type_link_exemptions: IndexMap<String, String>,
    /// Curated framework services visible to runtime inspection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_services: Option<Vec<ServiceEntry>>,
    /// Service keys forbidden to dynamic plugins.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_service_exclusions: Option<IndexSet<String>>,
    /// Framework events in curated order.
    pub inherited_events: Vec<InheritedEntry>,
    /// Framework Context members in curated order.
    pub inherited_services: Vec<InheritedEntry>,
}

/// Validated documentation partition for one face.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CordisCatalogModel {
    /// Events in source package/member order.
    pub events: Vec<EventEntry>,
    /// Selected services sorted by key.
    pub services: Vec<ServiceEntry>,
}

/// Validates and projects an analyzed face without a compiler or runtime registry.
pub struct CordisCatalogProjector<'a> {
    face: &'a FaceModel,
    source_declarations: &'a [SourceDeclarationModel],
    policy: &'a CordisCatalogPolicy,
    renderer: TypeGraphRenderer<'a>,
}

impl<'a> CordisCatalogProjector<'a> {
    /// Binds the projector to caller-owned model and policy data.
    pub fn new(
        face: &'a FaceModel,
        source_declarations: &'a [SourceDeclarationModel],
        policy: &'a CordisCatalogPolicy,
    ) -> Self {
        Self {
            face,
            source_declarations,
            policy,
            renderer: TypeGraphRenderer::new(&face.graph),
        }
    }

    /// Validates events before services, preserving source diagnostic precedence.
    ///
    /// # Errors
    /// Aggregates documentation violations before type-link violations for each partition.
    pub fn project(&self) -> Result<CordisCatalogModel> {
        Ok(CordisCatalogModel {
            events: self.collect_events()?,
            services: self.collect_services()?,
        })
    }

    /// Produces the structured catalog consumed by native and WASM query code.
    ///
    /// # Errors
    /// Rejects invalid type-name regular expressions in the source declaration closure.
    pub fn runtime_catalog(&self, model: &CordisCatalogModel) -> Result<RuntimeApiCatalog> {
        let mut services = model
            .services
            .iter()
            .chain(self.policy.runtime_services.as_deref().unwrap_or_default())
            .filter(|service| {
                self.policy
                    .runtime_service_exclusions
                    .as_ref()
                    .is_none_or(|excluded| !excluded.contains(&service.key))
            })
            .collect::<Vec<_>>();
        services.sort_by(|left, right| locale_compare(&left.key, &right.key));
        let types = runtime::referenced_types(
            &services,
            &model.events,
            self.source_declarations,
            self.face.face,
        )?;
        Ok(runtime::catalog(
            &services,
            &model.events,
            types,
            &self.policy.inherited_services,
        ))
    }

    /// Renders the source-compatible static API text from the same catalog data.
    ///
    /// # Errors
    /// Propagates type-name closure failures.
    pub fn render_runtime_api(&self, model: &CordisCatalogModel) -> Result<String> {
        Ok(runtime::render(&self.runtime_catalog(model)?))
    }

    fn collect_events(&self) -> Result<Vec<EventEntry>> {
        let mut entries = Vec::new();
        let mut violations = Vec::new();
        let mut links = Vec::new();
        for package in &self.face.packages {
            for event in &package.events {
                let parsed =
                    jsdoc::parse(event.documentation.js_doc.as_deref().unwrap_or_default());
                if parsed.deprecated {
                    continue;
                }
                let source = pointer(&event.location);
                let where_ = format!("event '{}' ({source})", event.name);
                let node = self.renderer.node(&event.signature)?;
                let TypeNodeModel::Defined(node) = node else {
                    violations.push(format!("{where_} is not represented by a callable type."));
                    continue;
                };
                let TypeNodeKind::Function { signature } = &node.kind else {
                    violations.push(format!("{where_} is not represented by a callable type."));
                    continue;
                };
                self.check_type_links(&where_, signature, &mut links)?;
                let mode = event.mode.as_deref().and_then(Mode::parse);
                let has_next = signature
                    .parameters
                    .last()
                    .is_some_and(|parameter| parameter.name == "next");
                if let Some(mode) = mode {
                    if has_next && mode != Mode::Waterfall {
                        violations.push(format!("{where_} has a trailing 'next' parameter (structurally a waterfall) but is tagged '@mode {}'. Fix the tag or the signature.", mode.as_str()));
                    } else if !has_next && mode == Mode::Waterfall {
                        violations.push(format!("{where_} is tagged '@mode waterfall' but has no trailing 'next' parameter. A waterfall delegates via next()."));
                    }
                } else {
                    violations.push(format!("{where_} is missing an @mode tag. Add '@mode emit|bail|waterfall|parallel|serial' to its JSDoc (see AGENTS.md)."));
                }
                if parsed.doc.is_empty() {
                    violations.push(format!("{where_} has no description prose. Say what happened / what a listener may do, above the block tags."));
                }
                check_params(
                    &where_,
                    "event",
                    &signature.parameters,
                    &parsed.params,
                    has_next,
                    &mut violations,
                );
                if let Some(mode) = mode {
                    entries.push(EventEntry {
                        name: event.name.clone(),
                        scope: event
                            .name
                            .split('/')
                            .next()
                            .unwrap_or(&event.name)
                            .to_owned(),
                        signature: event.text.clone(),
                        js_doc: event.documentation.js_doc.clone().unwrap_or_default(),
                        mode,
                        doc: parsed.doc,
                        source,
                    });
                }
            }
        }
        report(&violations, &links)?;
        Ok(entries)
    }

    fn renderable_services(&self) -> Result<Vec<&'a ServiceModel>> {
        let mut chosen = IndexMap::<&str, &ServiceModel>::new();
        for package in &self.face.packages {
            for service in &package.services {
                let declaration = self.renderer.declaration(&service.symbol)?;
                if !matches!(
                    declaration.kind,
                    DeclarationKind::Class | DeclarationKind::Interface
                ) {
                    continue;
                }
                let Some(owner) = source_owner(&service.location.file) else {
                    continue;
                };
                let tail = &service.location.file[owner.len()..];
                let matches_face = match self.face.face {
                    TypertFace::Host => {
                        static HOST_FILE: LazyLock<regress::Regex> = LazyLock::new(|| {
                            regress::Regex::new(r"^[^/]+\.ts$").expect("source Host file pattern")
                        });
                        HOST_FILE.find(tail).is_some()
                    }
                    TypertFace::Client => tail.strip_prefix("client/").is_some_and(is_source_file),
                };
                if !matches_face || !declaration.location.file.starts_with(owner) {
                    continue;
                }
                if let Some(current) = chosen.get(service.key.as_str())
                    && self.renderer.declaration(&current.symbol)?.kind == DeclarationKind::Class
                {
                    continue;
                }
                chosen.insert(&service.key, service);
            }
        }
        Ok(chosen.into_values().collect())
    }

    fn collect_services(&self) -> Result<Vec<ServiceEntry>> {
        let mut entries = Vec::new();
        let mut violations = Vec::new();
        let mut links = Vec::new();
        for service in self.renderable_services()? {
            let declaration = self.renderer.declaration(&service.symbol)?;
            let parsed = jsdoc::parse(
                declaration
                    .documentation
                    .js_doc
                    .as_deref()
                    .unwrap_or_default(),
            );
            if parsed.deprecated {
                continue;
            }
            let source = pointer(&declaration.location);
            if parsed.doc.is_empty() {
                violations.push(format!(
                    "service ctx.{} ({source}): {} {} has no JSDoc.",
                    service.key,
                    declaration.kind.as_str(),
                    declaration.name
                ));
            }
            let mut methods = Vec::new();
            for id in &service.members {
                let member = self.renderer.member(id)?;
                let MemberModel::Defined(member) = member else {
                    continue;
                };
                let base = &member.base;
                if base.name.starts_with('[') {
                    continue;
                }
                let parsed = jsdoc::parse(base.documentation.js_doc.as_deref().unwrap_or_default());
                if parsed.deprecated {
                    continue;
                }
                if let MemberKind::Property { .. } = member.kind {
                    if let Some(js_doc) = &base.documentation.js_doc {
                        methods.push(ServiceMethodEntry {
                            kind: Some(ServiceMemberKind::Property),
                            signature: base.text.clone(),
                            js_doc: js_doc.clone(),
                        });
                    }
                    continue;
                }
                let MemberKind::Method { signature } = &member.kind else {
                    continue;
                };
                let where_ = format!(
                    "service method ctx.{}.{} ({})",
                    service.key,
                    base.name,
                    pointer(&base.location)
                );
                self.check_type_links(&where_, signature, &mut links)?;
                methods.push(ServiceMethodEntry {
                    kind: Some(ServiceMemberKind::Method),
                    signature: base.text.clone(),
                    js_doc: base.documentation.js_doc.clone().unwrap_or_default(),
                });
                if base.documentation.js_doc.is_none() {
                    violations.push(format!("{where_} has no JSDoc."));
                    continue;
                }
                if parsed.doc.is_empty() {
                    violations.push(format!(
                        "{where_} has no description prose above its block tags."
                    ));
                }
                check_params(
                    &where_,
                    "service",
                    &signature.parameters,
                    &parsed.params,
                    false,
                    &mut violations,
                );
                self.check_returns(
                    &where_,
                    signature,
                    parsed.returns.as_deref(),
                    &mut violations,
                )?;
            }
            entries.push(ServiceEntry {
                key: service.key.clone(),
                type_name: declaration.name.clone(),
                is_abstract: declaration.is_abstract,
                doc: parsed.doc,
                methods,
                source,
            });
        }
        report(&violations, &links)?;
        entries.sort_by(|left, right| locale_compare(&left.key, &right.key));
        Ok(entries)
    }

    fn check_returns(
        &self,
        where_: &str,
        signature: &SignatureModel,
        description: Option<&str>,
        violations: &mut Vec<String>,
    ) -> Result<()> {
        let returns = self.renderer.render_type(&signature.returns, None)?;
        if returns == "void" || returns == "Promise<void>" {
            return Ok(());
        }
        match description {
            None => violations.push(format!(
                "{where_} is missing @returns (return type: {returns})."
            )),
            Some(value) if jsdoc::trim(value).is_empty() => {
                violations.push(format!("{where_}: @returns has an empty description."));
            }
            Some(_) => {}
        }
        Ok(())
    }

    fn check_type_links(
        &self,
        where_: &str,
        signature: &SignatureModel,
        violations: &mut Vec<String>,
    ) -> Result<()> {
        if self.face.face != TypertFace::Host {
            return Ok(());
        }
        for name in signature_type_names(&self.renderer, signature)? {
            if self.policy.linked_type_pages.contains_key(&name)
                || self.policy.foundation_type_names.contains(&name)
                || self.policy.type_link_exemptions.contains_key(&name)
            {
                continue;
            }
            violations.push(format!("{where_} references unclassified type '{name}'. Add it to linkedTypePages with its documentation page, to foundationTypeNames if TypeScript or the framework owns it, or to typeLinkExemptions with the non-catalog documentation owner."));
        }
        Ok(())
    }
}

fn check_params(
    where_: &str,
    api_kind: &str,
    parameters: &[ParameterModel],
    tags: &IndexMap<String, String>,
    has_next: bool,
    violations: &mut Vec<String>,
) {
    for (index, parameter) in parameters.iter().enumerate() {
        if parameter.binding != ParameterBinding::Identifier {
            violations.push(format!("{where_}: parameter '{}' is a binding pattern; the {api_kind} API needs simple identifier parameters so @param can name them.", parameter.name));
            continue;
        }
        if parameter.receiver || (has_next && index + 1 == parameters.len()) {
            continue;
        }
        match tags.get(&parameter.name) {
            None => violations.push(format!("{where_} is missing @param {}.", parameter.name)),
            Some(value) if jsdoc::trim(value).is_empty() => violations.push(format!(
                "{where_}: @param {} has an empty description.",
                parameter.name
            )),
            Some(_) => {}
        }
    }
    for name in tags.keys() {
        if !parameters.iter().any(|parameter| {
            parameter.binding == ParameterBinding::Identifier && parameter.name == *name
        }) {
            violations.push(format!(
                "{where_}: @param {name} does not match any parameter (stale tag?)."
            ));
        }
    }
}

fn report(violations: &[String], links: &[String]) -> Result<()> {
    let (count, kind, values) = if !violations.is_empty() {
        (
            violations.len(),
            "JSDoc completeness violation(s) (see AGENTS.md)",
            violations,
        )
    } else if !links.is_empty() {
        (
            links.len(),
            "signature type-link coverage violation(s)",
            links,
        )
    } else {
        return Ok(());
    };
    Err(TypertGeneratorError::Catalog(format!(
        "gen-cordis-catalog: {count} {kind}:\n{}",
        values
            .iter()
            .map(|value| format!("  {value}"))
            .collect::<Vec<_>>()
            .join("\n")
    )))
}

fn pointer(location: &SourceLocation) -> String {
    format!("{}:{}", location.file, location.line)
}

enum TypePattern<'a> {
    Identifier(&'a str),
    Expression(regress::Regex),
}

impl TypePattern<'_> {
    fn is_match(&self, text: &str) -> bool {
        match self {
            Self::Identifier(name) => text.match_indices(*name).any(|(index, _)| {
                let bytes = text.as_bytes();
                let word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
                (index == 0 || !word(bytes[index - 1]))
                    && bytes
                        .get(index + name.len())
                        .is_none_or(|byte| !word(*byte))
            }),
            Self::Expression(pattern) => pattern.find(text).is_some(),
        }
    }
}

fn type_pattern(name: &str) -> Result<TypePattern<'_>> {
    static PREFIX_QUANTIFIER: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"^(?:[*+?]|\{[0-9]+(?:,[0-9]*)?\})").expect("quantifier prefix")
    });
    if !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Ok(TypePattern::Identifier(name));
    }
    let pattern = format!(r"\b{name}\b");
    if PREFIX_QUANTIFIER.is_match(name) {
        return Err(TypertGeneratorError::Syntax(format!(
            "Invalid regular expression: /{pattern}/: Nothing to repeat"
        )));
    }
    regress::Regex::new(&pattern)
        .map(TypePattern::Expression)
        .map_err(|error| {
            let reason = match error.text.as_str() {
                "Unbalanced bracket" => "Unterminated character class",
                "Invalid character range"
                | "Range values reversed, start char code is greater than end char code." => {
                    "Range out of order in character class"
                }
                "Unbalanced parenthesis" if unmatched_close(&pattern) => "Unmatched ')'",
                "Unbalanced parenthesis" => "Unterminated group",
                "Invalid group modifier" => "Invalid group",
                "Quantifier not allowed here" | "Invalid atom character" => "Nothing to repeat",
                "Invalid token at named capture group identifier" => "Invalid capture group name",
                reason => reason,
            };
            TypertGeneratorError::Syntax(format!(
                "Invalid regular expression: /{pattern}/: {reason}"
            ))
        })
}

fn unmatched_close(pattern: &str) -> bool {
    let mut depth = 0_usize;
    let mut escaped = false;
    let mut in_class = false;
    for character in pattern.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' => escaped = true,
            '[' if !in_class => in_class = true,
            ']' if in_class => in_class = false,
            '(' if !in_class => depth += 1,
            ')' if !in_class => {
                if depth == 0 {
                    return true;
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    false
}

fn source_owner(path: &str) -> Option<&str> {
    let mut components = path.splitn(5, '/');
    if components.next()? != "packages" {
        return None;
    }
    let group = components.next()?;
    let package = components.next()?;
    if group.is_empty() || package.is_empty() || components.next()? != "src" {
        return None;
    }
    let tail = components.next()?;
    Some(&path[..path.len() - tail.len()])
}

fn is_source_file(path: &str) -> bool {
    static FILE: LazyLock<regress::Regex> = LazyLock::new(|| {
        regress::Regex::new(r"^.+\.tsx?$").expect("source declaration file pattern")
    });
    FILE.find(path).is_some()
}

fn signature_type_names(
    renderer: &TypeGraphRenderer<'_>,
    signature: &SignatureModel,
) -> Result<Vec<String>> {
    let mut walk = SignatureNames {
        renderer,
        visited: HashSet::new(),
        names: HashSet::new(),
    };
    walk.signature(signature)?;
    let mut names = walk.names.into_iter().collect::<Vec<_>>();
    names.sort_by(|left, right| utf16_compare(left, right));
    Ok(names)
}

struct SignatureNames<'r, 'g> {
    renderer: &'r TypeGraphRenderer<'g>,
    visited: HashSet<TypeNodeId>,
    names: HashSet<String>,
}

impl SignatureNames<'_, '_> {
    fn signature(&mut self, signature: &SignatureModel) -> Result<()> {
        for parameter in &signature.type_parameters {
            if let Some(id) = &parameter.constraint {
                self.node(id)?;
            }
            if let Some(id) = &parameter.default {
                self.node(id)?;
            }
        }
        for parameter in &signature.parameters {
            self.node(&parameter.ty)?;
        }
        self.node(&signature.returns)
    }

    fn node(&mut self, id: &TypeNodeId) -> Result<()> {
        if !self.visited.insert(id.clone()) {
            return Ok(());
        }
        let node = self.renderer.node(id)?;
        if let TypeNodeModel::Defined(node) = node {
            match &node.kind {
                TypeNodeKind::Reference { name, target, .. }
                    if !matches!(target, TypeTargetModel::TypeParameter { .. }) =>
                {
                    self.names.insert(name.clone());
                }
                TypeNodeKind::TypeQuery { expression, .. } => {
                    self.names.insert(expression.clone());
                }
                _ => {}
            }
        }
        for child in child_type_node_ids(node)? {
            self.node(&child)?;
        }
        if let TypeNodeModel::Defined(node) = node {
            match &node.kind {
                TypeNodeKind::Object { members } => {
                    for member in members {
                        let MemberModel::Defined(member) = member else {
                            continue;
                        };
                        match &member.kind {
                            MemberKind::Property { ty } => self.node(ty)?,
                            kind => {
                                if let Some(signature) = kind.signature() {
                                    self.signature(signature)?;
                                }
                            }
                        }
                    }
                }
                TypeNodeKind::Function { signature }
                | TypeNodeKind::Constructor { signature, .. } => self.signature(signature)?,
                _ => {}
            }
        }
        Ok(())
    }
}
