//! Ordered profile-patch parsing and composition.
//!
//! This module preserves the source include plugin's wire-shaped entry objects
//! without changing the loader crate's normalized [`crate::Entry`] and
//! [`crate::Patch`] API. JavaScript expressions remain inert typed nodes here;
//! evaluating them belongs to a later runtime integration.

use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap},
    fmt, str,
};

use indexmap::IndexMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use serde_yml::{
    Value as YamlValue,
    libyml::emitter::{
        Emitter as YamlEmitter, Event as YamlEmitEvent, Mapping as YamlEmitMapping,
        Scalar as YamlEmitScalar, ScalarStyle as YamlEmitScalarStyle, Sequence as YamlEmitSequence,
    },
    libyml::parser::{
        Anchor, Event as YamlEvent, MappingStart, Parser as YamlParser, Scalar, ScalarStyle,
        SequenceStart,
    },
    value::{Tag, TaggedValue},
};
use thiserror::Error;

const JAVASCRIPT_TAG: &str = "tag:yaml.org,2002:js";

/// An inert JavaScript expression from a YAML `!!js` scalar.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JavaScriptExpression(String);

impl JavaScriptExpression {
    /// Creates an expression node without evaluating or validating its source.
    #[must_use]
    pub fn new(source: impl Into<String>) -> Self {
        Self(source.into())
    }

    /// Returns the expression source exactly as parsed.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A source-profile entry identifier.
///
/// Unlike [`crate::EntryId`], this compatibility identifier deliberately
/// preserves empty and whitespace-only strings. JavaScript treats only the
/// empty spelling as falsey during patch lookup and later generates an id for
/// it when the entry tree materializes.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProfileEntryId(String);

impl ProfileEntryId {
    /// Preserves one identifier exactly as it appeared on the wire.
    #[must_use]
    pub fn from_wire(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the exact wire spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns whether JavaScript would treat this string as a patch target.
    #[must_use]
    pub fn is_truthy(&self) -> bool {
        !self.0.is_empty()
    }
}

impl fmt::Display for ProfileEntryId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// One JSON-safe profile value, plus the include dialect's inert `!!js` node.
#[derive(Clone, Debug, PartialEq)]
pub enum ProfileNode {
    /// YAML or JSON null.
    Null,
    /// A boolean.
    Bool(bool),
    /// A YAML number.
    Number(serde_yml::Number),
    /// A string.
    String(String),
    /// An ordered sequence.
    Sequence(Vec<Self>),
    /// An insertion-ordered string-keyed mapping.
    Mapping(IndexMap<String, Self>),
    /// An unevaluated `!!js` scalar.
    JavaScript(JavaScriptExpression),
}

impl ProfileNode {
    /// Returns an inert JavaScript expression when this is a `!!js` node.
    #[must_use]
    pub fn as_javascript(&self) -> Option<&JavaScriptExpression> {
        match self {
            Self::JavaScript(expression) => Some(expression),
            Self::Null
            | Self::Bool(_)
            | Self::Number(_)
            | Self::String(_)
            | Self::Sequence(_)
            | Self::Mapping(_) => None,
        }
    }

    /// Returns the contained string when this is an ordinary string node.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            Self::Null
            | Self::Bool(_)
            | Self::Number(_)
            | Self::Sequence(_)
            | Self::Mapping(_)
            | Self::JavaScript(_) => None,
        }
    }

    /// Returns the contained sequence.
    #[must_use]
    pub fn as_sequence(&self) -> Option<&[Self]> {
        match self {
            Self::Sequence(value) => Some(value),
            Self::Null
            | Self::Bool(_)
            | Self::Number(_)
            | Self::String(_)
            | Self::Mapping(_)
            | Self::JavaScript(_) => None,
        }
    }

    /// Returns the contained mapping.
    #[must_use]
    pub fn as_mapping(&self) -> Option<&IndexMap<String, Self>> {
        match self {
            Self::Mapping(value) => Some(value),
            Self::Null
            | Self::Bool(_)
            | Self::Number(_)
            | Self::String(_)
            | Self::Sequence(_)
            | Self::JavaScript(_) => None,
        }
    }

    /// Applies JavaScript truthiness without evaluating expression nodes.
    ///
    /// A `!!js` node is an object in the source parser and is therefore truthy.
    #[must_use]
    pub fn is_javascript_truthy(&self) -> bool {
        match self {
            Self::Null => false,
            Self::Bool(value) => *value,
            Self::Number(value) => value
                .as_f64()
                .is_none_or(|value| value != 0.0 && !value.is_nan()),
            Self::String(value) => !value.is_empty(),
            Self::Sequence(_) | Self::Mapping(_) | Self::JavaScript(_) => true,
        }
    }
}

impl Serialize for ProfileNode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        profile_node_to_yaml(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ProfileNode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = YamlValue::deserialize(deserializer)?;
        profile_node_from_yaml(value).map_err(D::Error::custom)
    }
}

/// One ordered source-profile entry object.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProfileEntry {
    fields: IndexMap<String, ProfileNode>,
}

impl ProfileEntry {
    /// Creates an entry from insertion-ordered wire fields.
    #[must_use]
    pub fn from_fields(fields: IndexMap<String, ProfileNode>) -> Self {
        Self { fields }
    }

    /// Returns every wire field in insertion order.
    #[must_use]
    pub fn fields(&self) -> &IndexMap<String, ProfileNode> {
        &self.fields
    }

    /// Returns one field by name.
    #[must_use]
    pub fn field(&self, name: &str) -> Option<&ProfileNode> {
        self.fields.get(name)
    }

    /// Returns the optional source-profile id without normalizing its spelling.
    #[must_use]
    pub fn id(&self) -> Option<ProfileEntryId> {
        self.field("id")
            .and_then(ProfileNode::as_str)
            .map(ProfileEntryId::from_wire)
    }

    /// Returns the optional plugin name.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.field("name").and_then(ProfileNode::as_str)
    }

    /// Returns the optional whole plugin config node.
    #[must_use]
    pub fn config(&self) -> Option<&ProfileNode> {
        self.field("config")
    }

    /// Returns the optional group marker without coercing it.
    #[must_use]
    pub fn group(&self) -> Option<&ProfileNode> {
        self.field("group")
    }

    /// Returns the optional disabled marker or expression.
    #[must_use]
    pub fn disabled(&self) -> Option<&ProfileNode> {
        self.field("disabled")
    }

    /// Returns the optional injection declaration.
    #[must_use]
    pub fn inject(&self) -> Option<&ProfileNode> {
        self.field("inject")
    }

    /// Returns the optional interception declaration.
    #[must_use]
    pub fn intercept(&self) -> Option<&ProfileNode> {
        self.field("intercept")
    }

    /// Returns the optional isolation declaration.
    #[must_use]
    pub fn isolate(&self) -> Option<&ProfileNode> {
        self.field("isolate")
    }

    fn set_id(&mut self, id: &ProfileEntryId) {
        self.fields
            .insert("id".to_owned(), ProfileNode::String(id.0.clone()));
    }
}

/// One ordered source-profile patch object.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProfilePatch {
    fields: IndexMap<String, ProfileNode>,
}

impl ProfilePatch {
    /// Creates a patch from insertion-ordered wire fields.
    #[must_use]
    pub fn from_fields(fields: IndexMap<String, ProfileNode>) -> Self {
        Self { fields }
    }

    /// Returns every wire field in insertion order.
    #[must_use]
    pub fn fields(&self) -> &IndexMap<String, ProfileNode> {
        &self.fields
    }

    /// Returns one field by name.
    #[must_use]
    pub fn field(&self, name: &str) -> Option<&ProfileNode> {
        self.fields.get(name)
    }

    /// Returns the optional target id without normalizing its spelling.
    #[must_use]
    pub fn id(&self) -> Option<ProfileEntryId> {
        self.field("id")
            .and_then(ProfileNode::as_str)
            .map(ProfileEntryId::from_wire)
    }

    /// Returns the optional plugin-name guard.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.field("name").and_then(ProfileNode::as_str)
    }

    /// Returns the optional insertion node.
    #[must_use]
    pub fn insert(&self) -> Option<&ProfileNode> {
        self.field("insert")
    }
}

/// A source-compatible skipped-patch diagnostic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProfilePatchWarning {
    /// An insertion named an id absent from the current entry index.
    InsertTargetNotFound {
        /// Missing target id.
        id: ProfileEntryId,
    },
    /// An insertion targeted an entry whose `group` field is falsey.
    InsertTargetNotGroup {
        /// Non-group target id.
        id: ProfileEntryId,
    },
    /// A non-insertion patch omitted a truthy id.
    MissingId,
    /// A non-insertion patch named an id absent from the current entry index.
    TargetNotFound {
        /// Missing target id.
        id: ProfileEntryId,
    },
    /// A truthy patch `name` did not match the target's current name.
    NameMismatch {
        /// Target id.
        id: ProfileEntryId,
        /// Current target name, or `None` for JavaScript `undefined`.
        expected: Option<String>,
        /// Guard supplied by the patch.
        actual: String,
    },
}

impl fmt::Display for ProfilePatchWarning {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InsertTargetNotFound { id } => write!(
                formatter,
                "patch insert: entry {} not found",
                code_string(Some(id.as_str()))
            ),
            Self::InsertTargetNotGroup { id } => write!(
                formatter,
                "patch insert: entry {} is not a group",
                code_string(Some(id.as_str()))
            ),
            Self::MissingId => formatter.write_str("patch: id is required for non-insert patches"),
            Self::TargetNotFound { id } => write!(
                formatter,
                "patch: entry {} not found",
                code_string(Some(id.as_str()))
            ),
            Self::NameMismatch {
                id,
                expected,
                actual,
            } => write!(
                formatter,
                "patch: name mismatch for {} (expected {}, got {}), skipping",
                code_string(Some(id.as_str())),
                code_string(expected.as_deref()),
                code_string(Some(actual))
            ),
        }
    }
}

/// Profile patch parsing or composition failure.
#[derive(Debug, Error)]
pub enum ProfilePatchError {
    /// The YAML or JSON document could not be parsed or rendered.
    #[error("profile patch document is invalid: {0}")]
    InvalidDocument(String),
    /// A parsed document did not contain a top-level array.
    #[error("profile patch document must be a top-level array")]
    TopLevelArrayRequired,
    /// An entry or patch position did not contain a mapping.
    #[error("profile patch {context} must be a mapping")]
    MappingRequired {
        /// Human-readable position within the document or insertion.
        context: String,
    },
    /// A mapping key was not representable as a JavaScript object key.
    #[error("profile patch mapping key must be a scalar")]
    ScalarMappingKeyRequired,
    /// A YAML tag other than the include dialect's scalar `!!js` was present.
    #[error("profile patch YAML tag {0:?} is not supported")]
    UnsupportedTag(String),
    /// A `!!js` tag did not wrap a scalar string.
    #[error("profile patch !!js value must be a scalar string")]
    JavaScriptScalarRequired,
    /// A truthy `insert` value was not an entry array.
    #[error("profile patch insert at index {patch_index} must be an array")]
    InsertArrayRequired {
        /// Zero-based patch position.
        patch_index: usize,
    },
}

/// Detached composition output and warnings in source emission order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProfileComposition {
    entries: Vec<ProfileEntry>,
    warnings: Vec<ProfilePatchWarning>,
}

impl ProfileComposition {
    /// Returns the detached effective entry list.
    #[must_use]
    pub fn entries(&self) -> &[ProfileEntry] {
        &self.entries
    }

    /// Returns skipped-patch warnings in patch application order.
    #[must_use]
    pub fn warnings(&self) -> &[ProfilePatchWarning] {
        &self.warnings
    }

    /// Splits the result into its owned entry list and warning list.
    #[must_use]
    pub fn into_parts(self) -> (Vec<ProfileEntry>, Vec<ProfilePatchWarning>) {
        (self.entries, self.warnings)
    }
}

/// Parses an entry-list YAML or JSON document while preserving `!!js` nodes.
///
/// # Errors
///
/// Returns a syntax, unsupported-tag, top-level-shape, or row-shape failure.
pub fn parse_entry_list_yaml(source: &str) -> Result<Vec<ProfileEntry>, ProfilePatchError> {
    parse_object_list(source, "entry")
        .map(|objects| objects.into_iter().map(ProfileEntry::from_fields).collect())
}

/// Parses a profile patch-list YAML or JSON document while preserving `!!js` nodes.
///
/// # Errors
///
/// Returns a syntax, unsupported-tag, top-level-shape, or row-shape failure.
pub fn parse_patch_list_yaml(source: &str) -> Result<Vec<ProfilePatch>, ProfilePatchError> {
    parse_object_list(source, "patch")
        .map(|objects| objects.into_iter().map(ProfilePatch::from_fields).collect())
}

/// Renders an entry list as YAML while retaining inert JavaScript tags.
///
/// # Errors
///
/// Returns a serializer failure.
pub fn render_entry_list_yaml(entries: &[ProfileEntry]) -> Result<String, ProfilePatchError> {
    render_object_list(entries.iter().map(ProfileEntry::fields))
}

/// Renders a patch list as YAML while retaining inert JavaScript tags.
///
/// # Errors
///
/// Returns a serializer failure.
pub fn render_patch_list_yaml(patches: &[ProfilePatch]) -> Result<String, ProfilePatchError> {
    render_object_list(patches.iter().map(ProfilePatch::fields))
}

/// Applies ordered patches to a detached clone of a base entry list.
///
/// The entry index is constructed once from the base and extended after each
/// insertion, exactly like the source include plugin. Ordinary overrides do
/// not re-index replacement configs.
///
/// # Errors
///
/// Returns a malformed truthy insertion or nested entry-list failure.
pub fn apply_entry_patches(
    base: &[ProfileEntry],
    patches: &[ProfilePatch],
) -> Result<ProfileComposition, ProfilePatchError> {
    let mut warnings = Vec::new();
    let entries = apply_entry_patches_with_warning_sink(base, patches, |warning| {
        warnings.push(warning);
    })?;
    Ok(ProfileComposition { entries, warnings })
}

/// Applies ordered patches while delivering skipped-patch diagnostics at the
/// point each patch is evaluated.
///
/// This is the source-compatible warning boundary for callers that must retain
/// diagnostics emitted before a later patch fails. [`apply_entry_patches`]
/// remains a convenience wrapper for callers that want warnings bundled with a
/// successful composition.
///
/// # Errors
///
/// Returns a malformed truthy insertion or nested entry-list failure. Warnings
/// emitted before the failure have already been delivered to `warn`.
pub fn apply_entry_patches_with_warning_sink<Warn>(
    base: &[ProfileEntry],
    patches: &[ProfilePatch],
    mut warn: Warn,
) -> Result<Vec<ProfileEntry>, ProfilePatchError>
where
    Warn: FnMut(ProfilePatchWarning),
{
    let mut composer = Composer::default();
    for entry in base {
        let handle = composer.allocate_entry(entry, "base entry")?;
        composer.roots.push(handle);
    }
    for (patch_index, patch) in patches.iter().enumerate() {
        let result = composer.apply_patch(patch, patch_index);
        for warning in composer.warnings.drain(..) {
            warn(warning);
        }
        result?;
    }
    Ok(composer.finish_entries())
}

/// Flattens profile patch layers in declaration order and composes an empty root.
///
/// # Errors
///
/// Returns a malformed truthy insertion or nested entry-list failure.
pub fn compose_profile_layers(
    layers: &[Vec<ProfilePatch>],
) -> Result<ProfileComposition, ProfilePatchError> {
    let patches = layers
        .iter()
        .flat_map(|layer| layer.iter().cloned())
        .collect::<Vec<_>>();
    apply_entry_patches(&[], &patches)
}

/// Preserves a truthy supplied id or assigns the first unoccupied generated id.
///
/// This is the deterministic seam for the source entry tree's later
/// `ensureId` step. Composition itself intentionally leaves absent and empty
/// ids untouched. Supplied ids are not collision-checked here, matching the
/// source; the entry-group update owns duplicate handling.
pub fn ensure_entry_id_with<Occupied, Generate>(
    entry: &mut ProfileEntry,
    mut is_occupied: Occupied,
    mut generate: Generate,
) -> ProfileEntryId
where
    Occupied: FnMut(&ProfileEntryId) -> bool,
    Generate: FnMut() -> ProfileEntryId,
{
    if let Some(id) = entry.id().filter(ProfileEntryId::is_truthy) {
        return id;
    }
    loop {
        let candidate = generate();
        if !candidate.is_truthy() || is_occupied(&candidate) {
            continue;
        }
        entry.set_id(&candidate);
        return candidate;
    }
}

fn parse_object_list(
    source: &str,
    item_name: &str,
) -> Result<Vec<IndexMap<String, ProfileNode>>, ProfilePatchError> {
    let node = ProfileDocumentParser::parse(source)?;
    let ProfileNode::Sequence(items) = node else {
        return Err(ProfilePatchError::TopLevelArrayRequired);
    };
    items
        .into_iter()
        .enumerate()
        .map(|(index, item)| match item {
            ProfileNode::Mapping(fields) => Ok(fields),
            ProfileNode::Null
            | ProfileNode::Bool(_)
            | ProfileNode::Number(_)
            | ProfileNode::String(_)
            | ProfileNode::Sequence(_)
            | ProfileNode::JavaScript(_) => Err(ProfilePatchError::MappingRequired {
                context: format!("{item_name} at index {index}"),
            }),
        })
        .collect()
}

/// Event-level parser for the include dialect.
///
/// `serde_yml::Value` intentionally discards unknown YAML tags while
/// deserializing. The include's `!!js` type is such a tag, so the public
/// parsing boundary has to consume libyaml events before converting the rest
/// of the document into the profile AST.
struct ProfileDocumentParser<'input> {
    parser: YamlParser<'input>,
    anchors: BTreeMap<Anchor, ProfileNode>,
}

impl<'input> ProfileDocumentParser<'input> {
    fn parse(source: &'input str) -> Result<ProfileNode, ProfilePatchError> {
        let mut document = Self {
            parser: YamlParser::new(Cow::Borrowed(source.as_bytes())),
            anchors: BTreeMap::new(),
        };
        document.expect_stream_start()?;
        match document.next_event()? {
            YamlEvent::StreamEnd => return Ok(ProfileNode::Null),
            YamlEvent::DocumentStart => {}
            event => return Err(Self::unexpected_event("start of document", &event)),
        }
        let value = document.parse_next_node()?;
        document.expect_document_end()?;
        match document.next_event()? {
            YamlEvent::StreamEnd => Ok(value),
            YamlEvent::DocumentStart => Err(ProfilePatchError::InvalidDocument(
                "multiple YAML documents are not supported".to_owned(),
            )),
            event => Err(Self::unexpected_event("end of stream", &event)),
        }
    }

    fn next_event(&mut self) -> Result<YamlEvent<'input>, ProfilePatchError> {
        self.parser
            .parse_next_event()
            .map(|(event, _)| event)
            .map_err(|error| ProfilePatchError::InvalidDocument(error.to_string()))
    }

    fn expect_stream_start(&mut self) -> Result<(), ProfilePatchError> {
        match self.next_event()? {
            YamlEvent::StreamStart => Ok(()),
            event => Err(Self::unexpected_event("start of stream", &event)),
        }
    }

    fn expect_document_end(&mut self) -> Result<(), ProfilePatchError> {
        match self.next_event()? {
            YamlEvent::DocumentEnd => Ok(()),
            event => Err(Self::unexpected_event("end of document", &event)),
        }
    }

    fn parse_next_node(&mut self) -> Result<ProfileNode, ProfilePatchError> {
        let event = self.next_event()?;
        self.parse_node(event)
    }

    fn parse_node(&mut self, event: YamlEvent<'input>) -> Result<ProfileNode, ProfilePatchError> {
        match event {
            YamlEvent::Alias(anchor) => self.anchors.get(&anchor).cloned().ok_or_else(|| {
                ProfilePatchError::InvalidDocument(format!(
                    "YAML alias {anchor:?} does not name an earlier anchor"
                ))
            }),
            YamlEvent::Scalar(scalar) => self.parse_scalar(scalar),
            YamlEvent::SequenceStart(start) => self.parse_sequence(start),
            YamlEvent::MappingStart(start) => self.parse_mapping(start),
            YamlEvent::StreamStart
            | YamlEvent::StreamEnd
            | YamlEvent::DocumentStart
            | YamlEvent::DocumentEnd
            | YamlEvent::SequenceEnd
            | YamlEvent::MappingEnd => Err(Self::unexpected_event("profile value", &event)),
        }
    }

    fn parse_scalar(&mut self, scalar: Scalar<'input>) -> Result<ProfileNode, ProfilePatchError> {
        let Scalar {
            anchor,
            tag,
            value,
            style,
            repr: _,
        } = scalar;
        let source = str::from_utf8(&value).map_err(|error| {
            ProfilePatchError::InvalidDocument(format!("YAML scalar is not UTF-8: {error}"))
        })?;
        let node = resolve_scalar(tag.as_ref(), source, style)?;
        self.register_anchor(anchor, &node);
        Ok(node)
    }

    fn parse_sequence(&mut self, start: SequenceStart) -> Result<ProfileNode, ProfilePatchError> {
        validate_collection_tag(start.tag.as_ref(), "tag:yaml.org,2002:seq")?;
        let mut values = Vec::new();
        loop {
            let event = self.next_event()?;
            if matches!(event, YamlEvent::SequenceEnd) {
                break;
            }
            values.push(self.parse_node(event)?);
        }
        let node = ProfileNode::Sequence(values);
        self.register_anchor(start.anchor, &node);
        Ok(node)
    }

    fn parse_mapping(&mut self, start: MappingStart) -> Result<ProfileNode, ProfilePatchError> {
        validate_collection_tag(start.tag.as_ref(), "tag:yaml.org,2002:map")?;
        let mut fields = IndexMap::new();
        loop {
            let key_event = self.next_event()?;
            if matches!(key_event, YamlEvent::MappingEnd) {
                break;
            }
            let key = javascript_mapping_key(&self.parse_node(key_event)?)?;
            if fields.contains_key(&key) {
                return Err(ProfilePatchError::InvalidDocument(format!(
                    "duplicated mapping key {key:?}"
                )));
            }
            let value = self.parse_next_node()?;
            fields.insert(key, value);
        }
        let node = ProfileNode::Mapping(fields);
        self.register_anchor(start.anchor, &node);
        Ok(node)
    }

    fn register_anchor(&mut self, anchor: Option<Anchor>, value: &ProfileNode) {
        if let Some(anchor) = anchor {
            self.anchors.insert(anchor, value.clone());
        }
    }

    fn unexpected_event(expected: &str, actual: &YamlEvent<'_>) -> ProfilePatchError {
        ProfilePatchError::InvalidDocument(format!("expected {expected}, found {actual:?}"))
    }
}

fn validate_collection_tag(
    tag: Option<&serde_yml::libyml::tag::Tag>,
    expected: &str,
) -> Result<(), ProfilePatchError> {
    let Some(tag) = tag else {
        return Ok(());
    };
    if tag == JAVASCRIPT_TAG {
        return Err(ProfilePatchError::JavaScriptScalarRequired);
    }
    if tag == expected {
        return Ok(());
    }
    Err(ProfilePatchError::UnsupportedTag(libyml_tag_string(tag)))
}

fn resolve_scalar(
    tag: Option<&serde_yml::libyml::tag::Tag>,
    value: &str,
    style: ScalarStyle,
) -> Result<ProfileNode, ProfilePatchError> {
    if let Some(tag) = tag {
        if tag == JAVASCRIPT_TAG {
            if value.is_empty() && style == ScalarStyle::Plain {
                return Err(ProfilePatchError::JavaScriptScalarRequired);
            }
            return Ok(ProfileNode::JavaScript(JavaScriptExpression::new(value)));
        }
        if tag == "tag:yaml.org,2002:str" {
            return Ok(ProfileNode::String(value.to_owned()));
        }
        if tag == "tag:yaml.org,2002:null" {
            let value = if value.is_empty() && style == ScalarStyle::Plain {
                None
            } else {
                Some(value)
            };
            return resolve_yaml_null(value)
                .then_some(ProfileNode::Null)
                .ok_or_else(|| invalid_explicit_scalar("null", value.unwrap_or_default()));
        }
        if tag == "tag:yaml.org,2002:bool" {
            return resolve_yaml_bool(value)
                .map(ProfileNode::Bool)
                .ok_or_else(|| invalid_explicit_scalar("boolean", value));
        }
        if tag == "tag:yaml.org,2002:int" {
            return resolve_yaml_integer(value)
                .map(ProfileNode::Number)
                .ok_or_else(|| invalid_explicit_scalar("integer", value));
        }
        if tag == "tag:yaml.org,2002:float" {
            return resolve_yaml_float(value)
                .map(ProfileNode::Number)
                .ok_or_else(|| invalid_explicit_scalar("float", value));
        }
        return Err(ProfilePatchError::UnsupportedTag(libyml_tag_string(tag)));
    }

    if style != ScalarStyle::Plain {
        return Ok(ProfileNode::String(value.to_owned()));
    }
    if resolve_yaml_null((!value.is_empty()).then_some(value)) {
        return Ok(ProfileNode::Null);
    }
    if let Some(value) = resolve_yaml_bool(value) {
        return Ok(ProfileNode::Bool(value));
    }
    if let Some(value) = resolve_yaml_integer(value) {
        return Ok(ProfileNode::Number(value));
    }
    if let Some(value) = resolve_yaml_float(value) {
        return Ok(ProfileNode::Number(value));
    }
    Ok(ProfileNode::String(value.to_owned()))
}

fn invalid_explicit_scalar(kind: &str, value: &str) -> ProfilePatchError {
    ProfilePatchError::InvalidDocument(format!("explicit YAML {kind} tag cannot resolve {value:?}"))
}

fn libyml_tag_string(tag: &serde_yml::libyml::tag::Tag) -> String {
    String::from_utf8_lossy(tag).into_owned()
}

fn resolve_yaml_null(value: Option<&str>) -> bool {
    matches!(value, None | Some("~" | "null" | "Null" | "NULL"))
}

fn resolve_yaml_bool(value: &str) -> Option<bool> {
    match value {
        "true" | "True" | "TRUE" => Some(true),
        "false" | "False" | "FALSE" => Some(false),
        _ => None,
    }
}

fn resolve_yaml_integer(value: &str) -> Option<serde_yml::Number> {
    let (negative, unsigned) = strip_yaml_sign(value)?;
    if unsigned == "0" {
        return Some(serde_yml::Number::from(0_u64));
    }
    let parsed = if let Some(digits) = unsigned.strip_prefix("0b") {
        parse_radix_number(digits, 2)?
    } else if let Some(digits) = unsigned.strip_prefix("0o") {
        parse_radix_number(digits, 8)?
    } else if let Some(digits) = unsigned.strip_prefix("0x") {
        parse_radix_number(digits, 16)?
    } else {
        if unsigned.is_empty() || !unsigned.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        unsigned.parse::<f64>().ok()?
    };
    let parsed = if negative { -parsed } else { parsed };
    parsed.is_finite().then(|| number_from_javascript(parsed))
}

fn strip_yaml_sign(value: &str) -> Option<(bool, &str)> {
    if let Some(value) = value.strip_prefix('-') {
        (!value.is_empty()).then_some((true, value))
    } else if let Some(value) = value.strip_prefix('+') {
        (!value.is_empty()).then_some((false, value))
    } else if value.is_empty() {
        None
    } else {
        Some((false, value))
    }
}

fn parse_radix_number(digits: &str, radix: u32) -> Option<f64> {
    if digits.is_empty() {
        return None;
    }
    let mut value = 0.0_f64;
    for byte in digits.bytes() {
        let digit = char::from(byte).to_digit(radix)?;
        value = value.mul_add(f64::from(radix), f64::from(digit));
        if !value.is_finite() {
            return None;
        }
    }
    Some(value)
}

fn resolve_yaml_float(value: &str) -> Option<serde_yml::Number> {
    let parsed = match value {
        ".inf" | ".Inf" | ".INF" | "+.inf" | "+.Inf" | "+.INF" => f64::INFINITY,
        "-.inf" | "-.Inf" | "-.INF" => f64::NEG_INFINITY,
        ".nan" | ".NaN" | ".NAN" => f64::NAN,
        _ => {
            if !is_yaml_finite_float(value) {
                return None;
            }
            let parsed = value.parse::<f64>().ok()?;
            if !parsed.is_finite() {
                return None;
            }
            parsed
        }
    };
    Some(number_from_javascript(parsed))
}

fn is_yaml_finite_float(value: &str) -> bool {
    let unsigned = value.strip_prefix(['+', '-']).unwrap_or(value);
    let mantissa = match unsigned.find(['e', 'E']) {
        Some(index) => {
            let (mantissa, exponent) = unsigned.split_at(index);
            if exponent[1..].contains(['e', 'E']) || !valid_yaml_exponent(&exponent[1..]) {
                return false;
            }
            mantissa
        }
        None => unsigned,
    };
    if let Some(fraction) = mantissa.strip_prefix('.') {
        !fraction.is_empty() && fraction.bytes().all(|byte| byte.is_ascii_digit())
    } else if let Some((integer, fraction)) = mantissa.split_once('.') {
        !integer.is_empty()
            && integer.bytes().all(|byte| byte.is_ascii_digit())
            && fraction.bytes().all(|byte| byte.is_ascii_digit())
    } else {
        !mantissa.is_empty() && mantissa.bytes().all(|byte| byte.is_ascii_digit())
    }
}

fn valid_yaml_exponent(value: &str) -> bool {
    let digits = value.strip_prefix(['+', '-']).unwrap_or(value);
    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
}

fn number_from_javascript(value: f64) -> serde_yml::Number {
    if value.is_sign_negative() && value == 0.0 {
        return serde_yml::Number::from(value);
    }
    if value.is_finite() && value.fract() == 0.0 {
        let integer = format!("{value:.0}");
        if let Ok(number) = integer.parse() {
            return number;
        }
    }
    serde_yml::Number::from(value)
}

fn javascript_mapping_key(value: &ProfileNode) -> Result<String, ProfilePatchError> {
    match value {
        ProfileNode::Null => Ok("null".to_owned()),
        ProfileNode::Bool(value) => Ok(value.to_string()),
        ProfileNode::Number(value) => Ok(javascript_number_string(value)),
        ProfileNode::String(value) => Ok(value.clone()),
        ProfileNode::Sequence(values) => values
            .iter()
            .map(javascript_array_key_element)
            .collect::<Result<Vec<_>, _>>()
            .map(|values| values.join(",")),
        ProfileNode::Mapping(_) | ProfileNode::JavaScript(_) => Ok("[object Object]".to_owned()),
    }
}

fn javascript_array_key_element(value: &ProfileNode) -> Result<String, ProfilePatchError> {
    match value {
        ProfileNode::Null => Ok(String::new()),
        ProfileNode::Sequence(_) => Err(ProfilePatchError::ScalarMappingKeyRequired),
        ProfileNode::Mapping(_) | ProfileNode::JavaScript(_) => Ok("[object Object]".to_owned()),
        ProfileNode::Bool(_) | ProfileNode::Number(_) | ProfileNode::String(_) => {
            javascript_mapping_key(value)
        }
    }
}

fn javascript_number_string(value: &serde_yml::Number) -> String {
    let value = value
        .as_f64()
        .expect("every serde_yml number converts to a JavaScript number");
    if value.is_nan() {
        "NaN".to_owned()
    } else if value == f64::INFINITY {
        "Infinity".to_owned()
    } else if value == f64::NEG_INFINITY {
        "-Infinity".to_owned()
    } else if value == 0.0 {
        "0".to_owned()
    } else {
        value.to_string()
    }
}

fn render_object_list<'a>(
    objects: impl Iterator<Item = &'a IndexMap<String, ProfileNode>>,
) -> Result<String, ProfilePatchError> {
    let value = ProfileNode::Sequence(
        objects
            .map(|fields| ProfileNode::Mapping(fields.clone()))
            .collect(),
    );
    let mut output = Vec::new();
    {
        let mut emitter = YamlEmitter::new(Box::new(&mut output));
        emit_yaml_event(&mut emitter, YamlEmitEvent::StreamStart)?;
        emit_yaml_event(&mut emitter, YamlEmitEvent::DocumentStart)?;
        emit_profile_node(&mut emitter, &value)?;
        emit_yaml_event(&mut emitter, YamlEmitEvent::DocumentEnd)?;
        emit_yaml_event(&mut emitter, YamlEmitEvent::StreamEnd)?;
        emitter
            .flush()
            .map_err(|error| yaml_emitter_error(&error))?;
    }
    String::from_utf8(output).map_err(|error| {
        ProfilePatchError::InvalidDocument(format!("rendered YAML is not UTF-8: {error}"))
    })
}

fn emit_profile_node(
    emitter: &mut YamlEmitter<'_>,
    value: &ProfileNode,
) -> Result<(), ProfilePatchError> {
    match value {
        ProfileNode::Null => emit_scalar(emitter, None, "null", YamlEmitScalarStyle::Plain),
        ProfileNode::Bool(value) => emit_scalar(
            emitter,
            None,
            if *value { "true" } else { "false" },
            YamlEmitScalarStyle::Plain,
        ),
        ProfileNode::Number(value) => emit_scalar(
            emitter,
            None,
            &value.to_string(),
            YamlEmitScalarStyle::Plain,
        ),
        ProfileNode::String(value) => emit_scalar(
            emitter,
            None,
            value,
            if plain_scalar_preserves_string(value) {
                YamlEmitScalarStyle::Any
            } else {
                YamlEmitScalarStyle::SingleQuoted
            },
        ),
        ProfileNode::JavaScript(expression) => {
            let style = if expression.0.is_empty() {
                YamlEmitScalarStyle::SingleQuoted
            } else if expression.0.contains('\n') {
                YamlEmitScalarStyle::Literal
            } else {
                YamlEmitScalarStyle::Any
            };
            emit_scalar(emitter, Some(JAVASCRIPT_TAG), &expression.0, style)
        }
        ProfileNode::Sequence(values) => {
            emit_yaml_event(
                emitter,
                YamlEmitEvent::SequenceStart(YamlEmitSequence { tag: None }),
            )?;
            for value in values {
                emit_profile_node(emitter, value)?;
            }
            emit_yaml_event(emitter, YamlEmitEvent::SequenceEnd)
        }
        ProfileNode::Mapping(fields) => {
            emit_yaml_event(
                emitter,
                YamlEmitEvent::MappingStart(YamlEmitMapping { tag: None }),
            )?;
            for (key, value) in fields {
                emit_scalar(emitter, None, key, YamlEmitScalarStyle::Any)?;
                emit_profile_node(emitter, value)?;
            }
            emit_yaml_event(emitter, YamlEmitEvent::MappingEnd)
        }
    }
}

fn plain_scalar_preserves_string(value: &str) -> bool {
    matches!(
        resolve_scalar(None, value, ScalarStyle::Plain),
        Ok(ProfileNode::String(parsed)) if parsed == value
    )
}

fn emit_scalar(
    emitter: &mut YamlEmitter<'_>,
    tag: Option<&str>,
    value: &str,
    style: YamlEmitScalarStyle,
) -> Result<(), ProfilePatchError> {
    emit_yaml_event(
        emitter,
        YamlEmitEvent::Scalar(YamlEmitScalar {
            tag: tag.map(str::to_owned),
            value,
            style,
        }),
    )
}

fn emit_yaml_event(
    emitter: &mut YamlEmitter<'_>,
    event: YamlEmitEvent<'_>,
) -> Result<(), ProfilePatchError> {
    emitter
        .emit(event)
        .map_err(|error| yaml_emitter_error(&error))
}

fn yaml_emitter_error(error: &serde_yml::libyml::emitter::Error) -> ProfilePatchError {
    ProfilePatchError::InvalidDocument(format!("YAML emitter failed: {error:?}"))
}

fn profile_node_from_yaml(value: YamlValue) -> Result<ProfileNode, ProfilePatchError> {
    match value {
        YamlValue::Null => Ok(ProfileNode::Null),
        YamlValue::Bool(value) => Ok(ProfileNode::Bool(value)),
        YamlValue::Number(value) => Ok(ProfileNode::Number(value)),
        YamlValue::String(value) => Ok(ProfileNode::String(value)),
        YamlValue::Sequence(values) => values
            .into_iter()
            .map(profile_node_from_yaml)
            .collect::<Result<_, _>>()
            .map(ProfileNode::Sequence),
        YamlValue::Mapping(mapping) => {
            let mut fields = IndexMap::with_capacity(mapping.len());
            for (key, value) in mapping {
                fields.insert(yaml_mapping_key(key)?, profile_node_from_yaml(value)?);
            }
            Ok(ProfileNode::Mapping(fields))
        }
        YamlValue::Tagged(tagged) if is_javascript_tag(&tagged.tag) => {
            let YamlValue::String(source) = tagged.value else {
                return Err(ProfilePatchError::JavaScriptScalarRequired);
            };
            Ok(ProfileNode::JavaScript(JavaScriptExpression::new(source)))
        }
        YamlValue::Tagged(tagged) => Err(ProfilePatchError::UnsupportedTag(tagged.tag.string)),
    }
}

fn yaml_mapping_key(value: YamlValue) -> Result<String, ProfilePatchError> {
    match value {
        YamlValue::Null => Ok("null".to_owned()),
        YamlValue::Bool(value) => Ok(value.to_string()),
        YamlValue::Number(value) => Ok(value.to_string()),
        YamlValue::String(value) => Ok(value),
        YamlValue::Sequence(_) | YamlValue::Mapping(_) | YamlValue::Tagged(_) => {
            Err(ProfilePatchError::ScalarMappingKeyRequired)
        }
    }
}

fn profile_node_to_yaml(value: &ProfileNode) -> YamlValue {
    match value {
        ProfileNode::Null => YamlValue::Null,
        ProfileNode::Bool(value) => YamlValue::Bool(*value),
        ProfileNode::Number(value) => YamlValue::Number(*value),
        ProfileNode::String(value) => YamlValue::String(value.clone()),
        ProfileNode::Sequence(values) => {
            YamlValue::Sequence(values.iter().map(profile_node_to_yaml).collect())
        }
        ProfileNode::Mapping(fields) => YamlValue::Mapping(
            fields
                .iter()
                .map(|(key, value)| (YamlValue::String(key.clone()), profile_node_to_yaml(value)))
                .collect(),
        ),
        ProfileNode::JavaScript(expression) => YamlValue::Tagged(Box::new(TaggedValue {
            tag: Tag::new(JAVASCRIPT_TAG),
            value: YamlValue::String(expression.0.clone()),
        })),
    }
}

fn is_javascript_tag(tag: &Tag) -> bool {
    matches!(
        tag.string.as_str(),
        JAVASCRIPT_TAG | "!!js" | "!<tag:yaml.org,2002:js>"
    )
}

fn code_string(value: Option<&str>) -> String {
    value.map_or_else(
        || "undefined".to_owned(),
        |value| serde_json::to_string(value).expect("strings always serialize as JSON"),
    )
}

#[derive(Clone, Debug, PartialEq)]
enum WorkingNode {
    Null,
    Bool(bool),
    Number(serde_yml::Number),
    String(String),
    Sequence(Vec<Self>),
    Mapping(IndexMap<String, Self>),
    JavaScript(JavaScriptExpression),
    Entry(usize),
}

impl WorkingNode {
    fn from_profile(value: &ProfileNode) -> Self {
        match value {
            ProfileNode::Null => Self::Null,
            ProfileNode::Bool(value) => Self::Bool(*value),
            ProfileNode::Number(value) => Self::Number(*value),
            ProfileNode::String(value) => Self::String(value.clone()),
            ProfileNode::Sequence(values) => {
                Self::Sequence(values.iter().map(Self::from_profile).collect())
            }
            ProfileNode::Mapping(fields) => Self::Mapping(
                fields
                    .iter()
                    .map(|(key, value)| (key.clone(), Self::from_profile(value)))
                    .collect(),
            ),
            ProfileNode::JavaScript(expression) => Self::JavaScript(expression.clone()),
        }
    }

    fn is_javascript_truthy(&self) -> bool {
        match self {
            Self::Null => false,
            Self::Bool(value) => *value,
            Self::Number(value) => value
                .as_f64()
                .is_none_or(|value| value != 0.0 && !value.is_nan()),
            Self::String(value) => !value.is_empty(),
            Self::Sequence(_) | Self::Mapping(_) | Self::JavaScript(_) | Self::Entry(_) => true,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
struct WorkingEntry {
    fields: IndexMap<String, WorkingNode>,
}

impl WorkingEntry {
    fn id(&self) -> Option<ProfileEntryId> {
        match self.fields.get("id") {
            Some(WorkingNode::String(value)) => Some(ProfileEntryId::from_wire(value)),
            Some(
                WorkingNode::Null
                | WorkingNode::Bool(_)
                | WorkingNode::Number(_)
                | WorkingNode::Sequence(_)
                | WorkingNode::Mapping(_)
                | WorkingNode::JavaScript(_)
                | WorkingNode::Entry(_),
            )
            | None => None,
        }
    }

    fn name(&self) -> Option<&str> {
        match self.fields.get("name") {
            Some(WorkingNode::String(value)) => Some(value),
            Some(
                WorkingNode::Null
                | WorkingNode::Bool(_)
                | WorkingNode::Number(_)
                | WorkingNode::Sequence(_)
                | WorkingNode::Mapping(_)
                | WorkingNode::JavaScript(_)
                | WorkingNode::Entry(_),
            )
            | None => None,
        }
    }

    fn is_group(&self) -> bool {
        self.fields
            .get("group")
            .is_some_and(WorkingNode::is_javascript_truthy)
    }
}

#[derive(Default)]
struct Composer {
    entries: Vec<WorkingEntry>,
    roots: Vec<usize>,
    entry_map: HashMap<ProfileEntryId, usize>,
    warnings: Vec<ProfilePatchWarning>,
}

impl Composer {
    fn allocate_entry(
        &mut self,
        entry: &ProfileEntry,
        context: &str,
    ) -> Result<usize, ProfilePatchError> {
        let nested = if entry.group().is_some_and(ProfileNode::is_javascript_truthy) {
            entry.config().and_then(|config| match config {
                ProfileNode::Sequence(values) => Some(values.clone()),
                ProfileNode::Null
                | ProfileNode::Bool(_)
                | ProfileNode::Number(_)
                | ProfileNode::String(_)
                | ProfileNode::Mapping(_)
                | ProfileNode::JavaScript(_) => None,
            })
        } else {
            None
        };
        let fields = entry
            .fields
            .iter()
            .map(|(key, value)| (key.clone(), WorkingNode::from_profile(value)))
            .collect();
        let handle = self.entries.len();
        self.entries.push(WorkingEntry { fields });
        if let Some(id) = self.entries[handle].id().filter(ProfileEntryId::is_truthy) {
            self.entry_map.insert(id, handle);
        }
        if let Some(nested) = nested {
            let mut children = Vec::with_capacity(nested.len());
            for (index, child) in nested.into_iter().enumerate() {
                let ProfileNode::Mapping(fields) = child else {
                    return Err(ProfilePatchError::MappingRequired {
                        context: format!("{context} child at index {index}"),
                    });
                };
                let child_context = format!("{context} child at index {index}");
                let entry = ProfileEntry::from_fields(fields);
                let child = self.allocate_entry(&entry, &child_context)?;
                children.push(WorkingNode::Entry(child));
            }
            self.entries[handle]
                .fields
                .insert("config".to_owned(), WorkingNode::Sequence(children));
        }
        Ok(handle)
    }

    fn apply_patch(
        &mut self,
        patch: &ProfilePatch,
        patch_index: usize,
    ) -> Result<(), ProfilePatchError> {
        if let Some(insert) = patch
            .insert()
            .filter(|insert| insert.is_javascript_truthy())
        {
            let target = patch.id().filter(ProfileEntryId::is_truthy);
            if let Some(id) = target {
                let Some(&handle) = self.entry_map.get(&id) else {
                    self.warnings
                        .push(ProfilePatchWarning::InsertTargetNotFound { id });
                    return Ok(());
                };
                if !self.entries[handle].is_group() {
                    self.warnings
                        .push(ProfilePatchWarning::InsertTargetNotGroup { id });
                    return Ok(());
                }
                let insertions = insertion_entries(insert, patch_index)?;
                let inserted = self.allocate_insertions(&insertions, patch_index)?;
                let config = self.entries[handle]
                    .fields
                    .entry("config".to_owned())
                    .or_insert_with(|| WorkingNode::Sequence(Vec::new()));
                if !matches!(config, WorkingNode::Sequence(_)) {
                    *config = WorkingNode::Sequence(Vec::new());
                }
                let WorkingNode::Sequence(config) = config else {
                    unreachable!("config was normalized to a sequence")
                };
                config.extend(inserted.into_iter().map(WorkingNode::Entry));
            } else {
                let insertions = insertion_entries(insert, patch_index)?;
                let inserted = self.allocate_insertions(&insertions, patch_index)?;
                self.roots.extend(inserted);
            }
            return Ok(());
        }

        let Some(id) = patch.id().filter(ProfileEntryId::is_truthy) else {
            self.warnings.push(ProfilePatchWarning::MissingId);
            return Ok(());
        };
        let Some(&handle) = self.entry_map.get(&id) else {
            self.warnings
                .push(ProfilePatchWarning::TargetNotFound { id });
            return Ok(());
        };
        if let Some(actual) = patch.name().filter(|name| !name.is_empty()) {
            let expected = self.entries[handle].name().map(str::to_owned);
            if expected.as_deref() != Some(actual) {
                self.warnings.push(ProfilePatchWarning::NameMismatch {
                    id,
                    expected,
                    actual: actual.to_owned(),
                });
                return Ok(());
            }
        }
        for (key, value) in &patch.fields {
            if matches!(key.as_str(), "id" | "insert" | "name") {
                continue;
            }
            self.entries[handle]
                .fields
                .insert(key.clone(), WorkingNode::from_profile(value));
        }
        Ok(())
    }

    fn allocate_insertions(
        &mut self,
        insertions: &[ProfileEntry],
        patch_index: usize,
    ) -> Result<Vec<usize>, ProfilePatchError> {
        insertions
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                self.allocate_entry(
                    entry,
                    &format!("insert entry {index} in patch {patch_index}"),
                )
            })
            .collect()
    }

    fn finish_entries(self) -> Vec<ProfileEntry> {
        self.roots
            .iter()
            .map(|handle| self.export_entry(*handle))
            .collect()
    }

    fn export_entry(&self, handle: usize) -> ProfileEntry {
        ProfileEntry::from_fields(
            self.entries[handle]
                .fields
                .iter()
                .map(|(key, value)| (key.clone(), self.export_node(value)))
                .collect(),
        )
    }

    fn export_node(&self, value: &WorkingNode) -> ProfileNode {
        match value {
            WorkingNode::Null => ProfileNode::Null,
            WorkingNode::Bool(value) => ProfileNode::Bool(*value),
            WorkingNode::Number(value) => ProfileNode::Number(*value),
            WorkingNode::String(value) => ProfileNode::String(value.clone()),
            WorkingNode::Sequence(values) => {
                ProfileNode::Sequence(values.iter().map(|value| self.export_node(value)).collect())
            }
            WorkingNode::Mapping(fields) => ProfileNode::Mapping(
                fields
                    .iter()
                    .map(|(key, value)| (key.clone(), self.export_node(value)))
                    .collect(),
            ),
            WorkingNode::JavaScript(expression) => ProfileNode::JavaScript(expression.clone()),
            WorkingNode::Entry(handle) => ProfileNode::Mapping(self.export_entry(*handle).fields),
        }
    }
}

fn insertion_entries(
    insert: &ProfileNode,
    patch_index: usize,
) -> Result<Vec<ProfileEntry>, ProfilePatchError> {
    let ProfileNode::Sequence(entries) = insert else {
        return Err(ProfilePatchError::InsertArrayRequired { patch_index });
    };
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| match entry {
            ProfileNode::Mapping(fields) => Ok(ProfileEntry::from_fields(fields.clone())),
            ProfileNode::Null
            | ProfileNode::Bool(_)
            | ProfileNode::Number(_)
            | ProfileNode::String(_)
            | ProfileNode::Sequence(_)
            | ProfileNode::JavaScript(_) => Err(ProfilePatchError::MappingRequired {
                context: format!("insert entry {index} in patch {patch_index}"),
            }),
        })
        .collect()
}
