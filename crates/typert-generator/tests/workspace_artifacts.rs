//! Package opt-in, exact artifact roots, ordered writes, and stale Remote cleanup.

use seekdeep_typert_generator::{
    emitter::{ModelEmitResult, RemoteModelEmitResult},
    model::TypertFace,
    workspace::{
        WorkspaceEmitResult, has_typert_export, package_root, validate_artifact_manifest,
        workspace_root, write_artifacts,
    },
};
use serde_json::{Value, json};

fn artifact(face: TypertFace, remote: bool) -> WorkspaceEmitResult {
    WorkspaceEmitResult {
        package_root: "packages/fixture".to_owned(),
        artifact: ModelEmitResult {
            package: "@fixture/package".to_owned(),
            face,
            exports: Vec::new(),
            js: "local JavaScript\n".to_owned(),
            dts: "local declarations\n".to_owned(),
            remote: remote.then(|| RemoteModelEmitResult {
                js: "remote JavaScript\n".to_owned(),
                dts: "remote declarations\n".to_owned(),
                dts_map: "remote map\n".to_owned(),
            }),
        },
    }
}

fn manifest(face: TypertFace, remote: bool) -> Value {
    let subpath = if face == TypertFace::Host {
        "./typert"
    } else {
        "./client/typert"
    };
    let face = face.as_str();
    let mut value = json!({"exports":{subpath:{"types":format!("./lib/typert.{face}.d.ts"),"default":format!("./lib/typert.{face}.js"),"custom":"allowed"}},"files":[format!("lib/typert.{face}.js"),format!("lib/typert.{face}.d.ts")]});
    if remote {
        value["exports"]["./remote"] = json!({"types":"./lib/typert.remote-client.d.ts","default":"./lib/typert.remote-client.js"});
        value["files"].as_array_mut().unwrap().extend([
            json!("lib/typert.remote-client.js"),
            json!("lib/typert.remote-client.d.ts"),
        ]);
    }
    value
}

#[test]
fn every_face_requires_its_exact_export_and_file_membership() {
    for face in [TypertFace::Host, TypertFace::Client] {
        let artifact = artifact(face, false);
        let valid = manifest(face, false);
        validate_artifact_manifest(&valid, &artifact.artifact).unwrap();
        assert!(has_typert_export(&valid["exports"]));
        let mut wrong = valid.clone();
        let subpath = if face == TypertFace::Host {
            "./typert"
        } else {
            "./client/typert"
        };
        wrong["exports"][subpath]["types"] = json!("./lib/types/typert.d.ts");
        let error = validate_artifact_manifest(&wrong, &artifact.artifact).unwrap_err();
        assert_eq!(error.name(), "TypertAnalysisError");
        assert!(error.to_string().contains("must export"));
        wrong = valid;
        wrong["files"] = json!([]);
        assert!(
            validate_artifact_manifest(&wrong, &artifact.artifact)
                .unwrap_err()
                .to_string()
                .contains("package files must include")
        );
    }
    assert!(!has_typert_export(&json!(["./typert"])));
}

#[test]
fn remote_publication_is_required_only_for_host_invocations() {
    let remote = artifact(TypertFace::Host, true);
    validate_artifact_manifest(&manifest(TypertFace::Host, true), &remote.artifact).unwrap();
    assert!(
        validate_artifact_manifest(&manifest(TypertFace::Host, false), &remote.artifact)
            .unwrap_err()
            .to_string()
            .contains("must export ./remote")
    );
    let no_remote = artifact(TypertFace::Host, false);
    assert!(
        validate_artifact_manifest(&manifest(TypertFace::Host, true), &no_remote.artifact)
            .unwrap_err()
            .to_string()
            .contains("has no Remote methods")
    );
    let mut null_remote = manifest(TypertFace::Host, false);
    null_remote["exports"]["./remote"] = Value::Null;
    assert!(validate_artifact_manifest(&null_remote, &no_remote.artifact).is_err());
    let client = artifact(TypertFace::Client, false);
    validate_artifact_manifest(&manifest(TypertFace::Client, true), &client.artifact).unwrap();
}

#[test]
fn host_rebuild_removes_only_stale_remote_artifacts_and_client_rebuild_preserves_them() {
    let temporary = tempfile::tempdir().unwrap();
    let package = temporary.path().join("packages/fixture");
    write_artifacts(&package, &[artifact(TypertFace::Host, true)]).unwrap();
    let output = package.join("lib");
    let remote = output.join("typert.remote-client.js");
    assert_eq!(
        std::fs::read_to_string(&remote).unwrap(),
        "remote JavaScript\n"
    );
    std::fs::write(output.join("unrelated.txt"), "keep").unwrap();
    write_artifacts(&package, &[artifact(TypertFace::Client, false)]).unwrap();
    assert!(remote.exists());
    write_artifacts(&package, &[artifact(TypertFace::Host, false)]).unwrap();
    for name in [
        "typert.remote-client.js",
        "typert.remote-client.d.ts",
        "typert.remote-client.d.ts.map",
    ] {
        assert!(!output.join(name).exists());
    }
    assert!(output.join("typert.client.js").exists());
    assert_eq!(
        std::fs::read_to_string(output.join("unrelated.txt")).unwrap(),
        "keep"
    );
}

#[test]
fn bundle_ownership_uses_nearest_package_and_aggregate_root() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let package = root.join("packages/fixture");
    std::fs::create_dir_all(package.join("lib/dev")).unwrap();
    std::fs::write(root.join("tsconfig.host.json"), "{}").unwrap();
    std::fs::write(root.join("package.json"), "{}").unwrap();
    std::fs::write(package.join("package.json"), "{}").unwrap();
    assert_eq!(workspace_root(&package.join("lib/dev")).unwrap(), root);
    assert_eq!(
        package_root(&package.join("lib/dev"), root).unwrap(),
        Some(package)
    );
    assert_eq!(package_root(root, root).unwrap(), None);
}
