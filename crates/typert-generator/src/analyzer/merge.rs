//! Stable model merging for bounded per-face compiler programs.

use indexmap::IndexMap;

use crate::{
    model::{
        CrossFaceLink, FaceModel, PackageModel, SymbolId, TypeDeclarationModel, TypeGraph,
        TypeNodeId, TypeNodeModel, TypertFace, WorkspaceModel,
    },
    text::locale_compare,
};

#[derive(Default)]
struct FaceParts {
    packages: IndexMap<String, PackageModel>,
    declarations: IndexMap<SymbolId, TypeDeclarationModel>,
    nodes: IndexMap<TypeNodeId, TypeNodeModel>,
}

/// Merges bounded analyses without flattening authored type structure.
///
/// Package and cross-face link duplicates keep the last record. Declaration
/// and expression duplicates keep the first record. Faces remain Host-first;
/// every other collection uses the source's ordered identity comparisons.
pub fn merge_workspace_models(models: impl IntoIterator<Item = WorkspaceModel>) -> WorkspaceModel {
    let mut faces = IndexMap::<TypertFace, FaceParts>::new();
    let mut links = IndexMap::<String, CrossFaceLink>::new();
    for model in models {
        for face in model.faces {
            let parts = faces.entry(face.face).or_default();
            for package in face.packages {
                parts.packages.insert(package.name.clone(), package);
            }
            for declaration in face.graph.declarations {
                parts
                    .declarations
                    .entry(declaration.id.clone())
                    .or_insert(declaration);
            }
            for node in face.graph.nodes {
                parts.nodes.entry(node.id()).or_insert(node);
            }
        }
        for link in model.cross_face_links {
            let key = [
                link.from_face.as_str(),
                &link.from_package,
                link.to_face.as_str(),
                &link.to_package,
                &link.subpath,
                &link.name,
            ]
            .join("\0");
            links.insert(key, link);
        }
    }
    let faces = [TypertFace::Host, TypertFace::Client]
        .into_iter()
        .filter_map(|face| {
            let parts = faces.shift_remove(&face)?;
            let mut packages = parts.packages.into_values().collect::<Vec<_>>();
            packages.sort_by(|left, right| locale_compare(&left.name, &right.name));
            let mut declarations = parts.declarations.into_values().collect::<Vec<_>>();
            declarations.sort_by(|left, right| locale_compare(left.id.as_str(), right.id.as_str()));
            let mut nodes = parts.nodes.into_values().collect::<Vec<_>>();
            nodes.sort_by(|left, right| locale_compare(left.id().as_str(), right.id().as_str()));
            Some(FaceModel {
                face,
                packages,
                graph: TypeGraph {
                    declarations,
                    nodes,
                },
            })
        })
        .collect();
    let mut cross_face_links = links.into_values().collect::<Vec<_>>();
    cross_face_links.sort_by(|left, right| {
        locale_compare(left.from_face.as_str(), right.from_face.as_str())
            .then_with(|| locale_compare(&left.from_package, &right.from_package))
            .then_with(|| locale_compare(left.to_face.as_str(), right.to_face.as_str()))
            .then_with(|| locale_compare(&left.to_package, &right.to_package))
            .then_with(|| locale_compare(&left.subpath, &right.subpath))
            .then_with(|| locale_compare(&left.name, &right.name))
    });
    WorkspaceModel {
        faces,
        cross_face_links,
    }
}
