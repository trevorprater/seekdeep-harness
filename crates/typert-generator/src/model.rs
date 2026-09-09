//! The `FaceModel` types, shared through `seekdeep-typert-model`; the child-node walk returns the
//! generator's error type here.

pub use seekdeep_typert_model::*;

/// Direct child type-node ids of one node, in the generator's error domain.
///
/// # Errors
///
/// Returns when the node is an unsupported model variant.
pub fn child_type_node_ids(node: &TypeNodeModel) -> crate::Result<Vec<TypeNodeId>> {
    seekdeep_typert_model::child_type_node_ids(node)
        .map_err(|error| crate::TypertGeneratorError::Model(error.0))
}
