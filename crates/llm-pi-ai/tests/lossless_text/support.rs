use std::{fs, path::PathBuf, process::Command};

use seekdeep_llm::{
    CallId, ContentBlock, GenerateOptions, JsonString, Message, ModelId, ProviderId,
};
use seekdeep_lossless_json::JsonValue;

pub(super) fn samples() -> Vec<JsonString> {
    vec![
        JsonString::from_utf16(&[0xd800]),
        JsonString::from_utf16(&[0xdfff]),
        JsonString::from_utf16(&[0x61, 0xd800, 0x62, 0xdfff, 0x63]),
        JsonString::from_utf16(&[0xd83d, 0xde00]),
        "\\ud800".into(),
        JsonString::from_utf16(&[0xd800, 0x0a, 0xdc00]),
        JsonString::from_utf16(&[0xfeff, 0xd800, 0x20]),
        JsonString::default(),
    ]
}

pub(super) fn tool_request(provider: &str, model: &str, text: JsonString) -> GenerateOptions {
    GenerateOptions::new(
        ProviderId::new(provider),
        ModelId::new(model),
        vec![Message::tool_result(
            &CallId::new("lossless-call"),
            vec![ContentBlock::Text { text }],
            false,
        )],
    )
}

pub(super) fn source_call(input: &JsonValue) -> JsonValue {
    let fixture = tempfile::tempdir().unwrap();
    let input_path = fixture.path().join("input.json");
    fs::write(&input_path, input.as_raw()).unwrap();
    let output = Command::new("node")
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/lossless_text/source.mjs"))
        .arg(source_root())
        .arg(input_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    JsonValue::parse(String::from_utf8(output.stdout).unwrap()).unwrap()
}

pub(super) fn source_root() -> PathBuf {
    std::env::var_os("SEEKDEEP_PARITY_SOURCE").map_or_else(
        || {
            PathBuf::from(
                include_str!("../../../../SOURCE_SNAPSHOT")
                    .lines()
                    .find_map(|line| line.strip_prefix("repository="))
                    .unwrap(),
            )
        },
        PathBuf::from,
    )
}
