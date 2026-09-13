//! Rendering and declaration closure over compiler-independent type graphs.

use std::{
    collections::{HashMap, HashSet},
    fmt::Write as _,
};

use crate::{
    Result, TypertGeneratorError,
    model::{
        DeclarationKind, DefinedTypeNode, MappedModifier, MemberId, MemberKind, MemberModel,
        ParameterBinding, ParameterModel, SignatureModel, SymbolId, TemplateSpanModel,
        TupleElementModel, TypeDeclarationModel, TypeGraph, TypeNodeId, TypeNodeKind,
        TypeNodeModel, TypeParameterId, TypeParameterModel, TypeTargetModel, Variance,
        child_type_node_ids,
    },
};

/// Generated names substituted only for declaration references.
pub type ReferenceNames = HashMap<SymbolId, String>;

/// Read-only indexed view retaining graph and declaration order.
pub struct TypeGraphRenderer<'a> {
    /// Complete source graph.
    pub graph: &'a TypeGraph,
    nodes: HashMap<TypeNodeId, usize>,
    declarations: HashMap<SymbolId, usize>,
    members: HashMap<MemberId, (usize, usize)>,
    parameter_names: HashMap<TypeParameterId, String>,
}

impl<'a> TypeGraphRenderer<'a> {
    /// Indexes the complete graph with source-compatible last-write-wins lookup.
    pub fn new(graph: &'a TypeGraph) -> Self {
        let mut renderer = Self {
            graph,
            nodes: graph
                .nodes
                .iter()
                .enumerate()
                .map(|(index, node)| (node.id(), index))
                .collect(),
            declarations: graph
                .declarations
                .iter()
                .enumerate()
                .map(|(index, declaration)| (declaration.id.clone(), index))
                .collect(),
            members: HashMap::new(),
            parameter_names: HashMap::new(),
        };
        for (declaration_index, declaration) in graph.declarations.iter().enumerate() {
            renderer.index_parameters(&declaration.type_parameters);
            for (member_index, member) in declaration.members.iter().enumerate() {
                renderer
                    .members
                    .insert(member.id(), (declaration_index, member_index));
                if let MemberModel::Defined(member) = member
                    && let Some(signature) = member.kind.signature()
                {
                    renderer.index_parameters(&signature.type_parameters);
                }
            }
        }
        renderer
    }

    /// Resolves a node without discarding unknown records.
    ///
    /// # Errors
    /// Reports the exact missing node identity.
    pub fn node(&self, id: &TypeNodeId) -> Result<&'a TypeNodeModel> {
        self.nodes
            .get(id)
            .map(|index| &self.graph.nodes[*index])
            .ok_or_else(|| {
                TypertGeneratorError::Render(format!("type graph references missing node {id}"))
            })
    }

    /// Resolves a represented node or reports the unsupported record.
    ///
    /// # Errors
    /// Rejects missing edges and unsupported node records.
    pub fn defined_node(&self, id: &TypeNodeId) -> Result<&'a DefinedTypeNode> {
        match self.node(id)? {
            TypeNodeModel::Defined(node) => Ok(node),
            node @ TypeNodeModel::Unsupported(_) => Err(unsupported(node)),
        }
    }

    /// Resolves a declaration by its workspace identity.
    ///
    /// # Errors
    /// Reports the exact missing declaration identity.
    pub fn declaration(&self, id: &SymbolId) -> Result<&'a TypeDeclarationModel> {
        self.declarations
            .get(id)
            .map(|index| &self.graph.declarations[*index])
            .ok_or_else(|| {
                TypertGeneratorError::Render(format!(
                    "type graph references missing declaration {id}"
                ))
            })
    }

    /// Resolves one declaration member.
    ///
    /// # Errors
    /// Reports the exact missing member identity.
    pub fn member(&self, id: &MemberId) -> Result<&'a MemberModel> {
        self.members
            .get(id)
            .map(|(declaration, member)| &self.graph.declarations[*declaration].members[*member])
            .ok_or_else(|| {
                TypertGeneratorError::Render(format!("type graph references missing member {id}"))
            })
    }

    /// Renders the authored type expression with optional declaration substitutions.
    ///
    /// # Errors
    /// Rejects missing edges, unsupported records, and mapped parameters without constraints.
    pub fn render_type(
        &self,
        id: &TypeNodeId,
        references: Option<&ReferenceNames>,
    ) -> Result<String> {
        let node = self.defined_node(id)?;
        Ok(match &node.kind {
            TypeNodeKind::Keyword { name } => name.as_str().to_owned(),
            TypeNodeKind::Literal { text, .. } => text.clone(),
            TypeNodeKind::Parenthesized { ty } => {
                format!("({})", self.render_type(ty, references)?)
            }
            TypeNodeKind::Reference {
                name,
                target,
                arguments,
            } => self.reference_type(name, target, arguments, references)?,
            TypeNodeKind::Union { types } => self.join_types(types, " | ", references)?,
            TypeNodeKind::Intersection { types } => self.join_types(types, " & ", references)?,
            TypeNodeKind::Array { element } => self.array_type(element, references)?,
            TypeNodeKind::Tuple { elements } => self.tuple_type(elements, references)?,
            TypeNodeKind::Object { members } => self.render_object(members, references)?,
            TypeNodeKind::Function { signature } => {
                self.callable_type(signature, None, references)?
            }
            TypeNodeKind::Constructor {
                is_abstract,
                signature,
            } => self.callable_type(signature, Some(*is_abstract), references)?,
            TypeNodeKind::IndexedAccess { object, index } => format!(
                "{}[{}]",
                self.render_type(object, references)?,
                self.render_type(index, references)?
            ),
            TypeNodeKind::Operator { operator, ty } => format!(
                "{} {}",
                operator.as_str(),
                self.render_type(ty, references)?
            ),
            TypeNodeKind::Conditional {
                check,
                extends,
                when_true,
                when_false,
            } => format!(
                "{} extends {} ? {} : {}",
                self.render_type(check, references)?,
                self.render_type(extends, references)?,
                self.render_type(when_true, references)?,
                self.render_type(when_false, references)?
            ),
            TypeNodeKind::Infer { parameter } => format!(
                "infer {}",
                self.type_parameter(parameter, false, references)?
            ),
            TypeNodeKind::Mapped {
                parameter,
                name_type,
                value,
                read_only,
                optional,
            } => self.mapped_type(
                parameter,
                name_type.as_ref(),
                value.as_ref(),
                (*read_only, *optional),
                references,
            )?,
            TypeNodeKind::TemplateLiteral { head, spans } => {
                self.template_type(head, spans, references)?
            }
            TypeNodeKind::TypeQuery {
                expression,
                arguments,
            } => format!(
                "typeof {expression}{}",
                self.type_arguments(arguments, references)?
            ),
            TypeNodeKind::ImportType {
                module,
                qualifier,
                arguments,
                is_typeof,
                attributes,
                ..
            } => self.import_type(
                module,
                qualifier.as_deref(),
                arguments,
                *is_typeof,
                attributes.as_deref(),
                references,
            )?,
            TypeNodeKind::Predicate {
                asserts,
                parameter,
                ty,
            } => self.predicate_type(*asserts, parameter, ty.as_ref(), references)?,
            TypeNodeKind::This => "this".to_owned(),
        })
    }

    /// Renders a callable signature without a member name.
    ///
    /// # Errors
    /// Propagates graph traversal and rendering failures.
    pub fn render_signature(
        &self,
        signature: &SignatureModel,
        references: Option<&ReferenceNames>,
    ) -> Result<String> {
        Ok(format!(
            "{}: {}",
            self.signature_head(signature, references)?,
            self.render_type(&signature.returns, references)?
        ))
    }

    /// Renders one body-free member, or returns its exact retained source text.
    ///
    /// # Errors
    /// Rejects unsupported members and broken type edges.
    pub fn render_member(
        &self,
        member: &MemberModel,
        source_modifiers: bool,
        references: Option<&ReferenceNames>,
    ) -> Result<String> {
        if source_modifiers {
            return match member {
                MemberModel::Defined(member) => Ok(member.base.text.clone()),
                MemberModel::Unsupported(value) => value
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| unsupported(member)),
            };
        }
        let MemberModel::Defined(member) = member else {
            return Err(unsupported(member));
        };
        let name = render_property_name(&member.base.name);
        let optional = if member.base.optional { "?" } else { "" };
        let readonly = if member.base.read_only {
            "readonly "
        } else {
            ""
        };
        let abstract_ = if member.base.is_abstract {
            "abstract "
        } else {
            ""
        };
        Ok(match &member.kind {
            MemberKind::Property { ty } => format!(
                "{abstract_}{readonly}{name}{optional}: {}",
                self.render_type(ty, references)?
            ),
            MemberKind::Method { signature } => format!(
                "{abstract_}{name}{optional}{}",
                self.render_signature(signature, references)?
            ),
            MemberKind::Getter { signature } => format!(
                "{abstract_}get {name}(): {}",
                self.render_type(&signature.returns, references)?
            ),
            MemberKind::Setter { signature } => format!(
                "{abstract_}set {name}{}",
                self.signature_head(signature, references)?
            ),
            MemberKind::Call { signature } => self.render_signature(signature, references)?,
            MemberKind::Construct { signature } => {
                format!("new {}", self.render_signature(signature, references)?)
            }
            MemberKind::Index { signature } => format!(
                "{readonly}[{}]: {}",
                self.parameters(&signature.parameters, references)?,
                self.render_type(&signature.returns, references)?
            ),
        })
    }

    /// Renders a named declaration without documentation.
    ///
    /// # Errors
    /// Rejects missing declarations, missing alias types, and broken member edges.
    pub fn render_declaration(&self, id: &SymbolId) -> Result<String> {
        let declaration = self.declaration(id)?;
        let parameters = self.type_parameters(&declaration.type_parameters, None)?;
        if declaration.kind == DeclarationKind::Enum {
            let members = declaration
                .enum_members
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|member| {
                    format!(
                        "    {}{},",
                        render_property_name(&member.name),
                        member
                            .initializer
                            .as_ref()
                            .map_or_else(String::new, |value| format!(" = {value}"))
                    )
                });
            return Ok(
                std::iter::once(format!("export enum {} {{", declaration.name))
                    .chain(members)
                    .chain(std::iter::once("}".to_owned()))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
        if declaration.kind == DeclarationKind::Alias {
            let ty = declaration.ty.as_ref().ok_or_else(|| {
                TypertGeneratorError::Render(format!("alias {id} has no type node"))
            })?;
            return Ok(format!(
                "export type {}{parameters} = {};",
                declaration.name,
                self.render_type(ty, None)?
            ));
        }
        let extends = self.heritage(&declaration.extends, "extends")?;
        let implements = self.heritage(&declaration.implements, "implements")?;
        let prefix = if declaration.kind == DeclarationKind::Class && declaration.is_abstract {
            "abstract "
        } else {
            ""
        };
        let mut lines = vec![format!(
            "export {prefix}{} {}{parameters}{extends}{implements} {{",
            declaration.kind.as_str(),
            declaration.name
        )];
        for member in &declaration.members {
            lines.push(format!("    {};", self.render_member(member, false, None)?));
        }
        lines.push("}".to_owned());
        Ok(lines.join("\n"))
    }

    /// Finds transitive member dependencies, retaining original graph order.
    ///
    /// # Errors
    /// Rejects broken graph edges or unsupported records.
    pub fn declaration_closure_for_members(
        &self,
        members: &[MemberId],
    ) -> Result<Vec<&'a TypeDeclarationModel>> {
        self.declaration_closure(members, &[])
    }

    /// Finds transitive type dependencies, retaining original graph order.
    ///
    /// # Errors
    /// Rejects broken graph edges or unsupported records.
    pub fn declaration_closure_for_types(
        &self,
        types: &[TypeNodeId],
    ) -> Result<Vec<&'a TypeDeclarationModel>> {
        self.declaration_closure(&[], types)
    }

    fn declaration_closure(
        &self,
        members: &[MemberId],
        types: &[TypeNodeId],
    ) -> Result<Vec<&'a TypeDeclarationModel>> {
        let mut closure = DeclarationClosure {
            renderer: self,
            found: HashSet::new(),
            visiting: HashSet::new(),
        };
        for member in members {
            closure.member(self.member(member)?)?;
        }
        for ty in types {
            closure.node(ty)?;
        }
        Ok(self
            .graph
            .declarations
            .iter()
            .filter(|declaration| closure.found.contains(&declaration.id))
            .collect())
    }

    fn callable_type(
        &self,
        signature: &SignatureModel,
        constructor: Option<bool>,
        references: Option<&ReferenceNames>,
    ) -> Result<String> {
        let prefix = match constructor {
            None => "",
            Some(false) => "new ",
            Some(true) => "abstract new ",
        };
        Ok(format!(
            "{prefix}{} => {}",
            self.signature_head(signature, references)?,
            self.render_type(&signature.returns, references)?
        ))
    }

    fn predicate_type(
        &self,
        asserts: bool,
        parameter: &str,
        ty: Option<&TypeNodeId>,
        references: Option<&ReferenceNames>,
    ) -> Result<String> {
        let suffix = ty
            .map(|ty| {
                self.render_type(ty, references)
                    .map(|ty| format!(" is {ty}"))
            })
            .transpose()?
            .unwrap_or_default();
        Ok(format!(
            "{}{parameter}{suffix}",
            if asserts { "asserts " } else { "" }
        ))
    }

    fn template_type(
        &self,
        head: &str,
        spans: &[TemplateSpanModel],
        references: Option<&ReferenceNames>,
    ) -> Result<String> {
        let mut rendered = format!("`{}", escape_template(head));
        for span in spans {
            write!(
                &mut rendered,
                "${{{}}}{}",
                self.render_type(&span.ty, references)?,
                escape_template(&span.text)
            )
            .expect("writing to a string cannot fail");
        }
        rendered.push('`');
        Ok(rendered)
    }

    fn import_type(
        &self,
        module: &str,
        qualifier: Option<&str>,
        arguments: &[TypeNodeId],
        is_typeof: bool,
        attributes: Option<&str>,
        references: Option<&ReferenceNames>,
    ) -> Result<String> {
        Ok(format!(
            "{}import({}{}){}{}",
            if is_typeof { "typeof " } else { "" },
            quote(module),
            attributes.map_or_else(String::new, |value| format!(", {value}")),
            qualifier.map_or_else(String::new, |value| format!(".{value}")),
            self.type_arguments(arguments, references)?
        ))
    }

    fn reference_type(
        &self,
        name: &str,
        target: &TypeTargetModel,
        arguments: &[TypeNodeId],
        references: Option<&ReferenceNames>,
    ) -> Result<String> {
        let name = match target {
            TypeTargetModel::TypeParameter { parameter } => self
                .parameter_names
                .get(parameter)
                .map_or(name, String::as_str),
            TypeTargetModel::Declaration { symbol } => references
                .and_then(|names| names.get(symbol))
                .map_or(name, String::as_str),
            TypeTargetModel::CrossFace { .. }
            | TypeTargetModel::External { .. }
            | TypeTargetModel::Standard { .. } => name,
        };
        Ok(format!(
            "{name}{}",
            self.type_arguments(arguments, references)?
        ))
    }

    fn array_type(
        &self,
        element: &TypeNodeId,
        references: Option<&ReferenceNames>,
    ) -> Result<String> {
        let rendered = self.render_type(element, references)?;
        if matches!(
            self.defined_node(element)?.kind,
            TypeNodeKind::Union { .. }
                | TypeNodeKind::Intersection { .. }
                | TypeNodeKind::Function { .. }
                | TypeNodeKind::Constructor { .. }
                | TypeNodeKind::Conditional { .. }
        ) {
            Ok(format!("({rendered})[]"))
        } else {
            Ok(format!("{rendered}[]"))
        }
    }

    fn tuple_type(
        &self,
        elements: &[TupleElementModel],
        references: Option<&ReferenceNames>,
    ) -> Result<String> {
        let elements = elements
            .iter()
            .map(|element| {
                let ty = self.render_type(&element.ty, references)?;
                let rest = if element.rest { "..." } else { "" };
                let optional = if element.optional { "?" } else { "" };
                Ok(element.name.as_ref().map_or_else(
                    || format!("{rest}{ty}{optional}"),
                    |name| format!("{rest}{name}{optional}: {ty}"),
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(format!("[{}]", elements.join(", ")))
    }

    fn mapped_type(
        &self,
        parameter: &TypeParameterModel,
        name_type: Option<&TypeNodeId>,
        value: Option<&TypeNodeId>,
        modifiers: (MappedModifier, MappedModifier),
        references: Option<&ReferenceNames>,
    ) -> Result<String> {
        let constraint = parameter.constraint.as_ref().ok_or_else(|| {
            TypertGeneratorError::Render(format!(
                "mapped type parameter {} has no constraint",
                parameter.name
            ))
        })?;
        let constraint = self.render_type(constraint, references)?;
        let readonly = match modifiers.0 {
            MappedModifier::Preserve => "",
            MappedModifier::Remove => "-readonly ",
            MappedModifier::Add => "readonly ",
        };
        let optional = match modifiers.1 {
            MappedModifier::Preserve => "",
            MappedModifier::Remove => "-?",
            MappedModifier::Add => "?",
        };
        let name = name_type
            .map(|id| {
                self.render_type(id, references)
                    .map(|ty| format!(" as {ty}"))
            })
            .transpose()?
            .unwrap_or_default();
        let value = value
            .map(|id| self.render_type(id, references))
            .transpose()?
            .unwrap_or_else(|| "unknown".to_owned());
        Ok(format!(
            "{{ {readonly}[{} in {constraint}{name}]{optional}: {value} }}",
            parameter.name
        ))
    }

    fn join_types(
        &self,
        types: &[TypeNodeId],
        separator: &str,
        references: Option<&ReferenceNames>,
    ) -> Result<String> {
        Ok(types
            .iter()
            .map(|ty| self.render_type(ty, references))
            .collect::<Result<Vec<_>>>()?
            .join(separator))
    }

    fn type_arguments(
        &self,
        arguments: &[TypeNodeId],
        references: Option<&ReferenceNames>,
    ) -> Result<String> {
        if arguments.is_empty() {
            Ok(String::new())
        } else {
            Ok(format!(
                "<{}>",
                self.join_types(arguments, ", ", references)?
            ))
        }
    }

    fn heritage(&self, types: &[TypeNodeId], keyword: &str) -> Result<String> {
        if types.is_empty() {
            Ok(String::new())
        } else {
            Ok(format!(
                " {keyword} {}",
                self.join_types(types, ", ", None)?
            ))
        }
    }

    fn signature_head(
        &self,
        signature: &SignatureModel,
        references: Option<&ReferenceNames>,
    ) -> Result<String> {
        Ok(format!(
            "{}({})",
            self.type_parameters(&signature.type_parameters, references)?,
            self.parameters(&signature.parameters, references)?
        ))
    }

    fn parameters(
        &self,
        parameters: &[ParameterModel],
        references: Option<&ReferenceNames>,
    ) -> Result<String> {
        parameters
            .iter()
            .map(|parameter| {
                let name = if parameter.binding == ParameterBinding::Identifier {
                    render_property_name(&parameter.name)
                } else {
                    parameter.name.clone()
                };
                let optional =
                    if parameter.initializer.is_none() && parameter.optional && !parameter.rest {
                        "?"
                    } else {
                        ""
                    };
                let rest = if parameter.rest { "..." } else { "" };
                let initializer = parameter
                    .initializer
                    .as_ref()
                    .map_or_else(String::new, |value| format!(" = {value}"));
                Ok(format!(
                    "{rest}{name}{optional}: {}{initializer}",
                    self.render_type(&parameter.ty, references)?
                ))
            })
            .collect::<Result<Vec<_>>>()
            .map(|parameters| parameters.join(", "))
    }

    fn type_parameters(
        &self,
        parameters: &[TypeParameterModel],
        references: Option<&ReferenceNames>,
    ) -> Result<String> {
        if parameters.is_empty() {
            return Ok(String::new());
        }
        Ok(format!(
            "<{}>",
            parameters
                .iter()
                .map(|parameter| self.type_parameter(parameter, true, references))
                .collect::<Result<Vec<_>>>()?
                .join(", ")
        ))
    }

    fn type_parameter(
        &self,
        parameter: &TypeParameterModel,
        include_default: bool,
        references: Option<&ReferenceNames>,
    ) -> Result<String> {
        let variance = match parameter.variance {
            None => "",
            Some(Variance::In) => "in ",
            Some(Variance::Out) => "out ",
            Some(Variance::InOut) => "in out ",
        };
        let const_ = if parameter.is_const { "const " } else { "" };
        let constraint = parameter
            .constraint
            .as_ref()
            .map(|id| {
                self.render_type(id, references)
                    .map(|ty| format!(" extends {ty}"))
            })
            .transpose()?
            .unwrap_or_default();
        let fallback = if include_default {
            parameter
                .default
                .as_ref()
                .map(|id| {
                    self.render_type(id, references)
                        .map(|ty| format!(" = {ty}"))
                })
                .transpose()?
                .unwrap_or_default()
        } else {
            String::new()
        };
        Ok(format!(
            "{const_}{variance}{}{constraint}{fallback}",
            parameter.name
        ))
    }

    fn render_object(
        &self,
        members: &[MemberModel],
        references: Option<&ReferenceNames>,
    ) -> Result<String> {
        if members.is_empty() {
            return Ok("{}".to_owned());
        }
        Ok(format!(
            "{{ {} }}",
            members
                .iter()
                .map(|member| self
                    .render_member(member, false, references)
                    .map(|member| format!("{member};")))
                .collect::<Result<Vec<_>>>()?
                .join(" ")
        ))
    }

    fn index_parameters(&mut self, parameters: &[TypeParameterModel]) {
        for parameter in parameters {
            self.parameter_names
                .insert(parameter.id.clone(), parameter.name.clone());
        }
    }
}

struct DeclarationClosure<'r, 'g> {
    renderer: &'r TypeGraphRenderer<'g>,
    found: HashSet<SymbolId>,
    visiting: HashSet<SymbolId>,
}

impl DeclarationClosure<'_, '_> {
    fn node(&mut self, id: &TypeNodeId) -> Result<()> {
        let node = self.renderer.node(id)?;
        if let TypeNodeModel::Defined(node) = node {
            match &node.kind {
                TypeNodeKind::Reference {
                    target: TypeTargetModel::Declaration { symbol },
                    ..
                }
                | TypeNodeKind::ImportType {
                    target: Some(TypeTargetModel::Declaration { symbol }),
                    ..
                } => self.declaration(symbol)?,
                _ => {}
            }
        }
        for child in child_type_node_ids(node)? {
            self.node(&child)?;
        }
        if let TypeNodeModel::Defined(node) = node {
            match &node.kind {
                TypeNodeKind::Function { signature }
                | TypeNodeKind::Constructor { signature, .. } => self.signature(signature)?,
                TypeNodeKind::Object { members } => {
                    for member in members {
                        self.member(member)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn parameters(&mut self, parameters: &[TypeParameterModel]) -> Result<()> {
        for parameter in parameters {
            if let Some(constraint) = &parameter.constraint {
                self.node(constraint)?;
            }
            if let Some(default) = &parameter.default {
                self.node(default)?;
            }
        }
        Ok(())
    }

    fn signature(&mut self, signature: &SignatureModel) -> Result<()> {
        self.parameters(&signature.type_parameters)?;
        for parameter in &signature.parameters {
            self.node(&parameter.ty)?;
        }
        self.node(&signature.returns)
    }

    fn member(&mut self, member: &MemberModel) -> Result<()> {
        let MemberModel::Defined(member) = member else {
            return Err(unsupported(member));
        };
        match &member.kind {
            MemberKind::Property { ty } => self.node(ty),
            MemberKind::Method { signature }
            | MemberKind::Getter { signature }
            | MemberKind::Setter { signature }
            | MemberKind::Call { signature }
            | MemberKind::Construct { signature }
            | MemberKind::Index { signature } => self.signature(signature),
        }
    }

    fn declaration(&mut self, id: &SymbolId) -> Result<()> {
        if self.found.contains(id) || !self.visiting.insert(id.clone()) {
            return Ok(());
        }
        let declaration = self.renderer.declaration(id)?;
        self.parameters(&declaration.type_parameters)?;
        for ty in declaration.extends.iter().chain(&declaration.implements) {
            self.node(ty)?;
        }
        if let Some(ty) = &declaration.ty {
            self.node(ty)?;
        }
        for member in &declaration.members {
            self.member(member)?;
        }
        self.visiting.remove(id);
        self.found.insert(id.clone());
        Ok(())
    }
}

fn unsupported(value: &impl serde::Serialize) -> TypertGeneratorError {
    TypertGeneratorError::Render(format!(
        "unsupported model variant {}",
        serde_json::to_string(value).expect("model records serialize")
    ))
}

fn render_property_name(name: &str) -> String {
    let identifier = name.as_bytes().split_first().is_some_and(|(first, rest)| {
        (first.is_ascii_alphabetic() || *first == b'_' || *first == b'$')
            && rest
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$'))
    });
    let numeric = !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_digit());
    if name.starts_with('[') && name.ends_with(']') || identifier || numeric {
        name.to_owned()
    } else {
        quote(name)
    }
}

fn quote(value: &str) -> String {
    format!(
        "'{}'",
        value
            .replace('\\', "\\\\")
            .replace('\'', "\\'")
            .replace('\n', "\\n")
    )
}
fn escape_template(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('`', "\\`")
        .replace("${", "\\${")
}
