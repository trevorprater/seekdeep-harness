//! Live WASM coverage for conversation clock and duration helpers.

#![cfg(target_arch = "wasm32")]
#![allow(clippy::approx_constant, clippy::float_cmp)]

use js_sys::Function;
use seekdeep_client_ui_conversation::{
    format_latency_seconds_browser, format_message_clock_browser, format_run_duration_browser,
    format_tokens_per_second_browser, milliseconds_until_next_local_midnight_browser,
    start_of_local_day_browser,
};
use wasm_bindgen::prelude::wasm_bindgen;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
export function localDate(year, month, day, hours = 0, minutes = 0) {
  return new Date(year, month, day, hours, minutes).getTime()
}
export function makeChromeTranslate() {
  return (key, vars) => {
    if (key === 'duration.seconds') return `${vars.seconds}秒`
    if (key === 'duration.minutes') return `${vars.minutes}分${vars.seconds}秒`
    if (key === 'clock.md') return `${vars.m}月${vars.d}日`
    if (key === 'clock.ymd') return `${vars.y}年${vars.m}月${vars.d}日`
    throw new Error(`unexpected key: ${key}`)
  }
}
"#)]
extern "C" {
    #[wasm_bindgen(js_name = localDate)]
    fn local_date(year: u32, month: u32, day: u32, hours: u32, minutes: u32) -> f64;
    #[wasm_bindgen(js_name = makeChromeTranslate)]
    fn make_chrome_translate() -> Function;
}

#[wasm_bindgen_test]
fn duration_and_metric_labels_match_javascript_rounding() {
    let translate = make_chrome_translate();
    for (milliseconds, expected) in [
        (0.0, "0秒"),
        (-500.0, "0秒"),
        (15_999.0, "15秒"),
        (125_000.0, "2分05秒"),
    ] {
        assert_eq!(
            format_run_duration_browser(milliseconds, translate.clone())
                .unwrap()
                .as_string()
                .as_deref(),
            Some(expected)
        );
    }
    for (milliseconds, expected) in [
        (840.0, "0.8"),
        (1_000.0, "1"),
        (9_949.0, "9.9"),
        (12_400.0, "12"),
        (-5.0, "0"),
    ] {
        assert_eq!(
            format_latency_seconds_browser(milliseconds).unwrap(),
            expected
        );
    }
    for (tokens_per_second, expected) in [(34.4, "34"), (9.96, "10"), (3.14, "3.1"), (-1.0, "0")] {
        assert_eq!(
            format_tokens_per_second_browser(tokens_per_second).unwrap(),
            expected
        );
    }
}

#[wasm_bindgen_test]
fn message_clock_uses_local_day_and_year_boundaries() {
    let translate = make_chrome_translate();
    let now = local_date(2026, 6, 29, 10, 0);
    assert_eq!(
        format_message_clock_browser(
            local_date(2026, 6, 29, 14, 24),
            translate.clone(),
            Some(now)
        )
        .unwrap(),
        "14:24"
    );
    assert_eq!(
        format_message_clock_browser(local_date(2026, 0, 1, 14, 24), translate.clone(), Some(now))
            .unwrap(),
        "1月1日 14:24"
    );
    assert_eq!(
        format_message_clock_browser(local_date(2025, 11, 31, 9, 5), translate, Some(now)).unwrap(),
        "2025年12月31日 09:05"
    );
}

#[wasm_bindgen_test]
fn local_midnight_helpers_preserve_browser_calendar_semantics() {
    let noon = local_date(2026, 6, 29, 12, 0);
    assert_eq!(
        start_of_local_day_browser(noon).unwrap(),
        local_date(2026, 6, 29, 0, 0)
    );
    assert_eq!(
        milliseconds_until_next_local_midnight_browser(noon).unwrap(),
        12.0 * 3_600_000.0
    );
}
