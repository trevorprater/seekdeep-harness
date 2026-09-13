use std::{cell::RefCell, rc::Rc};

use js_sys::{Array, Reflect};
use wasm_bindgen::{JsCast as _, prelude::*};
use web_sys::{Element, Event, Window};

/// Builds the edit URL from canonical page frontmatter.
///
/// # Errors
/// Propagates property getters and rejects missing or non-string `editSource` metadata.
#[wasm_bindgen(js_name = editLink)]
pub fn edit_link(page: &JsValue, branch: &str) -> Result<String, JsValue> {
    let frontmatter = Reflect::get(page, &JsValue::from_str("frontmatter"))?;
    let source = if frontmatter.is_object() {
        Reflect::get(&frontmatter, &JsValue::from_str("editSource"))?.as_string()
    } else {
        None
    };
    let source = source.ok_or_else(|| {
        js_sys::Error::new("Projected documentation page has no editSource frontmatter.")
    })?;
    Ok(format!(
        "https://github.com/trevorprater/seekdeep-harness/edit/{branch}/{source}"
    ))
}

/// Produces the serializable `VitePress` callback that delegates to the initialized runtime.
///
/// # Errors
/// Propagates JavaScript string-serialization failures.
#[wasm_bindgen(js_name = editLinkPattern)]
pub fn edit_link_pattern(branch: &str) -> Result<js_sys::Function, JsValue> {
    let branch = String::from(js_sys::JSON::stringify(&JsValue::from_str(branch))?);
    Ok(js_sys::Function::new_with_args(
        "page",
        &format!("return globalThis.__seekdeepDocsRuntime.editLink(page, {branch});"),
    ))
}

/// Checks that `VitePress` exposes both rendering rules before wrapping either.
///
/// # Errors
/// Rejects an undefined text or inline-code renderer.
#[wasm_bindgen(js_name = validateMarkdownRules)]
pub fn validate_markdown_rules(text: &JsValue, code: &JsValue) -> Result<(), JsValue> {
    if text.is_undefined() || code.is_undefined() {
        return Err(js_sys::Error::new(
            "VitePress Markdown renderer is missing its text or inline-code rule.",
        )
        .into());
    }
    Ok(())
}

/// Selects exactly the canonical files captured by the projection watcher.
#[wasm_bindgen(js_name = shouldProject)]
pub fn should_project(sources: &Array, changed: &JsValue) -> bool {
    sources.includes(changed, 0)
}

struct IdleTimeout {
    window: Window,
    id: i32,
    _callback: Closure<dyn FnMut()>,
}

impl Drop for IdleTimeout {
    fn drop(&mut self) {
        self.window.clear_timeout_with_handle(self.id);
    }
}

type Idle = Rc<RefCell<Option<IdleTimeout>>>;

/// Owns the capturing sidebar listener and its single idle timeout.
#[wasm_bindgen]
pub struct SidebarScrollbar {
    window: Window,
    listener: Closure<dyn FnMut(Event)>,
    idle: Idle,
}

#[wasm_bindgen]
impl SidebarScrollbar {
    /// Installs the source's 800 ms sidebar-scroll marker.
    ///
    /// # Errors
    /// Rejects non-browser environments and propagates listener registration failures.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Result<SidebarScrollbar, JsValue> {
        let window = web_sys::window().ok_or_else(|| {
            js_sys::Error::new("Documentation scrollbar requires a browser window.")
        })?;
        let idle = Rc::new(RefCell::new(None));
        let callback_window = window.clone();
        let callback_idle = idle.clone();
        let listener = Closure::new(move |event: Event| {
            if let Err(error) = scroll(&callback_window, &callback_idle, &event) {
                wasm_bindgen::throw_val(error);
            }
        });
        window.add_event_listener_with_callback_and_bool(
            "scroll",
            listener.as_ref().unchecked_ref(),
            true,
        )?;
        Ok(Self {
            window,
            listener,
            idle,
        })
    }
}

impl Drop for SidebarScrollbar {
    fn drop(&mut self) {
        let _ = self.window.remove_event_listener_with_callback_and_bool(
            "scroll",
            self.listener.as_ref().unchecked_ref(),
            true,
        );
        self.idle.borrow_mut().take();
    }
}

fn scroll(window: &Window, idle: &Idle, event: &Event) -> Result<(), JsValue> {
    let Some(target) = event
        .target()
        .and_then(|target| target.dyn_into::<Element>().ok())
    else {
        return Ok(());
    };
    if !target.class_list().contains("VPSidebar") {
        return Ok(());
    }
    target.set_attribute("data-scrolling", "")?;
    idle.borrow_mut().take();
    let callback = Closure::once(move || {
        if let Err(error) = target.remove_attribute("data-scrolling") {
            wasm_bindgen::throw_val(error);
        }
    });
    let id = window.set_timeout_with_callback_and_timeout_and_arguments_0(
        callback.as_ref().unchecked_ref(),
        800,
    )?;
    *idle.borrow_mut() = Some(IdleTimeout {
        window: window.clone(),
        id,
        _callback: callback,
    });
    Ok(())
}
