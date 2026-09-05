//! Differential native rendering, browser routing, config, and lifecycle parity.

use std::sync::Arc;

use chrono::{DateTime, Local};
use parking_lot::Mutex;
use seekdeep_cordis::{Context, LogMessage, LoggerLevel, LoggerType};
use seekdeep_logger_console::{
    ColorSetting, Config, ConsoleClock, ConsoleMethod, ConsoleRecord, LabelAlign, LabelStyle,
    install_browser_with_sink, install_with_sink,
};
use serde_json::{Map, Value, json};

#[derive(Debug)]
struct FixedClock {
    now: DateTime<Local>,
    milliseconds: i64,
}

impl ConsoleClock for FixedClock {
    fn now(&self) -> DateTime<Local> {
        self.now
    }

    fn now_millis(&self) -> i64 {
        self.milliseconds
    }
}

fn clock(milliseconds: i64) -> Arc<FixedClock> {
    Arc::new(FixedClock {
        now: DateTime::parse_from_rfc3339("2024-01-02T03:04:05.006-05:00")
            .unwrap()
            .with_timezone(&Local),
        milliseconds,
    })
}

fn message(
    context: &Context,
    message_type: LoggerType,
    name: &str,
    timestamp: i64,
    args: Vec<Value>,
) -> LogMessage {
    let level = match message_type {
        LoggerType::Error => LoggerLevel::Error,
        LoggerType::Info => LoggerLevel::Info,
        LoggerType::Warn => LoggerLevel::Warn,
        LoggerType::Debug => LoggerLevel::Debug,
    };
    LogMessage {
        sn: 1,
        ts: timestamp,
        name: name.to_owned(),
        message_type,
        level,
        args,
        fiber: Arc::downgrade(context.fiber()),
        meta: Map::new(),
    }
}

fn installation(
    config: &Config,
    milliseconds: i64,
) -> seekdeep_logger_console::ConsoleInstallation {
    let context = Context::new();
    install_with_sink(&context, config, clock(milliseconds), Arc::new(|_| {})).unwrap()
}

fn plain() -> Config {
    Config {
        colors: Some(ColorSetting::Disabled(false)),
        show_time: String::new(),
        ..Config::default()
    }
}

#[test]
fn native_rendering_matches_the_pinned_source_oracle_exactly() {
    let context = Context::new();
    let basic = installation(&plain(), 0);
    assert_eq!(
        basic.render(&message(
            &context,
            LoggerType::Info,
            "alpha",
            2_000,
            vec![json!("hello %s"), json!("world")],
        )),
        "[I] alpha hello world"
    );

    let right = installation(
        &Config {
            label: Some(LabelStyle {
                width: Some(8),
                margin: Some(2),
                align: Some(LabelAlign::Right),
            }),
            ..plain()
        },
        0,
    );
    assert_eq!(
        right.render(&message(
            &context,
            LoggerType::Warn,
            "xy",
            2_000,
            vec![json!("one\ntwo")],
        )),
        "      xy  [W]  one\n               two"
    );

    let diff = installation(
        &Config {
            show_diff: true,
            ..plain()
        },
        1_000,
    );
    assert_eq!(
        diff.render(&message(
            &context,
            LoggerType::Error,
            "alpha",
            2_500,
            vec![json!("boom")],
        )),
        "[E] alpha boom +2s"
    );
}

#[test]
fn time_object_and_color_formatting_match_the_pinned_source_oracle() {
    let context = Context::new();
    let timed = installation(
        &Config {
            colors: Some(ColorSetting::Disabled(false)),
            show_time: "yyyy-MM-dd hh:mm:ss.SSS ".to_owned(),
            ..Config::default()
        },
        0,
    );
    assert_eq!(
        timed.render(&message(
            &context,
            LoggerType::Info,
            "clock",
            1,
            vec![json!("tick")],
        )),
        "2024-01-02 03:04:05.006 [I] clock tick"
    );

    let object = installation(&plain(), 0);
    assert_eq!(
        object.render(&message(
            &context,
            LoggerType::Debug,
            "alpha",
            2_000,
            vec![json!({"a":1,"b":["x",true]})],
        )),
        "[D] alpha { a: 1, b: [ 'x', true ] }"
    );
    let numeric_keys = installation(&plain(), 0);
    assert_eq!(
        numeric_keys.render(&message(
            &context,
            LoggerType::Debug,
            "alpha",
            2_000,
            vec![json!({"bad-key":1,"1":2,"ok":"a\nb"})],
        )),
        "[D] alpha { '1': 2, 'bad-key': 1, ok: 'a\\nb' }"
    );
    for (level, expected) in [
        (1, "[I] \u{1b}[32malpha\u{1b}[0m hi"),
        (2, "[I] \u{1b}[38;5;206;1malpha\u{1b}[0m hi"),
    ] {
        let colored = installation(
            &Config {
                colors: Some(ColorSetting::Level(level)),
                ..plain()
            },
            0,
        );
        assert_eq!(
            colored.render(&message(
                &context,
                LoggerType::Info,
                "alpha",
                2_000,
                vec![json!("hi")],
            )),
            expected
        );
    }
}

#[tokio::test]
async fn native_registration_is_reversible_and_browser_dispatch_keeps_original_args() {
    let context = Context::new();
    let records = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&records);
    let installation = install_with_sink(
        &context,
        &plain(),
        clock(0),
        Arc::new(move |record| observed.lock().push(record)),
    )
    .unwrap();
    context
        .logger(Some("native"))
        .info([json!("value=%d"), json!(3.9)]);
    assert_eq!(
        *records.lock(),
        [ConsoleRecord::Rendered("[I] native value=3".to_owned())]
    );
    installation.dispose().await.unwrap();
    context.logger(Some("native")).info([json!("after")]);
    assert_eq!(records.lock().len(), 1);

    let browser = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&browser);
    let effect = install_browser_with_sink(
        &context,
        &Config {
            levels: Some(std::collections::BTreeMap::from([(
                "default".to_owned(),
                3,
            )])),
            ..Config::default()
        },
        Arc::new(move |record| captured.lock().push(record)),
    )
    .unwrap();
    context
        .logger(Some("browser"))
        .error([json!("boom"), json!({"code":7})]);
    context.logger(Some("browser")).warn([json!("careful")]);
    context.logger(Some("browser")).debug([json!("trace")]);
    assert_eq!(
        *browser.lock(),
        [
            ConsoleRecord::Browser {
                method: ConsoleMethod::Error,
                prefix: "[E] browser".to_owned(),
                args: vec![json!("boom"), json!({"code":7})],
            },
            ConsoleRecord::Browser {
                method: ConsoleMethod::Warn,
                prefix: "[W] browser".to_owned(),
                args: vec![json!("careful")],
            },
            ConsoleRecord::Browser {
                method: ConsoleMethod::Log,
                prefix: "[D] browser".to_owned(),
                args: vec![json!("trace")],
            },
        ]
    );
    effect.dispose().await.unwrap();
}

#[test]
fn config_rejects_true_and_out_of_range_colors_and_materializes_defaults() {
    for colors in [json!(true), json!(4)] {
        let config: Config = serde_json::from_value(json!({"colors":colors})).unwrap();
        let context = Context::new();
        assert!(install_with_sink(&context, &config, clock(0), Arc::new(|_| {})).is_err());
    }
    let defaults = serde_json::to_value(Config::default()).unwrap();
    assert_eq!(defaults["showDiff"], false);
    assert_eq!(defaults["showTime"], "yyyy-MM-dd hh:mm:ss ");
}
