//! Rust-owned syntax-highlighting policy over an injected Shiki tokenizer capability.

use std::{cell::RefCell, collections::BTreeSet};

use js_sys::{Array, Function, Object, Promise, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

const BOOT_GRAMMARS: &[&str] = &["typescript", "shellscript", "json"];
const LAZY_GRAMMARS: &[&str] = &[
    "python", "ruby", "go", "rust", "java", "c", "cpp", "csharp", "kotlin", "swift", "php", "yaml",
    "toml", "ini", "markdown", "mdx", "html", "css", "scss", "less", "sql", "xml", "lua",
];

thread_local! {
    static STATE: RefCell<Option<HighlightState>> = const { RefCell::new(None) };
}

struct HighlightState {
    backend: JsValue,
    requested: BTreeSet<String>,
    loaded: BTreeSet<String>,
    listeners: Vec<HighlightListener>,
    next_listener_id: u64,
    load_count: u32,
}

#[derive(Clone)]
struct HighlightListener {
    id: u64,
    function: Function,
}

/// Configures the external Shiki tokenizer while keeping all Harness policy in Rust.
///
/// The backend supplies `warm()`, `loadGrammar(id)`, `codeToHtml(code, id)`, and
/// `codeToTokens(code, id)`. Rust owns aliases, lazy admission, notification,
/// trailing-line normalization, and public fallback behavior.
///
/// # Errors
///
/// Returns missing backend methods or warmup-timer failures.
#[wasm_bindgen(js_name = configureClientUiPrimitiveHighlight)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_primitive_highlight(backend: JsValue) -> Result<(), JsValue> {
    for method in ["warm", "loadGrammar", "codeToHtml", "codeToTokens"] {
        required_function(&backend, method, "highlight backend")?;
    }
    STATE.with(|state| {
        *state.borrow_mut() = Some(HighlightState {
            backend: backend.clone(),
            requested: BTreeSet::new(),
            loaded: BOOT_GRAMMARS
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            listeners: Vec::new(),
            next_listener_id: 1,
            load_count: 0,
        });
    });
    let warm_backend = backend;
    let warm = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        call_method(&warm_backend, "warm", &[]).map(|_| ())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let global = js_sys::global();
    let handle = required_function(&global, "setTimeout", "global")?.call2(
        &global,
        &warm.into_js_value(),
        &JsValue::from_f64(0.0),
    )?;
    if let Ok(unref) = Reflect::get(&handle, &JsValue::from_str("unref"))
        && unref.is_function()
    {
        unref.dyn_into::<Function>()?.call0(&handle).map(|_| ())?;
    }
    Ok(())
}

/// Highlights source into Shiki's trusted HTML, or returns `undefined` for a fallback.
///
/// # Errors
///
/// Returns configuration or tokenizer failures.
#[wasm_bindgen(js_name = highlightToHtml)]
#[allow(clippy::needless_pass_by_value)]
pub fn highlight_to_html(code: String, lang: Option<String>) -> Result<JsValue, JsValue> {
    let Some(resolved) = resolve_language(lang.as_deref()) else {
        return Ok(JsValue::UNDEFINED);
    };
    if !ensure_grammar(resolved)? {
        return Ok(JsValue::UNDEFINED);
    }
    with_state(|state| {
        call_method(
            &state.backend,
            "codeToHtml",
            &[JsValue::from_str(&code), JsValue::from_str(resolved)],
        )
    })
}

/// Highlights source into per-line token runs, or returns `undefined` for a fallback.
///
/// # Errors
///
/// Returns configuration, tokenizer, or malformed-token failures.
#[wasm_bindgen(js_name = highlightLines)]
#[allow(clippy::needless_pass_by_value)]
pub fn highlight_lines(code: String, lang: Option<String>) -> Result<JsValue, JsValue> {
    let Some(resolved) = resolve_language(lang.as_deref()) else {
        return Ok(JsValue::UNDEFINED);
    };
    if !ensure_grammar(resolved)? {
        return Ok(JsValue::UNDEFINED);
    }
    let token_result = with_state(|state| {
        call_method(
            &state.backend,
            "codeToTokens",
            &[JsValue::from_str(&code), JsValue::from_str(resolved)],
        )
    })?;
    let tokens = required_array(&token_result, "tokens", "codeToTokens result")?;
    let retained =
        if tokens.length() > 1 && Array::from(&tokens.get(tokens.length() - 1)).length() == 0 {
            tokens.slice(0, tokens.length() - 1)
        } else {
            tokens
        };
    let lines = Array::new();
    for line in retained.iter() {
        let runs = Array::new();
        for token in Array::from(&line).iter() {
            let content = required_string(&token, "content", "highlight token")?;
            let color = required_string(&token, "color", "highlight token")?;
            runs.push(
                &object(&[
                    ("text", JsValue::from_str(&content)),
                    (
                        "style",
                        object(&[("color", JsValue::from_str(&color))])?.into(),
                    ),
                ])?
                .into(),
            );
        }
        lines.push(&runs);
    }
    Ok(lines.into())
}

/// Subscribes to successful lazy grammar registration.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = subscribeGrammarLoaded)]
pub fn subscribe_grammar_loaded(listener: Function) -> Result<Function, JsValue> {
    with_state_mut(|state| {
        if !state
            .listeners
            .iter()
            .any(|current| Object::is(&current.function, &listener))
        {
            let id = state.next_listener_id;
            state.next_listener_id = state
                .next_listener_id
                .checked_add(1)
                .ok_or_else(|| js_sys::Error::new("highlight listener id space exhausted"))?;
            state.listeners.push(HighlightListener {
                id,
                function: listener.clone(),
            });
        }
        Ok(())
    })?;
    Ok(Closure::wrap(Box::new(move || {
        let _ = with_state_mut(|state| {
            state
                .listeners
                .retain(|current| !Object::is(&current.function, &listener));
            Ok(())
        });
    }) as Box<dyn FnMut()>)
    .into_js_value()
    .unchecked_into())
}

/// Returns the opaque lazy-grammar load counter.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = grammarLoadCount)]
pub fn grammar_load_count() -> Result<u32, JsValue> {
    with_state(|state| Ok(state.load_count))
}

/// Returns the exact grammar aliases owned by the compiled policy.
#[wasm_bindgen(js_name = highlightAliases)]
pub fn highlight_aliases() -> Object {
    let output = Object::new();
    for (alias, resolved) in aliases() {
        let _ = Reflect::set(
            &output,
            &JsValue::from_str(alias),
            &JsValue::from_str(resolved),
        );
    }
    output
}

/// Returns lazy grammar ids in source order for the ESM backend's import table.
#[wasm_bindgen(js_name = lazyGrammarIds)]
pub fn lazy_grammar_ids() -> Array {
    LAZY_GRAMMARS
        .iter()
        .map(|value| JsValue::from_str(value))
        .collect()
}

pub(crate) fn highlight_store_faces() -> Result<(Function, Function), JsValue> {
    let subscribe =
        Closure::wrap(
            Box::new(move |listener: Function| subscribe_grammar_loaded(listener))
                as Box<dyn FnMut(Function) -> Result<Function, JsValue>>,
        )
        .into_js_value()
        .dyn_into::<Function>()?;
    let snapshot =
        Closure::wrap(Box::new(grammar_load_count) as Box<dyn FnMut() -> Result<u32, JsValue>>)
            .into_js_value()
            .dyn_into::<Function>()?;
    Ok((subscribe, snapshot))
}

fn ensure_grammar(resolved: &'static str) -> Result<bool, JsValue> {
    if !LAZY_GRAMMARS.contains(&resolved) {
        return Ok(true);
    }
    let backend = with_state(|state| Ok(state.backend.clone()))?;
    call_method(&backend, "warm", &[])?;
    let mut start = None;
    let ready = with_state_mut(|state| {
        if state.loaded.contains(resolved) {
            return Ok(true);
        }
        if state.requested.insert(resolved.to_owned()) {
            start = Some(state.backend.clone());
        }
        Ok(false)
    })?;
    if let Some(backend) = start {
        let pending = call_method(&backend, "loadGrammar", &[JsValue::from_str(resolved)])?;
        let resolved = resolved.to_owned();
        let success = Closure::wrap(Box::new(move |_value: JsValue| -> Result<(), JsValue> {
            let loaded = with_state_mut(|state| {
                if state.loaded.insert(resolved.clone()) {
                    state.load_count = state.load_count.saturating_add(1);
                    return Ok(true);
                }
                Ok(false)
            })?;
            if loaded {
                notify_listeners()?;
            }
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
        let promise = Promise::resolve(&pending);
        call_method(promise.as_ref(), "then", &[success.into_js_value()])?;
    }
    Ok(ready)
}

fn notify_listeners() -> Result<(), JsValue> {
    let mut cursor = 0_u64;
    loop {
        let next = with_state(|state| {
            Ok(state
                .listeners
                .iter()
                .find(|listener| listener.id > cursor)
                .cloned())
        })?;
        let Some(listener) = next else {
            return Ok(());
        };
        cursor = listener.id;
        listener.function.call0(&JsValue::UNDEFINED)?;
    }
}

fn resolve_language(lang: Option<&str>) -> Option<&'static str> {
    let lang = lang?.to_lowercase();
    aliases()
        .iter()
        .find_map(|(alias, resolved)| (*alias == lang).then_some(*resolved))
}

fn aliases() -> &'static [(&'static str, &'static str)] {
    &[
        ("typescript", "typescript"),
        ("ts", "typescript"),
        ("tsx", "typescript"),
        ("javascript", "typescript"),
        ("js", "typescript"),
        ("jsx", "typescript"),
        ("shellscript", "shellscript"),
        ("bash", "shellscript"),
        ("sh", "shellscript"),
        ("shell", "shellscript"),
        ("zsh", "shellscript"),
        ("json", "json"),
        ("jsonc", "json"),
        ("py", "python"),
        ("python", "python"),
        ("rb", "ruby"),
        ("ruby", "ruby"),
        ("go", "go"),
        ("rs", "rust"),
        ("rust", "rust"),
        ("java", "java"),
        ("c", "c"),
        ("cpp", "cpp"),
        ("cs", "csharp"),
        ("csharp", "csharp"),
        ("kotlin", "kotlin"),
        ("swift", "swift"),
        ("php", "php"),
        ("yaml", "yaml"),
        ("yml", "yaml"),
        ("toml", "toml"),
        ("ini", "ini"),
        ("md", "markdown"),
        ("markdown", "markdown"),
        ("mdx", "mdx"),
        ("html", "html"),
        ("css", "css"),
        ("scss", "scss"),
        ("less", "less"),
        ("sql", "sql"),
        ("xml", "xml"),
        ("lua", "lua"),
    ]
}

fn with_state<T>(
    callback: impl FnOnce(&HighlightState) -> Result<T, JsValue>,
) -> Result<T, JsValue> {
    STATE.with(|state| {
        let state = state.borrow();
        callback(state.as_ref().ok_or_else(|| {
            js_sys::Error::new("client-ui-primitives highlighter is not configured")
        })?)
    })
}

fn with_state_mut<T>(
    callback: impl FnOnce(&mut HighlightState) -> Result<T, JsValue>,
) -> Result<T, JsValue> {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        callback(state.as_mut().ok_or_else(|| {
            js_sys::Error::new("client-ui-primitives highlighter is not configured")
        })?)
    })
}

fn required_array(value: &JsValue, key: &str, owner: &str) -> Result<Array, JsValue> {
    let value = required_property(value, key, owner)?;
    if Array::is_array(&value) {
        Ok(Array::from(&value))
    } else {
        Err(js_sys::TypeError::new(&format!("{owner} {key} must be an array")).into())
    }
}

fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    required_property(value, key, owner)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be a string")).into())
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

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let arguments: Array = arguments.iter().collect();
    method.apply(value, &arguments)
}
