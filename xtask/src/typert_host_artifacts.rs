//! `cargo xtask typert-host-artifacts [--check]`: the compiled Host Typert artifacts.
//!
//! Source: every package exporting `./typert` ships a generated `typert.host.js` (strict Zod
//! codecs for its Remote boundaries). The port derives the same slice of the pinned Host face
//! model (`crates/api-remotes-client/contracts/host-model.json`, verified against the source
//! analyzer by `remote-contracts`) into each crate's `typert.host.json`, which the crate embeds
//! and registers through `seekdeep-typert-host-artifact`.

use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    path::PathBuf,
};

use seekdeep_typert_generator::{
    emitter::FaceModelEmitter,
    model::{
        FaceModel, InvocationModel, InvocationTarget, MemberKind, MemberModel, SymbolId,
        TypeDeclarationModel, TypeGraph, TypeNodeId, TypeNodeKind, TypeNodeModel, TypeTargetModel,
        child_type_node_ids,
    },
};
use serde_json::{Value, json};

/// Source package names and the crates that embed their Host artifacts.
const PACKAGES: &[(&str, &str)] = &[
    ("@deepseek-ai/dsh-commands", "crates/commands"),
    ("@deepseek-ai/dsh-goal", "crates/goal"),
    (
        "@deepseek-ai/dsh-cordis-host-runner",
        "crates/cordis-host-runner",
    ),
    (
        "@deepseek-ai/dsh-host-plugin-inventory",
        "crates/host-plugin-inventory",
    ),
    (
        "@deepseek-ai/dsh-message-feedback",
        "crates/message-feedback",
    ),
];

pub(super) fn run(check: bool) -> anyhow::Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let model_path = root.join("crates/api-remotes-client/contracts/host-model.json");
    let model = serde_json::from_slice::<Value>(&std::fs::read(&model_path)?)?;
    let face: FaceModel = serde_json::from_value(model["face"].clone())?;
    let emitter = FaceModelEmitter::new(&face);
    let mut drift = Vec::new();
    for (package, crate_dir) in PACKAGES {
        let modeled = face
            .packages
            .iter()
            .find(|candidate| candidate.name == *package)
            .ok_or_else(|| anyhow::anyhow!("host model has no package {package}"))?;
        let runtime = emitter.runtime_model(modeled)?;
        let graph = prune(&face.graph, &modeled.invocations)?;
        let artifact = json!({
            "package": modeled.name,
            "face": "host",
            "model": runtime,
            "invocations": modeled.invocations,
            "graph": graph,
        });
        let text = rename(&format!("{}\n", serde_json::to_string_pretty(&artifact)?));
        let path = root.join(crate_dir).join("typert.host.json");
        if check {
            let current = std::fs::read_to_string(&path).unwrap_or_default();
            if current != text {
                drift.push(path.display().to_string());
            }
        } else {
            std::fs::write(&path, text)?;
            println!("wrote {}", path.display());
        }
    }
    anyhow::ensure!(
        drift.is_empty(),
        "typert host artifacts are stale (run `cargo xtask typert-host-artifacts`): {}",
        drift.join(", ")
    );
    if check {
        println!(
            "typert host artifacts are current for {} packages",
            PACKAGES.len()
        );
    }
    Ok(())
}

/// The product identity of the port's packages.
fn rename(text: &str) -> String {
    text.replace("@deepseek-ai/dsh-", "@seekdeep-ai/seekdeep-")
}

/// The declarations and nodes the invocation boundaries reach, in graph order.
fn prune(graph: &TypeGraph, invocations: &[InvocationModel]) -> anyhow::Result<TypeGraph> {
    let nodes: HashMap<TypeNodeId, &TypeNodeModel> =
        graph.nodes.iter().map(|node| (node.id(), node)).collect();
    let declarations: HashMap<&SymbolId, &TypeDeclarationModel> = graph
        .declarations
        .iter()
        .map(|declaration| (&declaration.id, declaration))
        .collect();
    let mut queue = VecDeque::new();
    for invocation in invocations {
        if let InvocationTarget::Context { boundary, .. } = &invocation.invocation {
            queue.push_back(boundary.ty.clone());
            queue.push_back(boundary.codec_type.clone());
        }
        for parameter in &invocation.parameters {
            queue.push_back(parameter.boundary.ty.clone());
            queue.push_back(parameter.boundary.codec_type.clone());
        }
        queue.push_back(invocation.result.ty.clone());
        queue.push_back(invocation.result.codec_type.clone());
    }
    let mut seen_nodes = BTreeSet::new();
    let mut seen_declarations = BTreeSet::new();
    while let Some(id) = queue.pop_front() {
        if !seen_nodes.insert(id.clone()) {
            continue;
        }
        let Some(node) = nodes.get(&id) else { continue };
        let TypeNodeModel::Defined(defined) = node else {
            continue;
        };
        queue.extend(child_type_node_ids(node)?);
        // Inline object types carry their member types themselves.
        if let TypeNodeKind::Object { members } = &defined.kind {
            queue.extend(member_nodes(members));
        }
        if let TypeNodeKind::Reference {
            target: TypeTargetModel::Declaration { symbol },
            ..
        } = &defined.kind
            && seen_declarations.insert(symbol.clone())
            && let Some(declaration) = declarations.get(symbol)
        {
            queue.extend(declaration_nodes(declaration));
        }
    }
    Ok(TypeGraph {
        declarations: graph
            .declarations
            .iter()
            .filter(|declaration| seen_declarations.contains(&declaration.id))
            .cloned()
            .collect(),
        nodes: graph
            .nodes
            .iter()
            .filter(|node| seen_nodes.contains(&node.id()))
            .cloned()
            .collect(),
    })
}

/// Every type node a declaration's data schema can reach.
fn declaration_nodes(declaration: &TypeDeclarationModel) -> Vec<TypeNodeId> {
    let mut ids = Vec::new();
    ids.extend(declaration.ty.iter().cloned());
    ids.extend(declaration.extends.iter().cloned());
    ids.extend(declaration.implements.iter().cloned());
    for parameter in &declaration.type_parameters {
        ids.extend(parameter.constraint.iter().cloned());
        ids.extend(parameter.default.iter().cloned());
    }
    ids.extend(member_nodes(&declaration.members));
    ids
}

/// The type nodes an object's data-schema members reference.
fn member_nodes(members: &[MemberModel]) -> Vec<TypeNodeId> {
    let mut ids = Vec::new();
    for member in members {
        let MemberModel::Defined(member) = member else {
            continue;
        };
        match &member.kind {
            MemberKind::Property { ty } => ids.push(ty.clone()),
            MemberKind::Index { signature } => {
                ids.extend(
                    signature
                        .parameters
                        .iter()
                        .map(|parameter| parameter.ty.clone()),
                );
                ids.push(signature.returns.clone());
            }
            _ => {}
        }
    }
    ids
}
