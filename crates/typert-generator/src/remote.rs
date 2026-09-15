//! Strict Host-for-Client declaration projection and source mapping.

use std::{
    collections::{HashMap, HashSet},
    fmt::Write as _,
};

use indexmap::IndexMap;

use crate::{
    Result, TypertGeneratorError,
    model::{InvocationModel, InvocationTarget, PackageModel, RemoteTypeImportModel, SymbolId},
    renderer::{ReferenceNames, TypeGraphRenderer},
    source_map::DeclarationMap,
    text::{locale_compare, quote, safe_identifier, utf16_compare},
};

pub(crate) fn declarations(
    renderer: &TypeGraphRenderer<'_>,
    package: &PackageModel,
    banner: &str,
) -> Result<(String, String)> {
    let imports = remote_imports(&package.invocations)?;
    let references = allocate_names(&imports);
    let mut lines = import_prelude(&imports, &references, banner);
    let mut mappings = DeclarationMap::default();
    lines.extend([
        String::new(),
        "declare module '@seekdeep-ai/seekdeep-typert-protocol' {".to_owned(),
    ]);
    let direct = package
        .invocations
        .iter()
        .filter(|invocation| matches!(invocation.invocation, InvocationTarget::Direct))
        .collect::<Vec<_>>();
    let scoped = package
        .invocations
        .iter()
        .filter(|invocation| {
            matches!(invocation.invocation, InvocationTarget::Context { .. })
                || invocation.scope.is_some()
        })
        .collect::<Vec<_>>();
    if !direct.is_empty() {
        for namespace in namespaces(&direct) {
            lines.push(format!(
                "  interface {} {{",
                namespace_interface(&namespace)
            ));
            for invocation in direct
                .iter()
                .filter(|invocation| invocation.namespace == namespace)
            {
                let key = property_name(&invocation.method);
                let signature = format!(
                    "{key}: {}",
                    function_type(renderer, invocation, &references, false)?
                );
                push_mapping(
                    &mut lines,
                    &mut mappings,
                    package,
                    invocation,
                    &signature,
                    key.encode_utf16().count(),
                )?;
            }
            lines.push("  }".to_owned());
        }
        lines.push("  interface TypertRemoteMap {".to_owned());
        for invocation in &direct {
            push_signature(
                renderer,
                &mut lines,
                &mut mappings,
                package,
                invocation,
                &references,
                false,
            )?;
        }
        lines.extend([
            "  }".to_owned(),
            "  interface TypertRemoteNamespaceMap {".to_owned(),
        ]);
        for namespace in namespaces(&direct) {
            lines.push(format!(
                "    {}: {}",
                quote(&namespace),
                namespace_interface(&namespace)
            ));
        }
        lines.push("  }".to_owned());
    }
    if !scoped.is_empty() {
        lines.push("  interface TypertRemoteScopeMap {".to_owned());
        for invocation in &scoped {
            push_signature(
                renderer,
                &mut lines,
                &mut mappings,
                package,
                invocation,
                &references,
                true,
            )?;
        }
        lines.push("  }".to_owned());
    }
    lines.extend([
        "}".to_owned(),
        String::new(),
        "export declare const TYPERT_REMOTE: TypertRemoteContribution".to_owned(),
        "export default TYPERT_REMOTE".to_owned(),
        "//# sourceMappingURL=typert.remote-client.d.ts.map".to_owned(),
    ]);
    Ok((format!("{}\n", lines.join("\n")), mappings.render()))
}

fn import_prelude(
    imports: &[&RemoteTypeImportModel],
    references: &ReferenceNames,
    banner: &str,
) -> Vec<String> {
    let mut grouped = IndexMap::<String, Vec<(String, String)>>::new();
    for imported in imports {
        grouped
            .entry(imported.specifier.clone())
            .or_default()
            .push((imported.name.clone(), references[&imported.symbol].clone()));
    }
    let mut lines = vec![
        banner.to_owned(),
        "import type {".to_owned(),
        "  RemoteResult,".to_owned(),
        "  TypertRemoteContribution,".to_owned(),
        "} from '@seekdeep-ai/seekdeep-typert-protocol'".to_owned(),
    ];
    let mut grouped = grouped.into_iter().collect::<Vec<_>>();
    grouped.sort_by(|(left, _), (right, _)| locale_compare(left, right));
    for (specifier, mut values) in grouped {
        values.sort_by(|(_, left), (_, right)| locale_compare(left, right));
        let names = values
            .into_iter()
            .map(|(name, local)| {
                if name == local {
                    name
                } else {
                    format!("{name} as {local}")
                }
            })
            .collect::<Vec<_>>();
        lines.push(format!(
            "import type {{ {} }} from {}",
            names.join(", "),
            quote(&specifier)
        ));
    }
    lines
}

fn function_type(
    renderer: &TypeGraphRenderer<'_>,
    invocation: &InvocationModel,
    references: &ReferenceNames,
    scoped: bool,
) -> Result<String> {
    let mut parameters = invocation
        .parameters
        .iter()
        .filter(|parameter| {
            !scoped
                || matches!(invocation.invocation, InvocationTarget::Context { .. })
                || invocation
                    .scope
                    .as_ref()
                    .is_none_or(|scope| scope.wire != parameter.wire)
        })
        .map(|parameter| {
            Ok(format!(
                "{}{}: {}",
                safe_identifier(&parameter.wire),
                if parameter.optional == Some(true) {
                    "?"
                } else {
                    ""
                },
                renderer.render_type(&parameter.boundary.ty, Some(references))?
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    if invocation.cancellation.is_some() {
        parameters.push("signal?: AbortSignal".to_owned());
    }
    Ok(format!(
        "({}) => Promise<RemoteResult<{}>>",
        parameters.join(", "),
        renderer.render_type(&invocation.result.ty, Some(references))?
    ))
}

fn push_signature(
    renderer: &TypeGraphRenderer<'_>,
    lines: &mut Vec<String>,
    mappings: &mut DeclarationMap,
    package: &PackageModel,
    invocation: &InvocationModel,
    references: &ReferenceNames,
    scoped: bool,
) -> Result<()> {
    let context = match &invocation.invocation {
        InvocationTarget::Context { context, .. } => Some(context.as_str()),
        InvocationTarget::Direct => invocation
            .scope
            .as_ref()
            .map(|scope| scope.context.as_str()),
    };
    let key = if scoped {
        format!(
            "{}:{}/{}",
            context.unwrap_or("undefined"),
            invocation.namespace,
            invocation.method
        )
    } else {
        format!("{}/{}", invocation.namespace, invocation.method)
    };
    let signature = format!(
        "{}: {}",
        quote(&key),
        function_type(renderer, invocation, references, scoped)?
    );
    let delimiter = signature.find(": (").ok_or_else(|| {
        TypertGeneratorError::Emit(format!(
            "Remote signature {} has no property delimiter",
            invocation.id
        ))
    })?;
    let key_length = signature[..delimiter].encode_utf16().count();
    push_mapping(lines, mappings, package, invocation, &signature, key_length)
}

fn push_mapping(
    lines: &mut Vec<String>,
    mappings: &mut DeclarationMap,
    package: &PackageModel,
    invocation: &InvocationModel,
    signature: &str,
    key_length: usize,
) -> Result<()> {
    lines.push(format!("    {signature}"));
    mappings.add(
        lines.len(),
        key_length,
        &declaration_source(package, invocation)?,
        invocation.location.line,
        invocation.location.column,
        &invocation.method,
    )
}

fn declaration_source(package: &PackageModel, invocation: &InvocationModel) -> Result<String> {
    let root = path_parts(&package.root);
    let source = path_parts(&invocation.location.file);
    let common = root
        .iter()
        .zip(&source)
        .take_while(|(left, right)| left == right)
        .count();
    if package.root.starts_with('/') != invocation.location.file.starts_with('/')
        || common != root.len()
        || source.len() == common
    {
        return Err(TypertGeneratorError::Emit(format!(
            "Remote declaration {} is outside its package root {}",
            invocation.id, package.root
        )));
    }
    Ok(format!("../{}", source[common..].join("/")))
}

fn path_parts(path: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." if parts.last().is_some_and(|part| *part != "..") => {
                parts.pop();
            }
            ".." if path.starts_with('/') => {}
            _ => parts.push(part),
        }
    }
    parts
}

fn remote_imports(invocations: &[InvocationModel]) -> Result<Vec<&RemoteTypeImportModel>> {
    let mut imports = IndexMap::<SymbolId, &RemoteTypeImportModel>::new();
    for invocation in invocations {
        let context = match &invocation.invocation {
            InvocationTarget::Direct => None,
            InvocationTarget::Context { boundary, .. } => Some(boundary),
        };
        for boundary in context
            .into_iter()
            .chain(
                invocation
                    .parameters
                    .iter()
                    .map(|parameter| &parameter.boundary),
            )
            .chain(std::iter::once(&invocation.result))
        {
            for imported in &boundary.imports {
                if imports.get(&imported.symbol).is_some_and(|current| {
                    current.specifier != imported.specifier || current.name != imported.name
                }) {
                    return Err(TypertGeneratorError::Emit(format!(
                        "typert Remote emitter: symbol {} has inconsistent public imports",
                        imported.symbol
                    )));
                }
                imports.insert(imported.symbol.clone(), imported);
            }
        }
    }
    let mut imports = imports.into_values().collect::<Vec<_>>();
    imports.sort_by(|left, right| {
        locale_compare(&left.specifier, &right.specifier)
            .then_with(|| locale_compare(&left.name, &right.name))
    });
    Ok(imports)
}

fn allocate_names(imports: &[&RemoteTypeImportModel]) -> ReferenceNames {
    let mut used = HashSet::from([
        "TypertRemoteContribution".to_owned(),
        "TYPERT_REMOTE".to_owned(),
    ]);
    let mut names = HashMap::new();
    for imported in imports {
        let base = safe_identifier(&imported.name);
        let mut name = base.clone();
        let mut suffix = 2;
        while used.contains(&name) {
            name = format!("{base}$remote{suffix}");
            suffix += 1;
        }
        used.insert(name.clone());
        names.insert(imported.symbol.clone(), name);
    }
    names
}

fn namespaces(invocations: &[&InvocationModel]) -> Vec<String> {
    let mut names = invocations
        .iter()
        .map(|invocation| invocation.namespace.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    names.sort_by(|left, right| utf16_compare(left, right));
    names
}

fn namespace_interface(namespace: &str) -> String {
    let mut name = "TypertRemoteNamespace$".to_owned();
    for byte in namespace.as_bytes() {
        write!(&mut name, "{byte:02x}").expect("writing to a string cannot fail");
    }
    name
}

fn property_name(name: &str) -> String {
    let identifier = name.as_bytes().split_first().is_some_and(|(first, rest)| {
        (first.is_ascii_alphabetic() || matches!(first, b'_' | b'$'))
            && rest
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$'))
    });
    if identifier {
        name.to_owned()
    } else {
        quote(name)
    }
}
