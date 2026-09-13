//! Fixture guards, stable refresh, and real suite-runner parity.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use seekdeep_acp_snapshot::{
    AgentUnderTest, FixtureReplacement, HarvestedLog, NamedSnapshotContent, NormalizeContext,
    SnapshotScenario, SnapshotSuiteMode, SnapshotSuiteOptions, SnapshotSuitePlatform,
    assert_child_system_prompt_snapshot, assert_unique_snapshot_contents, claim_shared_snapshot,
    define_acp_snapshot_suite, fixture_context, format_system_prompt_snapshot,
    format_tool_schemas_snapshot, header_change_count, normalized_headers,
    normalized_system_prompts, normalized_tool_schemas, parse_tool_schemas_snapshot,
    refresh_fixture_replacements, restore_pinned_tool_schemas, scenario_skipped,
    session_fixture_names, stabilize_fixture_message_ids, stabilize_refresh_log,
    stdout_expected_variants, tokenize_session_fixture_cwd, unknown_tool_call_ids,
};
use serde_json::{Value, json};
use walkdir::WalkDir;

fn fixture_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_acp-snapshot-launcher-fixture"))
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/test-support/acp-snapshot/tests/fixtures")
}

fn agent() -> AgentUnderTest {
    AgentUnderTest {
        source_bin: fixture_binary(),
        library_bin: Some(fixture_binary()),
        config_path: fixture_root().join("cordis.yml"),
        tsconfig_path: fixture_root().join("tsconfig.json"),
    }
}

fn scenario(name: &str, has_model_turn: bool, recorded: bool) -> SnapshotScenario {
    SnapshotScenario {
        name: name.to_owned(),
        has_model_turn,
        recorded,
        ..SnapshotScenario::default()
    }
}

fn replay_scenarios() -> Vec<SnapshotScenario> {
    let mut pin = scenario("pin-turn", true, true);
    pin.pins_header = true;
    pin.expected_header_changes = 1;
    pin.header_class = Some("main".to_owned());

    let mut shared = scenario("shared-pin", true, true);
    shared.pins_header = true;
    shared.expected_header_changes = 1;
    shared.header_class = Some("shared".to_owned());
    shared.system_prompt_source = Some("pin-turn".to_owned());
    shared.tool_schemas_source = Some("pin-turn".to_owned());

    let mut plain = scenario("plain-turn", true, true);
    plain.header_class = Some("main".to_owned());
    plain
        .environment
        .insert("SEEKDEEP_PERMISSION_MODE".into(), "never".into());
    plain.pins_child_tool_schemas = vec![1];
    plain.pins_child_system_prompts = vec![1];
    plain.prepare_workspace = Some(Arc::new(|cwd| {
        Box::pin(async move {
            tokio::fs::write(cwd.join("seed.txt"), "prepared at runtime").await?;
            Ok(())
        })
    }));

    let mut no_model = scenario("no-model", false, false);
    no_model.header_class = Some("main".to_owned());
    let mut blocked = scenario("blocked-log", false, false);
    blocked.compares_log = Some(true);
    blocked.header_class = Some("main".to_owned());
    let mut authored = scenario("authored-error", true, false);
    authored.overridden = true;
    authored.header_class = Some("main".to_owned());
    vec![pin, shared, plain, no_model, blocked, authored]
}

fn record_scenarios() -> Vec<SnapshotScenario> {
    let mut pin = scenario("rec-pin", true, true);
    pin.pins_header = true;
    let mut child = scenario("rec-child", true, true);
    child.pins_child_tool_schemas = vec![1];
    let mut skipped = scenario("rec-skip", true, false);
    skipped.overridden = true;
    vec![pin, child, skipped]
}

fn options(
    snapshots_dir: PathBuf,
    scenarios: Vec<SnapshotScenario>,
    mode: SnapshotSuiteMode,
) -> SnapshotSuiteOptions {
    SnapshotSuiteOptions {
        agent: agent(),
        snapshots_dir,
        scenarios,
        mode,
        has_pwsh: None,
        replay_max_concurrency: 5,
    }
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in WalkDir::new(source).min_depth(1) {
        let entry = entry.unwrap();
        let relative = entry.path().strip_prefix(source).unwrap();
        let target = destination.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(target).unwrap();
        } else if entry.file_type().is_file() {
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn log(content: &str) -> HarvestedLog {
    HarvestedLog {
        id: "diagnostic".to_owned(),
        created_at: 1.0,
        parent_session: None,
        content: content.to_owned(),
    }
}

fn stabilize(fresh: &str, existing: &str) -> String {
    stabilize_refresh_log(fresh, existing, &[], &fixture_context(fresh).unwrap()).unwrap()
}

#[test]
fn skip_and_stdout_variant_selection_match_mode_and_host_contracts() {
    let mut authored = scenario("authored", true, false);
    let mut posix = scenario("posix", true, true);
    posix.posix_only = true;
    let mut pwsh = scenario("pwsh", true, true);
    pwsh.pwsh_only = true;
    assert!(scenario_skipped(
        &authored,
        true,
        SnapshotSuitePlatform::Other,
        None
    ));
    assert!(!scenario_skipped(
        &authored,
        false,
        SnapshotSuitePlatform::Other,
        None
    ));
    assert!(scenario_skipped(
        &posix,
        false,
        SnapshotSuitePlatform::Windows,
        Some(true)
    ));
    assert!(scenario_skipped(
        &pwsh,
        false,
        SnapshotSuitePlatform::Other,
        Some(false)
    ));
    assert!(!scenario_skipped(
        &pwsh,
        false,
        SnapshotSuitePlatform::Other,
        Some(true)
    ));

    authored.pins_native_windows_stdout = true;
    let windows = stdout_expected_variants(&authored, SnapshotSuitePlatform::Windows);
    assert_eq!(windows.len(), 2);
    assert_eq!(windows[0].file, "stdout.expected.jsonl");
    assert_eq!(windows[1].file, "stdout.expected.windows.jsonl");
    assert_eq!(
        stdout_expected_variants(&authored, SnapshotSuitePlatform::Other).len(),
        1
    );
}

#[test]
fn shared_claims_and_committed_duplicate_contents_fail_loud() {
    let mut claims = BTreeMap::new();
    claim_shared_snapshot(&mut claims, "shared", "one", "same").unwrap();
    claim_shared_snapshot(&mut claims, "shared", "two", "same").unwrap();
    let error = claim_shared_snapshot(&mut claims, "shared", "three", "different").unwrap_err();
    assert!(error.to_string().contains("diverged between one and three"));

    let duplicate = [
        NamedSnapshotContent {
            path: "a".into(),
            content: "same".into(),
        },
        NamedSnapshotContent {
            path: "b".into(),
            content: "same".into(),
        },
    ];
    assert!(
        assert_unique_snapshot_contents("prompt", &duplicate)
            .unwrap_err()
            .to_string()
            .contains("identical prompt snapshots")
    );
}

#[test]
fn session_fixture_inventory_requires_primary_contiguous_unique_children() {
    assert_eq!(
        session_fixture_names(&[
            "noise.txt".into(),
            "session.2.jsonl".into(),
            "session.jsonl".into(),
            "session.1.jsonl".into(),
        ])
        .unwrap(),
        ["session.jsonl", "session.1.jsonl", "session.2.jsonl"]
    );
    assert!(session_fixture_names(&["session.1.jsonl".into()]).is_err());
    assert!(
        session_fixture_names(&["session.jsonl".into(), "session.2.jsonl".into()])
            .unwrap_err()
            .to_string()
            .contains("contiguous")
    );
    assert!(
        session_fixture_names(&["session.jsonl".into(), "session.0.jsonl".into()])
            .unwrap_err()
            .to_string()
            .contains("invalid child")
    );
    assert!(
        session_fixture_names(&[
            "session.jsonl".into(),
            "session.1.jsonl".into(),
            "session.1.jsonl".into(),
        ])
        .unwrap_err()
        .to_string()
        .contains("contiguous")
    );
}

#[test]
fn fixture_context_and_header_extractors_normalize_volatile_values() {
    let id = "11111111-2222-4333-8444-555555555555";
    let input = format!(
        "{}\n{}\n{}\n",
        json!({"type":"session","id":id,"createdAt":5,"cwd":"/w"}),
        json!({"type":"request/header","time":9,"data":{"header":{"system":"work in /w","tools":[{"description":"read /w"}]}}}),
        json!({"type":"request/header","time":9,"data":{"header":{"system":null,"tools":null}}})
    );
    assert_eq!(
        fixture_context(&input).unwrap(),
        NormalizeContext {
            session_ids: vec![id.into()],
            cwd: "/w".into(),
            cwd_aliases: Vec::new(),
        }
    );
    assert_eq!(
        normalized_headers(&input, &fixture_context(&input).unwrap())
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        normalized_system_prompts(&input, &fixture_context(&input).unwrap()).unwrap(),
        ["work in {{cwd}}"]
    );
    assert_eq!(
        normalized_tool_schemas(&input, &fixture_context(&input).unwrap()).unwrap(),
        [vec![json!({"description":"read {{cwd}}"})]]
    );
    assert_eq!(fixture_context("").unwrap().cwd, "\0no-cwd\0");
}

#[test]
fn prompt_and_schema_sidecars_are_canonical_and_validated() {
    assert_eq!(format_system_prompt_snapshot("prompt", &[]), "prompt\n");
    assert_eq!(
        format_system_prompt_snapshot("prompt\n", &["new\nlines".into()]),
        "prompt\n\n<!-- request/header change 1 -->\n\nnew\nlines\n"
    );
    let initial = vec![json!({"name":"read","description":"Read."})];
    let changes = vec![vec![json!({"name":"grep"})]];
    let formatted = format_tool_schemas_snapshot(&initial, &changes).unwrap();
    let parsed = parse_tool_schemas_snapshot(&formatted).unwrap();
    assert_eq!(parsed.initial, initial);
    assert_eq!(parsed.changes, changes);
    assert_eq!(
        restore_pinned_tool_schemas(
            &json!({"system":"{{system}}","tools":"{{tools}}"}),
            &parsed.initial
        )
        .unwrap(),
        json!({"system":"{{system}}","tools":[{"name":"read","description":"Read."}]})
    );
    for invalid in ["null", "\"invalid\"", "[]"] {
        assert!(parse_tool_schemas_snapshot(invalid).is_err());
    }
    assert!(parse_tool_schemas_snapshot("{\"initial\":{},\"changes\":[]}").is_err());
    assert!(restore_pinned_tool_schemas(&json!({"tools":[]}), &[]).is_err());
    assert_child_system_prompt_snapshot("CHILD\n", "CLASS\n", "child").unwrap();
    assert!(assert_child_system_prompt_snapshot("\n", "CLASS\n", "child").is_err());
    assert!(assert_child_system_prompt_snapshot("CHILD", "CLASS\n", "child").is_err());
    assert!(assert_child_system_prompt_snapshot("CLASS\n", "CLASS\n", "child").is_err());
}

#[test]
fn header_changes_and_unknown_tools_use_structural_fields_only() {
    let content = concat!(
        "{\"type\":\"request/header\",\"data\":{\"reason\":\"initial\"}}\n",
        "{\"type\":\"turn/start\"}\n",
        "{\"type\":\"request/header\",\"data\":{\"reason\":\"change\"}}\n",
        "{\"type\":\"request/header\",\"data\":{\"reason\":\"change\"}}\n"
    );
    assert_eq!(header_change_count(content).unwrap(), 2);
    let results = concat!(
        "{\"type\":\"tool/result\",\"data\":{\"message\":{\"source\":{\"callId\":\"missing\"}},\"error\":{\"code\":\"UNKNOWN_TOOL\"}}}\n",
        "{\"type\":\"tool/result\",\"data\":{\"error\":{\"code\":\"UNKNOWN_TOOL\"}}}\n",
        "{\"type\":\"tool/result\",\"data\":{\"error\":{\"code\":\"EXECUTION_FAILED\"}}}\n"
    );
    assert_eq!(
        unknown_tool_call_ids(results).unwrap(),
        ["missing", "<missing callId>"]
    );
}

fn message_log(session: &str, id: &str, text: &str) -> String {
    format!(
        "{}\n{}\n",
        json!({"type":"session","id":session,"cwd":"{{cwd}}"}),
        json!({"type":"user/message","data":{"id":id,"role":"user","content":[{"type":"text","text":text}],"source":{"kind":"user"}}})
    )
}

#[test]
fn stable_message_ids_reuse_only_complete_unique_surface_messages() {
    let fresh_id = "11111111-1111-4111-8111-111111111111";
    let old_id = "22222222-2222-4222-8222-222222222222";
    let fresh = [
        message_log("fresh-parent", fresh_id, "same"),
        message_log("fresh-child", fresh_id, "same"),
    ];
    let existing = [
        message_log("old-parent", old_id, "same"),
        message_log("old-child", old_id, "same"),
    ];
    for fixture in stabilize_fixture_message_ids(&fresh, &existing).unwrap() {
        assert!(fixture.contains(old_id));
        assert!(!fixture.contains(fresh_id));
    }

    let conflicting = format!(
        "{}{}",
        message_log("old", old_id, "same"),
        message_log("old", old_id, "different")
    );
    assert_eq!(
        stabilize_fixture_message_ids(&[fresh[0].clone()], &[conflicting]).unwrap(),
        [fresh[0].clone()]
    );
}

#[test]
fn stable_message_ids_touch_inbox_and_surface_owners_but_not_lookalikes() {
    let fresh = "11111111-1111-4111-8111-111111111111";
    let old = "22222222-2222-4222-8222-222222222222";
    let complete = |id: &str| json!({"id":id,"role":"user","content":[{"type":"text","text":"same"}],"source":{"kind":"user"}});
    let build = |id: &str| {
        [
            json!({"type":"agent/inbox/spliced","data":{"inserted":[complete(id),{"id":id,"role":"user","content":[],"source":null}]}}),
            json!({"type":"user/message","data":complete(id)}),
            json!({"type":"turn/start","data":{"id":id}}),
            json!({"type":"steering/message","data":complete(id)}),
        ]
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
            + "\n"
    };
    let stable = stabilize_fixture_message_ids(&[build(fresh)], &[build(old)])
        .unwrap()
        .remove(0);
    let records = stable
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(records[0].pointer("/data/inserted/0/id"), Some(&json!(old)));
    assert_eq!(
        records[0].pointer("/data/inserted/1/id"),
        Some(&json!(fresh))
    );
    assert_eq!(records[1].pointer("/data/id"), Some(&json!(old)));
    assert_eq!(records[2].pointer("/data/id"), Some(&json!(fresh)));
    assert_eq!(records[3].pointer("/data/id"), Some(&json!(fresh)));
}

#[test]
fn fixture_ready_cwd_tokenization_allows_message_identity_reuse() {
    let fresh_id = "11111111-1111-4111-8111-111111111111";
    let old_id = "22222222-2222-4222-8222-222222222222";
    let cwd = "/tmp/acp-snapshot-fresh-cwd";
    let message = |id: &str, path: &str| {
        json!({
            "type":"user/message",
            "data":{
                "id":id,
                "role":"user",
                "content":[{"type":"text","text":format!("read {path}/input.txt")}],
                "source":{"kind":"user"}
            }
        })
    };
    let fresh = tokenize_session_fixture_cwd(&format!(
        "{}\n{}\n",
        json!({"type":"session","id":"fresh","cwd":cwd}),
        message(fresh_id, cwd)
    ))
    .unwrap();
    let existing = format!(
        "{}\n{}\n",
        json!({"type":"session","id":"old","cwd":"{{cwd}}"}),
        message(old_id, "{{cwd}}")
    );
    assert!(stabilize_fixture_message_ids(&[fresh], &[existing]).unwrap()[0].contains(old_id));
}

#[test]
fn refresh_replacements_cover_headers_and_named_spills_but_not_message_ids() {
    let fresh_spill = "/tmp/seekdeep-acp-snapshot-spill/session-aaaaaaaaaaaa/bbbbbbbbbbbb-bash.txt";
    let old_spill = "/tmp/seekdeep-acp-snapshot-spill/session-cccccccccccc/dddddddddddd-bash.txt";
    let fresh = format!(
        "{{\"type\":\"session\",\"id\":\"new\",\"cwd\":\"/new\"}}\n{{\"type\":\"tool/result\",\"data\":{{\"text\":\"stored at: {fresh_spill} \"}}}}\n"
    );
    let old = format!(
        "{{\"type\":\"session\",\"id\":\"old\",\"cwd\":\"/old\"}}\n{{\"type\":\"tool/result\",\"data\":{{\"text\":\"stored at: {old_spill} \"}}}}\n"
    );
    assert_eq!(
        refresh_fixture_replacements(&[log(&fresh)], &[old]).unwrap(),
        [
            FixtureReplacement {
                from: "new".into(),
                to: "old".into(),
            },
            FixtureReplacement {
                from: "/new".into(),
                to: "/old".into(),
            },
            FixtureReplacement {
                from: fresh_spill.into(),
                to: old_spill.into(),
            },
        ]
    );
}

#[test]
fn refresh_stabilization_preserves_unpacked_and_packed_member_times() {
    let fresh = concat!(
        "{\"type\":\"session\",\"id\":\"same\",\"createdAt\":200}\n",
        "{\"type\":\"reasoning-chunks\",\"seq0\":2,\"time0\":200,\"data\":{\"turn\":1,\"step\":1,\"index\":0,\"dt\":[5,7],\"texts\":[\"new\",\"\",\" split\"]}}\n",
        "{\"type\":\"assistant/message\",\"seq\":5,\"time\":220,\"data\":{}}\n"
    );
    let existing = concat!(
        "{\"type\":\"session\",\"id\":\"same\",\"createdAt\":100}\n",
        "{\"type\":\"assistant/chunk\",\"seq\":2,\"time\":100,\"data\":{}}\n",
        "{\"type\":\"assistant/chunk\",\"seq\":3,\"time\":101,\"data\":{}}\n",
        "{\"type\":\"assistant/chunk\",\"seq\":4,\"time\":103,\"data\":{}}\n",
        "{\"type\":\"assistant/message\",\"seq\":5,\"time\":104,\"data\":{}}\n"
    );
    assert_eq!(
        stabilize(fresh, existing),
        concat!(
            "{\"type\":\"session\",\"id\":\"same\",\"createdAt\":100}\n",
            "{\"type\":\"reasoning-chunks\",\"seq0\":2,\"time0\":100,\"data\":{\"turn\":1,\"step\":1,\"index\":0,\"dt\":[1,2],\"texts\":[\"new\",\"\",\" split\"]}}\n",
            "{\"type\":\"assistant/message\",\"seq\":5,\"time\":104,\"data\":{}}\n"
        )
    );

    let packed_existing = concat!(
        "{\"type\":\"session\",\"id\":\"same\",\"createdAt\":100}\n",
        "{\"type\":\"reasoning-chunks\",\"seq0\":2,\"time0\":100,\"data\":{\"turn\":1,\"step\":1,\"index\":0,\"dt\":[1,2],\"texts\":[\"old\",\"chunk\",\"shape\"]}}\n"
    );
    assert!(
        stabilize(
            fresh
                .lines()
                .take(2)
                .collect::<Vec<_>>()
                .join("\n")
                .as_str(),
            packed_existing
        )
        .contains("\"time0\":100")
    );
}

#[test]
fn refresh_stabilization_aligns_inserted_titles_and_keeps_fresh_semantics() {
    let fresh = concat!(
        "{\"type\":\"session\",\"id\":\"same\",\"createdAt\":200}\n",
        "{\"type\":\"turn/start\",\"seq\":0,\"time\":21}\n",
        "{\"type\":\"user/message\",\"seq\":1,\"time\":22}\n",
        "{\"type\":\"session/title\",\"seq\":2,\"time\":999}\n",
        "{\"type\":\"step/start\",\"seq\":3,\"time\":1000}\n"
    );
    let existing = concat!(
        "{\"type\":\"session\",\"id\":\"same\",\"createdAt\":100}\n",
        "{\"type\":\"turn/start\",\"seq\":0,\"time\":11}\n",
        "{\"type\":\"user/message\",\"seq\":1,\"time\":12}\n",
        "{\"type\":\"step/start\",\"seq\":2,\"time\":13}\n"
    );
    assert_eq!(
        stabilize(fresh, existing),
        concat!(
            "{\"type\":\"session\",\"id\":\"same\",\"createdAt\":100}\n",
            "{\"type\":\"turn/start\",\"seq\":0,\"time\":11}\n",
            "{\"type\":\"user/message\",\"seq\":1,\"time\":12}\n",
            "{\"type\":\"session/title\",\"seq\":2,\"time\":12}\n",
            "{\"type\":\"step/start\",\"seq\":3,\"time\":13}\n"
        )
    );
}

#[test]
fn refresh_stabilization_reuses_only_bijective_correlated_strings() {
    let fresh_id = "11111111-1111-4111-8111-111111111111";
    let old_id = "22222222-2222-4222-8222-222222222222";
    let fresh = format!(
        "{{\"type\":\"session\",\"id\":\"same\",\"createdAt\":200,\"cwd\":\"/old\"}}\n{}\n{}\n",
        json!({"type":"approval/asked","data":{"id":fresh_id}}),
        json!({"type":"approval/decided","data":{"id":fresh_id,"outcome":"allowed-once"}})
    );
    let existing = format!(
        "{{\"type\":\"session\",\"id\":\"same\",\"createdAt\":100,\"cwd\":\"/old\"}}\n{}\n{}\n",
        json!({"type":"approval/asked","data":{"id":old_id}}),
        json!({"type":"approval/decided","data":{"id":old_id,"outcome":"rejected"}})
    );
    let stable = stabilize(&fresh, &existing);
    assert!(stable.contains(old_id));
    assert!(!stable.contains(fresh_id));
    assert!(stable.contains("allowed-once"));

    let ambiguous = format!(
        "{{\"type\":\"session\",\"id\":\"same\",\"createdAt\":200,\"cwd\":\"/old\"}}\n{}\n{}\n",
        json!({"type":"approval/asked","data":{"id":fresh_id}}),
        json!({"type":"approval/asked","data":{"id":"33333333-3333-4333-8333-333333333333"}})
    );
    assert!(stabilize(&ambiguous, &existing).contains(fresh_id));
}

#[test]
fn refresh_stabilization_keeps_fixture_volatiles_and_fresh_meaningful_payloads() {
    let fresh = concat!(
        "{\"type\":\"session\",\"id\":\"new-child\",\"createdAt\":200,\"cwd\":\"/new\",\"parentSession\":\"new-parent\",\"seedLength\":1}\n",
        "{\"type\":\"hook/result\",\"seq\":1,\"time\":22,\"data\":{\"decision\":\"block\",\"durationMs\":37}}\n",
        "{\"type\":\"turn/end\",\"seq\":2,\"time\":33,\"data\":{\"error\":\"fresh error\"}}\n",
        "{\"type\":\"tool/result\",\"seq\":3,\"time\":44,\"data\":{\"text\":\"new-parent in /new\"}}\n",
        "{\"type\":\"hook/result\",\"seq\":4,\"time\":55,\"data\":{\"decision\":\"allow\",\"durationMs\":5}}\n"
    );
    let existing = concat!(
        "{\"type\":\"session\",\"id\":\"old-child\",\"createdAt\":100,\"cwd\":\"/old\",\"parentSession\":\"old-parent\",\"seedLength\":5}\n",
        "{\"type\":\"hook/result\",\"seq\":1,\"time\":11,\"data\":{\"decision\":\"stale\",\"durationMs\":99}}\n",
        "{\"type\":\"turn/end\",\"seq\":2,\"data\":{\"error\":\"stale\"}}\n",
        "{\"type\":\"assistant/message\",\"seq\":3,\"time\":12,\"data\":{\"text\":\"different type\"}}\n",
        "{\"type\":\"hook/result\",\"seq\":4,\"time\":13,\"data\":{\"decision\":\"stale\"}}\n"
    );
    let output = stabilize_refresh_log(
        fresh,
        existing,
        &[
            FixtureReplacement {
                from: "new-parent".into(),
                to: "old-parent".into(),
            },
            FixtureReplacement {
                from: "new-child".into(),
                to: "old-child".into(),
            },
            FixtureReplacement {
                from: "/new".into(),
                to: "/old".into(),
            },
        ],
        &fixture_context(fresh).unwrap(),
    )
    .unwrap();
    assert_eq!(
        output,
        concat!(
            "{\"type\":\"session\",\"id\":\"old-child\",\"createdAt\":100,\"cwd\":\"/old\",\"parentSession\":\"old-parent\",\"seedLength\":1}\n",
            "{\"type\":\"hook/result\",\"seq\":1,\"time\":11,\"data\":{\"decision\":\"block\",\"durationMs\":99}}\n",
            "{\"type\":\"turn/end\",\"seq\":2,\"time\":33,\"data\":{\"error\":\"fresh error\"}}\n",
            "{\"type\":\"tool/result\",\"seq\":3,\"time\":44,\"data\":{\"text\":\"old-parent in /old\"}}\n",
            "{\"type\":\"hook/result\",\"seq\":4,\"time\":13,\"data\":{\"decision\":\"allow\",\"durationMs\":5}}\n"
        )
    );
}

#[test]
fn refresh_stabilization_preserves_normalized_equivalent_nested_values() {
    let fresh_id = "11111111-1111-4111-8111-111111111111";
    let old_id = "22222222-2222-4222-8222-222222222222";
    let fresh = format!(
        "{}\n{}\n",
        json!({"type":"session","id":"same","createdAt":200,"cwd":"/old"}),
        json!({
            "type":"approval/asked","seq":1,"time":22,
            "data":{
                "id":fresh_id,
                "outcome":"fresh",
                "aliases":[fresh_id,"fresh"],
                "resized":[fresh_id,"new"],
                "shape":{"shared":fresh_id,"added":true}
            }
        })
    );
    let existing = format!(
        "{}\n{}\n",
        json!({"type":"session","id":"same","createdAt":100,"cwd":"/old"}),
        json!({
            "type":"approval/asked","seq":1,"time":11,
            "data":{
                "id":old_id,
                "outcome":"stale",
                "aliases":[old_id,"stale"],
                "resized":[old_id],
                "shape":{"shared":old_id}
            }
        })
    );
    let output = stabilize(&fresh, &existing);
    let event: Value = serde_json::from_str(output.lines().nth(1).unwrap()).unwrap();
    assert_eq!(event.pointer("/data/id"), Some(&json!(old_id)));
    assert_eq!(event.pointer("/data/aliases/0"), Some(&json!(old_id)));
    assert_eq!(event.pointer("/data/aliases/1"), Some(&json!("fresh")));
    assert_eq!(event.pointer("/data/resized/0"), Some(&json!(fresh_id)));
    assert_eq!(event.pointer("/data/shape/shared"), Some(&json!(old_id)));
    assert_eq!(event.pointer("/data/shape/added"), Some(&json!(true)));
}

#[test]
fn refresh_stabilization_uses_fresh_cwd_aliases_for_existing_path_reuse() {
    let fresh_cwd = r"C:\Users\RUNNER~1\AppData\Local\Temp\acp-snap-cwd-new";
    let fresh_alias = r"C:\Users\runneradmin\AppData\Local\Temp\acp-snap-cwd-new";
    let existing_cwd = r"C:\Users\RUNNER~1\AppData\Local\Temp\acp-snap-cwd-old";
    let fresh = format!(
        "{}\n{}\n",
        json!({"type":"session","id":"same","createdAt":200,"cwd":fresh_cwd}),
        json!({"type":"tool/result","data":{"path":format!(r"{fresh_alias}\result.txt")}})
    );
    let existing = format!(
        "{}\n{}\n",
        json!({"type":"session","id":"same","createdAt":100,"cwd":existing_cwd}),
        json!({"type":"tool/result","data":{"path":format!(r"{existing_cwd}\result.txt")}})
    );
    let context = NormalizeContext {
        cwd_aliases: vec![fresh_alias.into()],
        ..fixture_context(&fresh).unwrap()
    };
    let output = stabilize_refresh_log(
        &fresh,
        &existing,
        &[FixtureReplacement {
            from: fresh_cwd.into(),
            to: existing_cwd.into(),
        }],
        &context,
    )
    .unwrap();
    let path = serde_json::from_str::<Value>(output.lines().nth(1).unwrap())
        .unwrap()
        .pointer("/data/path")
        .and_then(Value::as_str)
        .unwrap()
        .to_owned();
    assert_eq!(path, format!("{existing_cwd}\\result.txt"));
    assert!(!output.contains("runneradmin"));
}

#[test]
fn refresh_stabilization_disables_non_bijective_string_reuse() {
    let ids = BTreeMap::from([
        ("a", "11111111-1111-4111-8111-111111111111"),
        ("b", "22222222-2222-4222-8222-222222222222"),
        ("x", "33333333-3333-4333-8333-333333333333"),
        ("y", "44444444-4444-4444-8444-444444444444"),
    ]);
    let build = |names: &[&str]| {
        std::iter::once(
            "{\"type\":\"session\",\"id\":\"same\",\"createdAt\":100,\"cwd\":\"/old\"}".to_owned(),
        )
        .chain(names.iter().enumerate().map(|(index, name)| {
            json!({
                "type":if index < 2 {"approval/asked"} else {"approval/decided"},
                "data":{"id":ids[name]}
            })
            .to_string()
        }))
        .chain(std::iter::once(String::new()))
        .collect::<Vec<_>>()
        .join("\n")
    };
    for (fresh_names, old_names) in [
        (vec!["a", "b", "b", "a"], vec!["x", "y", "x", "y"]),
        (vec!["a", "b"], vec!["x", "x"]),
    ] {
        let output = stabilize(&build(&fresh_names), &build(&old_names));
        for name in fresh_names {
            assert!(output.contains(ids[name]));
        }
    }
}

#[test]
fn refresh_stabilization_keeps_fresh_packed_gaps_when_old_members_do_not_align() {
    let fresh = concat!(
        "{\"type\":\"session\",\"id\":\"same\",\"createdAt\":200}\n",
        "{\"type\":\"reasoning-chunks\",\"seq0\":2,\"time0\":200,\"data\":{\"turn\":1,\"step\":1,\"index\":0,\"dt\":[5,7],\"texts\":[\"new\",\"\",\" split\"]}}\n"
    );
    let absent = "{\"type\":\"session\",\"id\":\"same\",\"createdAt\":100}\n";
    let output = stabilize(fresh, absent);
    assert!(output.contains("\"time0\":200"));
    assert!(output.contains("\"dt\":[5,7]"));

    let shorter = concat!(
        "{\"type\":\"session\",\"id\":\"same\",\"createdAt\":100}\n",
        "{\"type\":\"assistant/chunk\",\"time\":100}\n",
        "{\"type\":\"assistant/chunk\",\"time\":101}\n"
    );
    let output = stabilize(fresh, shorter);
    assert!(output.contains("\"time0\":100"));
    assert!(output.contains("\"dt\":[5,7]"));
}

#[test]
fn scenario_table_validation_rejects_missing_duplicate_and_redirected_pins() {
    let root = fixture_root().join("suite");
    let missing = options(
        root.clone(),
        vec![scenario("plain", true, true)],
        SnapshotSuiteMode::Replay,
    );
    assert!(
        define_acp_snapshot_suite(missing)
            .unwrap_err()
            .to_string()
            .contains("no scenario pins")
    );

    let mut first = scenario("pin", true, true);
    first.pins_header = true;
    let mut second = scenario("pin-2", true, true);
    second.pins_header = true;
    assert!(
        define_acp_snapshot_suite(options(
            root.clone(),
            vec![first.clone(), second],
            SnapshotSuiteMode::Replay
        ))
        .unwrap_err()
        .to_string()
        .contains("pinned by both")
    );

    let mut duplicate = first.clone();
    duplicate.pins_header = false;
    assert!(
        define_acp_snapshot_suite(options(
            root.clone(),
            vec![first.clone(), duplicate],
            SnapshotSuiteMode::Replay
        ))
        .unwrap_err()
        .to_string()
        .contains("duplicate scenario")
    );

    let mut non_pin_source = scenario("member", true, true);
    non_pin_source.system_prompt_source = Some("pin".into());
    assert!(
        define_acp_snapshot_suite(options(
            root,
            vec![first, non_pin_source],
            SnapshotSuiteMode::Replay
        ))
        .unwrap_err()
        .to_string()
        .contains("only valid on a header-pinning scenario")
    );
}

#[test]
fn committed_replay_fixture_tree_passes_every_static_guard() {
    define_acp_snapshot_suite(options(
        fixture_root().join("suite"),
        replay_scenarios(),
        SnapshotSuiteMode::Replay,
    ))
    .unwrap()
    .validate_fixtures()
    .unwrap();
}

#[tokio::test]
async fn replay_suite_runs_real_fake_subprocesses_and_all_live_comparisons() {
    let suite = define_acp_snapshot_suite(options(
        fixture_root().join("suite"),
        replay_scenarios(),
        SnapshotSuiteMode::Replay,
    ))
    .unwrap();
    let report = suite.run().await.unwrap();
    assert_eq!(report.scenarios.len(), 6);
    assert!(report.scenarios.iter().all(|scenario| !scenario.skipped));
}

#[tokio::test]
async fn record_suite_creates_primary_prunes_stale_children_and_skips_authored() {
    let temp = tempfile::tempdir().unwrap();
    copy_tree(&fixture_root().join("record-suite"), temp.path());
    fs::remove_file(temp.path().join("rec-pin/session.jsonl")).unwrap();
    fs::write(
        temp.path().join("rec-child/session.2.jsonl"),
        "stale child\n",
    )
    .unwrap();
    let suite = define_acp_snapshot_suite(options(
        temp.path().to_owned(),
        record_scenarios(),
        SnapshotSuiteMode::Record,
    ))
    .unwrap();
    let report = suite.run().await.unwrap();
    assert!(temp.path().join("rec-pin/session.jsonl").is_file());
    assert!(!temp.path().join("rec-child/session.2.jsonl").exists());
    for file in ["session.jsonl", "session.1.jsonl"] {
        let fixture = fs::read_to_string(temp.path().join("rec-child").join(file)).unwrap();
        assert!(fixture.contains("22222222-2222-4222-8222-222222222222"));
        assert!(!fixture.contains("11111111-1111-4111-8111-111111111111"));
    }
    assert!(
        report
            .scenarios
            .iter()
            .find(|scenario| scenario.name == "rec-skip")
            .unwrap()
            .skipped
    );
}

#[tokio::test]
async fn refresh_suite_rewrites_stale_outputs_and_revalidates_the_tree() {
    let temp = tempfile::tempdir().unwrap();
    copy_tree(&fixture_root().join("suite"), temp.path());
    fs::write(
        temp.path().join("plain-turn/stdout.expected.jsonl"),
        "stale stdout\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("pin-turn/system-prompt.expected.md"),
        "STALE PROMPT\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("pin-turn/tool-schemas.expected.json"),
        "{\"initial\":[{\"name\":\"stale\"}],\"changes\":[]}\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("plain-turn/tool-schemas.1.expected.json"),
        "{\"initial\":[{\"name\":\"stale-child\"}],\"changes\":[]}\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("plain-turn/system-prompt.1.expected.md"),
        "STALE CHILD PROMPT\n",
    )
    .unwrap();
    let behavior_path = temp.path().join("plain-turn/behavior.json");
    let mut behavior: Value = serde_json::from_slice(&fs::read(&behavior_path).unwrap()).unwrap();
    behavior
        .as_object_mut()
        .unwrap()
        .insert("echoEnv".to_owned(), Value::Bool(true));
    fs::write(
        &behavior_path,
        format!("{}\n", serde_json::to_string_pretty(&behavior).unwrap()),
    )
    .unwrap();
    fs::write(
        temp.path().join("blocked-log/session.jsonl"),
        concat!(
            "{\"type\":\"session\",\"id\":\"99999999-8888-4777-8666-555555555555\",\"createdAt\":13,\"cwd\":\"/rec/blocked-cwd\",\"delegationDepth\":0}\n",
            "{\"type\":\"hook/result\",\"seq\":1,\"time\":13,\"data\":{\"decision\":\"stale\",\"durationMs\":99}}\n"
        ),
    )
    .unwrap();
    fs::write(
        temp.path().join("authored-error/session.jsonl"),
        concat!(
            "{\"type\":\"session\",\"id\":\"77777777-8888-4777-8666-555555555555\",\"createdAt\":13,\"cwd\":\"/rec/error-cwd\",\"delegationDepth\":0}\n",
            "{\"type\":\"turn/end\",\"seq\":1,\"time\":9,\"data\":{\"error\":\"stale\"}}\n"
        ),
    )
    .unwrap();
    let suite = define_acp_snapshot_suite(options(
        temp.path().to_owned(),
        replay_scenarios(),
        SnapshotSuiteMode::Refresh,
    ))
    .unwrap();
    suite.run().await.unwrap();
    assert!(
        !fs::read_to_string(temp.path().join("plain-turn/stdout.expected.jsonl"))
            .unwrap()
            .contains("stale stdout")
    );
    let stdout = fs::read_to_string(temp.path().join("plain-turn/stdout.expected.jsonl")).unwrap();
    assert!(stdout.contains("env:{\\\"mode\\\":\\\"replay\\\""));
    assert!(stdout.contains("\\\"permissionMode\\\":\\\"never\\\""));
    assert!(
        fs::read_to_string(temp.path().join("pin-turn/system-prompt.expected.md"))
            .unwrap()
            .starts_with("SYS PROMPT")
    );
    assert!(
        !fs::read_to_string(temp.path().join("pin-turn/tool-schemas.expected.json"))
            .unwrap()
            .contains("stale")
    );
    let child_schema =
        fs::read_to_string(temp.path().join("plain-turn/tool-schemas.1.expected.json")).unwrap();
    assert!(child_schema.contains("child-only"));
    assert!(!child_schema.contains("stale-child"));
    assert_eq!(
        fs::read_to_string(temp.path().join("plain-turn/system-prompt.1.expected.md")).unwrap(),
        "SYS PROMPT\n\nCHILD GUIDANCE\n"
    );
    let blocked = fs::read_to_string(temp.path().join("blocked-log/session.jsonl")).unwrap();
    assert!(blocked.contains("\"decision\":\"block\""));
    assert!(!blocked.contains("stale"));
    let authored = fs::read_to_string(temp.path().join("authored-error/session.jsonl")).unwrap();
    assert!(authored.contains("model exploded"));
    assert!(!authored.contains("stale"));
}
