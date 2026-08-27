//! Produced-file prompt, Turn projection, mention, locale, and fit parity.

use std::rc::Rc;

use seekdeep_client_runtime::{
    AssemblerEventDefinitions, AssemblerNodeDefinition, AssemblerViewBuilder,
    AssemblerViewDefinition, AssemblerViewDefinitions, ConversationAssemblerError,
    ConversationEventInput, ConversationLocationEvent, ConversationNodeAssembler,
    ConversationTimelineSnapshot, ConversationViewNode,
};
use seekdeep_client_ui_deliverables::{
    DELIVERABLES_EN, DELIVERABLES_NS, DELIVERABLES_ZH, DeliverablesTurnData, SHOWN_LIMIT, basename,
    deliverables_definition, fit_produced_files, produced_file_mention, produced_for_closing,
    select_produced_files,
};
#[cfg(not(target_arch = "wasm32"))]
use seekdeep_client_ui_deliverables::{
    FILE_REFERENCE_PROMPT, INJECT, PROMPT_SECTION_NAME, PROMPT_SECTION_ORDER, host_plugin,
};
use serde_json::{Map, Value, json};

struct EventDefinitions(Vec<Rc<AssemblerNodeDefinition>>);

impl AssemblerEventDefinitions for EventDefinitions {
    fn entries(&self) -> Vec<Rc<AssemblerNodeDefinition>> {
        self.0.clone()
    }

    fn fallback_entry(&self) -> Option<Rc<AssemblerNodeDefinition>> {
        None
    }
}

struct ViewDefinitions(Vec<Rc<AssemblerViewDefinition>>);

impl AssemblerViewDefinitions for ViewDefinitions {
    fn entries(&self) -> Vec<Rc<AssemblerViewDefinition>> {
        self.0.clone()
    }
}

struct TimelineBuilder {
    snapshot: Rc<Value>,
}

impl TimelineBuilder {
    fn empty_snapshot() -> Rc<Value> {
        Rc::new(json!({"turns":{}}))
    }

    fn publish(&mut self, timeline: &ConversationTimelineSnapshot) -> Rc<Value> {
        let turns = timeline
            .turn_order
            .iter()
            .filter_map(|turn| {
                timeline.turns.get(turn).and_then(|location| {
                    location
                        .data
                        .get("deliverables")
                        .map(|data| (turn.to_string(), data.as_ref().clone()))
                })
            })
            .collect::<Map<_, _>>();
        self.snapshot = Rc::new(json!({"turns":turns}));
        self.snapshot.clone()
    }
}

impl AssemblerViewBuilder for TimelineBuilder {
    fn empty(&self) -> Rc<Value> {
        self.snapshot.clone()
    }

    fn replace(
        &mut self,
        _nodes: &[Rc<ConversationViewNode>],
        timeline: Rc<ConversationTimelineSnapshot>,
    ) -> Result<Rc<Value>, ConversationAssemblerError> {
        Ok(self.publish(&timeline))
    }

    fn apply(
        &mut self,
        _upserts: &[Rc<ConversationViewNode>],
        timeline: Rc<ConversationTimelineSnapshot>,
    ) -> Result<Rc<Value>, ConversationAssemblerError> {
        Ok(self.publish(&timeline))
    }
}

fn assembler(entries: &[ConversationEventInput], has_more: bool) -> ConversationNodeAssembler {
    let mut value = ConversationNodeAssembler::new(
        Rc::new(EventDefinitions(vec![Rc::new(deliverables_definition())])),
        Rc::new(ViewDefinitions(vec![Rc::new(AssemblerViewDefinition {
            target: "test".to_owned(),
            create: Rc::new(|| {
                Box::new(TimelineBuilder {
                    snapshot: TimelineBuilder::empty_snapshot(),
                })
            }),
        })])),
    );
    value.replace_window(entries, has_more).unwrap();
    value.flush().unwrap();
    value
}

fn at(
    seq: u64,
    event_type: &str,
    data: Value,
    view: Option<Value>,
    surface_op: Option<Value>,
) -> ConversationEventInput {
    let mut wire = json!({
        "seq":seq,
        "time":seq * 1_000,
        "type":event_type,
        "data":data,
    });
    if let Some(surface_op) = surface_op {
        wire["surfaceOp"] = surface_op;
    }
    ConversationEventInput {
        event: ConversationLocationEvent::with_wire(
            seq,
            i64::try_from(seq * 1_000).unwrap(),
            event_type,
            data,
            wire,
        ),
        view: view.map(Rc::new),
    }
}

fn turn_start(seq: u64, turn: u64) -> ConversationEventInput {
    at(seq, "turn/start", json!({"turn":turn}), None, None)
}

fn call(seq: u64, call_id: &str, call_view: Option<Value>, turn: u64) -> ConversationEventInput {
    at(
        seq,
        "tool/call",
        json!({
            "turn":turn,
            "step":1,
            "callId":call_id,
            "name":"fixture",
            "arguments":"{}",
        }),
        call_view.map(|view| json!({"for":"call","view":view})),
        None,
    )
}

fn result(
    seq: u64,
    call_id: &str,
    is_error: bool,
    turn: u64,
    append: bool,
) -> ConversationEventInput {
    at(
        seq,
        "tool/result",
        json!({
            "turn":turn,
            "step":1,
            "message":{
                "source":{"type":"tool-result","callId":call_id},
                "content":[{"type":"tool-result","content":[],"isError":is_error}],
            },
        }),
        None,
        Some(if append {
            json!("append")
        } else {
            json!({"op":"replace","start":1,"end":1})
        }),
    )
}

fn diff(paths: &[&str]) -> Value {
    json!({
        "card":"diff",
        "title":format!("Write {}", paths.first().copied().unwrap_or_default()),
        "diffs":paths.iter().map(|path| json!({"path":path,"oldText":null,"newText":"x"})).collect::<Vec<_>>(),
        "locations":paths.iter().map(|path| json!({"path":path})).collect::<Vec<_>>(),
    })
}

fn edit(path: &str) -> Value {
    json!({
        "card":"generic",
        "title":format!("insert {path}"),
        "kind":"edit",
        "locations":[{"path":path}],
    })
}

fn deliverables_of(value: &ConversationNodeAssembler, turn: u64) -> Option<DeliverablesTurnData> {
    let snapshot = value.snapshot("test")?;
    serde_json::from_value(snapshot.pointer(&format!("/turns/{turn}"))?.clone()).ok()
}

#[test]
fn turn_fold_accepts_successful_mutations_and_ignores_non_outputs() {
    let value = assembler(
        &[
            turn_start(1, 1),
            call(
                2,
                "write",
                Some(diff(&["out/index.html", "out/app.css"])),
                1,
            ),
            result(3, "write", false, 1, true),
            call(4, "edit", Some(edit("notes.md")), 1),
            result(5, "edit", false, 1, true),
            call(
                6,
                "read",
                Some(json!({
                    "card":"generic","title":"Read","locations":[{"path":"input.txt"}],
                })),
                1,
            ),
            result(7, "read", false, 1, true),
            call(8, "failed", Some(diff(&["broken.txt"])), 1),
            result(9, "failed", true, 1, true),
            call(
                10,
                "locationless",
                Some(json!({"card":"diff","title":"Write","diffs":[]})),
                1,
            ),
            result(11, "locationless", false, 1, true),
        ],
        false,
    );
    assert_eq!(
        produced_for_closing(deliverables_of(&value, 1).as_ref(), None),
        ["out/index.html", "out/app.css", "notes.md"]
    );
}

#[test]
fn fold_ignores_missing_views_orphans_and_replacement_results() {
    let value = assembler(
        &[
            turn_start(1, 1),
            call(2, "no-view", None, 1),
            result(3, "no-view", false, 1, true),
            call(
                4,
                "locationless-edit",
                Some(json!({"card":"generic","title":"Edit","kind":"edit"})),
                1,
            ),
            result(5, "locationless-edit", false, 1, true),
            result(6, "orphan", false, 1, true),
            call(7, "replacement", Some(diff(&["replaced.txt"])), 1),
            result(8, "replacement", false, 1, false),
            at(
                9,
                "turn/end",
                json!({"turn":1,"reason":{"kind":"completed"}}),
                None,
                None,
            ),
        ],
        false,
    );
    assert!(produced_for_closing(deliverables_of(&value, 1).as_ref(), None).is_empty());
}

#[test]
fn prepend_and_live_append_publish_the_same_turn_data() {
    let tail = [
        call(10, "late", Some(diff(&["history.txt"])), 1),
        result(11, "late", false, 1, true),
    ];
    let mut paged = assembler(&tail, true);
    assert_eq!(deliverables_of(&paged, 1), None);
    paged.prepend(&[turn_start(1, 1)], false).unwrap();
    paged.flush().unwrap();
    assert_eq!(
        produced_for_closing(deliverables_of(&paged, 1).as_ref(), None),
        ["history.txt"]
    );

    let mut live = assembler(
        &[
            turn_start(1, 1),
            call(2, "first", Some(diff(&["first.txt"])), 1),
            result(3, "first", false, 1, true),
        ],
        false,
    );
    live.append(&call(4, "second", Some(diff(&["second.txt"])), 1))
        .unwrap();
    live.append(&result(5, "second", false, 1, true)).unwrap();
    live.flush().unwrap();
    assert_eq!(
        produced_for_closing(deliverables_of(&live, 1).as_ref(), None),
        ["first.txt", "second.txt"]
    );
}

#[test]
fn closing_selection_mentions_and_cross_platform_basenames_are_exact() {
    let data = DeliverablesTurnData {
        produced: vec![
            seekdeep_client_ui_deliverables::ProducedPath {
                seq: 3,
                path: "out/index.html".to_owned(),
            },
            seekdeep_client_ui_deliverables::ProducedPath {
                seq: 4,
                path: "a/style.css".to_owned(),
            },
            seekdeep_client_ui_deliverables::ProducedPath {
                seq: 4,
                path: "out/index.html".to_owned(),
            },
            seekdeep_client_ui_deliverables::ProducedPath {
                seq: 8,
                path: "b/style.css".to_owned(),
            },
        ],
    };
    let paths = produced_for_closing(Some(&data), Some(6));
    assert_eq!(paths, ["out/index.html", "a/style.css"]);
    assert_eq!(select_produced_files(Some(&data), 6), Some(paths.clone()));
    assert_eq!(select_produced_files(None, 6), None);
    assert_eq!(basename(r"a\b\c.txt"), "c.txt");
    assert_eq!(basename("a/b/"), "");

    let all = vec![
        "out/index.html".to_owned(),
        "a/style.css".to_owned(),
        "b/style.css".to_owned(),
    ];
    let mention = produced_file_mention(&all, "index.html", |path| format!("打开 {path}")).unwrap();
    assert_eq!(mention.path, "out/index.html");
    assert_eq!(mention.title, "out/index.html");
    assert_eq!(mention.label, "打开 out/index.html");
    assert!(produced_file_mention(&all, "style.css", |_| String::new()).is_none());
    assert_eq!(
        produced_file_mention(&all, "a/style.css", |path| format!("Open {path}"))
            .unwrap()
            .path,
        "a/style.css"
    );
}

#[test]
fn chip_fit_limit_and_parallel_dictionaries_match_the_browser_policy() {
    assert_eq!(SHOWN_LIMIT, 6);
    assert_eq!(
        fit_produced_files(230.0, 8.0, &[70.0, 60.0, 60.0], &[Some(55.0); 4]),
        2
    );
    assert_eq!(
        fit_produced_files(145.0, 8.0, &[70.0, 60.0, 60.0], &[Some(55.0); 4]),
        1
    );
    assert_eq!(
        fit_produced_files(300.0, 8.0, &[70.0, 60.0, 60.0], &[Some(55.0); 4]),
        3
    );
    assert_eq!(
        fit_produced_files(0.0, 8.0, &[70.0, 60.0], &[Some(60.0), Some(50.0), None]),
        2
    );
    assert_eq!(
        fit_produced_files(128.0, 8.0, &[60.0, 60.0], &[Some(70.0), Some(50.0), None]),
        2
    );
    assert_eq!(
        fit_produced_files(126.0, 8.0, &[60.0], &[Some(70.0), Some(50.0)]),
        1
    );
    assert_eq!(
        fit_produced_files(20.0, 8.0, &[60.0], &[Some(70.0), Some(50.0)]),
        0
    );

    assert_eq!(DELIVERABLES_NS, "deliverables");
    assert_eq!(DELIVERABLES_ZH.len(), 5);
    assert!(
        DELIVERABLES_ZH
            .iter()
            .zip(DELIVERABLES_EN)
            .all(|(zh, en)| zh.0 == en.0)
    );
    assert_eq!(
        DELIVERABLES_ZH[4],
        ("produced.showInFolder", "在文件夹中显示")
    );
    assert_eq!(DELIVERABLES_EN[2], ("produced.more", "+ {count} files"));
}

#[cfg(not(target_arch = "wasm32"))]
#[tokio::test]
async fn host_prompt_section_is_exact_ordered_and_retracted_with_its_fiber() {
    assert_eq!(INJECT, ["systemPrompt"]);
    assert_eq!(PROMPT_SECTION_NAME, "ui:deliverable-file-references");
    assert_eq!(PROMPT_SECTION_ORDER.to_bits(), 190.0_f64.to_bits());
    let context = seekdeep_cordis::Context::new();
    let prompt = seekdeep_system_prompt::install(
        &context,
        seekdeep_system_prompt::SystemPromptConfig {
            persona: String::new(),
            ..seekdeep_system_prompt::SystemPromptConfig::default()
        },
    )
    .unwrap();
    let plugin = host_plugin();
    assert_eq!(plugin.inject(), ["systemPrompt"]);
    let mounted = context
        .registry()
        .mount(&context, plugin, json!({}))
        .unwrap();
    mounted.await_settled().await.unwrap();
    let section = prompt
        .assemble(seekdeep_system_prompt::AssembleContext::default())
        .await
        .unwrap()
        .sections
        .into_iter()
        .find(|section| section.name == PROMPT_SECTION_NAME)
        .unwrap();
    assert_eq!(section.text, FILE_REFERENCE_PROMPT);
    mounted.dispose().await.unwrap();
    assert!(
        prompt
            .assemble(seekdeep_system_prompt::AssembleContext::default())
            .await
            .unwrap()
            .sections
            .iter()
            .all(|section| section.name != PROMPT_SECTION_NAME)
    );
}
