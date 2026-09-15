//! Differential read windows and replay presentation against the pinned source.

use std::{path::Path, process::Command};

use seekdeep_cordis::Context;
use seekdeep_fs::FsError;
use seekdeep_lossless_json::{JsonString, JsonValue};
use seekdeep_source_oracle::SourceOracle;
use seekdeep_tool_fs::read_render::{
    FileReadOutcome, ReadWindow, build_window, format_read_output, lang_from_path,
    read_meta_from_meta,
};
use seekdeep_tools::{ToolResult, ToolRuntimeConfig};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Serialize)]
struct WindowCase {
    chunks: Vec<JsonString>,
    request: ReadWindow,
    path: String,
}

#[derive(Serialize)]
struct Requests {
    windows: Vec<WindowCase>,
    metadata: Vec<JsonValue>,
    languages: Vec<String>,
    presentations: Vec<ToolResult>,
}

#[derive(Deserialize)]
struct Observations {
    windows: Vec<JsonValue>,
    metadata: Vec<JsonValue>,
    languages: Vec<Option<String>>,
    presentations: Vec<JsonValue>,
}

fn windows() -> Vec<WindowCase> {
    let mut cases = Vec::new();
    let texts = [
        "",
        "a",
        "\n",
        "\n\n",
        "one\ntwo\nthree",
        "one\ntwo\n",
        "a\r\nb\r\n",
        "abc\rdef\n",
        "\r",
        "a\rb",
        "éééé",
        "a😀x",
        "😀😀x\nlast",
        "x\u{2028}y\u{2029}z",
    ]
    .into_iter()
    .map(JsonString::from)
    .chain([
        JsonString::from_utf16(&[0xd800]),
        JsonString::from_utf16(&[0xdc00, 0xd800, 0xd83d, 0xde00, 10, 0xdfff]),
    ]);
    for text in texts {
        for size in [1, 2, 5] {
            let chunks: Vec<_> = text
                .utf16_units()
                .chunks(size)
                .map(JsonString::from_utf16)
                .collect();
            for offset in [1, 2, 4] {
                for max_line_length in [1, 2, 3, 5] {
                    for max_bytes in [0, 6, 38, 80] {
                        cases.push(WindowCase {
                            chunks: chunks.clone(),
                            request: ReadWindow {
                                offset,
                                limit: 3,
                                max_line_length,
                                max_bytes,
                            },
                            path: "a\"b\nc.rs".to_owned(),
                        });
                    }
                }
            }
        }
    }
    for (text, offset, limit, size) in [
        ("y".repeat(100) + "\n", 1, 2000, 512),
        (vec!["y".repeat(100); 2000].join("\n"), 1, 2000, 512),
        ("z".repeat(5000), 1, 2000, 256),
        (format!("{}😀x", "a".repeat(1999)), 1, 2000, 2000),
        ("one\ntwo\nthree\nfour".to_owned(), 2, 2, 2),
        ("one\ntwo\nthree".to_owned(), 2, 1, 1),
    ] {
        let text = JsonString::from(text);
        cases.push(WindowCase {
            chunks: text
                .utf16_units()
                .chunks(size)
                .map(JsonString::from_utf16)
                .collect(),
            request: ReadWindow {
                offset,
                limit,
                max_line_length: 2000,
                max_bytes: 50 * 1024,
            },
            path: "source.rs".to_owned(),
        });
    }
    cases
}

fn metadata() -> Vec<JsonValue> {
    let good = json!({"path":"/abs/a.rs","offset":1,"lines":[{"number":1,"text":"x"}],"totalLines":1,"lang":"rs"});
    let mut cases: Vec<JsonValue> = [
        good.clone(),
        json!({"path":"empty","offset":1,"lines":[],"totalLines":0}),
        json!({"path":"capped","offset":5,"lines":[],"totalLines":9}),
        json!(null),
        json!("nope"),
        json!([good]),
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    for (key, values) in [
        ("path", vec![json!(5), json!(null)]),
        (
            "offset",
            vec![json!("1"), json!(0), json!(-1), json!(1.5), json!(null)],
        ),
        (
            "totalLines",
            vec![json!("1"), json!(-1), json!(1.5), json!(null)],
        ),
        ("lang", vec![json!(5), json!(null)]),
        (
            "lines",
            vec![
                json!("nope"),
                json!([null]),
                json!([{"number":1}]),
                json!([{"number":"1","text":"x"}]),
                json!([{"number":0,"text":"x"}]),
                json!([{"number":1.5,"text":"x"}]),
                json!([{"number":2,"text":"x"}]),
                json!([{"number":1,"text":"x"},{"number":1,"text":"y"}]),
            ],
        ),
    ] {
        for value in values {
            let mut mutated = good.clone();
            mutated[key] = value;
            cases.push(mutated.into());
        }
        let mut missing = good.clone();
        missing.as_object_mut().unwrap().remove(key);
        cases.push(missing.into());
    }
    for raw in [
        r#"{"path":"\ud800.rs","offset":1,"lines":[{"number":1,"text":"\udfff"}],"totalLines":1,"lang":"\ud800"}"#,
        r#"{"path":"old","path":"new","offset":1e0,"lines":[{"number":1.0,"text":"old","text":"\ud800"}],"totalLines":1e0,"\udfff":"ignored"}"#,
        r#"{"path":"a","offset":2,"lines":[{"number":1,"text":"x"}],"totalLines":2}"#,
        r#"{"path":"a","offset":1,"lines":[{"number":2,"text":"x"},{"number":1,"text":"y"}],"totalLines":2}"#,
    ] {
        cases.push(JsonValue::parse(raw.to_owned()).unwrap());
    }
    cases
}

fn requests() -> Requests {
    let metadata = metadata();
    let mut presentations = Vec::new();
    let texts = [
        JsonString::from(
            "<path>/file.rs</path>\n<type>file</type>\n<content>\n1: α\n2: β\n</content>",
        ),
        JsonString::from("<path>/file.rs</path>\n<type>file</type>\n<content>\n\n</content>"),
        JsonString::from("wrong"),
        JsonString::from("<path>a\nb</path>\n<type>file</type>\n<content>\nbody\n</content>"),
        JsonString::parse(
            r#""<path>\ud800.rs</path>\n<type>file</type>\n<content>\n\udfff\n</content>""#
                .to_owned(),
        )
        .unwrap(),
    ];
    for text in texts {
        for suffix in [
            "", "\n", "\r", "\r\n", "\u{2028}", "\u{2029}", "\n\n", "garbage",
        ] {
            let mut text = text.clone();
            text.push_str(suffix);
            for meta in &metadata {
                presentations.push(ToolResult {
                    content: vec![seekdeep_llm::ContentBlock::Text { text: text.clone() }],
                    is_error: false,
                    meta: Some(meta.clone()),
                });
            }
        }
    }
    let mut invalid = presentations[0].clone();
    invalid.is_error = true;
    presentations.push(invalid);
    let mut invalid = presentations[0].clone();
    invalid.meta = None;
    presentations.push(invalid);
    let mut invalid = presentations[0].clone();
    invalid.content.push(invalid.content[0].clone());
    presentations.push(invalid);
    Requests {
        windows: windows(),
        metadata,
        presentations,
        languages: [
            "src/a.ts",
            "src/a.TSX",
            "/abs/module.mjs",
            "conf.yml",
            "README.md",
            "a.py.bak",
            "archive.tar.gz",
            "/dir.py/plain",
            r"C:\src\main.rs",
            ".gitignore",
            "/etc/hosts",
            "data.unknownext",
            "trailingdot.",
            "foo.constructor",
            "foo.__proto__",
            "foo.toString",
            "foo.hasOwnProperty",
            "file.\u{212a}t",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
    }
}

fn source_observations(requests: &Requests) -> Observations {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source = SourceOracle::open(&repository).unwrap();
    for file in [
        "packages/fs/tool-fs/src/read-render.ts",
        "packages/fs/tool-fs/src/read.ts",
    ] {
        assert_eq!(
            source.read(file).unwrap(),
            std::fs::read_to_string(source.root().join(file)).unwrap()
        );
    }
    let fixture = tempfile::tempdir().unwrap();
    let input = fixture.path().join("requests.json");
    let output = fixture.path().join("observations.json");
    std::fs::write(&input, serde_json::to_vec(requests).unwrap()).unwrap();
    let command = Command::new("node")
        .arg("--experimental-strip-types")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/read-render-oracle.mjs"))
        .arg(source.root())
        .arg(&input)
        .arg(&output)
        .env_remove("NODE_OPTIONS")
        .env_remove("NODE_PATH")
        .current_dir(source.root())
        .output()
        .unwrap();
    assert!(
        command.status.success(),
        "source read oracle: {}",
        String::from_utf8_lossy(&command.stderr)
    );
    serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap()
}

#[tokio::test]
async fn read_windows_metadata_and_cards_match_pinned_source() {
    let requests = requests();
    let observations = source_observations(&requests);
    assert_eq!(observations.windows.len(), requests.windows.len());
    for (index, (case, expected)) in requests
        .windows
        .iter()
        .zip(observations.windows)
        .enumerate()
    {
        let actual = match build_window(case.chunks.clone(), &case.request, &case.path) {
            Ok(window) => JsonValue::object([
                (
                    "render",
                    format_read_output(
                        &case.path,
                        &FileReadOutcome {
                            offset: case.request.offset,
                            lines: window.lines.clone(),
                            total_lines: window.total_lines,
                            truncated_by_bytes: Some(window.truncated_by_bytes),
                        },
                    )
                    .into(),
                ),
                ("value", JsonValue::from_serialize(&window).unwrap()),
            ]),
            Err(error) => {
                let error = error.downcast::<FsError>().unwrap();
                JsonValue::from(json!({"error":{"message":error.to_string(),"code":error.code}}))
            }
        };
        assert_eq!(actual, expected, "window {index}");
    }
    assert_eq!(observations.metadata.len(), requests.metadata.len());
    for (index, (meta, expected)) in requests
        .metadata
        .iter()
        .zip(observations.metadata)
        .enumerate()
    {
        assert_eq!(
            JsonValue::from_serialize(&read_meta_from_meta(meta)).unwrap(),
            expected,
            "metadata {index}"
        );
    }
    assert_eq!(observations.languages.len(), requests.languages.len());
    for (path, expected) in requests.languages.iter().zip(observations.languages) {
        assert_eq!(lang_from_path(path), expected.as_deref(), "language {path}");
    }
    let context = Context::new();
    let prompt = seekdeep_system_prompt::install(
        &context,
        seekdeep_system_prompt::SystemPromptConfig::default(),
    )
    .unwrap();
    let runtime = seekdeep_tools::install(&context, &prompt, ToolRuntimeConfig::default()).unwrap();
    seekdeep_tool_fs::apply_read_tool(
        &context,
        &seekdeep_tool_fs::Config::default().resolved().unwrap(),
    )
    .unwrap();
    let definition = runtime.get("read", None).unwrap();
    let presenter = definition.present_result.as_ref().unwrap();
    let args = JsonValue::from(json!({"file_path":"file.rs"}));
    assert_eq!(
        observations.presentations.len(),
        requests.presentations.len()
    );
    for (index, (result, expected)) in requests
        .presentations
        .iter()
        .zip(observations.presentations)
        .enumerate()
    {
        assert_eq!(
            JsonValue::from_serialize(&presenter(&args, result)).unwrap(),
            expected,
            "presentation {index}"
        );
    }
    context.fiber().dispose().await.unwrap();
}
