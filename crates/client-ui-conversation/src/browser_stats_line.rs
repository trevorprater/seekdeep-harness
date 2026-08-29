//! Compiled composer stats strip and browser-compatible stat helpers.

use std::cell::RefCell;

use js_sys::{Array, Function, Math, Number, Object, Reflect, Set};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{
    browser_message_chrome::format_tokens_per_second_browser, browser_reasoning::inject_style,
};

const STATS_CSS: &str =
    include_str!("../../../packages/client/ui-conversation/src/client/chat/StatsLine.module.css");

thread_local! {
    static COMPONENT: RefCell<Option<JsValue>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    fragment: JsValue,
    tooltip: JsValue,
}

#[derive(Clone, Copy)]
struct Stats {
    turns: f64,
    steps: f64,
    llm_ms: f64,
    tool_ms: f64,
    ttft_ms: f64,
    ttft_steps: f64,
    decode_ms: f64,
    decode_tokens: f64,
}

#[derive(Clone, Copy)]
struct Usage {
    uncached_input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
}

#[derive(Clone, Copy)]
struct BrowserStepReading {
    ttft_ms: Option<f64>,
    decode_ms: Option<f64>,
    output_tokens: Option<f64>,
}

/// Configures the compiled `StatsLine` component.
///
/// # Errors
///
/// Returns on missing React/Tooltip faces or stylesheet failure.
#[wasm_bindgen(js_name = configureClientUiConversationStatsLine)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_conversation_stats_line(
    react: JsValue,
    ui_primitives: JsValue,
) -> Result<(), JsValue> {
    for method in [
        "createElement",
        "memo",
        "useLayoutEffect",
        "useMemo",
        "useRef",
        "useState",
    ] {
        required_function(&react, method, "React")?;
    }
    let modules = BrowserModules {
        fragment: required_property(&react, "Fragment", "React")?,
        tooltip: required_property(&ui_primitives, "Tooltip", "ui-primitives")?,
        react,
    };
    inject_style(
        "StatsLine",
        STATS_CSS,
        &[
            ("root", "seekdeep-conversation-statsLine-root"),
            ("sep", "seekdeep-conversation-statsLine-sep"),
        ],
    )?;
    let render_modules = modules.clone();
    let raw =
        Closure::wrap(
            Box::new(move |props: JsValue| render_stats_line(&render_modules, &props))
                as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
        )
        .into_js_value();
    let component =
        required_function(&modules.react, "memo", "React")?.call1(&modules.react, &raw)?;
    COMPONENT.with(|configured| *configured.borrow_mut() = Some(component));
    Ok(())
}

/// Returns the compiled `StatsLine` component.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = statsLineComponent)]
pub fn stats_line_component() -> Result<JsValue, JsValue> {
    COMPONENT.with(|component| {
        component.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-conversation StatsLine was not configured").into()
        })
    })
}

/// Folds a browser conversation-node array into window-scoped fallback stats.
///
/// # Errors
///
/// Returns when a typed node field is missing or has the wrong primitive type.
#[wasm_bindgen(js_name = deriveStats)]
#[allow(clippy::needless_pass_by_value)]
pub fn derive_stats_browser(nodes: JsValue) -> Result<JsValue, JsValue> {
    Ok(derive_stats_value(&nodes.dyn_into::<Array>()?)?.into())
}

/// Formats a compact token count with JavaScript number semantics.
///
/// # Errors
///
/// Returns if JavaScript cannot stringify the result.
#[wasm_bindgen(js_name = formatTokens)]
pub fn format_tokens_browser(tokens: f64) -> Result<String, JsValue> {
    if tokens < 1_000.0 {
        number_string(tokens)
    } else if tokens < 1_000_000.0 {
        Ok(format!("{}K", scaled_number(tokens / 1_000.0)?))
    } else {
        Ok(format!("{}M", scaled_number(tokens / 1_000_000.0)?))
    }
}

/// Formats a compact duration with JavaScript rounding semantics.
///
/// # Errors
///
/// Returns if JavaScript cannot stringify a component.
#[wasm_bindgen(js_name = formatDuration)]
pub fn format_duration_browser(milliseconds: f64) -> Result<String, JsValue> {
    let seconds = milliseconds / 1_000.0;
    if seconds < 60.0 {
        Ok(format!(
            "{}s",
            number_string(Math::round(seconds * 10.0) / 10.0)?
        ))
    } else {
        let whole = Math::round(seconds);
        Ok(format!(
            "{}m{}s",
            number_string(Math::floor(whole / 60.0))?,
            number_string(whole % 60.0)?
        ))
    }
}

/// Sums the browser token projection's disjoint prompt-side buckets.
///
/// # Errors
///
/// Returns when a typed projection field is missing or not numeric.
#[wasm_bindgen(js_name = billedInputTokens)]
#[allow(clippy::needless_pass_by_value)]
pub fn billed_input_tokens_browser(usage: JsValue) -> Result<f64, JsValue> {
    Ok(parse_usage(&usage)?.billed_input_tokens())
}

/// Returns the rounded browser cache-hit percentage or `null` with no input.
///
/// # Errors
///
/// Returns when a typed projection field is missing or not numeric.
#[wasm_bindgen(js_name = cacheHitPercent)]
#[allow(clippy::needless_pass_by_value)]
pub fn cache_hit_percent_browser(usage: JsValue) -> Result<JsValue, JsValue> {
    let usage = parse_usage(&usage)?;
    Ok(usage
        .cache_hit_percent()
        .map_or(JsValue::NULL, JsValue::from_f64))
}

/// Resolves browser context occupancy or `null` until both values are known.
///
/// # Errors
///
/// Returns when a present projection field cannot be coerced to a number.
#[wasm_bindgen(js_name = contextOccupancy)]
#[allow(clippy::needless_pass_by_value)]
pub fn context_occupancy_browser(pressure: JsValue) -> Result<JsValue, JsValue> {
    if pressure.is_null() || pressure.is_undefined() {
        return Ok(JsValue::NULL);
    }
    let projected = Reflect::get(&pressure, &JsValue::from_str("projectedTokens"))?;
    let used = if projected.is_null() || projected.is_undefined() {
        Reflect::get(&pressure, &JsValue::from_str("pressureTokens"))?
    } else {
        projected
    };
    let context_window = Reflect::get(&pressure, &JsValue::from_str("contextWindow"))?;
    if used.is_undefined() || context_window.is_undefined() {
        return Ok(JsValue::NULL);
    }
    let used_tokens = javascript_number(&used)?;
    let context_window_number = javascript_number(&context_window)?;
    Ok(object(&[
        (
            "percent",
            JsValue::from_f64(Math::min(
                100.0,
                Math::round(used_tokens / context_window_number * 100.0),
            )),
        ),
        ("usedTokens", used),
        ("contextWindow", context_window),
    ])?
    .into())
}

#[allow(clippy::too_many_lines)] // Hook order and the complete grouped row stay together.
fn render_stats_line(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let settled_nodes = select_settled_nodes(props)?.dyn_into::<Array>()?;
    let use_projection = required_function(props, "useProjection", "StatsLine props")?;
    let usage = use_projection.call1(&JsValue::UNDEFINED, &JsValue::from_str("tokenUsage"))?;
    let projected =
        use_projection.call1(&JsValue::UNDEFINED, &JsValue::from_str("sessionStats"))?;
    let fallback_nodes = settled_nodes.clone();
    let projected_stats = projected.clone();
    let stats_factory = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if projected_stats.is_null() || projected_stats.is_undefined() {
            Ok(derive_stats_value(&fallback_nodes)?.into())
        } else {
            Ok(projected_stats.clone())
        }
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let stats_value = required_function(&modules.react, "useMemo", "React")?.call2(
        &modules.react,
        &stats_factory.into_js_value(),
        &Array::of2(&projected, settled_nodes.as_ref()),
    )?;
    let stats = parse_stats(&stats_value)?;
    let translate = required_function(props, "t", "StatsLine props")?;
    let groups = build_groups(&translate, stats, &usage)?;
    let line = groups.join(" | ");
    let root_ref = required_function(&modules.react, "useRef", "React")?
        .call1(&modules.react, &JsValue::NULL)?;
    let truncated_state = required_function(&modules.react, "useState", "React")?
        .call1(&modules.react, &JsValue::FALSE)?
        .dyn_into::<Array>()?;
    let truncated = truncated_state
        .get(0)
        .as_bool()
        .ok_or_else(|| js_sys::TypeError::new("StatsLine truncated state must be a boolean"))?;
    let set_truncated = truncated_state.get(1).dyn_into::<Function>()?;
    install_measure_effect(&modules.react, &root_ref, &set_truncated, &line)?;
    if groups.is_empty() {
        return Ok(JsValue::NULL);
    }
    let mut row_children = Vec::new();
    for (index, group) in groups.into_iter().enumerate() {
        let mut fragment_children = Vec::new();
        if index > 0 {
            fragment_children.push(create_element(
                &modules.react,
                &modules.fragment,
                None,
                &[
                    create_element(
                        &modules.react,
                        &JsValue::from_str("span"),
                        Some(&object(&[
                            (
                                "className",
                                JsValue::from_str("seekdeep-conversation-statsLine-sep"),
                            ),
                            ("aria-hidden", JsValue::TRUE),
                        ])?),
                        &[JsValue::from_str("|")],
                    )?,
                    JsValue::from_str(" "),
                ],
            )?);
        }
        fragment_children.push(create_element(
            &modules.react,
            &JsValue::from_str("span"),
            None,
            &[JsValue::from_str(&group)],
        )?);
        row_children.push(create_element(
            &modules.react,
            &modules.fragment,
            Some(&object(&[("key", JsValue::from_str(&group))])?),
            &fragment_children,
        )?);
    }
    let root = create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&object(&[
            ("ref", root_ref),
            (
                "className",
                JsValue::from_str("seekdeep-conversation-statsLine-root"),
            ),
        ])?),
        &row_children,
    )?;
    create_element(
        &modules.react,
        &modules.tooltip,
        Some(&object(&[
            ("label", JsValue::from_str(&line)),
            ("side", JsValue::from_str("top")),
            ("delayMs", JsValue::from_f64(500.0)),
            ("disabled", JsValue::from_bool(!truncated)),
        ])?),
        &[root],
    )
}

fn select_settled_nodes(props: &JsValue) -> Result<JsValue, JsValue> {
    let selector = Closure::wrap(
        Box::new(move |snapshot: JsValue| -> Result<JsValue, JsValue> {
            let chat = required_property(&snapshot, "chat", "conversation snapshot")?;
            let legacy = required_property(&chat, "legacy", "conversation chat snapshot")?;
            required_property(&legacy, "nodes", "legacy chat snapshot")
        }) as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    );
    required_function(props, "useSession", "StatsLine props")?
        .call1(&JsValue::UNDEFINED, &selector.into_js_value())
}

fn build_groups(
    translate: &Function,
    stats: Stats,
    usage_value: &JsValue,
) -> Result<Vec<String>, JsValue> {
    let mut groups = Vec::new();
    if stats.steps > 0.0 {
        groups.push(translate_text(
            translate,
            "stats.counts",
            &object(&[
                ("turns", JsValue::from_f64(stats.turns)),
                ("steps", JsValue::from_f64(stats.steps)),
            ])?,
        )?);
        let mut durations = Vec::new();
        if stats.llm_ms > 0.0 {
            durations.push(translate_text(
                translate,
                "stats.llm",
                &object(&[(
                    "duration",
                    JsValue::from_str(&format_duration_browser(stats.llm_ms)?),
                )])?,
            )?);
        }
        if stats.tool_ms > 0.0 {
            durations.push(translate_text(
                translate,
                "stats.toolCall",
                &object(&[(
                    "duration",
                    JsValue::from_str(&format_duration_browser(stats.tool_ms)?),
                )])?,
            )?);
        }
        if !durations.is_empty() {
            groups.push(durations.join(" · "));
        }
        let mut speeds = Vec::new();
        if stats.ttft_steps > 0.0 {
            speeds.push(translate_text(
                translate,
                "stats.ttftAverage",
                &object(&[(
                    "duration",
                    JsValue::from_str(&format_duration_browser(stats.ttft_ms / stats.ttft_steps)?),
                )])?,
            )?);
        }
        if stats.decode_ms > 0.0 {
            speeds.push(translate_text(
                translate,
                "stats.tokensPerSecond",
                &object(&[(
                    "throughput",
                    JsValue::from_str(&format_tokens_per_second_browser(
                        stats.decode_tokens / (stats.decode_ms / 1_000.0),
                    )?),
                )])?,
            )?);
        }
        if !speeds.is_empty() {
            groups.push(speeds.join(" · "));
        }
    }
    if !usage_value.is_undefined() {
        let usage = parse_usage(usage_value)?;
        let billed = usage.billed_input_tokens();
        if billed > 0.0 || usage.output > 0.0 {
            if let Some(percent) = usage.cache_hit_percent() {
                groups.push(translate_text(
                    translate,
                    "stats.cacheHit",
                    &object(&[("percent", JsValue::from_f64(percent))])?,
                )?);
            }
            groups.push(translate_text(
                translate,
                "stats.tokens",
                &object(&[
                    ("input", JsValue::from_str(&format_tokens_browser(billed)?)),
                    (
                        "output",
                        JsValue::from_str(&format_tokens_browser(usage.output)?),
                    ),
                ])?,
            )?);
        }
    }
    Ok(groups)
}

fn install_measure_effect(
    react: &JsValue,
    root_ref: &JsValue,
    set_truncated: &Function,
    line: &str,
) -> Result<(), JsValue> {
    let effect_ref = root_ref.clone();
    let effect_setter = set_truncated.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let element = Reflect::get(&effect_ref, &JsValue::from_str("current"))?;
        if element.is_null() {
            return Ok(JsValue::UNDEFINED);
        }
        let measure_element = element.clone();
        let measure_setter = effect_setter.clone();
        let measure = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            let scroll_width = numeric_property(&measure_element, "scrollWidth", "stats row")?;
            let client_width = numeric_property(&measure_element, "clientWidth", "stats row")?;
            measure_setter.call1(
                &JsValue::UNDEFINED,
                &JsValue::from_bool(scroll_width > client_width),
            )?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value()
        .dyn_into::<Function>()?;
        measure.call0(&JsValue::UNDEFINED)?;
        let constructor = Reflect::get(&js_sys::global(), &JsValue::from_str("ResizeObserver"))?;
        if constructor.is_undefined() {
            return Ok(JsValue::UNDEFINED);
        }
        let observer = Reflect::construct(
            &constructor.dyn_into::<Function>()?,
            &Array::of1(measure.as_ref()),
        )?;
        call_method(&observer, "observe", &[element])?;
        let cleanup_observer = observer;
        let cleanup = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            call_method(&cleanup_observer, "disconnect", &[])?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value();
        Ok(cleanup)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    required_function(react, "useLayoutEffect", "React")?.call2(
        react,
        &effect.into_js_value(),
        &Array::of1(&JsValue::from_str(line)),
    )?;
    Ok(())
}

fn derive_stats_value(nodes: &Array) -> Result<Object, JsValue> {
    let turns = Set::new(&JsValue::UNDEFINED);
    let mut stats = Stats {
        turns: 0.0,
        steps: 0.0,
        llm_ms: 0.0,
        tool_ms: 0.0,
        ttft_ms: 0.0,
        ttft_steps: 0.0,
        decode_ms: 0.0,
        decode_tokens: 0.0,
    };
    for index in 0..nodes.length() {
        let node = nodes.get(index);
        let kind = Reflect::get(&node, &JsValue::from_str("kind"))?
            .as_string()
            .unwrap_or_default();
        if kind == "tool-result" {
            let call_time = Reflect::get(&node, &JsValue::from_str("callTime"))?;
            if !call_time.is_null() {
                stats.tool_ms += Math::max(
                    0.0,
                    numeric_property(&node, "time", "tool result")?
                        - javascript_number(&call_time)?,
                );
            }
            continue;
        }
        if kind != "assistant" {
            continue;
        }
        turns.add(&required_property(&node, "turn", "assistant node")?);
        stats.steps += 1.0;
        let timing = Reflect::get(&node, &JsValue::from_str("timing"))?;
        if !timing.is_undefined() {
            let step_start = Reflect::get(&timing, &JsValue::from_str("stepStartTime"))?;
            if !step_start.is_null() {
                stats.llm_ms += Math::max(
                    0.0,
                    numeric_property(&timing, "completedTime", "assistant timing")?
                        - javascript_number(&step_start)?,
                );
            }
        }
        let reading = assistant_step_reading_value(&node, &timing)?;
        if let Some(ttft_ms) = reading.ttft_ms {
            stats.ttft_ms += ttft_ms;
            stats.ttft_steps += 1.0;
        }
        if let (Some(decode_ms), Some(output_tokens)) = (reading.decode_ms, reading.output_tokens) {
            stats.decode_ms += decode_ms;
            stats.decode_tokens += output_tokens;
        }
    }
    stats.turns = f64::from(turns.size());
    stats_object(stats)
}

fn assistant_step_reading_value(
    node: &JsValue,
    timing: &JsValue,
) -> Result<BrowserStepReading, JsValue> {
    let (ttft_ms, decode_ms) = if timing.is_undefined() {
        (None, None)
    } else {
        let step_start = Reflect::get(timing, &JsValue::from_str("stepStartTime"))?;
        let first_token = Reflect::get(timing, &JsValue::from_str("firstTokenTime"))?;
        let ttft = if step_start.is_null() || first_token.is_null() {
            None
        } else {
            Some(Math::max(
                0.0,
                javascript_number(&first_token)? - javascript_number(&step_start)?,
            ))
        };
        let decode = if first_token.is_null() {
            None
        } else {
            Some(Math::max(
                0.0,
                numeric_property(timing, "completedTime", "assistant timing")?
                    - javascript_number(&first_token)?,
            ))
        };
        (ttft, decode)
    };
    let usage = Reflect::get(node, &JsValue::from_str("usage"))?;
    let output_tokens = if usage.is_object() && !usage.is_null() {
        Reflect::get(&usage, &JsValue::from_str("outputTokens"))?
            .as_f64()
            .filter(|value| value.is_finite() && *value >= 0.0)
    } else {
        None
    };
    Ok(BrowserStepReading {
        ttft_ms,
        decode_ms,
        output_tokens,
    })
}

impl Usage {
    fn billed_input_tokens(self) -> f64 {
        self.uncached_input + self.cache_read + self.cache_write
    }

    fn cache_hit_percent(self) -> Option<f64> {
        let denominator = self.billed_input_tokens();
        (denominator != 0.0).then(|| Math::round(self.cache_read / denominator * 100.0))
    }
}

fn parse_usage(value: &JsValue) -> Result<Usage, JsValue> {
    Ok(Usage {
        uncached_input: numeric_property(value, "uncachedInputTokens", "token usage")?,
        output: numeric_property(value, "outputTokens", "token usage")?,
        cache_read: numeric_property(value, "cacheReadTokens", "token usage")?,
        cache_write: numeric_property(value, "cacheWriteTokens", "token usage")?,
    })
}

fn parse_stats(value: &JsValue) -> Result<Stats, JsValue> {
    Ok(Stats {
        turns: numeric_property(value, "turns", "session stats")?,
        steps: numeric_property(value, "steps", "session stats")?,
        llm_ms: numeric_property(value, "llmMs", "session stats")?,
        tool_ms: numeric_property(value, "toolMs", "session stats")?,
        ttft_ms: numeric_property(value, "ttftMs", "session stats")?,
        ttft_steps: numeric_property(value, "ttftSteps", "session stats")?,
        decode_ms: numeric_property(value, "decodeMs", "session stats")?,
        decode_tokens: numeric_property(value, "decodeTokens", "session stats")?,
    })
}

fn stats_object(stats: Stats) -> Result<Object, JsValue> {
    object(&[
        ("turns", JsValue::from_f64(stats.turns)),
        ("steps", JsValue::from_f64(stats.steps)),
        ("llmMs", JsValue::from_f64(stats.llm_ms)),
        ("toolMs", JsValue::from_f64(stats.tool_ms)),
        ("ttftMs", JsValue::from_f64(stats.ttft_ms)),
        ("ttftSteps", JsValue::from_f64(stats.ttft_steps)),
        ("decodeMs", JsValue::from_f64(stats.decode_ms)),
        ("decodeTokens", JsValue::from_f64(stats.decode_tokens)),
    ])
}

fn translate_text(translate: &Function, key: &str, parameters: &Object) -> Result<String, JsValue> {
    translate
        .apply(
            &JsValue::UNDEFINED,
            &Array::of2(&JsValue::from_str(key), parameters.as_ref()),
        )?
        .as_string()
        .ok_or_else(|| {
            js_sys::TypeError::new(&format!("{key} translation must be a string")).into()
        })
}

fn scaled_number(value: f64) -> Result<String, JsValue> {
    number_string(if value >= 100.0 {
        Math::round(value)
    } else {
        Math::round(value * 10.0) / 10.0
    })
}

fn number_string(value: f64) -> Result<String, JsValue> {
    Number::from(value)
        .to_string_with_radix(10)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("Number.toString() returned a non-string").into())
}

fn javascript_number(value: &JsValue) -> Result<f64, JsValue> {
    required_function(&js_sys::global(), "Number", "global")?
        .call1(&JsValue::UNDEFINED, value)?
        .as_f64()
        .ok_or_else(|| js_sys::TypeError::new("Number() returned a non-number").into())
}

fn numeric_property(value: &JsValue, key: &str, owner: &str) -> Result<f64, JsValue> {
    required_property(value, key, owner)?
        .as_f64()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be a number")).into())
}

fn required_function(value: &JsValue, key: &str, owner: &str) -> Result<Function, JsValue> {
    required_property(value, key, owner)?.dyn_into()
}

fn required_property(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_null() || property.is_undefined() {
        Err(js_sys::Error::new(&format!("{owner} omitted {key}")).into())
    } else {
        Ok(property)
    }
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let object = Object::new();
    for (key, value) in entries {
        Reflect::set(&object, &JsValue::from_str(key), value)?;
    }
    Ok(object)
}

fn create_element(
    react: &JsValue,
    kind: &JsValue,
    props: Option<&Object>,
    children: &[JsValue],
) -> Result<JsValue, JsValue> {
    let arguments = Array::new();
    arguments.push(kind);
    arguments.push(props.map_or(&JsValue::NULL, AsRef::as_ref));
    for child in children {
        arguments.push(child);
    }
    required_function(react, "createElement", "React")?.apply(react, &arguments)
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let arguments: Array = arguments.iter().collect();
    method.apply(value, &arguments)
}
