//! Per-session browser trigger orchestration over the portable detector and reducer.

use std::{
    cell::RefCell,
    collections::BTreeMap,
    rc::{Rc, Weak},
};

use js_sys::{Array, Function, Map, Object, Promise, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, future_to_promise, spawn_local};

use super::{
    call_method, log_error, object, optional, required, required_function,
    service::{BrowserInputTriggerService, BrowserSource},
    set,
};
use crate::{
    InputTriggerCandidate, MenuEvent, MenuGroupStatus, MenuState, MoveDirection, TokenSpan,
    TriggerChar, TriggerGuard, TriggerHit, TriggerPosition, detect_trigger, menu_reduce,
    seed_groups,
};

type Listener = Rc<dyn Fn()>;

struct ObservableInner<T> {
    snapshot: Rc<T>,
    listeners: BTreeMap<u64, Listener>,
    next_listener: u64,
}

pub(crate) struct Observable<T> {
    inner: RefCell<ObservableInner<T>>,
}

impl<T: Clone + 'static> Observable<T> {
    fn new(value: T) -> Rc<Self> {
        Rc::new(Self {
            inner: RefCell::new(ObservableInner {
                snapshot: Rc::new(value),
                listeners: BTreeMap::new(),
                next_listener: 0,
            }),
        })
    }

    fn snapshot(&self) -> Rc<T> {
        self.inner.borrow().snapshot.clone()
    }

    fn set(&self, value: T) {
        let listeners = {
            let mut inner = self.inner.borrow_mut();
            inner.snapshot = Rc::new(value);
            inner.listeners.values().cloned().collect::<Vec<_>>()
        };
        for listener in listeners {
            listener();
        }
    }

    fn subscribe(self: &Rc<Self>, listener: Listener) -> ObservableSubscription<T> {
        let id = {
            let mut inner = self.inner.borrow_mut();
            inner.next_listener = inner
                .next_listener
                .checked_add(1)
                .expect("input-trigger Store listener id exhausted");
            let id = inner.next_listener;
            inner.listeners.insert(id, listener);
            id
        };
        ObservableSubscription {
            observable: Rc::downgrade(self),
            id: Some(id),
        }
    }

    fn unsubscribe(&self, id: u64) {
        self.inner.borrow_mut().listeners.remove(&id);
    }
}

struct ObservableSubscription<T: Clone + 'static> {
    observable: Weak<Observable<T>>,
    id: Option<u64>,
}

impl<T: Clone + 'static> Drop for ObservableSubscription<T> {
    fn drop(&mut self) {
        let Some(id) = self.id.take() else {
            return;
        };
        if let Some(observable) = self.observable.upgrade() {
            observable.unsubscribe(id);
        }
    }
}

struct ControllerState {
    hit: Option<TriggerHit>,
    fetch: Option<JsValue>,
    disposed: bool,
    lexicon_offs: BTreeMap<u64, Function>,
}

pub(crate) struct BrowserInputTriggerController {
    actx: JsValue,
    session_id: String,
    service: Weak<BrowserInputTriggerService>,
    pub(crate) menu: Rc<Observable<MenuState>>,
    launcher: Rc<Observable<Option<String>>>,
    lexicon: Rc<Observable<BTreeMap<TriggerChar, Vec<String>>>>,
    state: RefCell<ControllerState>,
    face_cache: RefCell<Option<JsValue>>,
}

impl BrowserInputTriggerController {
    pub(crate) fn new(
        actx: JsValue,
        session_id: String,
        service: Weak<BrowserInputTriggerService>,
    ) -> Result<Rc<Self>, JsValue> {
        let controller = Rc::new(Self {
            actx,
            session_id,
            service,
            menu: Observable::new(MenuState::default()),
            launcher: Observable::new(None),
            lexicon: Observable::new(BTreeMap::new()),
            state: RefCell::new(ControllerState {
                hit: None,
                fetch: None,
                disposed: false,
                lexicon_offs: BTreeMap::new(),
            }),
            face_cache: RefCell::new(None),
        });
        let sources = controller
            .service
            .upgrade()
            .map_or_else(Vec::new, |service| service.all());
        for source in sources {
            controller.source_added(&source)?;
        }
        controller.refresh_lexicon();
        Ok(controller)
    }

    pub(crate) fn face(self: &Rc<Self>) -> Result<JsValue, JsValue> {
        if let Some(face) = self.face_cache.borrow().as_ref() {
            return Ok(face.clone());
        }
        let face: JsValue = WasmInputTriggerController::from_inner(self.clone())?.into();
        *self.face_cache.borrow_mut() = Some(face.clone());
        Ok(face)
    }

    pub(crate) fn source_added(self: &Rc<Self>, source: &BrowserSource) -> Result<(), JsValue> {
        let projection = self.project()?;
        if let Some(warm) = optional_function(&source.value, "warm")? {
            warm.call1(&source.value, &projection)?;
        }
        if optional_function(&source.value, "lexicon")?.is_some()
            && let Some(subscribe) = optional_function(&source.value, "subscribeLexicon")?
        {
            let weak = Rc::downgrade(self);
            let listener = Closure::wrap(Box::new(move || {
                if let Some(controller) = weak.upgrade() {
                    controller.refresh_lexicon();
                }
            }) as Box<dyn FnMut()>);
            let off = subscribe
                .call2(&source.value, &projection, &listener.into_js_value())?
                .dyn_into::<Function>()?;
            self.state.borrow_mut().lexicon_offs.insert(source.id, off);
        }
        self.refresh_lexicon();
        Ok(())
    }

    pub(crate) fn source_removed(&self, source: &BrowserSource) {
        let menu = self.menu.snapshot();
        if menu.open
            && menu
                .hit
                .as_ref()
                .is_some_and(|hit| hit.trigger == source.trigger)
        {
            self.reduce(MenuEvent::SourceFailed {
                generation: menu.generation,
                source: source.name.clone(),
            });
        }
        if let Some(off) = self.state.borrow_mut().lexicon_offs.remove(&source.id) {
            let _ = off.call0(&JsValue::UNDEFINED);
        }
        self.refresh_lexicon();
    }

    pub(crate) fn dispose(&self) {
        {
            let mut state = self.state.borrow_mut();
            state.disposed = true;
        }
        self.stop_fetch();
        self.reduce(MenuEvent::Close);
        let offs = std::mem::take(&mut self.state.borrow_mut().lexicon_offs);
        for off in offs.into_values() {
            let _ = off.call0(&JsValue::UNDEFINED);
        }
        self.state.borrow_mut().hit = None;
        self.face_cache.borrow_mut().take();
    }

    fn track(self: &Rc<Self>, draft: &str, caret: usize, guard: TriggerGuard, draft_rev: u64) {
        if self.state.borrow().disposed {
            return;
        }
        let launched = self.launcher.snapshot().is_some();
        self.clear_launcher();
        let Some(mut hit) = detect_trigger(draft, caret, guard) else {
            self.state.borrow_mut().hit = None;
            self.stop_fetch();
            self.reduce(MenuEvent::Close);
            return;
        };
        hit.span.draft_rev = draft_rev;
        let previous = self.menu.snapshot();
        let same = !launched
            && previous.open
            && previous.hit.as_ref().is_some_and(|previous| {
                previous.trigger == hit.trigger
                    && previous.query == hit.query
                    && previous.span.start == hit.span.start
                    && previous.span.end == hit.span.end
            });
        self.state.borrow_mut().hit = Some(hit.clone());
        if same {
            return;
        }
        let roster = self
            .service
            .upgrade()
            .map_or_else(Vec::new, |service| service.sources(hit.trigger));
        if roster.is_empty() {
            self.stop_fetch();
            self.reduce(MenuEvent::Close);
            return;
        }
        if launched
            || !previous.open
            || previous.hit.is_none()
            || previous
                .hit
                .as_ref()
                .is_some_and(|value| value.trigger != hit.trigger)
        {
            self.menu.set(seed_groups(
                &self.menu.snapshot(),
                &roster
                    .iter()
                    .map(|source| source.name.clone())
                    .collect::<Vec<_>>(),
            ));
        }
        self.reduce(MenuEvent::Hit(Some(hit.clone())));
        self.fetch_candidates(&hit, roster);
    }

    fn toggle_source(self: &Rc<Self>, source_name: &str, hit: &TriggerHit) {
        if self.state.borrow().disposed {
            return;
        }
        if self.launcher.snapshot().as_deref() == Some(source_name) && self.menu.snapshot().open {
            self.dismiss();
            return;
        }
        let source = self.service.upgrade().and_then(|service| {
            service
                .sources(hit.trigger)
                .into_iter()
                .find(|source| source.name == source_name)
        });
        let Some(source) = source else {
            self.dismiss();
            return;
        };
        self.stop_fetch();
        self.state.borrow_mut().hit = Some(hit.clone());
        self.launcher.set(Some(source_name.to_owned()));
        self.menu.set(seed_groups(
            &self.menu.snapshot(),
            &[source_name.to_owned()],
        ));
        self.reduce(MenuEvent::Hit(Some(hit.clone())));
        self.fetch_candidates(hit, vec![source]);
    }

    pub(crate) fn pick(&self, source_name: &str, index: usize) -> Result<(), JsValue> {
        let menu = self.menu.snapshot();
        let state = self.state.borrow();
        let Some(hit) = state.hit.clone() else {
            return Ok(());
        };
        if state.disposed || !menu.open {
            return Ok(());
        }
        drop(state);
        let candidate = menu
            .groups
            .iter()
            .find(|group| group.source == source_name && group.status == MenuGroupStatus::Ready)
            .and_then(|group| group.items.get(index))
            .cloned();
        let source = self.service.upgrade().and_then(|service| {
            service
                .sources(hit.trigger)
                .into_iter()
                .find(|source| source.name == source_name)
        });
        let (Some(candidate), Some(source)) = (candidate, source) else {
            return Ok(());
        };
        let input = self.pick_input(&candidate, &hit, "menu")?;
        let outcome = required_function(&source.value, "onPick", "trigger source")?
            .call1(&source.value, &input)?;
        self.stop_fetch();
        self.reduce(MenuEvent::Close);
        self.execute(&outcome, hit.span)?;
        Ok(())
    }

    fn arbitrate(&self, key: &str, composing: bool) -> Result<&'static str, JsValue> {
        if composing || self.state.borrow().disposed {
            return Ok("pass");
        }
        let menu = self.menu.snapshot();
        if !menu.open {
            return Ok("pass");
        }
        Ok(match key {
            "up" => {
                self.reduce(MenuEvent::Move(MoveDirection::Previous));
                "consumed"
            }
            "down" => {
                self.reduce(MenuEvent::Move(MoveDirection::Next));
                "consumed"
            }
            "escape" => {
                self.stop_fetch();
                self.reduce(MenuEvent::Close);
                "consumed"
            }
            "enter" if menu.highlight.is_some() => {
                let highlight = menu.highlight.as_ref().unwrap();
                self.pick(&highlight.source, highlight.index)?;
                "pick-highlighted"
            }
            _ => "pass",
        })
    }

    fn on_space(&self) -> Result<bool, JsValue> {
        let state = self.state.borrow();
        let Some(hit) = state.hit.clone() else {
            return Ok(false);
        };
        if state.disposed || hit.position != TriggerPosition::Leading {
            return Ok(false);
        }
        drop(state);
        let token = format!("{}{}", hit.trigger.as_char(), hit.query);
        let Ok(projection) = self.project() else {
            return Ok(false);
        };
        let roster = self
            .service
            .upgrade()
            .map_or_else(Vec::new, |service| service.sources(hit.trigger));
        for source in roster {
            let Some(matcher) = optional_function(&source.value, "matchSpace")? else {
                continue;
            };
            let outcome = matcher.call2(&source.value, &projection, &JsValue::from_str(&token))?;
            if outcome.is_undefined() {
                continue;
            }
            if outcome.as_string().as_deref() == Some("handled") {
                return Ok(true);
            }
            return self.execute(&outcome, hit.span);
        }
        Ok(false)
    }

    pub(crate) fn dismiss(&self) {
        if self.state.borrow().disposed {
            return;
        }
        self.stop_fetch();
        self.reduce(MenuEvent::Close);
    }

    fn serialize_reference(&self, source_name: &str, reference: &str, signal: &JsValue) -> Promise {
        let owner = self.service.upgrade().and_then(|service| {
            service
                .all()
                .into_iter()
                .find(|source| source.name == source_name)
        });
        let codec = owner
            .as_ref()
            .and_then(|source| optional(&source.value, "codec").ok().flatten());
        let serializer = codec
            .as_ref()
            .and_then(|codec| optional_function(codec, "serialize").ok().flatten());
        let (Some(codec), Some(serializer)) = (codec, serializer) else {
            return Promise::reject(&js_sys::Error::new(&format!(
                "slash: no serializer for reference source {source_name:?}"
            )));
        };
        match serializer.call2(&codec, &JsValue::from_str(reference), signal) {
            Ok(value) => Promise::resolve(&value),
            Err(error) => Promise::reject(&error),
        }
    }

    fn adjudicate(self: &Rc<Self>, line: String, signal: JsValue) -> Promise {
        let sources = self
            .service
            .upgrade()
            .map_or_else(Vec::new, |service| service.all());
        let projection = self.project();
        future_to_promise(async move {
            let projection = projection?;
            for source in sources {
                if Reflect::get(&signal, &JsValue::from_str("aborted"))?.as_bool() == Some(true) {
                    let reason = Reflect::get(&signal, &JsValue::from_str("reason"))?;
                    return Err(if reason.is_instance_of::<js_sys::Error>() {
                        reason
                    } else {
                        js_sys::Error::new("slash adjudication aborted").into()
                    });
                }
                let Some(matcher) = optional_function(&source.value, "matchEnter")? else {
                    continue;
                };
                if !line.starts_with(source.trigger.as_char()) {
                    continue;
                }
                let returned = matcher.call3(
                    &source.value,
                    &projection,
                    &JsValue::from_str(&line),
                    &signal,
                )?;
                let outcome = JsFuture::from(Promise::resolve(&returned)).await?;
                if !outcome.is_undefined() {
                    return Ok(outcome);
                }
            }
            Ok(JsValue::UNDEFINED)
        })
    }

    fn project(&self) -> Result<JsValue, JsValue> {
        object(&[("sessionId", JsValue::from_str(&self.session_id))]).map(Into::into)
    }

    fn pick_input(
        &self,
        candidate: &InputTriggerCandidate,
        hit: &TriggerHit,
        via: &str,
    ) -> Result<JsValue, JsValue> {
        object(&[
            ("candidate", to_js(candidate)?),
            ("session", self.project()?),
            (
                "position",
                JsValue::from_str(match hit.position {
                    TriggerPosition::Leading => "leading",
                    TriggerPosition::Inline => "inline",
                }),
            ),
            ("via", JsValue::from_str(via)),
            ("span", to_js(&hit.span)?),
        ])
        .map(Into::into)
    }

    fn execute(&self, outcome: &JsValue, span: TokenSpan) -> Result<bool, JsValue> {
        if outcome.is_undefined() || outcome.as_string().as_deref() == Some("handled") {
            return Ok(false);
        }
        let (event, field, value) = if Reflect::has(outcome, &JsValue::from_str("claim"))? {
            (
                "slash/input-begin-command",
                "claim",
                required(outcome, "claim", "pick outcome")?,
            )
        } else if Reflect::has(outcome, &JsValue::from_str("text"))? {
            (
                "slash/input-insert-text",
                "text",
                required(outcome, "text", "pick outcome")?,
            )
        } else {
            (
                "slash/input-insert-reference",
                "reference",
                required(outcome, "insert", "pick outcome")?,
            )
        };
        let payload = object(&[(field, value), ("span", to_js(&span)?)])?;
        Ok(call_method(
            &self.actx,
            "bail",
            &[self.actx.clone(), JsValue::from_str(event), payload.into()],
        )?
        .as_bool()
            == Some(true))
    }

    fn refresh_lexicon(&self) {
        let Ok(projection) = self.project() else {
            return;
        };
        let sources = self
            .service
            .upgrade()
            .map_or_else(Vec::new, |service| service.all());
        let mut rolls = BTreeMap::<TriggerChar, Vec<String>>::new();
        for source in sources {
            let Ok(Some(lexicon)) = optional_function(&source.value, "lexicon") else {
                continue;
            };
            let names = match lexicon.call1(&source.value, &projection) {
                Ok(names) if names.is_undefined() => continue,
                Ok(names) => Array::from(&names)
                    .iter()
                    .filter_map(|name| name.as_string())
                    .collect::<Vec<_>>(),
                Err(error) => {
                    log_error(
                        &format!(
                            "[ui-input-trigger] source {:?}{} lexicon failed:",
                            source.trigger.as_char(),
                            source.name
                        ),
                        &error,
                    );
                    continue;
                }
            };
            rolls.entry(source.trigger).or_default().extend(names);
        }
        self.lexicon.set(rolls);
    }

    fn fetch_candidates(self: &Rc<Self>, hit: &TriggerHit, roster: Vec<BrowserSource>) {
        self.stop_fetch();
        let Ok(controller) = construct("AbortController", &[]) else {
            return;
        };
        self.state.borrow_mut().fetch = Some(controller.clone());
        let generation = self.menu.snapshot().generation;
        let Ok(projection) = self.project() else {
            return;
        };
        let Ok(signal) = required(&controller, "signal", "AbortController") else {
            return;
        };
        for source in roster {
            let Ok(candidates) = required_function(&source.value, "candidates", "trigger source")
            else {
                continue;
            };
            let Ok(request) = object(&[
                ("query", JsValue::from_str(&hit.query)),
                (
                    "position",
                    JsValue::from_str(match hit.position {
                        TriggerPosition::Leading => "leading",
                        TriggerPosition::Inline => "inline",
                    }),
                ),
                ("signal", signal.clone()),
            ]) else {
                continue;
            };
            let returned = match candidates.call2(&source.value, &projection, &request.into()) {
                Ok(value) => value,
                Err(error) => {
                    log_error(
                        &format!(
                            "[ui-input-trigger] source {:?}{} candidates failed:",
                            source.trigger.as_char(),
                            source.name
                        ),
                        &error,
                    );
                    self.reduce(MenuEvent::SourceFailed {
                        generation,
                        source: source.name,
                    });
                    continue;
                }
            };
            let weak = Rc::downgrade(self);
            let source_name = source.name.clone();
            let signal = signal.clone();
            spawn_local(async move {
                let result = JsFuture::from(Promise::resolve(&returned)).await;
                if Reflect::get(&signal, &JsValue::from_str("aborted"))
                    .ok()
                    .and_then(|value| value.as_bool())
                    == Some(true)
                {
                    return;
                }
                let Some(controller) = weak.upgrade() else {
                    return;
                };
                match result {
                    Ok(value) => {
                        match serde_wasm_bindgen::from_value::<Vec<InputTriggerCandidate>>(value) {
                            Ok(items) => controller.reduce(MenuEvent::SourceSettled {
                                generation,
                                source: source_name,
                                items: Some(items),
                            }),
                            Err(_error) => controller.reduce(MenuEvent::SourceFailed {
                                generation,
                                source: source_name,
                            }),
                        }
                    }
                    Err(error) => {
                        log_error(
                            &format!(
                                "[ui-input-trigger] source {source_name:?} candidates failed:"
                            ),
                            &error,
                        );
                        controller.reduce(MenuEvent::SourceFailed {
                            generation,
                            source: source_name,
                        });
                    }
                }
            });
        }
    }

    fn stop_fetch(&self) {
        if let Some(fetch) = self.state.borrow_mut().fetch.take() {
            let _ = call_method(&fetch, "abort", &[]);
        }
    }

    fn clear_launcher(&self) {
        if self.launcher.snapshot().is_some() {
            self.launcher.set(None);
        }
    }

    #[allow(clippy::needless_pass_by_value)]
    fn reduce(&self, event: MenuEvent) {
        let current = self.menu.snapshot();
        if let std::borrow::Cow::Owned(next) = menu_reduce(&current, &event) {
            let open = next.open;
            self.menu.set(next);
            if !open {
                self.clear_launcher();
            }
        }
    }
}

/// Compiled per-session input-trigger controller.
#[wasm_bindgen(js_name = __InputTriggerController)]
pub struct WasmInputTriggerController {
    inner: Rc<BrowserInputTriggerController>,
    menu_face: JsValue,
    launcher_face: JsValue,
    lexicon_face: JsValue,
}

#[wasm_bindgen(js_class = __InputTriggerController)]
impl WasmInputTriggerController {
    /// Menu observable Store.
    #[wasm_bindgen(getter)]
    pub fn menu(&self) -> JsValue {
        self.menu_face.clone()
    }

    /// Programmatic-launcher observable Store.
    #[wasm_bindgen(getter)]
    pub fn launcher(&self) -> JsValue {
        self.launcher_face.clone()
    }

    /// Aggregated lexicon observable Store.
    #[wasm_bindgen(getter)]
    pub fn lexicon(&self) -> JsValue {
        self.lexicon_face.clone()
    }

    /// Tracks one draft/caret/revision state.
    ///
    /// # Errors
    ///
    /// Returns malformed guard input.
    #[allow(clippy::needless_pass_by_value)]
    pub fn track(
        &self,
        draft: String,
        caret: usize,
        guard: JsValue,
        draft_rev: f64,
    ) -> Result<(), JsValue> {
        let guard = serde_wasm_bindgen::from_value(guard)
            .map_err(|error| js_sys::Error::new(&error.to_string()))?;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        self.inner.track(&draft, caret, guard, draft_rev as u64);
        Ok(())
    }

    /// Toggles one programmatic source.
    ///
    /// # Errors
    ///
    /// Returns malformed hit input.
    #[wasm_bindgen(js_name = toggleSource)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn toggle_source(&self, source: String, hit: JsValue) -> Result<(), JsValue> {
        let hit = serde_wasm_bindgen::from_value(hit)
            .map_err(|error| js_sys::Error::new(&error.to_string()))?;
        self.inner.toggle_source(&source, &hit);
        Ok(())
    }

    /// Picks one source-local candidate.
    ///
    /// # Errors
    ///
    /// Returns source callback, outcome, or scoped input-dispatch failures.
    #[allow(clippy::needless_pass_by_value)]
    pub fn pick(&self, source: String, index: usize) -> Result<(), JsValue> {
        self.inner.pick(&source, index)
    }

    /// Arbitrates one menu key.
    ///
    /// # Errors
    ///
    /// Returns highlighted-pick failures.
    #[allow(clippy::needless_pass_by_value)]
    pub fn arbitrate(&self, key: String, composing: bool) -> Result<String, JsValue> {
        self.inner.arbitrate(&key, composing).map(ToOwned::to_owned)
    }

    /// Runs synchronous leading-token Space adjudication.
    ///
    /// # Errors
    ///
    /// Returns source callback, outcome, or scoped input-dispatch failures.
    #[wasm_bindgen(js_name = onSpace)]
    pub fn on_space(&self) -> Result<bool, JsValue> {
        self.inner.on_space()
    }

    /// Serializes one owner-scoped reference.
    #[wasm_bindgen(js_name = serializeReference)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn serialize_reference(
        &self,
        source: String,
        reference: String,
        signal: JsValue,
    ) -> Promise {
        self.inner.serialize_reference(&source, &reference, &signal)
    }

    /// Runs Enter-time source adjudication.
    #[allow(clippy::needless_pass_by_value)]
    pub fn adjudicate(&self, line: String, signal: JsValue) -> Promise {
        self.inner.adjudicate(line, signal)
    }

    /// Dismisses and aborts the current menu fetch.
    pub fn dismiss(&self) {
        self.inner.dismiss();
    }

    /// Tears down this Session controller.
    pub fn dispose(&self) {
        self.inner.dispose();
    }
}

impl WasmInputTriggerController {
    pub(crate) fn from_inner(inner: Rc<BrowserInputTriggerController>) -> Result<Self, JsValue> {
        let menu_face = observable_face(inner.menu.clone(), menu_to_js)?;
        let launcher_face = observable_face(inner.launcher.clone(), |value| {
            Ok(value
                .as_ref()
                .map_or(JsValue::NULL, |value| JsValue::from_str(value)))
        })?;
        let lexicon_face =
            observable_face(inner.lexicon.clone(), |rolls| Ok(lexicon_to_js(rolls)))?;
        Ok(Self {
            inner,
            menu_face,
            launcher_face,
            lexicon_face,
        })
    }
}

fn observable_face<T: Clone + 'static>(
    observable: Rc<Observable<T>>,
    serialize: impl Fn(&T) -> Result<JsValue, JsValue> + 'static,
) -> Result<JsValue, JsValue> {
    let face = Object::new();
    let cache = Rc::new(RefCell::new(None::<(Rc<T>, JsValue)>));
    let get_observable = observable.clone();
    let get_cache = cache;
    let get = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let snapshot = get_observable.snapshot();
        if let Some((cached, value)) = get_cache.borrow().as_ref()
            && Rc::ptr_eq(cached, &snapshot)
        {
            return Ok(value.clone());
        }
        let value = serialize(&snapshot)?;
        *get_cache.borrow_mut() = Some((snapshot, value.clone()));
        Ok(value)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    set(&face, "getSnapshot", &get.into_js_value())?;
    let subscribe_observable = observable;
    let subscribe = Closure::wrap(Box::new(move |listener: Function| -> JsValue {
        let subscription = subscribe_observable.subscribe(Rc::new(move || {
            let _ = listener.call0(&JsValue::UNDEFINED);
        }));
        let subscription = Rc::new(RefCell::new(Some(subscription)));
        Closure::wrap(Box::new(move || {
            subscription.borrow_mut().take();
        }) as Box<dyn FnMut()>)
        .into_js_value()
    }) as Box<dyn FnMut(Function) -> JsValue>);
    set(&face, "subscribe", &subscribe.into_js_value())?;
    Ok(face.into())
}

fn menu_to_js(state: &MenuState) -> Result<JsValue, JsValue> {
    let groups = Array::new();
    for group in &state.groups {
        let items = serde_wasm_bindgen::to_value(&group.items)
            .map_err(|error| js_sys::Error::new(&error.to_string()))?;
        let value: JsValue = object(&[
            ("source", JsValue::from_str(&group.source)),
            (
                "status",
                JsValue::from_str(match group.status {
                    MenuGroupStatus::Pending => "pending",
                    MenuGroupStatus::Ready => "ready",
                }),
            ),
            ("items", items),
        ])?
        .into();
        groups.push(&value);
    }
    let hit = state
        .hit
        .as_ref()
        .map(to_js)
        .transpose()?
        .unwrap_or(JsValue::NULL);
    let highlight = state
        .highlight
        .as_ref()
        .map_or(Ok(JsValue::NULL), |highlight| {
            object(&[
                ("source", JsValue::from_str(&highlight.source)),
                ("index", JsValue::from_f64(usize_as_f64(highlight.index))),
            ])
            .map(Into::into)
        })?;
    object(&[
        ("open", JsValue::from_bool(state.open)),
        ("hit", hit),
        (
            "generation",
            JsValue::from_f64(u64_as_f64(state.generation)),
        ),
        ("groups", groups.into()),
        ("highlight", highlight),
    ])
    .map(Into::into)
}

fn lexicon_to_js(rolls: &BTreeMap<TriggerChar, Vec<String>>) -> JsValue {
    let map = Map::new();
    for (trigger, names) in rolls {
        let values = Array::new();
        for name in names {
            values.push(&JsValue::from_str(name));
        }
        map.set(&JsValue::from_str(&trigger.as_char().to_string()), &values);
    }
    map.into()
}

fn optional_function(value: &JsValue, key: &str) -> Result<Option<Function>, JsValue> {
    optional(value, key)?
        .map(JsValue::dyn_into::<Function>)
        .transpose()
}

fn construct(name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let constructor =
        Reflect::get(&js_sys::global(), &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    Reflect::construct(&constructor, &args)
}

fn usize_as_f64(value: usize) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
        value as f64
    }
}

fn u64_as_f64(value: u64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
        value as f64
    }
}

fn to_js<T: serde::Serialize>(value: &T) -> Result<JsValue, JsValue> {
    value
        .serialize(
            &serde_wasm_bindgen::Serializer::new().serialize_large_number_types_as_bigints(false),
        )
        .map_err(|error| js_sys::Error::new(&error.to_string()).into())
}
