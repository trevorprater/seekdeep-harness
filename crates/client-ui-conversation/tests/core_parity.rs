//! Submission policy, image rejection, metrics, and Host settings parity.

#![cfg(not(target_arch = "wasm32"))]

use seekdeep_attachment::ImageAttachmentLimits;
use seekdeep_client_ui_conversation::{
    AssistantMetricNode, AssistantTiming, BusyEnterBehavior, ComposerSubmissionPolicy,
    ComposerSubmitGesture, ImageCopyLocale, assistant_step_reading, attachment_error_text,
    conversation_settings_schema, derive_turn_metrics, format_latency_seconds,
    format_tokens_per_second, image_size_text,
};

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
