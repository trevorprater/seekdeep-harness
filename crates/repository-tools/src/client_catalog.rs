//! The model-facing client slot catalog consumed by `cordis_inspect
//! what:"client"`. A dynamic package's browser half can only contribute UI
//! through `ctx.slots.register`, and every fact it needs to do that safely —
//! which keys exist, what each register call must pass, what the component
//! receives, who already occupies the seat, and when the seat exists at all —
//! is decided at compile time by the shipped web bundle. This generator reads
//! those facts lexically (no type-checker program) and emits them as data so
//! the host-side toolset teaches the browser surface without importing a
//! single client runtime module.

use std::path::Path;

use indexmap::IndexMap;
use serde::Serialize;

use crate::{
    slot_walk::{
        ScannedFile, SlotDeclaration, SlotRegistration, TypeDeclaration, declared_types,
        index_exported_types, referenced_type_names, scan_slot_files, slot_declarations,
        slot_registrations, standard_kit_members,
    },
    ts_lexical::{collapse, locale_compare},
};

/// Source globs: every workspace package's sources, `.tsx` included (a contract may live in one).
pub const SOURCE_GLOBS: [&str; 2] = ["packages/*/*/src/**/*.ts", "packages/*/*/src/**/*.tsx"];

/// Slot cardinalities the contract allows.
pub const KINDS: [&str; 4] = ["single", "list", "keyed", "chain"];
/// Slot data scopes the contract allows.
pub const SCOPES: [&str; 3] = ["root", "session", "session-maybe"];

/// Declarations longer than this render truncated; the full shape stays in source.
const MAX_DECL_CHARS: usize = 1200;

/// Line budget for ONE slot's expanded report: a report a model cannot finish
/// reading is a defect rather than a detail.
const MAX_ENTRY_LINES: usize = 120;

/// One register-call option as the catalog teaches it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OptionDoc {
    /// Option name as written in the register options object.
    pub name: &'static str,
    /// Whether the cardinality requires it.
    pub requirement: &'static str,
    /// Accepted type, in source spelling.
    #[serde(rename = "type")]
    pub type_name: &'static str,
    /// What it does, from the registrant's side.
    pub doc: &'static str,
}

const LIST_OPTIONS: [OptionDoc; 3] = [
    OptionDoc {
        name: "id",
        requirement: "required",
        type_name: "string",
        doc: "Your cell key. Use an id of your own: a fresh id is added beside the shipped entries, while reusing a shipped id puts you in THAT cell and replaces it. Owners that filter by id address you by it.",
    },
    OptionDoc {
        name: "order",
        requirement: "optional",
        type_name: "number",
        doc: "Position among the entries, ascending (default 0).",
    },
    OptionDoc {
        name: "label",
        requirement: "optional",
        type_name: "string | (() => string)",
        doc: "Display text where the owner projects one (nav rows, tabs). A thunk is re-read on every projection, so localized text follows the active locale without re-registering.",
    },
];

const KEYED_OPTIONS: [OptionDoc; 1] = [OptionDoc {
    name: "key",
    requirement: "required",
    type_name: "string",
    doc: "Your cell key: the entry renders where the owner dispatches this exact key. Registering an already-occupied key replaces that occupant.",
}];

const CHAIN_OPTIONS: [OptionDoc; 1] = [OptionDoc {
    name: "select",
    requirement: "required",
    type_name: "(owner) => unknown | null",
    doc: "Pure routing selector. Entries are tried in ascending order; the first non-null result wins and arrives as the component's `matched` prop. All-null falls through to the owner's fallback.",
}];

/// Register options per cardinality, curated from `KindOptions` in the slots
/// package — the authority for what a register call may pass.
#[must_use]
pub fn register_options(kind: &str) -> &'static [OptionDoc] {
    match kind {
        "list" => &LIST_OPTIONS,
        "keyed" => &KEYED_OPTIONS,
        "chain" => &CHAIN_OPTIONS,
        _ => &[],
    }
}

/// The one register option a dynamic package must NOT pass, and why.
const PRIORITY_NOTE: &str = "Do NOT pass `priority`: the browser-half facade assigns one automatically, and it is LOWER than every shipped entry — in a single or keyed cell that means your entry is the one that renders.";

/// Cross-cutting rules a registrant needs once, not per slot.
pub const CLIENT_NOTES: [&str; 6] = [
    "Contribute UI only through `ctx.slots.register(options, Component)`; declare `inject: ['slots']` in your returned plugin (object form) or the seat is withheld.",
    "Wrap every registration in `ctx.slots.inject(key, () => ctx.slots.register(...))`. A slot exists only while the entry that declared it is mounted, and registering into an undeclared slot throws; `inject` runs your registration when the declaration is (or becomes) live and re-runs it if the owner remounts.",
    PRIORITY_NOTE,
    "You cannot `import` anything, so the design-system components are out of reach: build markup with `React.createElement` and ship CSS through `styles.insert(css)`. Use the theme CSS variables (`var(--dsw-alias-bg-layer-1)`, `var(--dsw-alias-label-primary)`, …) instead of literal colors, or your contribution breaks in the other color scheme.",
    "Every component receives the framework hook seats listed under `framework props` for its scope; a selector hook is called with a selector, e.g. `useSessions(state => state.current)`.",
    "This catalog is the COMPILE-TIME contract of the shipped web bundle, not a snapshot of one page: a key is registrable only where the owner that declares it is mounted. A failed registration surfaces in the browser-half load report — read it back with `cordis_inspect what:\"temporary\"`.",
];

/// Standard-kit interface that applies to each scope, beyond the global one.
fn scope_kit(scope: &str) -> Option<&'static str> {
    match scope {
        "session" => Some("SessionStandardProps"),
        "session-maybe" => Some("SessionMaybeStandardProps"),
        _ => None,
    }
}

/// One resolved catalog entry, ready to render.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SlotEntry {
    /// `SlotMap` key passed as the register call's `name`.
    pub key: String,
    /// Cardinality: `single`, `list`, `keyed`, or `chain`.
    pub kind: String,
    /// Data scope: `root`, `session`, or `session-maybe`.
    pub scope: String,
    /// First sentence of the contract prose.
    pub summary: String,
    /// Full contract prose from the `SlotMap` declaration.
    pub doc: String,
    /// Options this cardinality accepts (beyond `name`).
    pub register_options: Vec<OptionDoc>,
    /// Declarations of the props the owner passes down, with their own documentation.
    pub owner_props: Vec<String>,
    /// Names of the shapes those props reference; deliberately not expanded here.
    pub owner_props_references: Vec<String>,
    /// Framework-supplied component props for this scope.
    pub standard_props: Vec<String>,
    /// For keyed slots: how the key set is constrained and which keys are taken.
    pub key_domain: String,
    /// Opaque per-render-site context passed to slot-level hooks, when the slot declares one.
    pub hook_context: String,
    /// Slot-level inject face every entry receives, when the slot declares one.
    pub slot_inject: String,
    /// Which mounted entry makes this slot exist.
    pub declared_by: String,
    /// Entries the shipped composition already registered here.
    pub occupants: Vec<String>,
    /// `shadows-shipped-ui` when registering here replaces shipped UI; `none` when additive.
    pub replace_risk: String,
    /// A minimal browser half that registers into this slot.
    pub example: String,
    /// Source pointer of the contract declaration.
    pub source: String,
}

/// Read the workspace and resolve every catalog entry, failing loud on a
/// contract the catalog cannot teach.
///
/// # Errors
/// Returns scan failures, a declared slot that is unteachable, a scan that
/// contradicts itself, or a slot whose report exceeds the per-slot budget.
pub fn collect_slot_entries(scan_root: &Path) -> anyhow::Result<Vec<SlotEntry>> {
    let files = scan_slot_files(scan_root, &SOURCE_GLOBS)?;
    let declarations = files.iter().flat_map(slot_declarations).collect::<Vec<_>>();
    let registrations = files
        .iter()
        .flat_map(slot_registrations)
        .collect::<Vec<_>>();
    let types = index_exported_types(scan_root, &SOURCE_GLOBS)?;
    let problems = validate_slot_contracts(&declarations, &registrations, &types);
    if !problems.is_empty() {
        anyhow::bail!(
            "gen-client-catalog: {} contract violation(s):\n{}",
            problems.len(),
            problems
                .iter()
                .map(|problem| format!("  {problem}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    let entries = resolve_slot_entries(
        &declarations,
        &registrations,
        &types,
        &standard_kits(&files),
    );
    let oversized = oversized_slot_reports(&entries);
    if !oversized.is_empty() {
        anyhow::bail!(
            "gen-client-catalog: {} slot(s) exceed the per-slot report budget of {MAX_ENTRY_LINES} lines:\n{}",
            oversized.len(),
            oversized
                .iter()
                .map(|problem| format!("  {problem}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    Ok(entries)
}

/// Slots whose expanded report exceeds the per-slot line budget.
#[must_use]
pub fn oversized_slot_reports(entries: &[SlotEntry]) -> Vec<String> {
    entries
        .iter()
        .filter(|entry| entry_lines(entry) > MAX_ENTRY_LINES)
        .map(|entry| {
            format!(
                "slot '{}' ({}) reports {} lines. Narrow the owner share it passes down (a slot hands a registrant a share, not a subsystem) or tighten its prose, so asking about one slot stays cheaper than asking about all of them.",
                entry.key,
                entry.source,
                entry_lines(entry)
            )
        })
        .collect()
}

/// Line count of one entry's variable-length content, the proxy for its rendered report.
fn entry_lines(entry: &SlotEntry) -> usize {
    let blocks = [entry.doc.as_str(), entry.example.as_str()]
        .into_iter()
        .chain(entry.owner_props.iter().map(String::as_str))
        .chain(entry.register_options.iter().map(|option| option.doc));
    blocks.map(|block| block.split('\n').count()).sum::<usize>()
        + entry.standard_props.len()
        + entry.owner_props_references.len()
        + entry.occupants.len()
}

/// Fail-closed contract checks: an unteachable slot must break the gate rather
/// than ship an entry a model cannot act on.
#[must_use]
pub fn validate_slot_contracts(
    declarations: &[SlotDeclaration],
    registrations: &[SlotRegistration],
    types: &IndexMap<String, TypeDeclaration>,
) -> Vec<String> {
    let mut problems = Vec::new();
    let mut by_key: IndexMap<&str, &SlotDeclaration> = IndexMap::new();
    for declaration in declarations {
        let where_ = format!("slot '{}' ({})", declaration.key, declaration.source);
        if let Some(previous) = by_key.get(declaration.key.as_str()) {
            problems.push(format!(
                "{where_} is also declared at {}; SlotMap merges duplicates silently, so the catalog cannot tell which documentation wins.",
                previous.source
            ));
            continue;
        }
        by_key.insert(declaration.key.as_str(), declaration);
        if !KINDS.contains(&declaration.kind.as_str()) {
            problems.push(format!(
                "{where_} has no literal 'kind'; the catalog derives the register options from it, so it must be one of {}.",
                KINDS.join("/")
            ));
        }
        if !SCOPES.contains(&declaration.scope.as_str()) {
            problems.push(format!(
                "{where_} has no literal 'scope'; the catalog derives the framework props from it, so it must be one of {}.",
                SCOPES.join("/")
            ));
        }
        if doc_prose(&declaration.js_doc).is_empty() {
            problems.push(format!(
                "{where_} has no JSDoc prose. Write it from the REGISTRANT's side: what to pass, what the component receives, whom a registration replaces, and what absence looks like (packages/client/ui-settings/src/client/contract/slots.ts is the template)."
            ));
        }
        if let Some(owner) = &declaration.owner_type
            && is_identifier(owner)
            && !types.contains_key(owner)
        {
            problems.push(format!(
                "{where_} names owner props '{owner}' that no exported declaration provides; export the interface so the catalog can show what the component receives."
            ));
        }
    }
    for registration in registrations {
        if !by_key.contains_key(registration.key.as_str()) {
            problems.push(format!(
                "registration into '{}' ({}) targets a slot no SlotMap merge declares; either the scan has a blind spot or the registration is dead.",
                registration.key, registration.source
            ));
        }
        for child in &registration.children {
            if !by_key.contains_key(child.as_str()) {
                problems.push(format!(
                    "registration at {} declares child slot '{child}' that no SlotMap merge types.",
                    registration.source
                ));
            }
        }
    }
    problems
}

fn is_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_' || first == '$')
        && characters.all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '$'
        })
}

/// Project validated declarations into catalog entries, sorted by key.
#[must_use]
pub fn resolve_slot_entries(
    declarations: &[SlotDeclaration],
    registrations: &[SlotRegistration],
    types: &IndexMap<String, TypeDeclaration>,
    kits: &IndexMap<String, Vec<String>>,
) -> Vec<SlotEntry> {
    let mut declared_by: IndexMap<&str, &SlotRegistration> = IndexMap::new();
    for registration in registrations {
        for child in &registration.children {
            declared_by.entry(child.as_str()).or_insert(registration);
        }
    }
    let mut entries = declarations
        .iter()
        .map(|declaration| {
            entry_of(
                declaration,
                registrations,
                declared_by.get(declaration.key.as_str()).copied(),
                types,
                kits,
            )
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| locale_compare(&left.key, &right.key));
    entries
}

/// The framework prop seats per scope, read from the merged standard-kit interfaces.
#[must_use]
pub fn standard_kits(files: &[ScannedFile]) -> IndexMap<String, Vec<String>> {
    let global = standard_kit_members(files, "GlobalStandardProps");
    let mut kits = IndexMap::new();
    for scope in SCOPES {
        let mut members = global.clone();
        if let Some(extra) = scope_kit(scope) {
            members.extend(standard_kit_members(files, extra));
        }
        kits.insert(scope.to_owned(), members);
    }
    kits
}

fn entry_of(
    declaration: &SlotDeclaration,
    registrations: &[SlotRegistration],
    declared_by: Option<&SlotRegistration>,
    types: &IndexMap<String, TypeDeclaration>,
    kits: &IndexMap<String, Vec<String>>,
) -> SlotEntry {
    let occupants = registrations
        .iter()
        .filter(|registration| registration.key == declaration.key)
        .collect::<Vec<_>>();
    let cell_occupied = occupants
        .iter()
        .any(|occupant| declaration.kind == "single" || occupant.entry_key.is_some());
    let doc = doc_prose(&declaration.js_doc);
    let (owner_declarations, owner_references) =
        owner_shapes(declaration.owner_type.as_deref(), types);
    SlotEntry {
        key: declaration.key.clone(),
        kind: declaration.kind.clone(),
        scope: declaration.scope.clone(),
        summary: first_sentence(&doc),
        doc,
        register_options: register_options(&declaration.kind).to_vec(),
        owner_props: owner_declarations
            .iter()
            .map(|declaration| truncate(&declaration.text))
            .collect(),
        owner_props_references: owner_references,
        standard_props: kits.get(&declaration.scope).cloned().unwrap_or_default(),
        key_domain: key_domain_of(declaration, &occupants),
        hook_context: declaration.hook_context.clone().unwrap_or_default(),
        slot_inject: declaration.inject_type.clone().unwrap_or_default(),
        declared_by: declared_by.map_or_else(
            || "the runtime itself (built in; always present)".to_owned(),
            |parent| {
                format!(
                    "an entry in '{}' ({}), so it exists while that entry is mounted",
                    parent.key,
                    short_package(&parent.package)
                )
            },
        ),
        occupants: occupants
            .iter()
            .map(|occupant| {
                let mut parts = vec![short_package(&occupant.package), occupant.component.clone()];
                if let Some(id) = &occupant.id {
                    parts.push(format!("id '{id}'"));
                }
                if let Some(key) = &occupant.entry_key {
                    parts.push(format!("key '{key}'"));
                }
                parts.join(" ")
            })
            .collect(),
        replace_risk: if cell_occupied
            && (declaration.kind == "single" || declaration.kind == "keyed")
        {
            "shadows-shipped-ui".to_owned()
        } else {
            "none".to_owned()
        },
        example: example_of(declaration),
        source: declaration.source.clone(),
    }
}

/// The owner-props contract at ONE level: the owner declaration(s) themselves,
/// plus the names of the shapes their fields reference.
fn owner_shapes(
    owner_type: Option<&str>,
    types: &IndexMap<String, TypeDeclaration>,
) -> (Vec<TypeDeclaration>, Vec<String>) {
    let Some(owner_type) = owner_type else {
        return (Vec::new(), Vec::new());
    };
    let declarations = declared_types(
        &referenced_type_names(&[owner_type.to_owned()], types),
        types,
    );
    let own = declarations
        .iter()
        .map(|declaration| declaration.name.as_str())
        .collect::<Vec<_>>();
    let seeds = declarations
        .iter()
        .map(|declaration| declaration.text.clone())
        .collect::<Vec<_>>();
    let references = referenced_type_names(&seeds, types)
        .into_iter()
        .filter(|name| !own.contains(&name.as_str()))
        .collect();
    (declarations, references)
}

/// How a keyed slot's key domain is constrained, empty for the other kinds.
fn key_domain_of(declaration: &SlotDeclaration, occupants: &[&SlotRegistration]) -> String {
    if declaration.kind != "keyed" {
        return String::new();
    }
    let mut taken = occupants
        .iter()
        .filter_map(|occupant| occupant.entry_key.clone())
        .collect::<Vec<_>>();
    taken.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
    taken.dedup();
    let shipped = if taken.is_empty() {
        "none are taken yet".to_owned()
    } else {
        format!("already taken: {}", taken.join(", "))
    };
    match &declaration.key_props {
        None => {
            format!("open: any string the owner dispatches (no compile-time key set), {shipped}")
        }
        Some(key_props) => format!("fixed by the owner's key table {key_props}, {shipped}"),
    }
}

/// A runnable minimal registration for one slot, per cardinality.
fn example_of(declaration: &SlotDeclaration) -> String {
    let extra: &[&str] = match declaration.kind.as_str() {
        "list" => &["id: 'my-entry'", "order: 100", "label: 'My entry'"],
        "keyed" => &["key: '<one key the owner dispatches>'"],
        "chain" => &["select: owner => null"],
        _ => &[],
    };
    let options = std::iter::once(format!("name: '{}'", declaration.key))
        .chain(extra.iter().map(|option| (*option).to_owned()))
        .collect::<Vec<_>>()
        .join(", ");
    [
        "return {".to_owned(),
        "  inject: ['slots'],".to_owned(),
        "  apply(ctx) {".to_owned(),
        format!(
            "    ctx.slots.inject('{}', () => ctx.slots.register(",
            declaration.key
        ),
        format!("      {{ {options} }},"),
        "      () => React.createElement('div', null, 'hello'),".to_owned(),
        "    ))".to_owned(),
        "  },".to_owned(),
        "}".to_owned(),
    ]
    .join("\n")
}

/// Drop the workspace scope prefix so rows stay readable.
fn short_package(name: &str) -> String {
    name.replacen("@deepseek-ai/dsh-", "", 1)
        .replacen("@seekdeep-ai/seekdeep-", "", 1)
}

/// Truncate an over-long declaration, naming the truncation.
fn truncate(text: &str) -> String {
    let units = text.encode_utf16().collect::<Vec<_>>();
    if units.len() > MAX_DECL_CHARS {
        format!(
            "{} /* …truncated — full shape in source */",
            String::from_utf16_lossy(&units[..MAX_DECL_CHARS])
        )
    } else {
        text.to_owned()
    }
}

/// `JSDoc` prose: comment markers and block tags removed, paragraphs kept.
#[must_use]
pub fn doc_prose(js_doc: &str) -> String {
    let body = js_doc.strip_prefix("/**").unwrap_or(js_doc);
    let body = body.strip_suffix("*/").unwrap_or(body);
    let mut kept = Vec::new();
    for line in body.split('\n') {
        let mut rest = line.trim_start_matches(crate::jsdoc::is_js_space);
        if let Some(after_star) = rest.strip_prefix('*') {
            rest = after_star;
            if let Some(first) = rest.chars().next()
                && crate::jsdoc::is_js_space(first)
            {
                rest = &rest[first.len_utf8()..];
            }
        }
        let line = rest.trim_end_matches(crate::jsdoc::is_js_space);
        if line
            .trim_start_matches(crate::jsdoc::is_js_space)
            .starts_with('@')
        {
            break;
        }
        kept.push(line.to_owned());
    }
    let joined = unlink(&kept.join("\n"));
    collapse_blank_runs(&joined)
        .trim_matches(crate::jsdoc::is_js_space)
        .to_owned()
}

/// `{@link X}` → `X` (label included, as the source keeps it).
fn unlink(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{@link") {
        let after = &rest[start + "{@link".len()..];
        let Some(space) = after
            .chars()
            .next()
            .filter(|character| crate::jsdoc::is_js_space(*character))
        else {
            output.push_str(&rest[..start + "{@link".len()]);
            rest = after;
            continue;
        };
        let after_space = &after[space.len_utf8()..];
        let Some(close) = after_space.find('}') else {
            output.push_str(rest);
            return output;
        };
        let inner = &after_space[..close];
        let mut inner_trimmed = inner;
        while let Some(first) = inner_trimmed
            .chars()
            .next()
            .filter(|character| crate::jsdoc::is_js_space(*character))
        {
            inner_trimmed = &inner_trimmed[first.len_utf8()..];
        }
        if inner_trimmed.is_empty() {
            output.push_str(&rest[..start + "{@link".len()]);
            rest = after;
            continue;
        }
        output.push_str(&rest[..start]);
        output.push_str(inner_trimmed);
        rest = &after_space[close + 1..];
    }
    output.push_str(rest);
    output
}

/// `\n{3,}` → `\n\n`.
fn collapse_blank_runs(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut newlines = 0;
    for character in text.chars() {
        if character == '\n' {
            newlines += 1;
            if newlines <= 2 {
                output.push('\n');
            }
        } else {
            newlines = 0;
            output.push(character);
        }
    }
    output
}

/// First sentence of a prose block, for the compact listing.
#[must_use]
pub fn first_sentence(doc: &str) -> String {
    let flat = collapse(doc);
    let characters = flat.char_indices().collect::<Vec<_>>();
    for (position, (offset, character)) in characters.iter().enumerate() {
        if !matches!(character, '.' | '!' | '?') {
            continue;
        }
        let next = characters.get(position + 1).map(|(_, next)| *next);
        if next.is_none_or(crate::jsdoc::is_js_space) {
            return flat[..offset + character.len_utf8()]
                .trim_matches(crate::jsdoc::is_js_space)
                .to_owned();
        }
    }
    flat.trim_matches(crate::jsdoc::is_js_space).to_owned()
}

/// Render one value as a single-quoted TypeScript literal.
fn quote(value: &str) -> String {
    format!(
        "'{}'",
        value
            .replace('\\', "\\\\")
            .replace('\'', "\\'")
            .replace('\n', "\\n")
    )
}

/// Render a readonly string-array literal.
fn list(values: &[String], indent: &str) -> String {
    if values.is_empty() {
        return "[]".to_owned();
    }
    let mut lines = vec!["[".to_owned()];
    lines.extend(
        values
            .iter()
            .map(|value| format!("{indent}  {},", quote(value))),
    );
    lines.push(format!("{indent}]"));
    lines.join("\n")
}

/// The catalog as the data object the Rust runtime embeds: authoring notes
/// plus every entry, sorted by key.
#[must_use]
pub fn client_catalog_json(entries: &[SlotEntry]) -> serde_json::Value {
    serde_json::json!({ "notes": CLIENT_NOTES, "entries": entries })
}

/// Render the generated data module in its source-compatible TypeScript form.
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "one rendered module, in source order"
)]
pub fn render_client_catalog(entries: &[SlotEntry]) -> String {
    let mut lines: Vec<String> = [
        "/**",
        " * Generated by scripts/gen-client-catalog.ts — do not edit by hand; run",
        " * `pnpm run gen-client-catalog` to regenerate (freshness-gated by",
        " * `pnpm run verify-client-catalog` in doc-sync).",
        " *",
        " * The compile-time contract of the shipped web bundle's slot surface, as",
        " * `cordis_inspect what:\"client\"` serves it to the model: every SlotMap key a",
        " * browser half can register into, what that register call must pass, what the",
        " * component receives, who already occupies the seat, and which owner has to be",
        " * mounted for the seat to exist. Data only — this module is the one legitimate",
        " * meeting point of the two planes, so it carries strings, never client imports.",
        " *",
        " * @module @seekdeep-ai/seekdeep-cordis-client-runner/client/slot-catalog",
        " */",
        "",
        "/* jscpd:ignore-start */",
        "/** One option a register call passes for a given slot cardinality. */",
        "export interface ClientSlotOption {",
        "  /** Option name as written in the register options object. */",
        "  name: string",
        "  /** Whether the cardinality requires it. */",
        "  requirement: string",
        "  /** Accepted type, in source spelling. */",
        "  type: string",
        "  /** What it does, from the registrant's side. */",
        "  doc: string",
        "}",
        "",
        "/** One browser-half slot a dynamic package can contribute UI into. */",
        "export interface ClientSlotEntry {",
        "  /** SlotMap key passed as the register call's `name`. */",
        "  key: string",
        "  /** Cardinality: `single`, `list`, `keyed`, or `chain`. */",
        "  kind: string",
        "  /** Data scope: `root`, `session`, or `session-maybe`. */",
        "  scope: string",
        "  /** First sentence of the contract prose. */",
        "  summary: string",
        "  /** Full contract prose from the SlotMap declaration. */",
        "  doc: string",
        "  /** Options this cardinality accepts (beyond `name`). */",
        "  registerOptions: readonly ClientSlotOption[]",
        "  /** Declarations of the props the owner passes down, with their own documentation. */",
        "  ownerProps: readonly string[]",
        "  /** Names of the shapes those props reference; deliberately not expanded here. */",
        "  ownerPropsReferences: readonly string[]",
        "  /** Framework-supplied component props for this scope. */",
        "  standardProps: readonly string[]",
        "  /** For keyed slots: how the key set is constrained and which keys are taken. */",
        "  keyDomain: string",
        "  /** Opaque per-render-site context passed to slot-level hooks, when the slot declares one. */",
        "  hookContext: string",
        "  /** Slot-level inject face every entry receives, when the slot declares one. */",
        "  slotInject: string",
        "  /** Which mounted entry makes this slot exist. */",
        "  declaredBy: string",
        "  /** Entries the shipped composition already registered here. */",
        "  occupants: readonly string[]",
        "  /** `shadows-shipped-ui` when registering here replaces shipped UI; `none` when additive. */",
        "  replaceRisk: string",
        "  /** A minimal browser half that registers into this slot. */",
        "  example: string",
        "  /** Source pointer of the contract declaration. */",
        "  source: string",
        "}",
        "",
        "/** Rules that apply to every browser-half contribution, in reading order. */",
        "export const CLIENT_NOTES: readonly string[] = [",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    lines.extend(
        CLIENT_NOTES
            .iter()
            .map(|note| format!("  {},", quote(note))),
    );
    lines.extend(
        [
            "]",
            "",
            "/** Every slot the shipped web bundle declares, sorted by key. */",
            "// Seats of one cardinality repeat their register options and framework props",
            "// verbatim; that sameness IS the contract a registrant reads, so clone",
            "// detection is told to skip the data rather than the file.",
            "export const CLIENT_SLOT_API: readonly ClientSlotEntry[] = [",
        ]
        .into_iter()
        .map(str::to_owned),
    );
    for entry in entries {
        lines.push("  {".to_owned());
        lines.push(format!("    key: {},", quote(&entry.key)));
        lines.push(format!("    kind: {},", quote(&entry.kind)));
        lines.push(format!("    scope: {},", quote(&entry.scope)));
        lines.push(format!("    summary: {},", quote(&entry.summary)));
        lines.push(format!("    doc: {},", quote(&entry.doc)));
        if entry.register_options.is_empty() {
            lines.push("    registerOptions: [],".to_owned());
        } else {
            lines.push("    registerOptions: [".to_owned());
            for option in &entry.register_options {
                lines.push("      {".to_owned());
                lines.push(format!("        name: {},", quote(option.name)));
                lines.push(format!(
                    "        requirement: {},",
                    quote(option.requirement)
                ));
                lines.push(format!("        type: {},", quote(option.type_name)));
                lines.push(format!("        doc: {},", quote(option.doc)));
                lines.push("      },".to_owned());
            }
            lines.push("    ],".to_owned());
        }
        lines.push(format!(
            "    ownerProps: {},",
            list(&entry.owner_props, "    ")
        ));
        lines.push(format!(
            "    ownerPropsReferences: {},",
            list(&entry.owner_props_references, "    ")
        ));
        lines.push(format!(
            "    standardProps: {},",
            list(&entry.standard_props, "    ")
        ));
        lines.push(format!("    keyDomain: {},", quote(&entry.key_domain)));
        lines.push(format!("    hookContext: {},", quote(&entry.hook_context)));
        lines.push(format!("    slotInject: {},", quote(&entry.slot_inject)));
        lines.push(format!("    declaredBy: {},", quote(&entry.declared_by)));
        lines.push(format!(
            "    occupants: {},",
            list(&entry.occupants, "    ")
        ));
        lines.push(format!("    replaceRisk: {},", quote(&entry.replace_risk)));
        lines.push(format!("    example: {},", quote(&entry.example)));
        lines.push(format!("    source: {},", quote(&entry.source)));
        lines.push("  },".to_owned());
    }
    lines.extend(
        ["]", "/* jscpd:ignore-end */", ""]
            .into_iter()
            .map(str::to_owned),
    );
    lines.join("\n")
}
