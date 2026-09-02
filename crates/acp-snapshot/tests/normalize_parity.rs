//! ACP transcript, Session-log, cwd, spill, and request-header normalization parity.

use indexmap::IndexMap;
use seekdeep_acp_snapshot::{
    CwdPathMode, NormalizeContext, NormalizeOptions, extract_snapshot_spill_paths,
    normalize_session_log, normalize_stdout, scrub_request_headers, scrub_system_prompts,
    scrub_tool_schemas, tokenize_session_fixture_cwd,
};
use serde_json::{Value, json};

fn context() -> NormalizeContext {
    NormalizeContext {
        session_ids: vec!["11111111-2222-3333-4444-555555555555".to_owned()],
        cwd: "/tmp/acp-snap-cwd-abc123".to_owned(),
        cwd_aliases: Vec::new(),
    }
}

fn compact(value: &Value) -> String {
    serde_json::to_string(value).unwrap()
}

fn canonical() -> NormalizeOptions {
    NormalizeOptions::default()
}

fn native() -> NormalizeOptions {
    NormalizeOptions {
        cwd_path_mode: CwdPathMode::Native,
    }
}

fn normalized_stdout(raw: &str, context: &NormalizeContext) -> String {
    normalize_stdout(raw, context, canonical()).unwrap()
}

fn normalized_log(raw: &str, context: &NormalizeContext) -> String {
    normalize_session_log(raw, context, canonical()).unwrap()
}

fn header(overrides: &[(&str, Value)]) -> String {
    let mut value = json!({"type":"session","version":0,"id":"s","createdAt":123});
    let object = value.as_object_mut().unwrap();
    for (key, entry) in overrides {
        object.insert((*key).to_owned(), entry.clone());
    }
    compact(&value)
}

fn event(overrides: &[(&str, Value)]) -> String {
    let mut value = json!({"type":"turn/start","seq":1,"time":999,"data":{"turn":1}});
    let object = value.as_object_mut().unwrap();
    for (key, entry) in overrides {
        object.insert((*key).to_owned(), entry.clone());
    }
    compact(&value)
}

#[test]
fn stdout_rewrites_rpc_ids_to_first_seen_sequence() {
    let raw = [
        compact(&json!({"jsonrpc":"2.0","id":42,"method":"initialize"})),
        compact(&json!({"jsonrpc":"2.0","id":42,"result":{}})),
        compact(&json!({"jsonrpc":"2.0","id":99,"method":"session/new"})),
    ]
    .join("\n");
    let output = normalized_stdout(&raw, &context());
    assert!(output.contains("\"id\":1"));
    assert!(output.contains("\"id\":2"));
    assert!(!output.contains("42"));
    assert!(!output.contains("99"));
}

#[test]
fn stdout_scrubs_cwd_and_session_identity_at_any_depth() {
    let context = context();
    let raw = compact(&json!({
        "jsonrpc":"2.0",
        "method":"session/update",
        "params":{
            "sessionId":context.session_ids[0],
            "cwd":context.cwd,
            "note":format!("at {}/x", context.cwd),
        },
    }));
    let output = normalized_stdout(&raw, &context);
    assert!(output.contains("{{sessionId}}"));
    assert!(output.contains("{{cwd}}"));
    assert!(!output.contains(&context.cwd));
    assert!(!output.contains(&context.session_ids[0]));
}

#[test]
fn stdout_cwd_matching_honors_file_uri_and_punctuation_boundaries() {
    let context = context();
    let raw = compact(&json!({
        "jsonrpc":"2.0",
        "method":"session/update",
        "params":{
            "uri":format!("file://{}/proof.txt", context.cwd),
            "punctuated":format!("{}.,", context.cwd),
            "dottedSegment":format!("{}.backup", context.cwd),
            "dashedSegment":format!("{}-backup", context.cwd),
        },
    }));
    let frame: Value = serde_json::from_str(normalized_stdout(&raw, &context).trim()).unwrap();
    assert_eq!(frame["params"]["uri"], "file://{{cwd}}/proof.txt");
    assert_eq!(frame["params"]["punctuated"], "{{cwd}}.,");
    assert_eq!(
        frame["params"]["dottedSegment"],
        format!("{}.backup", context.cwd)
    );
    assert_eq!(
        frame["params"]["dashedSegment"],
        format!("{}-backup", context.cwd)
    );
}

#[test]
fn stdout_scrubs_every_cwd_spelling_longest_first() {
    let long = r"C:\Users\runneradmin\AppData\Local\Temp\acp-snapshot";
    let context = NormalizeContext {
        session_ids: Vec::new(),
        cwd: r"C:\Users\RUNNER~1\AppData\Local\Temp\acp-snapshot".to_owned(),
        cwd_aliases: vec![
            long.to_owned(),
            r"C:\Users\runneradmin\AppData\Local\Temp\acp".to_owned(),
        ],
    };
    let raw = compact(&json!({"cwd":long,"path":format!(r"{long}\nested\proof.txt")}));
    let frame: Value = serde_json::from_str(normalized_stdout(&raw, &context).trim()).unwrap();
    assert_eq!(
        frame,
        json!({"cwd":"{{cwd}}","path":"{{cwd}}/nested/proof.txt"})
    );
}

#[test]
fn stdout_canonicalizes_only_generated_path_separators() {
    let context = NormalizeContext {
        session_ids: Vec::new(),
        cwd: r"C:\Users\runner\AppData\Local\Temp\acp-snapshot".to_owned(),
        cwd_aliases: Vec::new(),
    };
    let raw = compact(&json!({
        "params":{
            "path":format!(r"{}\nested\proof.txt", context.cwd),
            "regex":r"\d+\w+",
            "command":r#"printf "\n""#,
        },
    }));
    let frame: Value = serde_json::from_str(normalized_stdout(&raw, &context).trim()).unwrap();
    assert_eq!(frame["params"]["path"], "{{cwd}}/nested/proof.txt");
    assert_eq!(frame["params"]["regex"], r"\d+\w+");
    assert_eq!(frame["params"]["command"], r#"printf "\n""#);
}

#[test]
fn stdout_canonicalizes_relative_path_fields_and_markers_only() {
    let raw = compact(&json!({
        "path":r"nested\AGENTS.md",
        "content":"<path>.\\nested\\task.txt</path>\nAdditional instructions from: nested\\AGENTS.md",
        "regex":r"\d+\w+",
    }));
    let frame: Value = serde_json::from_str(
        normalized_stdout(
            &raw,
            &NormalizeContext {
                cwd: "/unused".to_owned(),
                ..NormalizeContext::default()
            },
        )
        .trim(),
    )
    .unwrap();
    assert_eq!(frame["path"], "nested/AGENTS.md");
    assert_eq!(
        frame["content"],
        "<path>./nested/task.txt</path>\nAdditional instructions from: nested/AGENTS.md"
    );
    assert_eq!(frame["regex"], r"\d+\w+");
}

#[test]
fn stdout_can_preserve_native_cwd_rooted_separators() {
    let context = NormalizeContext {
        cwd: r"C:\work\snapshot".to_owned(),
        ..NormalizeContext::default()
    };
    let raw = compact(&json!({"path":format!(r"{}\nested\proof.txt", context.cwd)}));
    let frame: Value =
        serde_json::from_str(normalize_stdout(&raw, &context, native()).unwrap().trim()).unwrap();
    assert_eq!(frame["path"], r"{{cwd}}\nested\proof.txt");
}

#[test]
fn stdout_scrubs_unknown_uuid_shapes() {
    let raw = compact(&json!({
        "jsonrpc":"2.0",
        "method":"x",
        "params":{"id":"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"},
    }));
    assert!(normalized_stdout(&raw, &context()).contains("{{sessionId}}"));
}

#[test]
fn stdout_notifications_remain_without_rpc_ids() {
    let raw = compact(&json!({"jsonrpc":"2.0","method":"session/update","params":{}}));
    assert!(!normalized_stdout(&raw, &context()).contains("\"id\""));
}

#[test]
fn stdout_scrubs_only_event_read_top_level_time_and_omitted_bytes() {
    let text = "Session prior — title\nTarget event seq 4:\n```json\n{\n  \"seq\": 4,\n  \"time\": 1784876275593,\n  \"data\": {\n    \"time\": 31337,\n    \"note\": \"model-visible\"\n  }\n}\n```\n\nAfter:\n  \"time\": 424242,\n  neighbor semantic text\n\n(Omitted 39387 bytes. Full formatted result stored at: /tmp/result.txt.)";
    let raw = compact(&json!({"params":{"update":{"content":[{"content":{"text":text}}]}}}));
    let output = normalized_stdout(&raw, &context());
    assert!(output.contains("\\\"time\\\": {{eventTime}}"));
    assert!(output.contains("\\\"time\\\": 31337"));
    assert!(output.contains("\\\"time\\\": 424242"));
    assert!(output.contains("Omitted {{eventOmittedBytes}} bytes"));
    assert!(!output.contains("1784876275593"));
    assert!(!output.contains("39387"));
}

#[test]
fn stdout_preserves_event_like_values_in_unrelated_text() {
    let text = "bash output:\n```json\n{\n  \"time\": 1784876275593,\n  \"data\": {}\n}\n```\n\n(Omitted 39387 bytes. Full formatted result stored at: /tmp/result.txt.)";
    let raw = compact(&json!({"params":{"update":{"content":[{"content":{"text":text}}]}}}));
    let output = normalized_stdout(&raw, &context());
    assert!(output.contains("1784876275593"));
    assert!(output.contains("39387"));
    assert!(!output.contains("{{eventTime}}"));
    assert!(!output.contains("{{eventOmittedBytes}}"));
}

#[test]
fn stdout_rejects_non_json_protocol_noise() {
    let raw = format!(
        "{}\noops a log leaked\n",
        compact(&json!({"jsonrpc":"2.0","id":1}))
    );
    assert!(normalize_stdout(&raw, &context(), canonical()).is_err());
}

#[test]
fn stdout_ignores_blank_lines() {
    let raw = format!(
        "\n{}\n\n",
        compact(&json!({"jsonrpc":"2.0","id":1,"method":"m"}))
    );
    assert!(normalize_stdout(&raw, &context(), canonical()).is_ok());
}

#[test]
fn session_log_zeroes_header_created_at() {
    let output = normalized_log(&format!("{}\n", header(&[])), &context());
    assert!(output.contains("\"createdAt\":0"));
    assert!(!output.contains("123"));
}

#[test]
fn session_log_zeroes_event_time_and_keeps_sequence() {
    let raw = format!(
        "{}\n{}\n",
        header(&[]),
        event(&[("seq", json!(7)), ("time", json!(999))])
    );
    let output = normalized_log(&raw, &context());
    assert!(output.contains("\"time\":0"));
    assert!(output.contains("\"seq\":7"));
    assert!(!output.contains("999"));
}

#[test]
fn session_log_scrubs_cwd_deep_in_event_data() {
    let context = context();
    let event = compact(&json!({
        "type":"tool/result","seq":2,"time":5,
        "data":{"content":[{"type":"text","text":format!("wrote {}/proof.txt", context.cwd)}]},
    }));
    let output = normalized_log(
        &format!("{}\n{event}\n", header(&[("cwd", json!(context.cwd))])),
        &context,
    );
    assert!(output.contains("{{cwd}}"));
    assert!(!output.contains(&context.cwd));
}

#[test]
fn session_log_scrubs_file_uri_and_punctuation_boundaries() {
    let context = context();
    let event = compact(&json!({
        "type":"tool/result","seq":2,"time":5,
        "data":{
            "uri":format!("file://{}/proof.txt", context.cwd),
            "punctuated":format!("{}.,", context.cwd),
        },
    }));
    let output = normalized_log(&format!("{}\n{event}\n", header(&[])), &context);
    assert!(output.contains("file://{{cwd}}/proof.txt"));
    assert!(output.contains("{{cwd}}.,"));
    assert!(!output.contains(&format!("file://{}", context.cwd)));
}

fn spill_event(path: &str) -> String {
    compact(&json!({
        "type":"tool/result","seq":2,"time":5,
        "data":{"content":[{"type":"text","text":format!(
            "Full formatted result stored at: {path}. Use read with offset/limit, or grep this path to search within it."
        )}]},
    }))
}

#[test]
fn session_log_scrubs_random_local_spill_paths() {
    let context = context();
    let path = format!(
        "{}/.spill/session-c22bc3f1d2af/8a7b6c5d4e3f-bash.txt",
        context.cwd
    );
    let output = normalized_log(
        &format!("{}\n{}\n", header(&[]), spill_event(&path)),
        &context,
    );
    assert!(output.contains("{{spillLocator:bash.txt}}"));
    assert!(!output.contains("session-c22bc3f1d2af"));
    assert!(!output.contains("8a7b6c5d4e3f"));
}

#[test]
fn session_log_scrubs_macos_private_alias_for_local_spills() {
    let context = context();
    let path = format!(
        "/private{}/.spill/session-c22bc3f1d2af/8a7b6c5d4e3f-bash.txt",
        context.cwd
    );
    let output = normalized_log(
        &format!("{}\n{}\n", header(&[]), spill_event(&path)),
        &context,
    );
    assert!(output.contains("{{spillLocator:bash.txt}}"));
    assert!(!output.contains("/private{{spillLocator"));
}

#[test]
fn session_log_collapses_macos_private_prefix_on_cwd_paths() {
    let context = context();
    let event = compact(&json!({
        "type":"tool/result","seq":2,"time":5,
        "data":{"content":[{"type":"text","text":format!(
            "The file /private{}/config.txt has been updated successfully.", context.cwd
        )}]},
    }));
    let output = normalized_log(&format!("{}\n{event}\n", header(&[])), &context);
    assert!(output.contains("{{cwd}}/config.txt"));
    assert!(!output.contains("/private{{cwd}}"));
}

#[test]
fn session_log_scrubs_fixed_snapshot_spills() {
    let path = "/tmp/dsh-acp-snapshot-spill/session-c22bc3f1d2af/8a7b6c5d4e3f-bash.txt";
    let output = normalized_log(
        &format!("{}\n{}\n", header(&[]), spill_event(path)),
        &context(),
    );
    assert!(output.contains("{{spillLocator:bash.txt}}"));
    assert!(!output.contains("/tmp/dsh-acp-snapshot-spill"));
}

#[test]
fn session_log_scrubs_scenario_owned_snapshot_spills() {
    let path = "/tmp/dsh-acp-snap-012345678/session-c22bc3f1d2af/8a7b6c5d4e3f-bash.txt";
    let output = normalized_log(
        &format!("{}\n{}\n", header(&[]), spill_event(path)),
        &context(),
    );
    assert!(output.contains("{{spillLocator:bash.txt}}"));
    assert!(!output.contains("/tmp/dsh-acp-snap-012345678"));
}

#[test]
fn session_log_scrubs_windows_snapshot_spills() {
    let path = r"C:\t\dsh-acp-snap-012345678\session-c22bc3f1d2af\8a7b6c5d4e3f-bash.txt";
    let output = normalized_log(
        &format!("{}\n{}\n", header(&[]), spill_event(path)),
        &context(),
    );
    assert!(output.contains("{{spillLocator:bash.txt}}"));
    assert!(!output.contains(r"C:\t\dsh-acp-snap-012345678"));
}

#[test]
fn session_log_shares_cwd_separator_modes_with_stdout() {
    let context = NormalizeContext {
        cwd: r"C:\work\snapshot".to_owned(),
        ..NormalizeContext::default()
    };
    let event = compact(&json!({
        "type":"tool/result","seq":2,"time":5,
        "data":{"path":format!(r"{}\nested\proof.txt", context.cwd)},
    }));
    let raw = format!("{}\n{event}\n", header(&[("cwd", json!(context.cwd))]));
    assert!(
        normalize_session_log(&raw, &context, canonical())
            .unwrap()
            .contains("{{cwd}}/nested/proof.txt")
    );
    assert!(
        normalize_session_log(&raw, &context, native())
            .unwrap()
            .contains(r"{{cwd}}\\nested\\proof.txt")
    );
}

#[test]
fn session_log_scrubs_header_session_identity() {
    let context = context();
    let output = normalized_log(
        &format!("{}\n", header(&[("id", json!(context.session_ids[0]))])),
        &context,
    );
    assert!(output.contains("{{sessionId}}"));
}

#[test]
fn session_log_zeroes_hook_duration_and_keeps_decision() {
    let event = compact(&json!({
        "type":"hook/result","seq":2,"time":5,
        "data":{"turn":1,"point":"UserPromptSubmit","handlerId":"h","decision":"block","exitCode":2,"durationMs":37},
    }));
    let output = normalized_log(&format!("{}\n{event}\n", header(&[])), &context());
    assert!(output.contains("\"durationMs\":0"));
    assert!(!output.contains("37"));
    assert!(output.contains("\"decision\":\"block\""));
}

#[test]
fn session_log_zeroes_packed_timing_but_keeps_sequence_and_payload() {
    let row = compact(&json!({
        "type":"text-chunks","seq0":7,"time0":999,
        "data":{"turn":1,"step":1,"index":0,"dt":[212,27,0],"texts":["a","b","c","d"]},
    }));
    let output = normalized_log(&format!("{}\n{row}\n", header(&[])), &context());
    assert!(output.contains("\"time0\":0"));
    assert!(output.contains("\"dt\":[0,0,0]"));
    assert!(output.contains("\"seq0\":7"));
    assert!(output.contains("\"texts\":[\"a\",\"b\",\"c\",\"d\"]"));
    assert!(!output.contains("999"));
    assert!(!output.contains("212"));
}

#[test]
fn session_log_zeroes_time0_without_a_gap_array() {
    let row = compact(&json!({"type":"text-chunks","seq0":1,"time0":999,"data":"not-an-object"}));
    let output = normalized_log(&format!("{}\n{row}\n", header(&[])), &context());
    assert!(output.contains("\"time0\":0"));
    assert!(!output.contains("999"));
}

#[test]
fn session_log_preserves_non_hook_duration() {
    let event = compact(&json!({"type":"tool/result","seq":2,"time":5,"data":{"durationMs":88}}));
    let output = normalized_log(&format!("{}\n{event}\n", header(&[])), &context());
    assert!(output.contains("\"durationMs\":88"));
}

#[test]
fn session_log_tolerates_missing_volatile_fields() {
    let raw = [
        compact(&json!({"type":"session","id":"s"})),
        compact(&json!({"type":"note","seq":1})),
        compact(&json!({"type":"hook/result","seq":2,"time":5,"data":{"decision":"allow"}})),
        compact(&json!({"type":"hook/result","seq":3,"time":6,"data":null})),
        String::new(),
    ]
    .join("\n");
    let output = normalized_log(&raw, &context());
    assert!(output.contains("\"type\":\"note\",\"seq\":1"));
    assert!(output.contains("\"decision\":\"allow\""));
    assert!(!output.contains("durationMs"));
}

fn assert_tokenized_workspace(cwd: &str, reported_cwd: &str) {
    let raw = [
        compact(&json!({"type":"session","id":"s","createdAt":1,"cwd":cwd})),
        compact(&json!({
            "type":"tool/result","seq":1,"time":2,
            "data":{"content":[{"type":"text","text":format!(
                "wrote {reported_cwd}/proof.txt. alias /different/root/acp-snap-cwd-abc123/alias.txt. cwd {cwd}. Next; kept {cwd}-backup, {cwd}.backup, and /tmp/authored.txt"
            )}]},
        })),
        String::new(),
    ]
    .join("\n");
    let output = tokenize_session_fixture_cwd(&raw).unwrap();
    let record: Value = serde_json::from_str(output.lines().nth(1).unwrap()).unwrap();
    let text = record["data"]["content"][0]["text"].as_str().unwrap();
    assert!(output.contains("\"cwd\":\"{{cwd}}\""));
    assert!(text.contains("wrote {{cwd}}/proof.txt"));
    assert!(text.contains("alias {{cwd}}/alias.txt"));
    assert!(text.contains("cwd {{cwd}}. Next"));
    assert!(text.contains(&format!("{cwd}-backup")));
    assert!(text.contains(&format!("{cwd}.backup")));
    assert!(text.contains("/tmp/authored.txt"));
    assert!(!text.contains(&format!("{reported_cwd}/proof.txt")));
    assert_eq!(tokenize_session_fixture_cwd(&output).unwrap(), output);
}

#[test]
fn fixture_tokenization_handles_macos_workspaces() {
    assert_tokenized_workspace(
        "/var/folders/2g/snapshot/T/acp-snap-cwd-abc123",
        "/private/var/folders/2g/snapshot/T/acp-snap-cwd-abc123",
    );
}

#[test]
fn fixture_tokenization_handles_linux_workspaces() {
    assert_tokenized_workspace("/tmp/acp-snap-cwd-abc123", "/tmp/acp-snap-cwd-abc123");
}

#[test]
fn fixture_tokenization_handles_windows_workspaces() {
    assert_tokenized_workspace(
        r"C:\Users\runner\AppData\Local\Temp\acp-snap-cwd-abc123",
        r"C:\Users\runner\AppData\Local\Temp\acp-snap-cwd-abc123",
    );
}

#[test]
fn fixture_tokenization_collapses_private_prefix_around_existing_token() {
    let raw = [
        compact(&json!({"type":"session","id":"s","createdAt":1,"cwd":"{{cwd}}"})),
        compact(&json!({
            "type":"tool/result","seq":1,"time":2,
            "data":{"content":[{"type":"text","text":"wrote /private{{cwd}}/proof.txt"}]},
        })),
        String::new(),
    ]
    .join("\n");
    let output = tokenize_session_fixture_cwd(&raw).unwrap();
    assert!(output.contains("wrote {{cwd}}/proof.txt"));
    assert!(!output.contains("/private{{cwd}}"));
    assert_eq!(tokenize_session_fixture_cwd(&output).unwrap(), output);
}

#[test]
fn fixture_tokenization_rejects_a_missing_cwd() {
    assert_eq!(
        tokenize_session_fixture_cwd("").unwrap_err().to_string(),
        "acp-snapshot: cannot tokenize a cwd without a basename"
    );
}

#[test]
fn spill_extraction_uses_full_paths_and_last_match_per_name() {
    let log = [
        "Full formatted result stored at: /tmp/dsh-acp-snapshot-spill/session-c22bc3f1d2af/8a7b6c5d4e3f-bash.txt. Use read with offset/limit, or grep this path to search within it.",
        "stale copy at /tmp/dsh-acp-snap-012345678/session-aaaaaaaaaaaa/bbbbbbbbbbbb-grep.txt then",
        "fresh copy at /tmp/dsh-acp-snap-012345678/session-cccccccccccc/dddddddddddd-grep.txt then",
    ]
    .join("\n");
    let expected = IndexMap::from([
        (
            "bash.txt".to_owned(),
            "/tmp/dsh-acp-snapshot-spill/session-c22bc3f1d2af/8a7b6c5d4e3f-bash.txt".to_owned(),
        ),
        (
            "grep.txt".to_owned(),
            "/tmp/dsh-acp-snap-012345678/session-cccccccccccc/dddddddddddd-grep.txt".to_owned(),
        ),
    ]);
    assert_eq!(extract_snapshot_spill_paths(&log), expected);
}

#[test]
fn spill_extraction_returns_empty_without_snapshot_paths() {
    assert!(extract_snapshot_spill_paths("no spill paths here, only /tmp/other.txt\n").is_empty());
}

fn header_event(header: &Value) -> String {
    compact(&json!({
        "type":"request/header","seq":3,"time":9,
        "data":{"header":header,"reason":"initial"},
    }))
}

#[test]
fn request_header_scrub_replaces_system_and_tools_but_keeps_structure() {
    let session = compact(&json!({"type":"session","version":0,"id":"s","createdAt":1,"cwd":"/w"}));
    let request = header_event(&json!({
        "config":{"model":"m"},
        "system":"You are an agent.\nBe brief.",
        "tools":[{"name":"read","description":"Read a file.","parameters":{"type":"object"}}],
    }));
    let output = scrub_request_headers(&format!("{session}\n{request}\n")).unwrap();
    assert!(output.contains("\"system\":\"{{system}}\""));
    assert!(output.contains("\"tools\":\"{{tools}}\""));
    assert!(output.contains("\"config\":{\"model\":\"m\"}"));
    assert!(output.contains("\"reason\":\"initial\""));
    assert!(!output.contains("You are an agent"));
    assert!(!output.contains("Read a file"));
}

#[test]
fn request_header_scrub_keeps_absent_fields_absent() {
    let output = scrub_request_headers(&format!(
        "{}\n",
        header_event(&json!({"config":{"model":"m"}}))
    ))
    .unwrap();
    assert!(!output.contains("{{system}}"));
    assert!(!output.contains("{{tools}}"));
}

#[test]
fn request_header_scrub_handles_one_present_field() {
    let system = scrub_request_headers(&format!(
        "{}\n",
        header_event(&json!({"system":"secret prompt"}))
    ))
    .unwrap();
    assert!(system.contains("\"system\":\"{{system}}\""));
    assert!(!system.contains("{{tools}}"));
    let tools = scrub_request_headers(&format!(
        "{}\n",
        header_event(&json!({"tools":[{"name":"t"}]}))
    ))
    .unwrap();
    assert!(tools.contains("\"tools\":\"{{tools}}\""));
    assert!(!tools.contains("{{system}}"));
}

#[test]
fn request_header_scrub_keeps_malformed_payloads_byte_exact() {
    let session = compact(&json!({"type":"session","version":0,"id":"s","createdAt":1,"cwd":"/w"}));
    let headerless =
        compact(&json!({"type":"request/header","seq":10,"time":9,"data":{"reason":"initial"}}));
    let null_data = compact(&json!({"type":"request/header","seq":11,"time":9,"data":null}));
    let raw = format!("{session}\n{headerless}\n{null_data}\n");
    assert_eq!(scrub_request_headers(&raw).unwrap(), raw);
}

#[test]
fn request_header_scrub_passes_other_lines_and_is_idempotent() {
    let session = compact(&json!({"type":"session","version":0,"id":"s","createdAt":1,"cwd":"/w"}));
    let request = header_event(&json!({"config":{"model":"m"},"system":"s","tools":[]}));
    let other = compact(&json!({
        "type":"assistant/chunk","seq":4,"time":9,
        "data":{"turn":1,"step":1,"chunk":{"type":"text-delta","index":0,"text":"hi"}},
    }));
    let raw = format!("{session}\n{request}\n{other}\n");
    let once = scrub_request_headers(&raw).unwrap();
    assert_eq!(once.lines().next(), Some(session.as_str()));
    assert_eq!(once.lines().nth(2), Some(other.as_str()));
    assert_eq!(scrub_request_headers(&once).unwrap(), once);
}

#[test]
fn system_scrub_keeps_tool_schemas_verbatim() {
    let initial = compact(&json!({
        "type":"request/header","seq":1,"time":2,
        "data":{"header":{"system":"full prompt","tools":[{"name":"read","description":"full schema"}]},"reason":"initial"},
    }));
    let changed = compact(&json!({
        "type":"request/header","seq":2,"time":3,
        "data":{"header":{"system":"new prompt","tools":[{"name":"read","description":"changed schema"}]},"reason":"change"},
    }));
    let tools_only = compact(&json!({
        "type":"request/header","seq":3,"time":4,
        "data":{"header":{"tools":[{"name":"read","description":"schema only"}]},"reason":"resume"},
    }));
    let output = scrub_system_prompts(&format!("{initial}\n{changed}\n{tools_only}\n")).unwrap();
    assert!(output.contains("\"system\":\"{{system}}\""));
    assert!(!output.contains("full prompt"));
    assert!(!output.contains("new prompt"));
    assert!(output.contains("full schema"));
    assert!(output.contains("changed schema"));
    assert_eq!(output.lines().nth(2), Some(tools_only.as_str()));
    assert_eq!(scrub_system_prompts(&output).unwrap(), output);
}

#[test]
fn tool_scrub_keeps_system_prompts_verbatim() {
    let initial = compact(&json!({
        "type":"request/header","seq":1,"time":2,
        "data":{"header":{"system":"full prompt","tools":[{"name":"read","description":"full schema","parameters":{"type":"object"}}]},"reason":"initial"},
    }));
    let changed = compact(&json!({
        "type":"request/header","seq":2,"time":3,
        "data":{"header":{"system":"new prompt","tools":[{"name":"grep","description":"new schema"}]},"reason":"change"},
    }));
    let system_only = compact(&json!({
        "type":"request/header","seq":3,"time":4,
        "data":{"header":{"system":"prompt only"},"reason":"resume"},
    }));
    let output = scrub_tool_schemas(&format!("{initial}\n{changed}\n{system_only}\n")).unwrap();
    assert_eq!(output.matches("\"tools\":\"{{tools}}\"").count(), 2);
    assert!(!output.contains("full schema"));
    assert!(!output.contains("new schema"));
    assert!(output.contains("full prompt"));
    assert!(output.contains("new prompt"));
    assert_eq!(output.lines().nth(2), Some(system_only.as_str()));
    assert_eq!(scrub_tool_schemas(&output).unwrap(), output);
}
