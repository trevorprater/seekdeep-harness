//! Submission policy, image rejection, metrics, and Host settings parity.

#![cfg(not(target_arch = "wasm32"))]

use seekdeep_attachment::ImageAttachmentLimits;
use seekdeep_client_ui_conversation::{
    AssistantMetricNode, AssistantTiming, BusyEnterBehavior, ComposerSubmissionPolicy,
    ComposerSubmitGesture, ContextPressureStats, ImageCopyLocale, TokenUsageStats,
    WindowMetricNode, assistant_step_reading, attachment_error_text, billed_input_tokens,
    cache_hit_percent, context_occupancy, conversation_settings_schema, derive_turn_metrics,
    derive_window_stats, format_duration, format_latency_seconds, format_tokens,
    format_tokens_per_second, image_size_text,
};

fn assert_close(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < f64::EPSILON);
}

#[test]
fn submission_policy_defaults_to_queue_and_accelerated_gesture_selects_the_opposite() {
    let mut policy = ComposerSubmissionPolicy::default();
    assert_eq!(policy.busy_enter(), BusyEnterBehavior::Queue);
    assert_eq!(
        policy.resolve(false, ComposerSubmitGesture::Enter, true),
        BusyEnterBehavior::Queue
    );
    assert_eq!(
        policy.resolve(true, ComposerSubmitGesture::Accelerated, true),
        BusyEnterBehavior::Steer
    );
    assert_eq!(
        policy.resolve(true, ComposerSubmitGesture::Accelerated, false),
        BusyEnterBehavior::Queue
    );
    assert!(policy.set_busy_enter(BusyEnterBehavior::Steer));
    assert!(!policy.set_busy_enter(BusyEnterBehavior::Steer));
    assert_eq!(
        policy.resolve(true, ComposerSubmitGesture::Enter, true),
        BusyEnterBehavior::Steer
    );
    assert_eq!(
        policy.resolve(true, ComposerSubmitGesture::Accelerated, true),
        BusyEnterBehavior::Queue
    );
    policy.adopt(seekdeep_client_ui_conversation::ConversationSettings {
        busy_enter: BusyEnterBehavior::Queue,
    });
    assert_eq!(policy.busy_enter(), BusyEnterBehavior::Queue);
}

#[test]
fn image_rejection_copy_names_limits_and_folds_unknown_reasons() {
    let limits = ImageAttachmentLimits {
        max_image_bytes: 5 * 1024 * 1024,
        max_images_per_message: 20,
        max_message_image_bytes: 100 * 1024 * 1024,
        max_image_pixels: 40_000_000,
        media_types: Vec::new(),
    };
    assert_eq!(image_size_text(10.0 * 1024.0 * 1024.0), "10MB");
    assert_eq!(image_size_text(2.5 * 1024.0 * 1024.0), "2.5MB");
    assert_eq!(
        attachment_error_text(ImageCopyLocale::Zh, "TOO_MANY_IMAGES", Some(&limits)),
        "一条消息最多添加 20 张图片"
    );
    assert_eq!(
        attachment_error_text(ImageCopyLocale::En, "IMAGE_TOO_LARGE", Some(&limits)),
        "Each image must be smaller than 5MB"
    );
    assert_eq!(
        attachment_error_text(ImageCopyLocale::Zh, "INVALID_IMAGE", Some(&limits)),
        "仅支持 PNG、JPG、WebP、GIF 格式的图片"
    );
    assert_eq!(
        attachment_error_text(ImageCopyLocale::En, "INVALID_IMAGE_BASE64", None),
        "Sending images failed (INVALID_IMAGE_BASE64); re-add them and try again"
    );
}

fn assistant(
    turn: u64,
    step: u64,
    timing: Option<AssistantTiming>,
    output_tokens: Option<f64>,
) -> AssistantMetricNode {
    AssistantMetricNode {
        turn,
        step,
        timing,
        output_tokens,
    }
}

#[test]
#[allow(clippy::approx_constant)]
fn turn_metrics_use_lowest_step_ttft_and_all_valid_decode_samples() {
    let first = assistant(
        1,
        1,
        Some(AssistantTiming {
            step_start_time: Some(1_000.0),
            first_token_time: Some(2_200.0),
            completed_time: 5_200.0,
        }),
        Some(40.0),
    );
    assert_eq!(
        assistant_step_reading(&first),
        seekdeep_client_ui_conversation::StepReading {
            ttft_ms: Some(1_200.0),
            decode_ms: Some(3_000.0),
            output_tokens: Some(40.0),
        }
    );
    assert_eq!(
        assistant_step_reading(&assistant(1, 1, None, Some(5.0))),
        seekdeep_client_ui_conversation::StepReading {
            ttft_ms: None,
            decode_ms: None,
            output_tokens: Some(5.0),
        }
    );
    assert_eq!(
        assistant_step_reading(&assistant(
            1,
            1,
            Some(AssistantTiming {
                step_start_time: Some(2_000.0),
                first_token_time: Some(1_500.0),
                completed_time: 1_200.0,
            }),
            Some(f64::NAN),
        )),
        seekdeep_client_ui_conversation::StepReading {
            ttft_ms: Some(0.0),
            decode_ms: Some(0.0),
            output_tokens: None,
        }
    );
    let metrics = derive_turn_metrics(&[
        assistant(
            1,
            2,
            Some(AssistantTiming {
                step_start_time: Some(10_000.0),
                first_token_time: Some(10_200.0),
                completed_time: 12_200.0,
            }),
            Some(60.0),
        ),
        first,
        assistant(
            2,
            1,
            Some(AssistantTiming {
                step_start_time: None,
                first_token_time: Some(5_000.0),
                completed_time: 5_000.0,
            }),
            Some(10.0),
        ),
    ]);
    assert_eq!(metrics[&1].ttft_ms, Some(1_200.0));
    assert_eq!(metrics[&1].tokens_per_second, Some(20.0));
    assert!(!metrics.contains_key(&2));
    let throughput_only = derive_turn_metrics(&[
        assistant(3, 1, None, None),
        assistant(
            3,
            2,
            Some(AssistantTiming {
                step_start_time: Some(10_000.0),
                first_token_time: Some(10_500.0),
                completed_time: 12_500.0,
            }),
            Some(30.0),
        ),
    ]);
    assert_eq!(throughput_only[&3].ttft_ms, None);
    assert_eq!(throughput_only[&3].tokens_per_second, Some(15.0));
    assert!(derive_turn_metrics(&[assistant(4, 1, None, None)]).is_empty());
    assert_eq!(format_latency_seconds(840.0), "0.8");
    assert_eq!(format_latency_seconds(1_000.0), "1");
    assert_eq!(format_latency_seconds(9_949.0), "9.9");
    assert_eq!(format_latency_seconds(12_400.0), "12");
    assert_eq!(format_latency_seconds(-5.0), "0");
    assert_eq!(format_tokens_per_second(34.4), "34");
    assert_eq!(format_tokens_per_second(9.96), "10");
    assert_eq!(format_tokens_per_second(3.14), "3.1");
    assert_eq!(format_tokens_per_second(-1.0), "0");
}

#[test]
fn stats_strip_fallback_formatting_billing_and_occupancy_match_the_oracle() {
    let timed = assistant(
        1,
        1,
        Some(AssistantTiming {
            step_start_time: Some(1_000.0),
            first_token_time: Some(1_800.0),
            completed_time: 4_800.0,
        }),
        Some(40.0),
    );
    let untimed = assistant(1, 2, None, None);
    let next_turn = assistant(2, 3, None, None);
    let stats = derive_window_stats(&[
        WindowMetricNode::Assistant(timed),
        WindowMetricNode::Assistant(untimed),
        WindowMetricNode::ToolResult {
            time: 7_000.0,
            call_time: Some(4_000.0),
        },
        WindowMetricNode::ToolResult {
            time: 9_000.0,
            call_time: None,
        },
        WindowMetricNode::Assistant(next_turn),
        WindowMetricNode::Other,
    ]);
    assert_eq!(stats.turns, 2);
    assert_eq!(stats.steps, 3);
    assert_close(stats.llm_ms, 3_800.0);
    assert_close(stats.tool_ms, 3_000.0);
    assert_close(stats.ttft_ms, 800.0);
    assert_eq!(stats.ttft_steps, 1);
    assert_close(stats.decode_ms, 3_000.0);
    assert_close(stats.decode_tokens, 40.0);

    assert_eq!(format_tokens(517.0), "517");
    assert_eq!(format_tokens(12_240.0), "12.2K");
    assert_eq!(format_tokens(517_000.0), "517K");
    assert_eq!(format_tokens(1_230_000.0), "1.2M");
    assert_eq!(format_duration(45_230.0), "45.2s");
    assert_eq!(format_duration(162_000.0), "2m42s");

    let usage = TokenUsageStats {
        uncached_input_tokens: 10.0,
        cache_read_tokens: 90.0,
        cache_write_tokens: 100.0,
        output_tokens: 7.0,
    };
    assert_close(billed_input_tokens(usage), 200.0);
    assert_eq!(cache_hit_percent(usage), Some(45.0));
    assert_eq!(cache_hit_percent(TokenUsageStats::default()), None);

    assert_eq!(
        context_occupancy(Some(ContextPressureStats {
            pressure_tokens: Some(32_000.0),
            projected_tokens: Some(6_000.0),
            context_window: Some(128_000.0),
        })),
        Some(seekdeep_client_ui_conversation::ContextOccupancy {
            percent: 5.0,
            used_tokens: 6_000.0,
            context_window: 128_000.0,
        })
    );
    assert_eq!(
        context_occupancy(Some(ContextPressureStats {
            pressure_tokens: Some(300_000.0),
            projected_tokens: None,
            context_window: Some(128_000.0),
        }))
        .map(|occupancy| occupancy.percent),
        Some(100.0)
    );
    assert_eq!(context_occupancy(None), None);
}

#[test]
fn host_schema_defaults_validates_and_host_plugin_stays_optional() {
    assert_eq!(
        conversation_settings_schema()
            .resolve(&serde_json::json!({}))
            .unwrap(),
        serde_json::json!({"busyEnter":"queue"})
    );
    assert!(
        conversation_settings_schema()
            .resolve(&serde_json::json!({"busyEnter":"invalid"}))
            .is_err()
    );
    assert!(
        seekdeep_client_ui_conversation::host_plugin()
            .inject()
            .is_empty()
    );
}
