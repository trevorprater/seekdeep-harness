//! Compiled browser `SessionInputShell` over the portable reducer.

use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};

use js_sys::{Array, Function, Object, Promise, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, spawn_local};

use crate::{
    ArbitrateKey, BusyEnterBehavior, CommandSubmitId, CommandSubmitOutcome,
    CommandSubmitOutcomeKind, DraftAttachmentId, DraftRevision, EditRange, EditSelection,
    InputCommandClaim, InputMachine, InputMachineEffect, InputMachineEvent, InputMachineOptions,
    InputMachineState, InputNoticeLevel, InputOccurrence, InputPickOutcome, InputReferenceInsert,
    InputTokenSpan, PasteComponent, SubmitAttempt, SubmitAttemptId, input_text_len,
    slice_input_text, splice_input_text, trim_input_text,
};

thread_local! {
    static EMPTY_QUEUE: JsValue = Array::new().into();
    static EMPTY_LEXICON: JsValue = js_sys::Map::new().into();
}

struct OccurrencesProjection {
    source: Rc<Vec<InputOccurrence>>,
    value: JsValue,
}

struct ShellInner {
    deps: JsValue,
    machine: RefCell<InputMachine>,
    state: JsValue,
    notices: JsValue,
    image_ids: RefCell<Vec<DraftAttachmentId>>,
    image_values: RefCell<JsValue>,
    occurrences: RefCell<OccurrencesProjection>,
    notice_seq: Cell<u64>,
    last_draft: RefCell<String>,
    disposed: Cell<bool>,
    mirror: RefCell<Option<Function>>,
    claim_seq: Cell<u64>,
    claims: RefCell<BTreeMap<CommandSubmitId, JsValue>>,
    controllers: RefCell<BTreeMap<SubmitAttemptId, JsValue>>,
    actions: RefCell<Option<JsValue>>,
    lexicon: RefCell<Option<JsValue>>,
    empty_lexicon: JsValue,
}

/// Browser session input facade and effect executor.
#[wasm_bindgen(js_name = SessionInputShell)]
pub struct BrowserSessionInputShell {
    inner: Rc<ShellInner>,
}

#[wasm_bindgen(js_class = SessionInputShell)]
#[allow(clippy::needless_pass_by_value)] // JavaScript class methods own their ABI arguments.
impl BrowserSessionInputShell {
    /// Creates one isolated shell.
    ///
    /// # Errors
    ///
    /// Returns for malformed dependencies, queue faces, or snapshot-store construction.
    #[wasm_bindgen(constructor)]
    pub fn new(deps: JsValue) -> Result<Self, JsValue> {
        let machine = InputMachine::new(InputMachineOptions {
            merge_window_ms: 1_000.0,
            now: Rc::new(js_sys::Date::now),
        });
        let image_ids = Vec::new();
        let machine_state = machine.state();
        let image_values: JsValue = Array::new().into();
        let occurrence_values: JsValue = occurrences_value(&machine_state.occurrences)?.into();
        let initial = compose_value(&machine_state, &image_values, &occurrence_values, &deps)?;
        let state = seekdeep_client_runtime::create_snapshot_store(initial, JsValue::UNDEFINED)?;
        let notices =
            seekdeep_client_runtime::create_snapshot_store(JsValue::NULL, JsValue::UNDEFINED)?;
        let inner = Rc::new(ShellInner {
            deps,
            machine: RefCell::new(machine),
            state,
            notices,
            image_ids: RefCell::new(image_ids),
            image_values: RefCell::new(image_values),
            occurrences: RefCell::new(OccurrencesProjection {
                source: machine_state.occurrences,
                value: occurrence_values,
            }),
            notice_seq: Cell::new(0),
            last_draft: RefCell::new(String::new()),
            disposed: Cell::new(false),
            mirror: RefCell::new(None),
            claim_seq: Cell::new(0),
            claims: RefCell::new(BTreeMap::new()),
            controllers: RefCell::new(BTreeMap::new()),
            actions: RefCell::new(None),
            lexicon: RefCell::new(None),
            empty_lexicon: EMPTY_LEXICON.with(Clone::clone),
        });
        subscribe_queue(&inner)?;
        Ok(Self { inner })
    }

    /// Published machine state plus queue/image overlays.
    #[wasm_bindgen(getter)]
    pub fn state(&self) -> JsValue {
        self.inner.state.clone()
    }

    /// Latest external/machine notice store.
    #[wasm_bindgen(getter)]
    pub fn notices(&self) -> JsValue {
        self.inner.notices.clone()
    }

    /// Stable public actions face.
    ///
    /// # Errors
    ///
    /// Returns if face construction fails.
    #[wasm_bindgen(getter)]
    pub fn actions(&self) -> Result<JsValue, JsValue> {
        self.inner.actions_face()
    }

    /// Stable hot lexicon observable face.
    ///
    /// # Errors
    ///
    /// Returns if face construction fails.
    #[wasm_bindgen(getter)]
    pub fn lexicon(&self) -> Result<JsValue, JsValue> {
        self.inner.lexicon_face()
    }

    /// Returns the live composed snapshot.
    ///
    /// # Errors
    ///
    /// Returns if the state store is malformed.
    #[wasm_bindgen(getter)]
    pub fn snapshot(&self) -> Result<JsValue, JsValue> {
        required_function(&self.inner.state, "getSnapshot", "input state store")?
            .call0(&self.inner.state)
    }

    /// Writes the full next draft.
    ///
    /// # Errors
    ///
    /// Returns for malformed edit ranges or state publication failures.
    #[wasm_bindgen(js_name = setDraft)]
    pub fn set_draft(&self, text: String, edit_range: JsValue) -> Result<(), JsValue> {
        self.inner.run(InputMachineEvent::DraftChanged {
            draft: text,
            edit_range: parse_optional_edit_range(&edit_range)?,
        })
    }

    /// Appends ordered image ids unless admission is locked.
    ///
    /// # Errors
    ///
    /// Returns for malformed id arrays or publication failures.
    #[wasm_bindgen(js_name = addImages)]
    pub fn add_images(&self, ids: JsValue) -> Result<bool, JsValue> {
        self.inner.add_images(parse_attachment_ids(&ids)?)
    }

    /// Removes one image id.
    ///
    /// # Errors
    ///
    /// Returns for invalid ids or publication failures.
    #[wasm_bindgen(js_name = removeImage)]
    pub fn remove_image(&self, id: String) -> Result<(), JsValue> {
        self.inner.remove_image(&DraftAttachmentId::new(id))
    }

    /// Keeps only ids still owned by the browser registry.
    ///
    /// # Errors
    ///
    /// Returns for malformed arrays or publication failures.
    #[wasm_bindgen(js_name = pruneImages)]
    pub fn prune_images(&self, ids: JsValue) -> Result<(), JsValue> {
        self.inner.prune_images(&parse_attachment_ids(&ids)?)
    }

    /// Restores failed-attempt images before newer ids.
    ///
    /// # Errors
    ///
    /// Returns for malformed arrays or publication failures.
    #[wasm_bindgen(js_name = restoreImages)]
    pub fn restore_images(&self, ids: JsValue) -> Result<(), JsValue> {
        self.inner.restore_images(&parse_attachment_ids(&ids)?)
    }

    /// Commits a successful ordinary send.
    ///
    /// # Errors
    ///
    /// Returns for malformed arrays or publication failures.
    #[wasm_bindgen(js_name = commitSend)]
    pub fn commit_send(&self, ids: JsValue) -> Result<(), JsValue> {
        self.inner.commit_send(&parse_attachment_ids(&ids)?)
    }

    /// Undoes one reducer transaction.
    ///
    /// # Errors
    ///
    /// Returns if publication fails.
    pub fn undo(&self) -> Result<(), JsValue> {
        self.inner.run(InputMachineEvent::Undo)
    }

    /// Redoes one reducer transaction.
    ///
    /// # Errors
    ///
    /// Returns if publication fails.
    pub fn redo(&self) -> Result<(), JsValue> {
        self.inner.run(InputMachineEvent::Redo)
    }

    /// Begins one paste transaction.
    ///
    /// # Errors
    ///
    /// Returns for malformed selections/components or publication failures.
    #[wasm_bindgen(js_name = pasteBegin)]
    pub fn paste_begin(
        &self,
        text: String,
        selection: JsValue,
        components: JsValue,
        generation: JsValue,
    ) -> Result<(), JsValue> {
        self.inner.run(InputMachineEvent::PasteBegin {
            text,
            selection: parse_selection(&selection, "paste selection")?,
            components: parse_paste_components(&components)?,
            generation: if generation.is_undefined() {
                0
            } else {
                number_to_u64(javascript_number(&generation)?, "paste generation")?
            },
        })
    }

    /// Invalidates the live paste attempt.
    ///
    /// # Errors
    ///
    /// Returns if publication fails.
    #[wasm_bindgen(js_name = invalidatePaste)]
    pub fn invalidate_paste(&self) -> Result<(), JsValue> {
        self.inner.run(InputMachineEvent::InvalidatePaste)
    }

    /// Enters submission with queue default.
    ///
    /// # Errors
    ///
    /// Returns for invalid modes or effect/publication failures.
    pub fn submit(&self, mode: Option<String>) -> Result<(), JsValue> {
        self.inner
            .submit(parse_mode(mode.as_deref().unwrap_or("queue"))?)
    }

    /// Tracks draft/caret through the optional trigger controller.
    ///
    /// # Errors
    ///
    /// Returns for malformed controller faces.
    pub fn track(&self, draft: String, caret: f64) -> Result<(), JsValue> {
        self.inner.track(&draft, caret)
    }

    /// Arbitrates one popup key.
    ///
    /// # Errors
    ///
    /// Returns for invalid keys or malformed controller faces.
    pub fn arbitrate(&self, key: String, composing: bool) -> Result<String, JsValue> {
        self.inner.arbitrate(&key, composing)
    }

    /// Triggers whole-queue steering when supported.
    ///
    /// # Errors
    ///
    /// Returns if the configured callback throws.
    #[wasm_bindgen(js_name = steerQueue)]
    pub fn steer_queue(&self) -> Result<(), JsValue> {
        self.inner.call_optional_dep("steerQueue", &[])?;
        Ok(())
    }

    /// Runs synchronous Space adjudication.
    ///
    /// # Errors
    ///
    /// Returns for malformed controller faces.
    pub fn space(&self) -> Result<bool, JsValue> {
        self.inner.space()
    }

    /// Dismisses the popup shell.
    ///
    /// # Errors
    ///
    /// Returns for malformed popup faces.
    #[wasm_bindgen(js_name = dismissPopup)]
    pub fn dismiss_popup(&self) -> Result<(), JsValue> {
        self.inner.dismiss_popup()
    }

    /// Applies one command claim after span CAS.
    ///
    /// # Errors
    ///
    /// Returns for malformed claims/spans or publication failures.
    #[wasm_bindgen(js_name = beginCommand)]
    pub fn begin_command(&self, claim: JsValue, span: JsValue) -> Result<bool, JsValue> {
        self.inner.begin_command(&claim, &span)
    }

    /// Applies one reference insertion after span CAS.
    ///
    /// # Errors
    ///
    /// Returns for malformed references/spans or publication failures.
    #[wasm_bindgen(js_name = insertReference)]
    pub fn insert_reference(&self, reference: JsValue, span: JsValue) -> Result<bool, JsValue> {
        self.inner.insert_reference(&reference, &span)
    }

    /// Consumes a span- or bare-token-guarded token.
    ///
    /// # Errors
    ///
    /// Returns for malformed guards or publication failures.
    #[wasm_bindgen(js_name = consumeToken)]
    pub fn consume_token(&self, guard: JsValue) -> Result<bool, JsValue> {
        self.inner.consume_token(&guard)
    }

    /// Splices literal text over one revision-CAS span.
    ///
    /// # Errors
    ///
    /// Returns for malformed spans or publication failures.
    #[wasm_bindgen(js_name = insertText)]
    pub fn insert_text(&self, text: String, span: JsValue) -> Result<bool, JsValue> {
        self.inner.insert_text(&text, &span)
    }

    /// Surfaces an external notice.
    ///
    /// # Errors
    ///
    /// Returns for invalid levels or notice-store failures.
    pub fn notify(&self, level: String, text: String) -> Result<(), JsValue> {
        self.inner.notice(parse_notice_level(&level)?, &text)
    }

    /// Binds the chat-draft persistence mirror and returns an unbind disposer.
    ///
    /// # Errors
    ///
    /// Returns for non-function writers.
    #[wasm_bindgen(js_name = bindMirror)]
    pub fn bind_mirror(&self, write: JsValue) -> Result<JsValue, JsValue> {
        self.inner.bind_mirror(write)
    }

    /// Aborts attempts and suppresses future asynchronous settlements.
    ///
    /// # Errors
    ///
    /// Returns if publication or `AbortController` teardown fails.
    pub fn dispose(&self) -> Result<(), JsValue> {
        self.inner.dispose()
    }
}

impl ShellInner {
    fn run(self: &Rc<Self>, event: InputMachineEvent) -> Result<(), JsValue> {
        let effects = self.machine.borrow_mut().dispatch(event);
        for effect in effects {
            self.execute(effect)?;
        }
        self.publish()?;
        self.prune_claims();
        Ok(())
    }

    fn execute(self: &Rc<Self>, effect: InputMachineEffect) -> Result<(), JsValue> {
        match effect {
            InputMachineEffect::Notice { level, text } => self.notice(level, &text),
            InputMachineEffect::DefaultSink { draft, mode } => self.sink_serialized(draft, mode),
            InputMachineEffect::Adjudicate { attempt, draft } => self.adjudicate(attempt, &draft),
            InputMachineEffect::BeginSubmit {
                attempt,
                claim,
                args,
            } => self.begin_submit(attempt, &claim, args),
        }
    }

    fn publish(&self) -> Result<(), JsValue> {
        let machine = self.machine.borrow().state();
        let occurrence_values = self.occurrence_values(&machine)?;
        let next = compose_value(
            &machine,
            &self.image_values.borrow(),
            &occurrence_values,
            &self.deps,
        )?;
        required_function(&self.state, "set", "input state store")?.call1(&self.state, &next)?;
        if *self.last_draft.borrow() != machine.draft {
            self.last_draft.borrow_mut().clone_from(&machine.draft);
            let write = self.mirror.borrow().clone();
            if let Some(write) = write {
                write.call1(&JsValue::UNDEFINED, &JsValue::from_str(&machine.draft))?;
            }
        }
        Ok(())
    }

    fn occurrence_values(&self, state: &InputMachineState) -> Result<JsValue, JsValue> {
        let mut projection = self.occurrences.borrow_mut();
        if !Rc::ptr_eq(&projection.source, &state.occurrences) {
            projection.source = state.occurrences.clone();
            projection.value = occurrences_value(&state.occurrences)?.into();
        }
        Ok(projection.value.clone())
    }

    fn replace_images(&self, ids: Vec<DraftAttachmentId>) {
        *self.image_values.borrow_mut() = attachment_ids_value(&ids).into();
        *self.image_ids.borrow_mut() = ids;
    }

    fn prune_claims(&self) {
        let current = self.machine.borrow().claim_submit_id();
        self.claims
            .borrow_mut()
            .retain(|id, _| Some(*id) == current);
    }

    fn add_images(&self, ids: Vec<DraftAttachmentId>) -> Result<bool, JsValue> {
        if matches!(
            self.machine.borrow().state().phase,
            crate::InputPhase::Adjudicating | crate::InputPhase::Submitting
        ) {
            return Ok(false);
        }
        if ids.is_empty() {
            return Ok(true);
        }
        let mut next = self.image_ids.borrow().clone();
        next.extend(ids);
        self.replace_images(next);
        self.publish()?;
        Ok(true)
    }

    fn remove_image(&self, id: &DraftAttachmentId) -> Result<(), JsValue> {
        let before = self.image_ids.borrow().len();
        let next = self
            .image_ids
            .borrow()
            .iter()
            .filter(|candidate| *candidate != id)
            .cloned()
            .collect::<Vec<_>>();
        if next.len() != before {
            self.replace_images(next);
            self.publish()?;
        }
        Ok(())
    }

    fn prune_images(&self, available: &[DraftAttachmentId]) -> Result<(), JsValue> {
        let keep = available.iter().cloned().collect::<BTreeSet<_>>();
        let before = self.image_ids.borrow().len();
        let next = self
            .image_ids
            .borrow()
            .iter()
            .filter(|id| keep.contains(*id))
            .cloned()
            .collect::<Vec<_>>();
        if next.len() != before {
            self.replace_images(next);
            self.publish()?;
        }
        Ok(())
    }

    fn restore_images(&self, ids: &[DraftAttachmentId]) -> Result<(), JsValue> {
        let current = self
            .image_ids
            .borrow()
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut restored = ids
            .iter()
            .filter(|id| !current.contains(*id))
            .cloned()
            .collect::<Vec<_>>();
        restored.extend(self.image_ids.borrow().iter().cloned());
        self.replace_images(restored);
        self.publish()
    }

    fn commit_send(self: &Rc<Self>, ids: &[DraftAttachmentId]) -> Result<(), JsValue> {
        let submitted = ids.iter().cloned().collect::<BTreeSet<_>>();
        let next = self
            .image_ids
            .borrow()
            .iter()
            .filter(|id| !submitted.contains(*id))
            .cloned()
            .collect();
        self.replace_images(next);
        self.run(InputMachineEvent::SendCommitted)
    }

    fn submit(self: &Rc<Self>, mode: BusyEnterBehavior) -> Result<(), JsValue> {
        let snapshot = self.machine.borrow().state();
        if trim_input_text(&snapshot.draft).is_empty() && !self.image_ids.borrow().is_empty() {
            if snapshot.phase == crate::InputPhase::Plain {
                let image_ids = self.image_ids.borrow().clone();
                self.default_sink("", &image_ids, mode)?;
            }
            return Ok(());
        }
        self.run(InputMachineEvent::Enter(mode))?;
        let state = self.machine.borrow().state();
        if matches!(
            state.phase,
            crate::InputPhase::Adjudicating | crate::InputPhase::Submitting
        ) {
            self.dismiss_popup()?;
            if let Some(controller) = self.controller()? {
                required_function(&controller, "track", "input trigger controller")?.apply(
                    &controller,
                    &Array::of4(
                        &JsValue::from_str(&state.draft),
                        &JsValue::from_f64(0.0),
                        object(&[("tier", JsValue::from_str("frozen"))])?.as_ref(),
                        &JsValue::from_f64(u64_to_f64(state.draft_rev.get())?),
                    ),
                )?;
            }
        }
        Ok(())
    }

    fn track(&self, draft: &str, caret: f64) -> Result<(), JsValue> {
        if let Some(controller) = self.controller()? {
            let state = self.machine.borrow().state();
            required_function(&controller, "track", "input trigger controller")?.apply(
                &controller,
                &Array::of4(
                    &JsValue::from_str(draft),
                    &JsValue::from_f64(caret),
                    object(&[("tier", JsValue::from_str(guard_tier(state.phase)))])?.as_ref(),
                    &JsValue::from_f64(u64_to_f64(state.draft_rev.get())?),
                ),
            )?;
        }
        Ok(())
    }

    fn arbitrate(&self, key: &str, composing: bool) -> Result<String, JsValue> {
        parse_arbitrate_key(key)?;
        let Some(controller) = self.controller()? else {
            return Ok("pass".to_owned());
        };
        let outcome = required_function(&controller, "arbitrate", "input trigger controller")?
            .call2(
                &controller,
                &JsValue::from_str(key),
                &JsValue::from_bool(composing),
            )?;
        if outcome.is_null() || outcome.is_undefined() {
            return Ok("pass".to_owned());
        }
        outcome
            .as_string()
            .ok_or_else(|| js_sys::TypeError::new("arbitration outcome must be string").into())
    }

    fn space(&self) -> Result<bool, JsValue> {
        let Some(controller) = self.controller()? else {
            return Ok(false);
        };
        let consumed = required_function(&controller, "onSpace", "input trigger controller")?
            .call0(&controller)?
            .as_bool()
            .unwrap_or(false);
        if consumed {
            let state = self.machine.borrow().state();
            required_function(&controller, "track", "input trigger controller")?.apply(
                &controller,
                &Array::of4(
                    &JsValue::from_str(&state.draft),
                    &JsValue::from_f64(f64::from(utf16_len(&state.draft))),
                    object(&[("tier", JsValue::from_str(guard_tier(state.phase)))])?.as_ref(),
                    &JsValue::from_f64(u64_to_f64(state.draft_rev.get())?),
                ),
            )?;
        }
        Ok(consumed)
    }

    fn dismiss_popup(&self) -> Result<(), JsValue> {
        let Some(popup) = self.resolve_thunk("popup")? else {
            return Ok(());
        };
        required_function(&popup, "dismiss", "popup face")?.call0(&popup)?;
        Ok(())
    }

    fn begin_command(self: &Rc<Self>, claim: &JsValue, span: &JsValue) -> Result<bool, JsValue> {
        let claim = self.parse_claim(claim)?;
        let before = self.machine.borrow().state().draft_rev;
        self.run(InputMachineEvent::BeginCommand {
            claim,
            span: parse_span(span)?,
        })?;
        let state = self.machine.borrow().state();
        Ok(state.phase == crate::InputPhase::Claimed && state.draft_rev != before)
    }

    fn insert_reference(
        self: &Rc<Self>,
        reference: &JsValue,
        span: &JsValue,
    ) -> Result<bool, JsValue> {
        let before = self.machine.borrow().state().draft_rev;
        self.run(InputMachineEvent::InsertReference {
            reference: parse_reference(reference)?,
            span: parse_span(span)?,
        })?;
        Ok(self.machine.borrow().state().draft_rev != before)
    }

    fn consume_token(self: &Rc<Self>, guard: &JsValue) -> Result<bool, JsValue> {
        let kind = required_string(guard, "kind", "consume guard")?;
        let snapshot = self.machine.borrow().state();
        if kind == "span" {
            let span_value = required_property(guard, "span", "consume guard")?;
            let span = parse_span(&span_value)?;
            if span.draft_rev != snapshot.draft_rev {
                return Ok(false);
            }
            self.run(InputMachineEvent::DraftChanged {
                draft: splice_input_text(&snapshot.draft, span.start, span.end, ""),
                edit_range: None,
            })?;
            return Ok(true);
        }
        if kind != "bare-token" {
            return Err(js_sys::Error::new(&format!("unreachable consume guard: {kind}")).into());
        }
        let token = required_string(guard, "token", "consume guard")?;
        if trim_input_text(&snapshot.draft) != token {
            return Ok(false);
        }
        self.run(InputMachineEvent::DraftChanged {
            draft: String::new(),
            edit_range: None,
        })?;
        Ok(true)
    }

    fn insert_text(self: &Rc<Self>, text: &str, span_value: &JsValue) -> Result<bool, JsValue> {
        let span = parse_span(span_value)?;
        let snapshot = self.machine.borrow().state();
        if span.draft_rev != snapshot.draft_rev {
            return Ok(false);
        }
        self.run(InputMachineEvent::DraftChanged {
            draft: splice_input_text(&snapshot.draft, span.start, span.end, text),
            edit_range: None,
        })?;
        Ok(true)
    }

    fn notice(&self, level: InputNoticeLevel, text: &str) -> Result<(), JsValue> {
        let seq = self.notice_seq.get().wrapping_add(1);
        self.notice_seq.set(seq);
        let value = object(&[
            ("level", JsValue::from_str(notice_level(level))),
            ("text", JsValue::from_str(text)),
            ("seq", JsValue::from_f64(u64_to_f64(seq)?)),
        ])?;
        required_function(&self.notices, "set", "input notices store")?
            .call1(&self.notices, value.as_ref())?;
        Ok(())
    }

    fn bind_mirror(self: &Rc<Self>, write: JsValue) -> Result<JsValue, JsValue> {
        let write = write.dyn_into::<Function>()?;
        *self.mirror.borrow_mut() = Some(write.clone());
        let weak = Rc::downgrade(self);
        Ok(Closure::wrap(Box::new(move || {
            if let Some(inner) = weak.upgrade()
                && inner
                    .mirror
                    .borrow()
                    .as_ref()
                    .is_some_and(|current| Object::is(current.as_ref(), write.as_ref()))
            {
                *inner.mirror.borrow_mut() = None;
            }
        }) as Box<dyn FnMut()>)
        .into_js_value())
    }

    fn dispose(self: &Rc<Self>) -> Result<(), JsValue> {
        self.disposed.set(true);
        let controllers = self
            .controllers
            .borrow()
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for controller in controllers {
            required_function(&controller, "abort", "AbortController")?.call0(&controller)?;
        }
        self.run(InputMachineEvent::Release)?;
        self.controllers.borrow_mut().clear();
        Ok(())
    }

    fn actions_face(self: &Rc<Self>) -> Result<JsValue, JsValue> {
        if let Some(face) = self.actions.borrow().as_ref() {
            return Ok(face.clone());
        }
        let weak = Rc::downgrade(self);
        let set_draft = Closure::wrap(Box::new(move |text: String| -> Result<(), JsValue> {
            weak.upgrade().map_or(Ok(()), |inner| {
                inner.run(InputMachineEvent::DraftChanged {
                    draft: text,
                    edit_range: None,
                })
            })
        }) as Box<dyn FnMut(String) -> Result<(), JsValue>>)
        .into_js_value();
        let weak = Rc::downgrade(self);
        let add_images = Closure::wrap(Box::new(move |ids: JsValue| -> Result<bool, JsValue> {
            weak.upgrade().map_or(Ok(false), |inner| {
                inner.add_images(parse_attachment_ids(&ids)?)
            })
        })
            as Box<dyn FnMut(JsValue) -> Result<bool, JsValue>>)
        .into_js_value();
        let weak = Rc::downgrade(self);
        let remove_image = Closure::wrap(Box::new(move |id: String| -> Result<(), JsValue> {
            weak.upgrade().map_or(Ok(()), |inner| {
                inner.remove_image(&DraftAttachmentId::new(id))
            })
        })
            as Box<dyn FnMut(String) -> Result<(), JsValue>>)
        .into_js_value();
        let weak = Rc::downgrade(self);
        let prune_images = Closure::wrap(Box::new(move |ids: JsValue| -> Result<(), JsValue> {
            weak.upgrade().map_or(Ok(()), |inner| {
                inner.prune_images(&parse_attachment_ids(&ids)?)
            })
        })
            as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
        .into_js_value();
        let weak = Rc::downgrade(self);
        let submit = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            weak.upgrade()
                .map_or(Ok(()), |inner| inner.submit(BusyEnterBehavior::Queue))
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value();
        let face = object(&[
            ("setDraft", set_draft),
            ("addImages", add_images),
            ("removeImage", remove_image),
            ("pruneImages", prune_images),
            ("submit", submit),
        ])?;
        let face: JsValue = face.into();
        *self.actions.borrow_mut() = Some(face.clone());
        Ok(face)
    }

    fn lexicon_face(self: &Rc<Self>) -> Result<JsValue, JsValue> {
        if let Some(face) = self.lexicon.borrow().as_ref() {
            return Ok(face.clone());
        }
        let weak = Rc::downgrade(self);
        let get_snapshot = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
            let Some(inner) = weak.upgrade() else {
                return Ok(js_sys::Map::new().into());
            };
            let Some(controller) = inner.controller()? else {
                return Ok(inner.empty_lexicon.clone());
            };
            let lexicon = required_property(&controller, "lexicon", "input trigger controller")?;
            required_function(&lexicon, "getSnapshot", "trigger lexicon")?.call0(&lexicon)
        })
            as Box<dyn FnMut() -> Result<JsValue, JsValue>>)
        .into_js_value();
        let weak = Rc::downgrade(self);
        let subscribe = Closure::wrap(Box::new(
            move |listener: JsValue| -> Result<JsValue, JsValue> {
                let Some(inner) = weak.upgrade() else {
                    return Ok(noop_function());
                };
                let Some(controller) = inner.controller()? else {
                    return Ok(noop_function());
                };
                let lexicon =
                    required_property(&controller, "lexicon", "input trigger controller")?;
                required_function(&lexicon, "subscribe", "trigger lexicon")?
                    .call1(&lexicon, &listener)
            },
        )
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
        .into_js_value();
        let face: JsValue =
            object(&[("getSnapshot", get_snapshot), ("subscribe", subscribe)])?.into();
        *self.lexicon.borrow_mut() = Some(face.clone());
        Ok(face)
    }

    fn controller(&self) -> Result<Option<JsValue>, JsValue> {
        self.resolve_thunk("inputTriggers")
    }

    fn resolve_thunk(&self, key: &str) -> Result<Option<JsValue>, JsValue> {
        let value = Reflect::get(&self.deps, &JsValue::from_str(key))?;
        if value.is_null() || value.is_undefined() {
            return Ok(None);
        }
        let resolved = value.dyn_into::<Function>()?.call0(&self.deps)?;
        Ok((!resolved.is_null() && !resolved.is_undefined()).then_some(resolved))
    }

    fn call_optional_dep(
        &self,
        key: &str,
        arguments: &[JsValue],
    ) -> Result<Option<JsValue>, JsValue> {
        let value = Reflect::get(&self.deps, &JsValue::from_str(key))?;
        if value.is_null() || value.is_undefined() {
            return Ok(None);
        }
        Ok(Some(
            value
                .dyn_into::<Function>()?
                .apply(&self.deps, &arguments.iter().collect())?,
        ))
    }

    fn parse_claim(&self, value: &JsValue) -> Result<InputCommandClaim, JsValue> {
        let seq = self.claim_seq.get().wrapping_add(1);
        self.claim_seq.set(seq);
        let id = CommandSubmitId::new(seq);
        required_function(value, "submit", "command claim")?;
        self.claims.borrow_mut().insert(id, value.clone());
        Ok(InputCommandClaim {
            token: required_string(value, "token", "command claim")?,
            hint: Reflect::get(value, &JsValue::from_str("hint"))?.as_string(),
            submit_id: id,
        })
    }

    fn ensure_controller(&self, attempt: &SubmitAttempt) -> Result<JsValue, JsValue> {
        if let Some(existing) = self.controllers.borrow().get(&attempt.id) {
            return Ok(existing.clone());
        }
        let constructor = required_function(&js_sys::global(), "AbortController", "global")?;
        let controller = Reflect::construct(&constructor, &Array::new())?;
        self.controllers
            .borrow_mut()
            .insert(attempt.id, controller.clone());
        Ok(controller)
    }

    fn dead(&self, attempt: &SubmitAttempt) -> bool {
        self.disposed.get() || attempt.signal.aborted()
    }

    fn adjudicate(self: &Rc<Self>, attempt: SubmitAttempt, draft: &str) -> Result<(), JsValue> {
        let Some(controller) = self.controller()? else {
            return self.run(InputMachineEvent::Adjudicated {
                attempt,
                outcome: InputPickOutcome::Miss,
            });
        };
        let abort = self.ensure_controller(&attempt)?;
        let signal = required_property(&abort, "signal", "AbortController")?;
        let returned = required_function(&controller, "adjudicate", "input trigger controller")?
            .call2(
                &controller,
                &JsValue::from_str(&trim_input_text(draft)),
                &signal,
            )?;
        let inner = Rc::clone(self);
        spawn_local(async move {
            let settled = JsFuture::from(Promise::resolve(&returned)).await;
            match settled {
                Ok(value) => {
                    if inner.dead(&attempt) {
                        return;
                    }
                    match inner.parse_pick_outcome(&value) {
                        Ok(outcome) => {
                            let keeps_controller = matches!(outcome, InputPickOutcome::Claim(_));
                            if let Err(error) = inner.run(InputMachineEvent::Adjudicated {
                                attempt: attempt.clone(),
                                outcome,
                            }) {
                                log_error(&error);
                            }
                            if !keeps_controller {
                                inner.controllers.borrow_mut().remove(&attempt.id);
                            }
                        }
                        Err(error) => log_error(&error),
                    }
                }
                Err(error) => {
                    if inner.dead(&attempt) {
                        return;
                    }
                    let message = error_message(&error);
                    if let Err(error) = inner.run(InputMachineEvent::AdjudicationFailed {
                        attempt: attempt.clone(),
                        message,
                    }) {
                        log_error(&error);
                    }
                    inner.controllers.borrow_mut().remove(&attempt.id);
                }
            }
        });
        Ok(())
    }

    fn begin_submit(
        self: &Rc<Self>,
        attempt: SubmitAttempt,
        claim: &InputCommandClaim,
        args: String,
    ) -> Result<(), JsValue> {
        let original = self
            .claims
            .borrow()
            .get(&claim.submit_id)
            .cloned()
            .ok_or_else(|| js_sys::Error::new("command claim submit closure unavailable"))?;
        let inner = Rc::clone(self);
        spawn_local(async move {
            let _ = JsFuture::from(Promise::resolve(&JsValue::UNDEFINED)).await;
            let settled = match required_property(&inner.deps, "actx", "SessionInput deps")
                .and_then(|actx| {
                    required_function(&original, "submit", "command claim")?.call2(
                        &original,
                        &JsValue::from_str(&args),
                        &actx,
                    )
                }) {
                Ok(value) => JsFuture::from(Promise::resolve(&value)).await,
                Err(error) => Err(error),
            };
            if inner.dead(&attempt) {
                return;
            }
            let event = match settled {
                Ok(value) => match parse_submit_outcome(&value) {
                    Ok(outcome) => InputMachineEvent::SubmitSettled {
                        attempt: attempt.clone(),
                        ok: outcome.kind == CommandSubmitOutcomeKind::Success,
                        outcome: Some(outcome),
                        message: None,
                    },
                    Err(error) => {
                        log_error(&error);
                        return;
                    }
                },
                Err(error) => InputMachineEvent::SubmitSettled {
                    attempt: attempt.clone(),
                    ok: false,
                    outcome: None,
                    message: Some(error_message(&error)),
                },
            };
            if let Err(error) = inner.run(event) {
                log_error(&error);
            }
            inner.controllers.borrow_mut().remove(&attempt.id);
        });
        Ok(())
    }

    fn sink_serialized(
        self: &Rc<Self>,
        draft: String,
        mode: BusyEnterBehavior,
    ) -> Result<(), JsValue> {
        let images = self.image_ids.borrow().clone();
        let occurrences = self.machine.borrow().state().occurrences;
        if occurrences.is_empty() {
            return self.default_sink(&trim_input_text(&draft), &images, mode);
        }
        let controller = self.controller()?;
        let abort_constructor = required_function(&js_sys::global(), "AbortController", "global")?;
        let abort = Reflect::construct(&abort_constructor, &Array::new())?;
        let signal = required_property(&abort, "signal", "AbortController")?;
        let promises = Array::new();
        for occurrence in occurrences.iter() {
            let promise = controller.as_ref().map_or_else(
                || {
                    Promise::reject(
                        &js_sys::Error::new(&format!(
                            "no serializer for reference source \"{}\"",
                            occurrence.source
                        ))
                        .into(),
                    )
                },
                |controller| match required_function(
                    controller,
                    "serializeReference",
                    "input trigger controller",
                )
                .and_then(|serialize| {
                    serialize.apply(
                        controller,
                        &Array::of3(
                            &JsValue::from_str(&occurrence.source),
                            &JsValue::from_str(&occurrence.reference),
                            &signal,
                        ),
                    )
                }) {
                    Ok(returned) => Promise::resolve(&returned),
                    Err(error) => Promise::reject(&error),
                },
            );
            promises.push(&promise);
        }
        let inner = Rc::clone(self);
        spawn_local(async move {
            let settled = JsFuture::from(Promise::all(&promises)).await;
            match settled {
                Ok(values) => {
                    if inner.disposed.get() {
                        return;
                    }
                    let values = values.unchecked_into::<Array>();
                    let mut output = String::new();
                    let mut cursor = 0;
                    for (index, occurrence) in occurrences.iter().enumerate() {
                        output.push_str(&slice_input_text(&draft, cursor, occurrence.offset));
                        output.push_str(
                            &values
                                .get(u32::try_from(index).unwrap_or_default())
                                .as_string()
                                .unwrap_or_default(),
                        );
                        cursor = occurrence.offset.saturating_add(1);
                    }
                    output.push_str(&slice_input_text(&draft, cursor, input_text_len(&draft)));
                    if let Err(error) = inner.default_sink(&trim_input_text(&output), &images, mode)
                    {
                        log_error(&error);
                    }
                }
                Err(error) => {
                    let _ = required_function(&abort, "abort", "AbortController")
                        .and_then(|abort_fn| abort_fn.call0(&abort));
                    if !inner.disposed.get()
                        && let Err(error) =
                            inner.notice(InputNoticeLevel::Error, &error_message(&error))
                    {
                        log_error(&error);
                    }
                }
            }
        });
        Ok(())
    }

    fn default_sink(
        &self,
        text: &str,
        images: &[DraftAttachmentId],
        mode: BusyEnterBehavior,
    ) -> Result<(), JsValue> {
        let image_values = Array::new();
        for image in images {
            image_values.push(&JsValue::from_str(image.as_str()));
        }
        required_function(&self.deps, "defaultSink", "SessionInput deps")?.apply(
            &self.deps,
            &Array::of3(
                &JsValue::from_str(text),
                image_values.as_ref(),
                &JsValue::from_str(mode_name(mode)),
            ),
        )?;
        Ok(())
    }

    fn parse_pick_outcome(self: &Rc<Self>, value: &JsValue) -> Result<InputPickOutcome, JsValue> {
        if value.is_undefined() {
            return Ok(InputPickOutcome::Miss);
        }
        if value.as_string().as_deref() == Some("handled") {
            return Ok(InputPickOutcome::Handled);
        }
        let claim = Reflect::get(value, &JsValue::from_str("claim"))?;
        if !claim.is_undefined() {
            return Ok(InputPickOutcome::Claim(self.parse_claim(&claim)?));
        }
        let insert = Reflect::get(value, &JsValue::from_str("insert"))?;
        if !insert.is_undefined() {
            return Ok(InputPickOutcome::Insert(parse_reference(&insert)?));
        }
        let text = Reflect::get(value, &JsValue::from_str("text"))?;
        if let Some(text) = text.as_string() {
            return Ok(InputPickOutcome::Text(text));
        }
        Err(js_sys::Error::new("unrecognized input pick outcome").into())
    }
}

fn subscribe_queue(inner: &Rc<ShellInner>) -> Result<(), JsValue> {
    let queue = Reflect::get(&inner.deps, &JsValue::from_str("queue"))?;
    if queue.is_null() || queue.is_undefined() {
        return Ok(());
    }
    let weak = Rc::downgrade(inner);
    let listener = Closure::wrap(Box::new(move || {
        if let Some(inner) = weak.upgrade()
            && let Err(error) = inner.publish()
        {
            log_error(&error);
        }
    }) as Box<dyn FnMut()>);
    required_function(&queue, "subscribe", "queue face")?
        .call1(&queue, &listener.into_js_value())?;
    Ok(())
}

fn compose_value(
    state: &InputMachineState,
    image_values: &JsValue,
    occurrence_values: &JsValue,
    deps: &JsValue,
) -> Result<JsValue, JsValue> {
    let mut entries = vec![
        ("draft", JsValue::from_str(&state.draft)),
        ("imageIds", image_values.clone()),
        (
            "draftRev",
            JsValue::from_f64(u64_to_f64(state.draft_rev.get())?),
        ),
        ("phase", JsValue::from_str(phase_name(state.phase))),
        ("occurrences", occurrence_values.clone()),
    ];
    if let Some(claim) = state.claim.as_ref() {
        let mut claim_entries = vec![("token", JsValue::from_str(&claim.token))];
        if let Some(hint) = claim.hint.as_ref() {
            claim_entries.push(("hint", JsValue::from_str(hint)));
        }
        entries.push(("claim", object(&claim_entries)?.into()));
    }
    if let Some(paste) = state.paste {
        entries.push((
            "paste",
            object(&[
                (
                    "attemptId",
                    JsValue::from_f64(u64_to_f64(paste.attempt_id.get())?),
                ),
                (
                    "insertedRange",
                    selection_value(paste.inserted_range)?.into(),
                ),
                (
                    "generation",
                    JsValue::from_f64(u64_to_f64(paste.generation)?),
                ),
            ])?
            .into(),
        ));
    }
    let queue = Reflect::get(deps, &JsValue::from_str("queue"))?;
    let queue = if queue.is_null() || queue.is_undefined() {
        EMPTY_QUEUE.with(Clone::clone)
    } else {
        required_function(&queue, "getSnapshot", "queue face")?.call0(&queue)?
    };
    entries.push(("queue", queue));
    Ok(object(&entries)?.into())
}

fn occurrences_value(occurrences: &[crate::InputOccurrence]) -> Result<Array, JsValue> {
    let values = Array::new();
    for occurrence in occurrences {
        let mut entries = vec![
            (
                "occurrenceId",
                JsValue::from_f64(u64_to_f64(occurrence.occurrence_id.get())?),
            ),
            ("source", JsValue::from_str(&occurrence.source)),
            ("ref", JsValue::from_str(&occurrence.reference)),
            ("offset", JsValue::from_f64(f64::from(occurrence.offset))),
            ("label", JsValue::from_str(&occurrence.label)),
            (
                "clipboardText",
                JsValue::from_str(&occurrence.clipboard_text),
            ),
        ];
        if occurrence.invalid {
            entries.push(("invalid", JsValue::TRUE));
        }
        values.push(object(&entries)?.as_ref());
    }
    Ok(values)
}

fn parse_reference(value: &JsValue) -> Result<InputReferenceInsert, JsValue> {
    Ok(InputReferenceInsert {
        source: required_string(value, "source", "reference insert")?,
        reference: required_string(value, "ref", "reference insert")?,
        label: required_string(value, "label", "reference insert")?,
        clipboard_text: required_string(value, "clipboardText", "reference insert")?,
    })
}

fn parse_span(value: &JsValue) -> Result<InputTokenSpan, JsValue> {
    Ok(InputTokenSpan {
        start: number_to_u32(
            numeric_property(value, "start", "token span")?,
            "span start",
        )?,
        end: number_to_u32(numeric_property(value, "end", "token span")?, "span end")?,
        draft_rev: DraftRevision::new(number_to_u64(
            numeric_property(value, "draftRev", "token span")?,
            "span draftRev",
        )?),
    })
}

fn parse_optional_edit_range(value: &JsValue) -> Result<Option<EditRange>, JsValue> {
    if value.is_null() || value.is_undefined() {
        return Ok(None);
    }
    Ok(Some(EditRange {
        start: number_to_u32(
            numeric_property(value, "start", "edit range")?,
            "edit start",
        )?,
        end: number_to_u32(numeric_property(value, "end", "edit range")?, "edit end")?,
        inserted_length: number_to_u32(
            numeric_property(value, "insertedLength", "edit range")?,
            "edit insertedLength",
        )?,
    }))
}

fn parse_selection(value: &JsValue, owner: &str) -> Result<EditSelection, JsValue> {
    Ok(EditSelection {
        start: number_to_u32(numeric_property(value, "start", owner)?, "selection start")?,
        end: number_to_u32(numeric_property(value, "end", owner)?, "selection end")?,
    })
}

fn parse_paste_components(value: &JsValue) -> Result<Vec<PasteComponent>, JsValue> {
    if value.is_null() || value.is_undefined() {
        return Ok(Vec::new());
    }
    if !Array::is_array(value) {
        return Err(js_sys::TypeError::new("paste components must be an array").into());
    }
    let values = value.clone().dyn_into::<Array>()?;
    let mut components = Vec::new();
    for index in 0..values.length() {
        let value = values.get(index);
        components.push(PasteComponent {
            start: number_to_u32(
                numeric_property(&value, "start", "paste component")?,
                "component start",
            )?,
            end: number_to_u32(
                numeric_property(&value, "end", "paste component")?,
                "component end",
            )?,
            reference: parse_reference(&required_property(
                &value,
                "reference",
                "paste component",
            )?)?,
        });
    }
    Ok(components)
}

fn parse_submit_outcome(value: &JsValue) -> Result<CommandSubmitOutcome, JsValue> {
    let kind = match Reflect::get(value, &JsValue::from_str("kind"))?.as_string() {
        Some(kind) if kind == "success" => CommandSubmitOutcomeKind::Success,
        _ => CommandSubmitOutcomeKind::Error,
    };
    Ok(CommandSubmitOutcome {
        kind,
        text: Reflect::get(value, &JsValue::from_str("text"))?.as_string(),
    })
}

fn parse_attachment_ids(value: &JsValue) -> Result<Vec<DraftAttachmentId>, JsValue> {
    if !Array::is_array(value) {
        return Err(js_sys::TypeError::new("image ids must be an array").into());
    }
    let values = value.clone().dyn_into::<Array>()?;
    values
        .iter()
        .map(|value| {
            value
                .as_string()
                .map(DraftAttachmentId::new)
                .ok_or_else(|| js_sys::TypeError::new("image ids must be strings").into())
        })
        .collect()
}

fn attachment_ids_value(ids: &[DraftAttachmentId]) -> Array {
    ids.iter()
        .map(|id| JsValue::from_str(id.as_str()))
        .collect()
}

fn selection_value(selection: EditSelection) -> Result<Object, JsValue> {
    object(&[
        ("start", JsValue::from_f64(f64::from(selection.start))),
        ("end", JsValue::from_f64(f64::from(selection.end))),
    ])
}

fn parse_mode(value: &str) -> Result<BusyEnterBehavior, JsValue> {
    match value {
        "queue" => Ok(BusyEnterBehavior::Queue),
        "steer" => Ok(BusyEnterBehavior::Steer),
        _ => Err(js_sys::Error::new(&format!("unknown input submit mode: {value}")).into()),
    }
}

fn parse_notice_level(value: &str) -> Result<InputNoticeLevel, JsValue> {
    match value {
        "info" => Ok(InputNoticeLevel::Info),
        "error" => Ok(InputNoticeLevel::Error),
        _ => Err(js_sys::Error::new(&format!("unknown input notice level: {value}")).into()),
    }
}

fn parse_arbitrate_key(value: &str) -> Result<ArbitrateKey, JsValue> {
    match value {
        "up" => Ok(ArbitrateKey::Up),
        "down" => Ok(ArbitrateKey::Down),
        "enter" => Ok(ArbitrateKey::Enter),
        "escape" => Ok(ArbitrateKey::Escape),
        _ => Err(js_sys::Error::new(&format!("unknown arbitrate key: {value}")).into()),
    }
}

fn phase_name(phase: crate::InputPhase) -> &'static str {
    match phase {
        crate::InputPhase::Plain => "plain",
        crate::InputPhase::Adjudicating => "adjudicating",
        crate::InputPhase::Claimed => "claimed",
        crate::InputPhase::Submitting => "submitting",
    }
}

fn guard_tier(phase: crate::InputPhase) -> &'static str {
    match phase {
        crate::InputPhase::Plain => "plain",
        crate::InputPhase::Claimed => "claimed",
        crate::InputPhase::Adjudicating | crate::InputPhase::Submitting => "frozen",
    }
}

fn mode_name(mode: BusyEnterBehavior) -> &'static str {
    match mode {
        BusyEnterBehavior::Queue => "queue",
        BusyEnterBehavior::Steer => "steer",
    }
}

fn notice_level(level: InputNoticeLevel) -> &'static str {
    match level {
        InputNoticeLevel::Info => "info",
        InputNoticeLevel::Error => "error",
    }
}

fn utf16_len(value: &str) -> u32 {
    u32::try_from(value.encode_utf16().count()).unwrap_or(u32::MAX)
}

fn u64_to_f64(value: u64) -> Result<f64, JsValue> {
    value
        .to_string()
        .parse::<f64>()
        .map_err(|_| js_sys::RangeError::new("integer cannot be represented as number").into())
}

fn number_to_u32(value: f64, owner: &str) -> Result<u32, JsValue> {
    number_string(value)?
        .parse::<u32>()
        .map_err(|_| js_sys::RangeError::new(&format!("{owner} must be a u32")).into())
}

fn number_to_u64(value: f64, owner: &str) -> Result<u64, JsValue> {
    number_string(value)?
        .parse::<u64>()
        .map_err(|_| js_sys::RangeError::new(&format!("{owner} must be a u64")).into())
}

fn number_string(value: f64) -> Result<String, JsValue> {
    js_sys::Number::from(value)
        .to_string_with_radix(10)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("Number.toString() returned non-string").into())
}

fn javascript_number(value: &JsValue) -> Result<f64, JsValue> {
    required_function(&js_sys::global(), "Number", "global")?
        .call1(&JsValue::UNDEFINED, value)?
        .as_f64()
        .ok_or_else(|| js_sys::TypeError::new("Number() returned non-number").into())
}

fn numeric_property(value: &JsValue, key: &str, owner: &str) -> Result<f64, JsValue> {
    required_property(value, key, owner)?
        .as_f64()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be number")).into())
}

fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    required_property(value, key, owner)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be string")).into())
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

fn error_message(error: &JsValue) -> String {
    if let Some(error) = error.dyn_ref::<js_sys::Error>() {
        return error.message().into();
    }
    required_function(&js_sys::global(), "String", "global")
        .and_then(|string| string.call1(&JsValue::UNDEFINED, error))
        .ok()
        .and_then(|message| message.as_string())
        .unwrap_or_else(|| format!("{error:?}"))
}

fn noop_function() -> JsValue {
    Closure::wrap(Box::new(move || {}) as Box<dyn FnMut()>).into_js_value()
}

fn log_error(error: &JsValue) {
    if let Ok(console) = Reflect::get(&js_sys::global(), &JsValue::from_str("console"))
        && let Ok(log) = Reflect::get(&console, &JsValue::from_str("error"))
            .and_then(wasm_bindgen::JsCast::dyn_into::<Function>)
    {
        let _ = log.call1(&console, error);
    }
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let object = Object::new();
    for (key, value) in entries {
        Reflect::set(&object, &JsValue::from_str(key), value)?;
    }
    Ok(object)
}
