//! Zod declarations projected solely from the authored type graph.

use std::{
    collections::{HashMap, HashSet},
    fmt::Write as _,
};

use crate::{
    Result, TypertGeneratorError,
    model::{
        ComputedMember, DeclarationKind, DocumentationModel, InvocationModel, InvocationTarget,
        KeywordTypeName, MemberKind, MemberModel, MemberVisibility, SchemaModel, SymbolId,
        TypeDeclarationModel, TypeNodeId, TypeNodeKind, TypeParameterId, TypeTargetModel,
    },
    renderer::TypeGraphRenderer,
    text::{quote, safe_identifier},
};

pub(crate) struct BoundaryRoot {
    pub(crate) key: String,
    pub(crate) ty: TypeNodeId,
}

pub(crate) struct SchemaExport<'a> {
    pub(crate) model: &'a SchemaModel,
    pub(crate) export_name: String,
    pub(crate) internal_name: String,
}

pub(crate) struct SchemaArtifact<'a> {
    pub(crate) definitions: Vec<String>,
    pub(crate) exports: Vec<SchemaExport<'a>>,
    boundary_names: HashMap<String, String>,
}

impl SchemaArtifact<'_> {
    pub(crate) fn boundary(&self, key: &str) -> Result<&str> {
        self.boundary_names
            .get(key)
            .map(String::as_str)
            .ok_or_else(|| {
                failure(
                    key,
                    "invocation boundary is outside the selected schema roots",
                )
            })
    }
}

pub(crate) struct SchemaEmitter<'r, 'g> {
    renderer: &'r TypeGraphRenderer<'g>,
    schemas: &'g [SchemaModel],
    boundaries: Vec<BoundaryRoot>,
    declarations: Vec<&'g TypeDeclarationModel>,
    names: HashMap<SymbolId, String>,
    boundary_names: HashMap<String, String>,
}

type Substitutions = HashMap<TypeParameterId, String>;

impl<'r, 'g> SchemaEmitter<'r, 'g> {
    pub(crate) fn new(
        renderer: &'r TypeGraphRenderer<'g>,
        schemas: &'g [SchemaModel],
        boundaries: Vec<BoundaryRoot>,
    ) -> Result<Self> {
        let mut selected = HashSet::new();
        for root in schemas
            .iter()
            .map(|schema| &schema.ty)
            .chain(boundaries.iter().map(|boundary| &boundary.ty))
        {
            for declaration in renderer.declaration_closure_for_types(std::slice::from_ref(root))? {
                selected.insert(declaration.id.clone());
            }
        }
        let declarations = renderer
            .graph
            .declarations
            .iter()
            .filter(|declaration| selected.contains(&declaration.id))
            .collect::<Vec<_>>();
        let mut identifiers = HashSet::new();
        let mut names = HashMap::new();
        for declaration in &declarations {
            names.insert(
                declaration.id.clone(),
                allocate(
                    &format!("{}$schema", safe_identifier(&declaration.name)),
                    &mut identifiers,
                ),
            );
        }
        let mut boundary_names = HashMap::new();
        for boundary in &boundaries {
            boundary_names.insert(
                boundary.key.clone(),
                allocate(
                    &format!("{}$schema", safe_identifier(&boundary.key)),
                    &mut identifiers,
                ),
            );
        }
        Ok(Self {
            renderer,
            schemas,
            boundaries,
            declarations,
            names,
            boundary_names,
        })
    }

    pub(crate) fn emit(&self) -> Result<SchemaArtifact<'g>> {
        let mut definitions = self
            .declarations
            .iter()
            .map(|declaration| self.declaration_definition(declaration))
            .collect::<Result<Vec<_>>>()?;
        for boundary in &self.boundaries {
            definitions.push(format!(
                "const {} = {}",
                self.boundary_name(&boundary.key)?,
                self.type_schema(&boundary.ty, &Substitutions::new())?
            ));
        }
        let exports = self
            .schemas
            .iter()
            .map(|model| {
                let internal_name = self.schema_name(&model.symbol)?.to_owned();
                if !self
                    .renderer
                    .declaration(&model.symbol)?
                    .type_parameters
                    .is_empty()
                {
                    return Err(failure(
                        &model.export.name,
                        "generic schema exports require a concrete declaration",
                    ));
                }
                Ok(SchemaExport {
                    model,
                    export_name: safe_identifier(&model.export.name),
                    internal_name,
                })
            })
            .collect::<Result<_>>()?;
        Ok(SchemaArtifact {
            definitions,
            exports,
            boundary_names: self.boundary_names.clone(),
        })
    }

    fn declaration_definition(&self, declaration: &TypeDeclarationModel) -> Result<String> {
        let name = self.schema_name(&declaration.id)?;
        if declaration.type_parameters.is_empty() {
            return Ok(format!(
                "const {name} = {}",
                self.declaration_schema(declaration, &Substitutions::new())?
            ));
        }
        let parameters = declaration
            .type_parameters
            .iter()
            .enumerate()
            .map(|(index, parameter)| (parameter.id.clone(), format!("type{index}$schema")))
            .collect::<Vec<_>>();
        let names = parameters
            .iter()
            .map(|(_, name)| name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let substitutions = parameters.into_iter().collect();
        Ok(format!(
            "const {name} = ({names}) => {}",
            self.declaration_schema(declaration, &substitutions)?
        ))
    }

    fn declaration_schema(
        &self,
        declaration: &TypeDeclarationModel,
        substitutions: &Substitutions,
    ) -> Result<String> {
        match declaration.kind {
            DeclarationKind::Enum => Err(failure(
                &declaration.name,
                "enum declarations have no Zod projection",
            )),
            DeclarationKind::Alias => {
                let ty = declaration
                    .ty
                    .as_ref()
                    .ok_or_else(|| failure(&declaration.name, "alias has no modeled type"))?;
                Ok(describe(
                    self.type_schema(ty, substitutions)?,
                    &declaration.documentation,
                ))
            }
            DeclarationKind::Interface | DeclarationKind::Class => {
                let mut result =
                    self.object_schema(&declaration.members, &declaration.name, substitutions)?;
                for heritage in &declaration.extends {
                    result = format!(
                        "z.intersection({}, {result})",
                        self.type_schema(heritage, substitutions)?
                    );
                }
                Ok(describe(result, &declaration.documentation))
            }
        }
    }

    fn type_schema(&self, id: &TypeNodeId, substitutions: &Substitutions) -> Result<String> {
        let node = self.renderer.defined_node(id)?;
        Ok(match &node.kind {
            TypeNodeKind::Keyword { name } => keyword_schema(name)?,
            TypeNodeKind::Literal { text, .. } => format!("z.literal({text})"),
            TypeNodeKind::Parenthesized { ty } => self.type_schema(ty, substitutions)?,
            TypeNodeKind::Reference {
                name,
                target,
                arguments,
            } => self.reference_schema(name, target, arguments, substitutions)?,
            TypeNodeKind::Union { types } => match types.as_slice() {
                [] => "z.never()".to_owned(),
                [only] => self.type_schema(only, substitutions)?,
                _ => format!(
                    "z.union([{}])",
                    types
                        .iter()
                        .map(|ty| self.type_schema(ty, substitutions))
                        .collect::<Result<Vec<_>>>()?
                        .join(", ")
                ),
            },
            TypeNodeKind::Intersection { types } => {
                if let Some((head, tail)) = types.split_first() {
                    let mut result = self.type_schema(head, substitutions)?;
                    for ty in tail {
                        result = format!(
                            "z.intersection({result}, {})",
                            self.type_schema(ty, substitutions)?
                        );
                    }
                    result
                } else {
                    "z.unknown()".to_owned()
                }
            }
            TypeNodeKind::Array { element } => {
                format!("z.array({})", self.type_schema(element, substitutions)?)
            }
            TypeNodeKind::Tuple { elements } => {
                let fixed = elements
                    .iter()
                    .filter(|element| !element.rest)
                    .map(|element| {
                        self.type_schema(&element.ty, substitutions)
                            .map(|schema| optional(schema, element.optional))
                    })
                    .collect::<Result<Vec<_>>>()?;
                let mut schema = format!("z.tuple([{}])", fixed.join(", "));
                if let Some(rest) = elements.iter().find(|element| element.rest) {
                    write!(
                        &mut schema,
                        ".rest({})",
                        self.tuple_rest_schema(&rest.ty, substitutions)?
                    )
                    .expect("writing to a string cannot fail");
                }
                schema
            }
            TypeNodeKind::Object { members } => {
                self.object_schema(members, id.as_str(), substitutions)?
            }
            kind @ (TypeNodeKind::Operator { .. }
            | TypeNodeKind::IndexedAccess { .. }
            | TypeNodeKind::Conditional { .. }
            | TypeNodeKind::Infer { .. }
            | TypeNodeKind::Mapped { .. }
            | TypeNodeKind::TemplateLiteral { .. }
            | TypeNodeKind::TypeQuery { .. }
            | TypeNodeKind::ImportType { .. }
            | TypeNodeKind::Predicate { .. }
            | TypeNodeKind::Function { .. }
            | TypeNodeKind::Constructor { .. }
            | TypeNodeKind::This) => {
                return Err(failure(
                    id.as_str(),
                    &format!("type node {} has no Zod projection", kind_name(kind)),
                ));
            }
        })
    }

    fn reference_schema(
        &self,
        name: &str,
        target: &TypeTargetModel,
        arguments: &[TypeNodeId],
        substitutions: &Substitutions,
    ) -> Result<String> {
        match target {
            TypeTargetModel::Declaration { symbol } => {
                let schema = self.schema_name(symbol)?;
                let declaration = self.renderer.declaration(symbol)?;
                if declaration.type_parameters.is_empty() {
                    if !arguments.is_empty() {
                        return Err(failure(
                            name,
                            &format!(
                                "non-generic declaration received {} type arguments",
                                arguments.len()
                            ),
                        ));
                    }
                    return Ok(format!("z.lazy(() => {schema})"));
                }
                let arguments =
                    self.declaration_arguments(name, arguments, declaration, substitutions)?;
                Ok(format!("z.lazy(() => {schema}({}))", arguments.join(", ")))
            }
            TypeTargetModel::TypeParameter { parameter } => {
                if !arguments.is_empty() {
                    return Err(failure(
                        name,
                        "type parameter reference cannot receive type arguments",
                    ));
                }
                substitutions
                    .get(parameter)
                    .cloned()
                    .ok_or_else(|| failure(name, "type parameter has no schema substitution"))
            }
            TypeTargetModel::Standard { name: standard } => match standard.as_str() {
                "Array" | "ReadonlyArray" => {
                    let element = arguments
                        .first()
                        .ok_or_else(|| failure(name, "array reference has no element type"))?;
                    Ok(readonly(
                        format!("z.array({})", self.type_schema(element, substitutions)?),
                        standard == "ReadonlyArray",
                    ))
                }
                "Record" => {
                    let key = arguments
                        .first()
                        .ok_or_else(|| failure(name, "Record requires key and value types"))?;
                    let value = arguments
                        .get(1)
                        .ok_or_else(|| failure(name, "Record requires key and value types"))?;
                    Ok(format!(
                        "z.record({}, {})",
                        self.type_schema(key, substitutions)?,
                        self.type_schema(value, substitutions)?
                    ))
                }
                "Date" => Ok("z.date()".to_owned()),
                _ => Err(failure(
                    name,
                    &format!("standard type {standard} has no Zod projection"),
                )),
            },
            TypeTargetModel::CrossFace { .. } => {
                Err(failure(name, "cross-face reference has no Zod projection"))
            }
            TypeTargetModel::External { .. } => {
                Err(failure(name, "external reference has no Zod projection"))
            }
        }
    }

    fn declaration_arguments(
        &self,
        name: &str,
        arguments: &[TypeNodeId],
        declaration: &TypeDeclarationModel,
        substitutions: &Substitutions,
    ) -> Result<Vec<String>> {
        if arguments.len() > declaration.type_parameters.len() {
            return Err(failure(
                name,
                &format!(
                    "generic declaration accepts {} type arguments but received {}",
                    declaration.type_parameters.len(),
                    arguments.len()
                ),
            ));
        }
        let mut resolved = substitutions.clone();
        let mut result = Vec::new();
        for (index, parameter) in declaration.type_parameters.iter().enumerate() {
            let schema = if let Some(argument) = arguments.get(index) {
                self.type_schema(argument, substitutions)?
            } else {
                let default = parameter.default.as_ref().ok_or_else(|| {
                    failure(name, &format!("missing type argument {}", parameter.name))
                })?;
                self.type_schema(default, &resolved)?
            };
            resolved.insert(parameter.id.clone(), schema.clone());
            result.push(schema);
        }
        Ok(result)
    }

    fn tuple_rest_schema(&self, id: &TypeNodeId, substitutions: &Substitutions) -> Result<String> {
        match &self.renderer.defined_node(id)?.kind {
            TypeNodeKind::Array { element } => self.type_schema(element, substitutions),
            TypeNodeKind::Reference {
                name,
                target: TypeTargetModel::Standard { name: standard },
                arguments,
            } if matches!(standard.as_str(), "Array" | "ReadonlyArray") => {
                let element = arguments
                    .first()
                    .ok_or_else(|| failure(name, "tuple rest array has no element type"))?;
                self.type_schema(element, substitutions)
            }
            _ => Err(failure(
                id.as_str(),
                "tuple rest element must retain an array type",
            )),
        }
    }

    fn object_schema(
        &self,
        members: &[MemberModel],
        subject: &str,
        substitutions: &Substitutions,
    ) -> Result<String> {
        let mut properties = Vec::new();
        let mut indices = Vec::new();
        let mut symbols = 0;
        for member in members {
            let MemberModel::Defined(member) = member else {
                return Err(failure(subject, "unsupported model variant"));
            };
            let base = &member.base;
            if base.is_static || base.visibility != MemberVisibility::Public {
                continue;
            }
            if base.computed == Some(ComputedMember::Symbol) {
                symbols += 1;
                continue;
            }
            if base.computed == Some(ComputedMember::Dynamic) {
                return Err(failure(
                    subject,
                    &format!(
                        "computed member {} has no fixed JSON property name",
                        base.name
                    ),
                ));
            }
            if let MemberKind::Index { signature } = &member.kind {
                let [parameter] = signature.parameters.as_slice() else {
                    return Err(failure(
                        subject,
                        "index signature must have exactly one key parameter",
                    ));
                };
                indices.push(readonly(
                    format!(
                        "z.record({}, {})",
                        self.type_schema(&parameter.ty, substitutions)?,
                        self.type_schema(&signature.returns, substitutions)?
                    ),
                    base.read_only,
                ));
                continue;
            }
            let MemberKind::Property { ty } = &member.kind else {
                return Err(failure(
                    subject,
                    &format!(
                        "{} member {} is not data-schema projectable",
                        member.kind.as_str(),
                        base.name
                    ),
                ));
            };
            let schema = describe(
                optional(
                    readonly(self.type_schema(ty, substitutions)?, base.read_only),
                    base.optional,
                ),
                &base.documentation,
            );
            properties.push(format!(
                "{}: {schema}",
                quote(base.json_name.as_ref().unwrap_or(&base.name))
            ));
        }
        if indices.len() > 1 {
            return Err(failure(
                subject,
                "object type has more than one JSON index signature",
            ));
        }
        if properties.is_empty() && indices.is_empty() && symbols > 0 {
            return Ok("z.unknown()".to_owned());
        }
        let body = if properties.is_empty() {
            String::new()
        } else {
            format!(
                "\n{}\n",
                properties
                    .iter()
                    .map(|property| format!("  {property},"))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        };
        let object = format!("z.object({{{body}}})");
        Ok(match indices.first() {
            None => object,
            Some(index) if properties.is_empty() => index.clone(),
            Some(index) => format!("z.intersection({object}, {index})"),
        })
    }

    fn schema_name(&self, symbol: &SymbolId) -> Result<&str> {
        self.names.get(symbol).map(String::as_str).ok_or_else(|| {
            failure(
                symbol.as_str(),
                "referenced declaration is outside the selected schema closure",
            )
        })
    }

    fn boundary_name(&self, key: &str) -> Result<&str> {
        self.boundary_names
            .get(key)
            .map(String::as_str)
            .ok_or_else(|| {
                failure(
                    key,
                    "invocation boundary is outside the selected schema roots",
                )
            })
    }
}

fn allocate(base: &str, used: &mut HashSet<String>) -> String {
    let mut name = base.to_owned();
    let mut suffix = 2;
    while used.contains(&name) {
        name = format!("{base}{suffix}");
        suffix += 1;
    }
    used.insert(name.clone());
    name
}

fn describe(schema: String, documentation: &DocumentationModel) -> String {
    match &documentation.description {
        None => schema,
        Some(description) => format!("{schema}.describe({})", quote(description)),
    }
}

fn optional(schema: String, enabled: bool) -> String {
    if enabled {
        format!("{schema}.optional()")
    } else {
        schema
    }
}
fn readonly(schema: String, enabled: bool) -> String {
    if enabled {
        format!("{schema}.readonly()")
    } else {
        schema
    }
}

fn keyword_schema(name: &KeywordTypeName) -> Result<String> {
    Ok(match name {
        KeywordTypeName::Any => "z.any()",
        KeywordTypeName::Unknown => "z.unknown()",
        KeywordTypeName::Never => "z.never()",
        KeywordTypeName::String => "z.string()",
        KeywordTypeName::Number => "z.number()",
        KeywordTypeName::Bigint => "z.bigint()",
        KeywordTypeName::Boolean => "z.boolean()",
        KeywordTypeName::Symbol => "z.symbol()",
        KeywordTypeName::Undefined => "z.undefined()",
        KeywordTypeName::Void => "z.void()",
        KeywordTypeName::Object => "z.custom((value) => (typeof value === 'object' && value !== null) || typeof value === 'function')",
        KeywordTypeName::Other(name) => return Err(failure(name, &format!("keyword {name} has no Zod projection"))),
    }.to_owned())
}

fn failure(subject: &str, message: &str) -> TypertGeneratorError {
    TypertGeneratorError::Emit(format!("typert Zod emitter: {subject}: {message}"))
}

fn kind_name(kind: &TypeNodeKind) -> &'static str {
    match kind {
        TypeNodeKind::Keyword { .. } => "keyword",
        TypeNodeKind::Literal { .. } => "literal",
        TypeNodeKind::Parenthesized { .. } => "parenthesized",
        TypeNodeKind::Reference { .. } => "reference",
        TypeNodeKind::Union { .. } => "union",
        TypeNodeKind::Intersection { .. } => "intersection",
        TypeNodeKind::Array { .. } => "array",
        TypeNodeKind::Tuple { .. } => "tuple",
        TypeNodeKind::Object { .. } => "object",
        TypeNodeKind::Function { .. } => "function",
        TypeNodeKind::Constructor { .. } => "constructor",
        TypeNodeKind::IndexedAccess { .. } => "indexed-access",
        TypeNodeKind::Operator { .. } => "operator",
        TypeNodeKind::Conditional { .. } => "conditional",
        TypeNodeKind::Infer { .. } => "infer",
        TypeNodeKind::Mapped { .. } => "mapped",
        TypeNodeKind::TemplateLiteral { .. } => "template-literal",
        TypeNodeKind::TypeQuery { .. } => "type-query",
        TypeNodeKind::ImportType { .. } => "import-type",
        TypeNodeKind::Predicate { .. } => "predicate",
        TypeNodeKind::This => "this",
    }
}

pub(crate) fn invocation_roots(invocations: &[InvocationModel]) -> Vec<BoundaryRoot> {
    let mut roots = Vec::new();
    for invocation in invocations {
        if let InvocationTarget::Context { boundary, .. } = &invocation.invocation {
            roots.push(BoundaryRoot {
                key: context_key(invocation),
                ty: boundary.codec_type.clone(),
            });
        }
        for (index, parameter) in invocation.parameters.iter().enumerate() {
            roots.push(BoundaryRoot {
                key: parameter_key(invocation, index),
                ty: parameter.boundary.codec_type.clone(),
            });
        }
        roots.push(BoundaryRoot {
            key: result_key(invocation),
            ty: invocation.result.codec_type.clone(),
        });
    }
    roots
}

pub(crate) fn context_key(invocation: &InvocationModel) -> String {
    format!("{}:context", invocation.id)
}
pub(crate) fn parameter_key(invocation: &InvocationModel, index: usize) -> String {
    format!("{}:parameter:{index}", invocation.id)
}
pub(crate) fn result_key(invocation: &InvocationModel) -> String {
    format!("{}:result", invocation.id)
}
