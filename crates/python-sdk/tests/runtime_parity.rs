//! Artifact discovery is separate from acquisition and process execution.

use std::sync::atomic::{AtomicBool, Ordering};

use seekdeep_python_sdk::{ErrorKind, IdSource, SeededIds, final_response, finish_reason, runtime};
use serde_json::json;

#[test]
fn modes_and_host_tags_preserve_explicit_empty_and_unsupported_cases() {
    assert_eq!(
        runtime::selected_mode(&json!(null), None).unwrap(),
        runtime::RuntimeMode::Exe
    );
    assert_eq!(
        runtime::selected_mode(&json!("exe"), Some("bad")).unwrap(),
        runtime::RuntimeMode::Exe
    );
    assert_eq!(
        runtime::selected_mode(&json!(null), Some("node")).unwrap(),
        runtime::RuntimeMode::Node
    );
    for mode in [json!(""), json!("NODE"), json!(false), json!(7), json!([])] {
        assert_eq!(
            runtime::selected_mode(&mode, None).unwrap_err().kind,
            ErrorKind::Value
        );
    }
    for (system, machine, expected) in [
        ("linux", "AMD64", "linux-x64"),
        ("darwin", "aarch64", "macos-arm64"),
        ("darwin", "arm64", "macos-arm64"),
    ] {
        assert_eq!(runtime::platform_tag(system, machine).unwrap(), expected);
    }
    assert_eq!(
        runtime::platform_tag("win32", "AMD64").unwrap_err().kind,
        ErrorKind::FileNotFound
    );
}

#[test]
fn package_metadata_config_executable_and_helper_fail_independently() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let module = root.join("__init__.py");
    assert!(runtime::bundled_package_dir(&module, &root).is_err());
    std::fs::write(root.join(runtime::PACKAGE_METADATA_FILENAME), "{}").unwrap();
    assert_eq!(runtime::bundled_package_dir(&module, &root).unwrap(), root);
    std::fs::create_dir(root.join("runtime")).unwrap();
    assert!(runtime::bundled_default_config_path(&root).is_err());
    std::fs::write(root.join("runtime/cordis.yml"), "[]").unwrap();
    assert_eq!(
        runtime::bundled_default_config_path(&root).unwrap(),
        root.join("runtime/cordis.yml")
    );
    let linux = root.join("runtime/seekdeep-jsonrpc-agent-pkg-linux-x64");
    std::fs::write(&linux, "").unwrap();
    assert_eq!(
        runtime::bundled_runtime_path(&root, "linux-x64").unwrap(),
        linux
    );
    std::fs::write(
        root.join("runtime/seekdeep-jsonrpc-agent-pkg-macos-arm64"),
        "",
    )
    .unwrap();
    let error = runtime::bundled_runtime_path(&root, "macos-arm64").unwrap_err();
    assert!(error.message.contains("node-pty spawn helper"));
    std::fs::write(
        root.join("runtime/seekdeep-jsonrpc-agent-pkg-macos-arm64-spawn-helper"),
        "",
    )
    .unwrap();
    assert!(runtime::bundled_runtime_path(&root, "macos-arm64").is_ok());
}

#[test]
fn node_lookup_occurs_only_after_the_generated_entry_exists() {
    let root = tempfile::tempdir().unwrap();
    let looked_up = AtomicBool::new(false);
    assert!(
        runtime::node_launch_args(root.path(), || {
            looked_up.store(true, Ordering::Relaxed);
            Ok(None)
        })
        .is_err()
    );
    assert!(!looked_up.load(Ordering::Relaxed));
    let entry = root.path().join(
        "runtime/node/node_modules/@seekdeep-ai/seekdeep-sdk-jsonrpc-demo/lib/packaged-bin.js",
    );
    std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
    std::fs::write(&entry, "").unwrap();
    assert!(
        runtime::node_launch_args(root.path(), || Ok(None))
            .unwrap_err()
            .message
            .contains("system")
    );
    assert_eq!(
        runtime::node_launch_args(root.path(), || Ok(Some("/node".to_owned()))).unwrap(),
        vec!["/node".to_owned(), entry.to_string_lossy().into_owned()]
    );
}

#[test]
fn projections_keep_falsey_text_nested_content_and_last_turn_end() {
    let event = json!({"type":"assistant/message","data":{"message":{"content":[
        {"type":"text","text":false},{"type":"text","text":7},{"type":"text","text":true}
    ]}}})
    .as_object()
    .unwrap()
    .clone();
    assert_eq!(final_response(&[event]), "7True");
    let mut events = vec![
        json!({"type":"turn/end","data":{"reason":{"kind":"future-kind"}}})
            .as_object()
            .unwrap()
            .clone(),
    ];
    assert_eq!(
        finish_reason(&events).unwrap().as_deref(),
        Some("future-kind")
    );
    events.push(
        json!({"type":"turn/end","data":{"reason":{}}})
            .as_object()
            .unwrap()
            .clone(),
    );
    assert_eq!(
        finish_reason(&events).unwrap_err().kind,
        ErrorKind::Protocol
    );
    assert_eq!(finish_reason(&[]).unwrap(), None);
}

#[test]
fn explicit_seeds_reproduce_uuid_spelling_without_ambient_randomness() {
    let first = SeededIds::new([3; 16]);
    let second = SeededIds::new([3; 16]);
    let id = first.next_uuid();
    assert_eq!(id, second.next_uuid());
    assert_eq!(id.get_version_num(), 4);
    assert_ne!(id, first.next_uuid());
}
