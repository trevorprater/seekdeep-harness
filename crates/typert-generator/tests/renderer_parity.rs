//! Full source-analyzed graph and defensive renderer parity.

use seekdeep_typert_generator::{
    Result, TypertGeneratorError,
    model::{
        MemberId, MemberModel, SymbolId, TypeGraph, TypeNodeId, WorkspaceModel, child_type_node_ids,
    },
    renderer::TypeGraphRenderer,
};
use serde::Serialize;
use serde_json::{Value, json};

fn outcome<T: Serialize>(result: Result<T>) -> Value {
    match result {
        Ok(value) => json!({ "ok": value }),
        Err(error) => json!({ "error": { "name": error.name(), "message": error.to_string() } }),
    }
}

#[test]
fn complete_source_graph_round_trips_and_renders_every_node_member_and_closure() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/source_type_model.json")).unwrap();
    let model: WorkspaceModel = serde_json::from_value(fixture["workspace"].clone()).unwrap();
    assert_eq!(serde_json::to_value(&model).unwrap(), fixture["workspace"]);
    let expected_faces = fixture["faces"].as_array().unwrap();
    assert_eq!(model.faces.len(), expected_faces.len());
    for (face, expected) in model.faces.iter().zip(expected_faces) {
        assert_eq!(face.face.as_str(), expected["face"]);
        let renderer = TypeGraphRenderer::new(&face.graph);
        assert_eq!(
            face.graph.nodes.len(),
            expected["nodes"].as_array().unwrap().len()
        );
        for expected in expected["nodes"].as_array().unwrap() {
            let id = TypeNodeId::from(expected["id"].as_str().unwrap());
            assert_eq!(
                outcome(renderer.render_type(&id, None)),
                expected["rendered"],
                "render {id}"
            );
            assert_eq!(
                outcome(child_type_node_ids(renderer.node(&id).unwrap())),
                expected["edges"],
                "edges {id}"
            );
            assert_eq!(
                outcome(
                    renderer
                        .declaration_closure_for_types(std::slice::from_ref(&id))
                        .map(|declarations| {
                            declarations
                                .iter()
                                .map(|declaration| &declaration.id)
                                .collect::<Vec<_>>()
                        })
                ),
                expected["closure"],
                "closure {id}"
            );
        }
        for expected in expected["declarations"].as_array().unwrap() {
            let id = SymbolId::from(expected["id"].as_str().unwrap());
            assert_eq!(
                outcome(renderer.render_declaration(&id)),
                expected["rendered"],
                "declaration {id}"
            );
            let member_ids = renderer
                .declaration(&id)
                .unwrap()
                .members
                .iter()
                .map(seekdeep_typert_generator::model::MemberModel::id)
                .collect::<Vec<_>>();
            assert_eq!(
                outcome(renderer.declaration_closure_for_members(&member_ids).map(
                    |declarations| {
                        declarations
                            .iter()
                            .map(|declaration| &declaration.id)
                            .collect::<Vec<_>>()
                    }
                )),
                expected["closure"],
                "member closure {id}"
            );
            for expected in expected["members"].as_array().unwrap() {
                let id = MemberId::from(expected["id"].as_str().unwrap());
                let member = renderer.member(&id).unwrap();
                assert_eq!(
                    outcome(renderer.render_member(member, false, None)),
                    expected["rendered"],
                    "member {id}"
                );
                assert_eq!(
                    outcome(renderer.render_member(member, true, None)),
                    expected["source"],
                    "source member {id}"
                );
            }
        }
    }
}

fn declaration() -> Value {
    json!({
        "id": "alias", "package": "@fixture/renderer", "name": "Alias", "kind": "alias",
        "abstract": false, "exported": true, "location": {"file":"fixture.ts", "line":1,"column":1},
        "text":"export type Alias", "typeParameters":[],"extends":[],"implements":[],"members":[],"tags":[]
    })
}

#[test]
fn missing_edges_and_unknown_records_keep_source_error_classes() {
    let graph: TypeGraph = serde_json::from_value(json!({
        "declarations": [declaration()],
        "nodes": [
            {"id":"mapped","kind":"mapped","parameter":{"id":"key","name":"Key","const":false},"readonly":"preserve","optional":"preserve"},
            {"id":"invalid","kind":"future-node"}
        ]
    })).unwrap();
    let renderer = TypeGraphRenderer::new(&graph);
    assert_eq!(
        renderer.node(&"missing".into()).unwrap_err().to_string(),
        "type graph references missing node missing"
    );
    assert_eq!(
        renderer
            .declaration(&"missing".into())
            .unwrap_err()
            .to_string(),
        "type graph references missing declaration missing"
    );
    assert_eq!(
        renderer.member(&"missing".into()).unwrap_err().to_string(),
        "type graph references missing member missing"
    );
    assert_eq!(
        renderer
            .render_type(&"mapped".into(), None)
            .unwrap_err()
            .to_string(),
        "mapped type parameter Key has no constraint"
    );
    assert!(
        renderer
            .declaration_closure_for_types(&["mapped".into()])
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        renderer
            .render_declaration(&"alias".into())
            .unwrap_err()
            .to_string(),
        "alias alias has no type node"
    );
    assert_eq!(
        outcome(renderer.render_type(&"invalid".into(), None)),
        json!({"error":{"name":"TypeGraphRenderError","message":"unsupported model variant {\"id\":\"invalid\",\"kind\":\"future-node\"}"}})
    );
    assert!(matches!(
        renderer.declaration_closure_for_types(&["invalid".into()]),
        Err(TypertGeneratorError::Model(_))
    ));
}

#[test]
fn duplicate_lookup_ids_keep_last_values_without_reordering_declarations() {
    let graph: TypeGraph = serde_json::from_value(json!({
        "declarations": [],
        "nodes": [{"id":"same","kind":"keyword","name":"string"},{"id":"same","kind":"keyword","name":"number"}]
    })).unwrap();
    let renderer = TypeGraphRenderer::new(&graph);
    assert_eq!(
        renderer.render_type(&"same".into(), None).unwrap(),
        "number"
    );
}

#[test]
fn optional_shapes_preserve_rendering_and_declaration_closure() {
    let mut dependency = declaration();
    dependency["id"] = json!("dependency");
    dependency["name"] = json!("Dependency");
    dependency["kind"] = json!("interface");
    let mut empty_enum = declaration();
    empty_enum["id"] = json!("empty-enum");
    empty_enum["name"] = json!("EmptyEnum");
    empty_enum["kind"] = json!("enum");
    let graph: TypeGraph = serde_json::from_value(json!({
        "declarations": [dependency, empty_enum],
        "nodes": [
            {"id":"string","kind":"keyword","name":"string"},
            {"id":"union","kind":"union","types":["string","string"]},
            {"id":"array","kind":"array","element":"union"},
            {"id":"array-of-string","kind":"array","element":"string"},
            {"id":"tuple","kind":"tuple","elements":[
                {"type":"string","optional":false,"rest":false},
                {"type":"string","optional":true,"rest":false},
                {"type":"array-of-string","optional":false,"rest":true}
            ]},
            {"id":"mapped","kind":"mapped","parameter":{"id":"key","name":"Key","const":false,"constraint":"string","default":"string"},"readonly":"preserve","optional":"preserve"},
            {"id":"infer","kind":"infer","parameter":{"id":"inferred","name":"Value","const":false,"constraint":"string","default":"string"}},
            {"id":"imported","kind":"import-type","module":"@fixture/dependency","qualifier":"Dependency","arguments":["mapped","infer"],"typeof":false,"target":{"kind":"declaration","symbol":"dependency"}},
            {"id":"empty-object","kind":"object","members":[]}
        ]
    })).unwrap();
    let renderer = TypeGraphRenderer::new(&graph);
    for (id, expected) in [
        ("array", "(string | string)[]"),
        ("tuple", "[string, string?, ...string[]]"),
        ("mapped", "{ [Key in string]: unknown }"),
        ("empty-object", "{}"),
    ] {
        assert_eq!(renderer.render_type(&id.into(), None).unwrap(), expected);
    }
    assert_eq!(
        renderer.render_declaration(&"empty-enum".into()).unwrap(),
        "export enum EmptyEnum {\n}"
    );
    let closure = renderer
        .declaration_closure_for_types(&["imported".into()])
        .unwrap();
    assert_eq!(
        closure
            .iter()
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>(),
        ["Dependency"]
    );
    for id in ["mapped", "infer"] {
        assert_eq!(
            child_type_node_ids(renderer.node(&id.into()).unwrap()).unwrap(),
            vec![TypeNodeId::from("string"), TypeNodeId::from("string")]
        );
    }
}

#[test]
fn retained_member_text_does_not_require_a_supported_projection_kind() {
    let graph = TypeGraph {
        declarations: Vec::new(),
        nodes: Vec::new(),
    };
    let renderer = TypeGraphRenderer::new(&graph);
    let member: MemberModel = serde_json::from_value(json!({
        "id":"future-member","kind":"future-member","text":"future value: string"
    }))
    .unwrap();
    assert_eq!(
        renderer.render_member(&member, true, None).unwrap(),
        "future value: string"
    );
    assert!(matches!(
        renderer.render_member(&member, false, None),
        Err(TypertGeneratorError::Render(_))
    ));
}
