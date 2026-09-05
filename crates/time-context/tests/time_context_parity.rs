//! Production and browser-zone parity for durable time context.

use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};

use seekdeep_agent::{
    Agent, AgentEvents, AgentOptions, Inbox, NoopInboxNotifications, PreStepDecision,
};
use seekdeep_agent_loop::AgentPreStepEvent;
use seekdeep_cordis::{Context, Fiber};
use seekdeep_core::session::{AppendOptions, Session, SessionEvent, SessionId, SurfaceOp};
use seekdeep_llm::{AbortSignal, ContentBlock, MessageSource, UserMessage};
use seekdeep_scope::ScopeKey;
use seekdeep_time_context::{
    TimeContextConfig, apply_with_clock,
    request_zone::{
        BrowserTimeZoneContext, derive_browser_time_zone_context, render_browser_time_zone_context,
    },
    timestamp::create_timestamp_formatter,
    user_rpc_message,
};
use serde_json::{Value, json};

const BASE: i64 = 1_783_987_200_000;

fn user(text: &str) -> UserMessage {
    UserMessage::new(
        vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        MessageSource::user(),
    )
}

fn plugin_message(text: &str) -> UserMessage {
    UserMessage::new(
        vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        MessageSource::plugin("time-context-test"),
    )
}

fn event(event_type: &str, seq: u64, time: i64, data: Value, surface: bool) -> SessionEvent {
    SessionEvent {
        event_type: event_type.to_owned(),
        seq,
        time,
        data,
        source_event_seqs: None,
        surface_op: surface.then(SurfaceOp::append),
        ignorable: None,
    }
}

fn seeded_turn(id: &str, browser_zone: Option<&str>) -> Arc<Session> {
    let message = browser_zone.map_or_else(
        || user("turn 1"),
        |zone| user_rpc_message("turn 1", "turn-1", zone),
    );
    Session::create(
        &SessionId::new(id),
        Some(vec![
            event("turn/start", 0, BASE, json!({"turn": 1}), false),
            event(
                "user/message",
                1,
                BASE,
                serde_json::to_value(message).expect("message"),
                true,
            ),
        ]),
        None,
    )
    .expect("seeded session")
}

fn bare_session(id: &str) -> Arc<Session> {
    Session::create(
        &SessionId::new(id),
        Some(vec![event(
            "turn/start",
            0,
            BASE,
            json!({"turn": 1}),
            false,
        )]),
        None,
    )
    .expect("bare session")
}

fn agent(context: &Context, session: Arc<Session>, id: &str) -> Arc<Agent> {
    let inbox =
        Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).expect("inbox"));
    Arc::new(Agent::new(
        SessionId::new(id),
        AgentOptions::default(),
        session,
        inbox,
        context.clone(),
        ScopeKey::new(),
    ))
}

async fn fire(
    context: &Context,
    agent: &Arc<Agent>,
    turn: u64,
    step: u64,
    signal: AbortSignal,
) -> anyhow::Result<()> {
    let proposed = plugin_message("request proposal");
    let proposed_id = proposed.id().clone();
    let inner = proposed.clone();
    let decision = AgentEvents::new(context.clone(), agent.clone())
        .waterfall(
            "agent/pre-step",
            AgentPreStepEvent {
                messages: vec![proposed],
                turn,
                step,
                signal,
            },
            move || async move {
                Ok(PreStepDecision::Enter {
                    messages: vec![inner],
                })
            },
        )
        .await?;
    if let PreStepDecision::Enter { messages } = decision {
        for message in messages {
            if message.id() == &proposed_id {
                continue;
            }
            agent.session().append(
                "user/message",
                serde_json::to_value(message)?,
                AppendOptions {
                    surface_op: Some(SurfaceOp::append()),
                    ..AppendOptions::default()
                },
            )?;
        }
    }
    Ok(())
}

fn context_texts(session: &Session) -> Vec<String> {
    session
        .events()
        .into_iter()
        .filter(|event| {
            event.event_type == "user/message"
                && event.data["source"]["kind"] == "plugin"
                && event.data["source"]["plugin"] == "time-context"
        })
        .filter_map(|event| event.data["content"][0]["text"].as_str().map(str::to_owned))
        .collect()
}

fn is_time_context(event: &SessionEvent) -> bool {
    event.event_type == "user/message"
        && event.data["source"]["kind"] == "plugin"
        && event.data["source"]["plugin"] == "time-context"
}

fn install(
    context: &Context,
    now: Arc<AtomicI64>,
    config: &TimeContextConfig,
) -> anyhow::Result<()> {
    apply_with_clock(
        context,
        config,
        Arc::new(move || now.load(Ordering::Acquire)),
    )
}

#[test]
fn derives_missing_unique_and_sorted_mixed_browser_zones() {
    let plugin = plugin_message("plugin");
    assert_eq!(
        derive_browser_time_zone_context(&[plugin]).expect("missing"),
        BrowserTimeZoneContext::Missing
    );
    assert_eq!(
        derive_browser_time_zone_context(&[
            user_rpc_message("one", "one", "Asia/Shanghai"),
            user_rpc_message("two", "two", "Asia/Shanghai"),
        ])
        .expect("unique"),
        BrowserTimeZoneContext::Resolved {
            time_zone: "Asia/Shanghai".to_owned()
        }
    );
    assert_eq!(
        derive_browser_time_zone_context(&[
            user_rpc_message("one", "one", "Asia/Shanghai"),
            user_rpc_message("two", "two", "America/New_York"),
        ])
        .expect("mixed"),
        BrowserTimeZoneContext::Mixed {
            time_zones: vec!["America/New_York".to_owned(), "Asia/Shanghai".to_owned()]
        }
    );
}

#[test]
fn validates_every_browser_zone_before_mixed_classification() {
    let invalid = derive_browser_time_zone_context(&[user_rpc_message("x", "x", "+08:00")])
        .expect_err("shape");
    assert!(format!("{invalid:#}").contains("canonical UTC or IANA Area/Location"));
    let unsupported = derive_browser_time_zone_context(&[
        user_rpc_message("one", "one", "Asia/Shanghai"),
        user_rpc_message("two", "two", "Not/A_Real_Zone"),
    ])
    .expect_err("unsupported");
    assert!(format!("{unsupported:#}").contains("browser time zone is unsupported"));
    let alias = derive_browser_time_zone_context(&[user_rpc_message("x", "x", "Etc/UTC")])
        .expect_err("noncanonical alias");
    assert!(format!("{alias:#}").contains("browser time zone must be canonical"));
}

#[test]
fn timestamp_zone_resolution_matches_intl_canonical_identifiers() {
    let cases = [
        ("Africa/Asmara", "Africa/Asmera"),
        ("America/Argentina/Buenos_Aires", "America/Buenos_Aires"),
        ("America/Argentina/Catamarca", "America/Catamarca"),
        ("America/Argentina/Cordoba", "America/Cordoba"),
        ("America/Argentina/Jujuy", "America/Jujuy"),
        ("America/Argentina/Mendoza", "America/Mendoza"),
        ("America/Atikokan", "America/Coral_Harbour"),
        ("America/Indiana/Indianapolis", "America/Indianapolis"),
        ("America/Kentucky/Louisville", "America/Louisville"),
        ("America/Nuuk", "America/Godthab"),
        ("Asia/Ho_Chi_Minh", "Asia/Saigon"),
        ("Asia/Kathmandu", "Asia/Katmandu"),
        ("Asia/Kolkata", "Asia/Calcutta"),
        ("Asia/Yangon", "Asia/Rangoon"),
        ("Atlantic/Faroe", "Atlantic/Faeroe"),
        ("Etc/GMT", "UTC"),
        ("Etc/UTC", "UTC"),
        ("Europe/Kyiv", "Europe/Kiev"),
        ("MET", "Europe/Brussels"),
        ("Pacific/Chuuk", "Pacific/Truk"),
        ("Pacific/Kanton", "Pacific/Enderbury"),
        ("Pacific/Pohnpei", "Pacific/Ponape"),
    ];
    for (input, expected) in cases {
        assert_eq!(
            create_timestamp_formatter(Some(input))
                .expect("Intl-supported alias")
                .resolved_time_zone(),
            expected,
            "{input}"
        );
    }
    assert!(create_timestamp_formatter(Some("America/Coyhaique")).is_err());
}

#[test]
fn process_zone_fallback_honors_the_child_process_environment() {
    const CHILD: &str = "SEEKDEEP_TIME_CONTEXT_ZONE_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let formatter = create_timestamp_formatter(None).expect("process zone");
        assert_eq!(formatter.resolved_time_zone(), "Asia/Shanghai");
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "process_zone_fallback_honors_the_child_process_environment",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env("TZ", "Asia/Shanghai")
        .output()
        .expect("spawn isolated test child");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn renders_explicit_policy_for_every_browser_context() {
    assert!(
        render_browser_time_zone_context(&BrowserTimeZoneContext::Resolved {
            time_zone: "Asia/Shanghai".to_owned()
        })
        .contains("Interpret otherwise-unqualified")
    );
    assert!(
        render_browser_time_zone_context(&BrowserTimeZoneContext::Mixed {
            time_zones: vec!["America/New_York".to_owned(), "Asia/Shanghai".to_owned()]
        })
        .contains(r#"mixed ["America/New_York","Asia/Shanghai"]"#)
    );
    assert!(
        render_browser_time_zone_context(&BrowserTimeZoneContext::Missing).contains("unavailable")
    );
}

#[tokio::test]
async fn records_turn_step_zone_browser_policy_and_elapsed_baseline() {
    let context = Context::new();
    let now = Arc::new(AtomicI64::new(BASE + 90_061_000));
    install(
        &context,
        now,
        &TimeContextConfig {
            time_zone: Some("Asia/Shanghai".to_owned()),
            refresh_interval_ms: None,
        },
    )
    .expect("install");
    let session = seeded_turn("first", Some("Asia/Shanghai"));
    let subject = agent(&context, session.clone(), "first");
    fire(&context, &subject, 1, 1, AbortSignal::default())
        .await
        .expect("fire");
    let expected = "Time sampled while preparing turn 1, step 1: 2026-07-15T09:01:01+08:00[Asia/Shanghai]\nBrowser time zone for this request: Asia/Shanghai. Interpret otherwise-unqualified dates and times in this zone.\nElapsed since the preceding model-visible message: 1d 1h 1m 1s.";
    assert_eq!(context_texts(&session), [expected]);
    let event = session.events().pop().expect("reading");
    assert_eq!(event.surface_op, Some(SurfaceOp::append()));
    assert_eq!(event.data["source"]["form"], "snapshot");
    assert_eq!(event.data["source"]["sections"][0]["name"], "time-context");
    assert_eq!(event.data["source"]["sections"][0]["text"], expected);
}

#[tokio::test]
async fn reports_unavailable_first_and_later_step_baselines() {
    let context = Context::new();
    let now = Arc::new(AtomicI64::new(BASE));
    install(
        &context,
        now,
        &TimeContextConfig {
            time_zone: Some("UTC".to_owned()),
            ..TimeContextConfig::default()
        },
    )
    .expect("install");
    let session = bare_session("unavailable");
    let subject = agent(&context, session.clone(), "unavailable");
    fire(&context, &subject, 1, 1, AbortSignal::default())
        .await
        .expect("first");
    assert!(context_texts(&session)[0].contains("model-visible message: unavailable"));

    let boundary = seeded_turn("later", None);
    let later = agent(&context, boundary.clone(), "later");
    fire(&context, &later, 1, 2, AbortSignal::default())
        .await
        .expect("later");
    assert!(context_texts(&boundary)[0].contains("step context: unavailable"));
}

#[tokio::test]
async fn later_step_uses_preceding_durable_context_timestamp() {
    for (label, interval) in [("omitted", None), ("zero", Some(0.0))] {
        let context = Context::new();
        let now = Arc::new(AtomicI64::new(BASE));
        install(
            &context,
            now.clone(),
            &TimeContextConfig {
                time_zone: Some("UTC".to_owned()),
                refresh_interval_ms: interval,
            },
        )
        .expect("install");
        let session = seeded_turn(&format!("later-step-{label}"), None);
        let subject = agent(&context, session.clone(), &format!("later-step-{label}"));
        fire(&context, &subject, 1, 1, AbortSignal::default())
            .await
            .expect("first");
        let prior = session.events().last().expect("reading").time;
        now.store(prior + 61_000, Ordering::Release);
        fire(&context, &subject, 1, 2, AbortSignal::default())
            .await
            .expect("second");
        assert!(
            context_texts(&session)[1].contains("preceding step context: 1m 1s"),
            "{label} interval"
        );
    }
}

#[tokio::test]
async fn unique_browser_zone_formats_timestamp_while_mixed_uses_fallback() {
    let context = Context::new();
    let now = Arc::new(AtomicI64::new(BASE));
    install(
        &context,
        now,
        &TimeContextConfig {
            time_zone: Some("UTC".to_owned()),
            ..TimeContextConfig::default()
        },
    )
    .expect("install");
    let resolved = seeded_turn("resolved", Some("America/New_York"));
    fire(
        &context,
        &agent(&context, resolved.clone(), "resolved"),
        1,
        1,
        AbortSignal::default(),
    )
    .await
    .expect("resolved");
    assert!(context_texts(&resolved)[0].contains(
        "2026-07-13T20:00:00-04:00[America/New_York]\nBrowser time zone for this request: America/New_York"
    ));

    let mixed = seeded_turn("mixed", Some("Asia/Shanghai"));
    mixed
        .append(
            "user/message",
            serde_json::to_value(user_rpc_message(
                "other browser",
                "mixed-steer",
                "America/New_York",
            ))
            .expect("message"),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .expect("steering");
    fire(
        &context,
        &agent(&context, mixed.clone(), "mixed"),
        1,
        1,
        AbortSignal::default(),
    )
    .await
    .expect("mixed");
    assert!(context_texts(&mixed)[0].contains(
        "2026-07-14T00:00:00+00:00[UTC]\nBrowser time zone for this request: mixed [\"America/New_York\",\"Asia/Shanghai\"]"
    ));
}

#[tokio::test]
async fn refresh_interval_skips_forward_time_but_injects_after_backward_movement() {
    let context = Context::new();
    let now = Arc::new(AtomicI64::new(BASE));
    install(
        &context,
        now.clone(),
        &TimeContextConfig {
            time_zone: Some("UTC".to_owned()),
            refresh_interval_ms: Some(60_000.0),
        },
    )
    .expect("install");
    let session = seeded_turn("refresh", None);
    let subject = agent(&context, session.clone(), "refresh");
    fire(&context, &subject, 1, 1, AbortSignal::default())
        .await
        .expect("first");
    let last = session.events().last().expect("reading").time;
    now.store(last + 59_999, Ordering::Release);
    fire(&context, &subject, 1, 2, AbortSignal::default())
        .await
        .expect("skip");
    assert_eq!(context_texts(&session).len(), 1);
    now.store(last - 5_000, Ordering::Release);
    fire(&context, &subject, 1, 2, AbortSignal::default())
        .await
        .expect("backward");
    assert_eq!(context_texts(&session).len(), 2);
    assert!(context_texts(&session)[1].contains("preceding step context: 0s"));
}

#[tokio::test]
async fn refresh_state_is_durable_per_session_and_exact_threshold_is_eligible() {
    let context = Context::new();
    let now = Arc::new(AtomicI64::new(BASE));
    install(
        &context,
        now.clone(),
        &TimeContextConfig {
            time_zone: Some("UTC".to_owned()),
            refresh_interval_ms: Some(1_000.0),
        },
    )
    .expect("install");
    let first = seeded_turn("first-interval", None);
    let first_agent = agent(&context, first.clone(), "first-interval");
    fire(&context, &first_agent, 1, 1, AbortSignal::default())
        .await
        .expect("first");
    let last = first.events().last().expect("reading").time;
    now.store(last + 999, Ordering::Release);
    fire(&context, &first_agent, 2, 1, AbortSignal::default())
        .await
        .expect("skip");
    assert_eq!(context_texts(&first).len(), 1);

    let independent = seeded_turn("independent", None);
    fire(
        &context,
        &agent(&context, independent.clone(), "independent"),
        1,
        1,
        AbortSignal::default(),
    )
    .await
    .expect("independent");
    assert_eq!(context_texts(&independent).len(), 1);

    now.store(last + 1_000, Ordering::Release);
    fire(&context, &first_agent, 2, 2, AbortSignal::default())
        .await
        .expect("threshold");
    assert_eq!(context_texts(&first).len(), 2);
}

#[tokio::test]
async fn shadowed_reading_survives_resume_for_refresh_but_not_step_baseline() {
    let context = Context::new();
    let now = Arc::new(AtomicI64::new(BASE));
    install(
        &context,
        now.clone(),
        &TimeContextConfig {
            time_zone: Some("UTC".to_owned()),
            refresh_interval_ms: Some(1_000.0),
        },
    )
    .expect("install");
    let original = seeded_turn("seed-source", None);
    let original_agent = agent(&context, original.clone(), "seed-source");
    fire(&context, &original_agent, 1, 1, AbortSignal::default())
        .await
        .expect("first");
    let events = original.events();
    let user_seq = events
        .iter()
        .find(|event| event.event_type == "user/message" && event.data["source"]["kind"] == "user")
        .expect("user")
        .seq;
    let reading_seq = events
        .iter()
        .find(|event| is_time_context(event))
        .expect("reading")
        .seq;
    original
        .append(
            "user/message",
            serde_json::to_value(UserMessage::new(
                vec![ContentBlock::Text {
                    text: "compacted history".to_owned(),
                }],
                MessageSource::plugin("compaction-basic"),
            ))
            .expect("compaction"),
            AppendOptions {
                surface_op: Some(SurfaceOp::replace(user_seq, reading_seq)),
                source_event_seqs: Some(vec![user_seq, reading_seq]),
                ..AppendOptions::default()
            },
        )
        .expect("replace surface");
    original
        .append(
            "turn/end",
            json!({"turn": 1, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .expect("end turn");
    assert!(original.derive_messages().iter().all(|message| {
        !serde_json::to_string(message)
            .expect("message")
            .contains("Time sampled while preparing")
    }));

    let resumed =
        Session::create(&SessionId::new("resumed"), Some(original.events()), None).expect("resume");
    let resumed_agent = agent(&context, resumed.clone(), "resumed");
    let last = resumed
        .events()
        .iter()
        .find(|event| is_time_context(event))
        .expect("durable reading")
        .time;
    now.store(last + 999, Ordering::Release);
    resumed
        .append("turn/start", json!({"turn": 2}), AppendOptions::default())
        .expect("second turn");
    resumed
        .append(
            "user/message",
            serde_json::to_value(user("turn 2")).expect("turn message"),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .expect("turn message");
    let before = resumed.events().len();
    fire(&context, &resumed_agent, 2, 1, AbortSignal::default())
        .await
        .expect("skip before threshold");
    assert_eq!(resumed.events().len(), before);
    assert_eq!(context_texts(&resumed).len(), 1);

    now.store(last + 1_000, Ordering::Release);
    fire(&context, &resumed_agent, 2, 2, AbortSignal::default())
        .await
        .expect("inject at threshold");
    assert_eq!(context_texts(&resumed).len(), 2);
    assert!(context_texts(&resumed)[1].contains("preceding step context: unavailable"));
}

#[tokio::test]
async fn aborted_or_downstream_rejected_preparation_does_not_add_reading() {
    let context = Context::new();
    let now = Arc::new(AtomicI64::new(BASE));
    install(
        &context,
        now,
        &TimeContextConfig {
            time_zone: Some("UTC".to_owned()),
            ..TimeContextConfig::default()
        },
    )
    .expect("install");
    let session = seeded_turn("abort", None);
    let subject = agent(&context, session.clone(), "abort");
    let aborted = AbortSignal::default();
    aborted.abort();
    fire(&context, &subject, 1, 1, aborted)
        .await
        .expect("aborted");
    assert!(context_texts(&session).is_empty());

    context
        .events()
        .on_waterfall(
            &context,
            "agent/pre-step",
            |_, _, _| {
                Box::pin(async {
                    Ok(seekdeep_cordis::EventReply::Value(Arc::new(
                        PreStepDecision::Reject,
                    )))
                })
            },
            seekdeep_cordis::EventOptions::default(),
        )
        .expect("reject");
    fire(&context, &subject, 1, 2, AbortSignal::default())
        .await
        .expect("rejected");
    assert!(context_texts(&session).is_empty());
}

#[tokio::test]
async fn downstream_failure_or_cancellation_cannot_commit_a_reading() {
    let throwing = Context::new();
    install(
        &throwing,
        Arc::new(AtomicI64::new(BASE)),
        &TimeContextConfig {
            time_zone: Some("UTC".to_owned()),
            ..TimeContextConfig::default()
        },
    )
    .expect("install");
    throwing
        .events()
        .on_waterfall(
            &throwing,
            "agent/pre-step",
            |_, _, _| Box::pin(async { Err(anyhow::anyhow!("later pre-step failure")) }),
            seekdeep_cordis::EventOptions::default(),
        )
        .expect("throwing listener");
    let throw_session = seeded_turn("downstream-throw", None);
    let error = fire(
        &throwing,
        &agent(&throwing, throw_session.clone(), "downstream-throw"),
        1,
        1,
        AbortSignal::default(),
    )
    .await
    .expect_err("downstream failure");
    assert!(format!("{error:#}").contains("later pre-step failure"));
    assert!(context_texts(&throw_session).is_empty());

    let cancelling = Context::new();
    install(
        &cancelling,
        Arc::new(AtomicI64::new(BASE)),
        &TimeContextConfig {
            time_zone: Some("UTC".to_owned()),
            ..TimeContextConfig::default()
        },
    )
    .expect("install");
    let signal = AbortSignal::default();
    let downstream_signal = signal.clone();
    cancelling
        .events()
        .on_waterfall(
            &cancelling,
            "agent/pre-step",
            move |_, _, next| {
                let signal = downstream_signal.clone();
                Box::pin(async move {
                    signal.abort();
                    next.run().await
                })
            },
            seekdeep_cordis::EventOptions::default(),
        )
        .expect("cancelling listener");
    let cancel_session = seeded_turn("downstream-cancel", None);
    fire(
        &cancelling,
        &agent(&cancelling, cancel_session.clone(), "downstream-cancel"),
        1,
        1,
        signal,
    )
    .await
    .expect("cancelled decision");
    assert!(context_texts(&cancel_session).is_empty());
}

#[tokio::test]
async fn listener_is_removed_with_owning_fiber() {
    let root = Context::new();
    let owner = Fiber::active_child("time-context-owner");
    let child = root.with_fiber(owner.clone());
    let now = Arc::new(AtomicI64::new(BASE));
    install(
        &child,
        now,
        &TimeContextConfig {
            time_zone: Some("UTC".to_owned()),
            ..TimeContextConfig::default()
        },
    )
    .expect("install");
    let session = seeded_turn("dispose", None);
    let subject = agent(&root, session.clone(), "dispose");
    fire(&root, &subject, 1, 1, AbortSignal::default())
        .await
        .expect("before");
    owner.dispose().await.expect("dispose");
    fire(&root, &subject, 1, 2, AbortSignal::default())
        .await
        .expect("after");
    assert_eq!(context_texts(&session).len(), 1);
}

#[test]
fn invalid_zone_and_refresh_configuration_fail_loud() {
    let context = Context::new();
    let now = Arc::new(AtomicI64::new(BASE));
    let zone = install(
        &context,
        now.clone(),
        &TimeContextConfig {
            time_zone: Some("Not/A_Real_Zone".to_owned()),
            ..TimeContextConfig::default()
        },
    )
    .expect_err("zone");
    assert!(format!("{zone:#}").contains("invalid IANA timeZone"));
    for interval in [-1.0, 0.5, 9_007_199_254_740_992.0, f64::INFINITY, f64::NAN] {
        let error = install(
            &context,
            now.clone(),
            &TimeContextConfig {
                time_zone: Some("UTC".to_owned()),
                refresh_interval_ms: Some(interval),
            },
        )
        .expect_err("interval");
        assert!(format!("{error:#}").contains("non-negative safe integer"));
    }
}
