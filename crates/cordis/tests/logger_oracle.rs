//! Logger naming, thresholds, buffering, formatting, colors, and clock oracle.

use std::{collections::BTreeMap, sync::Arc};

use parking_lot::Mutex;
use seekdeep_cordis::{
    Context, CordisClock, LogExporter, LogMessage, Logger, LoggerLevel, LoggerType,
};
use serde_json::json;

#[derive(Debug)]
struct FixedClock;

impl CordisClock for FixedClock {
    fn now_ms(&self) -> i64 {
        1_234
    }
}

#[tokio::test]
async fn exporter_thresholds_sequence_buffer_and_intercepted_names_match_source() {
    let context = Context::new_with_clock(Arc::new(FixedClock));
    context.logger_service().set_buffer_size(2);
    let captured = Arc::new(Mutex::new(Vec::<LogMessage>::new()));
    let capture = captured.clone();
    let mut exporter = LogExporter::new(move |message| capture.lock().push(message));
    exporter.max_length = 5;
    exporter.levels = BTreeMap::from([("custom".to_owned(), 0), ("default".to_owned(), 3)]);
    let registration = context
        .logger_service()
        .exporter(&context, exporter)
        .expect("exporter");

    let root = context.logger(None);
    assert_eq!(root.name(), "root");
    root.error([json!("root-error")]);
    root.info([json!("root-info")]);
    root.warn([json!("root-warn")]);

    let child = context.intercept("logger", json!({"name":"custom", "level":3}));
    let custom = child.logger(None);
    assert_eq!(custom.name(), "custom");
    custom.error([json!("x=%d %s %%"), json!(3.9), json!("ok")]);
    custom.debug([json!({"a":1}), json!("tail")]);

    {
        let records = captured.lock();
        assert_eq!(records.len(), 4);
        assert_eq!(
            records
                .iter()
                .map(|message| (
                    message.sn,
                    message.ts,
                    message.name.as_str(),
                    message.message_type,
                    message.level,
                ))
                .collect::<Vec<_>>(),
            [
                (1, 1_234, "root", LoggerType::Error, LoggerLevel::Error),
                (2, 1_234, "root", LoggerType::Info, LoggerLevel::Info),
                (3, 1_234, "root", LoggerType::Warn, LoggerLevel::Warn),
                (4, 1_234, "custom", LoggerType::Error, LoggerLevel::Error),
            ]
        );
        let format_exporter = {
            let mut exporter = LogExporter::new(|_| {});
            exporter.max_length = 5;
            exporter
        };
        assert_eq!(
            records
                .iter()
                .map(|message| Logger::format(&format_exporter, message))
                .collect::<Vec<_>>(),
            ["root-...", "root-...", "root-...", "x=3 o..."]
        );
    }

    assert_eq!(
        context
            .logger_service()
            .buffer()
            .iter()
            .map(|message| (message.sn, message.name.as_str(), message.message_type))
            .collect::<Vec<_>>(),
        [
            (4, "custom", LoggerType::Error),
            (5, "custom", LoggerType::Debug),
        ]
    );
    let captured_before_disposal = captured.lock().len();
    registration.dispose().await.unwrap();
    root.error([json!("after-dispose")]);
    assert_eq!(captured.lock().len(), captured_before_disposal);
}

#[test]
fn formatting_color_codes_multiline_and_custom_formatter_match_source() {
    let context = Context::new_with_clock(Arc::new(FixedClock));
    let logger = context.logger(Some("alpha"));
    logger.info([json!("one\r\n\nthree"), json!({"tail":true})]);
    let message = context.logger_service().buffer().remove(0);
    let mut exporter = LogExporter::new(|_| {});
    exporter.max_length = 3;
    assert_eq!(Logger::format(&exporter, &message), "one\n\nthr...");
    assert_eq!(Logger::code("alpha", 1), Some(2));
    assert_eq!(Logger::code("alpha", 2), Some(206));
    assert_eq!(Logger::color(&exporter, 6, "x", ""), "x");
    exporter.colors = 1;
    assert_eq!(Logger::color(&exporter, 6, "x", ""), "\u{1b}[36mx\u{1b}[0m");
}
