//! Syntax-level helpers over compiler nodes, shared by every face analysis.
#![expect(
    clippy::unused_self,
    reason = "Node accessors keep one uniform compiler-helper call surface"
)]

use std::path::Path;

use crate::{
    Result, TypertGeneratorError,
    model::{
        DeclarationKind, DocumentationModel, JsDocTagModel, KeywordTypeName, LiteralValue,
        MappedModifier, MemberVisibility, TypeNodeKind,
    },
};

use super::{
    engine::Constants,
    js::{self, Scope, Val},
    jstext, paths,
};

/// Module identities accepted for the Cordis declaration-merge namespace.
pub(crate) const CORDIS_MODULES: &[&str] = &["@seekdeep-ai/cordis", "@deepseek-ai/cordis"];

/// Module identities accepted for the Typert protocol namespace.
pub(crate) const PROTOCOL_MODULES: &[&str] = &[
    "@seekdeep-ai/seekdeep-typert-protocol",
    "@deepseek-ai/dsh-typert-protocol",
];

macro_rules! kinds {
    ($($field:ident => $name:literal),+ $(,)?) => {
        /// Syntax kinds captured once per compiler.
        #[derive(Clone, Debug)]
        pub(crate) struct Kinds {
            $(pub(crate) $field: u32,)+
        }
        impl Kinds {
            fn capture(constants: &Constants) -> Self {
                Self { $($field: constants.kind($name),)+ }
            }
        }
    };
}

kinds! {
    class_declaration => "ClassDeclaration",
    class_expression => "ClassExpression",
    interface_declaration => "InterfaceDeclaration",
    type_alias_declaration => "TypeAliasDeclaration",
    enum_declaration => "EnumDeclaration",
    module_declaration => "ModuleDeclaration",
    module_block => "ModuleBlock",
    string_literal => "StringLiteral",
    no_substitution_template_literal => "NoSubstitutionTemplateLiteral",
    numeric_literal => "NumericLiteral",
    bigint_literal => "BigIntLiteral",
    identifier => "Identifier",
    private_identifier => "PrivateIdentifier",
    computed_property_name => "ComputedPropertyName",
    property_signature => "PropertySignature",
    property_declaration => "PropertyDeclaration",
    method_signature => "MethodSignature",
    method_declaration => "MethodDeclaration",
    get_accessor => "GetAccessor",
    set_accessor => "SetAccessor",
    call_signature => "CallSignature",
    construct_signature => "ConstructSignature",
    index_signature => "IndexSignature",
    constructor => "Constructor",
    export_keyword => "ExportKeyword",
    static_keyword => "StaticKeyword",
    readonly_keyword => "ReadonlyKeyword",
    async_keyword => "AsyncKeyword",
    abstract_keyword => "AbstractKeyword",
    private_keyword => "PrivateKeyword",
    protected_keyword => "ProtectedKeyword",
    const_keyword => "ConstKeyword",
    in_keyword => "InKeyword",
    out_keyword => "OutKeyword",
    extends_keyword => "ExtendsKeyword",
    import_declaration => "ImportDeclaration",
    export_declaration => "ExportDeclaration",
    namespace_export => "NamespaceExport",
    namespace_import => "NamespaceImport",
    named_imports => "NamedImports",
    expression_statement => "ExpressionStatement",
    call_expression => "CallExpression",
    super_keyword => "SuperKeyword",
    this_keyword => "ThisKeyword",
    object_literal_expression => "ObjectLiteralExpression",
    property_assignment => "PropertyAssignment",
    property_access_expression => "PropertyAccessExpression",
    type_reference => "TypeReference",
    import_type => "ImportType",
    union_type => "UnionType",
    intersection_type => "IntersectionType",
    undefined_keyword => "UndefinedKeyword",
    parenthesized_type => "ParenthesizedType",
    literal_type => "LiteralType",
    array_type => "ArrayType",
    tuple_type => "TupleType",
    named_tuple_member => "NamedTupleMember",
    optional_type => "OptionalType",
    rest_type => "RestType",
    type_literal => "TypeLiteral",
    function_type => "FunctionType",
    constructor_type => "ConstructorType",
    indexed_access_type => "IndexedAccessType",
    type_operator => "TypeOperator",
    conditional_type => "ConditionalType",
    infer_type => "InferType",
    mapped_type => "MappedType",
    template_literal_type => "TemplateLiteralType",
    type_query => "TypeQuery",
    type_predicate => "TypePredicate",
    this_type => "ThisType",
    true_keyword => "TrueKeyword",
    false_keyword => "FalseKeyword",
    null_keyword => "NullKeyword",
    prefix_unary_expression => "PrefixUnaryExpression",
    plus_token => "PlusToken",
    minus_token => "MinusToken",
    comma_token => "CommaToken",
    close_paren_token => "CloseParenToken",
    object_binding_pattern => "ObjectBindingPattern",
    type_parameter => "TypeParameter",
    any_keyword => "AnyKeyword",
    bigint_keyword => "BigIntKeyword",
    boolean_keyword => "BooleanKeyword",
    never_keyword => "NeverKeyword",
    number_keyword => "NumberKeyword",
    object_keyword => "ObjectKeyword",
    string_keyword => "StringKeyword",
    symbol_keyword => "SymbolKeyword",
    unknown_keyword => "UnknownKeyword",
    void_keyword => "VoidKeyword",
}

/// Compiler namespace plus captured constants for one analysis run.
pub(crate) struct Syntax<'s> {
    pub(crate) ts: Val<'s>,
    pub(crate) kinds: Kinds,
    pub(crate) constants: Constants,
    printer: Val<'s>,
    comment_printer: Val<'s>,
}

impl<'s> Syntax<'s> {
    pub(crate) fn new(
        scope: &mut Scope<'s, '_>,
        ts: Val<'s>,
        constants: &Constants,
    ) -> Result<Self> {
        let printer = js::call(scope, ts, "createPrinter", &[])?;
        let remove_comments = js::boolean(scope, true);
        let options = js::object(scope, &[("removeComments", remove_comments)])?;
        let comment_printer = js::call(scope, ts, "createPrinter", &[options])?;
        Ok(Self {
            ts,
            kinds: Kinds::capture(constants),
            constants: constants.clone(),
            printer,
            comment_printer,
        })
    }

    pub(crate) fn kind(&self, scope: &mut Scope<'s, '_>, node: Val<'s>) -> Result<u32> {
        Ok(js::integer(js::get_number(scope, node, "kind")?))
    }

    pub(crate) fn is(&self, scope: &mut Scope<'s, '_>, node: Val<'s>, kind: u32) -> Result<bool> {
        Ok(!node.is_null_or_undefined() && self.kind(scope, node)? == kind)
    }

    /// Calls one of the compiler's `isX` node predicates.
    pub(crate) fn predicate(
        &self,
        scope: &mut Scope<'s, '_>,
        name: &str,
        node: Val<'s>,
    ) -> Result<bool> {
        let value = js::call(scope, self.ts, name, &[node])?;
        Ok(value.boolean_value(scope))
    }

    pub(crate) fn create_source_file(
        &self,
        scope: &mut Scope<'s, '_>,
        file: &str,
        source: &str,
    ) -> Result<Val<'s>> {
        let file = js::string(scope, file);
        let source = js::string(scope, source);
        let latest = js::number(
            scope,
            f64::from(self.constants.value("ScriptTarget.Latest")),
        );
        let parents = js::boolean(scope, true);
        js::call(
            scope,
            self.ts,
            "createSourceFile",
            &[file, source, latest, parents],
        )
    }

    pub(crate) fn type_declaration_kind(
        &self,
        scope: &mut Scope<'s, '_>,
        node: Val<'s>,
    ) -> Result<Option<DeclarationKind>> {
        if node.is_null_or_undefined() {
            return Ok(None);
        }
        let kind = self.kind(scope, node)?;
        Ok(if kind == self.kinds.class_declaration {
            Some(DeclarationKind::Class)
        } else if kind == self.kinds.interface_declaration {
            Some(DeclarationKind::Interface)
        } else if kind == self.kinds.type_alias_declaration {
            Some(DeclarationKind::Alias)
        } else if kind == self.kinds.enum_declaration {
            Some(DeclarationKind::Enum)
        } else {
            None
        })
    }

    pub(crate) fn is_type_declaration(
        &self,
        scope: &mut Scope<'s, '_>,
        node: Val<'s>,
    ) -> Result<bool> {
        Ok(self.type_declaration_kind(scope, node)?.is_some())
    }

    pub(crate) fn modifiers(
        &self,
        scope: &mut Scope<'s, '_>,
        node: Val<'s>,
    ) -> Result<Vec<Val<'s>>> {
        if !self.predicate(scope, "canHaveModifiers", node)? {
            return Ok(Vec::new());
        }
        let modifiers = js::call(scope, self.ts, "getModifiers", &[node])?;
        if modifiers.is_null_or_undefined() {
            return Ok(Vec::new());
        }
        js::items(scope, modifiers)
    }

    pub(crate) fn has_modifier(
        &self,
        scope: &mut Scope<'s, '_>,
        node: Val<'s>,
        kind: u32,
    ) -> Result<bool> {
        for modifier in self.modifiers(scope, node)? {
            if self.kind(scope, modifier)? == kind {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(crate) fn decorators(
        &self,
        scope: &mut Scope<'s, '_>,
        node: Val<'s>,
    ) -> Result<Vec<Val<'s>>> {
        if !self.predicate(scope, "canHaveDecorators", node)? {
            return Ok(Vec::new());
        }
        let decorators = js::call(scope, self.ts, "getDecorators", &[node])?;
        if decorators.is_null_or_undefined() {
            return Ok(Vec::new());
        }
        js::items(scope, decorators)
    }

    pub(crate) fn visibility_of(
        &self,
        scope: &mut Scope<'s, '_>,
        node: Val<'s>,
    ) -> Result<MemberVisibility> {
        if let Some(name) = js::get_defined(scope, node, "name")?
            && self.is(scope, name, self.kinds.private_identifier)?
        {
            return Ok(MemberVisibility::Private);
        }
        if self.has_modifier(scope, node, self.kinds.private_keyword)? {
            return Ok(MemberVisibility::Private);
        }
        if self.has_modifier(scope, node, self.kinds.protected_keyword)? {
            return Ok(MemberVisibility::Protected);
        }
        Ok(MemberVisibility::Public)
    }

    pub(crate) fn source_file_of(
        &self,
        scope: &mut Scope<'s, '_>,
        node: Val<'s>,
    ) -> Result<Val<'s>> {
        js::call(scope, node, "getSourceFile", &[])
    }

    pub(crate) fn file_name_of(&self, scope: &mut Scope<'s, '_>, node: Val<'s>) -> Result<String> {
        let source_file = self.source_file_of(scope, node)?;
        js::get_string(scope, source_file, "fileName")
    }

    pub(crate) fn node_text(&self, scope: &mut Scope<'s, '_>, node: Val<'s>) -> Result<String> {
        let text = js::call(scope, node, "getText", &[])?;
        Ok(js::text(scope, text))
    }

    pub(crate) fn node_text_in(
        &self,
        scope: &mut Scope<'s, '_>,
        node: Val<'s>,
        source_file: Val<'s>,
    ) -> Result<String> {
        let text = js::call(scope, node, "getText", &[source_file])?;
        Ok(js::text(scope, text))
    }

    pub(crate) fn node_start(
        &self,
        scope: &mut Scope<'s, '_>,
        node: Val<'s>,
        source_file: Option<Val<'s>>,
    ) -> Result<f64> {
        let arguments = source_file.map_or_else(Vec::new, |file| vec![file]);
        let start = js::call(scope, node, "getStart", &arguments)?;
        start
            .number_value(scope)
            .ok_or_else(|| js::failure("node start is not numeric"))
    }

    /// Line and column of one position within a source file.
    pub(crate) fn line_and_character(
        &self,
        scope: &mut Scope<'s, '_>,
        source_file: Val<'s>,
        position: f64,
    ) -> Result<(usize, usize)> {
        let position = js::number(scope, position);
        let result = js::call(
            scope,
            source_file,
            "getLineAndCharacterOfPosition",
            &[position],
        )?;
        Ok((
            js::offset(js::get_number(scope, result, "line")?),
            js::offset(js::get_number(scope, result, "character")?),
        ))
    }

    pub(crate) fn member_name(&self, scope: &mut Scope<'s, '_>, name: Val<'s>) -> Result<String> {
        let kind = self.kind(scope, name)?;
        if kind == self.kinds.identifier
            || kind == self.kinds.private_identifier
            || kind == self.kinds.string_literal
            || kind == self.kinds.numeric_literal
            || kind == self.kinds.no_substitution_template_literal
        {
            return js::get_string(scope, name, "text");
        }
        if kind == self.kinds.computed_property_name {
            let expression = js::get(scope, name, "expression")?;
            return Ok(format!("[{}]", self.node_text(scope, expression)?));
        }
        self.node_text(scope, name)
    }

    pub(crate) fn string_literal_value(
        &self,
        scope: &mut Scope<'s, '_>,
        node: Option<Val<'s>>,
    ) -> Result<Option<String>> {
        let Some(node) = node.filter(|node| !node.is_null_or_undefined()) else {
            return Ok(None);
        };
        let kind = self.kind(scope, node)?;
        if kind == self.kinds.string_literal || kind == self.kinds.no_substitution_template_literal
        {
            return Ok(Some(js::get_string(scope, node, "text")?));
        }
        Ok(None)
    }

    pub(crate) fn expression_name(
        &self,
        scope: &mut Scope<'s, '_>,
        node: Val<'s>,
    ) -> Result<Option<String>> {
        let kind = self.kind(scope, node)?;
        if kind == self.kinds.identifier {
            return Ok(Some(js::get_string(scope, node, "text")?));
        }
        if kind == self.kinds.property_access_expression {
            let name = js::get(scope, node, "name")?;
            return Ok(Some(js::get_string(scope, name, "text")?));
        }
        Ok(None)
    }

    pub(crate) fn keyword_name(&self, kind: u32) -> Option<KeywordTypeName> {
        let kinds = &self.kinds;
        Some(if kind == kinds.any_keyword {
            KeywordTypeName::Any
        } else if kind == kinds.bigint_keyword {
            KeywordTypeName::Bigint
        } else if kind == kinds.boolean_keyword {
            KeywordTypeName::Boolean
        } else if kind == kinds.never_keyword {
            KeywordTypeName::Never
        } else if kind == kinds.number_keyword {
            KeywordTypeName::Number
        } else if kind == kinds.object_keyword {
            KeywordTypeName::Object
        } else if kind == kinds.string_keyword {
            KeywordTypeName::String
        } else if kind == kinds.symbol_keyword {
            KeywordTypeName::Symbol
        } else if kind == kinds.undefined_keyword {
            KeywordTypeName::Undefined
        } else if kind == kinds.unknown_keyword {
            KeywordTypeName::Unknown
        } else if kind == kinds.void_keyword {
            KeywordTypeName::Void
        } else {
            return None;
        })
    }

    pub(crate) fn literal_model(
        &self,
        scope: &mut Scope<'s, '_>,
        node: Val<'s>,
    ) -> Result<TypeNodeKind> {
        let literal = js::get(scope, node, "literal")?;
        let kind = self.kind(scope, literal)?;
        let text = self.node_text(scope, literal)?;
        let kinds = &self.kinds;
        let value =
            if kind == kinds.string_literal || kind == kinds.no_substitution_template_literal {
                LiteralValue::String(js::get_string(scope, literal, "text")?)
            } else if kind == kinds.numeric_literal {
                let raw = js::get_string(scope, literal, "text")?;
                number_value(jstext::parse_number(&raw))
            } else if kind == kinds.bigint_literal {
                let raw = js::get_string(scope, literal, "text")?;
                LiteralValue::BigInt {
                    digits: jstext::bigint_decimal(raw.strip_suffix('n').unwrap_or(&raw)),
                }
            } else if kind == kinds.true_keyword {
                return Ok(TypeNodeKind::Literal {
                    value: LiteralValue::Boolean(true),
                    text: "true".to_owned(),
                });
            } else if kind == kinds.false_keyword {
                return Ok(TypeNodeKind::Literal {
                    value: LiteralValue::Boolean(false),
                    text: "false".to_owned(),
                });
            } else if kind == kinds.null_keyword {
                return Ok(TypeNodeKind::Literal {
                    value: LiteralValue::Null,
                    text: "null".to_owned(),
                });
            } else if kind == kinds.prefix_unary_expression {
                let operand = js::get(scope, literal, "operand")?;
                let operand_kind = self.kind(scope, operand)?;
                if operand_kind == kinds.bigint_literal {
                    LiteralValue::BigInt {
                        digits: jstext::bigint_decimal(text.strip_suffix('n').unwrap_or(&text)),
                    }
                } else if operand_kind == kinds.numeric_literal {
                    number_value(jstext::parse_number(&text))
                } else {
                    return Err(TypertGeneratorError::Analysis(format!(
                        "typert: unsupported literal type {text}"
                    )));
                }
            } else {
                return Err(TypertGeneratorError::Analysis(format!(
                    "typert: unsupported literal type {text}"
                )));
            };
        Ok(TypeNodeKind::Literal { value, text })
    }

    pub(crate) fn modifier_mode(
        &self,
        scope: &mut Scope<'s, '_>,
        token: Option<Val<'s>>,
    ) -> Result<MappedModifier> {
        let Some(token) = token.filter(|token| !token.is_null_or_undefined()) else {
            return Ok(MappedModifier::Preserve);
        };
        let kind = self.kind(scope, token)?;
        Ok(if kind == self.kinds.plus_token {
            MappedModifier::Add
        } else if kind == self.kinds.minus_token {
            MappedModifier::Remove
        } else {
            MappedModifier::Add
        })
    }

    /// `ts.getJSDocTags(node)` with tag name, argument, comment, and text.
    pub(crate) fn jsdoc_tags(
        &self,
        scope: &mut Scope<'s, '_>,
        node: Val<'s>,
    ) -> Result<Vec<(Val<'s>, String, Option<String>)>> {
        let tags = js::call(scope, self.ts, "getJSDocTags", &[node])?;
        let mut result = Vec::new();
        for tag in js::items(scope, tags)? {
            let tag_name = js::get(scope, tag, "tagName")?;
            let name = js::get_string(scope, tag_name, "text")?;
            let comment = js::get(scope, tag, "comment")?;
            let comment_text = js::call(scope, self.ts, "getTextOfJSDocComment", &[comment])?;
            let comment_text = if comment_text.is_null_or_undefined() {
                None
            } else {
                Some(js::text(scope, comment_text))
            };
            result.push((tag, name, comment_text));
        }
        Ok(result)
    }

    pub(crate) fn documentation_of(
        &self,
        scope: &mut Scope<'s, '_>,
        node: Val<'s>,
    ) -> Result<DocumentationModel> {
        let comments = js::call(scope, self.ts, "getJSDocCommentsAndTags", &[node])?;
        let mut block = None;
        for candidate in js::items(scope, comments)? {
            if self.predicate(scope, "isJSDoc", candidate)? {
                block = Some(candidate);
            }
        }
        let Some(block) = block else {
            return Ok(DocumentationModel::default());
        };
        let comment = js::get(scope, block, "comment")?;
        let comment_text = js::call(scope, self.ts, "getTextOfJSDocComment", &[comment])?;
        let description = if comment_text.is_null_or_undefined() {
            None
        } else {
            jstext::normalized_doc_text(Some(&js::text(scope, comment_text)))
        };
        let mut tags = Vec::new();
        for (tag, name, comment) in self.jsdoc_tags(scope, node)? {
            let argument = match js::get_defined(scope, tag, "name")? {
                Some(argument) => Some(self.node_text(scope, argument)?),
                None => None,
            };
            let source_file = self.source_file_of(scope, tag)?;
            let text = self.node_text_in(scope, tag, source_file)?;
            tags.push(JsDocTagModel {
                name,
                argument,
                comment: jstext::normalized_doc_text(comment.as_deref()),
                text: jstext::trim(&text).to_owned(),
            });
        }
        let summary = description.as_deref().map(jstext::first_sentence);
        Ok(DocumentationModel {
            description,
            summary,
            tags,
            js_doc: Some(self.raw_jsdoc(scope, node)?),
        })
    }

    fn raw_jsdoc(&self, scope: &mut Scope<'s, '_>, node: Val<'s>) -> Result<String> {
        let source_file = self.source_file_of(scope, node)?;
        let source = js::call(scope, source_file, "getFullText", &[])?;
        let full_start = js::call(scope, node, "getFullStart", &[])?;
        let ranges = js::call(
            scope,
            self.ts,
            "getLeadingCommentRanges",
            &[source, full_start],
        )?;
        let mut selected = None;
        for range in js::items(scope, ranges)? {
            let pos = js::get(scope, range, "pos")?;
            let pos_number = js::get_number(scope, range, "pos")?;
            let end = js::number(scope, pos_number + 3.0);
            let head = js::call(scope, source, "slice", &[pos, end])?;
            if js::text(scope, head) == "/**" {
                selected = Some(range);
            }
        }
        let range = selected.ok_or_else(|| js::failure("documented node has no JSDoc range"))?;
        let pos = js::get(scope, range, "pos")?;
        let end = js::get(scope, range, "end")?;
        let raw = js::call(scope, source, "slice", &[pos, end])?;
        let raw = js::text(scope, raw);
        let range_pos = js::get_number(scope, range, "pos")?;
        let (line, _) = self.line_and_character(scope, source_file, range_pos)?;
        let line_value = js::number(scope, u32::try_from(line).map_or(f64::NAN, f64::from));
        let zero = js::number(scope, 0.0);
        let line_start = js::call(
            scope,
            source_file,
            "getPositionOfLineAndCharacter",
            &[line_value, zero],
        )?;
        let indent = js::call(scope, source, "slice", &[line_start, pos])?;
        let indent = js::text(scope, indent);
        Ok(raw
            .split('\n')
            .enumerate()
            .map(|(index, text)| {
                if index > 0
                    && let Some(stripped) = text.strip_prefix(indent.as_str())
                {
                    stripped
                } else {
                    text
                }
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }

    pub(crate) fn typert_mode(
        &self,
        scope: &mut Scope<'s, '_>,
        node: Val<'s>,
    ) -> Result<Option<&'static str>> {
        for (_, name, comment) in self.jsdoc_tags(scope, node)? {
            if name != "typert" {
                continue;
            }
            let mode = jstext::first_word(comment.as_deref().unwrap_or(""));
            if mode == "object" {
                return Ok(Some("object"));
            }
            if mode.is_empty() || mode == "schema" || mode == "type" {
                return Ok(Some("schema"));
            }
        }
        Ok(None)
    }

    pub(crate) fn typert_service_tag(
        &self,
        scope: &mut Scope<'s, '_>,
        node: Val<'s>,
    ) -> Result<Option<(Val<'s>, String)>> {
        for (tag, name, comment) in self.jsdoc_tags(scope, node)? {
            let comment = comment.unwrap_or_default();
            if name == "typert" && jstext::first_word(&comment) == "service" {
                return Ok(Some((tag, comment)));
            }
        }
        Ok(None)
    }

    /// Body-free source text of one member.
    pub(crate) fn member_text(&self, scope: &mut Scope<'s, '_>, member: Val<'s>) -> Result<String> {
        let source_file = self.source_file_of(scope, member)?;
        let full = self.node_text_in(scope, member, source_file)?;
        let signature = match js::get_defined(scope, member, "body")? {
            Some(body) => {
                let body_text = self.node_text_in(scope, body, source_file)?;
                let keep =
                    jstext::utf16_length(&full).saturating_sub(jstext::utf16_length(&body_text));
                jstext::utf16_slice(&full, 0, keep)
            }
            None => full,
        };
        Ok(
            jstext::trim(&jstext::collapse_spaces(jstext::strip_member_tail(
                &signature,
            )))
            .to_owned(),
        )
    }

    /// Canonical body-free declaration text.
    pub(crate) fn declaration_text(
        &self,
        scope: &mut Scope<'s, '_>,
        declaration: Val<'s>,
    ) -> Result<String> {
        let projected = if self.is(scope, declaration, self.kinds.class_declaration)? {
            self.class_shape(scope, declaration)?
        } else {
            declaration
        };
        let source_file = self.source_file_of(scope, declaration)?;
        let hint = js::number(
            scope,
            f64::from(self.constants.value("EmitHint.Unspecified")),
        );
        let printed = js::call(
            scope,
            self.comment_printer,
            "printNode",
            &[hint, projected, source_file],
        )?;
        Ok(js::text(scope, printed).replace('\r', ""))
    }

    /// Prints one node with the default printer.
    pub(crate) fn print_node(
        &self,
        scope: &mut Scope<'s, '_>,
        node: Val<'s>,
        source_file: Val<'s>,
    ) -> Result<String> {
        let hint = js::number(
            scope,
            f64::from(self.constants.value("EmitHint.Unspecified")),
        );
        let printed = js::call(scope, self.printer, "printNode", &[hint, node, source_file])?;
        Ok(js::text(scope, printed))
    }

    fn class_shape(&self, scope: &mut Scope<'s, '_>, node: Val<'s>) -> Result<Val<'s>> {
        let factory = js::get(scope, self.ts, "factory")?;
        let undefined = js::undefined(scope);
        let mut members = Vec::new();
        for member in js::get_items(scope, node, "members")? {
            let non_public = self.has_modifier(scope, member, self.kinds.private_keyword)?
                || self.has_modifier(scope, member, self.kinds.protected_keyword)?;
            let kind = self.kind(scope, member)?;
            if non_public
                || (kind == self.kinds.property_declaration && {
                    let name = js::get(scope, member, "name")?;
                    self.is(scope, name, self.kinds.private_identifier)?
                })
            {
                continue;
            }
            let read = |scope: &mut Scope<'s, '_>, name: &str| js::get(scope, member, name);
            let projected = if kind == self.kinds.method_declaration {
                let arguments = [
                    member,
                    read(scope, "modifiers")?,
                    read(scope, "asteriskToken")?,
                    read(scope, "name")?,
                    read(scope, "questionToken")?,
                    read(scope, "typeParameters")?,
                    read(scope, "parameters")?,
                    read(scope, "type")?,
                    undefined,
                ];
                js::call(scope, factory, "updateMethodDeclaration", &arguments)?
            } else if kind == self.kinds.constructor {
                let arguments = [
                    member,
                    read(scope, "modifiers")?,
                    read(scope, "parameters")?,
                    undefined,
                ];
                js::call(scope, factory, "updateConstructorDeclaration", &arguments)?
            } else if kind == self.kinds.get_accessor {
                let arguments = [
                    member,
                    read(scope, "modifiers")?,
                    read(scope, "name")?,
                    read(scope, "parameters")?,
                    read(scope, "type")?,
                    undefined,
                ];
                js::call(scope, factory, "updateGetAccessorDeclaration", &arguments)?
            } else if kind == self.kinds.set_accessor {
                let arguments = [
                    member,
                    read(scope, "modifiers")?,
                    read(scope, "name")?,
                    read(scope, "parameters")?,
                    undefined,
                ];
                js::call(scope, factory, "updateSetAccessorDeclaration", &arguments)?
            } else if kind == self.kinds.property_declaration {
                let question = read(scope, "questionToken")?;
                let token = if question.is_null_or_undefined() {
                    read(scope, "exclamationToken")?
                } else {
                    question
                };
                let arguments = [
                    member,
                    read(scope, "modifiers")?,
                    read(scope, "name")?,
                    token,
                    read(scope, "type")?,
                    undefined,
                ];
                js::call(scope, factory, "updatePropertyDeclaration", &arguments)?
            } else {
                member
            };
            members.push(projected);
        }
        let members = js::array(scope, &members);
        let arguments = [
            node,
            js::get(scope, node, "modifiers")?,
            js::get(scope, node, "name")?,
            js::get(scope, node, "typeParameters")?,
            js::get(scope, node, "heritageClauses")?,
            members,
        ];
        js::call(scope, factory, "updateClassDeclaration", &arguments)
    }

    /// The declaration the source prefers for symbol identity.
    pub(crate) fn preferred_declaration(
        &self,
        scope: &mut Scope<'s, '_>,
        symbol: Val<'s>,
    ) -> Result<Option<Val<'s>>> {
        let declarations = js::get_items(scope, symbol, "declarations")?;
        for declaration in &declarations {
            if self.is_type_declaration(scope, *declaration)? {
                return Ok(Some(*declaration));
            }
        }
        if let Some(value) = js::get_defined(scope, symbol, "valueDeclaration")? {
            return Ok(Some(value));
        }
        Ok(declarations.first().copied())
    }

    /// Whether a `declare module` statement targets one of the accepted module names.
    pub(crate) fn module_named(
        &self,
        scope: &mut Scope<'s, '_>,
        statement: Val<'s>,
        names: &[&str],
    ) -> Result<Option<Val<'s>>> {
        if !self.is(scope, statement, self.kinds.module_declaration)? {
            return Ok(None);
        }
        let name = js::get(scope, statement, "name")?;
        if !self.is(scope, name, self.kinds.string_literal)? {
            return Ok(None);
        }
        let text = js::get_string(scope, name, "text")?;
        if !names.contains(&text.as_str()) {
            return Ok(None);
        }
        let Some(body) = js::get_defined(scope, statement, "body")? else {
            return Ok(None);
        };
        if !self.is(scope, body, self.kinds.module_block)? {
            return Ok(None);
        }
        Ok(Some(body))
    }

    pub(crate) fn location(
        &self,
        scope: &mut Scope<'s, '_>,
        root: &Path,
        node: Val<'s>,
    ) -> Result<crate::model::SourceLocation> {
        let source_file = self.source_file_of(scope, node)?;
        let start = self.node_start(scope, node, Some(source_file))?;
        let (line, character) = self.line_and_character(scope, source_file, start)?;
        let file_name = js::get_string(scope, source_file, "fileName")?;
        Ok(crate::model::SourceLocation {
            file: paths::relative(root, Path::new(&file_name)),
            line: line + 1,
            column: character + 1,
        })
    }
}

fn number_value(value: f64) -> LiteralValue {
    let text = jstext::number_text(value);
    text.parse::<serde_json::Number>()
        .map_or(LiteralValue::String(text), LiteralValue::Number)
}

/// Whether one parsed file carries a Typert or Cordis surface.
pub(crate) fn source_file_has_surface<'s>(
    scope: &mut Scope<'s, '_>,
    syntax: &Syntax<'s>,
    source_file: Val<'s>,
) -> Result<bool> {
    let kinds = &syntax.kinds;
    for statement in js::get_items(scope, source_file, "statements")? {
        if syntax.is_type_declaration(scope, statement)?
            && (syntax.typert_mode(scope, statement)?.is_some()
                || syntax.typert_service_tag(scope, statement)?.is_some())
        {
            return Ok(true);
        }
        if syntax.is(scope, statement, kinds.class_declaration)? {
            for member in js::get_items(scope, statement, "members")? {
                if syntax.is(scope, member, kinds.property_declaration)? {
                    let name = js::get(scope, member, "name")?;
                    if syntax.member_name(scope, name)? == "typertRemote"
                        && let Some(initializer) = js::get_defined(scope, member, "initializer")?
                        && syntax.is(scope, initializer, kinds.call_expression)?
                    {
                        let expression = js::get(scope, initializer, "expression")?;
                        if syntax.expression_name(scope, expression)?.as_deref()
                            == Some("bindTypertRemote")
                        {
                            return Ok(true);
                        }
                    }
                }
                for decorator in syntax.decorators(scope, member)? {
                    let expression = js::get(scope, decorator, "expression")?;
                    let expression = if syntax.is(scope, expression, kinds.call_expression)? {
                        js::get(scope, expression, "expression")?
                    } else {
                        expression
                    };
                    let name = syntax.expression_name(scope, expression)?;
                    if matches!(name.as_deref(), Some("Remote" | "RemoteScope")) {
                        return Ok(true);
                    }
                }
            }
        }
        let Some(body) = syntax.module_named(scope, statement, CORDIS_MODULES)? else {
            continue;
        };
        for member in js::get_items(scope, body, "statements")? {
            if syntax.is(scope, member, kinds.interface_declaration)? {
                let name = js::get(scope, member, "name")?;
                let name = js::get_string(scope, name, "text")?;
                if (name == "Context" || name == "Events")
                    && !js::get_items(scope, member, "members")?.is_empty()
                {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}
