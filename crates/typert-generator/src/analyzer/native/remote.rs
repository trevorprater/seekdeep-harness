//! Remote invocation contracts: decorators, gateway bindings, and codec projections.
#![expect(
    clippy::too_many_lines,
    reason = "Each routine mirrors one source analyzer method so differential review stays line-aligned"
)]

use std::collections::HashMap;

use indexmap::IndexMap;

use crate::{
    TypertGeneratorError,
    analyzer::{
        is_remote_segment, is_standard_library_file, package_export_targets, source_path_for_export,
    },
    model::{
        CancellationParameter, DeclarationKind, DefinedMember, DefinedTypeNode, DocumentationModel,
        InvocationCancellation, InvocationId, InvocationModel, InvocationParameterModel,
        InvocationParameterSource, InvocationScope, InvocationTarget, KeywordTypeName,
        LiteralValue, MemberBase, MemberKind, MemberModel, MemberVisibility, PackageModel,
        ParameterBinding, ParameterModel, RemoteBoundaryModel, RemoteTypeImportModel,
        SignatureModel, SymbolId, TupleElementModel, TypeDeclarationModel, TypeNodeId,
        TypeNodeKind, TypeNodeModel, TypeSymbolId, TypeTargetModel,
    },
    text::locale_compare,
};

use super::{
    face::{
        FaceAnalyzer, FaceResult, Interrupt, StaticContextDeclaration, StaticLookupDeclaration,
        TypePurpose,
    },
    js::{self, JsMap, JsSet, Val},
    jstext, paths,
    syntax::PROTOCOL_MODULES,
    workspace::PackageRegistration,
};

#[derive(Clone)]
enum RemoteMarker {
    Direct {
        export_name: Option<String>,
    },
    Context {
        context: String,
        export_name: Option<String>,
    },
}

impl RemoteMarker {
    fn export_name(&self) -> Option<&str> {
        match self {
            Self::Direct { export_name } | Self::Context { export_name, .. } => {
                export_name.as_deref()
            }
        }
    }
}

struct GatewayBinding<'s> {
    service: String,
    namespace: String,
    site: Val<'s>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TopLevelAbsence {
    Reject,
    Undefined,
    UndefinedOrVoid,
}

impl<'s> FaceAnalyzer<'_, 's, '_> {
    pub(crate) fn collect_invocations(
        &mut self,
        registration: &PackageRegistration,
        reachable: &[Val<'s>],
    ) -> FaceResult<Vec<InvocationModel>> {
        let kinds = self.syntax.kinds.clone();
        let mut result = Vec::new();
        for source_file in reachable {
            for statement in js::get_items(self.scope, *source_file, "statements")? {
                if !self
                    .syntax
                    .is(self.scope, statement, kinds.class_declaration)?
                {
                    continue;
                }
                let mut marked = Vec::new();
                for member in js::get_items(self.scope, statement, "members")? {
                    let Some(invocation) = self.remote_marker(member)? else {
                        continue;
                    };
                    if !self
                        .syntax
                        .is(self.scope, member, kinds.method_declaration)?
                    {
                        return Err(self
                            .fail(member, "Remote decorators require a public instance method")?);
                    }
                    marked.push((member, invocation));
                }
                let Some((first, _)) = marked.first() else {
                    continue;
                };
                let Some(binding) = self.gateway_binding(statement)? else {
                    return Err(self.fail(
                        *first,
                        "Remote methods require TypertRemoteService or readonly typertGateway = bindTypertRemote(this, serviceKey)",
                    )?);
                };
                for (method, invocation) in marked {
                    result.push(self.invocation_model(
                        registration,
                        &binding,
                        method,
                        &invocation,
                    )?);
                }
            }
        }
        Ok(result)
    }

    fn invocation_model(
        &mut self,
        registration: &PackageRegistration,
        binding: &GatewayBinding<'s>,
        method: Val<'s>,
        invocation: &RemoteMarker,
    ) -> FaceResult<InvocationModel> {
        let kinds = self.syntax.kinds.clone();
        if self.syntax.visibility_of(self.scope, method)? != MemberVisibility::Public
            || self
                .syntax
                .has_modifier(self.scope, method, kinds.static_keyword)?
        {
            return Err(self.fail(method, "Remote decorators require a public instance method")?);
        }
        if self
            .syntax
            .has_modifier(self.scope, method, kinds.abstract_keyword)?
            || js::get_defined(self.scope, method, "body")?.is_none()
        {
            return Err(self.fail(method, "Remote methods must have a concrete implementation")?);
        }
        let name = js::get(self.scope, method, "name")?;
        if !self.syntax.is(self.scope, name, kinds.identifier)? {
            return Err(self.fail(method, "Remote method names must be identifiers")?);
        }
        if !js::get_items(self.scope, method, "typeParameters")?.is_empty() {
            return Err(self.fail(method, "generic Remote methods are not supported")?);
        }
        let method_name = js::get_string(self.scope, name, "text")?;
        let exported_method = invocation
            .export_name()
            .map_or_else(|| method_name.clone(), str::to_owned);

        let lookups = self.lookup_declarations()?;
        let lookup_by_host = lookups
            .iter()
            .map(|(key, host_symbol, wire_type)| (host_symbol.clone(), (key.clone(), *wire_type)))
            .collect::<HashMap<_, _>>();
        let mut parameters = Vec::new();
        let mut cancellation = None;
        let mut wires = std::collections::HashSet::new();
        let method_parameters = js::get_items(self.scope, method, "parameters")?;
        for (parameter_index, parameter) in method_parameters.iter().enumerate() {
            let parameter = *parameter;
            let parameter_name = js::get(self.scope, parameter, "name")?;
            if !self
                .syntax
                .is(self.scope, parameter_name, kinds.identifier)?
            {
                return Err(self.fail(parameter, "Remote parameters must use identifier bindings")?);
            }
            if js::get_defined(self.scope, parameter, "dotDotDotToken")?.is_some() {
                return Err(self.fail(parameter, "Remote parameters cannot be rest parameters")?);
            }
            if js::get_defined(self.scope, parameter, "initializer")?.is_some() {
                return Err(self.fail(parameter, "Remote parameters cannot have default values")?);
            }
            let parameter_text = js::get_string(self.scope, parameter_name, "text")?;
            if parameter_text == "this" {
                return Err(self.fail(
                    parameter,
                    "Remote methods cannot declare an explicit this parameter",
                )?);
            }
            let optional = js::get_defined(self.scope, parameter, "questionToken")?.is_some();
            let explicit = js::get_defined(self.scope, parameter, "type")?;
            let authored_type = self.required_type(parameter, explicit, TypePurpose::Parameter)?;
            let cancellation_name = parameter_text == "signal";
            let cancellation_type = self.is_global_abort_signal(authored_type)?;
            if cancellation_name || cancellation_type {
                if !cancellation_name || !cancellation_type {
                    return Err(self.fail(
                        parameter,
                        "Remote cancellation must use a parameter named signal with the global AbortSignal type",
                    )?);
                }
                if parameter_index != method_parameters.len() - 1 {
                    return Err(self.fail(
                        parameter,
                        "Remote cancellation signal must be the final parameter",
                    )?);
                }
                cancellation = Some(InvocationCancellation {
                    parameter: CancellationParameter::Signal,
                });
                continue;
            }
            let host_symbol = self.symbol_at_type(authored_type)?;
            let lookup = match host_symbol {
                Some(symbol) => {
                    let id = self.symbol_id(symbol)?;
                    lookup_by_host.get(&id).cloned()
                }
                None => None,
            };
            let modeled = if let Some((lookup_key, wire_type)) = lookup {
                if optional {
                    return Err(self.fail(
                        parameter,
                        &format!("lookup parameter for {lookup_key} cannot be optional"),
                    )?);
                }
                if parameter_text != lookup_key {
                    return Err(self.fail(
                        parameter,
                        &format!(
                            "lookup parameter for {lookup_key} must also be named {lookup_key}"
                        ),
                    )?);
                }
                let boundary = self.remote_boundary(
                    wire_type,
                    &format!(
                        "{}#{}/{exported_method}:{lookup_key}Id",
                        registration.name, binding.namespace
                    ),
                    true,
                    TopLevelAbsence::Reject,
                    false,
                )?;
                InvocationParameterModel {
                    name: parameter_text.clone(),
                    wire: format!("{lookup_key}Id"),
                    source: InvocationParameterSource::Lookup,
                    lookup: Some(lookup_key),
                    optional: None,
                    boundary,
                }
            } else {
                if let Some(symbol) = host_symbol
                    && self.is_workspace_class(symbol)?
                {
                    let symbol_name = js::get_string(self.scope, symbol, "name")?;
                    return Err(self.fail(
                        parameter,
                        &format!("non-JSON class parameter {symbol_name} requires a TypertLookupMap entry"),
                    )?);
                }
                let boundary = self.remote_boundary(
                    authored_type,
                    &format!(
                        "{}#{}/{exported_method}:{parameter_text}",
                        registration.name, binding.namespace
                    ),
                    false,
                    TopLevelAbsence::Undefined,
                    optional,
                )?;
                InvocationParameterModel {
                    name: parameter_text.clone(),
                    wire: parameter_text.clone(),
                    source: InvocationParameterSource::Json,
                    lookup: None,
                    optional: optional.then_some(true),
                    boundary,
                }
            };
            if !wires.insert(modeled.wire.clone()) {
                return Err(self.fail(
                    parameter,
                    &format!("duplicate Remote wire field {}", modeled.wire),
                )?);
            }
            parameters.push(modeled);
        }

        let mut receiver = InvocationTarget::Direct;
        if let RemoteMarker::Context { context, .. } = invocation {
            let contexts = self.context_declarations()?;
            let Some(declaration) = contexts.get(context) else {
                return Err(self.fail(
                    method,
                    &format!("Remote Scope {context} has no TypertContextMap entry"),
                )?);
            };
            let wire = format!("{context}Id");
            if wires.contains(&wire) {
                return Err(self.fail(
                    method,
                    &format!("Remote Scope wire field {wire} conflicts with a method parameter"),
                )?);
            }
            let wire_type = declaration.wire_type;
            let boundary = self.remote_boundary(
                wire_type,
                &format!(
                    "{}#{}/{exported_method}:{wire}",
                    registration.name, binding.namespace
                ),
                true,
                TopLevelAbsence::Reject,
                false,
            )?;
            receiver = InvocationTarget::Context {
                context: context.clone(),
                wire,
                boundary,
            };
        }

        let mut scope = None;
        if matches!(invocation, RemoteMarker::Direct { .. }) {
            let lookup_parameters = parameters
                .iter()
                .filter(|parameter| parameter.source == InvocationParameterSource::Lookup)
                .collect::<Vec<_>>();
            if lookup_parameters.len() == 1 {
                let parameter = lookup_parameters[0].clone();
                if let Some(lookup) = &parameter.lookup {
                    let contexts = self.context_declarations()?;
                    if let Some(context) = contexts.get(lookup) {
                        let context_key = context.key.clone();
                        let wire_type = context.wire_type;
                        let context_boundary = self.remote_boundary(
                            wire_type,
                            &format!(
                                "{}#{}/{exported_method}:scope:{context_key}",
                                registration.name, binding.namespace
                            ),
                            true,
                            TopLevelAbsence::Reject,
                            false,
                        )?;
                        if context_boundary.type_symbol != parameter.boundary.type_symbol {
                            return Err(self.fail(
                                method,
                                &format!(
                                    "Remote scope {context_key} wire type {} does not match lookup wire type {}",
                                    context_boundary.type_symbol, parameter.boundary.type_symbol
                                ),
                            )?);
                        }
                        scope = Some(InvocationScope {
                            context: context_key,
                            wire: parameter.wire.clone(),
                        });
                    }
                }
            }
        }

        let result_type = self.remote_result_type(method)?;
        let result = self.remote_boundary(
            result_type,
            &format!(
                "{}#{}/{exported_method}:result",
                registration.name, binding.namespace
            ),
            false,
            TopLevelAbsence::UndefinedOrVoid,
            false,
        )?;
        let location = self.location(name)?;
        Ok(InvocationModel {
            id: InvocationId::from(format!(
                "{}#{}/{exported_method}",
                registration.name, binding.namespace
            )),
            service: binding.service.clone(),
            namespace: binding.namespace.clone(),
            method: exported_method.clone(),
            implementation: (exported_method != method_name).then_some(method_name),
            invocation: receiver,
            scope,
            parameters,
            cancellation,
            result,
            location,
        })
    }

    fn gateway_binding(&mut self, declaration: Val<'s>) -> FaceResult<Option<GatewayBinding<'s>>> {
        let field = self.gateway_field_binding(declaration)?;
        let base = self.gateway_service_binding(declaration)?;
        if let (Some(field), Some(_)) = (&field, &base) {
            let site = field.site;
            return Err(self.fail(
                site,
                "TypertRemoteService subclasses must not declare a second typertRemote binding",
            )?);
        }
        Ok(field.or(base))
    }

    fn gateway_field_binding(
        &mut self,
        declaration: Val<'s>,
    ) -> FaceResult<Option<GatewayBinding<'s>>> {
        let kinds = self.syntax.kinds.clone();
        let mut candidates = Vec::new();
        for member in js::get_items(self.scope, declaration, "members")? {
            if self
                .syntax
                .is(self.scope, member, kinds.property_declaration)?
            {
                let name = js::get(self.scope, member, "name")?;
                if self.syntax.member_name(self.scope, name)? == "typertRemote" {
                    candidates.push(member);
                }
            }
        }
        let Some(property) = candidates.first().copied() else {
            return Ok(None);
        };
        if let Some(duplicate) = candidates.get(1).copied() {
            return Err(self.fail(duplicate, "Service has more than one typertGateway field")?);
        }
        if self.syntax.visibility_of(self.scope, property)? != MemberVisibility::Public
            || self
                .syntax
                .has_modifier(self.scope, property, kinds.static_keyword)?
            || !self
                .syntax
                .has_modifier(self.scope, property, kinds.readonly_keyword)?
        {
            return Err(self.fail(
                property,
                "typertGateway must be a public readonly instance field",
            )?);
        }
        let initializer = js::get_defined(self.scope, property, "initializer")?;
        let call = match initializer {
            Some(initializer)
                if self
                    .syntax
                    .is(self.scope, initializer, kinds.call_expression)? =>
            {
                let expression = js::get(self.scope, initializer, "expression")?;
                if !self.is_type_meta_symbol(expression, "bindTypertRemote")? {
                    return Err(self.fail(property, "typertGateway must call bindTypertRemote()")?);
                }
                initializer
            }
            _ => return Err(self.fail(property, "typertGateway must call bindTypertRemote()")?),
        };
        let arguments = js::get_items(self.scope, call, "arguments")?;
        if arguments.len() < 2 || arguments.len() > 3 {
            return Err(self.fail(
                call,
                "bindTypertRemote() requires this, service key, and an optional options object",
            )?);
        }
        let first = arguments[0];
        if self.syntax.kind(self.scope, first)? != kinds.this_keyword {
            return Err(self.fail(first, "bindTypertRemote() first argument must be this")?);
        }
        Ok(Some(self.gateway_binding_arguments(call, property)?))
    }

    fn gateway_service_binding(
        &mut self,
        declaration: Val<'s>,
    ) -> FaceResult<Option<GatewayBinding<'s>>> {
        let kinds = self.syntax.kinds.clone();
        let mut heritage = None;
        for clause in js::get_items(self.scope, declaration, "heritageClauses")? {
            if js::integer(js::get_number(self.scope, clause, "token")?) != kinds.extends_keyword {
                continue;
            }
            for ty in js::get_items(self.scope, clause, "types")? {
                let expression = js::get(self.scope, ty, "expression")?;
                if heritage.is_none()
                    && self.is_type_meta_symbol(expression, "TypertRemoteService")?
                {
                    heritage = Some(ty);
                }
            }
        }
        let Some(heritage) = heritage else {
            return Ok(None);
        };
        let mut constructor = None;
        for member in js::get_items(self.scope, declaration, "members")? {
            if constructor.is_none() && self.syntax.is(self.scope, member, kinds.constructor)? {
                constructor = Some(member);
            }
        }
        let body = match constructor {
            Some(constructor) => js::get_defined(self.scope, constructor, "body")?,
            None => None,
        };
        let (Some(constructor), Some(body)) = (constructor, body) else {
            return Err(self.fail(
                heritage,
                "TypertRemoteService subclasses must declare a constructor with super(ctx, serviceKey)",
            )?);
        };
        let mut call = None;
        for statement in js::get_items(self.scope, body, "statements")? {
            if call.is_some() {
                break;
            }
            if !self
                .syntax
                .is(self.scope, statement, kinds.expression_statement)?
            {
                continue;
            }
            let expression = js::get(self.scope, statement, "expression")?;
            if !self
                .syntax
                .is(self.scope, expression, kinds.call_expression)?
            {
                continue;
            }
            let callee = js::get(self.scope, expression, "expression")?;
            if self.syntax.kind(self.scope, callee)? == kinds.super_keyword {
                call = Some(expression);
            }
        }
        let Some(call) = call else {
            return Err(self.fail(
                constructor,
                "TypertRemoteService constructor must call super(ctx, serviceKey) directly",
            )?);
        };
        let arguments = js::get_items(self.scope, call, "arguments")?;
        if arguments.len() < 2 || arguments.len() > 3 {
            return Err(self.fail(
                call,
                "TypertRemoteService super() requires context, service key, and an optional options object",
            )?);
        }
        Ok(Some(self.gateway_binding_arguments(call, heritage)?))
    }

    fn gateway_binding_arguments(
        &mut self,
        call: Val<'s>,
        site: Val<'s>,
    ) -> FaceResult<GatewayBinding<'s>> {
        let kinds = self.syntax.kinds.clone();
        let arguments = js::get_items(self.scope, call, "arguments")?;
        let Some(service_argument) = arguments.get(1).copied() else {
            return Err(self.fail(call, "Gateway service key must be a string literal")?);
        };
        let Some(service) = self
            .syntax
            .string_literal_value(self.scope, Some(service_argument))?
        else {
            return Err(self.fail(
                service_argument,
                "Gateway service key must be a string literal",
            )?);
        };
        let mut namespace = service.clone();
        let options = arguments.get(2).copied();
        if let Some(options) = options {
            if !self
                .syntax
                .is(self.scope, options, kinds.object_literal_expression)?
            {
                return Err(self.fail(
                    options,
                    "bindTypertRemote() options must be an object literal",
                )?);
            }
            for property_option in js::get_items(self.scope, options, "properties")? {
                let is_assignment =
                    self.syntax
                        .is(self.scope, property_option, kinds.property_assignment)?;
                let named_namespace = if is_assignment {
                    let name = js::get(self.scope, property_option, "name")?;
                    self.syntax.member_name(self.scope, name)? == "namespace"
                } else {
                    false
                };
                if !is_assignment || !named_namespace {
                    return Err(self.fail(
                        property_option,
                        "bindTypertRemote() only supports a namespace option",
                    )?);
                }
                let initializer = js::get(self.scope, property_option, "initializer")?;
                let Some(value) = self
                    .syntax
                    .string_literal_value(self.scope, Some(initializer))?
                else {
                    return Err(
                        self.fail(initializer, "Gateway namespace must be a string literal")?
                    );
                };
                namespace = value;
            }
        }
        if !is_remote_segment(&service) {
            return Err(self.fail(
                service_argument,
                "Gateway service key must contain only RPC endpoint segment characters",
            )?);
        }
        if !is_remote_segment(&namespace) {
            let site = options.unwrap_or(call);
            return Err(self.fail(
                site,
                "Gateway namespace must contain only RPC endpoint segment characters",
            )?);
        }
        Ok(GatewayBinding {
            service,
            namespace,
            site,
        })
    }

    fn remote_marker(&mut self, member: Val<'s>) -> FaceResult<Option<RemoteMarker>> {
        let kinds = self.syntax.kinds.clone();
        let mut found: Option<RemoteMarker> = None;
        for decorator in self.syntax.decorators(self.scope, member)? {
            let expression = js::get(self.scope, decorator, "expression")?;
            let is_call = self
                .syntax
                .is(self.scope, expression, kinds.call_expression)?;
            let callee = if is_call {
                Some(js::get(self.scope, expression, "expression")?)
            } else {
                None
            };
            let marker = if self.is_type_meta_symbol(expression, "Remote")? {
                RemoteMarker::Direct { export_name: None }
            } else if let Some(callee) = callee
                && self.is_type_meta_symbol(callee, "Remote")?
            {
                let arguments = js::get_items(self.scope, expression, "arguments")?;
                if arguments.len() != 1 {
                    return Err(
                        self.fail(expression, "Remote() requires one exported method name")?
                    );
                }
                let export_name = self
                    .syntax
                    .string_literal_value(self.scope, arguments.first().copied())?;
                match export_name {
                    Some(name) if is_remote_segment(&name) => RemoteMarker::Direct {
                        export_name: Some(name),
                    },
                    _ => {
                        let site = arguments.first().copied().unwrap_or(expression);
                        return Err(self.fail(
                            site,
                            "Remote() name must be a string literal containing only RPC endpoint segment characters",
                        )?);
                    }
                }
            } else if let Some(callee) = callee
                && self.is_type_meta_symbol(callee, "RemoteScope")?
            {
                let arguments = js::get_items(self.scope, expression, "arguments")?;
                if arguments.is_empty() || arguments.len() > 2 {
                    return Err(self.fail(
                        expression,
                        "RemoteScope() requires a Context key and optional exported method name",
                    )?);
                }
                let context = self
                    .syntax
                    .string_literal_value(self.scope, arguments.first().copied())?;
                let context = match context {
                    Some(context) if is_remote_segment(&context) => context,
                    _ => {
                        let site = arguments.first().copied().unwrap_or(expression);
                        return Err(self.fail(
                            site,
                            "RemoteScope() key must be a string literal containing only RPC endpoint segment characters",
                        )?);
                    }
                };
                let export_argument = arguments.get(1).copied();
                let export_name = self
                    .syntax
                    .string_literal_value(self.scope, export_argument)?;
                if let Some(argument) = export_argument
                    && !export_name.as_deref().is_some_and(is_remote_segment)
                {
                    return Err(self.fail(
                        argument,
                        "RemoteScope() name must be a string literal containing only RPC endpoint segment characters",
                    )?);
                }
                RemoteMarker::Context {
                    context,
                    export_name,
                }
            } else {
                continue;
            };
            if found.is_some() {
                return Err(self.fail(
                    decorator,
                    "a method can have only one Remote invocation decorator",
                )?);
            }
            found = Some(marker);
        }
        Ok(found)
    }

    fn remote_result_type(&mut self, method: Val<'s>) -> FaceResult<Val<'s>> {
        let explicit = js::get_defined(self.scope, method, "type")?;
        let authored = self.required_type(method, explicit, TypePurpose::Return)?;
        if !self
            .syntax
            .is(self.scope, authored, self.syntax.kinds.type_reference)?
        {
            return Ok(authored);
        }
        let type_name = js::get(self.scope, authored, "typeName")?;
        let symbol = js::call(
            self.scope,
            self.checker,
            "getSymbolAtLocation",
            &[type_name],
        )?;
        let resolved = if symbol.is_undefined() {
            None
        } else {
            Some(self.resolve_symbol(symbol)?)
        };
        let arguments = js::get_items(self.scope, authored, "typeArguments")?;
        let Some(resolved) = resolved else {
            return Ok(authored);
        };
        if js::get_string(self.scope, resolved, "name")? != "Promise" || arguments.len() != 1 {
            return Ok(authored);
        }
        let Some(declaration) = self.syntax.preferred_declaration(self.scope, resolved)? else {
            return Ok(authored);
        };
        let file = self.syntax.file_name_of(self.scope, declaration)?;
        if !is_standard_library_file(&file) {
            return Ok(authored);
        }
        Ok(arguments[0])
    }

    fn is_global_abort_signal(&mut self, ty: Val<'s>) -> FaceResult<bool> {
        let Some(symbol) = self.symbol_at_type(ty)? else {
            return Ok(false);
        };
        if js::get_string(self.scope, symbol, "name")? != "AbortSignal" {
            return Ok(false);
        }
        for declaration in js::get_items(self.scope, symbol, "declarations")? {
            let file = self.syntax.file_name_of(self.scope, declaration)?;
            if is_standard_library_file(&file) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn lookup_declarations(&mut self) -> FaceResult<Vec<(String, SymbolId, Val<'s>)>> {
        if let Some(lookups) = &self.static_lookups {
            return Ok(lookups
                .iter()
                .map(|lookup| {
                    (
                        lookup.key.clone(),
                        lookup.host_symbol.clone(),
                        lookup.wire_type,
                    )
                })
                .collect());
        }
        let kinds = self.syntax.kinds.clone();
        let mut by_key = IndexMap::<String, StaticLookupDeclaration<'s>>::new();
        let mut by_host = std::collections::HashSet::<SymbolId>::new();
        for declaration in self.type_meta_map_members("TypertLookupMap")? {
            let declared_type = js::get_defined(self.scope, declaration, "type")?;
            let (Some(declared_type), true) = (
                declared_type,
                self.syntax
                    .is(self.scope, declaration, kinds.property_signature)?,
            ) else {
                return Err(self.fail(
                    declaration,
                    "TypertLookupMap entries must be required properties",
                )?);
            };
            let name = js::get(self.scope, declaration, "name")?;
            let key = self.syntax.member_name(self.scope, name)?;
            if !is_remote_segment(&key) {
                return Err(self.fail(
                    name,
                    "TypertLookupMap key must contain only RPC endpoint segment characters",
                )?);
            }
            let arguments = js::get_items(self.scope, declared_type, "typeArguments")?;
            let valid = self
                .syntax
                .is(self.scope, declared_type, kinds.type_reference)?
                && {
                    let type_name = js::get(self.scope, declared_type, "typeName")?;
                    self.is_type_meta_symbol(type_name, "TypertLookup")?
                }
                && arguments.len() == 2;
            if !valid {
                return Err(self.fail(
                    declared_type,
                    "TypertLookupMap values must be TypertLookup<Host, Wire>",
                )?);
            }
            let host_type = arguments[0];
            let wire_type = arguments[1];
            let Some(host) = self.symbol_at_type(host_type)? else {
                return Err(self.fail(host_type, "TypertLookup Host must be a named type")?);
            };
            let host_symbol = self.symbol_id(host)?;
            if by_key.contains_key(&key) {
                return Err(
                    self.fail(declaration, &format!("duplicate TypertLookupMap key {key}"))?
                );
            }
            if by_host.contains(&host_symbol) {
                let host_name = js::get_string(self.scope, host, "name")?;
                return Err(self.fail(
                    declaration,
                    &format!("Host type {host_name} has more than one Typert lookup"),
                )?);
            }
            by_host.insert(host_symbol.clone());
            by_key.insert(
                key.clone(),
                StaticLookupDeclaration {
                    key,
                    host_symbol,
                    wire_type,
                },
            );
        }
        let lookups = by_key.into_values().collect::<Vec<_>>();
        let result = lookups
            .iter()
            .map(|lookup| {
                (
                    lookup.key.clone(),
                    lookup.host_symbol.clone(),
                    lookup.wire_type,
                )
            })
            .collect();
        self.static_lookups = Some(lookups);
        Ok(result)
    }

    fn context_declarations(
        &mut self,
    ) -> FaceResult<IndexMap<String, StaticContextDeclaration<'s>>> {
        if let Some(contexts) = &self.static_contexts {
            return Ok(contexts
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        StaticContextDeclaration {
                            key: value.key.clone(),
                            wire_type: value.wire_type,
                        },
                    )
                })
                .collect());
        }
        let kinds = self.syntax.kinds.clone();
        let mut result = IndexMap::<String, StaticContextDeclaration<'s>>::new();
        for declaration in self.type_meta_map_members("TypertContextMap")? {
            let declared_type = js::get_defined(self.scope, declaration, "type")?;
            let (Some(declared_type), true) = (
                declared_type,
                self.syntax
                    .is(self.scope, declaration, kinds.property_signature)?,
            ) else {
                return Err(self.fail(
                    declaration,
                    "TypertContextMap entries must be required properties",
                )?);
            };
            let name = js::get(self.scope, declaration, "name")?;
            let key = self.syntax.member_name(self.scope, name)?;
            if !is_remote_segment(&key) {
                return Err(self.fail(
                    name,
                    "TypertContextMap key must contain only RPC endpoint segment characters",
                )?);
            }
            let arguments = js::get_items(self.scope, declared_type, "typeArguments")?;
            let valid = self
                .syntax
                .is(self.scope, declared_type, kinds.type_reference)?
                && {
                    let type_name = js::get(self.scope, declared_type, "typeName")?;
                    self.is_type_meta_symbol(type_name, "TypertContext")?
                }
                && arguments.len() == 1;
            if !valid {
                return Err(self.fail(
                    declared_type,
                    "TypertContextMap values must be TypertContext<Wire>",
                )?);
            }
            if result.contains_key(&key) {
                return Err(self.fail(
                    declaration,
                    &format!("duplicate TypertContextMap key {key}"),
                )?);
            }
            result.insert(
                key.clone(),
                StaticContextDeclaration {
                    key,
                    wire_type: arguments[0],
                },
            );
        }
        let copy = result
            .iter()
            .map(|(key, value)| {
                (
                    key.clone(),
                    StaticContextDeclaration {
                        key: value.key.clone(),
                        wire_type: value.wire_type,
                    },
                )
            })
            .collect();
        self.static_contexts = Some(result);
        Ok(copy)
    }

    fn type_meta_map_members(&mut self, name: &str) -> FaceResult<Vec<Val<'s>>> {
        let kinds = self.syntax.kinds.clone();
        let mut result = Vec::new();
        let source_files = js::call(self.scope, self.program, "getSourceFiles", &[])?;
        for source_file in js::items(self.scope, source_files)? {
            for statement in js::get_items(self.scope, source_file, "statements")? {
                let Some(body) =
                    self.syntax
                        .module_named(self.scope, statement, PROTOCOL_MODULES)?
                else {
                    continue;
                };
                for nested in js::get_items(self.scope, body, "statements")? {
                    if !self
                        .syntax
                        .is(self.scope, nested, kinds.interface_declaration)?
                    {
                        continue;
                    }
                    let nested_name = js::get(self.scope, nested, "name")?;
                    if js::get_string(self.scope, nested_name, "text")? == name {
                        result.extend(js::get_items(self.scope, nested, "members")?);
                    }
                }
            }
        }
        Ok(result)
    }

    fn remote_boundary(
        &mut self,
        authored_type: Val<'s>,
        fallback_type_symbol: &str,
        require_named: bool,
        top_level_absence: TopLevelAbsence,
        optional: bool,
    ) -> FaceResult<RemoteBoundaryModel> {
        let ty = self.convert_type(authored_type)?;
        let declared_type = js::call(
            self.scope,
            self.checker,
            "getTypeFromTypeNode",
            &[authored_type],
        )?;
        // An optional parameter's authored node carries no `undefined`; the codec
        // still has to accept the omitted wire field the consumer sends.
        let resolved_type = if optional {
            let undefined_flag = js::number(
                self.scope,
                f64::from(self.syntax.constants.type_flag("Undefined")),
            );
            js::call(
                self.scope,
                self.checker,
                "getNullableType",
                &[declared_type, undefined_flag],
            )?
        } else {
            declared_type
        };
        let codec_type =
            self.resolved_remote_codec_type(authored_type, resolved_type, top_level_absence)?;
        let accepts_undefined = top_level_absence != TopLevelAbsence::Reject
            && self.includes_remote_absence(resolved_type)?;
        let root_symbol = self.named_workspace_type(authored_type)?;
        let mut imports = IndexMap::<SymbolId, RemoteTypeImportModel>::new();
        let mut stack = vec![authored_type];
        let kinds = self.syntax.kinds.clone();
        while let Some(node) = stack.pop() {
            let kind = self.syntax.kind(self.scope, node)?;
            if kind == kinds.type_reference || kind == kinds.import_type {
                let symbol = if kind == kinds.type_reference {
                    let type_name = js::get(self.scope, node, "typeName")?;
                    js::call(
                        self.scope,
                        self.checker,
                        "getSymbolAtLocation",
                        &[type_name],
                    )?
                } else {
                    match js::get_defined(self.scope, node, "qualifier")? {
                        Some(qualifier) => js::call(
                            self.scope,
                            self.checker,
                            "getSymbolAtLocation",
                            &[qualifier],
                        )?,
                        None => js::undefined(self.scope),
                    }
                };
                if !symbol.is_undefined() {
                    let resolved = self.resolve_symbol(symbol)?;
                    if let Some(declaration) =
                        self.syntax.preferred_declaration(self.scope, resolved)?
                    {
                        let file = self.syntax.file_name_of(self.scope, declaration)?;
                        if !is_standard_library_file(&file)
                            && self.registration_for_file(&file).is_some()
                        {
                            let imported = self.public_remote_type(resolved, node)?;
                            imports.insert(imported.symbol.clone(), imported);
                        }
                    }
                }
            }
            let children = self.children(node)?;
            for child in children.into_iter().rev() {
                stack.push(child);
            }
        }
        let mut imports = imports.into_values().collect::<Vec<_>>();
        imports.sort_by(|left, right| {
            locale_compare(&left.specifier, &right.specifier)
                .then_with(|| locale_compare(&left.name, &right.name))
        });
        if let Some(root_symbol) = root_symbol {
            let imported = self.public_remote_type(root_symbol, authored_type)?;
            return Ok(RemoteBoundaryModel {
                ty,
                codec_type,
                accepts_undefined,
                type_symbol: TypeSymbolId::from(format!(
                    "{}#{}",
                    imported.specifier, imported.name
                )),
                imports,
            });
        }
        if require_named {
            return Err(self.fail(
                authored_type,
                "lookup and Context wire types must be named public types",
            )?);
        }
        Ok(RemoteBoundaryModel {
            ty,
            codec_type,
            accepts_undefined,
            type_symbol: TypeSymbolId::from(fallback_type_symbol),
            imports,
        })
    }

    /// Direct child nodes in `forEachChild` order.
    fn children(&mut self, node: Val<'s>) -> FaceResult<Vec<Val<'s>>> {
        let collected = js::array(self.scope, &[]);
        let data = js::object(self.scope, &[("items", collected)])?;
        let visitor = v8::Function::builder(collect_child)
            .data(data)
            .build(self.scope)
            .ok_or_else(|| js::failure("cannot create child visitor"))?;
        js::call(
            self.scope,
            self.syntax.ts,
            "forEachChild",
            &[node, visitor.into()],
        )?;
        Ok(js::items(self.scope, collected)?)
    }

    fn resolved_remote_codec_type(
        &mut self,
        authored_type: Val<'s>,
        resolved_type: Val<'s>,
        top_level_absence: TopLevelAbsence,
    ) -> FaceResult<TypeNodeId> {
        let active = JsSet::new(self.scope);
        self.assert_remote_json_type(
            resolved_type,
            authored_type,
            active,
            top_level_absence != TopLevelAbsence::Reject,
            top_level_absence == TopLevelAbsence::UndefinedOrVoid,
        )?;
        let mut state = CodecState {
            completed: JsMap::new(self.scope),
            active: JsMap::new(self.scope),
            recursive_declarations: JsMap::new(self.scope),
        };
        self.convert_resolved(authored_type, resolved_type, &mut state)
    }

    fn convert_resolved(
        &mut self,
        authored_type: Val<'s>,
        ty: Val<'s>,
        state: &mut CodecState<'s>,
    ) -> FaceResult<TypeNodeId> {
        if let Some(cached) = state.completed.get(self.scope, ty) {
            return Ok(TypeNodeId::from(js::text(self.scope, cached)));
        }
        if let Some(active_id) = state.active.get(self.scope, ty) {
            let active_id = TypeNodeId::from(js::text(self.scope, active_id));
            if self.is_array_like(ty)? {
                let element = self.index_type(ty)?;
                if let Some(element) = element
                    && let Some(element_id) = state.active.get(self.scope, element)
                {
                    let element_id = TypeNodeId::from(js::text(self.scope, element_id));
                    let reference =
                        self.resolved_cycle_reference(element, authored_type, &element_id, state)?;
                    return self
                        .add_node(authored_type, TypeNodeKind::Array { element: reference });
                }
            }
            return self.resolved_cycle_reference(ty, authored_type, &active_id, state);
        }
        let id = self.allocate_node_id(authored_type)?;
        let id_value = js::string(self.scope, id.as_str());
        state.active.set(self.scope, ty, id_value);
        let outcome = self.convert_resolved_body(authored_type, ty, &id, state);
        state.active.delete(self.scope, ty);
        let kind = outcome?;
        self.nodes.insert(
            id.clone(),
            TypeNodeModel::Defined(DefinedTypeNode {
                id: id.clone(),
                kind,
            }),
        );
        state.completed.set(self.scope, ty, id_value);
        Ok(id)
    }

    fn type_flags(&mut self, ty: Val<'s>) -> FaceResult<u32> {
        Ok(js::get_flags(self.scope, ty, "flags")?)
    }

    fn has_flag(&self, flags: u32, name: &str) -> bool {
        flags & self.syntax.constants.type_flag(name) != 0
    }

    fn convert_resolved_body(
        &mut self,
        authored_type: Val<'s>,
        ty: Val<'s>,
        id: &TypeNodeId,
        state: &mut CodecState<'s>,
    ) -> FaceResult<TypeNodeKind> {
        let flags = self.type_flags(ty)?;
        let keyword = |name: KeywordTypeName| TypeNodeKind::Keyword { name };
        if self.has_flag(flags, "Any") {
            return Ok(keyword(KeywordTypeName::Any));
        }
        if self.has_flag(flags, "Unknown") {
            return Ok(keyword(KeywordTypeName::Unknown));
        }
        if self.has_flag(flags, "Never") {
            return Ok(keyword(KeywordTypeName::Never));
        }
        if self.has_flag(flags, "String") {
            return Ok(keyword(KeywordTypeName::String));
        }
        if self.has_flag(flags, "Number") {
            return Ok(keyword(KeywordTypeName::Number));
        }
        if self.has_flag(flags, "BigInt") {
            return Ok(keyword(KeywordTypeName::Bigint));
        }
        if self.has_flag(flags, "Boolean") {
            return Ok(keyword(KeywordTypeName::Boolean));
        }
        if self.has_flag(flags, "ESSymbol") {
            return Ok(keyword(KeywordTypeName::Symbol));
        }
        if self.has_flag(flags, "Undefined") {
            return Ok(keyword(KeywordTypeName::Undefined));
        }
        if self.has_flag(flags, "Void") {
            return Ok(keyword(KeywordTypeName::Void));
        }
        if self.has_flag(flags, "Null") {
            return Ok(TypeNodeKind::Literal {
                value: LiteralValue::Null,
                text: "null".to_owned(),
            });
        }
        if self.has_flag(flags, "StringLiteral") {
            let value = js::get_string(self.scope, ty, "value")?;
            return Ok(TypeNodeKind::Literal {
                text: serde_json::to_string(&value)
                    .map_err(|error| js::failure(error.to_string()))?,
                value: LiteralValue::String(value),
            });
        }
        if self.has_flag(flags, "NumberLiteral") {
            let value = js::get_number(self.scope, ty, "value")?;
            let text = jstext::number_text(value);
            return Ok(TypeNodeKind::Literal {
                value: text
                    .parse::<serde_json::Number>()
                    .map_or_else(|_| LiteralValue::String(text.clone()), LiteralValue::Number),
                text,
            });
        }
        if self.has_flag(flags, "BigIntLiteral") {
            let value = js::get(self.scope, ty, "value")?;
            let negative = js::get_bool(self.scope, value, "negative")?;
            let digits = js::get_string(self.scope, value, "base10Value")?;
            let sign = if negative { "-" } else { "" };
            return Ok(TypeNodeKind::Literal {
                value: LiteralValue::BigInt {
                    digits: jstext::bigint_decimal(&format!("{sign}{digits}")),
                },
                text: format!("{sign}{digits}n"),
            });
        }
        if self.has_flag(flags, "BooleanLiteral") {
            let intrinsic = js::get_optional_string(self.scope, ty, "intrinsicName")?;
            let value = intrinsic.as_deref() == Some("true");
            return Ok(TypeNodeKind::Literal {
                value: LiteralValue::Boolean(value),
                text: value.to_string(),
            });
        }
        let union_or_intersection = js::call(self.scope, ty, "isUnionOrIntersection", &[])?;
        if union_or_intersection.boolean_value(self.scope) {
            let mut types = Vec::new();
            for member in js::get_items(self.scope, ty, "types")? {
                types.push(self.convert_resolved(authored_type, member, state)?);
            }
            return Ok(if self.has_flag(flags, "Union") {
                TypeNodeKind::Union { types }
            } else {
                TypeNodeKind::Intersection { types }
            });
        }
        if self.has_flag(flags, "TypeParameter") {
            return Err(self.fail(
                authored_type,
                "Remote codec contains an unresolved type parameter",
            )?);
        }
        if !self.has_flag(flags, "Object") {
            let rendered = self.type_to_string(ty, Some(authored_type))?;
            return Err(self.fail(
                authored_type,
                &format!("Remote codec type {rendered} has no concrete Zod projection"),
            )?);
        }
        if self.is_tuple(ty)? {
            let target = js::get(self.scope, ty, "target")?;
            let element_flags = js::get_items(self.scope, target, "elementFlags")?;
            let arguments = js::call(self.scope, self.checker, "getTypeArguments", &[ty])?;
            let mut elements = Vec::new();
            for (index, argument) in js::items(self.scope, arguments)?.into_iter().enumerate() {
                let element_flag = match element_flags.get(index) {
                    Some(flag) => js::integer(flag.number_value(self.scope).unwrap_or(0.0)),
                    None => self.syntax.constants.element_flag("Required"),
                };
                let optional = element_flag & self.syntax.constants.element_flag("Optional") != 0;
                let rest = element_flag
                    & (self.syntax.constants.element_flag("Rest")
                        | self.syntax.constants.element_flag("Variadic"))
                    != 0;
                elements.push(TupleElementModel {
                    name: None,
                    ty: self.convert_resolved(authored_type, argument, state)?,
                    optional,
                    rest,
                });
            }
            return Ok(TypeNodeKind::Tuple { elements });
        }
        if self.is_array_like(ty)? {
            let Some(element) = self.index_type(ty)? else {
                return Err(self.fail(authored_type, "Remote codec array has no element type")?);
            };
            return Ok(TypeNodeKind::Array {
                element: self.convert_resolved(authored_type, element, state)?,
            });
        }
        if self.has_signatures(ty)? {
            return Err(self.fail(
                authored_type,
                "Remote codec cannot contain callable or constructable values",
            )?);
        }
        let mut members = Vec::new();
        let location = self.location(authored_type)?;
        let properties = js::call(self.scope, self.checker, "getPropertiesOfType", &[ty])?;
        for property in js::items(self.scope, properties)? {
            let declaration = self.property_declaration(property)?;
            let site = declaration.unwrap_or(authored_type);
            let property_type = js::call(
                self.scope,
                self.checker,
                "getTypeOfSymbolAtLocation",
                &[property, site],
            )?;
            let symbol_key = js::call(self.scope, property, "getName", &[])?;
            let symbol_key = js::text(self.scope, symbol_key);
            let property_flags = js::get_flags(self.scope, property, "flags")?;
            let read_only = match declaration {
                Some(declaration) => self.syntax.has_modifier(
                    self.scope,
                    declaration,
                    self.syntax.kinds.readonly_keyword,
                )?,
                None => false,
            };
            let ty = self.convert_resolved(authored_type, property_type, state)?;
            members.push(MemberModel::Defined(Box::new(DefinedMember {
                base: MemberBase {
                    documentation: DocumentationModel::empty(),
                    id: crate::model::MemberId::from(format!("{id}#{symbol_key}")),
                    name: symbol_key.clone(),
                    json_name: None,
                    computed: symbol_key
                        .starts_with("__@")
                        .then_some(crate::model::ComputedMember::Symbol),
                    optional: property_flags & self.syntax.constants.symbol_flag("Optional") != 0,
                    read_only,
                    is_async: false,
                    is_abstract: false,
                    is_static: false,
                    visibility: MemberVisibility::Public,
                    location: location.clone(),
                    text: String::new(),
                },
                kind: MemberKind::Property { ty },
            })));
        }
        let index_infos = js::call(self.scope, self.checker, "getIndexInfosOfType", &[ty])?;
        for (index, info) in js::items(self.scope, index_infos)?.into_iter().enumerate() {
            let key_type = js::get(self.scope, info, "keyType")?;
            let value_type = js::get(self.scope, info, "type")?;
            let read_only = js::get_bool(self.scope, info, "isReadonly")?;
            let key = self.convert_resolved(authored_type, key_type, state)?;
            let returns = self.convert_resolved(authored_type, value_type, state)?;
            members.push(MemberModel::Defined(Box::new(DefinedMember {
                base: MemberBase {
                    documentation: DocumentationModel::empty(),
                    id: crate::model::MemberId::from(format!("{id}#index:{index}")),
                    name: "(index)".to_owned(),
                    json_name: None,
                    computed: None,
                    optional: false,
                    read_only,
                    is_async: false,
                    is_abstract: false,
                    is_static: false,
                    visibility: MemberVisibility::Public,
                    location: location.clone(),
                    text: String::new(),
                },
                kind: MemberKind::Index {
                    signature: SignatureModel {
                        type_parameters: Vec::new(),
                        parameters: vec![ParameterModel {
                            name: "key".to_owned(),
                            binding: ParameterBinding::Identifier,
                            ty: key,
                            optional: false,
                            rest: false,
                            receiver: false,
                            initializer: None,
                        }],
                        returns,
                    },
                },
            })));
        }
        Ok(TypeNodeKind::Object { members })
    }

    fn property_declaration(&mut self, property: Val<'s>) -> FaceResult<Option<Val<'s>>> {
        if let Some(declaration) = js::get_defined(self.scope, property, "valueDeclaration")? {
            return Ok(Some(declaration));
        }
        Ok(js::get_items(self.scope, property, "declarations")?
            .first()
            .copied())
    }

    fn type_to_string(&mut self, ty: Val<'s>, site: Option<Val<'s>>) -> FaceResult<String> {
        let rendered = match site {
            Some(site) => {
                let flags = js::number(
                    self.scope,
                    f64::from(self.syntax.constants.value("TypeFormatFlags.NoTruncation")),
                );
                js::call(self.scope, self.checker, "typeToString", &[ty, site, flags])?
            }
            None => js::call(self.scope, self.checker, "typeToString", &[ty])?,
        };
        Ok(js::text(self.scope, rendered))
    }

    fn is_tuple(&mut self, ty: Val<'s>) -> FaceResult<bool> {
        let value = js::call(self.scope, self.checker, "isTupleType", &[ty])?;
        Ok(value.boolean_value(self.scope))
    }

    fn is_array_like(&mut self, ty: Val<'s>) -> FaceResult<bool> {
        let array = js::call(self.scope, self.checker, "isArrayType", &[ty])?;
        if array.boolean_value(self.scope) {
            return Ok(true);
        }
        let array_like = js::call(self.scope, self.checker, "isArrayLikeType", &[ty])?;
        Ok(array_like.boolean_value(self.scope))
    }

    fn index_type(&mut self, ty: Val<'s>) -> FaceResult<Option<Val<'s>>> {
        let number = js::number(
            self.scope,
            f64::from(self.syntax.constants.value("IndexKind.Number")),
        );
        let element = js::call(
            self.scope,
            self.checker,
            "getIndexTypeOfType",
            &[ty, number],
        )?;
        Ok((!element.is_undefined()).then_some(element))
    }

    fn has_signatures(&mut self, ty: Val<'s>) -> FaceResult<bool> {
        let calls = js::call(self.scope, ty, "getCallSignatures", &[])?;
        if !js::items(self.scope, calls)?.is_empty() {
            return Ok(true);
        }
        let constructs = js::call(self.scope, ty, "getConstructSignatures", &[])?;
        Ok(!js::items(self.scope, constructs)?.is_empty())
    }

    fn assert_remote_json_type(
        &mut self,
        ty: Val<'s>,
        site: Val<'s>,
        active: JsSet<'s>,
        allow_undefined: bool,
        allow_void: bool,
    ) -> FaceResult<()> {
        let flags = self.type_flags(ty)?;
        if self.has_flag(flags, "Undefined") && allow_undefined {
            return Ok(());
        }
        if self.has_flag(flags, "Void") && allow_void {
            return Ok(());
        }
        if self.has_flag(flags, "Any") || self.has_flag(flags, "Unknown") {
            let rendered = self.type_to_string(ty, None)?;
            return Err(self.fail(
                site,
                &format!("Remote boundary contains unconstrained {rendered} data"),
            )?);
        }
        if self.has_flag(flags, "BigIntLike")
            || self.has_flag(flags, "ESSymbolLike")
            || self.has_flag(flags, "Undefined")
            || self.has_flag(flags, "Void")
        {
            let rendered = self.type_to_string(ty, None)?;
            return Err(self.fail(
                site,
                &format!("Remote boundary contains non-JSON type {rendered}"),
            )?);
        }
        if self.has_flag(flags, "StringLike")
            || self.has_flag(flags, "NumberLike")
            || self.has_flag(flags, "BooleanLike")
            || self.has_flag(flags, "Null")
            || self.has_flag(flags, "Never")
        {
            return Ok(());
        }
        let is_union = js::call(self.scope, ty, "isUnion", &[])?.boolean_value(self.scope);
        if is_union {
            for member in js::get_items(self.scope, ty, "types")? {
                self.assert_remote_json_type(member, site, active, allow_undefined, allow_void)?;
            }
            return Ok(());
        }
        let is_intersection =
            js::call(self.scope, ty, "isIntersection", &[])?.boolean_value(self.scope);
        if is_intersection {
            let mut material = Vec::new();
            for member in js::get_items(self.scope, ty, "types")? {
                if !self.is_remote_phantom_constraint(member)? {
                    material.push(member);
                }
            }
            if material.is_empty() {
                return Err(self.fail(site, "Remote boundary contains a symbol-only object")?);
            }
            for member in material {
                self.assert_remote_json_type(member, site, active, false, false)?;
            }
            return Ok(());
        }
        if self.has_flag(flags, "TypeParameter") {
            return Err(self.fail(
                site,
                "Remote boundary contains an unresolved type parameter",
            )?);
        }
        if !self.has_flag(flags, "Object") {
            let rendered = self.type_to_string(ty, None)?;
            return Err(self.fail(
                site,
                &format!("Remote boundary contains non-JSON type {rendered}"),
            )?);
        }
        let symbol = js::call(self.scope, ty, "getSymbol", &[])?;
        if !symbol.is_undefined() {
            let declaration = self.property_declaration(symbol)?;
            if let Some(declaration) = declaration
                && (self
                    .syntax
                    .is(self.scope, declaration, self.syntax.kinds.class_declaration)?
                    || self.syntax.is(
                        self.scope,
                        declaration,
                        self.syntax.kinds.class_expression,
                    )?)
            {
                let name = js::get_string(self.scope, symbol, "name")?;
                return Err(self.fail(
                    site,
                    &format!("Remote boundary contains class instance {name}"),
                )?);
            }
        }
        if self.has_signatures(ty)? {
            return Err(self.fail(
                site,
                "Remote boundary contains callable or constructable data",
            )?);
        }
        if active.has(self.scope, ty) {
            return Ok(());
        }
        active.add(self.scope, ty);
        let outcome = self.assert_remote_json_object(ty, site, active);
        active.delete(self.scope, ty);
        outcome
    }

    fn assert_remote_json_object(
        &mut self,
        ty: Val<'s>,
        site: Val<'s>,
        active: JsSet<'s>,
    ) -> FaceResult<()> {
        if self.is_tuple(ty)? {
            let target = js::get(self.scope, ty, "target")?;
            let element_flags = js::get_items(self.scope, target, "elementFlags")?;
            let arguments = js::call(self.scope, self.checker, "getTypeArguments", &[ty])?;
            for (index, argument) in js::items(self.scope, arguments)?.into_iter().enumerate() {
                let element_flag = match element_flags.get(index) {
                    Some(flag) => js::integer(flag.number_value(self.scope).unwrap_or(0.0)),
                    None => self.syntax.constants.element_flag("Required"),
                };
                let optional = element_flag & self.syntax.constants.element_flag("Optional") != 0;
                self.assert_remote_json_type(argument, site, active, optional, false)?;
            }
            return Ok(());
        }
        if self.is_array_like(ty)? {
            let Some(element) = self.index_type(ty)? else {
                return Err(self.fail(site, "Remote boundary array has no element type")?);
            };
            return self.assert_remote_json_type(element, site, active, false, false);
        }
        let properties = js::call(self.scope, self.checker, "getPropertiesOfType", &[ty])?;
        let properties = js::items(self.scope, properties)?;
        for property in &properties {
            let name = js::call(self.scope, *property, "getName", &[])?;
            if js::text(self.scope, name).starts_with("__@") {
                return Err(self.fail(site, "Remote boundary contains a symbol-keyed property")?);
            }
        }
        for property in properties {
            let declaration = self.property_declaration(property)?;
            let location = declaration.unwrap_or(site);
            let property_type = js::call(
                self.scope,
                self.checker,
                "getTypeOfSymbolAtLocation",
                &[property, location],
            )?;
            let property_flags = js::get_flags(self.scope, property, "flags")?;
            self.assert_remote_json_type(
                property_type,
                site,
                active,
                property_flags & self.syntax.constants.symbol_flag("Optional") != 0,
                false,
            )?;
        }
        let index_infos = js::call(self.scope, self.checker, "getIndexInfosOfType", &[ty])?;
        for info in js::items(self.scope, index_infos)? {
            let key_type = js::get(self.scope, info, "keyType")?;
            let key_flags = self.type_flags(key_type)?;
            if self.has_flag(key_flags, "ESSymbolLike") {
                return Err(self.fail(site, "Remote boundary contains a symbol index signature")?);
            }
            let value_type = js::get(self.scope, info, "type")?;
            self.assert_remote_json_type(value_type, site, active, false, false)?;
        }
        Ok(())
    }

    fn includes_remote_absence(&mut self, ty: Val<'s>) -> FaceResult<bool> {
        let flags = self.type_flags(ty)?;
        if self.has_flag(flags, "Undefined") || self.has_flag(flags, "Void") {
            return Ok(true);
        }
        let is_union = js::call(self.scope, ty, "isUnion", &[])?.boolean_value(self.scope);
        if !is_union {
            return Ok(false);
        }
        for member in js::get_items(self.scope, ty, "types")? {
            if self.includes_remote_absence(member)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn is_remote_phantom_constraint(&mut self, ty: Val<'s>) -> FaceResult<bool> {
        let flags = self.type_flags(ty)?;
        if self.has_flag(flags, "Unknown") {
            return Ok(true);
        }
        if self.has_flag(flags, "Any") || !self.has_flag(flags, "Object") {
            return Ok(false);
        }
        if self.has_signatures(ty)? {
            return Ok(false);
        }
        let index_infos = js::call(self.scope, self.checker, "getIndexInfosOfType", &[ty])?;
        if !js::items(self.scope, index_infos)?.is_empty() {
            return Ok(false);
        }
        let properties = js::call(self.scope, self.checker, "getPropertiesOfType", &[ty])?;
        for property in js::items(self.scope, properties)? {
            let name = js::call(self.scope, property, "getName", &[])?;
            if !js::text(self.scope, name).starts_with("__@") {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn resolved_cycle_reference(
        &mut self,
        ty: Val<'s>,
        site: Val<'s>,
        resolved_type: &TypeNodeId,
        state: &mut CodecState<'s>,
    ) -> FaceResult<TypeNodeId> {
        let symbol = match js::get_defined(self.scope, ty, "aliasSymbol")? {
            Some(alias) => alias,
            None => js::call(self.scope, ty, "getSymbol", &[])?,
        };
        if symbol.is_undefined() {
            return Err(self.fail(site, "Remote codec contains an unnamed recursive type")?);
        }
        let resolved = self.resolve_symbol(symbol)?;
        let resolved_name = js::get_string(self.scope, resolved, "name")?;
        let declaration = self.syntax.preferred_declaration(self.scope, resolved)?;
        let declaration_file = match declaration {
            Some(declaration) => Some(self.syntax.file_name_of(self.scope, declaration)?),
            None => None,
        };
        let (Some(declaration), Some(file)) = (declaration, declaration_file) else {
            return Err(self.fail(
                site,
                &format!(
                    "Remote codec recursive type {resolved_name} has no workspace declaration"
                ),
            )?);
        };
        if is_standard_library_file(&file) {
            return Err(self.fail(
                site,
                &format!(
                    "Remote codec recursive type {resolved_name} has no workspace declaration"
                ),
            )?);
        }
        let Some(owner) = self
            .registration_for_file(&file)
            .map(|registration| registration.name.clone())
        else {
            return Err(self.fail(
                site,
                &format!("Remote codec recursive type {resolved_name} is not owned by this face"),
            )?);
        };
        let id = if let Some(existing) = state.recursive_declarations.get(self.scope, ty) {
            SymbolId::from(js::text(self.scope, existing))
        } else {
            {
                let base = self.symbol_id(resolved)?;
                let id = SymbolId::from(format!("{base}#remote-codec:{resolved_type}"));
                let id_value = js::string(self.scope, id.as_str());
                state.recursive_declarations.set(self.scope, ty, id_value);
                let location = self.location(declaration)?;
                self.declarations.insert(
                    id.clone(),
                    TypeDeclarationModel {
                        documentation: DocumentationModel::empty(),
                        id: id.clone(),
                        package: owner,
                        name: format!("{resolved_name}RemoteCodec"),
                        kind: DeclarationKind::Alias,
                        is_abstract: false,
                        exported: false,
                        location,
                        text: String::new(),
                        type_parameters: Vec::new(),
                        extends: Vec::new(),
                        implements: Vec::new(),
                        members: Vec::new(),
                        parts: None,
                        ty: Some(resolved_type.clone()),
                        enum_members: None,
                    },
                );
                id
            }
        };
        self.add_node(
            site,
            TypeNodeKind::Reference {
                name: format!("{resolved_name}RemoteCodec"),
                target: TypeTargetModel::Declaration { symbol: id },
                arguments: Vec::new(),
            },
        )
    }

    fn named_workspace_type(&mut self, node: Val<'s>) -> FaceResult<Option<Val<'s>>> {
        let kinds = self.syntax.kinds.clone();
        let kind = self.syntax.kind(self.scope, node)?;
        if kind != kinds.type_reference && kind != kinds.import_type {
            return Ok(None);
        }
        let symbol = if kind == kinds.type_reference {
            let type_name = js::get(self.scope, node, "typeName")?;
            js::call(
                self.scope,
                self.checker,
                "getSymbolAtLocation",
                &[type_name],
            )?
        } else {
            match js::get_defined(self.scope, node, "qualifier")? {
                Some(qualifier) => js::call(
                    self.scope,
                    self.checker,
                    "getSymbolAtLocation",
                    &[qualifier],
                )?,
                None => return Ok(None),
            }
        };
        if symbol.is_undefined() {
            return Ok(None);
        }
        let resolved = self.resolve_symbol(symbol)?;
        let Some(declaration) = self.syntax.preferred_declaration(self.scope, resolved)? else {
            return Ok(None);
        };
        let file = self.syntax.file_name_of(self.scope, declaration)?;
        if is_standard_library_file(&file) || self.registration_for_file(&file).is_none() {
            return Ok(None);
        }
        Ok(Some(resolved))
    }

    fn public_remote_type(
        &mut self,
        symbol: Val<'s>,
        site: Val<'s>,
    ) -> FaceResult<RemoteTypeImportModel> {
        let symbol_name = js::get_string(self.scope, symbol, "name")?;
        let Some(declaration) = self.syntax.preferred_declaration(self.scope, symbol)? else {
            return Err(self.fail(site, &format!("type {symbol_name} has no declaration"))?);
        };
        let file = self.syntax.file_name_of(self.scope, declaration)?;
        let Some(registration) = self.registration_for_file(&file) else {
            return Err(self.fail(
                site,
                &format!("type {symbol_name} is not owned by a workspace package"),
            )?);
        };
        let mut candidates = Vec::new();
        for (subpath, target) in package_export_targets(&registration.manifest) {
            if subpath == "."
                || subpath == "./package.json"
                || subpath == "./typert"
                || subpath == "./client/typert"
                || subpath == "./remote"
                || target.contains('*')
            {
                continue;
            }
            let source_path =
                paths::real_path(&source_path_for_export(&registration.root, &target)?);
            let Some(source_file) = self.source_files.get(&source_path).copied() else {
                continue;
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
            let exports = js::call(
                self.scope,
                self.checker,
                "getExportsOfModule",
                &[module_symbol],
            )?;
            for exported in js::items(self.scope, exports)? {
                let resolved = self.resolve_symbol(exported)?;
                if !js::same(resolved, symbol) {
                    continue;
                }
                candidates.push(RemoteTypeImportModel {
                    symbol: self.symbol_id(symbol)?,
                    specifier: package_export_specifier(&registration.name, &subpath),
                    name: js::get_string(self.scope, exported, "name")?,
                });
            }
        }
        candidates.sort_by(|left, right| {
            locale_compare(&left.specifier, &right.specifier)
                .then_with(|| locale_compare(&left.name, &right.name))
        });
        let Some(selected) = candidates.into_iter().next() else {
            return Err(self.fail(
                site,
                &format!("Remote boundary type {symbol_name} must be exported from a public non-root type subpath"),
            )?);
        };
        Ok(selected)
    }

    fn is_workspace_class(&mut self, symbol: Val<'s>) -> FaceResult<bool> {
        let Some(declaration) = self.syntax.preferred_declaration(self.scope, symbol)? else {
            return Ok(false);
        };
        if !self
            .syntax
            .is(self.scope, declaration, self.syntax.kinds.class_declaration)?
        {
            return Ok(false);
        }
        let file = self.syntax.file_name_of(self.scope, declaration)?;
        Ok(self.registration_for_file(&file).is_some())
    }

    pub(crate) fn validate_invocation_identity(
        &mut self,
        packages: &[PackageModel],
    ) -> FaceResult<()> {
        let mut endpoints = HashMap::<String, &InvocationModel>::new();
        let mut ids = HashMap::<InvocationId, &InvocationModel>::new();
        for invocation in packages.iter().flat_map(|package| &package.invocations) {
            let endpoint = format!("{}/{}", invocation.namespace, invocation.method);
            if let Some(existing) = endpoints.get(&endpoint) {
                return Err(Interrupt::Failed(TypertGeneratorError::Analysis(format!(
                    "typert({}): {}:{}:{}: Remote endpoint {endpoint} conflicts with {}",
                    self.face.as_str(),
                    invocation.location.file,
                    invocation.location.line,
                    invocation.location.column,
                    existing.id
                ))));
            }
            if let Some(existing) = ids.get(&invocation.id) {
                return Err(Interrupt::Failed(TypertGeneratorError::Analysis(format!(
                    "typert({}): {}:{}:{}: Remote invocation id {} conflicts with {}",
                    self.face.as_str(),
                    invocation.location.file,
                    invocation.location.line,
                    invocation.location.column,
                    invocation.id,
                    existing.id
                ))));
            }
            endpoints.insert(endpoint, invocation);
            ids.insert(invocation.id.clone(), invocation);
        }
        Ok(())
    }
}

struct CodecState<'s> {
    completed: JsMap<'s>,
    active: JsMap<'s>,
    recursive_declarations: JsMap<'s>,
}

fn package_export_specifier(package_name: &str, subpath: &str) -> String {
    if subpath == "." {
        package_name.to_owned()
    } else {
        format!("{package_name}{}", &subpath[1..])
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "V8 callback ABI passes arguments by value"
)]
fn collect_child<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    _: v8::ReturnValue<'s>,
) {
    if let Ok(items) = js::get(scope, args.data(), "items")
        && let Ok(array) = v8::Local::<v8::Array>::try_from(items)
    {
        let index = array.length();
        array.set_index(scope, index, args.get(0));
    }
}
