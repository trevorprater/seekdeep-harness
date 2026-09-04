//! Structured runtime catalog and its source-compatible text artifact.

use std::collections::HashSet;

use indexmap::IndexMap;
use seekdeep_cordis_api_catalog::RuntimeApiCatalog;
use serde_json::{Value, json};

use super::{
    EventEntry, InheritedEntry, ServiceEntry, is_source_file, jsdoc, runtime_text, source_owner,
};
use crate::{
    Result,
    model::{DeclarationKind, SourceDeclarationModel, TypertFace},
    text::locale_compare,
};

pub(super) fn referenced_types(
    services: &[&ServiceEntry],
    events: &[EventEntry],
    source_declarations: &[SourceDeclarationModel],
    face: TypertFace,
) -> Result<Vec<Value>> {
    let mut declarations = IndexMap::new();
    let mut ambiguous = HashSet::new();
    for declaration in source_declarations {
        if declaration.face != face || declaration.kind == DeclarationKind::Enum {
            continue;
        }
        let Some(owner) = source_owner(&declaration.location.file) else {
            continue;
        };
        if !is_source_file(&declaration.location.file[owner.len()..]) {
            continue;
        }
        if declarations.contains_key(&declaration.name) {
            ambiguous.insert(declaration.name.clone());
            continue;
        }
        let text = if declaration.text.encode_utf16().count() > 1500 {
            format!(
                "{} /* …truncated — full shape in source */",
                String::from_utf16_lossy(
                    &declaration
                        .text
                        .encode_utf16()
                        .take(1500)
                        .collect::<Vec<_>>()
                )
            )
        } else {
            declaration.text.clone()
        };
        declarations.insert(declaration.name.clone(), text);
    }
    for name in ambiguous {
        declarations.shift_remove(&name);
    }
    let mut included = IndexMap::new();
    let mut frontier = services
        .iter()
        .flat_map(|service| {
            service
                .methods
                .iter()
                .map(|method| method.signature.as_str())
        })
        .chain(events.iter().map(|event| event.signature.as_str()))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    while !frontier.is_empty() {
        let mut next = Vec::new();
        for (name, declaration) in &declarations {
            if included.contains_key(name) {
                continue;
            }
            let pattern = super::type_pattern(name)?;
            if frontier.iter().any(|text| pattern.is_match(text)) {
                included.insert(name.clone(), declaration.clone());
                next.push(declaration.clone());
            }
        }
        frontier = next;
    }
    let mut types = included.into_iter().collect::<Vec<_>>();
    types.sort_by(|(left, _), (right, _)| locale_compare(left, right));
    Ok(types
        .into_iter()
        .map(|(name, declaration)| json!({"name":name,"declaration":declaration}))
        .collect())
}

pub(super) fn catalog(
    services: &[&ServiceEntry],
    events: &[EventEntry],
    types: Vec<Value>,
    inherited: &[InheritedEntry],
) -> RuntimeApiCatalog {
    let services = services.iter().map(|service| {
        let methods = service.methods.iter().map(|method| {
            let contract = jsdoc::parse(&method.js_doc);
            let mut value = json!({"signature":method.signature,"description":contract.doc,"parameters":parameters(&contract.params)});
            if let Some(returns) = contract.returns { value["returns"] = json!(returns); }
            if !contract.throws.is_empty() { value["throws"] = json!(contract.throws); }
            value
        }).collect::<Vec<_>>();
        json!({"key":service.key,"summary":first_sentence(&service.doc),"description":service.doc,"methods":methods})
    }).collect();
    let mut events = events.iter().collect::<Vec<_>>();
    events.sort_by(|left, right| locale_compare(&left.name, &right.name));
    let events = events.into_iter().map(|event| {
        let contract = jsdoc::parse(&event.js_doc);
        json!({"name":event.name,"mode":event.mode,"signature":event.signature,"summary":first_sentence(&event.doc),"description":event.doc,"parameters":parameters(&contract.params)})
    }).collect();
    RuntimeApiCatalog {
        services,
        events,
        types,
        inherited_context: inherited
            .iter()
            .map(|entry| json!({"name":entry.name,"summary":entry.summary}))
            .collect(),
    }
}

fn parameters(values: &IndexMap<String, String>) -> Vec<Value> {
    values
        .iter()
        .map(|(name, description)| json!({"name":name,"description":description}))
        .collect()
}

fn first_sentence(doc: &str) -> &str {
    let line = doc.split('\n').next().unwrap_or_default();
    for (index, character) in line.char_indices() {
        if matches!(character, '\r' | '\u{2028}' | '\u{2029}') {
            break;
        }
        if matches!(character, '.' | '!' | '?')
            && line[index + 1..]
                .chars()
                .next()
                .is_none_or(jsdoc::is_whitespace)
        {
            return jsdoc::trim(&line[..=index]);
        }
    }
    jsdoc::trim(line)
}

pub(super) fn render(catalog: &RuntimeApiCatalog) -> String {
    let mut lines = runtime_text::HEADER
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for service in &catalog.services {
        render_service(service, &mut lines);
    }
    lines.extend([
        "]".to_owned(),
        String::new(),
        "/** Every harness event, sorted by name. */".to_owned(),
        "export const EVENT_API: readonly EventApiEntry[] = [".to_owned(),
    ]);
    for event in &catalog.events {
        lines.push("  {".to_owned());
        for key in ["name", "mode", "signature", "summary", "description"] {
            lines.push(format!("    {key}: {},", quote_value(&event[key])));
        }
        lines.push(format!(
            "    parameters: {},",
            render_parameters(&event["parameters"])
        ));
        lines.push("  },".to_owned());
    }
    lines.extend(["]".to_owned(), String::new(), "/** Shapes of every exported type the Service and Event signatures reference (transitively), sorted by name. */".to_owned(), "export const TYPE_API: readonly TypeApiEntry[] = [".to_owned()]);
    for ty in &catalog.types {
        lines.extend([
            "  {".to_owned(),
            format!("    name: {},", quote_value(&ty["name"])),
            format!("    declaration: {},", quote_value(&ty["declaration"])),
            "  },".to_owned(),
        ]);
    }
    lines.extend([
        "]".to_owned(),
        String::new(),
        "/** The inherited `ctx` API (cordis core + loader/hmr/timer), in curated order. */"
            .to_owned(),
        "export const INHERITED_CTX_API: readonly InheritedApiEntry[] = [".to_owned(),
    ]);
    for inherited in &catalog.inherited_context {
        lines.push(format!(
            "  {{ name: {}, summary: {} }},",
            quote_value(&inherited["name"]),
            quote_value(&inherited["summary"])
        ));
    }
    lines.extend([
        "]".to_owned(),
        String::new(),
        runtime_text::FOOTER.to_owned(),
    ]);
    lines.join("\n")
}

fn render_service(service: &Value, lines: &mut Vec<String>) {
    lines.push("  {".to_owned());
    for key in ["key", "summary", "description"] {
        lines.push(format!("    {key}: {},", quote_value(&service[key])));
    }
    let methods = service["methods"]
        .as_array()
        .expect("generated methods array");
    if methods.is_empty() {
        lines.push("    methods: [],".to_owned());
    } else {
        lines.push("    methods: [".to_owned());
        for method in methods {
            lines.push("      {".to_owned());
            for key in ["signature", "description"] {
                lines.push(format!("        {key}: {},", quote_value(&method[key])));
            }
            lines.push(format!(
                "        parameters: {},",
                render_parameters(&method["parameters"])
            ));
            if let Some(returns) = method.get("returns") {
                lines.push(format!("        returns: {},", quote_value(returns)));
            }
            if let Some(throws) = method.get("throws") {
                lines.push(format!(
                    "        throws: [{}],",
                    throws
                        .as_array()
                        .expect("generated throws array")
                        .iter()
                        .map(quote_value)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            lines.push("      },".to_owned());
        }
        lines.push("    ],".to_owned());
    }
    lines.push("  },".to_owned());
}

fn render_parameters(parameters: &Value) -> String {
    format!(
        "[{}]",
        parameters
            .as_array()
            .expect("generated parameter array")
            .iter()
            .map(|parameter| format!(
                "{{ name: {}, description: {} }}",
                quote_value(&parameter["name"]),
                quote_value(&parameter["description"])
            ))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn quote_value(value: &Value) -> String {
    quote(value.as_str().expect("generated catalog string"))
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
