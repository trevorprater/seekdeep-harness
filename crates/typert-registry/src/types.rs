//! Generated-artifact and runtime-reflection types.

use std::sync::Arc;

use seekdeep_typert_protocol::{InvocationDescriptor, TypertSchema};
use serde::{Deserialize, Serialize};

/// Independently compiled side that produced a contribution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TypertFace {
    /// Host runtime face.
    Host,
    /// Client runtime face.
    Client,
}

impl TypertFace {
    /// Stable wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Client => "client",
        }
    }
}

/// Structured `JSDoc` tag retained by generated metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypertDocTag {
    /// Tag name.
    pub name: String,
    /// Optional argument.
    pub argument: Option<String>,
    /// Optional comment.
    pub comment: Option<String>,
    /// Complete source text.
    pub text: String,
}

/// Source documentation retained on reflected package elements.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TypertDocumentation {
    /// Long description.
    pub description: Option<String>,
    /// Brief summary.
    pub summary: Option<String>,
    /// Structured tags.
    pub tags: Vec<TypertDocTag>,
    /// Complete `JSDoc` source.
    pub js_doc: Option<String>,
}

/// Reflected member kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TypertMemberKind {
    /// Property.
    Property,
    /// Method.
    Method,
    /// Getter.
    Getter,
    /// Setter.
    Setter,
    /// Call signature.
    Call,
    /// Construct signature.
    Construct,
    /// Index signature.
    Index,
}

/// One generated public member signature.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TypertMemberModel {
    /// Member kind.
    pub kind: TypertMemberKind,
    /// Public name.
    pub name: String,
    /// Rendered signature.
    pub signature: String,
    /// Optional summary.
    pub summary: Option<String>,
    /// Optional full `JSDoc`.
    pub js_doc: Option<String>,
}

/// One named type declaration referenced by reflected business APIs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypertTypeModel {
    /// Exported name.
    pub name: String,
    /// Complete declaration.
    pub declaration: String,
}

/// Runtime reflection metadata for one Cordis service.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TypertServiceModel {
    /// Documentation.
    #[serde(flatten)]
    pub documentation: TypertDocumentation,
    /// Cordis service key.
    pub key: String,
    /// Exported class name.
    pub export_name: String,
    /// Public members.
    pub members: Vec<TypertMemberModel>,
    /// Referenced named types.
    pub types: Vec<TypertTypeModel>,
}

/// Runtime reflection metadata for one Cordis event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TypertEventModel {
    /// Documentation.
    #[serde(flatten)]
    pub documentation: TypertDocumentation,
    /// Event name.
    pub name: String,
    /// Optional Cordis dispatch mode.
    pub mode: Option<String>,
    /// Rendered signature.
    pub signature: String,
}

/// Runtime reflection metadata for one exported reference object.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TypertObjectModel {
    /// Documentation.
    #[serde(flatten)]
    pub documentation: TypertDocumentation,
    /// Source name.
    pub name: String,
    /// Exported name.
    pub export_name: String,
    /// Public members.
    pub members: Vec<TypertMemberModel>,
    /// Referenced named types.
    pub types: Vec<TypertTypeModel>,
}

/// Generated business reflection for one package face.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypertPackageModel {
    /// Cordis services.
    pub services: Vec<TypertServiceModel>,
    /// Cordis events.
    pub events: Vec<TypertEventModel>,
    /// Explicit reference objects.
    pub objects: Vec<TypertObjectModel>,
}

/// One generated live runtime schema.
#[derive(Clone)]
pub struct TypertSchemaContribution {
    /// Exported schema name.
    pub name: String,
    /// Runtime schema implementation.
    pub schema: Arc<dyn TypertSchema>,
}

impl std::fmt::Debug for TypertSchemaContribution {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TypertSchemaContribution")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// One generated package contribution registered atomically.
#[derive(Clone, Debug)]
pub struct TypertContribution {
    /// Package name.
    pub package: String,
    /// Compiled face.
    pub face: TypertFace,
    /// Generated schemas.
    pub schemas: Vec<TypertSchemaContribution>,
    /// Generated reflection.
    pub model: TypertPackageModel,
    /// Host invocation definitions.
    pub invocations: Vec<InvocationDescriptor>,
}

/// A live schema plus its contribution identity.
#[derive(Clone)]
pub struct TypertSchemaRecord {
    /// Package name.
    pub package: String,
    /// Compiled face.
    pub face: TypertFace,
    /// Exported schema name.
    pub name: String,
    /// Global `<package>#<name>` key.
    pub key: String,
    /// Runtime schema.
    pub schema: Arc<dyn TypertSchema>,
}

impl std::fmt::Debug for TypertSchemaRecord {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TypertSchemaRecord")
            .field("package", &self.package)
            .field("face", &self.face)
            .field("name", &self.name)
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

/// Live generated package reflection plus stable identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypertPackageRecord {
    /// Package name.
    pub package: String,
    /// Compiled face.
    pub face: TypertFace,
    /// Global `<package>#<face>` key.
    pub key: String,
    /// Generated reflection.
    pub model: TypertPackageModel,
}

/// Optional schema enumeration filter.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TypertSchemaFilter {
    /// Exact package restriction.
    pub package: Option<String>,
    /// Exact face restriction.
    pub face: Option<TypertFace>,
}

/// Optional package-model enumeration filter.
pub type TypertPackageFilter = TypertSchemaFilter;
