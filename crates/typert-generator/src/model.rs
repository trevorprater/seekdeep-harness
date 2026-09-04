//! Authored type structure and independent package-face models.

use std::{borrow::Borrow, fmt};

use serde::{Deserialize, Serialize};
use serde_json::Value;

macro_rules! spelling_enum {
    ($name:ident, $description:literal, $( $variant:ident => $spelling:literal ),+ $(,)?) => {
        #[doc = $description]
        #[derive(Clone, Debug, PartialEq, Eq)]
        pub enum $name {
            $(#[doc = $spelling] $variant,)+
            /// Unrecognized spelling retained for downstream diagnostics.
            Other(String),
        }
        impl $name {
            /// Authored source spelling.
            pub fn as_str(&self) -> &str {
                match self { $(Self::$variant => $spelling,)+ Self::Other(value) => value }
            }
        }
        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                Ok(match value.as_str() { $($spelling => Self::$variant,)+ _ => Self::Other(value) })
            }
        }
    };
}

spelling_enum!(KeywordTypeName, "TypeScript keyword types.",
    Any => "any", Bigint => "bigint", Boolean => "boolean", Never => "never",
    Number => "number", Object => "object", String => "string", Symbol => "symbol",
    Undefined => "undefined", Unknown => "unknown", Void => "void",
);
spelling_enum!(TypeOperatorName, "Prefix TypeScript type operators.",
    Keyof => "keyof", Readonly => "readonly", Unique => "unique",
);

macro_rules! identifier {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(
            Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(String);
        impl $name {
            /// Borrows the exact model identity.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }
        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }
        impl Borrow<str> for $name {
            fn borrow(&self) -> &str {
                self.as_str()
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

identifier!(TypeNodeId, "Graph-local identity of one type expression.");
identifier!(SymbolId, "Workspace identity of one declared symbol.");
identifier!(
    TypeSymbolId,
    "Public type identity carried by a Remote codec."
);
identifier!(MemberId, "Stable identity of one declared member.");
identifier!(
    TypeParameterId,
    "Identity of one generic parameter, independent of its spelling."
);
identifier!(
    InvocationId,
    "Stable public identity of one Remote invocation."
);

/// Independently compiled workspace face.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TypertFace {
    /// Native Host side.
    Host,
    /// Browser Client side.
    Client,
}

impl TypertFace {
    /// Stable source spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Client => "client",
        }
    }
}

/// Source coordinate, with one-based line and column.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceLocation {
    /// Workspace-relative file.
    pub file: String,
    /// One-based line.
    pub line: usize,
    /// One-based column.
    pub column: usize,
}

/// One structured documentation tag, including unrecognized tags.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsDocTagModel {
    /// Tag name.
    pub name: String,
    /// Optional first argument.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub argument: Option<String>,
    /// Remaining comment text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// Complete original tag text.
    pub text: String,
}

/// Documentation retained independently of compiler objects.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentationModel {
    /// Complete description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Brief summary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Ordered structured tags.
    pub tags: Vec<JsDocTagModel>,
    /// Original documentation block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub js_doc: Option<String>,
}

/// A package export and its resolved declaration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportModel {
    /// Public export subpath.
    pub subpath: String,
    /// Public export name.
    pub name: String,
    /// Resolved workspace declaration.
    pub symbol: SymbolId,
    /// Ordered source aliases.
    pub aliases: Vec<String>,
}

/// One Cordis service contribution.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServiceModel {
    /// Source documentation.
    #[serde(flatten)]
    pub documentation: DocumentationModel,
    /// Cordis service key.
    pub key: String,
    /// Service declaration.
    pub symbol: SymbolId,
    /// Public export.
    pub export: ExportModel,
    /// Selected public instance members.
    pub members: Vec<MemberId>,
    /// Source coordinate.
    pub location: SourceLocation,
}

/// One Cordis event contribution.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventModel {
    /// Source documentation.
    #[serde(flatten)]
    pub documentation: DocumentationModel,
    /// Event name.
    pub name: String,
    /// Function type node.
    pub signature: TypeNodeId,
    /// Body-free authored declaration.
    pub text: String,
    /// Optional dispatch mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Source coordinate.
    pub location: SourceLocation,
}

/// Reference-passing marker for exported objects.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ObjectPassing {
    /// Preserve object identity across the Remote boundary.
    Reference,
}

/// One explicitly selected reference object.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ObjectModel {
    /// Source documentation.
    #[serde(flatten)]
    pub documentation: DocumentationModel,
    /// Public export.
    pub export: ExportModel,
    /// Object declaration.
    pub symbol: SymbolId,
    /// Passing policy.
    pub passing: ObjectPassing,
}

/// One explicitly selected schema root.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SchemaModel {
    /// Source documentation.
    #[serde(flatten)]
    pub documentation: DocumentationModel,
    /// Public export.
    pub export: ExportModel,
    /// Root declaration.
    pub symbol: SymbolId,
    /// Selected type expression.
    #[serde(rename = "type")]
    pub ty: TypeNodeId,
}

/// A business-type import needed by a generated Remote declaration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteTypeImportModel {
    /// Workspace declaration identity.
    pub symbol: SymbolId,
    /// Public module specifier.
    pub specifier: String,
    /// Public exported name.
    pub name: String,
}

/// Authored type and checker-resolved runtime codec projection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteBoundaryModel {
    /// Authored public type.
    #[serde(rename = "type")]
    pub ty: TypeNodeId,
    /// Resolved codec type.
    pub codec_type: TypeNodeId,
    /// Explicit top-level absence acceptance.
    pub accepts_undefined: bool,
    /// Stable public type symbol text.
    pub type_symbol: TypeSymbolId,
    /// Public imports needed to name the type.
    pub imports: Vec<RemoteTypeImportModel>,
}

/// Remote argument origin.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InvocationParameterSource {
    /// Value decoded from JSON.
    Json,
    /// Host object resolved by a lookup provider.
    Lookup,
}

/// One ordered Remote business parameter.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InvocationParameterModel {
    /// Authored parameter name.
    pub name: String,
    /// Wire field name.
    pub wire: String,
    /// JSON or object-lookup source.
    pub source: InvocationParameterSource,
    /// Lookup-provider key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lookup: Option<String>,
    /// Whether consumers may omit the wire field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub optional: Option<bool>,
    /// Authored and resolved boundary.
    pub boundary: RemoteBoundaryModel,
}

/// Direct or Context-resolved Host invocation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum InvocationTarget {
    /// Invoke the registered service directly.
    Direct,
    /// Resolve an invocation Context from the wire identity.
    Context {
        /// Context-provider key.
        context: String,
        /// Context identity wire field.
        wire: String,
        /// Strict Context identity boundary.
        boundary: RemoteBoundaryModel,
    },
}

/// Optional scope projection of a direct invocation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvocationScope {
    /// Context-provider key.
    pub context: String,
    /// Identity field supplied by the scope.
    pub wire: String,
}

/// Cancellation parameter name supported by Remote generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CancellationParameter {
    /// Carrier cancellation signal.
    Signal,
}

/// Carrier cancellation metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvocationCancellation {
    /// Parameter receiving cancellation.
    pub parameter: CancellationParameter,
}

/// One strictly analyzed Host method.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InvocationModel {
    /// Public invocation identity.
    pub id: InvocationId,
    /// Cordis service key.
    pub service: String,
    /// Remote namespace.
    pub namespace: String,
    /// Public method name.
    pub method: String,
    /// Optional implementation-member override.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub implementation: Option<String>,
    /// Host invocation selection.
    pub invocation: InvocationTarget,
    /// Optional scoped-call projection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<InvocationScope>,
    /// Ordered business arguments.
    pub parameters: Vec<InvocationParameterModel>,
    /// Optional carrier cancellation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancellation: Option<InvocationCancellation>,
    /// Result boundary.
    pub result: RemoteBoundaryModel,
    /// Method source coordinate.
    pub location: SourceLocation,
}

/// Business semantics of one package on one face.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PackageModel {
    /// Package name.
    pub name: String,
    /// Workspace-relative root.
    pub root: String,
    /// Public exports.
    pub exports: Vec<ExportModel>,
    /// Cordis service contributions.
    pub services: Vec<ServiceModel>,
    /// Cordis event contributions.
    pub events: Vec<EventModel>,
    /// Reference objects.
    pub objects: Vec<ObjectModel>,
    /// Schema roots.
    pub schemas: Vec<SchemaModel>,
    /// Remote method contracts.
    pub invocations: Vec<InvocationModel>,
}

/// Explicit source import/re-export across independent faces.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrossFaceLink {
    /// Importing face.
    pub from_face: TypertFace,
    /// Importing package.
    pub from_package: String,
    /// Declaring face.
    pub to_face: TypertFace,
    /// Declaring package.
    pub to_package: String,
    /// Public export subpath.
    pub subpath: String,
    /// Public exported name.
    pub name: String,
}

/// Complete independently analyzed face.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FaceModel {
    /// Independently compiled side.
    pub face: TypertFace,
    /// Package models in source order.
    pub packages: Vec<PackageModel>,
    /// Authored type graph.
    pub graph: TypeGraph,
}

/// Host/Client models and their explicit cross-face relationships.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceModel {
    /// Independent faces.
    pub faces: Vec<FaceModel>,
    /// Explicit imports and re-exports.
    pub cross_face_links: Vec<CrossFaceLink>,
}

/// Authored declaration category.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeclarationKind {
    /// Interface, including modeled declaration merging.
    Interface,
    /// Class.
    Class,
    /// Type alias.
    Alias,
    /// Enum.
    Enum,
}

impl DeclarationKind {
    /// Source-model spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Interface => "interface",
            Self::Class => "class",
            Self::Alias => "alias",
            Self::Enum => "enum",
        }
    }
}

/// Indexed declaration without implicitly making it a graph root.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceDeclarationModel {
    /// Source face.
    pub face: TypertFace,
    /// Owning package.
    pub package: String,
    /// Declaration name.
    pub name: String,
    /// Declaration category.
    pub kind: DeclarationKind,
    /// Source coordinate.
    pub location: SourceLocation,
    /// Body-free authored declaration.
    pub text: String,
}

/// Generic parameter variance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Variance {
    /// Contravariant.
    In,
    /// Covariant.
    Out,
    /// Both variance markers.
    InOut,
}

/// Authored generic parameter with unevaluated constraint and default.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TypeParameterModel {
    /// Graph parameter identity.
    pub id: TypeParameterId,
    /// Source name.
    pub name: String,
    /// Const parameter modifier.
    #[serde(rename = "const")]
    pub is_const: bool,
    /// Authored constraint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub constraint: Option<TypeNodeId>,
    /// Authored default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<TypeNodeId>,
    /// Optional variance markers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variance: Option<Variance>,
}

/// Parameter binding-pattern category.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParameterBinding {
    /// Ordinary identifier.
    Identifier,
    /// Object destructuring.
    Object,
    /// Array destructuring.
    Array,
}

/// One function-like parameter.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParameterModel {
    /// Identifier or retained binding pattern.
    pub name: String,
    /// Binding-pattern category.
    pub binding: ParameterBinding,
    /// Parameter type.
    #[serde(rename = "type")]
    pub ty: TypeNodeId,
    /// Authored optional marker.
    pub optional: bool,
    /// Rest parameter marker.
    pub rest: bool,
    /// Explicit receiver parameter.
    pub receiver: bool,
    /// Authored default expression.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initializer: Option<String>,
}

/// Call, construct, accessor, or index signature.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignatureModel {
    /// Authored generic parameters.
    pub type_parameters: Vec<TypeParameterModel>,
    /// Ordered parameters.
    pub parameters: Vec<ParameterModel>,
    /// Return type.
    pub returns: TypeNodeId,
}

/// Authored member visibility.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemberVisibility {
    /// Public instance API.
    Public,
    /// Protected member.
    Protected,
    /// Private member.
    Private,
}

/// Non-literal computed member key category.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ComputedMember {
    /// Symbol key, absent from JSON schemas.
    Symbol,
    /// Dynamic key without a fixed JSON property name.
    Dynamic,
}

/// Shared authored member flags and documentation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Authored TypeScript modifiers are independent model flags"
)]
pub struct MemberBase {
    /// Source documentation.
    #[serde(flatten)]
    pub documentation: DocumentationModel,
    /// Stable member identity.
    pub id: MemberId,
    /// Authored name.
    pub name: String,
    /// JSON name of a literal computed key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json_name: Option<String>,
    /// Non-literal computed key category.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub computed: Option<ComputedMember>,
    /// Optional member marker.
    pub optional: bool,
    /// Readonly member marker.
    #[serde(rename = "readonly")]
    pub read_only: bool,
    /// Async member marker.
    #[serde(rename = "async")]
    pub is_async: bool,
    /// Abstract member marker.
    #[serde(rename = "abstract")]
    pub is_abstract: bool,
    /// Static member marker.
    #[serde(rename = "static")]
    pub is_static: bool,
    /// Authored visibility.
    pub visibility: MemberVisibility,
    /// Source coordinate.
    pub location: SourceLocation,
    /// Body-free source text.
    pub text: String,
}

/// Member-specific structure.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum MemberKind {
    /// Property type.
    Property {
        #[serde(rename = "type")]
        #[doc = "Property type."]
        ty: TypeNodeId,
    },
    /// Method signature.
    Method {
        #[doc = "Method signature."]
        signature: SignatureModel,
    },
    /// Getter signature.
    Getter {
        #[doc = "Getter signature."]
        signature: SignatureModel,
    },
    /// Setter signature.
    Setter {
        #[doc = "Setter signature."]
        signature: SignatureModel,
    },
    /// Callable object signature.
    Call {
        #[doc = "Call signature."]
        signature: SignatureModel,
    },
    /// Constructible object signature.
    Construct {
        #[doc = "Construct signature."]
        signature: SignatureModel,
    },
    /// Index signature.
    Index {
        #[doc = "Index signature."]
        signature: SignatureModel,
    },
}

impl MemberKind {
    /// Stable source discriminant.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Property { .. } => "property",
            Self::Method { .. } => "method",
            Self::Getter { .. } => "getter",
            Self::Setter { .. } => "setter",
            Self::Call { .. } => "call",
            Self::Construct { .. } => "construct",
            Self::Index { .. } => "index",
        }
    }
    /// Callable signature, absent for properties.
    pub const fn signature(&self) -> Option<&SignatureModel> {
        match self {
            Self::Property { .. } => None,
            Self::Method { signature }
            | Self::Getter { signature }
            | Self::Setter { signature }
            | Self::Call { signature }
            | Self::Construct { signature }
            | Self::Index { signature } => Some(signature),
        }
    }
}

/// One represented member.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DefinedMember {
    /// Shared source flags.
    #[serde(flatten)]
    pub base: MemberBase,
    /// Member-specific structure.
    #[serde(flatten)]
    pub kind: MemberKind,
}

/// A member or an unrecognized record retained for fail-loud diagnostics.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MemberModel {
    /// Complete represented member.
    Defined(Box<DefinedMember>),
    /// Unrecognized caller-supplied record; emitters must reject it.
    Unsupported(Value),
}

/// Enum member retaining its authored initializer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EnumMemberModel {
    /// Source documentation.
    #[serde(flatten)]
    pub documentation: DocumentationModel,
    /// Member name.
    pub name: String,
    /// Authored initializer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initializer: Option<String>,
    /// Source coordinate.
    pub location: SourceLocation,
}

/// One independently authored part of a merged interface.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeDeclarationPartModel {
    /// Source documentation.
    #[serde(flatten)]
    pub documentation: DocumentationModel,
    /// Owning package.
    pub package: String,
    /// Source coordinate.
    pub location: SourceLocation,
    /// Authored generic parameters.
    pub type_parameters: Vec<TypeParameterModel>,
    /// Explicit inheritance edges.
    pub extends: Vec<TypeNodeId>,
    /// Members owned by this part.
    pub members: Vec<MemberId>,
}

/// Complete authored class, interface, alias, or enum.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeDeclarationModel {
    /// Source documentation.
    #[serde(flatten)]
    pub documentation: DocumentationModel,
    /// Workspace declaration identity.
    pub id: SymbolId,
    /// Owning package.
    pub package: String,
    /// Authored name.
    pub name: String,
    /// Declaration category.
    pub kind: DeclarationKind,
    /// Abstract class marker.
    #[serde(rename = "abstract")]
    pub is_abstract: bool,
    /// Public export membership.
    pub exported: bool,
    /// Source coordinate.
    pub location: SourceLocation,
    /// Canonical body-free declaration.
    pub text: String,
    /// Authored generic parameters.
    pub type_parameters: Vec<TypeParameterModel>,
    /// Explicit inheritance edges.
    pub extends: Vec<TypeNodeId>,
    /// Explicit implementation edges.
    pub implements: Vec<TypeNodeId>,
    /// Authored members.
    pub members: Vec<MemberModel>,
    /// Parts of a merged interface.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parts: Option<Vec<TypeDeclarationPartModel>>,
    /// Alias expression.
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub ty: Option<TypeNodeId>,
    /// Authored enum entries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enum_members: Option<Vec<EnumMemberModel>>,
}

/// Target of an authored named type reference.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum TypeTargetModel {
    /// Local declaration.
    Declaration {
        #[doc = "Declaration identity."]
        symbol: SymbolId,
    },
    /// Generic parameter.
    TypeParameter {
        #[doc = "Parameter identity."]
        parameter: TypeParameterId,
    },
    /// Explicit import from another face.
    CrossFace {
        /// Declaring face.
        face: TypertFace,
        /// Public package.
        package: String,
        /// Public subpath.
        subpath: String,
        /// Public exported name.
        name: String,
    },
    /// External package whose declarations are not copied.
    External {
        /// Package name.
        module: String,
        /// Public subpath.
        subpath: String,
        /// Public exported name.
        name: String,
    },
    /// Standard-library type.
    Standard {
        #[doc = "Standard-library name."]
        name: String,
    },
}

/// Authored tuple element.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TupleElementModel {
    /// Optional tuple label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Element type.
    #[serde(rename = "type")]
    pub ty: TypeNodeId,
    /// Optional marker.
    pub optional: bool,
    /// Rest marker.
    pub rest: bool,
}

/// One template-literal type interpolation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TemplateSpanModel {
    /// Interpolated type.
    #[serde(rename = "type")]
    pub ty: TypeNodeId,
    /// Following literal text.
    pub text: String,
}

/// Mapped-type property modifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MappedModifier {
    /// Add the modifier.
    Add,
    /// Remove the modifier.
    Remove,
    /// Preserve the original modifier.
    Preserve,
}

/// Literal value; the bigint marker is the lossless JSON transport for source bigints.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LiteralValue {
    /// Arbitrary-precision integer digits.
    BigInt {
        #[serde(rename = "$bigint")]
        #[doc = "Decimal bigint digits."]
        digits: String,
    },
    /// String value.
    String(String),
    /// JavaScript numeric literal.
    Number(serde_json::Number),
    /// Boolean value.
    Boolean(bool),
    /// Null literal.
    Null,
}

/// Structure of one represented type expression.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum TypeNodeKind {
    /// Keyword spelling.
    Keyword {
        #[doc = "Authored keyword."]
        name: KeywordTypeName,
    },
    /// Literal value and exact authored text.
    Literal {
        #[doc = "Literal value."]
        value: LiteralValue,
        #[doc = "Exact authored text."]
        text: String,
    },
    /// Explicit parentheses.
    Parenthesized {
        #[serde(rename = "type")]
        #[doc = "Inner expression."]
        ty: TypeNodeId,
    },
    /// Named type application.
    Reference {
        #[doc = "Authored reference name."]
        name: String,
        #[doc = "Resolved target."]
        target: TypeTargetModel,
        #[doc = "Authored type arguments."]
        arguments: Vec<TypeNodeId>,
    },
    /// Union alternatives.
    Union {
        #[doc = "Ordered alternatives."]
        types: Vec<TypeNodeId>,
    },
    /// Intersection constituents.
    Intersection {
        #[doc = "Ordered constituents."]
        types: Vec<TypeNodeId>,
    },
    /// Array type.
    Array {
        #[doc = "Element type."]
        element: TypeNodeId,
    },
    /// Tuple type.
    Tuple {
        #[doc = "Ordered tuple elements."]
        elements: Vec<TupleElementModel>,
    },
    /// Object-literal type.
    Object {
        #[doc = "Authored members."]
        members: Vec<MemberModel>,
    },
    /// Function type.
    Function {
        #[doc = "Callable signature."]
        signature: SignatureModel,
    },
    /// Constructor type.
    Constructor {
        #[serde(rename = "abstract")]
        #[doc = "Abstract constructor marker."]
        is_abstract: bool,
        #[doc = "Constructor signature."]
        signature: SignatureModel,
    },
    /// Indexed-access type.
    IndexedAccess {
        #[doc = "Object type."]
        object: TypeNodeId,
        #[doc = "Index type."]
        index: TypeNodeId,
    },
    /// Prefix type operator.
    Operator {
        #[doc = "Authored operator spelling."]
        operator: TypeOperatorName,
        #[serde(rename = "type")]
        #[doc = "Operand."]
        ty: TypeNodeId,
    },
    /// Conditional type.
    Conditional {
        #[doc = "Checked type."]
        check: TypeNodeId,
        #[doc = "Constraint type."]
        extends: TypeNodeId,
        #[doc = "True branch."]
        when_true: TypeNodeId,
        #[doc = "False branch."]
        when_false: TypeNodeId,
    },
    /// Inferred generic parameter.
    Infer {
        #[doc = "Inferred parameter."]
        parameter: TypeParameterModel,
    },
    /// Mapped type.
    Mapped {
        /// Mapped key parameter.
        parameter: TypeParameterModel,
        /// Optional key remapping.
        #[serde(skip_serializing_if = "Option::is_none")]
        name_type: Option<TypeNodeId>,
        /// Optional value expression.
        #[serde(skip_serializing_if = "Option::is_none")]
        value: Option<TypeNodeId>,
        /// Readonly modifier.
        #[serde(rename = "readonly")]
        read_only: MappedModifier,
        /// Optional modifier.
        optional: MappedModifier,
    },
    /// Template-literal type.
    TemplateLiteral {
        #[doc = "Leading literal text."]
        head: String,
        #[doc = "Ordered interpolations."]
        spans: Vec<TemplateSpanModel>,
    },
    /// Type query.
    TypeQuery {
        #[doc = "Authored value expression."]
        expression: String,
        #[doc = "Authored type arguments."]
        arguments: Vec<TypeNodeId>,
    },
    /// Import type.
    ImportType {
        /// Public module specifier.
        module: String,
        /// Optional qualified name.
        #[serde(skip_serializing_if = "Option::is_none")]
        qualifier: Option<String>,
        /// Type arguments.
        arguments: Vec<TypeNodeId>,
        /// Type-query marker.
        #[serde(rename = "typeof")]
        is_typeof: bool,
        /// Retained import attributes.
        #[serde(skip_serializing_if = "Option::is_none")]
        attributes: Option<String>,
        /// Resolved import target.
        #[serde(skip_serializing_if = "Option::is_none")]
        target: Option<TypeTargetModel>,
    },
    /// Assertion or predicate type.
    Predicate {
        /// Assertion marker.
        asserts: bool,
        /// Parameter spelling.
        parameter: String,
        /// Optional predicate type.
        #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
        ty: Option<TypeNodeId>,
    },
    /// Polymorphic receiver type.
    This,
}

/// One represented graph node.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DefinedTypeNode {
    /// Graph-local identity.
    pub id: TypeNodeId,
    /// Authored expression structure.
    #[serde(flatten)]
    pub kind: TypeNodeKind,
}

/// A represented node or an unknown record retained for diagnostics.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TypeNodeModel {
    /// Complete represented expression.
    Defined(DefinedTypeNode),
    /// Unrecognized caller-supplied record; renderers must reject it.
    Unsupported(Value),
}

/// Type declarations and expressions owned by one face.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TypeGraph {
    /// Declarations in graph order.
    pub declarations: Vec<TypeDeclarationModel>,
    /// Expressions in graph order.
    pub nodes: Vec<TypeNodeModel>,
}

impl TypeNodeModel {
    /// Node identity used by the source's last-write-wins index.
    pub fn id(&self) -> TypeNodeId {
        match self {
            Self::Defined(node) => node.id.clone(),
            Self::Unsupported(value) => value
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("undefined")
                .into(),
        }
    }
}

impl MemberModel {
    /// Member identity used by the source's last-write-wins index.
    pub fn id(&self) -> MemberId {
        match self {
            Self::Defined(member) => member.base.id.clone(),
            Self::Unsupported(value) => value
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("undefined")
                .into(),
        }
    }
}

/// Direct expression edges in the exact authored order.
///
/// # Errors
///
/// Rejects an unrecognized model record without silently dropping its edges.
pub fn child_type_node_ids(node: &TypeNodeModel) -> crate::Result<Vec<TypeNodeId>> {
    let node = match node {
        TypeNodeModel::Defined(node) => node,
        TypeNodeModel::Unsupported(raw) => {
            return Err(crate::TypertGeneratorError::Model(format!(
                "unsupported model variant {raw}"
            )));
        }
    };
    Ok(match &node.kind {
        TypeNodeKind::Parenthesized { ty } | TypeNodeKind::Operator { ty, .. } => vec![ty.clone()],
        TypeNodeKind::Reference { arguments, .. }
        | TypeNodeKind::TypeQuery { arguments, .. }
        | TypeNodeKind::ImportType { arguments, .. } => arguments.clone(),
        TypeNodeKind::Union { types } | TypeNodeKind::Intersection { types } => types.clone(),
        TypeNodeKind::Array { element } => vec![element.clone()],
        TypeNodeKind::Tuple { elements } => {
            elements.iter().map(|element| element.ty.clone()).collect()
        }
        TypeNodeKind::IndexedAccess { object, index } => vec![object.clone(), index.clone()],
        TypeNodeKind::Conditional {
            check,
            extends,
            when_true,
            when_false,
        } => vec![
            check.clone(),
            extends.clone(),
            when_true.clone(),
            when_false.clone(),
        ],
        TypeNodeKind::Mapped {
            parameter,
            name_type,
            value,
            ..
        } => parameter
            .constraint
            .iter()
            .chain(&parameter.default)
            .chain(name_type)
            .chain(value)
            .cloned()
            .collect(),
        TypeNodeKind::Infer { parameter } => parameter
            .constraint
            .iter()
            .chain(&parameter.default)
            .cloned()
            .collect(),
        TypeNodeKind::TemplateLiteral { spans, .. } => {
            spans.iter().map(|span| span.ty.clone()).collect()
        }
        TypeNodeKind::Predicate { ty, .. } => ty.iter().cloned().collect(),
        TypeNodeKind::Keyword { .. }
        | TypeNodeKind::Literal { .. }
        | TypeNodeKind::Object { .. }
        | TypeNodeKind::Function { .. }
        | TypeNodeKind::Constructor { .. }
        | TypeNodeKind::This => Vec::new(),
    })
}
