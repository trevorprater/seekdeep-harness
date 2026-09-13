//! Byte-exact source artifacts from independently analyzed fixture faces.

use seekdeep_typert_generator::{emitter::FaceModelEmitter, model::WorkspaceModel};
use serde_json::Value;

fn renamed(text: &str) -> String {
    text.replace(
        "@deepseek-ai/dsh-typert-generator",
        "@seekdeep-ai/seekdeep-typert-generator",
    )
    .replace(
        "@deepseek-ai/dsh-typert-protocol",
        "@seekdeep-ai/seekdeep-typert-protocol",
    )
}

fn same_text(actual: &str, expected: &str, label: &str) {
    let expected = renamed(expected);
    if actual != expected {
        for (index, (actual, expected)) in actual.split('\n').zip(expected.split('\n')).enumerate()
        {
            assert_eq!(actual, expected, "{label}, line {}", index + 1);
        }
        assert_eq!(
            actual.len(),
            expected.len(),
            "{label}, complete artifact bytes"
        );
    }
}

#[test]
fn full_source_faces_emit_exact_reflection_schemas_remote_contracts_and_maps() {
    let renderer_fixture: Value =
        serde_json::from_str(include_str!("fixtures/source_type_model.json")).unwrap();
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/source_emitter.json")).unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let workspace = if case["name"] == "type-model" {
            &renderer_fixture["workspace"]
        } else {
            &case["workspace"]
        };
        let workspace: WorkspaceModel = serde_json::from_value(workspace.clone()).unwrap();
        for expected in case["artifacts"].as_array().unwrap() {
            let face = workspace
                .faces
                .iter()
                .find(|face| face.face.as_str() == expected["face"])
                .unwrap();
            let expected = &expected["artifact"];
            let package = expected["package"].as_str().unwrap();
            let actual = FaceModelEmitter::new(face).emit(package).unwrap();
            assert_eq!(actual.package, package);
            assert_eq!(
                serde_json::to_value(&actual.exports).unwrap(),
                expected["exports"]
            );
            same_text(
                &actual.js,
                expected["js"].as_str().unwrap(),
                &format!("{package} JavaScript"),
            );
            same_text(
                &actual.dts,
                expected["dts"].as_str().unwrap(),
                &format!("{package} declarations"),
            );
            match (&actual.remote, expected.get("remote")) {
                (Some(actual), Some(expected)) => {
                    same_text(
                        &actual.js,
                        expected["js"].as_str().unwrap(),
                        "Remote JavaScript",
                    );
                    same_text(
                        &actual.dts,
                        expected["dts"].as_str().unwrap(),
                        "Remote declarations",
                    );
                    same_text(
                        &actual.dts_map,
                        expected["dtsMap"].as_str().unwrap(),
                        "Remote declaration map",
                    );
                }
                (None, None) => {}
                _ => panic!("{package} Remote artifact presence differs"),
            }
        }
    }
}
